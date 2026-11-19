//! 只读运行指标：RSS、打开的数据库数、正文缓存占用、JS 规则耗时。
//!
//! 不引入任何额外依赖：Linux 读 `/proc/self/statm`，Windows 走 kernel32 的
//! `GetProcessMemoryInfo`（std FFI），其他平台返回 `null` 并在字段中标注。

use axum::{extract::State, Json};
use serde_json::json;
use sqlx::SqlitePool;

use crate::auth::{AuthUser, UserDb};
use crate::error::AppResult;
use crate::routes::AppState;

const MB: u64 = 1024 * 1024;

pub async fn simple() -> &'static str {
    "ok"
}

/// 进程 RSS（字节）
#[cfg(target_os = "linux")]
pub fn rss_bytes() -> Option<u64> {
    // statm: size resident shared text lib data dt（单位：页）
    let raw = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident: u64 = raw.split_whitespace().nth(1)?.parse().ok()?;
    Some(resident * 4096)
}

#[cfg(windows)]
pub fn rss_bytes() -> Option<u64> {
    #[repr(C)]
    #[allow(non_snake_case)]
    struct ProcessMemoryCounters {
        cb: u32,
        PageFaultCount: u32,
        PeakWorkingSetSize: usize,
        WorkingSetSize: usize,
        QuotaPeakPagedPoolUsage: usize,
        QuotaPagedPoolUsage: usize,
        QuotaPeakNonPagedPoolUsage: usize,
        QuotaNonPagedPoolUsage: usize,
        PagefileUsage: usize,
        PeakPagefileUsage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
    }
    // GetProcessMemoryInfo 位于 psapi.dll，需单独链接
    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }
    let mut c = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        PageFaultCount: 0,
        PeakWorkingSetSize: 0,
        WorkingSetSize: 0,
        QuotaPeakPagedPoolUsage: 0,
        QuotaPagedPoolUsage: 0,
        QuotaPeakNonPagedPoolUsage: 0,
        QuotaNonPagedPoolUsage: 0,
        PagefileUsage: 0,
        PeakPagefileUsage: 0,
    };
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    if ok == 0 {
        None
    } else {
        Some(c.WorkingSetSize as u64)
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn rss_bytes() -> Option<u64> {
    None
}

/// 馆藏正文占用（字节 + 行数）
async fn cache_usage(pool: &SqlitePool) -> (i64, i64) {
    let bytes: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(size_bytes),0) FROM library_content")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_content")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    (bytes, rows)
}

/// 按书占用（前 5 本），用于定位馆藏空间被谁占住。
/// `name` 取自 `library_books`，缺失时前端回退显示 `book_url`。
async fn cache_usage_by_book(pool: &SqlitePool) -> Vec<(String, String, i64)> {
    sqlx::query_as(
        "SELECT c.book_url, COALESCE(b.name, '') AS name, COALESCE(SUM(c.size_bytes),0) AS total \
         FROM library_content c LEFT JOIN library_books b ON b.book_url = c.book_url \
         GROUP BY c.book_url ORDER BY total DESC LIMIT 5",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default()
}

fn base_metrics(state: &AppState) -> serde_json::Value {
    let rss = rss_bytes();
    let (js_last, js_max) = crate::rule_engine::js_metrics();
    let (img_bytes, img_items, img_limit) = crate::image_cache::stats();
    let cache = crate::cache_metrics::stats();
    json!({
        // 当前运行的服务版本，前端设置页直接展示
        "version": env!("CARGO_PKG_VERSION"),
        "rss_bytes": rss,
        "rss_mb": rss.map(|b| (b as f64 / MB as f64 * 100.0).round() / 100.0),
        "open_user_dbs": state.user_pools.lock().map(|c| c.len()).unwrap_or(0),
        // 容量上限已废弃：馆藏永久保存，只报实际占用（见下方 cache.bytes）。
        "js": {
            "enabled": state.cfg.js_enabled,
            "last_ms": js_last,
            "max_ms": js_max,
            "budget_ms": state.cfg.js_timeout_ms,
        },
        "http": {
            "concurrency": state.cfg.http_concurrency,
            "timeout_secs": state.cfg.http_timeout_secs,
            "max_body_mb": state.cfg.http_max_body_bytes / (1024 * 1024),
        },
        // 搜索调度的真实生效值（与启动日志 `search budgets resolved` 同源）：
        // 单源预算按 scope 分档（curated 8s / all 5s）；整批预算按 scope 分档；源级并发槽按 scope 分池。
        "search": {
            "per_source_secs": state.cfg.search_timeout_secs,
            "per_source_ms": state.cfg.search_timeout_secs * 1000,
            "all_per_source_secs": state.cfg.search_all_timeout_secs,
            "all_per_source_ms": state.cfg.search_all_timeout_secs * 1000,
            "total_curated_secs": state.cfg.search_curated_budget_secs,
            "total_all_secs": state.cfg.search_all_budget_secs,
            "source_slots": state.cfg.http_concurrency,
            "all_source_slots": state.cfg.search_all_slots,
            "comic_slots": (state.cfg.http_concurrency / 4).max(1),
            "comic_slots_all": (state.cfg.search_all_slots / 4).max(1),
            // 每源在飞上限：书源未声明 concurrentRate 时的默认值，1–8。
            "source_concurrency_default": state.cfg.source_concurrency_default,
            "source_concurrency_min": crate::config::SOURCE_CONCURRENCY_MIN,
            "source_concurrency_max": crate::config::SOURCE_CONCURRENCY_MAX,
            // 按源限流导致的等待次数 / 累计等待毫秒（进程生命周期内累计）。
            "source_limit_waits": crate::req::source_limit_metrics().0,
            "source_limit_wait_ms": crate::req::source_limit_metrics().1,
            // 服务端书名包含过滤：raw/kept/dropped 为进程内累计，
            // 用于判断过滤是否过狠（每响应明细见 5.2/5.6 的 title_filter 字段）。
            "title_filter": crate::title_filter::metrics(),
        },
        // 正文缓存命中/落库/淘汰计数（进程生命周期内累计，仅用于观察与调参）
        "content_cache": {
            "hits": cache.hits,
            "misses": cache.misses,
            "hit_rate": cache.hit_rate(),
            "saves": cache.saves,
            "evictions": cache.evictions,
            "evicted_rows": cache.evicted_rows,
        },
        // 图片代理进程内 LRU（与正文缓存分开，但同属缓存预算）
        "image_cache": {
            "bytes": img_bytes,
            "mb": (img_bytes as f64 / MB as f64 * 100.0).round() / 100.0,
            "items": img_items,
            "limit_mb": img_limit / (1024 * 1024),
        },
    })
}

/// 公开健康检查：仅进程级只读指标（不含任何用户数据），部署探活可直接轮询。
pub async fn healthz(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mut v = base_metrics(&state);
    if let Some(obj) = v.as_object_mut() {
        obj.insert("ok".into(), json!(true));
    }
    Json(v)
}

/// 需要登录：在公开指标上追加馆藏占用（全局共享，不设上限，只做监控）。
pub async fn healthz_user(
    State(state): State<AppState>,
    _u: AuthUser,
    _db: UserDb,
) -> AppResult<Json<serde_json::Value>> {
    let (bytes, rows) = cache_usage(&state.library).await;
    let books = cache_usage_by_book(&state.library).await;
    let stats = crate::library::stats(&state.library, state.cfg.library_temp_ttl_secs).await;
    let mut v = base_metrics(&state);
    if let Some(obj) = v.as_object_mut() {
        obj.insert("ok".into(), json!(true));
        obj.insert(
            "cache".into(),
            json!({
                "bytes": bytes,
                "mb": (bytes as f64 / MB as f64 * 100.0).round() / 100.0,
                "rows": rows,
                // 分层监控：馆藏永久保存，临时缓存按 TTL 回收
                "collection": {
                    "books": stats.collection_books,
                    "chapters": stats.collection_chapters,
                    "bytes": stats.collection_bytes,
                    "mb": (stats.collection_bytes as f64 / MB as f64 * 100.0).round() / 100.0,
                },
                "temp": {
                    "books": stats.temp_books,
                    "chapters": stats.temp_chapters,
                    "bytes": stats.temp_bytes,
                    "mb": (stats.temp_bytes as f64 / MB as f64 * 100.0).round() / 100.0,
                    "expired_books": stats.temp_expired_books,
                    "ttl_days": state.cfg.library_temp_ttl_secs / 86_400,
                },
                "books_total": stats.collection_books + stats.temp_books,
                // 图片缓存计入占用统计（进程级，所有用户共享）
                "image_cache_bytes": crate::image_cache::stats().0,
                "total_bytes": bytes + crate::image_cache::stats().0 as i64,
                // 前 5 本按占用降序，便于定位空间被谁占住
                "books": books
                    .iter()
                    .map(|(url, name, bytes)| json!({
                        "book_url": url,
                        "name": name,
                        "bytes": bytes,
                    }))
                    .collect::<Vec<_>>(),
            }),
        );
    }
    Ok(Json(v))
}
