//! Legado 规则引擎（AnalyzeRule 等价实现）
//!
//! 支持：CSS 选择器(scraper) / JsonPath / XPath(sxd-xpath, XML 源) / 正则 / 变量 `${name}` 展开 /
//! 整串 JS 段(`<js>`、`@js:`)切分 / 列表解析(get_string_list)。
//! 元素级 `&&`/`||`/`%%`/`@` 组合与索引/范围语法、WebView 渲染降级为后续增强项。

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, SyncSender};
use std::sync::OnceLock;
use std::time::Instant;

use jsonpath_rust::JsonPathQuery;
use once_cell::sync::OnceCell;
use regex::Regex;
use serde_json::Value;

/// JS 规则预算。boa 0.19 无中断/时间片 API，因此用「循环迭代上限 + 递归/栈上限」
/// 做滥用防护；墙钟耗时用于统计与超时提示（见 `JS_TIMEOUT_MS`）。
#[derive(Clone, Copy)]
struct JsConf {
    enabled: bool,
    timeout_ms: u64,
    loop_iteration_limit: u64,
    recursion_limit: usize,
    stack_size_limit: usize,
}

static JS_CONF: OnceCell<JsConf> = OnceCell::new();
static JS_LAST_MS: AtomicU64 = AtomicU64::new(0);
static JS_MAX_MS: AtomicU64 = AtomicU64::new(0);
/// 当前 JS 任务里 `java.ajax` 阻塞联网花费的毫秒数（JS 预算只算纯脚本耗时）。
static JS_AJAX_NET_MS: AtomicU64 = AtomicU64::new(0);

pub fn init_js(enabled: bool, timeout_ms: u64) {
    let _ = JS_CONF.set(JsConf {
        enabled,
        timeout_ms,
        loop_iteration_limit: 500_000,
        recursion_limit: 100,
        stack_size_limit: 512,
    });
}

fn js_conf() -> JsConf {
    *JS_CONF.get().unwrap_or(&JsConf {
        enabled: true,
        timeout_ms: crate::config::DEFAULT_JS_TIMEOUT_MS,
        loop_iteration_limit: 500_000,
        recursion_limit: 100,
        stack_size_limit: 512,
    })
}

/// 最近一次 / 历史最长 JS 规则耗时（毫秒），供 /healthz 观察。
pub fn js_metrics() -> (u64, u64) {
    (
        JS_LAST_MS.load(Ordering::Relaxed),
        JS_MAX_MS.load(Ordering::Relaxed),
    )
}

/// boa 0.19 的 `Context` 为 `!Send`，且实测多线程并发创建/执行 `Context` 会触发
/// boa_gc 引用计数错乱（`STATUS_HEAP_CORRUPTION`）。因此改为「单一专用 JS 线程」：
/// 全进程只有一个 `Context`，既规避线程安全问题，也把 boa 常驻内存固定为一个实例。
/// 该线程仅在首次遇到 JS 规则时启动（见 `js_sender`）。
/// JS 执行任务：脚本文本 + 回传通道。
struct JsJob {
    script: String,
    /// (求值结果, java.ajax 的网络耗时毫秒, 实际脚本执行耗时毫秒)
    /// 执行耗时由 worker 自己计时，不包含等待单一 JS 线程排队的墙钟时间。
    reply: Sender<(Result<String, String>, u64, u64)>,
}

/// 全局唯一的 JS 执行线程入口（容量 8，避免无界排队）。
static JS_TX: OnceLock<SyncSender<JsJob>> = OnceLock::new();

/// 等待 JS 线程回传结果。
///
/// 规则解析整条调用链是同步的（`AnalyzeRule` 内含 `RefCell`/`Rc`，既不能 `await`
/// 也不满足 `Send + 'static`，无法整体搬进 `spawn_blocking`）。因此这里在
/// tokio 多线程 runtime 上用 `block_in_place`：它会把当前 worker 的调度核心
/// 迁移到 blocking 线程并从 blocking 池补一个执行者，本线程只做等待，
/// **不再占住 worker**，慢 JS 也就不会把 2 个 worker 一起拖死。
///
/// 单线程 runtime（`current_thread`）与纯同步上下文（单测/CLI）没有该能力，
/// 退回直接阻塞，行为与原实现一致。
fn wait_js_reply(
    rx: Receiver<(Result<String, String>, u64, u64)>,
) -> Result<(Result<String, String>, u64, u64), RuleError> {
    let wait = || {
        rx.recv()
            .map_err(|_| RuleError::Other("js worker dropped".into()))
    };
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(wait)
        }
        _ => wait(),
    }
}

fn js_sender() -> &'static SyncSender<JsJob> {
    JS_TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<JsJob>(8);
        std::thread::Builder::new()
            .name("boa-js".into())
            // 单线程独占 1 MiB 栈：boa 递归/解析需要空间，仍远小于默认 8 MiB
            .stack_size(1024 * 1024)
            .spawn(move || js_worker(rx))
            .expect("spawn boa js worker");
        tx
    })
}

/// 创建 JS 执行上下文（含预算配置与 `java.ajax` 网络桥）。
fn new_js_context(conf: JsConf) -> boa_engine::Context {
    let mut ctx = boa_engine::Context::default();
    {
        let l = ctx.runtime_limits_mut();
        l.set_loop_iteration_limit(conf.loop_iteration_limit);
        l.set_recursion_limit(conf.recursion_limit);
        l.set_stack_size_limit(conf.stack_size_limit);
    }
    // java.ajax 的网络桥：boa 求值在专用线程上同步执行，这里用当前线程的
    // 单线程 tokio runtime 阻塞等待 `req::fetch`，因而天然复用同一套
    // 15s 超时 / 5MiB 体积上限 / 私网拦截 / 重定向校验策略。
    let _ = ctx.register_global_callable(
        boa_engine::JsString::from("__nativeAjax"),
        0,
        boa_engine::NativeFunction::from_fn_ptr(native_ajax),
    );
    // java.setContent 之后的 DOM 选择器求值桥（惰性解析，见 native_select）。
    let _ = ctx.register_global_callable(
        boa_engine::JsString::from("__nativeSelect"),
        0,
        boa_engine::NativeFunction::from_fn_ptr(native_select),
    );
    ctx
}

/// 专用线程主循环：复用 Context 处理任务；**仅在 RuntimeLimit 失败后重建**。
///
/// boa 0.19 的 runtime-limit 错误是不可被 JS `catch` 的，且其栈解绑逻辑在顶层
/// （`exit_early`）帧上会提前 break，`vm.stack` 可能停在溢出时的长度不回收。
/// 而后续每次求值都会先用 `vm.stack.len()` 做上限检查，于是“一次深递归”会让
/// 这个单一 Context 之后**所有** JS 规则都报同一个 RuntimeLimit，直到重启进程。
/// 因此出错（仅此类错误）后重建 Context，保证坏脚本不污染后续规则；正常路径
/// 不创建任何新对象，成本与改动前一致。
fn js_worker(rx: Receiver<JsJob>) {
    let conf = js_conf();
    let mut ctx = new_js_context(conf);
    while let Ok(job) = rx.recv() {
        JS_AJAX_NET_MS.store(0, Ordering::Relaxed);
        let started = Instant::now();
        let mut poisoned = false;
        let res = ctx
            .eval(boa_engine::Source::from_bytes(job.script.as_bytes()))
            // `{e:?}` 对 opaque Error 只会打印 ErrorObject 地址，书源作者看不懂。
            // 用 Display 取出 message；垫片抛出的未支持 API 显式给出特性名。
            .map_err(|e| {
                poisoned = e.as_native().map(|n| n.is_runtime_limit()).unwrap_or(false);
                js_error_message(&e.to_string())
            })
            .and_then(|v| {
                let mut to_string_failed = false;
                let out = v.to_string(&mut ctx).map_err(|e| {
                    to_string_failed = true;
                    format!("js to_string: {e}")
                });
                // to_string 失败同样说明 Context 状态不可信
                poisoned |= to_string_failed;
                out
            })
            .map(|s| s.to_std_string().unwrap_or_default());
        let net_ms = JS_AJAX_NET_MS.load(Ordering::Relaxed);
        // 只统计真正花在这条规则上的时间；排队等 Context 的时间不计入预算。
        let js_ms = (started.elapsed().as_millis() as u64).saturating_sub(net_ms);
        let _ = job.reply.send((res, net_ms, js_ms));
        if poisoned {
            ctx = new_js_context(conf);
        }
    }
}

/// `java.ajax(url[, method[, body[, headersJson]]])` 的原生实现。
/// 同步阻塞在 JS 专用线程上，失败以带 URL 的可读文案抛回 JS。
fn native_ajax(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let text_arg = |i: usize| -> Option<String> {
        args.get(i)
            .and_then(|v| v.as_string())
            .map(|s| s.to_std_string_escaped())
    };
    let url = text_arg(0).unwrap_or_default();
    if url.trim().is_empty() {
        return Err(ajax_error("java.ajax 缺少 url 参数"));
    }
    let method = text_arg(1).unwrap_or_else(|| "GET".into());
    let body = text_arg(2).filter(|s| !s.is_empty());
    let headers: HashMap<String, String> = args
        .get(3)
        .and_then(|v| v.as_string())
        .and_then(|s| serde_json::from_str(&s.to_std_string_escaped()).ok())
        .unwrap_or_default();
    let started = Instant::now();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ajax_error(&format!("java.ajax 初始化运行时失败：{e}")))?;
    // fetch_checked 与普通抓取共用并发/超时/5MiB 上限，并逐跳做私网校验
    let res = rt.block_on(crate::req::fetch_checked(
        &url,
        method.trim(),
        &headers,
        body.as_deref(),
        false,
    ));
    let spent = started.elapsed().as_millis() as u64;
    JS_AJAX_NET_MS.fetch_add(spent, Ordering::Relaxed);
    match res {
        Ok(s) => Ok(boa_engine::JsValue::from(boa_engine::JsString::from(s))),
        Err(e) => Err(ajax_error(&format!("java.ajax 请求失败：{e}"))),
    }
}

fn ajax_error(msg: &str) -> boa_engine::JsError {
    boa_engine::JsError::from_native(
        boa_engine::JsNativeError::error().with_message(msg.to_string()),
    )
}

/// `java.setContent` 之后 DOM 三件套的按需求值（`__nativeSelect(content, rule, wantList)`）。
///
/// 上游的 `java.setContent` 把内容设到同一个 `AnalyzeRule` 上，后续 `getElements` 在该
/// 文档上按当前模式（HTML/CSS 或 JSON/JSONPath）求值；本服务把这一步下沉为原生函数，
/// 惰性解析，不再依赖 eval 前的字面量选择器预抽取。
///
/// 只允许“纯选择器/JSONPath”规则：含 `@js:`、`<js>`、`{{…}}`、`@put/@get` 的规则会在
/// 同一 JS 线程上再次排队，必然死锁，直接返回具名未支持错误。
fn native_select(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let text_arg = |i: usize| -> String {
        args.get(i)
            .and_then(|v| v.as_string())
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default()
    };
    let content = text_arg(0);
    let rule = text_arg(1);
    let want_list = args.get(2).map(|v| v.to_boolean()).unwrap_or(true);
    if rule.trim().is_empty() {
        return Err(ajax_error("java.setContent 后缺少选择器参数"));
    }
    if rule.contains("@js:")
        || rule.contains("<js>")
        || rule.contains("{{")
        || rule.contains("@put:")
        || rule.contains("@get:")
    {
        return Err(ajax_error(
            "UNSUPPORTED_JS_API:java.setContent 后的选择器不能再含 JS/变量段",
        ));
    }
    let ar = AnalyzeRule::new();
    let json = if want_list {
        match ar.get_string_list(&content, &rule) {
            Ok(v) => serde_json::to_string(&v).unwrap_or_else(|_| "[]".into()),
            Err(e) => {
                return Err(ajax_error(&format!("java.setContent 选择器求值失败：{e}")));
            }
        }
    } else {
        match ar.get_string(&content, &rule, false, "") {
            Ok(v) => serde_json::to_string(&v).unwrap_or_else(|_| "\"\"".into()),
            Err(e) => {
                return Err(ajax_error(&format!("java.setContent 选择器求值失败：{e}")));
            }
        }
    };
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(json)))
}

/// 垫片抛出的“未支持 API”统一前缀；worker 把它翻译成用户能读懂的提示。
const UNSUPPORTED_JS_API: &str = "UNSUPPORTED_JS_API:";

/// 把 boa 的求值错误转成可读文案：
/// - 垫片主动抛出的未支持特性 → “该规则使用了未支持的 JS 特性：<名称>（<原因>）”；
/// - 其他语法/运行时错误 → 保留 JS 自己的 error 文本，便于书源作者定位。
fn js_error_message(msg: &str) -> String {
    match msg.find(UNSUPPORTED_JS_API) {
        Some(pos) => {
            let detail = msg[pos + UNSUPPORTED_JS_API.len()..].trim();
            format!("该规则使用了未支持的 JS 特性：{detail}")
        }
        None => format!("js eval: {msg}"),
    }
}

/// 中文数字转阿拉伯数字（章节名用，支持 零~九、十百千万亿、两）。
const CN_TO_NUM_JS: &str = r#"
function cn2num(s){
  if(s==null) return "";
  s=String(s);
  if(/^[0-9]+$/.test(s)) return s;
  var d={'零':0,'一':1,'二':2,'两':2,'三':3,'四':4,'五':5,'六':6,'七':7,'八':8,'九':9};
  var u={'十':10,'百':100,'千':1000,'万':10000,'亿':100000000};
  var total=0,section=0,num=0;
  for(var i=0;i<s.length;i++){
    var ch=s[i];
    if(d[ch]!==undefined){ num=d[ch]; }
    else if(u[ch]!==undefined){
      if(ch==='万'||ch==='亿'){ if(num===0) num=1; section+=num*u[ch]; total+=section; section=0; num=0; }
      else { if(num===0) num=1; section+=num*u[ch]; num=0; }
    } else if(ch==='零'){ num=0; }
  }
  total+=section+num;
  return total===0 ? s : String(total);
}
"#;

/// UTF-8 编码为“字节字符串”，供 base64/hex/转义类函数使用（不依赖 escape/unescape）。
const JS_UTF8_HELPER: &str = r#"
function __utf8(s){
  s = encodeURIComponent(String(s));
  return s.replace(/%([0-9A-Fa-f]{2})/g, function(_, h){ return String.fromCharCode(parseInt(h, 16)); });
}
function __fromUtf8(bin){
  // 等价于 Java 的 new String(bytes, UTF_8)：非法字节用 U+FFFD 替换，绝不抛异常。
  // 之前用 decodeURIComponent 拼接 %xx，遇到脏字节会抛错并返回空串，
  // 导致 base64Decode/hexDecode 对“局部损坏但 ASCII 部分完好”的内容整段丢失。
  var out = "", i = 0, n = bin.length;
  while (i < n) {
    var c = bin.charCodeAt(i);
    if (c < 0x80) { out += String.fromCharCode(c); i += 1; continue; }
    var need, cp;
    if (c >= 0xC2 && c <= 0xDF) { need = 1; cp = c & 0x1F; }
    else if (c >= 0xE0 && c <= 0xEF) { need = 2; cp = c & 0x0F; }
    else if (c >= 0xF0 && c <= 0xF4) { need = 3; cp = c & 0x07; }
    else { out += "\uFFFD"; i += 1; continue; }
    if (i + need >= n) { out += "\uFFFD"; i += 1; continue; }
    var ok = true;
    for (var k = 1; k <= need; k++) {
      var cc = bin.charCodeAt(i + k);
      if ((cc & 0xC0) !== 0x80) { ok = false; break; }
      cp = (cp << 6) | (cc & 0x3F);
    }
    if (!ok) { out += "\uFFFD"; i += 1; continue; }
    if ((need === 1 && cp < 0x80) || (need === 2 && cp < 0x800) ||
        (need === 3 && cp < 0x10000) || cp > 0x10FFFF ||
        (cp >= 0xD800 && cp <= 0xDFFF)) {
      out += "\uFFFD"; i += 1; continue;
    }
    if (cp > 0xFFFF) {
      cp -= 0x10000;
      out += String.fromCharCode(0xD800 + (cp >> 10), 0xDC00 + (cp & 0x3FF));
    } else {
      out += String.fromCharCode(cp);
    }
    i += need + 1;
  }
  return out;
}
// 纯 JS base64，避免依赖宿主是否提供 btoa/atob（boa 0.19 默认不带）
var __B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
function __b64enc(bin, urlSafe, pad){
  var alpha = urlSafe ? __B64URL : __B64;
  var out = "", i;
  for (i = 0; i < bin.length; i += 3) {
    var c1 = bin.charCodeAt(i);
    var c2 = i + 1 < bin.length ? bin.charCodeAt(i + 1) : 0;
    var c3 = i + 2 < bin.length ? bin.charCodeAt(i + 2) : 0;
    var e1 = c1 >> 2, e2 = ((c1 & 3) << 4) | (c2 >> 4);
    var e3 = ((c2 & 15) << 2) | (c3 >> 6), e4 = c3 & 63;
    out += alpha.charAt(e1) + alpha.charAt(e2);
    out += (i + 1 < bin.length ? alpha.charAt(e3) : "=");
    out += (i + 2 < bin.length ? alpha.charAt(e4) : "=");
  }
  if (pad === false) out = out.replace(/=+$/, "");
  return out;
}
function __b64dec(s){
  s = String(s).replace(/-/g, "+").replace(/_/g, "/").replace(/[^A-Za-z0-9+/=]/g, "");
  var bin = "", i;
  for (i = 0; i < s.length; i += 4) {
    var c1 = __B64.indexOf(s.charAt(i));
    var c2 = __B64.indexOf(s.charAt(i + 1));
    var c3 = s.charAt(i + 2) === "=" ? -1 : __B64.indexOf(s.charAt(i + 2));
    var c4 = s.charAt(i + 3) === "=" ? -1 : __B64.indexOf(s.charAt(i + 3));
    if (c1 < 0 || c2 < 0) continue;
    bin += String.fromCharCode((c1 << 2) | (c2 >> 4));
    if (c3 >= 0) bin += String.fromCharCode(((c2 & 15) << 4) | (c3 >> 2));
    if (c4 >= 0) bin += String.fromCharCode(((c3 & 3) << 6) | c4);
  }
  return bin;
}
// base64 变体（上游 JsExtensions.kt 签名）：
//   base64Encode(str) / base64Encode(str, flags)      flags 默认 2(NO_WRAP)，8=URL_SAFE，1=NO_PADDING
//   base64Decode(str) / base64Decode(str, charset) / base64Decode(str, flags)
//   base64DecodeToByteArray(str[, flags])
var __B64URL = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
function __b64encFlags(bin, flags){
  flags = (flags === undefined || flags === null) ? 2 : Number(flags);
  return __b64enc(bin, (flags & 8) !== 0, (flags & 1) === 0);
}
function __b64Bytes(s){ var bin = __b64dec(s), a = []; for (var i = 0; i < bin.length; i++) a.push(bin.charCodeAt(i)); return a; }
function __hexBin(h){
  h = String(h || "").replace(/[^0-9A-Fa-f]/g, "");
  if (h.length % 2 !== 0) return "";
  var out = "";
  // boa 0.19 没有实现 Annex B 的 String.prototype.substr，这里用 slice
  for (var i = 0; i < h.length; i += 2) out += String.fromCharCode(parseInt(h.slice(i, i + 2), 16));
  return out;
}
function __b64Charset(bin, charset){
  // 只实现 Legado 源里常见的几种；GBK 等无 JS 解码表，退化为 UTF-8（与上游的 GBK 解码有差异）。
  var c = String(charset).toUpperCase().replace(/[_-]/g, "");
  if (c === "UTF8" || c === "") return __fromUtf8(bin);
  if (c === "LATIN1" || c === "ISO88591" || c === "ASCII" || c === "USASCII") return bin;
  if (c === "UTF16LE" || c === "UNICODELITTLE") {
    var out = "";
    for (var i = 0; i + 1 < bin.length; i += 2) out += String.fromCharCode(bin.charCodeAt(i) | (bin.charCodeAt(i + 1) << 8));
    return out;
  }
  return __fromUtf8(bin);
}
"#;

/// 纯 JS MD5（Joseph Myers 实现），避免依赖 boa 原生函数绑定。
/// 输入先经 __utf8 转成字节串，保证中文等非 ASCII 文本的摘要与 Java 侧一致。
const JS_MD5: &str = r#"
function __add32(a, b){ return (a + b) & 0xFFFFFFFF; }
function __cmn(q, a, b, x, s, t){
  a = __add32(__add32(a, q), __add32(x, t));
  return __add32((a << s) | (a >>> (32 - s)), b);
}
function __ff(a, b, c, d, x, s, t){ return __cmn((b & c) | ((~b) & d), a, b, x, s, t); }
function __gg(a, b, c, d, x, s, t){ return __cmn((b & d) | (c & (~d)), a, b, x, s, t); }
function __hh(a, b, c, d, x, s, t){ return __cmn(b ^ c ^ d, a, b, x, s, t); }
function __ii(a, b, c, d, x, s, t){ return __cmn(c ^ (b | (~d)), a, b, x, s, t); }
function __md5blk(s){
  var out = [], i;
  for (i = 0; i < 64; i += 4) {
    out[i >> 2] = s.charCodeAt(i) + (s.charCodeAt(i + 1) << 8) +
      (s.charCodeAt(i + 2) << 16) + (s.charCodeAt(i + 3) << 24);
  }
  return out;
}
function __md5cycle(x, k){
  var a = x[0], b = x[1], c = x[2], d = x[3];
  a = __ff(a,b,c,d,k[0],7,-680876936); d = __ff(d,a,b,c,k[1],12,-389564586);
  c = __ff(c,d,a,b,k[2],17,606105819); b = __ff(b,c,d,a,k[3],22,-1044525330);
  a = __ff(a,b,c,d,k[4],7,-176418897); d = __ff(d,a,b,c,k[5],12,1200080426);
  c = __ff(c,d,a,b,k[6],17,-1473231341); b = __ff(b,c,d,a,k[7],22,-45705983);
  a = __ff(a,b,c,d,k[8],7,1770035416); d = __ff(d,a,b,c,k[9],12,-1958414417);
  c = __ff(c,d,a,b,k[10],17,-42063); b = __ff(b,c,d,a,k[11],22,-1990404162);
  a = __ff(a,b,c,d,k[12],7,1804603682); d = __ff(d,a,b,c,k[13],12,-40341101);
  c = __ff(c,d,a,b,k[14],17,-1502002290); b = __ff(b,c,d,a,k[15],22,1236535329);
  a = __gg(a,b,c,d,k[1],5,-165796510); d = __gg(d,a,b,c,k[6],9,-1069501632);
  c = __gg(c,d,a,b,k[11],14,643717713); b = __gg(b,c,d,a,k[0],20,-373897302);
  a = __gg(a,b,c,d,k[5],5,-701558691); d = __gg(d,a,b,c,k[10],9,38016083);
  c = __gg(c,d,a,b,k[15],14,-660478335); b = __gg(b,c,d,a,k[4],20,-405537848);
  a = __gg(a,b,c,d,k[9],5,568446438); d = __gg(d,a,b,c,k[14],9,-1019803690);
  c = __gg(c,d,a,b,k[3],14,-187363961); b = __gg(b,c,d,a,k[8],20,1163531501);
  a = __gg(a,b,c,d,k[13],5,-1444681467); d = __gg(d,a,b,c,k[2],9,-51403784);
  c = __gg(c,d,a,b,k[7],14,1735328473); b = __gg(b,c,d,a,k[12],20,-1926607734);
  a = __hh(a,b,c,d,k[5],4,-378558); d = __hh(d,a,b,c,k[8],11,-2022574463);
  c = __hh(c,d,a,b,k[11],16,1839030562); b = __hh(b,c,d,a,k[14],23,-35309556);
  a = __hh(a,b,c,d,k[1],4,-1530992060); d = __hh(d,a,b,c,k[4],11,1272893353);
  c = __hh(c,d,a,b,k[7],16,-155497632); b = __hh(b,c,d,a,k[10],23,-1094730640);
  a = __hh(a,b,c,d,k[13],4,681279174); d = __hh(d,a,b,c,k[0],11,-358537222);
  c = __hh(c,d,a,b,k[3],16,-722521979); b = __hh(b,c,d,a,k[6],23,76029189);
  a = __hh(a,b,c,d,k[9],4,-640364487); d = __hh(d,a,b,c,k[12],11,-421815835);
  c = __hh(c,d,a,b,k[15],16,530742520); b = __hh(b,c,d,a,k[2],23,-995338651);
  a = __ii(a,b,c,d,k[0],6,-198630844); d = __ii(d,a,b,c,k[7],10,1126891415);
  c = __ii(c,d,a,b,k[14],15,-1416354905); b = __ii(b,c,d,a,k[5],21,-57434055);
  a = __ii(a,b,c,d,k[12],6,1700485571); d = __ii(d,a,b,c,k[3],10,-1894986606);
  c = __ii(c,d,a,b,k[10],15,-1051523); b = __ii(b,c,d,a,k[1],21,-2054922799);
  a = __ii(a,b,c,d,k[8],6,1873313359); d = __ii(d,a,b,c,k[15],10,-30611744);
  c = __ii(c,d,a,b,k[6],15,-1560198380); b = __ii(b,c,d,a,k[13],21,1309151649);
  a = __ii(a,b,c,d,k[4],6,-145523070); d = __ii(d,a,b,c,k[11],10,-1120210379);
  c = __ii(c,d,a,b,k[2],15,718787259); b = __ii(b,c,d,a,k[9],21,-343485551);
  x[0] = __add32(a, x[0]); x[1] = __add32(b, x[1]);
  x[2] = __add32(c, x[2]); x[3] = __add32(d, x[3]);
}
function __rhex(n){
  var s = "", j;
  for (j = 0; j < 4; j++) {
    s += ("0" + ((n >> (j * 8)) & 255).toString(16)).slice(-2);
  }
  return s;
}
function __md5(s){
  s = __utf8(s);
  var state = [1732584193, -271733879, -1732584194, 271733878], i;
  for (i = 64; i <= s.length; i += 64) { __md5cycle(state, __md5blk(s.substring(i - 64, i))); }
  s = s.substring(i - 64);
  var tail = [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0];
  for (i = 0; i < s.length; i++) { tail[i >> 2] |= s.charCodeAt(i) << ((i % 4) << 3); }
  tail[i >> 2] |= 0x80 << ((i % 4) << 3);
  if (i > 55) { __md5cycle(state, tail); for (i = 0; i < 16; i++) tail[i] = 0; }
  tail[14] = s.length * 8;
  __md5cycle(state, tail);
  var out = "";
  for (i = 0; i < 4; i++) { out += __rhex(state[i]); }
  return out;
}
"#;

/// 给 boa 注入的 java / book 垫片。
/// 覆盖可离线实现的高频内置；依赖网络（ajax/post）、Cookie/WebView 的桥接函数显式抛错，
/// DOM 三件套（getElements/getString/getStringList）在 Phase 1 走 Rust 预抽取查表，
/// 只有字面量选择器可用，动态拼串仍显式抛出具名错误。
/// 简→繁常用字表（供 `java.t2s`）。只覆盖常见字；未收录字原样返回。
const JS_T2S: &str = r#"var __T2S = {"爱":"愛","罢":"罷","摆":"擺","败":"敗","办":"辦","帮":"幫","宝":"寶","报":"報","备":"備","贝":"貝","笔":"筆","毕":"畢","边":"邊","变":"變","标":"標","表":"錶","别":"彆","宾":"賓","补":"補","参":"參","惨":"慘","灿":"燦","仓":"倉","产":"產","长":"長","尝":"嘗","车":"車","彻":"徹","尘":"塵","陈":"陳","衬":"襯","称":"稱","惩":"懲","迟":"遲","冲":"衝","丑":"醜","础":"礎","处":"處","触":"觸","传":"傳","疮":"瘡","闯":"闖","创":"創","纯":"純","词":"詞","从":"從","丛":"叢","窜":"竄","达":"達","带":"帶","贷":"貸","担":"擔","单":"單","胆":"膽","当":"當","党":"黨","荡":"蕩","档":"檔","导":"導","敌":"敵","邓":"鄧","递":"遞","点":"點","电":"電","垫":"墊","钓":"釣","调":"調","叠":"疊","东":"東","动":"動","冻":"凍","斗":"鬥","独":"獨","读":"讀","断":"斷","对":"對","队":"隊","吨":"噸","夺":"奪","堕":"墮","儿":"兒","尔":"爾","发":"發","罚":"罰","阀":"閥","飞":"飛","费":"費","废":"廢","纷":"紛","坟":"墳","奋":"奮","粪":"糞","风":"風","丰":"豐","冯":"馮","凤":"鳳","肤":"膚","妇":"婦","复":"復","该":"該","干":"乾","赶":"趕","冈":"岡","刚":"剛","岗":"崗","纲":"綱","钢":"鋼","个":"個","给":"給","巩":"鞏","沟":"溝","构":"構","购":"購","谷":"穀","顾":"顧","刮":"颳","关":"關","观":"觀","馆":"館","惯":"慣","广":"廣","归":"歸","龟":"龜","规":"規","国":"國","过":"過","汉":"漢","号":"號","轰":"轟","后":"後","胡":"鬍","壶":"壺","沪":"滬","护":"護","划":"劃","怀":"懷","坏":"壞","欢":"歡","环":"環","还":"還","回":"迴","汇":"匯","会":"會","秽":"穢","荤":"葷","浑":"渾","伙":"夥","获":"獲","击":"擊","鸡":"雞","积":"積","极":"極","际":"際","继":"繼","家":"傢","价":"價","艰":"艱","歼":"殲","舰":"艦","姜":"薑","浆":"漿","桨":"槳","奖":"獎","讲":"講","酱":"醬","胶":"膠","浇":"澆","骄":"驕","娇":"嬌","搅":"攪","缴":"繳","觉":"覺","较":"較","阶":"階","节":"節","洁":"潔","结":"結","借":"藉","仅":"僅","惊":"驚","竞":"競","旧":"舊","剧":"劇","据":"據","惧":"懼","卷":"捲","决":"決","绝":"絕","军":"軍","开":"開","克":"剋","壳":"殼","夸":"誇","矿":"礦","亏":"虧","困":"睏","腊":"臘","蜡":"蠟","来":"來","赖":"賴","兰":"蘭","拦":"攔","栏":"欄","烂":"爛","垒":"壘","类":"類","里":"裡","礼":"禮","丽":"麗","厉":"厲","励":"勵","离":"離","历":"歷","帘":"簾","联":"聯","怜":"憐","炼":"煉","练":"練","粮":"糧","两":"兩","辆":"輛","疗":"療","辽":"遼","了":"瞭","猎":"獵","临":"臨","邻":"鄰","岭":"嶺","庐":"廬","芦":"蘆","炉":"爐","陆":"陸","虏":"虜","录":"錄","虑":"慮","乱":"亂","罗":"羅","萝":"蘿","络":"絡","妈":"媽","马":"馬","骂":"罵","买":"買","麦":"麥","卖":"賣","迈":"邁","脉":"脈","猫":"貓","么":"麼","门":"門","闷":"悶","梦":"夢","弥":"彌","觅":"覓","绵":"綿","面":"麵","庙":"廟","灭":"滅","亩":"畝","恼":"惱","脑":"腦","拟":"擬","酿":"釀","鸟":"鳥","聂":"聶","宁":"寧","农":"農","齐":"齊","岂":"豈","启":"啟","气":"氣","弃":"棄","牵":"牽","签":"簽","迁":"遷","窍":"竅","窃":"竊","亲":"親","轻":"輕","庆":"慶","琼":"瓊","穷":"窮","秋":"鞦","区":"區","驱":"驅","趋":"趨","权":"權","劝":"勸","确":"確","让":"讓","扰":"擾","热":"熱","认":"認","荣":"榮","绒":"絨","洒":"灑","伞":"傘","丧":"喪","扫":"掃","涩":"澀","杀":"殺","晒":"曬","纱":"紗","设":"設","绅":"紳","审":"審","声":"聲","胜":"勝","圣":"聖","师":"師","湿":"濕","时":"時","实":"實","适":"適","势":"勢","兽":"獸","书":"書","术":"術","树":"樹","帅":"帥","双":"雙","谁":"誰","顺":"順","说":"說","丝":"絲","松":"鬆","苏":"蘇","诉":"訴","虽":"雖","随":"隨","台":"臺","态":"態","摊":"攤","滩":"灘","坛":"壇","叹":"嘆","汤":"湯","烫":"燙","涛":"濤","体":"體","条":"條","铁":"鐵","听":"聽","厅":"廳","头":"頭","图":"圖","涂":"塗","团":"團","洼":"窪","袜":"襪","弯":"彎","万":"萬","网":"網","韦":"韋","违":"違","围":"圍","为":"為","卫":"衛","温":"溫","闻":"聞","问":"問","稳":"穩","务":"務","雾":"霧","牺":"犧","习":"習","系":"係","戏":"戲","细":"細","虾":"蝦","吓":"嚇","鲜":"鮮","纤":"纖","咸":"鹹","显":"顯","险":"險","现":"現","献":"獻","乡":"鄉","详":"詳","响":"響","向":"嚮","协":"協","挟":"挾","泻":"瀉","谢":"謝","兴":"興","须":"須","许":"許","悬":"懸","选":"選","学":"學","寻":"尋","压":"壓","亚":"亞","严":"嚴","盐":"鹽","阳":"陽","养":"養","样":"樣","痒":"癢","谣":"謠","药":"藥","爷":"爺","业":"業","叶":"葉","页":"頁","义":"義","议":"議","亿":"億","忆":"憶","艺":"藝","隐":"隱","应":"應","营":"營","拥":"擁","优":"優","忧":"憂","邮":"郵","余":"餘","鱼":"魚","与":"與","屿":"嶼","语":"語","吁":"籲","郁":"鬱","誉":"譽","渊":"淵","园":"園","员":"員","圆":"圓","缘":"緣","远":"遠","愿":"願","跃":"躍","云":"雲","杂":"雜","灾":"災","赞":"讚","赃":"贓","脏":"臟","凿":"鑿","枣":"棗","灶":"竈","责":"責","择":"擇","则":"則","泽":"澤","贼":"賊","赠":"贈","扎":"紮","张":"張","帐":"帳","战":"戰","这":"這","针":"針","征":"徵","争":"爭","挣":"掙","症":"癥","证":"證","只":"隻","帜":"幟","质":"質","种":"種","众":"眾","昼":"晝","烛":"燭","筑":"築","庄":"莊","装":"裝","妆":"妝","壮":"壯","状":"狀","准":"準","浊":"濁","总":"總","钻":"鑽","纵":"縱","邹":"鄒","诅":"詛","组":"組","话":"話","记":"記","写":"寫","诗":"詩","请":"請","难":"難","乐":"樂","进":"進","岁":"歲","间":"間","见":"見","龙":"龍","颜":"顏","恋":"戀","泪":"淚","华":"華","画":"畫"};"#;

/// `cache` 全局（Legado: 规则语义 `bindings["cache"] = CacheManager`）。
/// 上游 CacheManager 的方法名是 `get / put / delete`（另有 getFromMemory / putMemory /
/// deleteMemory，CacheManager.kt:60-160），**没有** set/remove/clear。
/// 本实现是每作用域一份的字符串 map：`get` 未命中返回 null（同上游返回 String?），
/// put 的对象/数组按 JSON 串存储（上游存 Any）。
const JS_CACHE: &str = r#"
var __cache = {};
var cache = {
  get: function(k){
    k = String(k);
    return Object.prototype.hasOwnProperty.call(__cache, k) ? __cache[k] : null;
  },
  getFromMemory: function(k){ return cache.get(k); },
  put: function(k, v){
    k = String(k);
    if (typeof v === "string") { __cache[k] = v; return; }
    try { __cache[k] = JSON.stringify(v); } catch (e) { __cache[k] = String(v); }
  },
  putMemory: function(k, v){ cache.put(k, v); },
  delete: function(k){ delete __cache[String(k)]; },
  deleteMemory: function(k){ cache.delete(k); }
};
"#;

/// JS 执行结果里回传 cache 变更的分隔符（结果串 + 分隔符 + cache JSON）。
const JS_CACHE_SENTINEL: &str = "\u{1}F1CACHE\u{1}";

/// 结果串尾部的“规则自身耗时”（毫秒）：预算只该算规则代码，不该算固定垫片/lib 的解析。
const JS_RULE_MS_SENTINEL: &str = "\u{1}F1RULEMS\u{1}";

const JAVA_BOOK_SHIM: &str = r#"
var __vars = {};
// DOM 桥 Phase 1 的预抽取结果（由 Rust 在 eval 前按字面量选择器算好并赋值）
var __DOM = {};
var __DOMS = {};
// java.setContent 设置的“当前文档”。null 表示未设置，仍走原始页面的预抽取表；
// 设置后 java.getElements/getString/getStringList 才按需解析这个文档（惰性求值）。
var __content = null;
var book = {
  getVariable: function(k){ return __vars[k] !== undefined ? __vars[k] : ""; },
  putVariable: function(k,v){ __vars[k] = String(v); }
};
// 本服务不持有书源实例对象（书源配置在 Rust 侧），source.key/getKey 无法给真实值。
// 显式抛出具备名的“未支持”，避免书源作者只看到 TypeErrors: not a callable function。
var source = {
  key: "",
  getKey: function(){ throw new Error("UNSUPPORTED_JS_API:source.getKey（本服务不持有书源实例数据）"); },
  get: function(){ throw new Error("UNSUPPORTED_JS_API:source.get（本服务不持有书源实例数据）"); },
  put: function(){ throw new Error("UNSUPPORTED_JS_API:source.put（本服务不持有书源实例数据）"); }
};
var java = {
  toNumChapter: function(s){ return s==null ? "" : cn2num(String(s)); },
  md5Encode: function(s){ return __md5(s == null ? "" : String(s)); },
  md5Encode16: function(s){ return __md5(s == null ? "" : String(s)).substring(8, 24); },
  md5: function(s){ return java.md5Encode(s); },
  // 简繁转换：常用字表（书源多用于书名/作者检索的繁简互通）。未收录的生僻字原样保留。
  t2s: function(s){
    s = String(s == null ? "" : s);
    var out = "";
    for (var i = 0; i < s.length; i++) { out += (__T2S[s.charAt(i)] || s.charAt(i)); }
    return out;
  },
  base64Encode: function(s, flags){ try { return __b64encFlags(__utf8(s), flags); } catch(e){ return ""; } },
  base64Decode: function(s, arg){
    try {
      var bin = __b64dec(s);
      if (typeof arg === "string") return __b64Charset(bin, arg);
      return __fromUtf8(bin);
    } catch(e){ return ""; }
  },
  base64DecodeToString: function(s){ return java.base64Decode(s); },
  base64DecodeToByteArray: function(s, flags){ try { return __b64Bytes(s); } catch(e){ return []; } },
  encodeURI: function(s){ return encodeURIComponent(String(s)); },
  decodeURI: function(s){ try { return decodeURIComponent(String(s)); } catch(e){ return ""; } },
  encodeURIComponent: function(s){ return encodeURIComponent(String(s)); },
  decodeURIComponent: function(s){ try { return decodeURIComponent(String(s)); } catch(e){ return ""; } },
  strToInt: function(s){ var n = parseInt(String(s), 10); return isNaN(n) ? 0 : n; },
  intToStr: function(n){ return String(n); },
  hexEncode: function(s){
    var b = __utf8(s), out = "";
    for (var i = 0; i < b.length; i++) { out += ("0" + b.charCodeAt(i).toString(16)).slice(-2); }
    return out;
  },
  hexEncodeToString: function(s){ return java.hexEncode(s); },
  hexDecode: function(s){ try { return __fromUtf8(__hexBin(s)); } catch(e){ return ""; } },
  hexDecodeToString: function(s){ return java.hexDecode(s); },
  hexDecodeToByteArray: function(s){
    try {
      var bin = __hexBin(s), a = [];
      for (var i = 0; i < bin.length; i++) a.push(bin.charCodeAt(i));
      return a;
    } catch(e){ return []; }
  },
  random: function(max){
    if (max === undefined || max === null) return Math.random();
    return Math.floor(Math.random() * Number(max));
  },
  randomInt: function(min, max){
    min = Number(min || 0); max = Number(max);
    return min + Math.floor(Math.random() * (max - min));
  },
  timeFormat: function(t, f){
    if (typeof t === "string" && f === undefined) { f = t; t = undefined; }
    if (t === undefined || t === null || t === "") t = Date.now();
    var n = Number(t), d;
    if (!isNaN(n)) {
      // Legado 的 time 多为毫秒；小于 1e11 视为秒级时间戳
      d = new Date(n < 100000000000 ? n * 1000 : n);
    } else { d = new Date(String(t)); }
    if (isNaN(d.getTime())) return "";
    var p = function(x, l){ x = String(x); while (x.length < (l || 2)) x = "0" + x; return x; };
    var map = {
      yyyy: p(d.getFullYear(), 4), MM: p(d.getMonth() + 1), dd: p(d.getDate()),
      HH: p(d.getHours()), mm: p(d.getMinutes()), ss: p(d.getSeconds()),
      SSS: p(d.getMilliseconds(), 3)
    };
    f = f || "yyyy-MM-dd HH:mm:ss";
    return String(f).replace(/yyyy|MM|dd|HH|mm|ss|SSS/g, function(k){ return map[k]; });
  },
  htmlFormat: function(s){
    return String(s == null ? "" : s).replace(/&/g, "&amp;").replace(/</g, "&lt;")
      .replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
  },
  htmlFormat2: function(s){
    var t = String(s == null ? "" : s).replace(/<br\s*\/?>/gi, "\n");
    t = t.replace(/<[^>]+>/g, "");
    return t.replace(/&nbsp;/g, " ").replace(/&lt;/g, "<").replace(/&gt;/g, ">")
      .replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, "&");
  },
  escape: function(s){ return escape(String(s)); },
  unescape: function(s){ return unescape(String(s)); },
  put: function(k, v){ book.putVariable(k, v); },
  get: function(k){ return book.getVariable(k); },
  getVariable: function(k){ return book.getVariable(k); },
  putVariable: function(k, v){ book.putVariable(k, v); },
  deleteVariable: function(k){ delete __vars[k]; },
  log: function(){},
  toast: function(){},
  longToast: function(){},
  // Legado: java.ajax(url[, method[, body[, headers]]]) -> 响应文本。
  // 由 Rust 原生 __nativeAjax 同步联网，复用 req 的 15s 超时 / 5MiB / 私网策略。
  ajax: function(url, method, body, headers){
    return __nativeAjax(String(url == null ? "" : url),
      method == null ? "GET" : String(method),
      body == null ? "" : String(body),
      headers == null ? "" : (typeof headers === "string" ? headers : JSON.stringify(headers)));
  },
  post: function(url, body, headers){ return java.ajax(url, "POST", body, headers); },
  connect: function(){ throw new Error("UNSUPPORTED_JS_API:java.connect（本服务无 socket 桥接）"); },
  // DOM 桥：`java.setContent(html)` 之后按需在“当前文档”上求值（Rust 原生解析，
  // 不再依赖预抽取的只读表）；未 setContent 时仍用 Phase 1 的字面量选择器快路。
  setContent: function(html){ __content = (html == null) ? "" : String(html); },
  getElements: function(rule, index){
    var key = rule == null ? "" : String(rule);
    if (__content !== null) {
      var arr = JSON.parse(__nativeSelect(__content, key, true));
      if (typeof index === "number") {
        return (index >= 0 && index < arr.length) ? arr[index] : "";
      }
      return arr;
    }
    if (rule == null || !__DOM.hasOwnProperty(key)) {
      throw new Error("UNSUPPORTED_JS_API:java.getElements（仅支持字面量选择器参数）");
    }
    var arr = __DOM[key].slice(0);
    if (typeof index === "number") {
      return (index >= 0 && index < arr.length) ? arr[index] : "";
    }
    return arr;
  },
  getStringList: function(rule){
    var key = rule == null ? "" : String(rule);
    if (__content !== null) {
      return JSON.parse(__nativeSelect(__content, key, true));
    }
    if (rule == null || !__DOM.hasOwnProperty(key)) {
      throw new Error("UNSUPPORTED_JS_API:java.getStringList（仅支持字面量选择器参数）");
    }
    return __DOM[key].slice(0);
  },
  getString: function(rule){
    var key = rule == null ? "" : String(rule);
    if (__content !== null) {
      return JSON.parse(__nativeSelect(__content, key, false));
    }
    if (rule == null || !__DOMS.hasOwnProperty(key)) {
      throw new Error("UNSUPPORTED_JS_API:java.getString（仅支持字面量选择器参数）");
    }
    return __DOMS[key];
  },
  getCookie: function(){ throw new Error("UNSUPPORTED_JS_API:java.getCookie（本服务无 Cookie 桥接）"); },
  setCookie: function(){ throw new Error("UNSUPPORTED_JS_API:java.setCookie（本服务无 Cookie 桥接）"); },
  webView: function(){ throw new Error("UNSUPPORTED_JS_API:java.webView（本服务无 WebView）"); },
  webViewGetSource: function(){ throw new Error("UNSUPPORTED_JS_API:java.webViewGetSource（本服务无 WebView）"); },
  webViewGetOverrideUrl: function(){ throw new Error("UNSUPPORTED_JS_API:java.webViewGetOverrideUrl（本服务无 WebView）"); }
};
"#;

/// DOM 桥上限：只作用于“已经抓到的页面”，输入体积、节点数、抽取总量都设硬上界，
/// 避免大列表把预抽取结果塞进 JS 源里造成内存/解析放大。
const DOM_MAX_INPUT_BYTES: usize = 1024 * 1024;
const DOM_MAX_NODES: usize = 2000;

/// 扫描 `java.getElements/getStringList/getString("字面量")` 调用。
/// 只提取字面量选择器；动态拼串不预处理，运行时由垫片返回具名“未支持”提示。
fn scan_dom_calls(code: &str) -> Vec<(String, String)> {
    let re = Regex::new(
        r#"java\.(getElements|getStringList|getString)\s*\(\s*(?:"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)')"#,
    )
    .unwrap();
    let mut out = Vec::new();
    for caps in re.captures_iter(code) {
        let kind = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let raw = caps
            .get(2)
            .or_else(|| caps.get(3))
            .map(|m| m.as_str())
            .unwrap_or("");
        out.push((kind.to_string(), js_unescape(raw)));
    }
    out
}

/// 还原 JS 字符串字面量里的常见转义（真实书源基本只用到这些）。
fn js_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let mut hex = String::new();
                for _ in 0..4 {
                    if let Some(h) = it.next() {
                        hex.push(h);
                    }
                }
                if let Ok(n) = u32::from_str_radix(&hex, 16) {
                    if let Some(ch) = char::from_u32(n) {
                        out.push(ch);
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[derive(Debug)]
pub enum RuleError {
    // 历史上 CSS 选择器解析失败用此变体；当前统一走 `Other`，变体保留以稳定错误分类。
    #[allow(dead_code)]
    Selector(String),
    Json(String),
    Regex(String),
    Other(String),
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleError::Selector(s) => write!(f, "selector: {s}"),
            RuleError::Json(s) => write!(f, "json: {s}"),
            RuleError::Regex(s) => write!(f, "regex: {s}"),
            RuleError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for RuleError {}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Css,
    XPath,
    Json,
    Js,
    Regex,
    Default,
}

pub struct AnalyzeRule {
    pub variables: RefCell<HashMap<String, String>>,
    /// Legado JS 环境里的 `cache`（上游 规则语义 `bindings["cache"] = CacheManager`）。
    /// 最小等价版：同作用域可读写的字符串 map。上游 CacheManager 是 App 级单例 + SQLite，
    /// 本服务**每作用域一份**（随 AnalyzeRule 存活），不共享、不持久化。
    pub cache: RefCell<HashMap<String, String>>,
    /// 跨请求的 book 层变量（Legado `book.putVariable`，上游 规则语义）。
    /// `@put:{key:rule}` 除了写当前 `variables`，也写这里；路由层在请求结束后按
    /// `book_key` 落库，下一次同书请求（info → toc → content）预载回来。
    pub book_vars: RefCell<HashMap<String, String>>,
    /// 当前作用域所属的 book_key（由路由层/编排层设置，None 表示没有 book 维度）。
    pub book_key: RefCell<Option<String>>,
    pub js_lib: String,
    pub is_json: bool,
}

impl Default for AnalyzeRule {
    fn default() -> Self {
        Self {
            variables: RefCell::new(HashMap::new()),
            cache: RefCell::new(HashMap::new()),
            book_vars: RefCell::new(HashMap::new()),
            book_key: RefCell::new(None),
            js_lib: String::new(),
            is_json: false,
        }
    }
}

impl AnalyzeRule {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_var(&self, k: impl Into<String>, v: impl Into<String>) {
        self.variables.borrow_mut().insert(k.into(), v.into());
    }

    /// 批量注入变量；生产路径逐条调用 `set_var`，本方法仅单测使用。
    #[allow(dead_code)]
    pub fn set_vars(&self, m: HashMap<String, String>) {
        self.variables.borrow_mut().extend(m);
    }

    /// 进入一本书的作用域：清空请求内变量，载入该书持久化的变量与 cache 快照。
    /// 之后 `@put` 同时写入 `variables`（本请求可见）与 `book_vars`（请求结束落库）。
    pub fn begin_book_scope(
        &self,
        key: String,
        vars: HashMap<String, String>,
        cache: HashMap<String, String>,
    ) {
        let mut v = self.variables.borrow_mut();
        v.clear();
        v.extend(vars.iter().map(|(k, val)| (k.clone(), val.clone())));
        self.book_vars.replace(vars);
        self.cache.replace(cache);
        self.book_key.replace(Some(key));
    }

    pub fn book_scope_key(&self) -> Option<String> {
        self.book_key.borrow().clone()
    }

    pub fn book_vars_snapshot(&self) -> HashMap<String, String> {
        self.book_vars.borrow().clone()
    }

    pub fn cache_snapshot(&self) -> HashMap<String, String> {
        self.cache.borrow().clone()
    }

    /// 变量/JSONPath 模板展开：支持 `${var}`、`{{var}}`，以及 `{{$.field}}` / `${$.field}` / `{$.field}`
    /// （JSONPath 形式按**当前规则输入** `content` 求值，用于书源里
    /// `.../read?bid={{$.id}}`、`https://api.x/topics/{$.id}` 这类“列表项字段拼进 URL/POST body”的写法）。
    fn expand_vars(&self, rule: &str, content: &str) -> String {
        let re = Regex::new(r"\$\{([^}]+)\}|\{\{([^}]+)\}\}|\{(\$[^}]+)\}").unwrap();
        let json: Option<Value> = if rule.contains("$.") || rule.contains("$[") {
            serde_json::from_str(content).ok()
        } else {
            None
        };
        re.replace_all(rule, |caps: &regex::Captures| {
            // `${…}`：只在确实是引擎变量/JSONPath 时替换；未知名字原样保留——
            // 它通常是 JS 模板字符串（快看 `x=>`<img src="${x},${headers}">``），
            // 替换成空串会把 JS 的插值一起吃掉。
            if let Some(m) = caps.get(1) {
                let key = m.as_str().trim();
                if key.starts_with("$.") || key.starts_with("$[") {
                    if let Some(v) = json.as_ref() {
                        if let Ok(mut vals) = json_query(v, key) {
                            if let Some(x) = vals.pop() {
                                return value_to_string(x);
                            }
                        }
                    }
                    return String::new();
                }
                if let Some(v) = self.variables.borrow().get(key).cloned() {
                    return v;
                }
                return caps.get(0).unwrap().as_str().to_string();
            }
            let key = caps
                .get(2)
                .or_else(|| caps.get(3))
                .map(|m| m.as_str())
                .unwrap_or("");
            let key = key.trim();
            if key.starts_with("$.") || key.starts_with("$[") {
                if let Some(v) = json.as_ref() {
                    if let Ok(mut vals) = json_query(v, key) {
                        if let Some(x) = vals.pop() {
                            return value_to_string(x);
                        }
                    }
                }
                return String::new();
            }
            if let Some(v) = self.variables.borrow().get(key).cloned() {
                return v;
            }
            // `{{…}}`：Legado 对非规则内容一律交给 evalJS（`{{page*20}}`、`{{java.base64Encode(…)}}` 等），
            // 变量已在 eval_js 作用域内绑定；JS 失败时退化为空串。
            if caps.get(2).is_some() {
                if let Ok(s) = self.eval_js(content, key, "") {
                    return s;
                }
            }
            String::new()
        })
        .to_string()
    }

    fn detect_mode(rule: &str, is_json: bool, content: &str) -> Mode {
        // Legado 的 `@CSS:`/`@XPath:`/`@Json:` 前缀全部大小写不敏感。
        if starts_with_ci(rule, "@xpath:") {
            Mode::XPath
        } else if starts_with_ci(rule, "@json:") {
            Mode::Json
        } else if starts_with_ci(rule, "@css:") || rule.starts_with("@@") {
            Mode::Css
        } else if rule.starts_with("$.") || rule.starts_with("$[") {
            Mode::Json
        } else if is_json && !content.trim_start().starts_with('<') {
            // 源级 JSON 提示只对 JSON 内容生效：HTML 页面上的 `img@data-src`
            // 不能被强行当成 JSONPath（快看图片规则就栽在这里）。
            Mode::Json
        } else if rule.starts_with('/') {
            // `/.../`（含替换形式 `/pat/repl/`）判定为正则；普通 XPath 不以 / 结尾
            if rule.len() > 1 && rule.ends_with('/') {
                Mode::Regex
            } else {
                Mode::XPath
            }
        } else if rule.starts_with(':') {
            Mode::Regex
        } else {
            Mode::Css
        }
    }

    /// 整串层切分：抽出 `<js>…</js>` 与 `@js:…` 段作为 JS，其余作为默认模式
    fn split_js(rule: &str) -> Vec<(Mode, String)> {
        let mut parts = Vec::new();
        let mut last = 0usize;
        let mut i = 0usize;
        let n = rule.len();
        while i < n {
            // 只在 ASCII 字节处做标记匹配，避免多字节 UTF-8 内部切片越界
            if rule.as_bytes()[i] < 0x80 {
                let s = &rule[i..];
                if s.starts_with("<js>") {
                    if i > last {
                        parts.push((Mode::Default, rule[last..i].to_string()));
                    }
                    let code_start = i + 4;
                    match find_js_tag_end(rule, code_start) {
                        Some(end) => {
                            parts.push((Mode::Js, rule[code_start..end].to_string()));
                            i = end + 5;
                            last = i;
                            continue;
                        }
                        None => {
                            // 未闭合 `<js>`：与旧行为一致，剩余内容整体按 JS 处理
                            parts.push((Mode::Js, rule[code_start..].to_string()));
                            return parts;
                        }
                    }
                }
                if starts_with_ci(s, "@js:") {
                    if i > last {
                        parts.push((Mode::Default, rule[last..i].to_string()));
                    }
                    parts.push((Mode::Js, rule[i + 4..].to_string()));
                    return parts;
                }
            }
            i += 1;
        }
        if last < n {
            parts.push((Mode::Default, rule[last..].to_string()));
        }
        parts
    }

    /// 大小写不敏感的前缀剥离（Legado `startsWith(..., ignoreCase = true)`）。
    fn strip_prefix<'a>(rule: &'a str, p: &str) -> &'a str {
        if starts_with_ci(rule, p) {
            &rule[p.len()..]
        } else {
            rule
        }
    }

    /// Legado `@put:{key:rule}` / `@get:{key}`：
    /// - 上游 `AnalyzeRule.splitPutRule`（L516，正则 L1027 `@put:(\{[^}]+?\})`，大小写不敏感）
    ///   先把 `@put:{…}` 从规则正文里摘掉；求值时 `putRule`（L507）把每个 value 当**完整规则**
    ///   在当前 content 上求值后 `put(key, value)`（L856；chapter→book→ruleData→source 依次落库）。
    /// - `@get:{key}` 由 `makeUpRule`（L698/L718）替换成变量值，未命中返回空串（`get` L875）。
    /// - key 允许下划线；`value` 走完整求值链（含 `@js:`、`$.path`）；put 失败不影响主结果。
    ///
    /// 本服务没有 Book/Chapter 实体，作用域退化为一次 `AnalyzeRule`（一次请求内新建）的
    /// 变量表，不写任何全局状态，因此不同源/不同请求天然隔离。
    fn apply_put_get(&self, rule: &str, content: &str, is_url: bool, base_url: &str) -> String {
        if !rule.contains("@put:")
            && !rule.contains("@get:")
            && !rule.contains("@PUT:")
            && !rule.contains("@GET:")
        {
            return rule.to_string();
        }
        let put_re = Regex::new(r"(?i)@put:\{([^}]*)\}").unwrap();
        let mut text = rule.to_string();
        for caps in put_re.captures_iter(rule) {
            let whole = caps.get(0).map(|m| m.as_str()).unwrap_or("");
            let inner = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            // 先摘掉操作符本身（与上游 splitPutRule 一致），失败也不影响剩余规则
            text = text.replacen(whole, "", 1);
            for (key, value_rule) in split_put_pairs(inner) {
                let key = key.trim();
                if key.is_empty() {
                    continue;
                }
                let value = value_rule.trim().to_string();
                if let Ok(v) = self.get_string_inner(content, &value, is_url, base_url) {
                    self.variables
                        .borrow_mut()
                        .insert(key.to_string(), v.clone());
                    // book 层持久化：即使本轮没有 book_key（如搜索阶段），也留在
                    // book_vars 里等待编排层落库，不跨请求丢失。
                    self.book_vars.borrow_mut().insert(key.to_string(), v);
                }
            }
        }
        let get_re = Regex::new(r"(?i)@get:\{([^}]*)\}").unwrap();
        get_re
            .replace_all(&text, |caps: &regex::Captures| {
                let key = caps.get(1).map(|m| m.as_str()).unwrap_or("").trim();
                self.variables
                    .borrow()
                    .get(key)
                    .cloned()
                    .unwrap_or_default()
            })
            .to_string()
    }

    /// 单值入口：对应 Legado `getString(..., unescape = true)`。
    /// HTML4 实体只在**最外层**反转义一次；内部递归走 `get_string_inner`，避免重复反转义。
    pub fn get_string(
        &self,
        content: &str,
        rule: &str,
        is_url: bool,
        base_url: &str,
    ) -> Result<String, RuleError> {
        let raw = self.get_string_inner(content, rule, is_url, base_url)?;
        Ok(unescape_html4(&raw))
    }

    /// 单值管道（不做 HTML 反转义）：对 content 应用一条规则串，逐段执行，上一步结果作为下一步输入
    fn get_string_inner(
        &self,
        content: &str,
        rule: &str,
        is_url: bool,
        base_url: &str,
    ) -> Result<String, RuleError> {
        // 整条规则只有 `@get:{key}`：直接返回变量值（否则替换后的字面量会被当成 CSS 选择器）。
        let raw = rule.trim();
        let only_get = (starts_with_ci(raw, "@get:") && raw.ends_with('}'))
            && !raw[5..].contains('@')
            && raw[5..].starts_with('{');
        let rule = self.apply_put_get(rule, content, is_url, base_url);
        if only_get {
            return Ok(rule.trim().to_string());
        }
        let rule = self.expand_vars(&rule, content);
        let rule = rule.trim().to_string();
        if rule.is_empty() {
            return Ok(String::new());
        }
        let rule = rule.as_str();
        // 已是完整 URL 的模板（如 `https://api.x/read?bid={{$.id}}`）：展开后直接返回，
        // 不再当 CSS 选择器解析（否则 JSON 源里“字面量 URL”形态的 bookUrl 会得到空串）。
        if is_url
            && (rule.starts_with("http://")
                || rule.starts_with("https://")
                || rule.starts_with("//"))
        {
            return Ok(rule.to_string());
        }
        // 字面量 JSON 请求体模板（`{"bid":"{{$.id}}"}`）：展开后原样作为 body 返回，
        // 不能当 CSS 选择器解析。
        if rule.starts_with('{') && rule.ends_with('}') && rule.contains(':') {
            return Ok(rule.to_string());
        }
        // @get:{key} / @put:{key:value} 已在 `apply_put_get` 里按 Legado 的
        // `splitPutRule` + `makeUpRule` 语义处理（可出现在规则任意位置，不只整串）。
        // 顶层操作符（不在 <js>…</js> / @js: 段内）。
        // 顺序与 Legado 一致：先在 makeUpRule 里拆 `##`，再交给解析器处理 `||`/`&&`/`%%`。
        if let Some(parts) = split_outside_js(rule, "##") {
            let sub = self.get_string_inner(content, parts[0].trim(), is_url, base_url)?;
            let pattern = parts.get(1).copied().unwrap_or("");
            if pattern.is_empty() {
                return Ok(sub);
            }
            let replacement = parts.get(2).copied().unwrap_or("");
            let replace_first = parts.len() > 3;
            return Ok(apply_replace_regex(
                &sub,
                pattern,
                replacement,
                replace_first,
            ));
        }
        if let Some(parts) = split_outside_js(rule, "||") {
            for p in &parts {
                let mut v = self.eval_list(content, p, base_url);
                if !v.is_empty() {
                    return Ok(if is_url { v.remove(0) } else { v.join("\n") });
                }
            }
            return Ok(String::new());
        }
        if let Some(parts) = split_outside_js(rule, "&&") {
            // Legado 的 `&&` 是「列表合并」：多值用 `\n` 连接，而不是空串粘连。
            let mut items: Vec<String> = Vec::new();
            for p in &parts {
                items.extend(self.eval_list(content, p, base_url));
            }
            if is_url && !items.is_empty() {
                return Ok(items.remove(0));
            }
            return Ok(items.join("\n"));
        }
        if let Some(parts) = split_outside_js(rule, "%%") {
            let lists: Vec<Vec<String>> = parts
                .iter()
                .map(|p| self.eval_list(content, p, base_url))
                .collect();
            let mut items: Vec<String> = Vec::new();
            if let Some(first) = lists.first() {
                for i in 0..first.len() {
                    for l in &lists {
                        if let Some(x) = l.get(i) {
                            items.push(x.clone());
                        }
                    }
                }
            }
            if is_url && !items.is_empty() {
                return Ok(items.remove(0));
            }
            return Ok(items.join("\n"));
        }
        // 裸抽取标记：`text` / `href` / `src` 等没有选择器前缀时，表示“取当前节点”。
        // 不能当成 CSS 标签选择器（否则章节标题/链接会解析成空）。
        let bare = rule.trim();
        if !self.is_json && is_extract_keyword(bare) {
            return Ok(extract_first(content, bare));
        }
        // `@text` / `@href` / `@src` / `@html` 与裸标记等价：都作用于“当前节点”，
        // 属性不存在时返回空串（不能当 CSS 标签选择器解析成空）。
        if !self.is_json {
            if let Some(k) = bare.strip_prefix('@') {
                if is_extract_keyword(k) {
                    return Ok(extract_first(content, k));
                }
            }
        }
        // 默认：<js> / @js: 段切分管道
        let mut input = content.to_string();
        for (mode, text) in Self::split_js(rule) {
            let t = text.trim();
            if t.is_empty() {
                continue;
            }
            input = match mode {
                Mode::Js => self.eval_js(&input, t, base_url)?,
                _ => {
                    let m = if mode == Mode::Default {
                        Self::detect_mode(t, self.is_json, &input)
                    } else {
                        mode
                    };
                    self.apply_by_mode(&input, m, t, is_url)?
                }
            };
        }
        Ok(input)
    }

    fn apply_by_mode(
        &self,
        input: &str,
        mode: Mode,
        rule: &str,
        first_only: bool,
    ) -> Result<String, RuleError> {
        match mode {
            Mode::XPath => self.apply_xpath(input, Self::strip_prefix(rule, "@xpath:")),
            Mode::Json => self.apply_json(input, Self::strip_prefix(rule, "@json:")),
            Mode::Regex => self.apply_regex(input, rule),
            _ => self.apply_css(input, Self::strip_prefix(rule, "@css:"), first_only),
        }
    }

    fn apply_css(&self, html: &str, rule: &str, first_only: bool) -> Result<String, RuleError> {
        let (selector, tokens) = split_css_rule(rule);
        let doc = parse_css_document(html);
        let mut els = select_initial(&doc, &selector);
        // 依次处理遍历标记（@tag.x / @children / 裸标签 / !下标），遇到末端抽取标记即停止
        let mut terminal: Option<String> = None;
        for tok in &tokens {
            let tok = *tok;
            if tok.is_empty() {
                continue;
            }
            match parse_token(tok) {
                Token::Terminal(t) => {
                    terminal = Some(t);
                    break;
                }
                Token::Children => {
                    els = els.into_iter().flat_map(child_elements).collect();
                }
                Token::Tag {
                    name,
                    index,
                    indices,
                    exclude,
                } => {
                    let mut m = apply_tag(els, &build_spec(&name, index));
                    m = filter_indices(m, &indices, exclude);
                    els = m;
                }
            }
        }
        // 单值抽取：Legado `JSoup 规则.getString` = getResultLast 收集**全部**匹配
        // 元素的值（空值/重复属性跳过），多值再按 `\n` 合并；isUrl 走 getString 只取首个。
        match terminal {
            Some(t) => {
                let vals = terminal_values(&els, &t);
                Ok(if first_only {
                    vals.into_iter().next().unwrap_or_default()
                } else {
                    vals.join("\n")
                })
            }
            None => Ok(els.first().map(|e| e.inner_html()).unwrap_or_default()),
        }
    }

    fn apply_json(&self, json: &str, rule: &str) -> Result<String, RuleError> {
        let v: Value = serde_json::from_str(json).map_err(|e| RuleError::Json(e.to_string()))?;
        let vals = json_query(&v, rule)?;
        // Legado 的 getString 对多值结果按 `\n` 拼接（`$..image` 这类递归路径会命中多个节点）。
        // 此前只取最后一个，导致「章节多图」被压成 1 张，源规则里 `@js:result.split("\n")` 也只剩一行。
        if vals.is_empty() {
            return Ok(String::new());
        }
        Ok(vals
            .into_iter()
            .map(value_to_string)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn apply_regex(&self, input: &str, rule: &str) -> Result<String, RuleError> {
        // 斜杠包裹规则：/pat/ 为提取（捕获组1），/pat/repl/ 为替换
        if let Some(interior) = rule.strip_prefix('/') {
            // interior 含结尾斜杠
            let slash_count = interior.matches('/').count();
            if slash_count >= 2 {
                // 替换形式：/pat/repl/
                let end1 = interior.find('/').unwrap();
                let re_str = &interior[..end1];
                let replacement = interior[end1 + 1..].trim_end_matches('/');
                let re = Regex::new(re_str).map_err(|e| RuleError::Regex(e.to_string()))?;
                return Ok(re.replace_all(input, replacement).to_string());
            }
            // 提取形式：/pat/
            let re_str = interior.trim_end_matches('/');
            let re = Regex::new(re_str).map_err(|e| RuleError::Regex(e.to_string()))?;
            if let Some(c) = re.captures(input) {
                if let Some(m) = c.get(1) {
                    return Ok(m.as_str().to_string());
                }
                return Ok(c.get(0).unwrap().as_str().to_string());
            }
            return Ok(String::new());
        }
        let re = Regex::new(rule).map_err(|e| RuleError::Regex(e.to_string()))?;
        if let Some(c) = re.captures(input) {
            if let Some(m) = c.get(1) {
                return Ok(m.as_str().to_string());
            }
            return Ok(c.get(0).unwrap().as_str().to_string());
        }
        Ok(String::new())
    }

    fn apply_xpath(&self, content: &str, rule: &str) -> Result<String, RuleError> {
        use sxd_document::parser::parse as xml_parse;
        use sxd_xpath::evaluate_xpath;
        let package =
            xml_parse(content).map_err(|e| RuleError::Other(format!("xpath parse: {e:?}")))?;
        let doc = package.as_document();
        let val = evaluate_xpath(&doc, rule)
            .map_err(|e| RuleError::Other(format!("xpath eval: {e:?}")))?;
        Ok(match val {
            sxd_xpath::Value::String(s) => s,
            sxd_xpath::Value::Number(n) => n.to_string(),
            sxd_xpath::Value::Boolean(b) => b.to_string(),
            sxd_xpath::Value::Nodeset(ns) => {
                let mut s = String::new();
                for n in ns.iter() {
                    s.push_str(&n.string_value());
                }
                s
            }
        })
    }

    /// DOM 桥 Phase 1：按规则里出现的字面量选择器，在当前页面 HTML 上预抽取。
    /// 返回 (`getElements`/`getStringList` 的 HTML 列表映射, `getString` 的单值映射)。
    fn dom_bridge(
        &self,
        input: &str,
        code: &str,
        base_url: &str,
    ) -> (
        serde_json::Map<String, serde_json::Value>,
        serde_json::Map<String, serde_json::Value>,
    ) {
        let mut lists = serde_json::Map::new();
        let mut singles = serde_json::Map::new();
        // 页面超过上限不做预抽取：垫片会返回“仅支持字面量选择器”的具名提示
        if input.len() > DOM_MAX_INPUT_BYTES {
            return (lists, singles);
        }
        let mut total_bytes = 0usize;
        for (kind, sel) in scan_dom_calls(code) {
            let sel = sel.trim().to_string();
            // 选择器自身含 JS 段时不预处理，避免递归
            if sel.is_empty() || sel.contains("@js:") || sel.contains("<js>") {
                continue;
            }
            if lists.contains_key(&sel) || singles.contains_key(&sel) {
                continue;
            }
            if kind == "getString" {
                if let Ok(v) = self.get_string(input, &sel, false, base_url) {
                    singles.insert(sel, serde_json::Value::String(v));
                }
                continue;
            }
            let Ok(mut vals) = self.get_string_list(input, &sel) else {
                continue;
            };
            vals.truncate(DOM_MAX_NODES);
            total_bytes += vals.iter().map(|v| v.len()).sum::<usize>();
            if total_bytes > DOM_MAX_INPUT_BYTES {
                break;
            }
            lists.insert(
                sel,
                serde_json::Value::Array(vals.into_iter().map(serde_json::Value::String).collect()),
            );
        }
        (lists, singles)
    }

    /// 把规则变量注入 JS 作用域（等价于 Legado evalJS 前绑定 page/title 等变量）。
    fn js_vars(&self) -> String {
        const RESERVED: &[&str] = &[
            "result",
            "__input",
            "__base",
            "baseUrl",
            "__DOM",
            "__DOMS",
            "__r",
            "__content",
            "__nativeSelect",
            "__nativeAjax",
            "java",
            "cache",
            "__cache",
            "__out",
        ];
        let mut out = String::new();
        for (k, v) in self.variables.borrow().iter() {
            let mut chars = k.chars();
            let ok_first = chars
                .next()
                .map(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
                .unwrap_or(false);
            if !ok_first
                || !k
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
                || RESERVED.contains(&k.as_str())
            {
                continue;
            }
            out.push_str("var ");
            out.push_str(k);
            out.push('=');
            out.push_str(&js_string_literal(v));
            out.push_str(";\n");
        }
        out
    }

    fn eval_js(&self, input: &str, code: &str, base_url: &str) -> Result<String, RuleError> {
        let conf = js_conf();
        if !conf.enabled {
            return Err(RuleError::Other("JS rules disabled by JS_ENABLED=0".into()));
        }
        // 整段包成 IIFE，避免不同书源/不同次执行之间的全局变量污染
        let shim = format!(
            "{}\n{}\n{}\n{}\n{}",
            CN_TO_NUM_JS, JS_UTF8_HELPER, JS_MD5, JS_T2S, JAVA_BOOK_SHIM
        );
        // DOM 桥 Phase 1：把规则里“字面量选择器”的求值结果预抽取成 JSON 映射，
        // 注入到同一个 IIFE 作用域；JS 里 java.getElements 只做查表，不再抛错。
        let (dom, doms) = self.dom_bridge(input, code, base_url);
        let dom_js = serde_json::to_string(&dom).unwrap_or_else(|_| "{}".into());
        let doms_js = serde_json::to_string(&doms).unwrap_or_else(|_| "{}".into());
        let vars_js = self.js_vars();
        let cache_init =
            serde_json::to_string(&self.cache.borrow().clone()).unwrap_or_else(|_| "{}".into());
        let script = format!(
            "(function(){{ var result={r}; var __input={r}; var __base={b}; var baseUrl=__base;\n{vars}\n{cachejs}\n{lib}\n{shim}\n__cache={cacheinit};\n__DOM={dom};\n__DOMS={doms};\nvar __t0 = Date.now();\nvar __r = eval({code});\nvar __ms = Date.now() - __t0;\nvar __out;\nif (Array.isArray(__r)) {{ try {{ __out = JSON.stringify(__r); }} catch(__e) {{}} }}\nelse __out = String(__r);\nif (typeof __out === 'undefined') __out = String(__r);\nreturn __out + {sent} + JSON.stringify(__cache) + {msent} + String(__ms); }})()",
            r = js_string_literal(input),
            b = js_string_literal(base_url),
            vars = vars_js,
            cachejs = JS_CACHE,
            lib = self.js_lib,
            shim = shim,
            cacheinit = cache_init,
            dom = dom_js,
            doms = doms_js,
            code = js_string_literal(code),
            sent = js_string_literal(JS_CACHE_SENTINEL),
            msent = js_string_literal(JS_RULE_MS_SENTINEL),
        );
        let (tx, rx) = channel::<(Result<String, String>, u64, u64)>();
        js_sender()
            .send(JsJob { script, reply: tx })
            .map_err(|_| RuleError::Other("js worker unavailable".into()))?;
        // 慢 JS 只阻塞本次规则求值，不再占住 tokio worker（见 `wait_js_reply`）。
        let (result, net_ms, js_elapsed) = wait_js_reply(rx)?;
        // 结果串尾部依次带回 cache 变更与规则自身耗时（非法/超大都忽略，不影响主结果）
        let mut rule_ms: Option<u64> = None;
        let result = match result {
            Ok(mut s) => {
                if let Some(idx) = s.rfind(JS_RULE_MS_SENTINEL) {
                    let tail = s[idx + JS_RULE_MS_SENTINEL.len()..].trim().to_string();
                    if let Ok(ms) = tail.parse::<u64>() {
                        rule_ms = Some(ms);
                    }
                    s.truncate(idx);
                }
                if let Some(idx) = s.rfind(JS_CACHE_SENTINEL) {
                    let tail = s[idx + JS_CACHE_SENTINEL.len()..].to_string();
                    if tail.len() <= 1_048_576 {
                        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&tail) {
                            self.cache.replace(map);
                        }
                    }
                    s.truncate(idx);
                }
                Ok(s)
            }
            Err(e) => Err(e),
        };
        let (result, net_ms) = (result.map_err(RuleError::Other), net_ms);
        // 预算只算规则自身耗时（页面里 Date.now 包裹 eval 的那段），固定垫片/lib 的
        // 解析成本与机器负载相关，不该算到源作者头上；`java.ajax` 联网时间也扣掉。
        // 拿不到回传耗时（脚本抛错等）时退回整段耗时，保守但不会放过慢规则。
        let budget_ms = rule_ms.map_or(js_elapsed, |ms| ms).saturating_sub(net_ms);
        // 排队等单一 JS 线程的时间不计入预算：并发测试/并发请求下同一规则仍应通过。
        JS_LAST_MS.store(js_elapsed, Ordering::Relaxed);
        JS_MAX_MS.fetch_max(budget_ms, Ordering::Relaxed);
        // 已经报错的脚本保留原始错误（如 RuntimeLimit），不要被“超预算”盖掉
        if result.is_ok() && budget_ms > conf.timeout_ms {
            tracing::warn!(
                elapsed_ms = js_elapsed,
                rule_ms = budget_ms,
                net_ms,
                budget_ms = conf.timeout_ms,
                "js rule exceeded time budget"
            );
            return Err(RuleError::Other(format!(
                "js eval exceeded {}ms budget (took {}ms)",
                conf.timeout_ms, budget_ms
            )));
        }
        result
    }

    /// 列表：返回多个元素的字符串（HTML 取 inner_html；JSON 取数组元素字符串）
    pub fn get_string_list(&self, content: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        let rule = self.apply_put_get(rule, content, false, "");
        let rule = self.expand_vars(&rule, content);
        // Legado 约定：规则以 `-` 开头表示「结果倒序」（多见于章节列表，如 `-$..comics.*`）。
        if let Some(rest) = rule.trim_start().strip_prefix('-') {
            let mut v = self.get_string_list(content, rest.trim_start())?;
            v.reverse();
            return Ok(v);
        }
        // 列表级 `||` 回退：第一个非空列表胜出（如 `$..list[*]||$.books[*]`）
        if let Some(parts) = split_outside_js(&rule, "||") {
            for p in &parts {
                let v = self.get_string_list(content, p)?;
                if !v.is_empty() {
                    return Ok(v);
                }
            }
            return Ok(vec![]);
        }
        // 混合管道：`<js>预处理</js>$..list[*]` 先用 JS 改写输入，再把尾段按**列表**语义求值
        let js_parts = Self::split_js(&rule);
        if js_parts.len() >= 2 && js_parts.iter().any(|(m, _)| *m == Mode::Js) {
            let last = js_parts.len() - 1;
            let mut input = content.to_string();
            for (i, (mode, text)) in js_parts.iter().enumerate() {
                let t = text.trim();
                if t.is_empty() {
                    continue;
                }
                if i == last {
                    if *mode == Mode::Default {
                        return self.get_string_list(&input, t);
                    }
                    let res = self.eval_js(&input, t, "")?;
                    if res.starts_with('[') {
                        if let Ok(arr) = serde_json::from_str::<Vec<String>>(&res) {
                            return Ok(arr);
                        }
                    }
                    return Ok(vec![res]);
                }
                input = match mode {
                    Mode::Js => self.eval_js(&input, t, "")?,
                    _ => self.get_string_inner(&input, t, false, "")?,
                };
            }
            return Ok(vec![input]);
        }
        let r = rule.trim_start();
        if starts_with_ci(r, "@js:") || contains_ci(r, "@js:") || rule.contains("<js>") {
            // 走与单值相同的分段管道：`CSS前缀<js>…</js>` 只把 JS 段交给求值器，
            // 不再把 `<js>` 标签和前缀一起塞进 boa（否则会在 `<js>` 处语法报错）。
            let res = self.get_string_inner(content, &rule, false, "")?;
            // 整段 JS 常返回数组（如 java.getElements 的结果），序列化为 JSON 数组则拆成多项
            if res.starts_with('[') {
                if let Ok(arr) = serde_json::from_str::<Vec<String>>(&res) {
                    return Ok(arr);
                }
            }
            return Ok(vec![res]);
        }
        let mode = Self::detect_mode(&rule, self.is_json, content);
        match mode {
            Mode::Json => self.json_list(content, Self::strip_prefix(&rule, "@json:")),
            _ => self.css_list(content, Self::strip_prefix(&rule, "@css:")),
        }
    }

    /// 列表求值（保留末端抽取标记）：用于 `nextTocUrl` / 漫画图片等“可能返回多条”的规则。
    /// 与 Legado `getStringList` 对齐：`||` 取首个非空列表、`&&` 合并列表、`%%` 交错，
    /// `##` 先解析出列表再**逐项**替换，`@js:` 结果按数组或换行拆项。
    pub fn get_string_list_all(&self, content: &str, rule: &str, base_url: &str) -> Vec<String> {
        let rule = self.apply_put_get(rule, content, false, base_url);
        let rule = self.expand_vars(&rule, content);
        let r = rule.trim();
        if r.is_empty() {
            return vec![];
        }
        self.eval_list(content, r, base_url)
    }

    /// 列表层求值：先按 Legado 的优先级切分操作符，再调用单段解析。
    fn eval_list(&self, content: &str, rule: &str, base_url: &str) -> Vec<String> {
        let rule = rule.trim();
        if rule.is_empty() {
            return vec![];
        }
        // `##` 在解析完成后逐项应用（`getStringList` 的语义，不是先把列表拼成字符串）
        if let Some(parts) = split_outside_js(rule, "##") {
            let mut list = self.eval_list(content, parts[0].trim(), base_url);
            let pattern = parts.get(1).copied().unwrap_or("");
            if !pattern.is_empty() {
                let replacement = parts.get(2).copied().unwrap_or("");
                let replace_first = parts.len() > 3;
                list = list
                    .into_iter()
                    .map(|s| apply_replace_regex(&s, pattern, replacement, replace_first))
                    .collect();
            }
            return list;
        }
        // `||`：第一个非空列表胜出
        if let Some(parts) = split_outside_js(rule, "||") {
            for p in &parts {
                let v = self.eval_list(content, p, base_url);
                if !v.is_empty() {
                    return v;
                }
            }
            return vec![];
        }
        // `&&`：列表拼接
        if let Some(parts) = split_outside_js(rule, "&&") {
            let mut out = Vec::new();
            for p in &parts {
                out.extend(self.eval_list(content, p, base_url));
            }
            return out;
        }
        // `%%`：按首个列表的下标交错合并
        if let Some(parts) = split_outside_js(rule, "%%") {
            let lists: Vec<Vec<String>> = parts
                .iter()
                .map(|p| self.eval_list(content, p, base_url))
                .collect();
            let mut out = Vec::new();
            if let Some(first) = lists.first() {
                for i in 0..first.len() {
                    for l in &lists {
                        if let Some(x) = l.get(i) {
                            out.push(x.clone());
                        }
                    }
                }
            }
            return out;
        }
        self.eval_segment_list(content, rule, base_url)
    }

    /// 不含 `&&`/`||`/`%%`/`##` 的单段列表求值。
    fn eval_segment_list(&self, content: &str, rule: &str, base_url: &str) -> Vec<String> {
        let r = rule.trim();
        if r.is_empty() {
            return vec![];
        }
        // JS 段：数组按 JSON 拆项，普通字符串按 `\n` 拆（Legado getStringList 对 String 的处理）
        if starts_with_ci(r, "@js:") || contains_ci(r, "@js:") || r.contains("<js>") {
            let js_parts = Self::split_js(r);
            if js_parts.len() >= 2 {
                let last = js_parts.len() - 1;
                let mut input = content.to_string();
                for (i, (mode, text)) in js_parts.iter().enumerate() {
                    let t = text.trim();
                    if t.is_empty() {
                        continue;
                    }
                    if i == last {
                        return match self.eval_js(&input, t, base_url) {
                            Ok(s) if !s.is_empty() => {
                                if s.starts_with('[') {
                                    if let Ok(arr) = serde_json::from_str::<Vec<String>>(&s) {
                                        return arr;
                                    }
                                }
                                s.split('\n').map(|x| x.to_string()).collect()
                            }
                            _ => vec![],
                        };
                    }
                    if *mode == Mode::Js {
                        input = self.eval_js(&input, t, base_url).unwrap_or_default();
                    } else {
                        // 列表语义：JSONPath 指向数组时先摊平成多项，再以 `\n` 交给 JS。
                        // 爱看 `$.data.current_chapter.chapter_img_list@js:result.split("\n")…`
                        // 依赖的就是「每条图片 URL 一行」的输入（上游 getStringList 的 List 语义）。
                        input = self.eval_list(&input, t, base_url).join("\n");
                    }
                }
            }
            return match self.get_string_inner(content, r, false, base_url) {
                Ok(s) if !s.is_empty() => {
                    if s.starts_with('[') {
                        if let Ok(arr) = serde_json::from_str::<Vec<String>>(&s) {
                            return arr;
                        }
                    }
                    s.split('\n').map(|x| x.to_string()).collect()
                }
                _ => vec![],
            };
        }
        match Self::detect_mode(r, self.is_json, content) {
            Mode::Json => self
                .json_list(content, Self::strip_prefix(r, "@json:"))
                .unwrap_or_default(),
            Mode::Css => self.css_extract_all(content, r),
            _ => match self.get_string_inner(content, r, false, base_url) {
                Ok(s) if !s.trim().is_empty() => vec![s],
                _ => vec![],
            },
        }
    }

    /// 纯 CSS 列表抽取：对每个匹配元素分别取末端标记（`@href` / `@text` / `@all` 等）。
    fn css_extract_all(&self, html: &str, rule: &str) -> Vec<String> {
        let (selector, tokens) = split_css_rule(rule);
        let doc = scraper::Html::parse_document(html);
        let mut els = select_initial(&doc, &selector);
        let mut terminal: Option<String> = None;
        for tok in &tokens {
            let tok = *tok;
            if tok.is_empty() {
                continue;
            }
            match parse_token(tok) {
                Token::Terminal(t) => {
                    terminal = Some(t);
                    break;
                }
                Token::Children => {
                    els = els.into_iter().flat_map(child_elements).collect();
                }
                Token::Tag {
                    name,
                    index,
                    indices,
                    exclude,
                } => {
                    let mut m = apply_tag(els, &build_spec(&name, index));
                    m = filter_indices(m, &indices, exclude);
                    els = m;
                }
            }
        }
        match terminal {
            Some(t) if t == "textNodes" => els.iter().map(|e| text_nodes(e)).collect(),
            Some(t) => els.iter().map(|e| extract_token(*e, &t)).collect(),
            None => els.iter().map(|e| e.html()).collect(),
        }
    }

    fn css_list(&self, html: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        Ok(elements_html(html, rule))
    }

    fn json_list(&self, json: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        let v: Value = serde_json::from_str(json).map_err(|e| RuleError::Json(e.to_string()))?;
        let mut out = Vec::new();
        // JSONPath 直接命中数组时（`$['hits']` / `$.data.topics`）按数组元素展开：
        // Legado 的 getElements 会把 List 的每个元素当作列表项，不把整个数组当成一项。
        for val in json_query(&v, rule)? {
            match val {
                Value::Array(a) => {
                    out.extend(a.into_iter().filter(|x| !x.is_null()).map(value_to_string))
                }
                Value::Null => {}
                other => out.push(value_to_string(other)),
            }
        }
        Ok(out)
    }
}

/// 生成安全的 JS 字符串字面量（复用 serde_json 转义，并处理 JS 行分隔符）。
fn js_string_literal(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| "\"\"".to_string())
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// 把 JSON 元素转成纯文本：字符串取内部值（不含 JSON 引号），其余 JSON 序列化。
fn value_to_string(v: Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Legado 常用 JSONPath 子集：`$.a.b` / `$..field`（递归下降）/ `[*]` / `[n]` / `['key']`。
#[derive(Debug, Clone, PartialEq)]
enum JpSeg {
    Key(String),
    Index(i64),
    Wildcard,
    RecursiveKey(String),
    RecursiveWildcard,
    /// 过滤谓词 `[?(@.field=='x' && @.n>1)]` / `[?(@.a=='x' || @.b=='y')]`
    Filter {
        conds: Vec<JpCond>,
        any: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
enum JpOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone, PartialEq)]
struct JpCond {
    path: Vec<String>,
    op: JpOp,
    value: Value,
}

/// 找到与 `open` 处 `[` 配对的 `]`：跳过引号与内层方括号（谓词里可能有 `@['k']`）。
fn find_close_bracket(cs: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (i, &c) in cs.iter().enumerate().skip(open) {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// 按顶层 `&&` 或 `||` 切分谓词；两种混用视为不支持（需要括号优先级）。
fn split_predicate(body: &str) -> Result<(Vec<String>, bool), String> {
    let cs: Vec<char> = body.chars().collect();
    let mut quote: Option<char> = None;
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut op: Option<bool> = None; // Some(true)=||, Some(false)=&&
    let mut i = 0usize;
    while i < cs.len() {
        let c = cs[i];
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            cur.push(c);
            i += 1;
            continue;
        }
        if (c == '&' || c == '|') && i + 1 < cs.len() && cs[i + 1] == c {
            let is_any = c == '|';
            if op.map(|o| o != is_any).unwrap_or(false) {
                return Err("谓词不支持混用 && 与 ||".to_string());
            }
            op = Some(is_any);
            parts.push(cur.trim().to_string());
            cur.clear();
            i += 2;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    parts.push(cur.trim().to_string());
    Ok((parts, op.unwrap_or(false)))
}

/// 解析 `@.a.b` / `@['a']` 形式的字段路径。
fn parse_cond_path(raw: &str) -> Result<Vec<String>, String> {
    let s = raw.trim();
    let mut rest = s
        .strip_prefix('@')
        .ok_or_else(|| format!("谓词字段必须以 @ 开头：{s}"))?;
    let mut out = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if let Some(r) = rest.strip_prefix('.') {
            let end = r.find(['.', '[']).unwrap_or(r.len());
            let key = r[..end].trim();
            if key.is_empty() {
                return Err(format!("谓词字段名为空：{s}"));
            }
            out.push(key.to_string());
            rest = &r[end..];
        } else if let Some(r) = rest.strip_prefix('[') {
            let end = r
                .find(']')
                .ok_or_else(|| format!("谓词字段方括号未闭合：{s}"))?;
            let key = unquote_arg(&r[..end]);
            if key.is_empty() {
                return Err(format!("谓词字段名为空：{s}"));
            }
            out.push(key);
            rest = &r[end + 1..];
        } else {
            return Err(format!("谓词字段无法解析：{s}"));
        }
    }
    if out.is_empty() {
        return Err(format!("谓词字段为空：{s}"));
    }
    Ok(out)
}

/// 解析谓词比较值：字符串 / 数字 / true / false / null。
fn parse_pred_value(raw: &str) -> Result<Value, String> {
    let s = raw.trim();
    if s.len() >= 2
        && ((s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')))
    {
        return Ok(Value::String(unquote_arg(s)));
    }
    match s {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        "null" => return Ok(Value::Null),
        _ => {}
    }
    if let Ok(n) = s.parse::<i64>() {
        return Ok(Value::from(n));
    }
    if let Ok(f) = s.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Ok(Value::Number(n));
        }
    }
    Err(format!("不支持的谓词比较值：{s}"))
}

/// 解析单条谓词：`@.field == 'x'` / `@.price>=10`（空格可选）。
fn parse_cond(raw: &str) -> Result<JpCond, String> {
    let s = raw.trim();
    let ops = ["==", "!=", ">=", "<=", ">", "<"];
    let mut found: Option<(usize, &str)> = None;
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            continue;
        }
        for op in ops {
            if s[i..].starts_with(op) {
                found = Some((i, op));
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    let (pos, op) = found.ok_or_else(|| format!("谓词缺少比较运算符：{s}"))?;
    let path = parse_cond_path(s[..pos].trim())?;
    let rhs = &s[pos + op.len()..];
    let value = parse_pred_value(rhs)?;
    let op = match op {
        "==" => JpOp::Eq,
        "!=" => JpOp::Ne,
        ">=" => JpOp::Ge,
        "<=" => JpOp::Le,
        ">" => JpOp::Gt,
        _ => JpOp::Lt,
    };
    Ok(JpCond { path, op, value })
}

/// 解析 `?(...)` 里的全部谓词。
fn parse_filter(body: &str) -> Result<(Vec<JpCond>, bool), String> {
    let inner = body.trim();
    if !(inner.starts_with("?(") && inner.ends_with(')')) {
        return Err(format!("谓词必须以 ?(...) 包裹：{body}"));
    }
    let expr = &inner[2..inner.len() - 1];
    let (parts, any) = split_predicate(expr)?;
    let conds = parts
        .iter()
        .map(|p| parse_cond(p))
        .collect::<Result<Vec<_>, _>>()?;
    if conds.is_empty() {
        return Err("谓词表达式为空".to_string());
    }
    Ok((conds, any))
}

/// 解析 JSONPath 子集；不支持的语法返回可读中文原因（不再把 PestError 原样抛给用户）。
fn parse_json_path(path: &str) -> Result<Vec<JpSeg>, String> {
    let s = path.trim();
    let cs: Vec<char> = s.chars().collect();
    if cs.first() != Some(&'$') {
        return Err(format!("路径必须以 $ 开头：{s}"));
    }
    let read_key = |start: usize| -> (String, usize) {
        let mut i = start;
        while i < cs.len() && !matches!(cs[i], '.' | '[' | ']' | '*') {
            i += 1;
        }
        (cs[start..i].iter().collect::<String>(), i)
    };
    let mut segs = Vec::new();
    let mut i = 1usize;
    while i < cs.len() {
        match cs[i] {
            '.' => {
                if cs.get(i + 1) == Some(&'.') {
                    // 递归下降：`$..books` / `$..*`
                    i += 2;
                    if cs.get(i) == Some(&'*') {
                        segs.push(JpSeg::RecursiveWildcard);
                        i += 1;
                    } else {
                        let (k, ni) = read_key(i);
                        if k.is_empty() {
                            return Err(format!("递归下降缺少字段名：{s}"));
                        }
                        segs.push(JpSeg::RecursiveKey(k));
                        i = ni;
                    }
                } else {
                    i += 1;
                    if cs.get(i) == Some(&'*') {
                        segs.push(JpSeg::Wildcard);
                        i += 1;
                    } else if cs.get(i) == Some(&'[') {
                        // Legado 用 Jayway json-path，容忍 `$.data.[*]` 这类多余的点
                        continue;
                    } else {
                        let (k, ni) = read_key(i);
                        if k.is_empty() {
                            return Err(format!("字段名为空：{s}"));
                        }
                        segs.push(JpSeg::Key(k));
                        i = ni;
                    }
                }
            }
            '[' => {
                let end = find_close_bracket(&cs, i).ok_or_else(|| format!("方括号未闭合：{s}"))?;
                let inner: String = cs[i + 1..end].iter().collect();
                let inner = inner.trim();
                if inner == "*" {
                    segs.push(JpSeg::Wildcard);
                } else if let Ok(n) = inner.parse::<i64>() {
                    segs.push(JpSeg::Index(n));
                } else if inner.starts_with("?(") {
                    let (conds, any) = parse_filter(inner)?;
                    segs.push(JpSeg::Filter { conds, any });
                } else if (inner.starts_with('\'') && inner.ends_with('\''))
                    || (inner.starts_with('"') && inner.ends_with('"'))
                {
                    segs.push(JpSeg::Key(inner[1..inner.len() - 1].to_string()));
                } else {
                    return Err(format!("不支持的方括号表达式 [{inner}]"));
                }
                i = end + 1;
            }
            '*' => {
                segs.push(JpSeg::Wildcard);
                i += 1;
            }
            c => return Err(format!("无法识别的字符 '{c}'：{s}")),
        }
    }
    Ok(segs)
}

fn jp_recursive_key(v: &Value, key: &str, out: &mut Vec<Value>) {
    match v {
        Value::Object(m) => {
            if let Some(x) = m.get(key) {
                out.push(x.clone());
            }
            for x in m.values() {
                jp_recursive_key(x, key, out);
            }
        }
        Value::Array(a) => {
            for x in a {
                jp_recursive_key(x, key, out);
            }
        }
        _ => {}
    }
}

fn jp_recursive_all(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(m) => {
            for x in m.values() {
                out.push(x.clone());
                jp_recursive_all(x, out);
            }
        }
        Value::Array(a) => {
            for x in a {
                out.push(x.clone());
                jp_recursive_all(x, out);
            }
        }
        _ => {}
    }
}

fn jp_num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn jp_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 谓词比较：两侧都能转数字时按数值比较，否则按字符串（只支持 ==/!=）。
fn jp_compare(a: &Value, b: &Value, op: &JpOp) -> bool {
    let nums = jp_num(a).zip(jp_num(b));
    match op {
        JpOp::Eq | JpOp::Ne => {
            let eq = match nums {
                Some((x, y)) => x == y,
                None => jp_text(a) == jp_text(b),
            };
            if *op == JpOp::Eq {
                eq
            } else {
                !eq
            }
        }
        _ => {
            let (x, y) = match nums {
                Some(v) => v,
                None => return false,
            };
            match op {
                JpOp::Gt => x > y,
                JpOp::Ge => x >= y,
                JpOp::Lt => x < y,
                JpOp::Le => x <= y,
                _ => false,
            }
        }
    }
}

fn jp_matches(cond: &JpCond, el: &Value) -> bool {
    let mut cur = el;
    for k in &cond.path {
        cur = match cur {
            Value::Object(m) => match m.get(k) {
                Some(v) => v,
                None => return false,
            },
            Value::Array(a) => match k.parse::<usize>().ok().and_then(|i| a.get(i)) {
                Some(v) => v,
                None => return false,
            },
            _ => return false,
        };
    }
    jp_compare(cur, &cond.value, &cond.op)
}

fn jp_filter_match(conds: &[JpCond], any: bool, el: &Value) -> bool {
    if any {
        conds.iter().any(|c| jp_matches(c, el))
    } else {
        conds.iter().all(|c| jp_matches(c, el))
    }
}

fn eval_json_path(root: &Value, segs: &[JpSeg]) -> Vec<Value> {
    let mut cur = vec![root.clone()];
    for seg in segs {
        let mut next = Vec::new();
        match seg {
            JpSeg::Key(k) => {
                for v in &cur {
                    if let Value::Object(m) = v {
                        if let Some(x) = m.get(k) {
                            next.push(x.clone());
                        }
                    }
                }
            }
            JpSeg::Index(n) => {
                for v in &cur {
                    if let Value::Array(a) = v {
                        let idx = if *n < 0 { a.len() as i64 + *n } else { *n };
                        if idx >= 0 && (idx as usize) < a.len() {
                            next.push(a[idx as usize].clone());
                        }
                    }
                }
            }
            JpSeg::Wildcard => {
                for v in &cur {
                    match v {
                        Value::Array(a) => next.extend(a.clone()),
                        Value::Object(m) => next.extend(m.values().cloned()),
                        _ => {}
                    }
                }
            }
            JpSeg::RecursiveKey(k) => {
                for v in &cur {
                    jp_recursive_key(v, k, &mut next);
                }
            }
            JpSeg::RecursiveWildcard => {
                for v in &cur {
                    jp_recursive_all(v, &mut next);
                }
            }
            JpSeg::Filter { conds, any } => {
                for v in &cur {
                    match v {
                        Value::Array(a) => {
                            for el in a {
                                if jp_filter_match(conds, *any, el) {
                                    next.push(el.clone());
                                }
                            }
                        }
                        // 对象也允许过滤（命中则保留对象本身），与部分 JSONPath 实现一致
                        Value::Object(_) if jp_filter_match(conds, *any, v) => {
                            next.push(v.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
        cur = next;
    }
    cur
}

/// 对 JSON 执行 JSONPath：先走本地子集解析，含过滤表达式等复杂写法再退回 jsonpath-rust；
/// 两条路径的语法错误都统一成“JSONPath 语法不受支持：…”，不暴露 PestError。
/// 按顶层 `&&` 切分（跳过引号与 [] / () 内部，过滤表达式里的 && 不拆）。
fn split_top_level_and(rule: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut last = 0usize;
    let bytes = rule.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == q && (i == 0 || bytes[i - 1] != b'\\') {
                    quote = None;
                }
            }
            None => match c {
                b'\'' | b'"' => quote = Some(c),
                b'[' | b'(' => depth += 1,
                b']' | b')' => depth -= 1,
                b'&' if depth == 0 && bytes.get(i + 1) == Some(&b'&') => {
                    parts.push(rule[last..i].trim());
                    last = i + 2;
                    i += 2;
                    continue;
                }
                _ => {}
            },
        }
        i += 1;
    }
    if parts.is_empty() {
        None
    } else {
        parts.push(rule[last..].trim());
        Some(parts)
    }
}

fn json_query(v: &Value, rule: &str) -> Result<Vec<Value>, RuleError> {
    let rule = rule.trim();
    // Legado 的 JSONPath `&&` 表示多个路径结果合并（`$.a.*&&$.b.*`）
    if let Some(parts) = split_top_level_and(rule) {
        let mut out = Vec::new();
        for p in parts {
            out.extend(json_query(v, p)?);
        }
        return Ok(out);
    }
    match parse_json_path(rule) {
        Ok(segs) => Ok(eval_json_path(v, &segs)),
        Err(local_err) => {
            let complex = rule.contains("[?") || rule.contains("](") || rule.contains("&&");
            if complex {
                match v.clone().path(rule) {
                    Ok(Value::Array(arr)) => Ok(arr),
                    Ok(other) => Ok(vec![other]),
                    Err(e) => Err(RuleError::Json(format!("JSONPath 语法不受支持：{e}"))),
                }
            } else {
                Err(RuleError::Json(format!(
                    "JSONPath 语法不受支持：{local_err}"
                )))
            }
        }
    }
}

/// jsoup `Tag.blockTags`（1.16.2，含 `br` 之外的块级元素）。
fn is_jsoup_block_tag(name: &str) -> bool {
    matches!(
        name,
        "html"
            | "head"
            | "body"
            | "frameset"
            | "script"
            | "noscript"
            | "style"
            | "meta"
            | "link"
            | "title"
            | "frame"
            | "noframes"
            | "section"
            | "nav"
            | "aside"
            | "hgroup"
            | "header"
            | "footer"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "pre"
            | "div"
            | "blockquote"
            | "hr"
            | "address"
            | "figure"
            | "figcaption"
            | "form"
            | "fieldset"
            | "ins"
            | "del"
            | "dl"
            | "dt"
            | "dd"
            | "li"
            | "table"
            | "caption"
            | "thead"
            | "tfoot"
            | "tbody"
            | "colgroup"
            | "col"
            | "tr"
            | "th"
            | "td"
            | "video"
            | "audio"
            | "canvas"
            | "details"
            | "menu"
            | "plaintext"
            | "template"
            | "article"
            | "main"
            | "svg"
            | "math"
            | "center"
            | "dir"
            | "applet"
            | "marquee"
            | "listing"
    )
}

/// jsoup `Tag.preserveWhitespaceTags`（1.16.2）。
fn is_jsoup_preserve_ws_tag(name: &str) -> bool {
    matches!(name, "pre" | "plaintext" | "title" | "textarea")
}

/// jsoup `Tag.formatAsInlineTags`（1.16.2）：块级标签但按行内方式排版，
/// `Element.text()` 的 tail 阶段不会在这些元素前补空格。
fn is_jsoup_format_inline_tag(name: &str) -> bool {
    matches!(
        name,
        "title"
            | "a"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "pre"
            | "address"
            | "li"
            | "th"
            | "td"
            | "script"
            | "style"
            | "ins"
            | "del"
            | "s"
    )
}

/// jsoup 的 `isActuallyWhitespace`（只认这几个 ASCII 空白，不把 NBSP/全角空格算进去）。
fn is_jsoup_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C')
}

fn jsoup_last_char_is_ws(accum: &str) -> bool {
    accum.chars().last().map(is_jsoup_ws).unwrap_or(false)
}

/// jsoup `TextNode.appendNormalisedWhitespace`：连续空白折叠成一个空格，
/// `accum` 已以空白结尾时跳过前导空白。
fn append_jsoup_normalised(accum: &mut String, text: &str) {
    let strip_leading = jsoup_last_char_is_ws(accum);
    let mut last_white = false;
    let mut reached_non_white = false;
    for c in text.chars() {
        if is_jsoup_ws(c) {
            if strip_leading && !reached_non_white {
                continue;
            }
            if last_white {
                continue;
            }
            accum.push(' ');
            last_white = true;
        } else {
            accum.push(c);
            reached_non_white = true;
            last_white = false;
        }
    }
}

/// jsoup `Element.text()`（1.16.2）的等价实现：块级元素/`br` 前后补空格、
/// 空白按 `appendNormalisedWhitespace` 折叠、`pre/plaintext/title/textarea` 保留原样。
fn jsoup_text(el: scraper::ElementRef<'_>) -> String {
    let mut accum = String::new();
    // (节点, 是否出栈, 是否处于 preserveWhitespace 子树)
    let mut stack = vec![(*el, false, is_jsoup_preserve_ws_tag(el.value().name()))];
    while let Some((node, exiting, preserve)) = stack.pop() {
        if exiting {
            // jsoup tail：块级元素结束且下一个兄弟是文本或行内元素时补空格
            if let Some(e) = node.value().as_element() {
                if is_jsoup_block_tag(e.name())
                    && !accum.is_empty()
                    && !jsoup_last_char_is_ws(&accum)
                {
                    let need_space = match node.next_sibling() {
                        Some(n) => match n.value() {
                            scraper::node::Node::Text(_) => true,
                            scraper::node::Node::Element(ne) => {
                                !is_jsoup_format_inline_tag(ne.name())
                            }
                            _ => false,
                        },
                        None => false,
                    };
                    if need_space {
                        accum.push(' ');
                    }
                }
            }
            continue;
        }
        match node.value() {
            scraper::node::Node::Text(t) => {
                let s = t.text.to_string();
                if preserve {
                    accum.push_str(&s);
                } else {
                    append_jsoup_normalised(&mut accum, &s);
                }
            }
            scraper::node::Node::Element(e) => {
                let name = e.name();
                // jsoup head：进入块级元素（或 br）时，若已有内容且不以空白结尾则补空格
                if !accum.is_empty()
                    && (is_jsoup_block_tag(name) || name == "br")
                    && !jsoup_last_char_is_ws(&accum)
                {
                    accum.push(' ');
                }
                stack.push((node, true, preserve));
                let kids: Vec<_> = node.children().collect();
                let child_preserve = preserve || is_jsoup_preserve_ws_tag(name);
                for child in kids.into_iter().rev() {
                    stack.push((child, false, child_preserve));
                }
            }
            _ => {
                stack.push((node, true, preserve));
                let kids: Vec<_> = node.children().collect();
                for child in kids.into_iter().rev() {
                    stack.push((child, false, preserve));
                }
            }
        }
    }
    accum.trim().to_string()
}

/// 对单个元素按抽取标记取值（@text/@href/@content/@src/@html/@ownText/@all 等）。
fn extract_token(el: scraper::ElementRef, tok: &str) -> String {
    match tok {
        "text" => jsoup_text(el),
        "ownText" => el.text().collect::<Vec<_>>().join(""),
        "html" => el.inner_html(),
        "all" => el.html(),
        "href" | "src" | "alt" | "content" | "id" | "value" | "class" | "style" | "title"
        | "name" => el.value().attr(tok).unwrap_or("").to_string(),
        // 任意属性（@data-id / @data-src 等）：属性不存在返回空串，不回退成整节点文本
        _ => el.value().attr(tok).unwrap_or("").to_string(),
    }
}

/// CSS 单值解析用的文档构造：表格行/单元格片段要带回 `<table>` 上下文，
/// 否则 html5ever 会按 “in body” 规则丢弃 tr/td，`p:nth-of-type` 等结构选择器会失真。
fn parse_css_document(html: &str) -> scraper::Html {
    let head: String = html
        .trim_start()
        .chars()
        .take(9)
        .collect::<String>()
        .to_ascii_lowercase();
    let table_fragment = [
        "<tr", "<td", "<th", "<tbody", "<thead", "<tfoot", "<caption",
    ]
    .iter()
    .any(|p| head.starts_with(p));
    if table_fragment {
        scraper::Html::parse_document(&format!("<table>{}</table>", html.trim_start()))
    } else {
        scraper::Html::parse_document(html)
    }
}

/// 单值抽取的集合级实现：对应上游 `JSoup 规则.getResultLast`。
/// - text / ownText：逐元素取值，空值跳过；
/// - textNodes：逐元素把直接文本节点 trim 后按 `\n` 合并，空元素跳过；
/// - html：先删 script/style，再取整组元素的 outerHtml（合并为一个值）；
/// - all：整组元素 outerHtml（合并为一个值）；
/// - 其余按属性名逐个取值，空值与重复值跳过。
fn terminal_values(els: &[scraper::ElementRef], tok: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    match tok {
        "text" => {
            for el in els {
                let v = extract_token(*el, "text");
                if !v.is_empty() {
                    out.push(v);
                }
            }
        }
        "ownText" => {
            for el in els {
                let v: String = el
                    .children()
                    .filter_map(|n| n.value().as_text().map(|t| t.to_string()))
                    .collect();
                if !v.is_empty() {
                    out.push(v);
                }
            }
        }
        "textNodes" => {
            for el in els {
                let mut tn: Vec<String> = Vec::new();
                for n in el.children() {
                    if let Some(t) = n.value().as_text() {
                        let s = t.trim();
                        if !s.is_empty() {
                            tn.push(s.to_string());
                        }
                    }
                }
                if !tn.is_empty() {
                    out.push(tn.join("\n"));
                }
            }
        }
        "html" => {
            let joined: String = els
                .iter()
                .map(|e| outer_html_without_script_style(*e))
                .collect();
            if !joined.is_empty() {
                out.push(joined);
            }
        }
        "all" => {
            let joined: String = els.iter().map(|e| e.html()).collect();
            out.push(joined);
        }
        _ => {
            for el in els {
                let v = el.value().attr(tok).unwrap_or("").to_string();
                if v.trim().is_empty() || out.contains(&v) {
                    continue;
                }
                out.push(v);
            }
        }
    }
    out
}

/// 上游 `getResultLast("html")` 的取法：剔除元素内的 script/style 后取 outerHtml。
fn outer_html_without_script_style(el: scraper::ElementRef) -> String {
    if matches!(el.value().name(), "script" | "style") {
        return String::new();
    }
    static STRIP: OnceLock<(Regex, Regex)> = OnceLock::new();
    let (script, style) = STRIP.get_or_init(|| {
        (
            Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>").unwrap(),
            Regex::new(r"(?is)<style\b[^>]*>.*?</style\s*>").unwrap(),
        )
    });
    let outer = el.html();
    let html = script.replace_all(&outer, "");
    style.replace_all(&html, "").to_string()
}

/// 抽取/遍历 token 解析结果。
enum Token {
    Children,
    Tag {
        name: String,
        index: Option<i64>,
        indices: Vec<usize>,
        /// Legado 的 `!` 是“排除这些下标”，而不是倒序索引（`li!0:1` = 去掉前两个）。
        exclude: bool,
    },
    Terminal(String),
}

/// 已知的末端抽取关键字（@text/@href/@html/@all 等）。
fn is_extract_keyword(tok: &str) -> bool {
    matches!(
        tok,
        "text"
            | "ownText"
            | "textNodes"
            | "html"
            | "all"
            | "href"
            | "src"
            | "content"
            | "alt"
            | "id"
            | "value"
            | "class"
            | "style"
            | "title"
            | "name"
    )
}

/// 解析一个 `@` 之后的 traversal/terminal token（支持裸标签、`tag.`、`!indices`）。
fn parse_token(tok: &str) -> Token {
    if tok == "children" || tok == "child" {
        return Token::Children;
    }
    // 下标筛选：dd!0:1:2 或 tag.a!0:1（`!` 表示排除）
    if let Some(pos) = tok.find('!') {
        let (name, idxs) = tok.split_at(pos);
        let name = name.strip_prefix("tag.").unwrap_or(name);
        let indices: Vec<usize> = idxs[1..]
            .split(':')
            .filter_map(|x| x.parse::<usize>().ok())
            .collect();
        return Token::Tag {
            name: name.to_string(),
            index: None,
            indices,
            exclude: true,
        };
    }
    if let Some(rest) = tok.strip_prefix("tag.") {
        let (name, index) = split_name_index(rest);
        return Token::Tag {
            name: shorthand_segment(&name).to_string(),
            index,
            indices: vec![],
            exclude: false,
        };
    }
    if is_extract_keyword(tok) {
        return Token::Terminal(tok.to_string());
    }
    let (raw_name, index) = split_name_index(tok);
    // Legado 的 `class.xxx` / `id.xxx` 是类/ID 选择器简写，不是末端抽取标记
    let name = if let Some(rest) = raw_name.strip_prefix("class.") {
        class_name_css(shorthand_segment(rest))
    } else if let Some(rest) = raw_name.strip_prefix("id.") {
        format!("#{}", shorthand_segment(rest))
    } else {
        raw_name
    };
    // `@.book.0` / `@#id` 是 Legado 默认规则里的 class/id 简写；此前被当成末端抽取
    // 标记，选择器结果直接丢弃（错层小说网详情 cover/kind 全 null 的原因）。
    let css_shorthand = (name.starts_with('.') || name.starts_with('#')) && name.len() > 1;
    // jQuery 伪类 / 属性选择器同样属于选择器（`@li:eq(0)`、`@a[href]`），不能当抽取标记
    let pseudo_selector = name.contains(':') || name.contains('[');
    if css_shorthand
        || pseudo_selector
        || (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic()))
    {
        return Token::Tag {
            name,
            index,
            indices: vec![],
            exclude: false,
        };
    }
    Token::Terminal(tok.to_string())
}

/// 拼装 apply_tag 用的 spec（name 或 name.index）。
fn build_spec(name: &str, index: Option<i64>) -> String {
    match index {
        Some(i) => format!("{name}.{i}"),
        None => name.to_string(),
    }
}

/// 解析 `0:1:2` 形式的下标列表。
fn parse_indices(s: &str) -> Vec<usize> {
    s.split(':')
        .filter_map(|x| x.trim().parse::<usize>().ok())
        .collect()
}

/// 按 `!` 语义过滤下标：`exclude=true` 时剔除这些下标，否则只保留这些下标。
fn filter_indices<'a>(
    els: Vec<scraper::ElementRef<'a>>,
    indices: &[usize],
    exclude: bool,
) -> Vec<scraper::ElementRef<'a>> {
    if indices.is_empty() {
        return els;
    }
    els.into_iter()
        .enumerate()
        .filter(|(i, _)| indices.contains(i) != exclude)
        .map(|(_, e)| e)
        .collect()
}

/// 按规则选中元素并返回其 outer HTML 列表（供 bookList 嵌套解析与 java.getElements 复用）。
fn elements_html(html: &str, rule: &str) -> Vec<String> {
    let (selector, tokens) = split_css_rule(rule);
    let doc = scraper::Html::parse_document(html);
    let mut els = select_initial(&doc, &selector);
    for tok in &tokens {
        let tok = *tok;
        if tok.is_empty() {
            continue;
        }
        match parse_token(tok) {
            Token::Children => els = els.into_iter().flat_map(child_elements).collect(),
            Token::Tag {
                name,
                index,
                indices,
                exclude,
            } => {
                let mut m = apply_tag(els, &build_spec(&name, index));
                m = filter_indices(m, &indices, exclude);
                els = m;
            }
            Token::Terminal(_) => break, // 列表模式遇到抽取标记即停止遍历
        }
    }
    els.iter().map(|e| e.html()).collect()
}

/// 去掉 @css:/@CSS:/@@ 前缀，并按 '@' 切分为 (选择器, 抽取/遍历标记)。
fn split_css_rule(rule: &str) -> (String, Vec<&str>) {
    let rule = strip_prefix_ci(rule, "@css:")
        .trim_start_matches("@@")
        .trim();
    let parts: Vec<&str> = rule.split('@').collect();
    let selector = parts[0].trim().to_string();
    let tokens: Vec<&str> = parts[1..].iter().map(|s| s.trim()).collect();
    (selector, tokens)
}

/// 拆出 Legado 末尾的 `[n]` / `[n,m]` / `[a:b]` 下标表达式。
fn split_bracket_indices(selector: &str) -> (String, Option<String>) {
    let s = selector.trim();
    if !s.ends_with(']') {
        return (s.to_string(), None);
    }
    if let Some(open) = s.rfind('[') {
        let inner = &s[open + 1..s.len() - 1];
        if inner
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ',' | ':' | '-' | ' '))
        {
            return (s[..open].trim().to_string(), Some(inner.to_string()));
        }
    }
    (s.to_string(), None)
}

/// 对已选元素应用 Legado 下标表达式：`[3,4]` 取第 3/4 个，`[0:2]` 切片，`[-1:0]` 倒序。
fn apply_index_expr<'a>(
    els: Vec<scraper::ElementRef<'a>>,
    expr: &str,
) -> Vec<scraper::ElementRef<'a>> {
    let len = els.len() as i64;
    if len == 0 {
        return els;
    }
    let mut selected: Vec<usize> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    {
        let mut push = |i: i64, selected: &mut Vec<usize>| {
            if i >= 0 && i < len {
                let u = i as usize;
                if seen.insert(u) {
                    selected.push(u);
                }
            }
        };
        for part in expr.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some((start_s, rest)) = part.split_once(':') {
                let (end_s, step_s) = match rest.split_once(':') {
                    Some((e, st)) => (e, Some(st)),
                    None => (rest, None),
                };
                let parse_opt = |s: &str| -> Option<i64> {
                    let t = s.trim();
                    if t.is_empty() {
                        None
                    } else {
                        t.parse::<i64>().ok()
                    }
                };
                let start_x = parse_opt(start_s);
                let end_x = parse_opt(end_s);
                let step_x = parse_opt(step_s.unwrap_or("")).unwrap_or(1);
                let mut start = start_x.unwrap_or(0);
                if start < 0 {
                    start += len;
                }
                let mut end = match end_x {
                    Some(e) if e < 0 => e + len,
                    Some(e) => e,
                    None => len - 1,
                };
                // 两端都在同一侧越界：整段无效（上游直接跳过）
                if (start < 0 && end < 0) || (start >= len && end >= len) {
                    continue;
                }
                start = start.clamp(0, len - 1);
                end = end.clamp(0, len - 1);
                if start == end || step_x >= len {
                    push(start, &mut selected);
                    continue;
                }
                let step = if step_x > 0 {
                    step_x
                } else if -step_x < len {
                    step_x + len
                } else {
                    1
                };
                if end > start {
                    let mut i = start;
                    while i <= end {
                        push(i, &mut selected);
                        i += step;
                    }
                } else {
                    let mut i = start;
                    while i >= end {
                        push(i, &mut selected);
                        i -= step;
                    }
                }
            } else if let Ok(it) = part.parse::<i64>() {
                let idx = if it < 0 { it + len } else { it };
                push(idx, &mut selected);
            }
        }
    }
    selected.into_iter().map(|i| els[i]).collect()
}

/// 解析 `name` 或 `name.N`（N 可为负数）形式的规格，返回 (名称, 可选索引)。
fn split_name_index(s: &str) -> (String, Option<i64>) {
    if let Some(dot) = s.rfind('.') {
        let (name, idxs) = s.split_at(dot);
        let raw = &idxs[1..];
        if !raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit() || c == '-') {
            if let Ok(i) = raw.parse::<i64>() {
                return (name.to_string(), Some(i));
            }
        }
    }
    (s.to_string(), None)
}

/// Legado 的 `class.a b` 表示「同时具备 a 和 b 两个类」（AND），
/// 但 CSS 里空格是后代选择器，必须归一化成 `.a.b`。
fn class_name_css(rest: &str) -> String {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.is_empty() {
        return String::new();
    }
    format!(".{}", parts.join("."))
}

/// `class.x.y` / `id.x.y` / `tag.x.y` 只取第一个点分段：上游 `JSoup 规则`
/// 对 `beforeRule.split(".")` 的结果只用 `rules[1]`，其余分段被忽略
/// （`class.list.ul` 等价 `class.list`）。
fn shorthand_segment(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

/// jQuery 扩展伪类（scraper/CSS 不认，需要先剥离再对结果集做过滤）。
#[derive(Debug, Clone, PartialEq)]
enum JqFilter {
    First,
    Last,
    Eq(i64),
    Lt(i64),
    Gt(i64),
    Contains(String),
}

fn starts_at(cs: &[char], i: usize, pat: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    cs.len() >= i + p.len() && cs[i..i + p.len()] == p[..]
}

/// 找到与 `open` 处 `(` 配对的 `)`；跳过引号内的括号。
fn find_close_paren(cs: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (i, &c) in cs.iter().enumerate().skip(open) {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn unquote_arg(raw: &str) -> String {
    let s = raw.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// 把 jQuery 伪类从选择器里剥离，返回 (基础 CSS 选择器, 过滤链)。
/// 只识别 BookSource 常用的 `:first/:last/:eq(n)/:lt(n)/:gt(n)/:contains(文本)`；
/// `:first-child`、`:nth-child(...)` 等原生 CSS 伪类原样留给 scraper。
fn split_jquery_filters(selector: &str) -> (String, Vec<JqFilter>) {
    let cs: Vec<char> = selector.chars().collect();
    let mut base = String::new();
    let mut filters = Vec::new();
    let mut i = 0usize;
    let mut quote: Option<char> = None;
    let mut bracket = 0usize;
    while i < cs.len() {
        let c = cs[i];
        if let Some(q) = quote {
            base.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                base.push(c);
                i += 1;
            }
            '[' => {
                bracket += 1;
                base.push(c);
                i += 1;
            }
            ']' => {
                bracket = bracket.saturating_sub(1);
                base.push(c);
                i += 1;
            }
            ':' if bracket == 0 => {
                let boundary = |n: usize| {
                    cs.get(i + n)
                        .map(|ch| !(ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_'))
                        .unwrap_or(true)
                };
                if starts_at(&cs, i, ":first") && boundary(6) {
                    filters.push(JqFilter::First);
                    i += 6;
                } else if starts_at(&cs, i, ":last") && boundary(5) {
                    filters.push(JqFilter::Last);
                    i += 5;
                } else if starts_at(&cs, i, ":eq(")
                    || starts_at(&cs, i, ":lt(")
                    || starts_at(&cs, i, ":gt(")
                {
                    let open = i + 3;
                    if let Some(close) = find_close_paren(&cs, open) {
                        let arg: String = cs[open + 1..close].iter().collect();
                        if let Ok(n) = unquote_arg(&arg).parse::<i64>() {
                            let f = if starts_at(&cs, i, ":eq(") {
                                JqFilter::Eq(n)
                            } else if starts_at(&cs, i, ":lt(") {
                                JqFilter::Lt(n)
                            } else {
                                JqFilter::Gt(n)
                            };
                            filters.push(f);
                            i = close + 1;
                            continue;
                        }
                    }
                    base.push(c);
                    i += 1;
                } else if starts_at(&cs, i, ":contains(") {
                    let open = i + 9;
                    if let Some(close) = find_close_paren(&cs, open) {
                        let arg: String = cs[open + 1..close].iter().collect();
                        filters.push(JqFilter::Contains(unquote_arg(&arg)));
                        i = close + 1;
                    } else {
                        base.push(c);
                        i += 1;
                    }
                } else {
                    base.push(c);
                    i += 1;
                }
            }
            _ => {
                base.push(c);
                i += 1;
            }
        }
    }
    (base.trim().to_string(), filters)
}

/// jQuery 负下标：`-1` 表示最后一个，越界返回 None。
fn jq_index(n: i64, len: usize) -> Option<usize> {
    let idx = if n < 0 { len as i64 + n } else { n };
    if idx >= 0 && (idx as usize) < len {
        Some(idx as usize)
    } else {
        None
    }
}

/// 按出现顺序依次套用 jQuery 过滤（与 jQuery 对结果集链式过滤的语义一致）。
fn apply_jq_filters<'a>(
    mut els: Vec<scraper::ElementRef<'a>>,
    filters: &[JqFilter],
) -> Vec<scraper::ElementRef<'a>> {
    for f in filters {
        let len = els.len();
        els = match f {
            JqFilter::First => els.into_iter().take(1).collect(),
            JqFilter::Last => els.pop().into_iter().collect(),
            JqFilter::Eq(n) => match jq_index(*n, len) {
                Some(i) => els.into_iter().skip(i).take(1).collect(),
                None => Vec::new(),
            },
            JqFilter::Lt(n) => {
                let bound = if *n < 0 { len as i64 + *n } else { *n };
                let take = bound.max(0) as usize;
                els.into_iter().take(take).collect()
            }
            JqFilter::Gt(n) => {
                let bound = if *n < 0 { len as i64 + *n } else { *n };
                if bound < 0 {
                    els
                } else {
                    els.into_iter().skip((bound + 1) as usize).collect()
                }
            }
            JqFilter::Contains(text) => els
                .into_iter()
                .filter(|e| e.text().collect::<String>().contains(text))
                .collect(),
        };
    }
    els
}

/// 初始选择器：支持 Legado 的 `class.x` / `id.x` / `tag.x`、可选索引（取第 i 个），
/// `!下标`（Legado 语义是**排除**这些下标，如 `li!0:1:2:3:4` 去掉前 5 个 li），
/// 以及 jQuery 伪类（`:first/:last/:eq(n)/:lt(n)/:gt(n)/:contains(...)`）。
fn select_initial<'a>(doc: &'a scraper::Html, selector: &str) -> Vec<scraper::ElementRef<'a>> {
    // Legado 末尾下标：class.x[3,4] / tag.li[0:2] / class.x[-1:0]（倒序）
    let (selector, index_expr) = split_bracket_indices(selector.trim());
    let selector = selector.as_str();
    let (core, excluded) = match selector.find('!') {
        Some(pos) => (&selector[..pos], parse_indices(&selector[pos + 1..])),
        None => (selector, Vec::new()),
    };
    let (core, jq) = split_jquery_filters(core.trim());
    let (css, idx) = if let Some(rest) = core.trim().strip_prefix("class.") {
        let (name, i) = split_name_index(rest);
        (class_name_css(shorthand_segment(&name)), i)
    } else if let Some(rest) = core.trim().strip_prefix("id.") {
        let (name, i) = split_name_index(rest);
        (format!("#{}", shorthand_segment(&name)), i)
    } else if let Some(rest) = core.trim().strip_prefix("tag.") {
        // Legado 常用 `tag.a@href`：`tag.` 前缀表示标签名，不是合法 CSS 选择器
        let (name, i) = split_name_index(rest);
        (shorthand_segment(&name).to_string(), i)
    } else {
        // 裸 `tag.N` / `tag.-N`（如 `a.0`、`li.-1`）与 `.class.N`（如 `.author.0`）：
        // 上游 `findIndexSet` 先把尾部 `.N` 拆成下标，剩余部分才是选择器；
        // 否则 CSS 会把 `a.0` / `.author.0` 判为非法选择器而返回空。
        let (name, i) = split_name_index(core.trim());
        let bare_tag_index =
            i.is_some() && !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric());
        let indexed_selector =
            i.is_some() && !name.is_empty() && scraper::Selector::parse(&name).is_ok();
        if bare_tag_index || indexed_selector {
            (name, i)
        } else {
            (core.trim().to_string(), None)
        }
    };
    // 只有伪类（如 `:first`）时按全部元素处理
    let css = if css.trim().is_empty() {
        "*".to_string()
    } else {
        css
    };
    let sel = match scraper::Selector::parse(&css) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let els: Vec<scraper::ElementRef<'a>> = doc.select(&sel).collect();
    let els = filter_indices(els, &excluded, true);
    let els = match &index_expr {
        Some(expr) => apply_index_expr(els, expr),
        None => els,
    };
    let els = apply_jq_filters(els, &jq);
    if let Some(i) = idx {
        match resolve_index(i, els.len()) {
            Some(k) => vec![els[k]],
            None => vec![],
        }
    } else {
        els
    }
}

/// 取元素的直接子元素（仅元素节点）。
fn child_elements<'a>(el: scraper::ElementRef<'a>) -> Vec<scraper::ElementRef<'a>> {
    el.children()
        .filter_map(scraper::ElementRef::wrap)
        .collect()
}

/// 从当前元素集合向下钻取到指定标签（@tag.name[.index]）。
/// Legado 的 `@tag.x` 是**后代**查询（jsoup `getElementsByTag`），不是只看直接子元素；
/// 需要直接子元素时用 `@children`。
fn apply_tag<'a>(els: Vec<scraper::ElementRef<'a>>, spec: &str) -> Vec<scraper::ElementRef<'a>> {
    let (spec, index_expr) = split_bracket_indices(spec.trim());
    let (tag, idx) = split_name_index(spec.trim());
    let (tag, jq) = split_jquery_filters(tag.trim());
    let css = if tag.trim().is_empty() {
        "*".to_string()
    } else {
        tag
    };
    let sel = match scraper::Selector::parse(&css) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let mut matched: Vec<scraper::ElementRef<'a>> = Vec::new();
    for e in &els {
        matched.extend(e.select(&sel));
    }
    let matched = apply_jq_filters(matched, &jq);
    let matched = match &index_expr {
        Some(expr) => apply_index_expr(matched, expr),
        None => matched,
    };
    if let Some(i) = idx {
        match resolve_index(i, matched.len()) {
            Some(k) => vec![matched[k]],
            None => vec![],
        }
    } else {
        matched
    }
}

/// 取元素自身的直接文本节点（不含子元素文本），对应 @textNodes。
fn text_nodes(el: &scraper::ElementRef) -> String {
    el.children()
        .filter_map(|n| n.value().as_text().map(|t| t.to_string()))
        .collect()
}

/// 裸抽取标记（`text` / `href` 等）：Legado 语义是对“当前节点”直接取值。
/// 解析片段，取第一个真实元素（跳过 html/head/body 容器）；没有元素时按纯文本处理。
fn extract_first(html: &str, tok: &str) -> String {
    let doc = scraper::Html::parse_document(html);
    let sel = match scraper::Selector::parse("*") {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let first = doc
        .select(&sel)
        .find(|e| !matches!(e.value().name(), "html" | "head" | "body"));
    match first {
        Some(el) => match tok {
            "textNodes" => text_nodes(&el),
            "html" => el.inner_html(),
            "all" => el.html(),
            _ => extract_token(el, tok),
        },
        None => match tok {
            "text" | "ownText" | "textNodes" | "html" | "all" => html.to_string(),
            _ => String::new(),
        },
    }
}

/// `/` 在此处是否构成正则字面量的开始（而非除号）。
fn js_regex_allowed(rule: &str, at: usize) -> bool {
    let b = rule.as_bytes();
    let mut j = at;
    while j > 0 && (b[j - 1] as char).is_ascii_whitespace() {
        j -= 1;
    }
    if j == 0 {
        return true;
    }
    let p = b[j - 1];
    if matches!(
        p,
        b'(' | b','
            | b'='
            | b':'
            | b'['
            | b'!'
            | b'&'
            | b'|'
            | b'?'
            | b'{'
            | b'}'
            | b';'
            | b'+'
            | b'-'
            | b'*'
            | b'%'
            | b'^'
            | b'~'
            | b'<'
            | b'>'
    ) {
        return true;
    }
    // `return /re/` 这类关键字后的 `/` 也是正则，而不是除法
    let mut k = j;
    while k > 0 && ((b[k - 1] as char).is_ascii_alphanumeric() || matches!(b[k - 1], b'_' | b'$')) {
        k -= 1;
    }
    matches!(
        &rule[k..j],
        "return" | "typeof" | "case" | "in" | "of" | "delete" | "void" | "instanceof" | "yield"
    )
}

/// 从 `<js>` 之后开始，找到**顶层** `</js>` 的字节位置：跳过字符串、模板串、
/// 正则字面量与注释，因此正则里的引号/斜杠、字符串里的 `</js>` 都不会截断 JS。
fn find_js_tag_end(rule: &str, start: usize) -> Option<usize> {
    enum St {
        Normal,
        Str(u8),
        Tpl,
        Regex,
        Line,
        Block,
    }
    let bytes = rule.as_bytes();
    let n = bytes.len();
    let mut st = St::Normal;
    let mut in_class = false;
    let mut i = start;
    while i < n {
        let c = bytes[i];
        match st {
            St::Normal => {
                if c == b'<' && rule[i..].starts_with("</js>") {
                    return Some(i);
                }
                if c == b'"' || c == b'\'' {
                    st = St::Str(c);
                    i += 1;
                    continue;
                }
                if c == b'`' {
                    st = St::Tpl;
                    i += 1;
                    continue;
                }
                if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
                    st = St::Line;
                    i += 2;
                    continue;
                }
                if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    st = St::Block;
                    i += 2;
                    continue;
                }
                if c == b'/' && js_regex_allowed(rule, i) {
                    st = St::Regex;
                    in_class = false;
                    i += 1;
                    continue;
                }
                i += 1;
            }
            St::Str(q) => {
                if c == b'\\' {
                    i += 2;
                    continue;
                }
                if c == q {
                    st = St::Normal;
                }
                i += 1;
            }
            St::Tpl => {
                if c == b'\\' {
                    i += 2;
                    continue;
                }
                if c == b'`' {
                    st = St::Normal;
                }
                i += 1;
            }
            St::Regex => {
                if c == b'\\' {
                    i += 2;
                    continue;
                }
                if c == b'[' {
                    in_class = true;
                } else if c == b']' {
                    in_class = false;
                } else if c == b'/' && !in_class {
                    st = St::Normal;
                }
                i += 1;
            }
            St::Line => {
                if c == b'\n' {
                    st = St::Normal;
                }
                i += 1;
            }
            St::Block => {
                if c == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    st = St::Normal;
                    i += 2;
                    continue;
                }
                i += 1;
            }
        }
    }
    None
}

/// 解析 `@put:{k1:v1,k2:v2}` 的 `{…}` 内部：顶层（不在引号/括号内）按 `,` 分组，再按首个 `:`
/// 拆 key/value。对应上游 `splitPutRule`（规则语义 L516-530）里的 GSON 宽松解析。
fn split_put_pairs(inner: &str) -> Vec<(String, String)> {
    let mut groups: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut depth = 0i32;
    for ch in inner.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            cur.push(ch);
            continue;
        }
        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                cur.push(ch);
            }
            '{' | '[' | '(' => {
                depth += 1;
                cur.push(ch);
            }
            '}' | ']' | ')' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => {
                groups.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        groups.push(cur.trim().to_string());
    }
    let mut out = Vec::new();
    for group in groups {
        let mut quote: Option<char> = None;
        let mut depth = 0i32;
        let mut split_at = None;
        for (idx, ch) in group.char_indices() {
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                }
                continue;
            }
            match ch {
                '"' | '\'' => quote = Some(ch),
                '{' | '[' | '(' => depth += 1,
                '}' | ']' | ')' => depth -= 1,
                ':' if depth == 0 => {
                    split_at = Some(idx);
                    break;
                }
                _ => {}
            }
        }
        if let Some(idx) = split_at {
            let key = group[..idx]
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim();
            let value = group[idx + 1..]
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim();
            if !key.is_empty() {
                out.push((key.to_string(), value.to_string()));
            }
        }
    }
    out
}

/// 在规则串顶层按分隔符拆分，但忽略落在 `<js>…</js>` 或 `@js:` 段内的分隔符。
/// 仅当当前字节为 ASCII 时才尝试 `starts_with`（分隔符/标记均为 ASCII），避免多字节
/// UTF-8（如中文）字符内部切片越界。
fn split_outside_js<'a>(rule: &'a str, sep: &str) -> Option<Vec<&'a str>> {
    let mut parts: Vec<&str> = Vec::new();
    let bytes = rule.as_bytes();
    let n = bytes.len();
    let mut start = 0usize;
    let mut i = 0usize;
    // 方括号/圆括号/引号内的分隔符属于表达式内部（如 JSONPath 谓词的 `||/&&`），不能切分
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    while i < n {
        if bytes[i] < 0x80 {
            if let Some(q) = quote {
                if bytes[i] == q {
                    quote = None;
                }
                i += 1;
                continue;
            }
            match bytes[i] {
                b'"' | b'\'' => {
                    quote = Some(bytes[i]);
                    i += 1;
                    continue;
                }
                b'[' | b'(' => {
                    depth += 1;
                    i += 1;
                    continue;
                }
                b']' | b')' => {
                    depth = depth.saturating_sub(1);
                    i += 1;
                    continue;
                }
                _ => {}
            }
            let s = &rule[i..];
            if s.starts_with("<js>") {
                if let Some(pos) = find_js_tag_end(rule, i + 4) {
                    let end = pos + 5;
                    i = end;
                    start = end;
                    continue;
                }
            }
            if starts_with_ci(s, "@js:") {
                // `@js:` 一直延伸到规则末尾：其内部的分隔符不能被当成分割点。
                // 这里只结束扫描（尾巴留在最后一个 part 里），而不是把 `@js:` 也拆成一个 part——
                // 否则 `##` 分支会把 `@js:...` 当成“替换正则”，把带 JS 尾段的规则静默吞掉。
                break;
            }
            if depth == 0 && s.starts_with(sep) {
                parts.push(&rule[start..i]);
                i += sep.len();
                start = i;
                continue;
            }
        }
        i += 1;
    }
    if start < n {
        parts.push(&rule[start..]);
    }
    if parts.len() > 1 {
        Some(parts)
    } else {
        None
    }
}

/// ASCII 大小写不敏感前缀判断（Legado `startsWith(..., ignoreCase = true)`）。
fn starts_with_ci(rule: &str, prefix: &str) -> bool {
    rule.len() >= prefix.len()
        && rule.as_bytes()[..prefix.len()]
            .iter()
            .zip(prefix.as_bytes())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn strip_prefix_ci<'a>(rule: &'a str, prefix: &str) -> &'a str {
    if starts_with_ci(rule, prefix) {
        &rule[prefix.len()..]
    } else {
        rule
    }
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_ascii_lowercase().contains(needle)
}

/// 把负下标解析为绝对下标；越界返回 None。
fn resolve_index(i: i64, len: usize) -> Option<usize> {
    let n = len as i64;
    let k = if i < 0 { n + i } else { i };
    if k >= 0 && k < n {
        Some(k as usize)
    } else {
        None
    }
}

/// `##` 替换的三种形态（Legado `AnalyzeRule.replaceRegex`）：
/// - `##pat`：删除所有匹配（replacement 为空）；
/// - `##pat##repl`：替换所有匹配；
/// - `##pat##repl###`：只替换第一个匹配（无匹配时原样返回）。
fn apply_replace_regex(
    value: &str,
    pattern: &str,
    replacement: &str,
    replace_first: bool,
) -> String {
    let re = match Regex::new(pattern) {
        Ok(r) => r,
        Err(_) => return value.replace(pattern, replacement),
    };
    // Java Matcher 的 `$1s` = 第 1 组 + 字面量 s（数字序列到非数字即结束），
    // regex crate 会把 `$1s` 当作命名组，需规范成 `${1}s`。
    let replacement = java_replacement_to_rust(replacement);
    if replace_first {
        return re.replacen(value, 1, &replacement).to_string();
    }
    re.replace_all(value, &replacement).to_string()
}

/// 把 Java 风格的 `$<数字>` 分组引用规范化为 regex crate 的 `${<数字>}`。
fn java_replacement_to_rust(replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len() + 2);
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek().is_some_and(|d| d.is_ascii_digit()) {
            let mut digits = String::new();
            while let Some(&d) = chars.peek() {
                if !d.is_ascii_digit() {
                    break;
                }
                digits.push(d);
                chars.next();
            }
            out.push_str("${");
            out.push_str(&digits);
            out.push('}');
            continue;
        }
        out.push(c);
    }
    out
}

/// HTML4 实体反转义（只在最终输出做一次，避免重复反转义）。
fn unescape_html4(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        if let Some(semi) = tail.find(';') {
            let ent = &tail[1..semi];
            if ent.len() <= 32 {
                if let Some(c) = decode_entity(ent) {
                    out.push(c);
                    rest = &tail[semi + 1..];
                    continue;
                }
            }
        }
        out.push('&');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

fn decode_entity(ent: &str) -> Option<char> {
    match ent {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        _ => {
            let num = ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse::<u32>().ok()));
            num.and_then(char::from_u32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn css_extracts_text() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="book"><span class="name">测试书名</span><span class="author">某作者</span></div>"#;
        let name = ar.get_string(html, ".name", false, "").unwrap();
        assert_eq!(name, "测试书名");
        let author = ar.get_string(html, ".author", false, "").unwrap();
        assert_eq!(author, "某作者");
    }

    #[test]
    fn class_shorthand_tokens_are_not_dropped() {
        // 错层小说网的真实详情规则：`.content@.book.0@.bookcover@tag.img@src`
        // 以及 `.content@.book.0@.bookinfo@tag.a.1@text`。此前 `.book`/`.bookcover`
        // 被当作末端抽取标记，选择器结果整段丢失。
        let ar = AnalyzeRule::new();
        let html = r#"
          <div class="content">
            <div class="book pt10">
              <div class="bookcover"><img src="/cover.jpg" /></div>
              <div class="bookinfo">
                <h1 class="booktitle">斗破苍穹</h1>
                <p class="booktag"><a>天蚕土豆</a><a>玄幻</a></p>
              </div>
            </div>
          </div>"#;
        let cover = ar
            .get_string(html, ".content@.book.0@.bookcover@tag.img@src", false, "")
            .unwrap();
        assert_eq!(cover, "/cover.jpg");
        let kind = ar
            .get_string(html, ".content@.book.0@.bookinfo@tag.a.1@text", false, "")
            .unwrap();
        assert_eq!(kind, "玄幻");
    }

    #[test]
    fn single_value_rule_joins_all_matched_elements() {
        // 上游 JSoup 规则.getString = getResultLast 收集全部匹配元素的值，
        // 非空才保留；getString 对多值按 "\n" 合并（isUrl 才走 getString 取首个）。
        // 夹具对应真实源 101/102/103 的 `@a@text`、`@a@href` 规则。
        let ar = AnalyzeRule::new();
        let html = r#"<div class="book">
            <a href="/b/1"></a>
            <a href="/b/1">剑来</a>
            <a href="/b/2">剑来 第84章</a>
        </div>"#;
        assert_eq!(
            ar.get_string(html, "class.book@a@text", false, "").unwrap(),
            "剑来\n剑来 第84章"
        );
        // 属性：空值与重复值跳过（上游 `if (url.isBlank() || textS.contains(url)) continue`）
        assert_eq!(
            ar.get_string(html, "class.book@a@href", false, "").unwrap(),
            "/b/1\n/b/2"
        );
        // is_url=true 走 getString：只取首个
        assert_eq!(
            ar.get_string(html, "class.book@a@href", true, "").unwrap(),
            "/b/1"
        );
    }

    #[test]
    fn table_row_fragment_keeps_structure_for_nth_of_type() {
        // 真实源 103 的列表项是 `<tr>`；上游拿到的是 Element（结构完整）。
        // 片段字符串重解析若丢掉 tr/td，p:nth-of-type 会把各单元格的 p 拉平成兄弟节点。
        let ar = AnalyzeRule::new();
        let row = r#"<tr>
            <td><p><a href="/book/1">书名</a></p></td>
            <td><p><a href="/author/x">作者</a></p></td>
            <td><p>连载</p></td>
        </tr>"#;
        assert_eq!(
            ar.get_string(row, "@CSS:p:nth-of-type(1) a@text", false, "")
                .unwrap(),
            "书名\n作者"
        );
        // 每个 td 内只有一个 p，因此不存在 nth-of-type(2)
        assert_eq!(
            ar.get_string(row, "@CSS:p:nth-of-type(2) a@text", false, "")
                .unwrap(),
            ""
        );
    }

    #[test]
    fn html_terminal_returns_outer_html_without_scripts() {
        // 上游 getResultLast("html")：先删 script/style，再取 Elements.outerHtml()。
        let ar = AnalyzeRule::new();
        let html = r#"<div class="text"><p>第一段</p><script>alert(1)</script><style>.x{color:red}</style><p>第二段</p></div>"#;
        let v = ar.get_string(html, "class.text@html", false, "").unwrap();
        assert!(
            v.starts_with("<div class=\"text\">"),
            "应保留元素本身（outerHtml），实际: {v}"
        );
        assert!(v.contains("<p>第一段</p>") && v.contains("<p>第二段</p>"));
        assert!(
            !v.contains("alert(1)") && !v.contains(".x{"),
            "应剔除 script/style，实际: {v}"
        );
    }

    #[test]
    fn regex_replacement_group_followed_by_letter() {
        // Java Matcher 的 `$1s` = 组 1 + 字面量 s；regex crate 会把 `$1s` 当命名组名。
        let ar = AnalyzeRule::new();
        let html = r#"<div class="n">第12章 剑来</div>"#;
        let v = ar
            .get_string(html, r"class.n@text##第(\d+)章##Chapter$1s", false, "")
            .unwrap();
        assert_eq!(v, "Chapter12s 剑来");
    }

    #[test]
    fn unsupported_js_api_reports_feature_name() {
        // 垫片抛出的未支持 API 必须带出具体名称，而不是只显示 JsError/Opaque
        let ar = AnalyzeRule::new();
        // java.ajax 已接入原生网络桥，这里改用仍未支持的 WebView 桥验证文案
        let err = ar
            .get_string("", "<js>java.webView('http://x')</js>", false, "")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("未支持的 JS 特性"),
            "错误文案应给出可读提示，实际: {err}"
        );
        assert!(err.contains("webView"), "应点名缺失的 API，实际: {err}");
    }

    /// 奇怪小说的真实 toc 规则（节选自 yetianlian 页面结构）：
    /// java.getElements 取 dl 的 children，再用 String(node) + 正则过滤出正文分卷的 dd。
    #[test]
    fn dom_bridge_yetianlian_toc_rule_returns_chapter_nodes() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="listmain">
          <dl>
            <dt>《雪中》最新章节列表</dt>
            <dd><a href="/yt110289/41770731.html">第1章 新章节</a></dd>
            <dd><a href="/yt110289/41770732.html">第2章 新章节</a></dd>
            <dt>《雪中》正文卷</dt>
            <dd><a href="/yt110289/1.html">第一章 小地瓜</a></dd>
            <dd><a href="/yt110289/2.html">第二章 北凉</a></dd>
            <dd><a href="/yt110289/3.html">第三章 剑来</a></dd>
          </dl>
        </div>"#;
        let rule = r#"@js:
var nodes = java.getElements("class.listmain@tag.dl@children");
var inFullToc = false;
nodes.filter(function (node) {
    var html = String(node);
    if (/^<dt\b/i.test(html)) { inFullToc = html.indexOf("最新章节") === -1; }
    return inFullToc && /^<dd\b/i.test(html);
});"#;
        let items = ar.get_string_list(html, rule).unwrap();
        assert_eq!(items.len(), 3, "应只保留正文卷的 3 个 dd，实际: {items:?}");
        assert!(items[0].contains("/yt110289/1.html"));
        // 逐项规则仍照常可用（目录名/URL 解析链路不变）
        assert_eq!(
            ar.get_string(&items[0], "a@text", false, "").unwrap(),
            "第一章 小地瓜"
        );
        assert_eq!(
            ar.get_string(&items[0], "a@href", false, "").unwrap(),
            "/yt110289/1.html"
        );
    }

    #[test]
    fn dom_bridge_get_string_and_string_list_on_current_page() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="head"><h1 class="title">测试书</h1></div>
            <div class="item">甲</div><div class="item">乙</div>"#;
        let title = ar
            .get_string(
                html,
                r#"@js: java.getString("class.title@text")"#,
                false,
                "",
            )
            .unwrap();
        assert_eq!(title, "测试书");
        let items = ar
            .get_string_list(html, r#"@js: java.getStringList("class.item")"#)
            .unwrap();
        assert_eq!(items.len(), 2);
        assert!(items[0].contains("甲"));
    }

    /// 快看的图片规则首段：CSS `img@data-src` 必须按列表返回每个属性值。
    #[test]
    fn get_list_css_attr_data_src() {
        let html = r#"<div class="ComicPics"><img data-src="https://cdn/x1.webp?a=1&amp;t=2" src="s1"><img data-src="https://cdn/x2.webp" src="s2"></div>"#;
        let ar = AnalyzeRule::new();
        let vals = ar.get_string_list_all(html, "img@data-src", "");
        assert_eq!(
            vals,
            vec![
                "https://cdn/x1.webp?a=1&t=2".to_string(),
                "https://cdn/x2.webp".to_string()
            ]
        );
        // 接着 JS 段：拼成 `<img src="url,{headers}">` 形态
        let rule = "img@data-src\n<js>\nlet headers = JSON.stringify({\"headers\":{\"Referer\":baseUrl}});\nresult.split('\\n').map(x=>`<img src=\"${x},${headers}\">`).join('\\n');\n</js>";
        let out = ar.get_string_list_all(html, rule, "https://m.kuaikanmanhua.com/");
        assert_eq!(out.len(), 2, "实际: {out:?}");
        assert!(
            out[0].starts_with("<img src=\"https://cdn/x1.webp?a=1&t=2,"),
            "实际: {out:?}"
        );
    }

    #[test]
    fn dom_bridge_dynamic_argument_reports_unsupported() {
        // 动态拼串不预处理，必须在运行时给出具名提示，而不是静默空结果
        let ar = AnalyzeRule::new();
        let err = ar
            .get_string(
                r#"<div class="item">x</div>"#,
                r#"@js: java.getElements("class." + "item")"#,
                false,
                "",
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("未支持的 JS 特性"), "实际: {err}");
        assert!(err.contains("java.getElements"), "实际: {err}");
        assert!(
            err.contains("字面量"),
            "应提示只支持字面量参数，实际: {err}"
        );
    }

    #[test]
    fn tag_traversal_extracts_nested() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="itemtxt"><p>第1章</p><a href="/b/1">链接</a></div>"#;
        // class.itemtxt -> .itemtxt，再向下钻取到子 <a> 取 href
        let href = ar
            .get_string(html, "@CSS:class.itemtxt@tag.a@href", false, "")
            .unwrap();
        assert_eq!(href, "/b/1");
        // 取第 0 个 <p> 的文本
        let p = ar
            .get_string(html, "@CSS:class.itemtxt@tag.p.0@text", false, "")
            .unwrap();
        assert_eq!(p, "第1章");
    }

    #[test]
    fn class_selector_index() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="des">A</div><div class="des">B</div>"#;
        // class.des.0 -> 取第 0 个 .des
        let r = ar
            .get_string(html, "@CSS:class.des.0@text", false, "")
            .unwrap();
        assert_eq!(r, "A");
    }

    #[test]
    fn children_and_textnodes() {
        let ar = AnalyzeRule::new();
        let html = r#"<ul><li><a href="/x">甲</a></li><li><a href="/y">乙</a></li></ul>"#;
        // @children 取直接子节点（li），再取 a 的 href；上游 getString 对多值按 "\n" 合并
        let r = ar
            .get_string(html, "@CSS:ul@children@tag.a@href", false, "")
            .unwrap();
        assert_eq!(r, "/x\n/y");
        let html2 = r#"<div>前缀<span>内</span>后缀</div>"#;
        let t = ar
            .get_string(html2, "@CSS:div@textNodes", false, "")
            .unwrap();
        // 上游 textNodes：直接文本节点逐个 trim，非空值按 "\n" 合并
        assert_eq!(t, "前缀\n后缀");
    }

    #[test]
    fn jsonpath_extracts_value() {
        let mut ar = AnalyzeRule::new();
        ar.is_json = true;
        let json = r#"{"list":[{"name":"书A","author":"甲"},{"name":"书B","author":"乙"}]}"#;
        let name = ar.get_string(json, "$.list[0].name", false, "").unwrap();
        assert_eq!(name, "书A");
    }

    #[test]
    fn json_list_extracts_array() {
        let mut ar = AnalyzeRule::new();
        ar.is_json = true;
        let json = r#"{"list":[{"name":"书A"},{"name":"书B"}]}"#;
        let names = ar.get_string_list(json, "$.list[*].name").unwrap();
        assert_eq!(names, vec!["书A".to_string(), "书B".to_string()]);
    }

    #[test]
    fn regex_capture_works() {
        let ar = AnalyzeRule::new();
        let txt = "编号: 12345 结束";
        let r = ar.get_string(txt, "/编号:\\s*(\\d+)/", false, "").unwrap();
        assert_eq!(r, "12345");
    }

    #[test]
    fn java_to_num_chapter() {
        let ar = AnalyzeRule::new();
        let r = ar
            .get_string(
                "",
                "<js>java.toNumChapter(\"第一百一十二\")</js>",
                false,
                "",
            )
            .unwrap();
        assert_eq!(r, "112");
    }

    #[test]
    fn put_then_get_in_same_scope() {
        let ar = AnalyzeRule::new();
        let html = "<div><a>lit</a></div>";
        // 上游语义：@put 是副作用，摘掉后主体规则照常求值（快看/爱看都是嵌在规则尾部）
        let stored = ar
            .get_string(html, "a@text@put:{x:a@text}", false, "")
            .unwrap();
        assert_eq!(stored, "lit");
        let got = ar.get_string(html, "@get:{x}", false, "").unwrap();
        assert_eq!(got, "lit");
        // 纯 @put 规则自身求值为空串（Legado splitPutRule 后规则正文为空）
        let only_put = ar.get_string(html, "@put:{y:a@text}", false, "").unwrap();
        assert_eq!(only_put, "");
        assert_eq!(ar.get_string("", "@get:{y}", false, "").unwrap(), "lit");
    }

    #[test]
    fn get_missing_returns_empty() {
        let ar = AnalyzeRule::new();
        let got = ar.get_string("", "@get:{nope}", false, "").unwrap();
        assert_eq!(got, "");
    }

    #[test]
    fn put_get_jsonpath_and_underscore_key() {
        let ar = AnalyzeRule::new();
        let json = r#"{"comic_name":"海贼王","comic_id":106492}"#;
        // 爱看形态：name 规则尾部 put，toc_url 里 get；key 允许下划线
        let name = ar
            .get_string(json, "$.comic_name@put:{comic_bid:$.comic_id}", false, "")
            .unwrap();
        assert_eq!(name, "海贼王");
        let url = ar
            .get_string(
                json,
                "https://m.kanman.com/api/getchapterlist?comic_id=@get:{comic_bid}",
                true,
                "",
            )
            .unwrap();
        assert_eq!(
            url,
            "https://m.kanman.com/api/getchapterlist?comic_id=106492"
        );
        // 多个 put 键
        let json2 = r#"{"a":1,"b":2}"#;
        ar.get_string(json2, "@put:{k1:$.a,k2:$.b}", false, "")
            .unwrap();
        assert_eq!(ar.get_string("", "@get:{k1}", false, "").unwrap(), "1");
        assert_eq!(ar.get_string("", "@get:{k2}", false, "").unwrap(), "2");
    }

    #[test]
    fn cache_get_put_in_same_scope() {
        let ar = AnalyzeRule::new();
        ar.eval_js("", "cache.put('kkmh', 'payload')", "").unwrap();
        // 同一作用域的下一次 eval 仍能读到（快看 init → chapter_list 的用法）
        let got = ar
            .eval_js("", "result = String(cache.get('kkmh'))", "")
            .unwrap();
        assert_eq!(got, "payload");
        // 未命中返回 null（上游 CacheManager.get 返回 String?）
        let miss = ar
            .eval_js("", "result = String(cache.get('nope'))", "")
            .unwrap();
        assert_eq!(miss, "null");
        // 非字符串按 JSON 串存
        ar.eval_js("", "cache.put('obj', {a:1})", "").unwrap();
        let obj = ar.eval_js("", "result = cache.get('obj')", "").unwrap();
        assert_eq!(obj, "{\"a\":1}");
        // delete 后可写
        ar.eval_js("", "cache.delete('kkmh')", "").unwrap();
        let gone = ar
            .eval_js("", "result = String(cache.get('kkmh'))", "")
            .unwrap();
        assert_eq!(gone, "null");
    }

    #[test]
    fn cache_does_not_leak_across_sources() {
        let a = AnalyzeRule::new();
        let b = AnalyzeRule::new();
        a.eval_js("", "cache.put('kk', 'A')", "").unwrap();
        // 另一个源（另一个 AnalyzeRule）读不到
        let b_got = b
            .eval_js("", "result = String(cache.get('kk'))", "")
            .unwrap();
        assert_eq!(b_got, "null");
        let a_got = a
            .eval_js("", "result = String(cache.get('kk'))", "")
            .unwrap();
        assert_eq!(a_got, "A");
        // 变量表也不跨作用域
        assert_eq!(b.get_string("", "@get:{kk}", false, "").unwrap(), "");
    }

    #[test]
    fn vars_do_not_leak_across_scopes() {
        let a = AnalyzeRule::new();
        let b = AnalyzeRule::new();
        let html = "<a>111</a>";
        a.get_string(html, "@put:{bid:a@text}", false, "").unwrap();
        // 另一个源（另一个 AnalyzeRule）读不到
        assert_eq!(b.get_string("", "@get:{bid}", false, "").unwrap(), "");
        assert_eq!(a.get_string("", "@get:{bid}", false, "").unwrap(), "111");
    }

    /// 跨请求：搜索阶段 `@put` 的 bid 在 info/toc 的新 AnalyzeRule 上读回（爱看链路）。
    #[test]
    fn book_vars_survive_new_request_scope() {
        let json = r#"{"comic_name":"海贼王","comic_id":106492}"#;
        // 请求 1（搜索）：写入 book 层变量
        let req1 = AnalyzeRule::new();
        req1.begin_book_scope("bk-1".into(), HashMap::new(), HashMap::new());
        let name = req1
            .get_string(json, "$.comic_name@put:{bid:$.comic_id}", false, "")
            .unwrap();
        assert_eq!(name, "海贼王");
        assert_eq!(
            req1.book_vars_snapshot().get("bid").map(|s| s.as_str()),
            Some("106492")
        );

        // 请求 2（目录）：新实例预载同一 book_key 的快照后 @get 生效
        let req2 = AnalyzeRule::new();
        req2.begin_book_scope("bk-1".into(), req1.book_vars_snapshot(), HashMap::new());
        let url = req2
            .get_string(
                "",
                "https://m.kanman.com/api/getchapterlist?comic_id=@get:{bid}",
                true,
                "",
            )
            .unwrap();
        assert_eq!(
            url,
            "https://m.kanman.com/api/getchapterlist?comic_id=106492"
        );

        // 另一本书（不同 book_key）不会读到
        let other = AnalyzeRule::new();
        other.begin_book_scope("bk-2".into(), HashMap::new(), HashMap::new());
        assert_eq!(other.get_string("", "@get:{bid}", false, "").unwrap(), "");
    }

    /// 跨请求 cache：info 阶段 `cache.put` 的 JSON，目录阶段预载后可 `cache.get`。
    #[test]
    fn cache_survives_new_request_scope() {
        let req1 = AnalyzeRule::new();
        req1.begin_book_scope("bk-1".into(), HashMap::new(), HashMap::new());
        req1.eval_js("", "cache.put('kkmh', '{\"data\":[1,2]}')", "")
            .unwrap();
        let snap = req1.cache_snapshot();
        assert_eq!(
            snap.get("kkmh").map(|s| s.as_str()),
            Some("{\"data\":[1,2]}")
        );

        let req2 = AnalyzeRule::new();
        req2.begin_book_scope("bk-1".into(), HashMap::new(), snap);
        let got = req2
            .eval_js("", "result = String(cache.get('kkmh'))", "")
            .unwrap();
        assert_eq!(got, "{\"data\":[1,2]}");
        // 目录阶段 delete 后快照里也没了（供路由层按 diff 删库）
        req2.eval_js("", "cache.delete('kkmh')", "").unwrap();
        assert!(req2.cache_snapshot().is_empty());
    }

    /// 快看形态：`java.setContent(json)` → `java.getElements('$.data[*].comicList[*]')`。
    #[test]
    fn set_content_then_get_elements_jsonpath_like_source() {
        let nuxt = r#"{"data":[{"comicList":[{"id":1,"title":"第1话"},{"id":2,"title":"第2话"}]},{"comicList":[{"id":3,"title":"第3话"}]}]}"#;
        let code = format!(
            "java.setContent({}); result = java.getElements('$.data[*].comicList[*]');",
            serde_json::to_string(nuxt).unwrap()
        );
        let ar = AnalyzeRule::new();
        let out = ar.eval_js("", &code, "").unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().expect("应返回章节节点数组");
        assert_eq!(arr.len(), 3, "两页 comicList 共 3 章: {out}");
        let first: Value = serde_json::from_str(arr[0].as_str().unwrap()).unwrap();
        assert_eq!(first["title"], "第1话");
    }

    /// setContent 也只作用于本次 eval（每次 eval 都是新的 IIFE）。
    #[test]
    fn set_content_scoped_to_call() {
        let ar = AnalyzeRule::new();
        ar.eval_js("", "java.setContent('{\"a\":[1,2]}'); result = 1", "")
            .unwrap();
        let err = ar
            .eval_js("", "result = java.getElements('$.a[*]')", "")
            .unwrap_err();
        assert!(
            err.to_string().contains("仅支持字面量选择器"),
            "下一次调用不应复用上次的文档（应回落到预抽取表并报未支持）：{err}"
        );
    }

    /// setContent 也支持 HTML 文档（CSS 选择器），下游 getString 同文档求值。
    #[test]
    fn set_content_html_css_selectors() {
        let ar = AnalyzeRule::new();
        let out = ar
            .eval_js(
                "",
                "java.setContent('<div class=\"x\"><p>甲</p></div>'); result = java.getString('div.x p');",
                "",
            )
            .unwrap();
        assert_eq!(out, "甲");
    }

    #[test]
    fn brace_var_expansion() {
        let ar = AnalyzeRule::new();
        ar.set_var("y", "world");
        // {{var}} 在 JS 段内展开
        let r = ar.get_string("", "<js>\"{{y}}\"</js>", false, "").unwrap();
        assert_eq!(r, "world");
    }

    #[test]
    fn css_not_first_child_supported() {
        let ar = AnalyzeRule::new();
        let html = r#"<table id="bookcon"><tr><th>h</th></tr><tr><td><p><a href="/1">a</a></p></td></tr><tr><td><p><a href="/2">b</a></p></td></tr></table>"#;
        let items = ar
            .get_string_list(html, "@css:table#bookcon tr:not(:first-child)")
            .unwrap();
        assert_eq!(
            items.len(),
            2,
            "tr:not(:first-child) should select 2 data rows"
        );
    }

    #[test]
    fn single_value_takes_first_match() {
        let ar = AnalyzeRule::new();
        // 一个 item 内多个 <a> 时，单值抽取（@href/@text）应取首个，而非拼接
        let html = r#"<tr><td><p><a href="/book/1">书名A</a></p></td><td><p><a href="/book/2">最新</a></p></td></tr>"#;
        let href = ar
            .get_string(html, "@css:p:nth-of-type(1) a@href", true, "")
            .unwrap();
        assert_eq!(href, "/book/1");
    }

    #[test]
    fn bare_tag_and_excluded_indices() {
        let ar = AnalyzeRule::new();
        let html =
            r#"<dl id="list"><dd>第1</dd><dd>第2</dd><dd>第3</dd><dd>第4</dd><dd>第5</dd></dl>"#;
        // 裸标签遍历（@dd）+ 多值按 "\n" 合并（上游 getResultLast 收集全部元素）
        let t1 = ar
            .get_string(html, "@CSS:id.list@dd@text", false, "")
            .unwrap();
        assert_eq!(t1, "第1\n第2\n第3\n第4\n第5");
        // Legado 的 `!0:1` 是“排除第 0、1 个”，剩余第 3/4/5 个
        let t2 = ar
            .get_string(html, "@CSS:id.list@dd!0:1@text", false, "")
            .unwrap();
        assert_eq!(t2, "第3\n第4\n第5");
        // `!2` 排除第 3 个
        let t3 = ar
            .get_string(html, "@CSS:id.list@dd!2@text", false, "")
            .unwrap();
        assert_eq!(t3, "第1\n第2\n第4\n第5");
    }

    // B3：初始选择器带 `!下标`（精华书阁真实形态 `li!0:1:2:3:4@a` / `option!0@value`）
    #[test]
    fn initial_selector_excludes_indices() {
        let ar = AnalyzeRule::new();
        // 前 5 个 li 是书头信息，后面才是章节
        let html = r#"<ul class="chapter"><li>书头0</li><li>书头1</li><li>书头2</li><li>书头3</li><li>书头4</li><li><a href="/c6">第6章</a></li><li><a href="/c7">第7章</a></li></ul>"#;
        let items = ar.get_string_list(html, "li!0:1:2:3:4@a").unwrap();
        assert_eq!(items.len(), 2, "前 5 个 li 应被 `!0:1:2:3:4` 排除");
        assert_eq!(
            ar.get_string(&items[0], "text", false, "").unwrap(),
            "第6章"
        );
        assert_eq!(ar.get_string(&items[0], "href", true, "").unwrap(), "/c6");

        let html2 =
            r#"<select><option value="">请选择</option><option value="/c1">1</option></select>"#;
        assert_eq!(
            ar.get_string(html2, "option!0@value", true, "").unwrap(),
            "/c1"
        );
    }

    // B4：裸 `text` / `href`（无选择器前缀）表示取当前节点
    #[test]
    fn bare_text_and_href_rules() {
        let ar = AnalyzeRule::new();
        let item = r#"<a href="/book/1">书名A</a>"#;
        assert_eq!(ar.get_string(item, "text", false, "").unwrap(), "书名A");
        assert_eq!(ar.get_string(item, "href", true, "").unwrap(), "/book/1");
        // 真实形态：h4 内嵌 a，裸 text 取整个节点的文本
        let h4 = r#"<h4><a href="/b/2">书名B</a></h4>"#;
        assert_eq!(ar.get_string(h4, "text", false, "").unwrap(), "书名B");
    }

    // B5：`@tag.x` 应命中后代，而不是只看直接子元素（得奇 `tag.h1@tag.a@href`）
    #[test]
    fn tag_traversal_hits_descendants() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="item"><div class="itemtxt"><p>简介</p><h1><a href="/b/9">书名C</a></h1></div></div>"#;
        // h1 在 div.itemtxt 内部（非直接子元素）
        assert_eq!(
            ar.get_string(html, "@CSS:class.itemtxt@tag.h1@tag.a@href", true, "")
                .unwrap(),
            "/b/9"
        );
        assert_eq!(
            ar.get_string(html, "@CSS:class.itemtxt@tag.h1@tag.a@text", false, "")
                .unwrap(),
            "书名C"
        );
    }

    #[test]
    fn js_segment_transforms() {
        let ar = AnalyzeRule::new();
        let input = "hello world";
        let out = ar
            .get_string(input, "<js>result.toUpperCase()</js>", false, "")
            .unwrap();
        // 纯字符串场景：JS 段把 result 转大写
        assert_eq!(out, "HELLO WORLD");
    }

    #[test]
    fn tag_prefix_selector_works() {
        // Legado 常见写法 `tag.a@href`：`tag.` 前缀此前被当成非法 CSS 选择器，搜索因此全空
        let ar = AnalyzeRule::new();
        let html = r#"<div class="item"><a href="/b/1">书A</a><span class="au">作者</span></div>"#;
        assert_eq!(ar.get_string(html, "tag.a@href", true, "").unwrap(), "/b/1");
        assert_eq!(ar.get_string(html, "tag.a@text", false, "").unwrap(), "书A");
        assert_eq!(
            ar.get_string(html, "class.au@text", false, "").unwrap(),
            "作者"
        );
    }

    #[test]
    fn whole_rule_js_does_not_recurse() {
        let ar = AnalyzeRule::new();
        // 整条规则以 `@js:` 开头：旧实现在 "||" 切分处把整条规则再喂回 get_string，
        // 无限递归直到栈溢出（小栈 tokio worker 上直接导致进程崩溃）。
        assert!(split_outside_js("@js: 1", "||").is_none());
        let out = ar
            .get_string("", "@js: JSON.stringify([{title:'a',url:'u'}])", false, "")
            .unwrap();
        assert!(out.starts_with('['), "unexpected js result: {out}");
    }

    #[test]
    fn js_runtime_limit_does_not_poison_following_rules() {
        // 单例 Context 复用会让「出错脚本留下的全局状态」污染后续规则；
        // 修复方式是 RuntimeLimit 后重建 Context。这里用全局变量探针固住：
        // 出错脚本先写 `globalThis.__P=1` 再栈溢出，随后必须读不到这个变量。
        let ar = AnalyzeRule::new();
        let bad = "@js:(globalThis.__P=1,(function f(){return f();})())";
        let err = ar.get_string("", bad, false, "").unwrap_err();
        assert!(
            err.to_string().contains("RuntimeLimit"),
            "expected RuntimeLimit, got: {err}"
        );
        // 旧实现会返回 \"number\"（Context 被复用），修复后必须是 \"undefined\"
        assert_eq!(
            ar.get_string("", "@js:typeof __P", false, "").unwrap(),
            "undefined"
        );
        assert_eq!(ar.get_string("", "@js:1+1", false, "").unwrap(), "2");
        // 连续 3 轮：每次都从污染中恢复，不累积
        for round in 0..3 {
            let _ = ar.get_string("", bad, false, "");
            assert_eq!(
                ar.get_string("", "@js:1+1", false, "").unwrap(),
                "2",
                "round {round}: js context still poisoned"
            );
        }
    }

    #[test]
    fn variable_expansion_works() {
        let ar = AnalyzeRule::new();
        ar.set_var("sel", ".name");
        let html = r#"<div class="name">测试书名</div>"#;
        let r = ar.get_string(html, "${sel}", false, "").unwrap();
        assert_eq!(r, "测试书名");
    }

    // `##正则##` 替换支持 `$1` 捕获组（Legado 原生写法）
    #[test]
    fn regex_replacement_supports_capture_group() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="n">第12章 标题</div>"#;
        let r = ar
            .get_string(html, r"@css:.n@text##第(\d+)章##Chapter$1", false, "")
            .unwrap();
        assert_eq!(r, "Chapter12 标题");
        // 非正则替换仍然可用
        let r2 = ar
            .get_string(html, "@css:.n@text##第12章##卷", false, "")
            .unwrap();
        assert_eq!(r2, "卷 标题");
    }

    // `page` 变量可被规则模板引用（多页目录/搜索分页共用）
    #[test]
    fn page_variable_expands_in_rule() {
        let ar = AnalyzeRule::new();
        ar.set_var("page", "2");
        let html = r#"<div class="page2"><a href="/b">2</a></div>"#;
        // `${page}` 先展开为 2，再作为 CSS 选择器 `.page2 a` 执行
        assert_eq!(
            ar.get_string(html, "@css:.page${page} a@href", true, "")
                .unwrap(),
            "/b"
        );
    }

    // nextTocUrl 需要“对每个元素抽取属性”，而不是只取首个元素
    #[test]
    fn list_all_extracts_multiple_hrefs() {
        let ar = AnalyzeRule::new();
        let html =
            r#"<div class="pages"><a href="/toc/2.html">2</a><a href="/toc/3.html">3</a></div>"#;
        let list = ar.get_string_list_all(html, "@css:.pages a@href", "");
        assert_eq!(list, vec!["/toc/2.html", "/toc/3.html"]);
        // 单值规则仍取首个
        assert_eq!(
            ar.get_string(html, "@css:.pages a@href", true, "").unwrap(),
            "/toc/2.html"
        );
    }

    // @js: 高频内置：md5（含 16 位）、base64、hex、timeFormat、变量
    #[test]
    fn js_high_frequency_builtins() {
        let ar = AnalyzeRule::new();
        assert_eq!(
            ar.get_string("", "<js>java.md5Encode('abc')</js>", false, "")
                .unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            ar.get_string("", "<js>java.md5Encode16('abc')</js>", false, "")
                .unwrap(),
            "3cd24fb0d6963f7d"
        );
        let b64 = ar
            .get_string("", "<js>java.base64Encode('中文')</js>", false, "")
            .unwrap();
        assert_eq!(b64, "5Lit5paH");
        assert_eq!(
            ar.get_string("", "<js>java.base64Decode('5Lit5paH')</js>", false, "")
                .unwrap(),
            "中文"
        );
        assert_eq!(
            ar.get_string("", "<js>java.hexEncode('A')</js>", false, "")
                .unwrap(),
            "41"
        );
        assert_eq!(
            ar.get_string("", "<js>java.hexDecode('41')</js>", false, "")
                .unwrap(),
            "A"
        );
        // base64 同族（上游 JsExtensions.kt L676-730 签名）
        assert_eq!(
            ar.get_string(
                "",
                "<js>java.base64Decode('5Lit5paH','UTF-8')</js>",
                false,
                ""
            )
            .unwrap(),
            "中文"
        );
        assert_eq!(
            ar.get_string("", "<js>java.base64Decode('5Lit5paH',2)</js>", false, "")
                .unwrap(),
            "中文"
        );
        // URL-safe 变体（flags=8）编码后可原样解回
        assert_eq!(
            ar.get_string(
                "",
                "<js>var e=java.base64Encode('a?b',8); java.base64Decode(e)</js>",
                false,
                ""
            )
            .unwrap(),
            "a?b"
        );
        // flags=1 去 padding
        assert_eq!(
            ar.get_string("", "<js>java.base64Encode('a',1)</js>", false, "")
                .unwrap(),
            "YQ"
        );
        assert_eq!(
            ar.get_string(
                "",
                "<js>java.base64DecodeToByteArray('5Lit5paH').length</js>",
                false,
                ""
            )
            .unwrap(),
            "6"
        );
        assert_eq!(
            ar.get_string("", "<js>java.hexEncodeToString('中文')</js>", false, "")
                .unwrap(),
            "e4b8ade69687"
        );
        assert_eq!(
            ar.get_string(
                "",
                "<js>java.hexDecodeToString('e4b8ade69687')</js>",
                false,
                ""
            )
            .unwrap(),
            "中文"
        );
        // 毫秒时间戳 + Java 风格 pattern
        assert_eq!(
            ar.get_string(
                "",
                "<js>java.timeFormat(1609459200000, 'yyyy-MM-dd')</js>",
                false,
                ""
            )
            .unwrap(),
            "2021-01-01"
        );
        // 同一次 JS 执行内的变量读写
        assert_eq!(
            ar.get_string(
                "",
                "<js>java.putVariable('k', 'v'); java.getVariable('k')</js>",
                false,
                ""
            )
            .unwrap(),
            "v"
        );
        let n = ar
            .get_string("", "<js>java.randomInt(1, 10)</js>", false, "")
            .unwrap();
        let n: i64 = n.parse().unwrap();
        assert!((1..10).contains(&n));
    }

    // JSONPath 兼容：递归下降 `$..field`、数组通配 `[*]`、下标 `[n]`（抽检样本直接形态）
    #[test]
    fn jsonpath_recursive_descent_and_wildcard() {
        let ar = AnalyzeRule::new();
        let json = r#"{"code":0,"data":{"page":1,"books":[{"id":"b1","book_name":"书A"},{"id":"b2","book_name":"书B"}]},"booklist":[{"book_name":"书C"}]}"#;
        let books = ar.get_string_list(json, "$..books[*]").unwrap();
        assert_eq!(books.len(), 2, "递归下降 + [*] 应取到嵌套 books");
        assert!(books[0].contains("b1"));
        let names = ar.get_string_list(json, "$.booklist[*].book_name").unwrap();
        assert_eq!(names, vec!["书C"]);
        // 单值：递归下降命中多个值时，与 Legado 一致按 `\n` 拼接（源规则常据此 `split("\n")`）
        assert_eq!(
            ar.get_string(json, "$..book_name", false, "").unwrap(),
            "书C\n书A\n书B"
        );
        // 下标（含负下标）
        assert_eq!(
            ar.get_string(json, "$.data.books[1].id", false, "")
                .unwrap(),
            "b2"
        );
        assert_eq!(
            ar.get_string(json, "$.data.books[-1].id", false, "")
                .unwrap(),
            "b2"
        );
    }

    // Legado 用的 Jayway json-path 容忍 `$.data.[*]`（点与方括号混用），爱看目录规则就是这种写法
    #[test]
    fn jsonpath_tolerates_dot_before_bracket() {
        let ar = AnalyzeRule::new();
        let json = r#"{"data":[{"id":1},{"id":2}]}"#;
        assert_eq!(
            ar.get_string(json, "$.data.[0].id", false, "").unwrap(),
            "1"
        );
        assert_eq!(
            ar.get_string_list(json, "$.data.[*].id").unwrap(),
            vec!["1", "2"]
        );
    }

    // 不支持的 JSONPath 不再原样抛 PestError，而是可读中文提示
    #[test]
    fn jsonpath_unsupported_syntax_has_readable_error() {
        let ar = AnalyzeRule::new();
        let err = ar
            .get_string(r#"{"a":1}"#, "$.a..", false, "")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("JSONPath 语法不受支持"),
            "unexpected error: {err}"
        );
        assert!(!err.contains("PestError"), "不应暴露 PestError: {err}");
    }

    // `{{$.field}}` 模板：列表项字段可拼进字面量 URL 与 POST body
    #[test]
    fn jsonpath_template_expands_in_url_and_body() {
        let ar = AnalyzeRule::new();
        let item = r#"{"id":"42","book_name":"书A"}"#;
        assert_eq!(
            ar.get_string(item, "https://api.x/read?bid={{$.id}}", true, "")
                .unwrap(),
            "https://api.x/read?bid=42"
        );
        assert_eq!(
            ar.get_string(item, "{\"bid\":\"{{$.id}}\"}", false, "")
                .unwrap(),
            "{\"bid\":\"42\"}"
        );
    }

    // 切分 JS 时字符串/正则里的 `</js>`、`||`、引号不能截断代码
    #[test]
    fn js_split_ignores_tags_inside_strings_and_regex() {
        let parts =
            AnalyzeRule::split_js(r#"<js>result.replace("</js>", "").replace(/a||b/g, "")</js>"#);
        assert_eq!(parts.len(), 1, "整段应只有一个 JS 段");
        let code = match &parts[0] {
            (Mode::Js, c) => c.clone(),
            _ => panic!("unexpected part: not a JS segment"),
        };
        assert!(code.contains("</js>"), "字符串里的 </js> 不应截断: {code}");
        let ar = AnalyzeRule::new();
        assert_eq!(
            ar.get_string(
                "x</js>y",
                r#"<js>result.replace("</js>", "")</js>"#,
                false,
                ""
            )
            .unwrap(),
            "xy"
        );
    }

    // 混合规则：`<js>预处理</js>$..list[*]||$.books[*]`（懒人听书 bookList 的形态）
    #[test]
    fn js_prefix_then_jsonpath_list_with_fallback() {
        let ar = AnalyzeRule::new();
        let json = r#"{"books":[{"name":"A"},{"name":"B"}]}"#;
        let list = ar
            .get_string_list(json, r#"<js>result;</js>$..list[*]||$.books[*]"#)
            .unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].contains("\"A\""));
        // 纯 JSONPath 的 || 回退：第一条为空时取第二条
        let fallback = ar
            .get_string_list(json, "$..missing[*]||$.books[*]")
            .unwrap();
        assert_eq!(fallback.len(), 2);
    }

    // java.t2s：常用简→繁映射
    #[test]
    fn js_t2s_common_chars() {
        let ar = AnalyzeRule::new();
        assert_eq!(
            ar.get_string("", "<js>java.t2s('国学说话书龙')</js>", false, "")
                .unwrap(),
            "國學說話書龍"
        );
    }

    // java.ajax：网络桥接受私网策略约束（127.0.0.1 应被拒绝且给出可读原因）
    #[test]
    fn js_ajax_rejects_private_target() {
        let ar = AnalyzeRule::new();
        let err = ar
            .get_string("", "<js>java.ajax('http://127.0.0.1:1/')</js>", false, "")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("拒绝访问") || err.contains("java.ajax"),
            "unexpected error: {err}"
        );
    }

    /// `java.ajax` 的同步语义不变，但求值发生在专用 JS 线程（非 tokio worker），
    /// 因此嵌套 `block_on` 不会在 worker 上触发 "Cannot start a runtime from within
    /// a runtime" 的 panic。这里从多线程 runtime 的任务里跑一次，固住该不变式。
    #[test]
    fn js_ajax_inside_runtime_does_not_panic_on_nested_block_on() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(async {
                tokio::spawn(async {
                    AnalyzeRule::new().get_string(
                        "",
                        "<js>java.ajax('http://127.0.0.1:1/')</js>",
                        false,
                        "",
                    )
                })
                .await
                .unwrap()
            })
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("拒绝访问") || err.contains("java.ajax"),
            "unexpected error: {err}"
        );
    }

    // ---------------- JS 调度层：慢 JS 不得占住 tokio worker ----------------

    /// 构造一条「可控墙钟耗时」的慢 JS 规则。
    ///
    /// 每轮做一次 8 KiB 字符串分配，迭代次数远低于 `loop_iteration_limit`(500k)，
    /// 因此耗时由 `ms` 决定，而不会先撞上 RuntimeLimit（便于稳定复现慢源）。
    fn slow_js_rule(ms: u64) -> String {
        format!(
            "@js:var t0=Date.now(), n=0; while(Date.now()-t0<{ms}) {{ 'abcdefghijklmnop'.repeat(500); n++; }} n"
        )
    }

    /// 在独立线程里跑规则，避免 `AnalyzeRule`（!Send）进入 tokio 任务状态机。
    fn run_rule_in_thread(rule: String) -> Result<String, RuleError> {
        AnalyzeRule::new().get_string("", &rule, false, "")
    }

    #[test]
    fn slow_js_does_not_block_other_requests() {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        // 与生产一致的 runtime 形状（main.rs：2 worker + max_blocking_threads 4）
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(4)
            .enable_all()
            .build()
            .unwrap();
        const SLOW_MS: u64 = 500;
        const NORMAL_MS: u64 = 30;
        // 旧实现：两个 worker 都被阻塞，普通请求要等第一条慢 JS 跑完（≈SLOW_MS）
        let latencies = Arc::new(Mutex::new(Vec::<Duration>::new()));

        let lat = latencies.clone();
        rt.block_on(async move {
            // 两个并发请求各命中一条慢 JS 源（单 JS 线程串行执行，两条各 500ms）。
            // 用 ready 信号确保两个 worker 分别进入 JS 求值后，再发起普通请求，
            // 否则任务可能集中在一个 worker 的本地队列里，无法复现“两个 worker 同时被占”。
            let (ready_tx, mut ready_rx) = tokio::sync::mpsc::channel::<()>(2);
            let mut slow = Vec::new();
            for _ in 0..2 {
                let rule = slow_js_rule(SLOW_MS);
                let tx = ready_tx.clone();
                slow.push(tokio::spawn(async move {
                    let _ = tx.send(()).await;
                    run_rule_in_thread(rule)
                }));
            }
            drop(ready_tx);
            let _ = ready_rx.recv().await;
            let _ = ready_rx.recv().await;
            // 两个 worker 此刻都在 JS 里同步阻塞；block_on 线程用 std sleep，
            // 不依赖 timer driver（此时它可能没线程来驱动）。
            std::thread::sleep(Duration::from_millis(20));

            // 普通请求：只需要一个空闲 worker 就能按时完成。
            // 计时从 spawn 前开始，包含“排队等 worker”的时间（旧实现的真正代价）。
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let lat2 = lat.clone();
            let normal_started = Instant::now();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(NORMAL_MS)).await;
                lat2.lock().unwrap().push(normal_started.elapsed());
                let _ = done_tx.send(());
            });
            let _ = done_rx.await;

            for h in slow {
                let _ = h.await;
            }
        });

        let latency = *latencies.lock().unwrap().first().expect("普通请求未执行");
        let budget = Duration::from_millis(SLOW_MS / 2);
        assert!(
            latency < budget,
            "普通请求被慢 JS 拖慢：{latency:?} >= {budget:?}（SLOW_MS={SLOW_MS}）"
        );
    }

    /// 未调用 `init_js` 时（单测/规则试跑等）必须采用配置模块的默认预算，
    /// 不再回落到早期写死的 200ms（否则腾讯漫画图片规则会被误判超时）。
    #[test]
    fn uninitialized_js_conf_uses_config_default_budget() {
        assert_eq!(
            js_conf().timeout_ms,
            crate::config::DEFAULT_JS_TIMEOUT_MS,
            "JsConf 默认预算应与 config::DEFAULT_JS_TIMEOUT_MS 一致"
        );
    }

    /// 超预算行为保持不变：报错文案含 exceeded/budget，且不污染后续求值。
    /// 这里用 `#[tokio::test]`（current_thread）同时验证降级路径不会 panic。
    #[tokio::test]
    async fn slow_js_rule_reports_budget_timeout() {
        // 测试进程未调用 init_js：预算为默认值，慢规则比默认预算多跑 500ms 以确保超限。
        let over_budget_ms = crate::config::DEFAULT_JS_TIMEOUT_MS + 500;
        let err = AnalyzeRule::new()
            .get_string("", &slow_js_rule(over_budget_ms), false, "")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("exceeded") && err.contains("budget"),
            "unexpected error: {err}"
        );
        assert_eq!(
            AnalyzeRule::new()
                .get_string("", "@js:1+1", false, "")
                .unwrap(),
            "2"
        );
    }

    // ---------------- 选择器扩展 ----------------

    const NEWS_HTML: &str = r#"<ul class="news-list">
          <li class="item"><a href="/1.html">第一章</a></li>
          <li class="item"><a href="/2.html">第二章</a></li>
          <li class="item"><a href="/3.html">第三章</a></li>
          <li class="item"><a href="/4.html">第四章</a></li>
        </ul>"#;

    #[test]
    fn jquery_pseudo_first_last_eq() {
        let ar = AnalyzeRule::new();
        assert_eq!(
            ar.get_string(NEWS_HTML, "li:first@a@text", false, "")
                .unwrap(),
            "第一章"
        );
        assert_eq!(
            ar.get_string(NEWS_HTML, "li:last@a@text", false, "")
                .unwrap(),
            "第四章"
        );
        assert_eq!(
            ar.get_string(NEWS_HTML, "li:eq(1)@a@text", false, "")
                .unwrap(),
            "第二章"
        );
        // jQuery 负下标：-1 为最后一个
        assert_eq!(
            ar.get_string(NEWS_HTML, "li:eq(-1)@a@text", false, "")
                .unwrap(),
            "第四章"
        );
    }

    #[test]
    fn jquery_pseudo_lt_gt_and_contains() {
        let ar = AnalyzeRule::new();
        // get_string_list 的 CSS 模式返回元素 HTML（与既有契约一致），按 href 校验结果集
        let lt = ar.get_string_list(NEWS_HTML, "li:lt(2)@a").unwrap();
        assert_eq!(lt.len(), 2, "实际: {lt:?}");
        assert!(lt[0].contains("/1.html") && lt[1].contains("/2.html"));
        let gt = ar.get_string_list(NEWS_HTML, "li:gt(1)@a").unwrap();
        assert_eq!(gt.len(), 2, "实际: {gt:?}");
        assert!(gt[0].contains("/3.html") && gt[1].contains("/4.html"));
        assert_eq!(
            ar.get_string(NEWS_HTML, "li:contains(第三章)@a@text", false, "")
                .unwrap(),
            "第三章"
        );
        // 遍历段里的伪类（@li:eq）同样生效
        assert_eq!(
            ar.get_string(NEWS_HTML, "ul@li:eq(3)@a@text", false, "")
                .unwrap(),
            "第四章"
        );
    }

    #[test]
    fn attribute_tokens_and_text_are_boundary_safe() {
        let ar = AnalyzeRule::new();
        let html = r#"<a class="link">链接文字</a>"#;
        // 属性不存在 → 空串，不回退成整节点文本
        assert_eq!(ar.get_string(html, "@href", false, "").unwrap(), "");
        assert_eq!(ar.get_string(html, "@data-id", false, "").unwrap(), "");
        assert_eq!(ar.get_string(html, ".link@src", false, "").unwrap(), "");
        // text 与 @text 行为一致
        assert_eq!(ar.get_string(html, "text", false, "").unwrap(), "链接文字");
        assert_eq!(ar.get_string(html, "@text", false, "").unwrap(), "链接文字");
        // 存在的任意属性正常取到
        let with_attr = r#"<a class="link" data-id="42" href="/x">详情</a>"#;
        assert_eq!(
            ar.get_string(with_attr, ".link@data-id", false, "")
                .unwrap(),
            "42"
        );
    }

    #[test]
    fn class_and_id_shorthand_are_selectors() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="item"><span class="title">测试书名</span></div>
                      <div id="box"><p>简介内容</p></div>"#;
        assert_eq!(
            ar.get_string(html, "class.item@class.title@text", false, "")
                .unwrap(),
            "测试书名"
        );
        assert_eq!(
            ar.get_string(html, "id.box@tag.p@text", false, "").unwrap(),
            "简介内容"
        );
    }

    // ---- P1：`class.a b` 是 AND（同时具备），不是 CSS 的后代选择器 ----

    #[test]
    fn p1_class_multi_name_is_and_not_descendant() {
        let ar = AnalyzeRule::new();
        // 初始选择器：只有同时具备 author/extra 的节点命中
        let html = r#"<div class="author extra"><span>甲</span></div>
                      <div class="author"><span>乙</span></div>"#;
        assert_eq!(
            ar.get_string(html, "class.author extra@text", false, "")
                .unwrap()
                .trim(),
            "甲"
        );
        // 下标解析必须发生在归一化之前：`class.author extra.-1` 取最后一个命中
        let html2 = r#"<div class="author extra">A</div><div class="author extra">B</div><div class="author">C</div>"#;
        assert_eq!(
            ar.get_string(html2, "class.author extra.-1@text", false, "")
                .unwrap(),
            "B"
        );
        // 遍历标记路径（parse_token）同样按 AND 处理
        let html3 = r#"<div class="item"><p class="author extra">作者甲</p></div>
                       <div class="item"><p class="author">作者乙</p></div>"#;
        assert_eq!(
            ar.get_string(html3, "class.item@class.author extra@text", false, "")
                .unwrap(),
            "作者甲"
        );
    }

    // ---- P2：首段裸 `tag.N` / `tag.-N` ----

    #[test]
    fn p2_bare_tag_index_in_initial_selector() {
        let ar = AnalyzeRule::new();
        let html =
            r#"<div class="list"><a href="/1">一</a><a href="/2">二</a><a href="/3">三</a></div>"#;
        assert_eq!(ar.get_string(html, "a.0@href", true, "").unwrap(), "/1");
        assert_eq!(ar.get_string(html, "a.-1@href", true, "").unwrap(), "/3");
        // 等价写法不得回归
        assert_eq!(ar.get_string(html, "tag.a.0@href", true, "").unwrap(), "/1");
        assert_eq!(ar.get_string(html, "a[0]@href", true, "").unwrap(), "/1");
    }

    // ---------------- JSONPath 谓词 / 回退链 / @json: ----------------

    const BOOKS_JSON: &str = r#"{"data":{"list":[
          {"title":"A","author":"天蚕土豆","price":12.5,"score":4},
          {"title":"B","author":"天蚕土豆","price":8.0,"score":4},
          {"title":"C","author":"我吃西红柿","price":20,"score":4},
          {"title":"D","author":"天蚕土豆","price":12.5,"score":6}
        ]}}"#;

    #[test]
    fn jsonpath_predicate_equality() {
        let ar = AnalyzeRule::new();
        let titles = ar
            .get_string_list(BOOKS_JSON, "$.data.list[?(@.author=='天蚕土豆')].title")
            .unwrap();
        assert_eq!(titles, vec!["A", "B", "D"]);
        // 方括号字段语法
        let titles2 = ar
            .get_string_list(
                BOOKS_JSON,
                "$.data.list[?(@['author']=='我吃西红柿')].title",
            )
            .unwrap();
        assert_eq!(titles2, vec!["C"]);
        // 数值比较
        let cheap = ar
            .get_string_list(BOOKS_JSON, "$.data.list[?(@.price<10)].title")
            .unwrap();
        assert_eq!(cheap, vec!["B"]);
    }

    #[test]
    fn jsonpath_predicate_compare_and_and() {
        let ar = AnalyzeRule::new();
        let hit = ar
            .get_string_list(
                BOOKS_JSON,
                "$.data.list[?(@.author=='天蚕土豆' && @.price>=10)].title",
            )
            .unwrap();
        assert_eq!(hit, vec!["A", "D"]);
        let none = ar
            .get_string_list(
                BOOKS_JSON,
                "$.data.list[?(@.author=='我吃西红柿' && @.price<10)].title",
            )
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn jsonpath_predicate_or() {
        let ar = AnalyzeRule::new();
        let hit = ar
            .get_string_list(
                BOOKS_JSON,
                "$.data.list[?(@.author=='我吃西红柿' || @.score>=6)].title",
            )
            .unwrap();
        assert_eq!(hit, vec!["C", "D"]);
    }

    #[test]
    fn jsonpath_fallback_chain_and_at_json_prefix() {
        let ar = AnalyzeRule::new();
        let chain = ar
            .get_string_list(
                BOOKS_JSON,
                "$.missing.list[*]||$.data.missing[*]||$.data.list[*].title",
            )
            .unwrap();
        assert_eq!(chain, vec!["A", "B", "C", "D"]);
        // @json: 前缀与裸 JSONPath 等价
        let prefixed = ar
            .get_string_list(
                BOOKS_JSON,
                "@json:$.data.list[?(@.author=='天蚕土豆')].title",
            )
            .unwrap();
        assert_eq!(prefixed, vec!["A", "B", "D"]);
    }

    #[test]
    fn jsonpath_unsupported_predicate_has_named_error() {
        let ar = AnalyzeRule::new();
        let err = ar
            .get_string_list(BOOKS_JSON, "$.data.list[?(@.title==)].title")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("JSONPath 语法不受支持"),
            "unexpected error: {err}"
        );
        assert!(!err.contains("PestError"), "不应暴露 PestError: {err}");
    }

    // ---- P0-7：模式前缀大小写不敏感 ----

    #[test]
    fn p0_7_mode_prefix_is_case_insensitive() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="list">ABC</div>"#;
        for rule in [
            "@CSS:div.list@text",
            "@Css:div.list@text",
            "@css:div.list@text",
        ] {
            assert_eq!(
                ar.get_string(html, rule, false, "").unwrap(),
                "ABC",
                "{rule}"
            );
        }
        let json = r#"{"title":"T"}"#;
        for rule in ["@Json:$.title", "@JSON:$.title", "@json:$.title"] {
            assert_eq!(ar.get_string(json, rule, false, "").unwrap(), "T", "{rule}");
        }
        let xml = "<root><name>N</name></root>";
        for rule in ["@XPath:/root/name/text()", "@XPATH:/root/name/text()"] {
            assert_eq!(ar.get_string(xml, rule, false, "").unwrap(), "N", "{rule}");
        }
        assert_eq!(ar.get_string("", "@JS:1+1", false, "").unwrap(), "2");
    }

    // ---- P0-1：列表规则不再折叠为单值 ----

    #[test]
    fn p0_1_fallback_returns_full_list_not_first_value() {
        let ar = AnalyzeRule::new();
        let html =
            r#"<div class="list"><li>A</li><li>B</li><li>C</li></div><div class="box"></div>"#;
        let v = ar.get_string_list_all(html, ".list li@text||.box li@text", "");
        assert_eq!(v, vec!["A", "B", "C"]);
    }

    #[test]
    fn p0_1_regex_replace_applies_per_list_item() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="list"><a href="/1?from=home">1</a><a href="/2?from=list">2</a><a href="/3?from=home">3</a></div>"#;
        let v = ar.get_string_list_all(html, ".list a@href##\\?from=home", "");
        assert_eq!(v, vec!["/1", "/2?from=list", "/3"]);
    }

    #[test]
    fn p0_1_and_merges_lists_in_list_mode() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="a"><li>A1</li><li>A2</li><li>A3</li></div><div class="b"><li>B1</li><li>B2</li><li>B3</li></div>"#;
        let v = ar.get_string_list_all(html, ".a li@text&&.b li@text", "");
        assert_eq!(v, vec!["A1", "A2", "A3", "B1", "B2", "B3"]);
    }

    // ---- P0-2：`&&` 多值用 `\n` 分隔 ----

    #[test]
    fn p0_2_and_joins_values_with_newline() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="a">A</div><div class="b">B</div>"#;
        assert_eq!(
            ar.get_string(html, ".a@text&&.b@text", false, "").unwrap(),
            "A\nB"
        );
        let json = r#"{"a":"A","b":"B"}"#;
        assert_eq!(ar.get_string(json, "$.a&&$.b", false, "").unwrap(), "A\nB");
    }

    // ---- P0-3：`##` 三种形态 ----

    #[test]
    fn p0_3_regex_replace_delete_all() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="n">第12章 标题</div>"#;
        assert_eq!(
            ar.get_string(html, ".n@text##第(\\d+)章", false, "")
                .unwrap()
                .trim(),
            "标题"
        );
    }

    #[test]
    fn p0_3_regex_replace_all_and_first_only() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="n">第12章 甲 第34章 乙</div>"#;
        // 三段：全替换
        assert_eq!(
            ar.get_string(html, ".n@text##第(\\d+)章##第$1节", false, "")
                .unwrap(),
            "第12节 甲 第34节 乙"
        );
        // 四段（尾部 `###`）：只替换第一个匹配，替换片段之后的原文必须保留
        assert_eq!(
            ar.get_string(html, ".n@text##第(\\d+)章##第$1节###", false, "")
                .unwrap(),
            "第12节 甲 第34章 乙"
        );
        // 无匹配时原样返回（而不是丢弃整串）
        assert_eq!(
            ar.get_string(html, ".n@text##第(\\d+)卷##第$1节###", false, "")
                .unwrap(),
            "第12章 甲 第34章 乙"
        );
    }

    // ---- P0-4：负索引 ----

    #[test]
    fn p0_4_negative_index_forms() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="list"><li>A</li><li>B</li><li>C</li></div>"#;
        assert_eq!(
            ar.get_string(html, "class.list@tag.li.-1@text", false, "")
                .unwrap(),
            "C"
        );
        let pics = r#"<div class="pic"><img src="1.jpg"><img src="2.jpg"><img src="3.jpg"></div>"#;
        assert_eq!(
            ar.get_string(pics, "class.pic@tag.img[-1]@src", false, "")
                .unwrap(),
            "3.jpg"
        );
        assert_eq!(
            ar.get_string_list_all(pics, "class.pic@tag.img[-2:]@src", ""),
            vec!["2.jpg", "3.jpg"]
        );
    }

    // ---- P0-5：HTML 实体反转义（只做一次） ----

    #[test]
    fn p0_5_html_entities_unescaped_once() {
        let ar = AnalyzeRule::new();
        let json = r#"{"title":"A &amp; B"}"#;
        assert_eq!(ar.get_string(json, "$.title", false, "").unwrap(), "A & B");
        // html5ever 已解一层，最终再解一层：`&amp;amp;` -> `&amp;` -> `&`
        let html = r#"<a class="x" href="/s?q=1&amp;amp;p=2">link</a>"#;
        assert_eq!(
            ar.get_string(html, ".x@href", false, "").unwrap(),
            "/s?q=1&p=2"
        );
    }

    // ---- P0-6：`{{…}}` 支持 JS 表达式 ----

    #[test]
    fn p0_6_template_js_expression() {
        let ar = AnalyzeRule::new();
        ar.set_var("page", "2");
        assert_eq!(
            ar.get_string("", "https://x/list/{{page*20}}", true, "")
                .unwrap(),
            "https://x/list/40"
        );
        assert_eq!(
            ar.get_string("", "https://x/list/{{(page-1)*20+10}}", true, "")
                .unwrap(),
            "https://x/list/30"
        );
        ar.set_var("key", "abc");
        assert_eq!(
            ar.get_string("", "https://x/{{java.base64Encode(key)}}", true, "")
                .unwrap(),
            "https://x/YWJj"
        );
    }

    // ---- P0-8：CSS 区间包含端点 / 步长 ----

    #[test]
    fn p0_8_css_range_is_inclusive_with_step() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="pic">
            <img src="1"><img src="2"><img src="3"><img src="4"><img src="5"><img src="6">
        </div>"#;
        // 闭区间：0..=2 共 3 个
        assert_eq!(
            ar.get_string_list_all(html, "class.pic@tag.img[0:2]@src", ""),
            vec!["1", "2", "3"]
        );
        // 负区间：最后 3 个
        assert_eq!(
            ar.get_string_list_all(html, "class.pic@tag.img[-3:]@src", ""),
            vec!["4", "5", "6"]
        );
        // 步长：0,2,4
        assert_eq!(
            ar.get_string_list_all(html, "class.pic@tag.img[::2]@src", ""),
            vec!["1", "3", "5"]
        );
        // 倒序：[-1:0] 全部反转
        assert_eq!(
            ar.get_string_list_all(html, "class.pic@tag.img[-1:0]@src", ""),
            vec!["6", "5", "4", "3", "2", "1"]
        );
    }

    // ---- F3：`.class.N` 点前缀索引（上游 findIndexSet 把尾部 `.N` 拆成下标） ----

    #[test]
    fn f3_dot_prefix_index_in_initial_selector() {
        let ar = AnalyzeRule::new();
        let html = r#"<div><span class="author">作者甲</span><span class="author">字数：1.0万</span></div>"#;
        // JSoup 规则.ElementsSingle.findIndexSet：`.author.0` -> beforeRule=`.author`、split='.'、index=[0]
        assert_eq!(
            ar.get_string(html, ".author.0@text", false, "").unwrap(),
            "作者甲"
        );
        assert_eq!(
            ar.get_string(html, ".author.1@text", false, "").unwrap(),
            "字数：1.0万"
        );
        // 与 `##` 替换组合（case 0/99 的实际写法）
        assert_eq!(
            ar.get_string(html, ".author.0@text##作者：", false, "")
                .unwrap(),
            "作者甲"
        );
    }

    // ---- F4：`class.x.y` 三段链只取第一个类名分段（上游 rules[1]） ----

    #[test]
    fn f4_class_chain_uses_first_segment_like_upstream() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="list"><div class="list_tit">目录</div><ul>
            <li><a href="/1">第一章</a></li><li><a href="/2">第二章</a></li></ul></div>"#;
        // JSoup 规则.getElementsSingle：beforeRule.split(".") 后 class 分支只用 rules[1]="list"
        let chapters = ar.get_string_list(html, "class.list.ul@li").unwrap();
        assert_eq!(chapters.len(), 2);
        assert_eq!(
            ar.get_string(&chapters[0], "a@text", false, "").unwrap(),
            "第一章"
        );
        assert_eq!(
            ar.get_string(&chapters[1], "a@href", false, "").unwrap(),
            "/2"
        );
    }

    // ---- F5：`@text` 块级元素之间补空格（jsoup Element.text() 行为） ----

    #[test]
    fn f5_text_inserts_space_between_block_elements() {
        let ar = AnalyzeRule::new();
        let html =
            r#"<div class="text"><p>二月二，龙抬头。</p><p>暮色里，小镇名叫泥瓶巷。</p></div>"#;
        assert_eq!(
            ar.get_string(html, "class.text@text", false, "").unwrap(),
            "二月二，龙抬头。 暮色里，小镇名叫泥瓶巷。"
        );
    }
}
