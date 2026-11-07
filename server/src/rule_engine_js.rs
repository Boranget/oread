//! JS 规则执行层（move-only 从 rule_engine.rs 拆出）。
//!
//! 包含 boa 运行时、worker/并发闸门、Legado Java/JSoup shim 与原生扩展；
//! 对父模块暴露 init_js / js_metrics 与内部 job 入口，其余保持模块私有。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use md5::{Digest, Md5};
use once_cell::sync::{Lazy, OnceCell};
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use serde_json::Value;

use super::html::*;
use super::{AnalyzeRule, RuleError};

/// JS 规则预算。boa 0.19 无中断/时间片 API，因此用「循环迭代上限 + 递归/栈上限」
/// 做滥用防护；墙钟耗时用于统计与超时提示（见 `JS_TIMEOUT_MS`）。
#[derive(Clone, Copy)]
pub(super) struct JsConf {
    pub(super) enabled: bool,
    pub(super) timeout_ms: u64,
    pub(super) loop_iteration_limit: u64,
    pub(super) recursion_limit: usize,
    pub(super) stack_size_limit: usize,
}

pub(super) static JS_CONF: OnceCell<JsConf> = OnceCell::new();
pub(super) static JS_LAST_MS: AtomicU64 = AtomicU64::new(0);
pub(super) static JS_MAX_MS: AtomicU64 = AtomicU64::new(0);
/// 当前 JS 任务里 `java.ajax` 阻塞联网花费的毫秒数（JS 预算只算纯脚本耗时）。
pub(super) static JS_AJAX_NET_MS: AtomicU64 = AtomicU64::new(0);

pub fn init_js(enabled: bool, timeout_ms: u64) {
    let _ = JS_CONF.set(JsConf {
        enabled,
        timeout_ms,
        loop_iteration_limit: 500_000,
        recursion_limit: 100,
        stack_size_limit: 512,
    });
}

/// `cache.getFile/putFile` 的落盘目录（部署侧 `<data_dir>/cache/rule`）。
/// 未初始化时（单测）退回系统临时目录下的独立子目录。
pub(super) static RULE_CACHE_DIR: OnceCell<PathBuf> = OnceCell::new();

pub fn init_rule_cache_dir(dir: impl Into<PathBuf>) {
    let _ = RULE_CACHE_DIR.set(dir.into());
}

pub(super) fn rule_cache_dir() -> PathBuf {
    RULE_CACHE_DIR
        .get()
        .cloned()
        .unwrap_or_else(|| std::env::temp_dir().join("boranlib-rule-cache"))
}

/// `cache.putFile` 单文件上限（Legado 无上限，这里给出明确边界）。
pub(super) const CACHE_FILE_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Legado `cache.getFile/putFile` 用 `key.hashCode()` 作文件名；这里改用 md5
/// （跨进程稳定，且天然屏蔽 `../` 等路径穿越——key 只当不透明标识，不当路径用）。
pub(super) fn cache_file_path(key: &str) -> PathBuf {
    let mut hasher = Md5::new();
    hasher.update(key.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    rule_cache_dir().join(digest)
}

pub(super) fn js_cache_file_get(key: &str) -> Result<String, String> {
    if key.is_empty() {
        return Err("cache.getFile: key 不能为空".into());
    }
    match std::fs::read_to_string(cache_file_path(key)) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("cache.getFile 读取失败：{e}")),
    }
}

pub(super) fn js_cache_file_put(key: &str, value: &str, save_time: i64) -> Result<(), String> {
    if key.is_empty() {
        return Err("cache.putFile: key 不能为空".into());
    }
    if value.len() > CACHE_FILE_MAX_BYTES {
        return Err("cache.putFile: 内容超过 16MB 上限".into());
    }
    let dir = rule_cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cache.putFile 创建缓存目录失败：{e}"))?;
    let body = if save_time != 0 {
        // Legado 的过期前缀：13 位当前毫秒 + "-" + saveTime + 空格 + 内容（getFile 原样返回）。
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("{now:013}-{save_time} {value}")
    } else {
        value.to_string()
    };
    std::fs::write(cache_file_path(key), body).map_err(|e| format!("cache.putFile 写入失败：{e}"))
}

/// 书源级变量（Legado `source.get/put` 落到 `CacheManager` 的 `v_<sourceKey>_<key>`）。
/// 本服务按进程内、按书源分桶保存：不落盘，重启即清空；Legado 的磁盘缓存语义未复刻。
pub(super) const SOURCE_VAR_MAX_VALUE_BYTES: usize = 256 * 1024;
pub(super) const SOURCE_VAR_MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(super) struct SourceVarStore {
    buckets: HashMap<String, HashMap<String, String>>,
    bytes: usize,
}

pub(super) static SOURCE_VARS: Lazy<Mutex<SourceVarStore>> =
    Lazy::new(|| Mutex::new(SourceVarStore::default()));

pub(super) fn js_source_var_get(source_key: &str, key: &str) -> String {
    let store = SOURCE_VARS.lock().unwrap_or_else(|e| e.into_inner());
    store
        .buckets
        .get(source_key.trim())
        .and_then(|b| b.get(key))
        .cloned()
        .unwrap_or_default()
}

pub(super) fn js_source_var_put(
    source_key: &str,
    key: &str,
    value: &str,
) -> Result<String, String> {
    if key.is_empty() {
        return Err("source.put: key 不能为空".into());
    }
    if value.len() > SOURCE_VAR_MAX_VALUE_BYTES {
        return Err("source.put: 单个值超过 256KB 上限".into());
    }
    let sk = source_key.trim();
    let mut store = SOURCE_VARS.lock().unwrap_or_else(|e| e.into_inner());
    let prev_bytes = store
        .buckets
        .get(sk)
        .and_then(|b| b.get(key))
        .map(|old| old.len() + key.len())
        .unwrap_or(0);
    let add = value.len() + key.len();
    let total = store.bytes - prev_bytes + add;
    if total > SOURCE_VAR_MAX_TOTAL_BYTES {
        return Err("source.put: 书源变量总量超过 8MB 上限".into());
    }
    store
        .buckets
        .entry(sk.to_string())
        .or_default()
        .insert(key.to_string(), value.to_string());
    store.bytes = total;
    Ok(value.to_string())
}

pub(super) fn js_conf() -> JsConf {
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
pub(super) struct JsJob {
    pub(super) script: String,
    /// (求值结果, java.ajax 的网络耗时毫秒, 实际脚本执行耗时毫秒)
    /// 执行耗时由 worker 自己计时，不包含等待单一 JS 线程排队的墙钟时间。
    pub(super) reply: Sender<(Result<String, String>, u64, u64)>,
}

/// 全局唯一的 JS 执行线程入口（容量 8，避免无界排队）。
pub(super) static JS_TX: OnceLock<SyncSender<JsJob>> = OnceLock::new();

/// JS 求值在途上限：1 个在跑 + 最多 2 个排队。慢规则占满这 3 个名额后，
/// 新来的规则最多只等 `JS_ADMIT_WAIT` 就降级失败，不会排十几秒队。
/// 上限取 3 的依据：`block_in_place` 的替身 worker 会占用 blocking 池线程
/// （`max_blocking_threads = 4`），3 个在途留 1 个给图片解码等其它阻塞任务；
/// 再放大会耗尽 blocking 池，被移交的 worker 核心反而无处执行。
#[cfg(not(test))]
pub(super) const JS_MAX_INFLIGHT: usize = 3;
/// 单测里 400+ 用例并行跑、共用同一条 JS 线程，按生产上限会互相抢名额并触发降级；
/// 测试环境下放宽，生产语义由 `js_gate_fast_fails_when_saturated` 直接覆盖。
#[cfg(test)]
pub(super) const JS_MAX_INFLIGHT: usize = 64;
/// 拿不到 JS 在途名额时的最长等待；超过即返回可读错误（调用方按空字段降级）。
pub(super) const JS_ADMIT_WAIT: Duration = Duration::from_millis(250);

/// 投递 JS 任务并等待回执（含在途名额限制）。
///
/// 规则解析整条调用链是同步的（`AnalyzeRule` 内含 `RefCell`，既不能 `await`
/// 也不满足 `Send + 'static`，无法整体搬进 `spawn_blocking`）。因此这里在
/// tokio 多线程 runtime 上用 `block_in_place`：它会把当前 worker 的调度核心
/// 迁移到 blocking 线程并从 blocking 池补一个执行者，本线程只做等待，
/// **不再占住 worker**，慢 JS 也就不会把 2 个 worker 一起拖死。
///
/// 单线程 runtime（`current_thread`）与纯同步上下文（单测/CLI）没有该能力，
/// 退回直接阻塞，行为与原实现一致。
///
/// 名额限制让最坏情况有界：并发慢规则超过 `JS_MAX_INFLIGHT` 时只等
/// `JS_ADMIT_WAIT`，随后按“规则降级”返回错误，而不是排队等前一条慢规则跑完。
pub(super) fn run_js_job(
    job: JsJob,
    rx: Receiver<(Result<String, String>, u64, u64)>,
) -> Result<(Result<String, String>, u64, u64), RuleError> {
    let work = || {
        let _slot = js_gate()
            .acquire(JS_MAX_INFLIGHT, JS_ADMIT_WAIT)
            .ok_or_else(|| {
                RuleError::Other(format!(
                    "js 执行队列繁忙（在途上限 {JS_MAX_INFLIGHT}），本次规则降级"
                ))
            })?;
        js_sender()
            .send(job)
            .map_err(|_| RuleError::Other("js worker unavailable".into()))?;
        rx.recv()
            .map_err(|_| RuleError::Other("js worker dropped".into()))
    };
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

pub(super) fn js_sender() -> &'static SyncSender<JsJob> {
    JS_TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<JsJob>(8);
        // 开发档（依赖 opt-level=2、本 crate 未优化）下 boa 解析/执行需要更多栈：
        // 1 MiB 会让 req::tests::source_header_js_is_evaluated 栈溢出（STATUS_STACK_OVERFLOW）。
        // release 档保持 1 MiB，真机内存红线不变。
        let stack_size = if cfg!(debug_assertions) {
            4 * 1024 * 1024
        } else {
            1024 * 1024
        };
        std::thread::Builder::new()
            .name("boa-js".into())
            .stack_size(stack_size)
            .spawn(move || js_worker(rx))
            .expect("spawn boa js worker");
        tx
    })
}

/// JS 在途名额（有界排队 + 超限快速失败）。
pub(super) struct JsGate {
    inflight: Mutex<usize>,
    ready: Condvar,
}

impl JsGate {
    pub(super) fn new() -> Self {
        Self {
            inflight: Mutex::new(0),
            ready: Condvar::new(),
        }
    }

    /// 最多等 `wait` 拿到名额；满额超时返回 `None`。
    pub(super) fn acquire(&self, limit: usize, wait: Duration) -> Option<JsSlot<'_>> {
        let mut n = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        if *n >= limit {
            let deadline = Instant::now() + wait;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return None;
                }
                let (guard, timed) = self
                    .ready
                    .wait_timeout(n, left)
                    .unwrap_or_else(|e| e.into_inner());
                n = guard;
                if *n < limit {
                    break;
                }
                if timed.timed_out() {
                    return None;
                }
            }
        }
        *n += 1;
        Some(JsSlot(self))
    }
}

/// 名额守卫：离开作用域即归还并唤醒一个等待者。
pub(super) struct JsSlot<'a>(&'a JsGate);

impl Drop for JsSlot<'_> {
    fn drop(&mut self) {
        let mut n = self.0.inflight.lock().unwrap_or_else(|e| e.into_inner());
        *n = n.saturating_sub(1);
        self.0.ready.notify_one();
    }
}

pub(super) static JS_GATE: OnceLock<JsGate> = OnceLock::new();

pub(super) fn js_gate() -> &'static JsGate {
    JS_GATE.get_or_init(JsGate::new)
}

/// 创建 JS 执行上下文（含预算配置与 `java.ajax` 网络桥）。
pub(super) fn new_js_context(conf: JsConf) -> boa_engine::Context {
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
    // jsLib / `java.importScript` 的脚本文本下载桥（进程内 TTL 缓存，见 native_import_script）。
    let _ = ctx.register_global_callable(
        boa_engine::JsString::from("__nativeImportScript"),
        0,
        boa_engine::NativeFunction::from_fn_ptr(native_import_script),
    );
    // java.setContent 之后的 DOM 选择器求值桥（惰性解析，见 native_select）。
    let _ = ctx.register_global_callable(
        boa_engine::JsString::from("__nativeSelect"),
        0,
        boa_engine::NativeFunction::from_fn_ptr(native_select),
    );
    // org.jsoup JS-Java 桥（scraper 后端）+ AES/字节桥。
    type JsNativeFn = fn(
        &boa_engine::JsValue,
        &[boa_engine::JsValue],
        &mut boa_engine::Context,
    ) -> boa_engine::JsResult<boa_engine::JsValue>;
    for (name, f) in [
        ("__jsoupParse", native_jsoup_parse as JsNativeFn),
        ("__jsoupSelect", native_jsoup_select as JsNativeFn),
        ("__jsoupAttr", native_jsoup_attr as JsNativeFn),
        ("__jsoupText", native_jsoup_text as JsNativeFn),
        ("__jsoupHtml", native_jsoup_html as JsNativeFn),
        ("__jsoupOuterHtml", native_jsoup_outer_html as JsNativeFn),
        ("__jsoupHasAttr", native_jsoup_has_attr as JsNativeFn),
        ("__jsoupParent", native_jsoup_parent as JsNativeFn),
        (
            "__jsoupNextSiblingElements",
            native_jsoup_next_siblings as JsNativeFn,
        ),
        ("__jsoupIs", native_jsoup_is as JsNativeFn),
        ("__jsoupTag", native_jsoup_tag as JsNativeFn),
        (
            "__nativeAesDecodeToString",
            native_aes_decode_to_string as JsNativeFn,
        ),
        ("__nativeSymmetric", native_aes_symmetric as JsNativeFn),
        ("__nativeBytesToStr", native_bytes_to_str as JsNativeFn),
        ("__nativeCookieGet", native_cookie_get as JsNativeFn),
        ("__nativeCookieSet", native_cookie_set as JsNativeFn),
        ("__nativeCookieRemove", native_cookie_remove as JsNativeFn),
        ("__nativeCookieClear", native_cookie_clear as JsNativeFn),
        ("__nativeSourceVarGet", native_source_var_get as JsNativeFn),
        ("__nativeSourceVarPut", native_source_var_put as JsNativeFn),
        ("__nativeHead", native_head as JsNativeFn),
        ("__nativeCacheFileGet", native_cache_file_get as JsNativeFn),
        ("__nativeCacheFilePut", native_cache_file_put as JsNativeFn),
    ] {
        let _ = ctx.register_global_callable(
            boa_engine::JsString::from(name),
            0,
            boa_engine::NativeFunction::from_fn_ptr(f),
        );
    }
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
pub(super) fn js_worker(rx: Receiver<JsJob>) {
    let conf = js_conf();
    let mut ctx = new_js_context(conf);
    while let Ok(job) = rx.recv() {
        JS_AJAX_NET_MS.store(0, Ordering::Relaxed);
        jsoup_clear_registry();
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

pub(super) fn cookie_args(args: &[boa_engine::JsValue]) -> Vec<String> {
    args.iter()
        .map(|v| {
            v.as_string()
                .map(|s| s.to_std_string_escaped())
                .unwrap_or_default()
        })
        .collect()
}

/// `cookie.getCookie(url)`：读当前书源的进程内 Cookie 桶（无值空串）。
pub(super) fn native_cookie_get(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let a = cookie_args(args);
    let s = crate::req::js_cookie_get(
        a.first().map(String::as_str).unwrap_or(""),
        a.get(1).map(String::as_str).unwrap_or(""),
    );
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(s)))
}

/// `cookie.setCookie(url, value)`：写入当前书源桶；返回空串。
pub(super) fn native_cookie_set(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let a = cookie_args(args);
    crate::req::js_cookie_set(
        a.first().map(String::as_str).unwrap_or(""),
        a.get(1).map(String::as_str).unwrap_or(""),
        a.get(2).map(String::as_str).unwrap_or(""),
    );
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from("")))
}

/// `cookie.removeCookie(url|domain)`：清理该源/该域桶；返回空串。
pub(super) fn native_cookie_remove(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let a = cookie_args(args);
    crate::req::js_cookie_remove(
        a.first().map(String::as_str).unwrap_or(""),
        a.get(1).map(String::as_str).unwrap_or(""),
    );
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from("")))
}

/// `cookie.clear()`：只清当前书源桶；返回空串。
pub(super) fn native_cookie_clear(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let a = cookie_args(args);
    crate::req::js_cookie_clear(a.first().map(String::as_str).unwrap_or(""));
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from("")))
}

/// `source.get(key)`：按当前书源读进程内变量；无值返回空串（与 Legado 一致）。
pub(super) fn native_source_var_get(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let a = cookie_args(args);
    let s = js_source_var_get(
        a.first().map(String::as_str).unwrap_or(""),
        a.get(1).map(String::as_str).unwrap_or(""),
    );
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(s)))
}

/// `source.put(key, value)`：按当前书源写进程内变量，返回写入的字符串值（与 Legado 一致）。
pub(super) fn native_source_var_put(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let a = cookie_args(args);
    match js_source_var_put(
        a.first().map(String::as_str).unwrap_or(""),
        a.get(1).map(String::as_str).unwrap_or(""),
        a.get(2).map(String::as_str).unwrap_or(""),
    ) {
        Ok(v) => Ok(boa_engine::JsValue::from(boa_engine::JsString::from(v))),
        Err(msg) => Err(ajax_error(&msg)),
    }
}

/// `cache.getFile(key)`：读落盘缓存；缺失返回空串，非法 key/IO 失败显式报错。
pub(super) fn native_cache_file_get(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let a = cookie_args(args);
    let key = a.first().map(String::as_str).unwrap_or("");
    match js_cache_file_get(key) {
        Ok(s) => Ok(boa_engine::JsValue::from(boa_engine::JsString::from(s))),
        Err(msg) => Err(ajax_error(&msg)),
    }
}

/// `cache.putFile(key, value[, saveTime])`：写落盘缓存（saveTime 秒，0 表示不设过期前缀）。
pub(super) fn native_cache_file_put(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let a = cookie_args(args);
    let save_time = a
        .get(2)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|n| n as i64)
        .unwrap_or(0);
    match js_cache_file_put(
        a.first().map(String::as_str).unwrap_or(""),
        a.get(1).map(String::as_str).unwrap_or(""),
        save_time,
    ) {
        Ok(()) => Ok(boa_engine::JsValue::from(boa_engine::JsString::from(""))),
        Err(msg) => Err(ajax_error(&msg)),
    }
}

/// `java.ajax(url[, method[, body[, headersJson]]])` 的原生实现。
/// 同步阻塞在 JS 专用线程上，失败以带 URL 的可读文案抛回 JS。
pub(super) fn native_ajax(
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

/// `java.head(url[, headersJson])` 的原生实现：不读正文，返回
/// `{status, url, headers}` 供 JS 构造与 Legado StrResponse 同形的响应对象。
pub(super) fn native_head(
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
        return Err(ajax_error("java.head 缺少 url 参数"));
    }
    let headers: HashMap<String, String> = args
        .get(1)
        .and_then(|v| v.as_string())
        .and_then(|s| serde_json::from_str(&s.to_std_string_escaped()).ok())
        .unwrap_or_default();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ajax_error(&format!("java.head 初始化运行时失败：{e}")))?;
    let res = rt.block_on(crate::req::head(&url, &headers));
    match res {
        Ok(h) => {
            let headers: serde_json::Map<String, serde_json::Value> = h
                .headers
                .into_iter()
                .map(|(k, v)| (k, serde_json::Value::String(v)))
                .collect();
            let payload = serde_json::json!({
                "status": h.status,
                "url": h.url,
                "headers": headers,
            });
            Ok(boa_engine::JsValue::from(boa_engine::JsString::from(
                payload.to_string(),
            )))
        }
        Err(e) => Err(ajax_error(&format!("java.head 请求失败：{e}"))),
    }
}

pub(super) fn ajax_error(msg: &str) -> boa_engine::JsError {
    boa_engine::JsError::from_native(
        boa_engine::JsNativeError::error().with_message(msg.to_string()),
    )
}

/// 脚本类资源（jsLib / `java.importScript` 的子规则脚本）的进程内 TTL 缓存。
///
/// Reload 在同一个源的详情/目录/正文里会被反复调用，每次都重新下载会让源作者的第
/// 三方脚本站承压，也让单次规则变慢；脚本内容本身不随请求变化，600s 足够。
pub(super) static JS_SCRIPT_CACHE: OnceLock<std::sync::Mutex<HashMap<String, (Instant, String)>>> =
    OnceLock::new();
pub(super) const JS_SCRIPT_TTL_SECS: u64 = 600;

pub(super) fn script_cache() -> &'static std::sync::Mutex<HashMap<String, (Instant, String)>> {
    JS_SCRIPT_CACHE.get_or_init(Default::default)
}

pub(super) fn cached_script(url: &str) -> Option<String> {
    let guard = script_cache().lock().ok()?;
    guard
        .get(url)
        .and_then(|(at, text)| (at.elapsed().as_secs() < JS_SCRIPT_TTL_SECS).then(|| text.clone()))
}

pub(super) fn store_script(url: &str, text: &str) {
    if let Ok(mut guard) = script_cache().lock() {
        guard.insert(url.to_string(), (Instant::now(), text.to_string()));
    }
}

/// `java.importScript` 的地址形态判定。Legado `p0.i0` 还支持 file/content/storage
/// （App 私有目录），服务端放开会变成“书源可读部署机任意路径”，故只允许 http(s)。
pub(super) fn import_script_supported(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// 取脚本文本：先查 TTL 缓存，未命中再走 `fetch_checked`（复用超时/体积/私网策略）。
pub(super) fn import_script(path: &str) -> Result<String, String> {
    if let Some(text) = cached_script(path) {
        return Ok(text);
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("初始化运行时失败：{e}"))?;
    let text = rt
        .block_on(crate::req::fetch_checked(
            path,
            "GET",
            &HashMap::new(),
            None,
            false,
        ))
        .map_err(|e| e.to_string())?;
    store_script(path, &text);
    Ok(text)
}

/// `java.importScript(path)` 的原生实现。空内容与非 http(s) 形态都以可读文案抛回 JS，
/// 不把 `SyntaxError`/底层解码错误泄漏给规则作者。
pub(super) fn native_import_script(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let path = args
        .first()
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    let path = path.trim();
    if path.is_empty() {
        return Err(ajax_error("java.importScript：缺少脚本地址"));
    }
    if !import_script_supported(path) {
        return Err(ajax_error(&format!(
            "UNSUPPORTED_JS_API:java.importScript（仅支持 http(s) 脚本地址，当前为 {path}）"
        )));
    }
    let started = Instant::now();
    let res = import_script(path);
    JS_AJAX_NET_MS.fetch_add(started.elapsed().as_millis() as u64, Ordering::Relaxed);
    match res {
        Ok(text) if !text.trim().is_empty() => {
            Ok(boa_engine::JsValue::from(boa_engine::JsString::from(text)))
        }
        Ok(_) => Err(ajax_error(&format!(
            "java.importScript：{path} 内容获取失败或者为空"
        ))),
        Err(e) => {
            tracing::warn!(url = path, error = %e, "importScript fetch failed");
            Err(ajax_error(&format!(
                "java.importScript 请求失败：{path}（{e}）"
            )))
        }
    }
}

/// `java.setContent` 之后 DOM 三件套的按需求值（`__nativeSelect(content, rule, wantList)`）。
///
/// 上游的 `java.setContent` 把内容设到同一个 `AnalyzeRule` 上，后续 `getElements` 在该
/// 文档上按当前模式（HTML/CSS 或 JSON/JSONPath）求值；本服务把这一步下沉为原生函数，
/// 惰性解析，不再依赖 eval 前的字面量选择器预抽取。
///
/// 只允许“纯选择器/JSONPath”规则：含 `@js:`、`<js>`、`{{…}}`、`@put/@get` 的规则会在
/// 同一 JS 线程上再次排队，必然死锁，直接返回具名未支持错误。
pub(super) fn native_select(
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
pub(super) const UNSUPPORTED_JS_API: &str = "UNSUPPORTED_JS_API:";

// ============================ org.jsoup JS-Java 桥 ============================
//
// 书源里的 `org.jsoup.Jsoup.parse(...)` 在 App 里是 Rhino 直接调用 jsoup 的 Java API。
// 本服务用 scraper(html5ever) 做 HTML5 解析与 CSS 匹配，按“节点预序编号”做句柄：
// 每次 eval 前清空注册表，句柄只在同一次 JS 求值内有效（与 App 的对象生命周期一致）。

pub(super) const JSOUP_MAX_DOCS: usize = 8;

pub(super) struct JsoupDoc {
    html: Html,
    base: String,
}

#[derive(Default)]
pub(super) struct JsoupRegistry {
    docs: Vec<JsoupDoc>,
    /// (doc 下标, 节点在文档中的先序序号)；序号 0 表示文档根（Document）。
    handles: Vec<(usize, usize)>,
}

thread_local! {
    static JSOUP_REG: RefCell<JsoupRegistry> = RefCell::new(JsoupRegistry::default());
}

/// 每次 JS 任务开始前清空，避免句柄/文档跨任务泄漏。
pub(crate) fn jsoup_clear_registry() {
    JSOUP_REG.with(|r| {
        let mut r = r.borrow_mut();
        r.docs.clear();
        r.handles.clear();
    });
}

pub(super) fn jsoup_arg_str(args: &[boa_engine::JsValue], i: usize) -> String {
    args.get(i)
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default()
}

pub(super) fn jsoup_arg_handle(args: &[boa_engine::JsValue], i: usize) -> Option<u32> {
    let n = args.get(i)?.as_number()?;
    if !n.is_finite() || n <= 0.0 {
        return None;
    }
    Some(n as u32)
}

pub(super) fn jsoup_preorder_index(doc: &Html, el: &ElementRef<'_>) -> usize {
    let target = el.id();
    doc.tree
        .root()
        .descendants()
        .enumerate()
        .find(|(_, n)| n.id() == target)
        .map(|(i, _)| i)
        .unwrap_or(0)
}

pub(super) fn jsoup_element_at(doc: &Html, index: usize) -> Option<ElementRef<'_>> {
    doc.tree
        .root()
        .descendants()
        .nth(index)
        .and_then(ElementRef::wrap)
}

pub(super) fn jsoup_add_handle(doc: usize, node: usize) -> u32 {
    JSOUP_REG.with(|r| {
        let mut r = r.borrow_mut();
        r.handles.push((doc, node));
        r.handles.len() as u32
    })
}

/// 解析 jsoup 选择器；jsoup 专有扩展伪类由 Rust 侧统一处理，`:matches`/`:nth`
/// 这类尚未实现的形态显式报“未支持”，不静默降级。
pub(super) struct JsoupPlan {
    base: Selector,
    filters: Vec<JqFilter>,
}

pub(super) fn jsoup_plan(text: &str) -> Result<JsoupPlan, String> {
    let s = text.trim();
    if s.is_empty() {
        return Err("空选择器".to_string());
    }
    if s.contains(":nth(") {
        return Err(format!(
            "选择器使用了 jsoup 专有扩展伪类 :nth(…，当前后端(scraper)不支持：{s}"
        ));
    }
    let (base, filters) = split_jquery_filters(s);
    let base = if base.is_empty() {
        "*".to_string()
    } else {
        base
    };
    let base = Selector::parse(&base).map_err(|e| format!("CSS 选择器不合法：{s}（{e:?}）"))?;
    Ok(JsoupPlan { base, filters })
}

pub(super) fn jsoup_with_handle<R>(h: u32, f: impl FnOnce(&Html, &str, usize) -> R) -> Option<R> {
    JSOUP_REG.with(|r| {
        let r = r.borrow();
        let (doc, node) = *r.handles.get((h as usize).checked_sub(1)?)?;
        let d = r.docs.get(doc)?;
        Some(f(&d.html, &d.base, node))
    })
}

pub(super) fn jsoup_select_handles(h: u32, plan: &JsoupPlan) -> Vec<u32> {
    let (doc_idx, nodes) = JSOUP_REG.with(|r| {
        let r = r.borrow();
        let Some((doc, node)) = (h as usize)
            .checked_sub(1)
            .and_then(|i| r.handles.get(i))
            .copied()
        else {
            return (usize::MAX, Vec::new());
        };
        let Some(d) = r.docs.get(doc) else {
            return (usize::MAX, Vec::new());
        };
        let mut out = Vec::new();
        if node == 0 {
            let els: Vec<_> = d.html.select(&plan.base).collect();
            for el in apply_jq_filters(els, &plan.filters) {
                out.push(jsoup_preorder_index(&d.html, &el));
            }
        } else if let Some(el) = jsoup_element_at(&d.html, node) {
            let els: Vec<_> = el.select(&plan.base).collect();
            for e in apply_jq_filters(els, &plan.filters) {
                out.push(jsoup_preorder_index(&d.html, &e));
            }
        }
        (doc, out)
    });
    if doc_idx == usize::MAX {
        return Vec::new();
    }
    nodes
        .into_iter()
        .map(|n| jsoup_add_handle(doc_idx, n))
        .collect()
}

pub(super) fn jsoup_error(msg: &str) -> boa_engine::JsError {
    ajax_error(&format!("{UNSUPPORTED_JS_API}{msg}"))
}

pub(super) fn native_jsoup_parse(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let html = jsoup_arg_str(args, 0);
    let base = jsoup_arg_str(args, 1);
    let doc = Html::parse_document(&html);
    let doc_idx = JSOUP_REG.with(|r| {
        let mut r = r.borrow_mut();
        if r.docs.len() >= JSOUP_MAX_DOCS {
            // 超过上限直接整体重置：句柄只在同一次求值内有效，重置是安全的上界保护。
            r.docs.clear();
            r.handles.clear();
        }
        r.docs.push(JsoupDoc { html: doc, base });
        r.docs.len() - 1
    });
    let h = jsoup_add_handle(doc_idx, 0);
    Ok(boa_engine::JsValue::from(h as f64))
}

pub(super) fn native_jsoup_select(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.select（无效的文档/元素句柄）"))?;
    let sel_text = jsoup_arg_str(args, 1);
    let plan = jsoup_plan(&sel_text).map_err(|e| jsoup_error(&format!("org.jsoup select：{e}")))?;
    let handles = jsoup_select_handles(h, &plan);
    let json = serde_json::to_string(&handles).unwrap_or_else(|_| "[]".into());
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(json)))
}

pub(super) fn native_jsoup_attr(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.attr（无效的文档/元素句柄）"))?;
    let key = jsoup_arg_str(args, 1);
    let v = jsoup_with_handle(h, |html, base, node| {
        let el = jsoup_element_at(html, node)?;
        // jsoup 的 `abs:<attr>` 前缀：先取真实属性，再按文档 baseUri 解析相对地址。
        // `abs:`（无属性名）等价于 `abs:href`。
        let (name, absolute) = match key.strip_prefix("abs:") {
            Some("") => ("href", true),
            Some(rest) => (rest, true),
            None => (key.as_str(), false),
        };
        let v = el.value().attr(name).unwrap_or("");
        let out = if absolute {
            if v.is_empty() {
                String::new()
            } else {
                let base = if base.is_empty() {
                    el.value().attr("base").unwrap_or("")
                } else {
                    base
                };
                url::Url::parse(base)
                    .ok()
                    .and_then(|b| b.join(v).ok())
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| v.to_string())
            }
        } else {
            v.to_string()
        };
        Some(out)
    });
    match v {
        Some(Some(s)) => Ok(boa_engine::JsValue::from(boa_engine::JsString::from(s))),
        _ => Ok(boa_engine::JsValue::from(boa_engine::JsString::from(""))),
    }
}

pub(super) fn native_jsoup_text(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.text（无效的文档/元素句柄）"))?;
    let out = jsoup_with_handle(h, |html, _base, node| {
        let el = jsoup_element_at(html, node)?;
        Some(jsoup_normalized_text(&el))
    })
    .flatten()
    .unwrap_or_default();
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(out)))
}

pub(super) fn native_jsoup_html(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.html（无效的文档/元素句柄）"))?;
    let out = jsoup_with_handle(h, |html, _base, node| {
        let el = jsoup_element_at(html, node)?;
        Some(el.inner_html())
    })
    .flatten()
    .unwrap_or_default();
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(out)))
}

pub(super) fn native_jsoup_outer_html(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.outerHtml（无效的文档/元素句柄）"))?;
    let out = jsoup_with_handle(h, |html, _base, node| {
        let el = jsoup_element_at(html, node)?;
        Some(el.html())
    })
    .flatten()
    .unwrap_or_default();
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(out)))
}

/// jsoup `text()`：拼接文本节点后把连续空白折叠为单个空格并去首尾（含块级换行近似）。
pub(super) fn jsoup_normalized_text(el: &ElementRef<'_>) -> String {
    let raw: String = el.text().collect();
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn native_jsoup_has_attr(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.hasAttr（无效的文档/元素句柄）"))?;
    let key = jsoup_arg_str(args, 1);
    let has = jsoup_with_handle(h, |html, _base, node| {
        jsoup_element_at(html, node).map(|el| el.value().attr(&key).is_some())
    })
    .flatten()
    .unwrap_or(false);
    Ok(boa_engine::JsValue::from(has))
}

pub(super) fn native_jsoup_parent(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.parent（无效的文档/元素句柄）"))?;
    let parent = JSOUP_REG.with(|r| {
        let r = r.borrow();
        let (doc, node) = *r.handles.get((h as usize).checked_sub(1)?)?;
        let html = &r.docs.get(doc)?.html;
        let parent = jsoup_element_at(html, node)?
            .parent()
            .and_then(ElementRef::wrap)?;
        Some((doc, jsoup_preorder_index(html, &parent)))
    });
    match parent {
        Some((doc, node)) => Ok(boa_engine::JsValue::from(jsoup_add_handle(doc, node) as f64)),
        None => Ok(boa_engine::JsValue::undefined()),
    }
}

pub(super) fn native_jsoup_next_siblings(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.nextSiblingElements（无效句柄）"))?;
    let (doc_idx, nodes) = JSOUP_REG.with(|r| {
        let r = r.borrow();
        let Some((d, node)) = (h as usize)
            .checked_sub(1)
            .and_then(|i| r.handles.get(i))
            .copied()
        else {
            return (usize::MAX, Vec::new());
        };
        let mut out = Vec::new();
        if let Some(el) = jsoup_element_at(&r.docs[d].html, node) {
            for e in el.next_siblings().filter_map(ElementRef::wrap) {
                out.push(jsoup_preorder_index(&r.docs[d].html, &e));
            }
        }
        (d, out)
    });
    let handles: Vec<u32> = nodes
        .into_iter()
        .map(|n| jsoup_add_handle(doc_idx, n))
        .collect();
    let json = serde_json::to_string(&handles).unwrap_or_else(|_| "[]".into());
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(json)))
}

pub(super) fn native_jsoup_is(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.is（无效的文档/元素句柄）"))?;
    let sel_text = jsoup_arg_str(args, 1);
    let plan = jsoup_plan(&sel_text).map_err(|e| jsoup_error(&format!("org.jsoup is：{e}")))?;
    let matched = jsoup_with_handle(h, |html, _base, node| {
        jsoup_element_at(html, node).map(|el| {
            plan.base.matches(&el) && !apply_jq_filters(vec![el], &plan.filters).is_empty()
        })
    })
    .flatten()
    .unwrap_or(false);
    Ok(boa_engine::JsValue::from(matched))
}

pub(super) fn native_jsoup_tag(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let h = jsoup_arg_handle(args, 0)
        .ok_or_else(|| jsoup_error("org.jsoup.tagName（无效的文档/元素句柄）"))?;
    let tag = jsoup_with_handle(h, |html, _base, node| {
        jsoup_element_at(html, node).map(|el| el.value().name().to_string())
    })
    .flatten()
    .unwrap_or_default();
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(tag)))
}

// ============================ AES / 字节桥 ============================

pub(super) fn native_aes_decode_to_string(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let data = jsoup_arg_str(args, 0);
    let key = parse_key_arg(&jsoup_arg_str(args, 1));
    let transformation = jsoup_arg_str(args, 2);
    let iv = parse_key_arg(&jsoup_arg_str(args, 3));
    let out = crate::crypto::decrypt_base64_to_string(&data, &key, &transformation, &iv)
        .map_err(|e| jsoup_error(&format!("java.aesBase64DecodeToString：{e}")))?;
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(out)))
}

/// key/iv 参数：JS 字节数组走 JSON（`[..]`），字符串按 Legado `String.toByteArray()`（UTF-8）。
pub(super) fn parse_key_arg(s: &str) -> Vec<u8> {
    if s.trim_start().starts_with('[') {
        parse_bytes_arg(s)
    } else {
        s.as_bytes().to_vec()
    }
}

pub(super) fn parse_bytes_arg(s: &str) -> Vec<u8> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if let Ok(v) = serde_json::from_str::<Vec<i64>>(trimmed) {
        return v.into_iter().map(|x| x as u8).collect();
    }
    // 兼容逗号分隔的十进制
    trimmed
        .split(',')
        .filter_map(|x| x.trim().parse::<i64>().ok().map(|v| v as u8))
        .collect()
}

pub(super) fn native_aes_symmetric(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let mode = jsoup_arg_str(args, 0);
    let transformation = jsoup_arg_str(args, 1);
    let key = parse_key_arg(&jsoup_arg_str(args, 2));
    let iv = parse_key_arg(&jsoup_arg_str(args, 3));
    let bytes = parse_bytes_arg(&jsoup_arg_str(args, 4));
    let out = if mode == "encrypt" {
        crate::crypto::encrypt_bytes(&bytes, &key, &transformation, &iv)
    } else {
        crate::crypto::decrypt_bytes(&bytes, &key, &transformation, &iv)
    }
    .map_err(|e| jsoup_error(&format!("java.createSymmetricCrypto：{e}")))?;
    let json = serde_json::to_string(&out).unwrap_or_else(|_| "[]".into());
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(json)))
}

pub(super) fn native_bytes_to_str(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let bytes = parse_bytes_arg(&jsoup_arg_str(args, 0));
    let s = String::from_utf8_lossy(&bytes).to_string();
    Ok(boa_engine::JsValue::from(boa_engine::JsString::from(s)))
}

/// 把 boa 的求值错误转成可读文案：
/// - 垫片主动抛出的未支持特性 → “该规则使用了未支持的 JS 特性：<名称>（<原因>）”；
/// - 其他语法/运行时错误 → 保留 JS 自己的 error 文本，便于书源作者定位。
pub(super) fn js_error_message(msg: &str) -> String {
    match msg.find(UNSUPPORTED_JS_API) {
        Some(pos) => {
            let detail = msg[pos + UNSUPPORTED_JS_API.len()..].trim();
            format!("该规则使用了未支持的 JS 特性：{detail}")
        }
        None => format!("js eval: {msg}"),
    }
}

/// 中文数字转阿拉伯数字（章节名用，支持 零~九、十百千万亿、两）。
pub(super) const CN_TO_NUM_JS: &str = r#"
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
pub(super) const JS_UTF8_HELPER: &str = r#"
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
pub(super) const JS_MD5: &str = r#"
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
pub(super) const JS_T2S: &str = r#"var __T2S = {"爱":"愛","罢":"罷","摆":"擺","败":"敗","办":"辦","帮":"幫","宝":"寶","报":"報","备":"備","贝":"貝","笔":"筆","毕":"畢","边":"邊","变":"變","标":"標","表":"錶","别":"彆","宾":"賓","补":"補","参":"參","惨":"慘","灿":"燦","仓":"倉","产":"產","长":"長","尝":"嘗","车":"車","彻":"徹","尘":"塵","陈":"陳","衬":"襯","称":"稱","惩":"懲","迟":"遲","冲":"衝","丑":"醜","础":"礎","处":"處","触":"觸","传":"傳","疮":"瘡","闯":"闖","创":"創","纯":"純","词":"詞","从":"從","丛":"叢","窜":"竄","达":"達","带":"帶","贷":"貸","担":"擔","单":"單","胆":"膽","当":"當","党":"黨","荡":"蕩","档":"檔","导":"導","敌":"敵","邓":"鄧","递":"遞","点":"點","电":"電","垫":"墊","钓":"釣","调":"調","叠":"疊","东":"東","动":"動","冻":"凍","斗":"鬥","独":"獨","读":"讀","断":"斷","对":"對","队":"隊","吨":"噸","夺":"奪","堕":"墮","儿":"兒","尔":"爾","发":"發","罚":"罰","阀":"閥","飞":"飛","费":"費","废":"廢","纷":"紛","坟":"墳","奋":"奮","粪":"糞","风":"風","丰":"豐","冯":"馮","凤":"鳳","肤":"膚","妇":"婦","复":"復","该":"該","干":"乾","赶":"趕","冈":"岡","刚":"剛","岗":"崗","纲":"綱","钢":"鋼","个":"個","给":"給","巩":"鞏","沟":"溝","构":"構","购":"購","谷":"穀","顾":"顧","刮":"颳","关":"關","观":"觀","馆":"館","惯":"慣","广":"廣","归":"歸","龟":"龜","规":"規","国":"國","过":"過","汉":"漢","号":"號","轰":"轟","后":"後","胡":"鬍","壶":"壺","沪":"滬","护":"護","划":"劃","怀":"懷","坏":"壞","欢":"歡","环":"環","还":"還","回":"迴","汇":"匯","会":"會","秽":"穢","荤":"葷","浑":"渾","伙":"夥","获":"獲","击":"擊","鸡":"雞","积":"積","极":"極","际":"際","继":"繼","家":"傢","价":"價","艰":"艱","歼":"殲","舰":"艦","姜":"薑","浆":"漿","桨":"槳","奖":"獎","讲":"講","酱":"醬","胶":"膠","浇":"澆","骄":"驕","娇":"嬌","搅":"攪","缴":"繳","觉":"覺","较":"較","阶":"階","节":"節","洁":"潔","结":"結","借":"藉","仅":"僅","惊":"驚","竞":"競","旧":"舊","剧":"劇","据":"據","惧":"懼","卷":"捲","决":"決","绝":"絕","军":"軍","开":"開","克":"剋","壳":"殼","夸":"誇","矿":"礦","亏":"虧","困":"睏","腊":"臘","蜡":"蠟","来":"來","赖":"賴","兰":"蘭","拦":"攔","栏":"欄","烂":"爛","垒":"壘","类":"類","里":"裡","礼":"禮","丽":"麗","厉":"厲","励":"勵","离":"離","历":"歷","帘":"簾","联":"聯","怜":"憐","炼":"煉","练":"練","粮":"糧","两":"兩","辆":"輛","疗":"療","辽":"遼","了":"瞭","猎":"獵","临":"臨","邻":"鄰","岭":"嶺","庐":"廬","芦":"蘆","炉":"爐","陆":"陸","虏":"虜","录":"錄","虑":"慮","乱":"亂","罗":"羅","萝":"蘿","络":"絡","妈":"媽","马":"馬","骂":"罵","买":"買","麦":"麥","卖":"賣","迈":"邁","脉":"脈","猫":"貓","么":"麼","门":"門","闷":"悶","梦":"夢","弥":"彌","觅":"覓","绵":"綿","面":"麵","庙":"廟","灭":"滅","亩":"畝","恼":"惱","脑":"腦","拟":"擬","酿":"釀","鸟":"鳥","聂":"聶","宁":"寧","农":"農","齐":"齊","岂":"豈","启":"啟","气":"氣","弃":"棄","牵":"牽","签":"簽","迁":"遷","窍":"竅","窃":"竊","亲":"親","轻":"輕","庆":"慶","琼":"瓊","穷":"窮","秋":"鞦","区":"區","驱":"驅","趋":"趨","权":"權","劝":"勸","确":"確","让":"讓","扰":"擾","热":"熱","认":"認","荣":"榮","绒":"絨","洒":"灑","伞":"傘","丧":"喪","扫":"掃","涩":"澀","杀":"殺","晒":"曬","纱":"紗","设":"設","绅":"紳","审":"審","声":"聲","胜":"勝","圣":"聖","师":"師","湿":"濕","时":"時","实":"實","适":"適","势":"勢","兽":"獸","书":"書","术":"術","树":"樹","帅":"帥","双":"雙","谁":"誰","顺":"順","说":"說","丝":"絲","松":"鬆","苏":"蘇","诉":"訴","虽":"雖","随":"隨","台":"臺","态":"態","摊":"攤","滩":"灘","坛":"壇","叹":"嘆","汤":"湯","烫":"燙","涛":"濤","体":"體","条":"條","铁":"鐵","听":"聽","厅":"廳","头":"頭","图":"圖","涂":"塗","团":"團","洼":"窪","袜":"襪","弯":"彎","万":"萬","网":"網","韦":"韋","违":"違","围":"圍","为":"為","卫":"衛","温":"溫","闻":"聞","问":"問","稳":"穩","务":"務","雾":"霧","牺":"犧","习":"習","系":"係","戏":"戲","细":"細","虾":"蝦","吓":"嚇","鲜":"鮮","纤":"纖","咸":"鹹","显":"顯","险":"險","现":"現","献":"獻","乡":"鄉","详":"詳","响":"響","向":"嚮","协":"協","挟":"挾","泻":"瀉","谢":"謝","兴":"興","须":"須","许":"許","悬":"懸","选":"選","学":"學","寻":"尋","压":"壓","亚":"亞","严":"嚴","盐":"鹽","阳":"陽","养":"養","样":"樣","痒":"癢","谣":"謠","药":"藥","爷":"爺","业":"業","叶":"葉","页":"頁","义":"義","议":"議","亿":"億","忆":"憶","艺":"藝","隐":"隱","应":"應","营":"營","拥":"擁","优":"優","忧":"憂","邮":"郵","余":"餘","鱼":"魚","与":"與","屿":"嶼","语":"語","吁":"籲","郁":"鬱","誉":"譽","渊":"淵","园":"園","员":"員","圆":"圓","缘":"緣","远":"遠","愿":"願","跃":"躍","云":"雲","杂":"雜","灾":"災","赞":"讚","赃":"贓","脏":"臟","凿":"鑿","枣":"棗","灶":"竈","责":"責","择":"擇","则":"則","泽":"澤","贼":"賊","赠":"贈","扎":"紮","张":"張","帐":"帳","战":"戰","这":"這","针":"針","征":"徵","争":"爭","挣":"掙","症":"癥","证":"證","只":"隻","帜":"幟","质":"質","种":"種","众":"眾","昼":"晝","烛":"燭","筑":"築","庄":"莊","装":"裝","妆":"妝","壮":"壯","状":"狀","准":"準","浊":"濁","总":"總","钻":"鑽","纵":"縱","邹":"鄒","诅":"詛","组":"組","话":"話","记":"記","写":"寫","诗":"詩","请":"請","难":"難","乐":"樂","进":"進","岁":"歲","间":"間","见":"見","龙":"龍","颜":"顏","恋":"戀","泪":"淚","华":"華","画":"畫"};"#;

/// `cache` 全局（Legado: 规则语义 `bindings["cache"] = CacheManager`）。
/// 上游 CacheManager 的方法名是 `get / put / delete`（另有 getFromMemory / putMemory /
/// deleteMemory，CacheManager.kt:60-160），**没有** set/remove/clear。
/// 本实现是每作用域一份的字符串 map：`get` 未命中返回 null（同上游返回 String?），
/// put 的对象/数组按 JSON 串存储（上游存 Any）。
pub(super) const JS_CACHE: &str = r#"
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
  deleteMemory: function(k){ cache.delete(k); },
  // Legado `CacheManager.getFile/putFile`：key 只当不透明标识（服务端把它 md5 成文件名），
  // 不允许用它直接拼路径；落盘目录固定为部署侧 `<data_dir>/cache/rule`。
  getFile: function(k){ return __nativeCacheFileGet(k == null ? "" : String(k)); },
  putFile: function(k, v, saveTime){
    return __nativeCacheFilePut(
      k == null ? "" : String(k),
      String(v),
      saveTime == null ? 0 : Math.trunc(Number(saveTime))
    );
  }
};
"#;

/// JS 执行结果里回传 cache 变更的分隔符（结果串 + 分隔符 + cache JSON）。
pub(super) const JS_CACHE_SENTINEL: &str = "\u{1}F1CACHE\u{1}";

/// 结果串尾部的“规则自身耗时”（毫秒）：预算只该算规则代码，不该算固定垫片/lib 的解析。
pub(super) const JS_RULE_MS_SENTINEL: &str = "\u{1}F1RULEMS\u{1}";

/// 结果串尾部的 `java.refreshTocUrl()` 标记（`1` 表示本次 JS 请求刷新目录地址）。
pub(super) const JS_REFRESH_TOC_SENTINEL: &str = "\u{1}F1TOC\u{1}";
/// 结果串尾部回传的 `book.bookUrl/book.tocUrl` 变更（`pre_update_js` 可能改写）。
pub(super) const JS_BOOK_SENTINEL: &str = "\u{1}F1BOOK\u{1}";
/// 结果串尾部回传的 JS 变量表（`java.put`/`book.putVariable` 等写入）。
pub(super) const JS_VARS_SENTINEL: &str = "\u{1}F1VARS\u{1}";

pub(super) const JAVA_BOOK_SHIM: &str = r#"
var __vars = {};
// DOM 桥 Phase 1 的预抽取结果（由 Rust 在 eval 前按字面量选择器算好并赋值）
var __DOM = {};
var __DOMS = {};
// java.setContent 设置的“当前文档”。null 表示未设置，仍走原始页面的预抽取表；
// 设置后 java.getElements/getString/getStringList 才按需解析这个文档（惰性求值）。
var __content = null;
var book = {
  bookUrl: __bookUrl,
  tocUrl: __bookTocUrl,
  canUpdate: true,
  name: __bookName,
  author: __bookAuthor,
  kind: __bookKind,
  intro: __bookIntro,
  coverUrl: __bookCoverUrl,
  getVariable: function(k){ return __vars[k] !== undefined ? __vars[k] : ""; },
  putVariable: function(k,v){ __vars[k] = String(v); }
};
// 书源 key（= bookSourceUrl）由 Rust 侧在每次求值前注入。`get/put` 对齐 Legado
// `BaseSource.get/put`：按书源分桶的字符串变量（CacheManager key `v_<sourceKey>_<key>`）。
// 本服务为进程内存储，不落盘；`put` 返回写入的字符串值，`get` 缺省返回空串。
var source = {
  key: __sourceKey,
  loginUrl: __sourceLoginUrl,
  getKey: function(){ return __sourceKey; },
  get: function(k){ return __nativeSourceVarGet(__sourceKey, k == null ? "" : String(k)); },
  put: function(k, v){
    var s = String(v);
    return __nativeSourceVarPut(__sourceKey, k == null ? "" : String(k), s);
  }
};
// Legado `cookie` 全局：落到 Rust 侧按书源分桶的进程内 Cookie 存储（req.rs COOKIE_JAR），
// 不新建全局 map；get 无值返回空串，set/remove/clear 返回空串（可直接拼进 {{ }} 模板）。
var cookie = {
  getCookie: function(url){ return __nativeCookieGet(__sourceKey, url == null ? "" : String(url)); },
  setCookie: function(url, value){
    __nativeCookieSet(__sourceKey, url == null ? "" : String(url), value == null ? "" : String(value));
    return "";
  },
  removeCookie: function(url){
    __nativeCookieRemove(__sourceKey, url == null ? "" : String(url));
    return "";
  },
  clear: function(){ __nativeCookieClear(__sourceKey); return ""; }
};
// Legado `java.connect` / `java.startBrowserAwait` 返回 StrResponse。服务端没有 WebView/人工验证，
// 这里用与 java.ajax 相同的原生抓取层发请求，并返回同形状的响应对象（body/url/code/header）。
// 抓取失败按 Legado 的容错方式把错误写进 body（调用方 `.body().includes(...)` 不会抛异常）。
function __httpResponse(url, header){
  var u = String(url == null ? "" : url);
  var h = header == null ? "" : (typeof header === "string" ? header : JSON.stringify(header));
  var body = "", code = 200;
  try {
    body = __nativeAjax(u, "GET", "", h);
  } catch (e) {
    body = "connect(" + u + ") error\n" + String(e && e.message ? e.message : e);
    code = 0;
  }
  return {
    body: function(){ return body; },
    getBody: function(){ return body; },
    url: function(){ return u; },
    getUrl: function(){ return u; },
    code: function(){ return code; },
    getCode: function(){ return code; },
    header: function(){ return ""; },
    headers: function(){ return ""; },
    toString: function(){ return body; }
  };
}
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
  // Legado `java.importScript(path)`：下载脚本文本供规则 `eval(String(...))`。本服务只接受
  // http(s)（进程内 TTL 缓存，见 __nativeImportScript）；file/content/storage 形态会读取
  // 部署机本地文件，等于把书源变成任意文件读取面，故显式报未支持而不是静默失败。
  importScript: function(path){
    return String(__nativeImportScript(path == null ? "" : String(path)));
  },
  // Legado `java.connect(url[, header])`：返回 StrResponse（body/url/code/header）。
  connect: function(url, header){ return __httpResponse(url, header); },
  // Legado `java.head(url[, header])`：HEAD 请求，返回带响应头的 StrResponse（不读正文）。
  // 书源 jsLib（如哔哩漫画 Reload）常用 `.header('etag')` 拿缓存指纹。
  head: function(url, header){
    var u = String(url == null ? "" : url);
    var h = header == null ? "" : (typeof header === "string" ? header : JSON.stringify(header));
    var r = JSON.parse(__nativeHead(u, h));
    var hs = r.headers || {};
    return {
      body: function(){ return ""; },
      getBody: function(){ return ""; },
      url: function(){ return r.url || u; },
      getUrl: function(){ return r.url || u; },
      code: function(){ return r.status || 0; },
      getCode: function(){ return r.status || 0; },
      header: function(n){
        var k = String(n == null ? "" : n).toLowerCase();
        for (var x in hs) { if (x.toLowerCase() === k) return String(hs[x]); }
        return "";
      },
      headers: function(){ return hs; },
      toString: function(){ return ""; }
    };
  },
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
  // Legado 的 java.getElement：从“当前页面”取第一个匹配元素（jsoup Element）。
  getElement: function(rule){
    var doc = new JsoupDocument(__jsoupParse(__page == null ? "" : String(__page), __base));
    return doc.selectFirst(rule == null ? "" : String(rule));
  },
  // Legado java.aesBase64DecodeToString(data, key, transformation, iv)
  aesBase64DecodeToString: function(data, key, transformation, iv){
    return __nativeAesDecodeToString(
      data == null ? "" : String(data),
      key == null ? "" : String(key),
      transformation == null ? "" : String(transformation),
      iv == null ? "" : String(iv));
  },
  // Legado java.createSymmetricCrypto(transformation, key, iv) -> {encrypt, decrypt}
  createSymmetricCrypto: function(transformation, key, iv){
    var tf = transformation == null ? "" : String(transformation);
    var k = __keyArg(key);
    var v = __keyArg(iv);
    return {
      encrypt: function(data){
        return JSON.parse(__nativeSymmetric("encrypt", tf, k, v, JSON.stringify(__toByteArray(data))));
      },
      decrypt: function(data){
        return JSON.parse(__nativeSymmetric("decrypt", tf, k, v, JSON.stringify(__toByteArray(data))));
      }
    };
  },
  // Legado java.bytesToStr(bytes) -> UTF-8 字符串
  bytesToStr: function(bytes){ return __nativeBytesToStr(JSON.stringify(__toByteArray(bytes))); },
  // Legado java.timeFormatUTC(time, format, offsetHours)
  timeFormatUTC: function(t, f, offset){
    if (typeof t === "string" && f === undefined) { f = t; t = undefined; }
    if (t === undefined || t === null || t === "") t = Date.now();
    var n = Number(t), d;
    if (!isNaN(n)) d = new Date(n < 100000000000 ? n * 1000 : n);
    else d = new Date(String(t));
    if (isNaN(d.getTime())) return "";
    if (offset !== undefined && offset !== null && offset !== "") {
      d = new Date(d.getTime() + Number(offset) * 3600 * 1000);
    }
    var p = function(x, l){ x = String(x); while (x.length < (l || 2)) x = "0" + x; return x; };
    var map = {
      yyyy: p(d.getUTCFullYear(), 4), MM: p(d.getUTCMonth() + 1), dd: p(d.getUTCDate()),
      HH: p(d.getUTCHours()), mm: p(d.getUTCMinutes()), ss: p(d.getUTCSeconds()),
      SSS: p(d.getUTCMilliseconds(), 3)
    };
    f = f || "yyyy-MM-dd HH:mm:ss";
    return String(f).replace(/yyyy|MM|dd|HH|mm|ss|SSS/g, function(k){ return map[k]; });
  },
  // Legado java.refreshTocUrl：标记“需要用 bookUrl 重新取目录”，由 toc 流程消费。
  refreshTocUrl: function(){ __refreshToc = true; },
  getCookie: function(){ throw new Error("UNSUPPORTED_JS_API:java.getCookie（本服务无 Cookie 桥接）"); },
  setCookie: function(){ throw new Error("UNSUPPORTED_JS_API:java.setCookie（本服务无 Cookie 桥接）"); },
  // Legado `java.startBrowserAwait(url, title[, refetchAfterSuccess])`：内置浏览器人工过验证后返回页面。
  // 服务端没有人机交互界面，这里直接抓取目标页并返回 StrResponse；遇到站点质询页时 body 就是质询页，
  // 由书源规则自行判断，行为与“验证失败但拿到了响应”一致。
  startBrowserAwait: function(url){ return __httpResponse(url, null); },
  // Legado `java.startBrowser(url, title)`：只打开内置浏览器，无返回值。服务端无 UI，等价空操作。
  startBrowser: function(){ return ""; },
  // Legado `java.webView(html, url, js)`：无 WebView 时只覆盖“取整页源码”的形态——
  // html 非空直接返回 html；否则抓取 url；传入 js 需要页面内 DOM 执行，显式报未支持。
  webView: function(html, url, js){
    if (js != null && String(js).trim() !== "") {
      throw new Error("UNSUPPORTED_JS_API:java.webView(js)（本服务无 WebView，无法执行页面内脚本）");
    }
    if (url != null && String(url).trim() !== "") {
      try { return __nativeAjax(String(url), "GET", "", ""); } catch(e) { return ""; }
    }
    return html == null ? "" : String(html);
  },
  // 这两个 API 依赖 WebView 观察最终资源地址/跳转地址，抓取层拿不到等价信号，保持显式未支持。
  webViewGetSource: function(){ throw new Error("UNSUPPORTED_JS_API:java.webViewGetSource（本服务无 WebView）"); },
  webViewGetOverrideUrl: function(){ throw new Error("UNSUPPORTED_JS_API:java.webViewGetOverrideUrl（本服务无 WebView）"); },
};

// ===== org.jsoup（按 147 源实际调用面实现）=====
// 后端是 scraper(html5ever)。jsoup 专有扩展伪类（:contains/:matches/:eq 等）由 Rust 侧显式抛错。
function JsoupElement(h){
  // `__h` 不可枚举：`for (i in element)` 不应看到内部句柄
  Object.defineProperty(this, "__h", { value: h, enumerable: false });
}
JsoupElement.prototype.attr = function(k){ return String(__jsoupAttr(this.__h, String(k))); };
JsoupElement.prototype.text = function(){ return String(__jsoupText(this.__h)); };
JsoupElement.prototype.html = function(){ return String(__jsoupHtml(this.__h)); };
JsoupElement.prototype.outerHtml = function(){ return String(__jsoupOuterHtml(this.__h)); };
JsoupElement.prototype.toString = function(){ return this.outerHtml(); };
JsoupElement.prototype.hasAttr = function(k){ return !!__jsoupHasAttr(this.__h, String(k)); };
JsoupElement.prototype.is = function(sel){ return !!__jsoupIs(this.__h, String(sel)); };
JsoupElement.prototype.tagName = function(){ return String(__jsoupTag(this.__h)); };
JsoupElement.prototype.parent = function(){
  var x = __jsoupParent(this.__h);
  return (x === null || x === undefined) ? null : new JsoupElement(x);
};
JsoupElement.prototype.nextSiblingElements = function(){
  return new JsoupElements(JSON.parse(__jsoupNextSiblingElements(this.__h)));
};
JsoupElement.prototype.select = function(sel){
  return new JsoupElements(JSON.parse(__jsoupSelect(this.__h, String(sel))));
};
JsoupElement.prototype.selectFirst = function(sel){
  return this.select(sel).first();
};
function JsoupElements(list){
  // `__list`/`length` 必须不可枚举：书源里大量 `for (i in a)` 依赖只枚举数字下标
  // （Rhino 对 Java List 的 for-in 行为），多一个属性就会把 `a['__list']` 当元素用。
  var arr = list || [];
  Object.defineProperty(this, "__list", { value: arr, enumerable: false });
  for (var i = 0; i < arr.length; i++) this[i] = new JsoupElement(arr[i]);
  Object.defineProperty(this, "length", { value: arr.length, enumerable: false });
}
JsoupElements.prototype.size = function(){ return this.length; };
JsoupElements.prototype.get = function(i){
  i = Number(i) || 0;
  return (i >= 0 && i < this.length) ? this[i] : null;
};
JsoupElements.prototype.first = function(){ return this.length ? this[0] : null; };
JsoupElements.prototype.last = function(){ return this.length ? this[this.length - 1] : null; };
// jsoup `Elements.eq(i)` 返回只含一个元素的 Elements（不是 Element）。
JsoupElements.prototype.eq = function(i){
  var n = Number(i) || 0;
  if (n < 0) n = this.length + n;
  return new JsoupElements((n >= 0 && n < this.length) ? [this.__list[n]] : []);
};
JsoupElements.prototype.attr = function(k){ return this.length ? this[0].attr(k) : ""; };
// jsoup `Elements.text()` 是**所有元素文本拼接**（空格分隔），`html()` 是所有内层 HTML 拼接。
JsoupElements.prototype.text = function(){
  var out = [];
  for (var i = 0; i < this.length; i++) out.push(this[i].text());
  return out.join(" ");
};
JsoupElements.prototype.html = function(){
  var out = [];
  for (var i = 0; i < this.length; i++) out.push(this[i].html());
  return out.join("");
};
JsoupElements.prototype.outerHtml = function(){ return this.length ? this[0].outerHtml() : ""; };
JsoupElements.prototype.hasAttr = function(k){ return this.length ? this[0].hasAttr(k) : false; };
JsoupElements.prototype.is = function(sel){
  for (var i = 0; i < this.length; i++) { if (this[i].is(sel)) return true; }
  return false;
};
JsoupElements.prototype.toArray = function(){
  var a = [];
  for (var i = 0; i < this.length; i++) a.push(this[i]);
  return a;
};
JsoupElements.prototype.select = function(sel){
  var all = [];
  for (var i = 0; i < this.length; i++) {
    var h = JSON.parse(__jsoupSelect(this.__list[i], String(sel)));
    for (var j = 0; j < h.length; j++) all.push(h[j]);
  }
  return new JsoupElements(all);
};
function JsoupDocument(h){
  Object.defineProperty(this, "__h", { value: h, enumerable: false });
}
JsoupDocument.prototype.select = function(sel){
  return new JsoupElements(JSON.parse(__jsoupSelect(this.__h, String(sel))));
};
JsoupDocument.prototype.selectFirst = function(sel){ return this.select(sel).first(); };
JsoupDocument.prototype.text = function(){
  var b = this.select("body").first();
  return b ? b.text() : "";
};
JsoupDocument.prototype.html = function(){
  var b = this.select("body").first();
  return b ? b.html() : "";
};
JsoupDocument.prototype.outerHtml = function(){ return String(__jsoupOuterHtml(this.__h)); };
// 原型方法默认 enumerable，会让 `for (i in elements)` 枚举到方法名（Boa/Rhino 的
// for-in 含继承的可枚举属性）。书源里大量 for-in 只应看到数字下标，这里统一隐藏。
function __jsHideProto(proto){
  for (var k in proto) {
    if (Object.prototype.hasOwnProperty.call(proto, k)) {
      Object.defineProperty(proto, k, {
        value: proto[k], enumerable: false, writable: true, configurable: true
      });
    }
  }
}
__jsHideProto(JsoupElement.prototype);
__jsHideProto(JsoupElements.prototype);
__jsHideProto(JsoupDocument.prototype);
var org = { jsoup: { Jsoup: {
  parse: function(html, base){
    var s = (html == null) ? "" : (typeof html === "string" ? html
      : (typeof html.outerHtml === "function" ? html.outerHtml() : String(html)));
    return new JsoupDocument(__jsoupParse(s, base == null ? __base : String(base)));
  },
  isValid: function(html){
    try { org.jsoup.Jsoup.parse(html); return true; } catch (e) { return false; }
  }
}}};

// ===== 字节/Buffer 辅助（哔哩漫画 image_decode 链路）=====
function __toByteArray(d){
  if (d == null) return [];
  if (typeof d === "string") {
    var b = __utf8(d), a = [];
    for (var i = 0; i < b.length; i++) a.push(b.charCodeAt(i));
    return a;
  }
  if (typeof d.length === "number") {
    var a = [];
    for (var i = 0; i < d.length; i++) a.push(Number(d[i]) & 255);
    return a;
  }
  return [];
}
// key/iv 参数：字节数组按 JSON 传给原生；字符串交给 Rust 按 UTF-8（Legado 的
// String.toByteArray()）转换，避免 `String([1,2,3])` 变成 "1,2,3"。
function __keyArg(x){
  if (x == null) return "";
  if (typeof x === "string") return x;
  return JSON.stringify(__toByteArray(x));
}
var Buffer = {
  concat: function(list){
    var out = [];
    if (!list) return out;
    for (var i = 0; i < list.length; i++) {
      var b = list[i];
      if (b == null) continue;
      var a = __toByteArray(b);
      for (var j = 0; j < a.length; j++) out.push(a[j]);
    }
    return out;
  }
};
"#;

/// DOM 桥上限：只作用于“已经抓到的页面”，输入体积、节点数、抽取总量都设硬上界，
/// 避免大列表把预抽取结果塞进 JS 源里造成内存/解析放大。
pub(super) const DOM_MAX_INPUT_BYTES: usize = 1024 * 1024;
pub(super) const DOM_MAX_NODES: usize = 2000;

/// 扫描 `java.getElements/getStringList/getString("字面量")` 调用。
/// 只提取字面量选择器；动态拼串不预处理，运行时由垫片返回具名“未支持”提示。
pub(super) fn scan_dom_calls(code: &str) -> Vec<(String, String)> {
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
pub(super) fn js_unescape(s: &str) -> String {
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

/// 解析书源 `jsLib` 字段（Legado 约定：`{库名: 脚本URL}` 的 JSON map）为脚本地址列表。
///
/// 真实源里存在三种形态：空串/非法 JSON（忽略）、JSON map（取非空字符串值）、
/// JSON 数组；个别源把整个字段直接写成单个 http(s) URL，也接受。
pub(super) fn js_lib_urls(js_lib: &str) -> Vec<String> {
    let s = js_lib.trim();
    if s.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut push = |u: &str| {
        let u = u.trim();
        if u.is_empty() || out.iter().any(|x| x == u) {
            return;
        }
        out.push(u.to_string());
    };
    match serde_json::from_str::<Value>(s) {
        Ok(Value::Object(map)) => {
            for v in map.values() {
                if let Some(u) = v.as_str() {
                    push(u);
                }
            }
        }
        Ok(Value::Array(list)) => {
            for v in list {
                if let Some(u) = v.as_str() {
                    push(u);
                }
            }
        }
        Ok(Value::String(u)) => push(&u),
        _ => {
            if s.starts_with("http://") || s.starts_with("https://") {
                push(s);
            }
        }
    }
    out
}

/// jsLib 预载片段。**必须插在 `JAVA_BOOK_SHIM` 之后**：jsLib 是 Legado 的共享作用域脚本
/// （`const { java, source, cookie, cache } = this;`），需要 `this` 上已有这些对象；
/// 它定义的 `Reload/Get` 等函数声明要能被执行规则代码的作用域看到，规则
/// `<js> eval(String(Reload('url'))) </js>` 才能直接调用。
///
/// 用**间接 eval**（`(0, eval)(src)`）把 jsLib 放到全局作用域执行，对齐 Legado：
/// jsLib 内部大量 `this.testUrl(...)`/`this.Option()` 依赖“顶层函数声明成为共享作用域
/// 的成员、函数里的 this 就是该作用域”。若用直接 eval 落进 IIFE 的局部作用域，
/// 这些 `this.xxx` 会全部 undefined。代价是这些全局名在单一 JS Context 里跨源留存；
/// 同名定义会被下一个带 jsLib 的源覆盖，没有 jsLib 的源不会引用这些名字。
///
/// 下载失败只降级（打日志，规则本身照常求值）：jsLib 是第三方站点资源，不能因为
/// 它不可达就把整个源的所有 JS 规则变成报错。
pub(super) fn js_lib_preload_js(urls: &[String]) -> String {
    if urls.is_empty() {
        return String::new();
    }
    let list = serde_json::to_string(urls).unwrap_or_else(|_| "[]".into());
    let mut out = String::new();
    out.push_str("var __jsLibUrls = ");
    out.push_str(&list);
    out.push_str(";\nvar __g = (typeof globalThis !== 'undefined') ? globalThis : this;\n");
    out.push_str("__g.java = java; __g.source = source; __g.cookie = cookie; __g.cache = cache; __g.book = book;\n");
    out.push_str("for (var __li = 0; __li < __jsLibUrls.length; __li++) {\n");
    out.push_str("  try {\n    var __ls = String(__nativeImportScript(__jsLibUrls[__li]));\n");
    out.push_str("    if (__ls) { (0, eval)(__ls); }\n  } catch (__le) {}\n}\n");
    out
}
