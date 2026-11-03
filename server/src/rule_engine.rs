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
use scraper::{Html, Selector};
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
        timeout_ms: 200,
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
    /// (求值结果, 其中 java.ajax 的网络耗时毫秒)
    reply: Sender<(Result<String, String>, u64)>,
}

/// 全局唯一的 JS 执行线程入口（容量 8，避免无界排队）。
static JS_TX: OnceLock<SyncSender<JsJob>> = OnceLock::new();

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

/// 专用线程主循环：只创建一次 Context，之后复用处理所有任务。
fn js_worker(rx: Receiver<JsJob>) {
    let conf = js_conf();
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
    while let Ok(job) = rx.recv() {
        JS_AJAX_NET_MS.store(0, Ordering::Relaxed);
        let res = ctx
            .eval(boa_engine::Source::from_bytes(job.script.as_bytes()))
            // `{e:?}` 对 opaque Error 只会打印 ErrorObject 地址，书源作者看不懂。
            // 用 Display 取出 message；垫片抛出的未支持 API 显式给出特性名。
            .map_err(|e| js_error_message(&e.to_string()))
            .and_then(|v| {
                v.to_string(&mut ctx)
                    .map_err(|e| format!("js to_string: {e}"))
            })
            .map(|s| s.to_std_string().unwrap_or_default());
        let net_ms = JS_AJAX_NET_MS.load(Ordering::Relaxed);
        let _ = job.reply.send((res, net_ms));
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
  var pct = "";
  for (var i = 0; i < bin.length; i++) {
    pct += "%" + ("0" + bin.charCodeAt(i).toString(16)).slice(-2);
  }
  try { return decodeURIComponent(pct); } catch (e) { return ""; }
}
// 纯 JS base64，避免依赖宿主是否提供 btoa/atob（boa 0.19 默认不带）
var __B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
function __b64enc(bin){
  var out = "", i;
  for (i = 0; i < bin.length; i += 3) {
    var c1 = bin.charCodeAt(i);
    var c2 = i + 1 < bin.length ? bin.charCodeAt(i + 1) : 0;
    var c3 = i + 2 < bin.length ? bin.charCodeAt(i + 2) : 0;
    var e1 = c1 >> 2, e2 = ((c1 & 3) << 4) | (c2 >> 4);
    var e3 = ((c2 & 15) << 2) | (c3 >> 6), e4 = c3 & 63;
    out += __B64.charAt(e1) + __B64.charAt(e2);
    out += (i + 1 < bin.length ? __B64.charAt(e3) : "=");
    out += (i + 2 < bin.length ? __B64.charAt(e4) : "=");
  }
  return out;
}
function __b64dec(s){
  s = String(s).replace(/[^A-Za-z0-9+/=]/g, "");
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

const JAVA_BOOK_SHIM: &str = r#"
var __vars = {};
// DOM 桥 Phase 1 的预抽取结果（由 Rust 在 eval 前按字面量选择器算好并赋值）
var __DOM = {};
var __DOMS = {};
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
  base64Encode: function(s){ try { return __b64enc(__utf8(s)); } catch(e){ return ""; } },
  base64Decode: function(s){ try { return __fromUtf8(__b64dec(s)); } catch(e){ return ""; } },
  base64DecodeToString: function(s){ return java.base64Decode(s); },
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
  hexDecode: function(s){
    s = String(s || "").replace(/\s+/g, "");
    if (s.length % 2 !== 0) return "";
    var pct = "";
    for (var i = 0; i < s.length; i += 2) { pct += "%" + s.slice(i, i + 2); }
    try { return decodeURIComponent(pct); } catch(e){ return ""; }
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
  // DOM 桥 Phase 1：只认 Rust 预抽取好的“字面量选择器”结果（__DOM / __DOMS），
  // 动态拼串或页面超限不走这里，直接给出具名未支持提示。
  getElements: function(rule, index){
    if (rule == null || !__DOM.hasOwnProperty(String(rule))) {
      throw new Error("UNSUPPORTED_JS_API:java.getElements（仅支持字面量选择器参数）");
    }
    var arr = __DOM[String(rule)].slice(0);
    if (typeof index === "number") {
      return (index >= 0 && index < arr.length) ? arr[index] : "";
    }
    return arr;
  },
  getStringList: function(rule){
    if (rule == null || !__DOM.hasOwnProperty(String(rule))) {
      throw new Error("UNSUPPORTED_JS_API:java.getStringList（仅支持字面量选择器参数）");
    }
    return __DOM[String(rule)].slice(0);
  },
  getString: function(rule){
    if (rule == null || !__DOMS.hasOwnProperty(String(rule))) {
      throw new Error("UNSUPPORTED_JS_API:java.getString（仅支持字面量选择器参数）");
    }
    return __DOMS[String(rule)];
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
    pub js_lib: String,
    pub is_json: bool,
}

impl Default for AnalyzeRule {
    fn default() -> Self {
        Self {
            variables: RefCell::new(HashMap::new()),
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

    pub fn set_vars(&self, m: HashMap<String, String>) {
        self.variables.borrow_mut().extend(m);
    }

    /// 变量/JSONPath 模板展开：支持 `${var}`、`{{var}}`，以及 `{{$.field}}` / `${$.field}`
    /// （JSONPath 形式按**当前规则输入** `content` 求值，用于书源里
    /// `.../read?bid={{$.id}}` 这类“列表项字段拼进 URL/POST body”的写法）。
    fn expand_vars(&self, rule: &str, content: &str) -> String {
        let re = Regex::new(r"\$\{([^}]+)\}|\{\{([^}]+)\}\}").unwrap();
        let json: Option<Value> = if rule.contains("$.") || rule.contains("$[") {
            serde_json::from_str(content).ok()
        } else {
            None
        };
        re.replace_all(rule, |caps: &regex::Captures| {
            let key = caps
                .get(1)
                .or_else(|| caps.get(2))
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
            self.variables.borrow().get(key).cloned().unwrap_or_default()
        })
        .to_string()
    }

    fn detect_mode(rule: &str, is_json: bool) -> Mode {
        if rule.starts_with("@xpath:") {
            Mode::XPath
        } else if rule.starts_with("@json:") {
            Mode::Json
        } else if rule.starts_with("@css:") || rule.starts_with("@@") {
            Mode::Css
        } else if rule.starts_with("$.") || rule.starts_with("$[") || is_json {
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
                if s.starts_with("@js:") {
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

    fn strip_prefix<'a>(rule: &'a str, p: &str) -> &'a str {
        rule.strip_prefix(p).unwrap_or(rule)
    }

    /// 单值管道：对 content 应用一条规则串，逐段执行，上一步结果作为下一步输入
    pub fn get_string(
        &self,
        content: &str,
        rule: &str,
        is_url: bool,
        base_url: &str,
    ) -> Result<String, RuleError> {
        let rule = self.expand_vars(rule, content);
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
        // @get:{key} / @put:{key:value} 规则级变量操作符（Legado 跨规则变量传递）
        let trimmed = rule.trim();
        if let Some(rest) = trimmed.strip_prefix("@get:{") {
            if rest.ends_with('}') {
                let key = &rest[..rest.len() - 1];
                return Ok(self
                    .variables
                    .borrow()
                    .get(key)
                    .cloned()
                    .unwrap_or_default());
            }
        }
        if let Some(rest) = trimmed.strip_prefix("@put:{") {
            if let Some(end) = rest.rfind('}') {
                let inner = &rest[..end];
                if let Some(colon) = inner.find(':') {
                    let key = inner[..colon].trim().to_string();
                    let mut val = inner[colon + 1..].trim();
                    // 去掉可能的引号包裹
                    if val.starts_with('"') && val.ends_with('"') && val.len() >= 2 {
                        val = &val[1..val.len() - 1];
                    }
                    let res = self.get_string(content, val, is_url, base_url)?;
                    self.variables.borrow_mut().insert(key, res.clone());
                    return Ok(res);
                }
            }
        }
        // 顶层操作符（不在 <js>…</js> / @js: 段内）：|| 回退、&& 拼接、## 正则替换
        if let Some(parts) = split_outside_js(&rule, "||") {
            for p in &parts {
                let r = self.get_string(content, p, is_url, base_url)?;
                if !r.is_empty() {
                    return Ok(r);
                }
            }
            return Ok(String::new());
        }
        if let Some(parts) = split_outside_js(&rule, "&&") {
            let mut out = String::new();
            for p in &parts {
                out.push_str(&self.get_string(content, p, is_url, base_url)?);
            }
            return Ok(out);
        }
        if let Some(parts) = split_outside_js(&rule, "##") {
            let sub = self.get_string(content, parts[0], is_url, base_url)?;
            if parts.len() >= 2 && !parts[1].trim().is_empty() {
                if let Ok(re) = Regex::new(parts[1].trim()) {
                    let repl = parts.get(2).map(|s| s.trim()).unwrap_or("");
                    return Ok(re.replace_all(&sub, repl).to_string());
                }
            }
            return Ok(sub);
        }
        // 裸抽取标记：`text` / `href` / `src` 等没有选择器前缀时，表示“取当前节点”。
        // 不能当成 CSS 标签选择器（否则章节标题/链接会解析成空）。
        let bare = rule.trim();
        if !self.is_json && is_extract_keyword(bare) {
            return Ok(extract_first(content, bare));
        }
        // 默认：<js> / @js: 段切分管道
        let mut input = content.to_string();
        for (mode, text) in Self::split_js(&rule) {
            let t = text.trim();
            if t.is_empty() {
                continue;
            }
            input = match mode {
                Mode::Js => self.eval_js(&input, t, base_url)?,
                _ => {
                    let m = if mode == Mode::Default {
                        Self::detect_mode(t, self.is_json)
                    } else {
                        mode
                    };
                    self.apply_by_mode(&input, m, t)?
                }
            };
        }
        Ok(input)
    }

    fn apply_by_mode(&self, input: &str, mode: Mode, rule: &str) -> Result<String, RuleError> {
        match mode {
            Mode::XPath => self.apply_xpath(input, Self::strip_prefix(rule, "@xpath:")),
            Mode::Json => self.apply_json(input, Self::strip_prefix(rule, "@json:")),
            Mode::Regex => self.apply_regex(input, rule),
            _ => self.apply_css(input, Self::strip_prefix(rule, "@css:")),
        }
    }

    fn apply_css(&self, html: &str, rule: &str) -> Result<String, RuleError> {
        let (selector, tokens) = split_css_rule(rule);
        let doc = scraper::Html::parse_document(html);
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
        // 单值抽取：Legado getString 只取首个匹配元素（getString 返回 list.get(0)）
        match terminal {
            Some(t) if t == "textNodes" => Ok(els
                .first()
                .map(|e| text_nodes(e))
                .unwrap_or_default()),
            Some(t) => Ok(els
                .first()
                .map(|e| extract_token(*e, &t))
                .unwrap_or_default()),
            None => Ok(els
                .first()
                .map(|e| e.inner_html())
                .unwrap_or_default()),
        }
    }

    fn apply_json(&self, json: &str, rule: &str) -> Result<String, RuleError> {
        let v: Value =
            serde_json::from_str(json).map_err(|e| RuleError::Json(e.to_string()))?;
        let mut vals = json_query(&v, rule)?;
        let val = match vals.pop() {
            Some(v) => v,
            None => return Ok(String::new()),
        };
        match val {
            Value::String(s) => Ok(s),
            Value::Number(n) => Ok(n.to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            other => Ok(other.to_string()),
        }
    }

    fn apply_regex(&self, input: &str, rule: &str) -> Result<String, RuleError> {
        // 斜杠包裹规则：/pat/ 为提取（捕获组1），/pat/repl/ 为替换
        if rule.starts_with('/') {
            let interior = &rule[1..]; // 含结尾斜杠
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
        let package = xml_parse(content).map_err(|e| RuleError::Other(format!("xpath parse: {e:?}")))?;
        let doc = package.as_document();
        let val =
            evaluate_xpath(&doc, rule).map_err(|e| RuleError::Other(format!("xpath eval: {e:?}")))?;
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
                serde_json::Value::Array(
                    vals.into_iter().map(serde_json::Value::String).collect(),
                ),
            );
        }
        (lists, singles)
    }

    fn eval_js(&self, input: &str, code: &str, base_url: &str) -> Result<String, RuleError> {
        let conf = js_conf();
        if !conf.enabled {
            return Err(RuleError::Other(
                "JS rules disabled by JS_ENABLED=0".into(),
            ));
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
        let script = format!(
            "(function(){{ var result={r}; var __input={r}; var __base={b}; var baseUrl=__base;\n{lib}\n{shim}\n__DOM={dom};\n__DOMS={doms};\nvar __r = eval({code});\nif (Array.isArray(__r)) {{ try {{ return JSON.stringify(__r); }} catch(__e) {{}} }}\nreturn String(__r); }})()",
            r = js_string_literal(input),
            b = js_string_literal(base_url),
            lib = self.js_lib,
            shim = shim,
            dom = dom_js,
            doms = doms_js,
            code = js_string_literal(code),
        );
        let start = Instant::now();
        let (tx, rx) = channel::<(Result<String, String>, u64)>();
        js_sender()
            .send(JsJob { script, reply: tx })
            .map_err(|_| RuleError::Other("js worker unavailable".into()))?;
        let (result, net_ms) = rx
            .recv()
            .map_err(|_| RuleError::Other("js worker dropped".into()))?;
        let (result, net_ms) = (result.map_err(RuleError::Other), net_ms);
        let elapsed = start.elapsed().as_millis() as u64;
        // java.ajax 的网络等待不计入纯 JS 预算（否则一次正常联网就会触发 200ms 超时）
        let js_elapsed = elapsed.saturating_sub(net_ms);
        JS_LAST_MS.store(js_elapsed, Ordering::Relaxed);
        JS_MAX_MS.fetch_max(js_elapsed, Ordering::Relaxed);
        if js_elapsed > conf.timeout_ms {
            tracing::warn!(
                elapsed_ms = js_elapsed,
                net_ms,
                budget_ms = conf.timeout_ms,
                "js rule exceeded time budget"
            );
            return Err(RuleError::Other(format!(
                "js eval exceeded {}ms budget (took {}ms)",
                conf.timeout_ms, js_elapsed
            )));
        }
        result
    }

    /// 列表：返回多个元素的字符串（HTML 取 inner_html；JSON 取数组元素字符串）
    pub fn get_string_list(&self, content: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        let rule = self.expand_vars(rule, content);
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
                    _ => self.get_string(&input, t, false, "")?,
                };
            }
            return Ok(vec![input]);
        }
        let r = rule.trim_start();
        if r.starts_with("@js:") || rule.contains("<js>") {
            // 走与单值相同的分段管道：`CSS前缀<js>…</js>` 只把 JS 段交给求值器，
            // 不再把 `<js>` 标签和前缀一起塞进 boa（否则会在 `<js>` 处语法报错）。
            let res = self.get_string(content, &rule, false, "")?;
            // 整段 JS 常返回数组（如 java.getElements 的结果），序列化为 JSON 数组则拆成多项
            if res.starts_with('[') {
                if let Ok(arr) = serde_json::from_str::<Vec<String>>(&res) {
                    return Ok(arr);
                }
            }
            return Ok(vec![res]);
        }
        let mode = Self::detect_mode(&rule, self.is_json);
        match mode {
            Mode::Json => self.json_list(content, Self::strip_prefix(&rule, "@json:")),
            _ => self.css_list(content, Self::strip_prefix(&rule, "@css:")),
        }
    }

    /// 列表求值（保留末端抽取标记）：用于 `nextTocUrl` 等“可能返回多条 URL”的规则。
    /// 纯 CSS（如 `.pages a@href` / `@css:option@value`）对每个匹配元素分别抽取；
    /// JsonPath 返回数组；含 `&&`/`||`/`##`/`@js:` 的复合规则退回单值求值（最多 1 条）。
    pub fn get_string_list_all(
        &self,
        content: &str,
        rule: &str,
        base_url: &str,
    ) -> Vec<String> {
        let rule = self.expand_vars(rule, content);
        let r = rule.trim();
        if r.is_empty() {
            return vec![];
        }
        let complex = r.contains("<js>")
            || r.contains("@js:")
            || r.contains("##")
            || r.contains("&&")
            || r.contains("||");
        if complex {
            return match self.get_string(content, r, true, base_url) {
                Ok(s) if !s.trim().is_empty() => vec![s],
                _ => vec![],
            };
        }
        match Self::detect_mode(r, self.is_json) {
            Mode::Json => self
                .json_list(content, Self::strip_prefix(r, "@json:"))
                .unwrap_or_default(),
            Mode::Css => {
                let (selector, tokens) = split_css_rule(r);
                let doc = scraper::Html::parse_document(content);
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
                    Some(t) if t == "textNodes" => {
                        els.iter().map(|e| text_nodes(e)).collect()
                    }
                    Some(t) => els.iter().map(|e| extract_token(*e, &t)).collect(),
                    None => els.iter().map(|e| e.html()).collect(),
                }
            }
            _ => match self.get_string(content, r, true, base_url) {
                Ok(s) if !s.trim().is_empty() => vec![s],
                _ => vec![],
            },
        }
    }

    fn css_list(&self, html: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        Ok(elements_html(html, rule))
    }

    fn json_list(&self, json: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        let v: Value =
            serde_json::from_str(json).map_err(|e| RuleError::Json(e.to_string()))?;
        Ok(json_query(&v, rule)?
            .into_iter()
            .map(value_to_string)
            .collect())
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
                let end = cs[i..]
                    .iter()
                    .position(|c| *c == ']')
                    .map(|p| i + p)
                    .ok_or_else(|| format!("方括号未闭合：{s}"))?;
                let inner: String = cs[i + 1..end].iter().collect();
                let inner = inner.trim();
                if inner == "*" {
                    segs.push(JpSeg::Wildcard);
                } else if let Ok(n) = inner.parse::<i64>() {
                    segs.push(JpSeg::Index(n));
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
                        let idx = if *n < 0 {
                            a.len() as i64 + *n
                        } else {
                            *n
                        };
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
        }
        cur = next;
    }
    cur
}

/// 对 JSON 执行 JSONPath：先走本地子集解析，含过滤表达式等复杂写法再退回 jsonpath-rust；
/// 两条路径的语法错误都统一成“JSONPath 语法不受支持：…”，不暴露 PestError。
fn json_query(v: &Value, rule: &str) -> Result<Vec<Value>, RuleError> {
    let rule = rule.trim();
    match parse_json_path(rule) {
        Ok(segs) => Ok(eval_json_path(v, &segs)),
        Err(local_err) => {
            let complex = rule.contains("[?") || rule.contains("](") || rule.contains("&&");
            if complex {
                match v.clone().path(rule) {
                    Ok(Value::Array(arr)) => Ok(arr),
                    Ok(other) => Ok(vec![other]),
                    Err(e) => Err(RuleError::Json(format!(
                        "JSONPath 语法不受支持：{e}"
                    ))),
                }
            } else {
                Err(RuleError::Json(format!(
                    "JSONPath 语法不受支持：{local_err}"
                )))
            }
        }
    }
}

/// 对单个元素按抽取标记取值（@text/@href/@content/@src/@html/@ownText/@all 等）。
fn extract_token(el: scraper::ElementRef, tok: &str) -> String {
    match tok {
        "text" | "ownText" => el.text().collect::<Vec<_>>().join(""),
        "html" => el.inner_html(),
        "all" => el.html(),
        "href" | "src" | "alt" | "content" | "id" | "value" | "class" | "style" | "title"
        | "name" => el.value().attr(tok).unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// 抽取/遍历 token 解析结果。
enum Token {
    Children,
    Tag {
        name: String,
        index: Option<usize>,
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
            name,
            index,
            indices: vec![],
            exclude: false,
        };
    }
    if is_extract_keyword(tok) {
        return Token::Terminal(tok.to_string());
    }
    let (name, index) = split_name_index(tok);
    // `@.book.0` / `@#id` 是 Legado 默认规则里的 class/id 简写；此前被当成末端抽取
    // 标记，选择器结果直接丢弃（错层小说网详情 cover/kind 全 null 的原因）。
    let css_shorthand = (name.starts_with('.') || name.starts_with('#')) && name.len() > 1;
    if css_shorthand || (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic())) {
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
fn build_spec(name: &str, index: Option<usize>) -> String {
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
fn split_css_rule<'a>(rule: &'a str) -> (String, Vec<&'a str>) {
    let rule = rule
        .strip_prefix("@css:")
        .or_else(|| rule.strip_prefix("@CSS:"))
        .unwrap_or(rule)
        .trim_start_matches("@@")
        .trim();
    let parts: Vec<&str> = rule.split('@').collect();
    let selector = parts[0].trim().to_string();
    let tokens: Vec<&str> = parts[1..].iter().map(|s| s.trim()).collect();
    (selector, tokens)
}

/// 解析 `name` 或 `name.0` 形式的规格，返回 (名称, 可选索引)。
fn split_name_index(s: &str) -> (String, Option<usize>) {
    if let Some(dot) = s.rfind('.') {
        let (name, idxs) = s.split_at(dot);
        if let Ok(i) = idxs[1..].parse::<usize>() {
            return (name.to_string(), Some(i));
        }
    }
    (s.to_string(), None)
}

/// 初始选择器：支持 Legado 的 `class.x` / `id.x` / `tag.x`、可选索引（取第 i 个），
/// 以及 `!下标`（Legado 语义是**排除**这些下标，如 `li!0:1:2:3:4` 去掉前 5 个 li）。
fn select_initial<'a>(doc: &'a scraper::Html, selector: &str) -> Vec<scraper::ElementRef<'a>> {
    let selector = selector.trim();
    let (core, excluded) = match selector.find('!') {
        Some(pos) => (&selector[..pos], parse_indices(&selector[pos + 1..])),
        None => (selector, Vec::new()),
    };
    let (css, idx) = if let Some(rest) = core.trim().strip_prefix("class.") {
        let (name, i) = split_name_index(rest);
        (format!(".{name}"), i)
    } else if let Some(rest) = core.trim().strip_prefix("id.") {
        let (name, i) = split_name_index(rest);
        (format!("#{name}"), i)
    } else if let Some(rest) = core.trim().strip_prefix("tag.") {
        // Legado 常用 `tag.a@href`：`tag.` 前缀表示标签名，不是合法 CSS 选择器
        let (name, i) = split_name_index(rest);
        (name, i)
    } else {
        (core.trim().to_string(), None)
    };
    let sel = match scraper::Selector::parse(&css) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let els: Vec<scraper::ElementRef<'a>> = doc.select(&sel).collect();
    let els = filter_indices(els, &excluded, true);
    if let Some(i) = idx {
        if i < els.len() {
            vec![els[i]]
        } else {
            vec![]
        }
    } else {
        els
    }
}

/// 取元素的直接子元素（仅元素节点）。
fn child_elements<'a>(el: scraper::ElementRef<'a>) -> Vec<scraper::ElementRef<'a>> {
    el.children().filter_map(scraper::ElementRef::wrap).collect()
}

/// 从当前元素集合向下钻取到指定标签（@tag.name[.index]）。
/// Legado 的 `@tag.x` 是**后代**查询（jsoup `getElementsByTag`），不是只看直接子元素；
/// 需要直接子元素时用 `@children`。
fn apply_tag<'a>(els: Vec<scraper::ElementRef<'a>>, spec: &str) -> Vec<scraper::ElementRef<'a>> {
    let (tag, idx) = split_name_index(spec);
    let sel = match scraper::Selector::parse(&tag) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let mut matched: Vec<scraper::ElementRef<'a>> = Vec::new();
    for e in &els {
        matched.extend(e.select(&sel));
    }
    if let Some(i) = idx {
        if i < matched.len() {
            vec![matched[i]]
        } else {
            vec![]
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
        b'(' | b',' | b'=' | b':' | b'[' | b'!' | b'&' | b'|' | b'?' | b'{' | b'}' | b';'
            | b'+' | b'-' | b'*' | b'%' | b'^' | b'~' | b'<' | b'>'
    ) {
        return true;
    }
    // `return /re/` 这类关键字后的 `/` 也是正则，而不是除法
    let mut k = j;
    while k > 0 && ((b[k - 1] as char).is_ascii_alphanumeric() || matches!(b[k - 1], b'_' | b'$'))
    {
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

/// 在规则串顶层按分隔符拆分，但忽略落在 `<js>…</js>` 或 `@js:` 段内的分隔符。
/// 仅当当前字节为 ASCII 时才尝试 `starts_with`（分隔符/标记均为 ASCII），避免多字节
/// UTF-8（如中文）字符内部切片越界。
fn split_outside_js<'a>(rule: &'a str, sep: &str) -> Option<Vec<&'a str>> {
    let mut parts: Vec<&str> = Vec::new();
    let bytes = rule.as_bytes();
    let n = bytes.len();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < n {
        if bytes[i] < 0x80 {
            let s = &rule[i..];
            if s.starts_with("<js>") {
                if let Some(pos) = find_js_tag_end(rule, i + 4) {
                    let end = pos + 5;
                    i = end;
                    start = end;
                    continue;
                }
            }
            if s.starts_with("@js:") {
                if start < i {
                    parts.push(&rule[start..i]);
                }
                parts.push(&rule[i..]);
                // 整条规则就是 `@js:…` 时不能返回单元素切分：否则调用方会把该段
                // 再喂回 get_string，造成无限递归（小栈 worker 上直接栈溢出）。
                return if parts.len() > 1 { Some(parts) } else { None };
            }
            if s.starts_with(sep) {
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
            .get_string(
                html,
                ".content@.book.0@.bookcover@tag.img@src",
                false,
                "",
            )
            .unwrap();
        assert_eq!(cover, "/cover.jpg");
        let kind = ar
            .get_string(html, ".content@.book.0@.bookinfo@tag.a.1@text", false, "")
            .unwrap();
        assert_eq!(kind, "玄幻");
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
            .get_string(html, r#"@js: java.getString("class.title@text")"#, false, "")
            .unwrap();
        assert_eq!(title, "测试书");
        let items = ar
            .get_string_list(html, r#"@js: java.getStringList("class.item")"#)
            .unwrap();
        assert_eq!(items.len(), 2);
        assert!(items[0].contains("甲"));
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
        assert!(err.contains("字面量"), "应提示只支持字面量参数，实际: {err}");
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
        // @children 取直接子节点（li），再取 a 的 href
        let r = ar
            .get_string(html, "@CSS:ul@children@tag.a@href", false, "")
            .unwrap();
        assert_eq!(r, "/x");
        let html2 = r#"<div>前缀<span>内</span>后缀</div>"#;
        let t = ar
            .get_string(html2, "@CSS:div@textNodes", false, "")
            .unwrap();
        assert_eq!(t, "前缀后缀");
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
            .get_string("", "<js>java.toNumChapter(\"第一百一十二\")</js>", false, "")
            .unwrap();
        assert_eq!(r, "112");
    }

    #[test]
    fn put_get_variables() {
        let ar = AnalyzeRule::new();
        let html = "<div><a>lit</a></div>";
        // @put 求值后存入变量，@get 取回
        let stored = ar.get_string(html, "@put:{x:a@text}", false, "").unwrap();
        assert_eq!(stored, "lit");
        let got = ar.get_string(html, "@get:{x}", false, "").unwrap();
        assert_eq!(got, "lit");
    }

    #[test]
    fn brace_var_expansion() {
        let ar = AnalyzeRule::new();
        ar.set_var("y", "world");
        // {{var}} 在 JS 段内展开
        let r = ar
            .get_string("", "<js>\"{{y}}\"</js>", false, "")
            .unwrap();
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
        let href = ar.get_string(html, "@css:p:nth-of-type(1) a@href", true, "").unwrap();
        assert_eq!(href, "/book/1");
    }

    #[test]
    fn bare_tag_and_excluded_indices() {
        let ar = AnalyzeRule::new();
        let html = r#"<dl id="list"><dd>第1</dd><dd>第2</dd><dd>第3</dd><dd>第4</dd><dd>第5</dd></dl>"#;
        // 裸标签遍历（@dd）+ 单值取首个
        let t1 = ar.get_string(html, "@CSS:id.list@dd@text", false, "").unwrap();
        assert_eq!(t1, "第1");
        // Legado 的 `!0:1` 是“排除第 0、1 个”，剩余第 3/4/5 个，取首个 = 第3
        let t2 = ar.get_string(html, "@CSS:id.list@dd!0:1@text", false, "").unwrap();
        assert_eq!(t2, "第3");
        // `!2` 排除第 3 个，取首个 = 第1
        let t3 = ar.get_string(html, "@CSS:id.list@dd!2@text", false, "").unwrap();
        assert_eq!(t3, "第1");
    }

    // B3：初始选择器带 `!下标`（精华书阁真实形态 `li!0:1:2:3:4@a` / `option!0@value`）
    #[test]
    fn initial_selector_excludes_indices() {
        let ar = AnalyzeRule::new();
        // 前 5 个 li 是书头信息，后面才是章节
        let html = r#"<ul class="chapter"><li>书头0</li><li>书头1</li><li>书头2</li><li>书头3</li><li>书头4</li><li><a href="/c6">第6章</a></li><li><a href="/c7">第7章</a></li></ul>"#;
        let items = ar.get_string_list(html, "li!0:1:2:3:4@a").unwrap();
        assert_eq!(items.len(), 2, "前 5 个 li 应被 `!0:1:2:3:4` 排除");
        assert_eq!(ar.get_string(&items[0], "text", false, "").unwrap(), "第6章");
        assert_eq!(ar.get_string(&items[0], "href", true, "").unwrap(), "/c6");

        let html2 = r#"<select><option value="">请选择</option><option value="/c1">1</option></select>"#;
        assert_eq!(ar.get_string(html2, "option!0@value", true, "").unwrap(), "/c1");
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
        let out = ar.get_string(input, "<js>result.toUpperCase()</js>", false, "").unwrap();
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
        assert_eq!(ar.get_string(html, "class.au@text", false, "").unwrap(), "作者");
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
    fn variable_expansion_works() {
        let mut ar = AnalyzeRule::new();
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
            ar.get_string(html, "@css:.page${page} a@href", true, "").unwrap(),
            "/b"
        );
    }

    // nextTocUrl 需要“对每个元素抽取属性”，而不是只取首个元素
    #[test]
    fn list_all_extracts_multiple_hrefs() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="pages"><a href="/toc/2.html">2</a><a href="/toc/3.html">3</a></div>"#;
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
        let names = ar
            .get_string_list(json, "$.booklist[*].book_name")
            .unwrap();
        assert_eq!(names, vec!["书C"]);
        // 单值：递归下降取最后一项（与既有单值语义一致）
        assert_eq!(ar.get_string(json, "$..book_name", false, "").unwrap(), "书B");
        // 下标（含负下标）
        assert_eq!(ar.get_string(json, "$.data.books[1].id", false, "").unwrap(), "b2");
        assert_eq!(ar.get_string(json, "$.data.books[-1].id", false, "").unwrap(), "b2");
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
            ar.get_string(item, "{\"bid\":\"{{$.id}}\"}", false, "").unwrap(),
            "{\"bid\":\"42\"}"
        );
    }

    // 切分 JS 时字符串/正则里的 `</js>`、`||`、引号不能截断代码
    #[test]
    fn js_split_ignores_tags_inside_strings_and_regex() {
        let parts = AnalyzeRule::split_js(r#"<js>result.replace("</js>", "").replace(/a||b/g, "")</js>"#);
        assert_eq!(parts.len(), 1, "整段应只有一个 JS 段");
        let code = match &parts[0] {
            (Mode::Js, c) => c.clone(),
            _ => panic!("unexpected part: not a JS segment"),
        };
        assert!(code.contains("</js>"), "字符串里的 </js> 不应截断: {code}");
        let ar = AnalyzeRule::new();
        assert_eq!(
            ar.get_string("x</js>y", r#"<js>result.replace("</js>", "")</js>"#, false, "")
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
}

