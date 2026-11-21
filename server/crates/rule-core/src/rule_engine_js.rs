//! JS 规则执行层（move-only 从 rule_engine.rs 拆出）。
//!
//! 包含 boa 运行时、worker/并发闸门、上游规则 Java/JSoup shim 与原生扩展；
//! 对父模块暴露 init_js / js_metrics 与内部 job 入口，其余保持模块私有。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use md5::{Digest, Md5};
use once_cell::sync::{Lazy, OnceCell};
use regex::Regex;
use serde_json::Value;

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

/// `cache.putFile` 单文件上限（上游规则无上限，这里给出明确边界）。
pub(super) const CACHE_FILE_MAX_BYTES: usize = 16 * 1024 * 1024;

/// 上游规则 `cache.getFile/putFile` 用 `key.hashCode()` 作文件名；这里改用 md5
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
        // 上游规则的过期前缀：13 位当前毫秒 + "-" + saveTime + 空格 + 内容（getFile 原样返回）。
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

/// 书源级变量（上游规则 `source.get/put` 落到 `CacheManager` 的 `v_<sourceKey>_<key>`）。
/// 本服务按进程内、按书源分桶保存：不落盘，重启即清空；上游规则的磁盘缓存语义未复刻。
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

/// `source.get(key)`：按当前书源读进程内变量；无值返回空串（与上游规则一致）。
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

/// `source.put(key, value)`：按当前书源写进程内变量，返回写入的字符串值（与上游规则一致）。
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
/// `{status, url, headers}` 供 JS 构造与上游规则 StrResponse 同形的响应对象。
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

/// `java.importScript` 的地址形态判定。上游规则 `p0.i0` 还支持 file/content/storage
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

// org.jsoup JS-Java 桥；显式 path 保持 rule_engine_js.rs 作为逻辑入口。
#[path = "rule_engine_js_jsoup.rs"]
mod jsoup;
pub(super) use jsoup::*;

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

/// key/iv 参数：JS 字节数组走 JSON（`[..]`），字符串按上游规则 `String.toByteArray()`（UTF-8）。
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
// JS 字符串资产；显式 path 保持 rule_engine_js.rs 作为逻辑入口。
#[path = "rule_engine_js_consts.rs"]
mod js_consts;
pub(super) use js_consts::*;

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

/// 解析书源 `jsLib` 字段（上游规则约定：`{库名: 脚本URL}` 的 JSON map）为脚本地址列表。
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

/// jsLib 预载片段。**必须插在 `JAVA_BOOK_SHIM` 之后**：jsLib 是上游规则的共享作用域脚本
/// （`const { java, source, cookie, cache } = this;`），需要 `this` 上已有这些对象；
/// 它定义的 `Reload/Get` 等函数声明要能被执行规则代码的作用域看到，规则
/// `<js> eval(String(Reload('url'))) </js>` 才能直接调用。
///
/// 用**间接 eval**（`(0, eval)(src)`）把 jsLib 放到全局作用域执行，对齐上游规则：
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
