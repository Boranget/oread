use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::auth::{AuthUser, UserDb};
use crate::entities::{Book, BookChapter, BookContent, SearchBook};
use crate::error::{AppError, AppResult};
use crate::routes::replace;
use crate::routes::sources::{load_source, load_sources};
use crate::routes::AppState;
use crate::source_health;
use crate::source_ledger;
use crate::web_book;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/search", get(search))
        .route("/api/search/aggregate", get(search_aggregate))
        .route("/api/search/start", get(search_start))
        .route("/api/search/result", get(search_result))
        .route(
            "/api/search/jobs",
            get(search_jobs).delete(search_job_delete),
        )
        .route("/api/search/jobs/cancel", post(search_job_cancel))
        .route("/api/explore", get(explore))
        .route("/api/explore/categories", get(explore_categories))
        .route("/api/book/info", get(book_info))
        .route("/api/book/toc", get(toc))
        .route("/api/book/toc/batch", post(toc_batch))
        .route("/api/book/content", get(content))
        .route("/api/book/search", get(book_search))
        .route("/api/book/cache-all", post(cache_all))
        .route("/api/book/cache/clear", post(cache_clear))
        .route("/api/book/switch-source", post(switch_source))
}

#[derive(Deserialize)]
pub struct SearchQ {
    /// 正式参数名；与文档里的 `q` 互为别名，缺两者才 400。
    pub(super) key: Option<String>,
    #[serde(default)]
    pub(super) q: Option<String>,
    /// 逗号分隔的书源 id：支持 `book_source_url`（书源主键）或列表顺序的 1-based 序号。
    #[serde(default)]
    pub(super) source_ids: Option<String>,
    /// 显式越权：为 `1`/`true` 时冷却中的源也参与搜索（排查用）。
    #[serde(default)]
    pub(super) include_cooled: Option<String>,
    /// 搜索范围：`all`（默认，全部可搜索源）/ `curated`（精选源）。
    /// `/api/search`、`/api/search/aggregate`、`/api/search/start` 共用同一解析。
    #[serde(default)]
    pub(super) scope: Option<String>,
    pub(super) page: Option<i32>,
    pub(super) page_size: Option<usize>,
}

/// 宽松布尔：`1`/`true`/`yes`/`on` 视为真。
pub(super) fn truthy(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

impl SearchQ {
    pub(super) fn keyword(&self) -> String {
        self.key
            .as_deref()
            .or(self.q.as_deref())
            .unwrap_or("")
            .trim()
            .to_string()
    }

    /// 搜索范围：缺省 all（新默认），显式 curated 才收紧。
    pub(super) fn scope(&self) -> crate::req::SearchScope {
        crate::req::SearchScope::parse(self.scope.as_deref())
    }
}

/// 解析 `use_replace` 参数：返回 `Some(true/false)` 表示显式覆盖，
/// `None` 表示沿用账号设置。目录与正文共用同一套取值规则。
pub(super) fn parse_use_replace(raw: Option<&str>) -> Option<bool> {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("0") | Some("false") | Some("off") => Some(false),
        Some("1") | Some("true") | Some("on") => Some(true),
        _ => None,
    }
}

/// 合并请求参数与账号设置，得到本次请求最终是否应用替换规则。
pub(super) async fn resolve_use_replace(raw: Option<&str>, db: &SqlitePool) -> bool {
    match parse_use_replace(raw) {
        Some(v) => v,
        None => replace::replace_rule_enabled(db).await,
    }
}
#[derive(Deserialize)]
pub struct JobResultQ {
    pub(super) job_id: String,
    pub(super) page: Option<i32>,
    pub(super) page_size: Option<usize>,
    /// 稳定翻页游标：上一页返回的 `next_cursor`（`bucket:seq`）；
    /// 传 `start` 表示从头开始并立刻冻结快照。缺省时退回旧的 offset 分页。
    pub(super) cursor: Option<String>,
}
#[derive(Deserialize)]
pub struct ExploreQ {
    pub(super) url: String,
    pub(super) page: Option<i32>,
}
#[derive(Deserialize)]
pub struct CategoryQ {
    pub(super) source: String,
    pub(super) page: Option<i32>,
}
#[derive(Deserialize)]
pub struct BookQ {
    pub(super) source: String,
    pub(super) book_url: String,
    /// 搜索结果带入的字段：详情规则为空/未命中时沿用（Legado 语义），避免被写成 null
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) author: Option<String>,
    #[serde(default)]
    pub(super) intro: Option<String>,
    #[serde(default)]
    pub(super) cover_url: Option<String>,
    /// 目录标题是否应用替换规则：`1`/`0` 显式覆盖用户设置；缺省沿用账号的
    /// `use_replace_rule`（与 `/api/book/content` 同名参数）
    #[serde(default)]
    pub(super) use_replace: Option<String>,
}
#[derive(Deserialize)]
pub struct ContentQ {
    pub(super) source: String,
    pub(super) chapter_url: String,
    /// 显式传入书籍 URL：用于按书名匹配替换规则，并回填到响应
    pub(super) book_url: Option<String>,
    /// 章节标题兜底（章节记录里没有标题时）
    pub(super) title: Option<String>,
    /// 是否应用替换规则：`1`/`0` 显式覆盖用户设置；缺省沿用账号设置
    pub(super) use_replace: Option<String>,
}
#[derive(Deserialize)]
pub struct SwitchQ {
    pub(super) name: String,
    pub(super) author: Option<String>,
    pub(super) exclude_source: Option<String>,
}

/// 每一页最多返回的书籍数（聚合后统一分页）
pub(super) const DEFAULT_PAGE_SIZE: usize = 30;

/// 聚合搜索结果缓存：同一查询 60s 内复用同一份完整结果，保证翻页不重不漏、
/// `total` 可信；仅保留 4 个查询、每个最多 400 本，内存上界约 1~2 MiB。
pub(super) const AGG_TTL_SECS: u64 = 60;
pub(super) const AGG_MAX_QUERIES: usize = 4;
pub(super) const AGG_MAX_BOOKS: usize = 400;

#[path = "books_search_job.rs"]
mod books_search_job;
use books_search_job::*;
#[path = "books_search_store.rs"]
mod books_search_store;
use books_search_store::*;
#[path = "books_search_source.rs"]
mod books_search_source;
use books_search_source::*;
#[path = "books_search_api.rs"]
mod books_search_api;
use books_search_api::*;
#[path = "books_info.rs"]
mod books_info;
use books_info::*;
#[path = "books_content.rs"]
mod books_content;
use books_content::*;
#[path = "books_store.rs"]
mod books_store;
use books_store::*;

pub(crate) use books_search_api::aggregated;
pub(crate) use books_search_job::searches_active;
pub(crate) use books_search_source::{
    is_searchable_source, order_by_health, resolve_search_sources, CuratedMissingFallback,
};
pub(crate) use books_store::{
    book_store_for, load_book_meta, load_cached_chapters, load_cached_content,
    persist_book_changes, save_book_meta, save_chapters, save_content_cached,
};

/// 启动时把上次进程遗留的 running 搜索任务标为 interrupted。
///
/// 任务本身随进程结束，但用户重新登录后仍能在「搜索任务」里看到它，而不是一行
/// 永远不动的 running。失败只记日志：清理是尽力而为，不能阻止服务启动。
pub async fn mark_interrupted_search_jobs(pool: &SqlitePool) {
    match books_search_store::mark_interrupted(pool).await {
        Ok(n) if n > 0 => {
            tracing::info!(count = n, "marked interrupted search jobs");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "mark interrupted search jobs failed"),
    }
}

#[allow(clippy::field_reassign_with_default)] // 测试里按字段逐个构造对象，与生产默认值的差异一目了然。
#[cfg(test)]
#[path = "books_tests.rs"]
mod tests;
