//! 馆藏浏览与管理接口。
//!
//! 内容层全局共享：任何登录账号都能浏览馆藏、看占用；只有管理员能入馆 / 移出 /
//! 删除整本 / 清理临时缓存。入馆会顺带排一张「采购单」（后台整本下载），
//! 由 `crate::library_worker` 在服务端跑，不依赖前端保持页面打开。

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{ensure_admin, AuthUser};
use crate::error::{AppError, AppResult};
use crate::library::{self, LibraryBook};
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/library/stats", get(stats))
        .route("/api/library/books", get(books))
        .route("/api/library/collect", post(collect))
        .route("/api/library/release", post(release))
        .route("/api/library/delete", post(delete))
        .route("/api/library/temp/clean", post(clean_temp))
        .route("/api/library/jobs", get(jobs))
        .route("/api/library/jobs/cancel", post(cancel_job))
}

fn mb(bytes: i64) -> f64 {
    (bytes as f64 / (1024.0 * 1024.0) * 100.0).round() / 100.0
}

/// 馆藏汇总 + 磁盘水位。任何登录账号可读（占用监控不需要管理员）。
async fn stats(State(s): State<AppState>, _u: AuthUser) -> AppResult<Json<serde_json::Value>> {
    let st = library::stats(&s.library, s.cfg.library_temp_ttl_secs).await;
    let free = library::free_bytes(std::path::Path::new(&s.cfg.data_dir));
    let floor = s.cfg.library_disk_floor_bytes;
    let paused = free.map(|f| f < floor).unwrap_or(false);
    Ok(Json(json!({
        "ok": true,
        "collection": {
            "books": st.collection_books,
            "chapters": st.collection_chapters,
            "bytes": st.collection_bytes,
            "mb": mb(st.collection_bytes),
        },
        "temp": {
            "books": st.temp_books,
            "chapters": st.temp_chapters,
            "bytes": st.temp_bytes,
            "mb": mb(st.temp_bytes),
            "expired_books": st.temp_expired_books,
            "ttl_days": s.cfg.library_temp_ttl_secs / 86_400,
        },
        "total": {
            "books": st.collection_books + st.temp_books,
            "chapters": st.total_chapters,
            "content_rows": st.content_rows,
            "bytes": st.content_bytes,
            "mb": mb(st.content_bytes),
        },
        "disk": {
            "free_bytes": free,
            "free_mb": free.map(|b| (b / (1024 * 1024)) as i64),
            "floor_bytes": floor,
            "floor_mb": floor / (1024 * 1024),
            // 剩余空间低于安全阀：后台整本下载暂停（交互阅读不受影响）
            "download_paused": paused,
        },
    })))
}

#[derive(Deserialize)]
pub struct BooksQ {
    /// `collection`（默认）| `temp` | `all`
    pub tier: Option<String>,
    /// 书名/作者模糊过滤
    pub q: Option<String>,
    /// keyset 游标：`<sort_ts>:<book_url>`，首页不传
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    /// `accessed`（默认）| `collected`
    pub sort: Option<String>,
}

fn parse_cursor(raw: Option<&str>) -> (i64, String) {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return (i64::MAX, String::new());
    };
    match raw.split_once(':') {
        Some((ts, url)) => (ts.parse::<i64>().unwrap_or(i64::MAX), url.to_string()),
        None => (i64::MAX, String::new()),
    }
}

fn book_item(b: &LibraryBook) -> serde_json::Value {
    json!({
        "book_url": b.book_url,
        "name": b.name,
        "author": b.author,
        "origin": b.origin,
        "origin_name": b.origin_name,
        "cover_url": b.cover_url,
        "tier": b.tier,
        "collected_at": b.collected_at,
        "last_accessed_at": b.last_accessed_at,
        "checked_at": b.checked_at,
        "total_chapter_num": b.total_chapter_num,
        "cached_chapters": b.cached_chapters,
        "content_bytes": b.content_bytes,
        "content_mb": mb(b.content_bytes),
        // 下载进度用百分比表示，目录未知时为 null（前端显示「未知」）
        "download_percent": if b.total_chapter_num > 0 {
            Some((b.cached_chapters * 100 / b.total_chapter_num).min(100))
        } else {
            None
        },
    })
}

/// 馆藏浏览：keyset 分页（`sort_ts DESC, book_url ASC`），不返回总数、不做 OFFSET。
async fn books(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<BooksQ>,
) -> AppResult<Json<serde_json::Value>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let tier = q.tier.as_deref().unwrap_or("collection");
    let tier_filter = match tier {
        "collection" | "temp" | "all" => tier,
        _ => {
            return Err(AppError::BadRequest(
                "tier 只能是 collection/temp/all".into(),
            ))
        }
    };
    let sort = q.sort.as_deref().unwrap_or("accessed");
    let sort_expr = match sort {
        "accessed" => "last_accessed_at",
        // 临时书没有入馆时间，统一按 0 排序，避免 NULL 在 keyset 比较里被漏掉。
        "collected" => "COALESCE(collected_at, 0)",
        _ => {
            return Err(AppError::BadRequest(
                "sort 只能是 accessed/collected".into(),
            ))
        }
    };
    let (ts, after_url) = parse_cursor(q.cursor.as_deref());
    let like =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("%{}%", s.replace('%', "\\%").replace('_', "\\_")));

    let sql = format!(
        "SELECT * FROM library_books \
         WHERE (?1 = 'all' OR tier = ?1) \
           AND (?2 = '' OR name LIKE ?2 ESCAPE '\\' OR author LIKE ?2 ESCAPE '\\') \
           AND ({col} < ?3 OR ({col} = ?3 AND book_url > ?4)) \
         ORDER BY {col} DESC, book_url ASC LIMIT ?5",
        col = sort_expr
    );
    let rows: Vec<LibraryBook> = sqlx::query_as(&sql)
        .bind(tier_filter)
        .bind(like.clone().unwrap_or_default())
        .bind(ts)
        .bind(&after_url)
        .bind(limit + 1)
        .fetch_all(&s.library)
        .await?;
    let has_more = rows.len() as i64 > limit;
    let page: Vec<&LibraryBook> = rows.iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        page.last().map(|b| {
            let key = if sort == "collected" {
                b.collected_at.unwrap_or(0)
            } else {
                b.last_accessed_at
            };
            format!("{key}:{}", b.book_url)
        })
    } else {
        None
    };
    Ok(Json(json!({
        "ok": true,
        "tier": tier_filter,
        "sort": sort,
        "limit": limit,
        "items": page.iter().map(|b| book_item(b)).collect::<Vec<_>>(),
        "next_cursor": next_cursor,
    })))
}

#[derive(Deserialize)]
pub struct BookReq {
    #[serde(alias = "book_id")]
    pub book_url: String,
    /// 可选书源；缺省用馆藏书目里记录的 origin
    pub source: Option<String>,
}

/// 入馆：置为永久保存，并排一张后台整本下载任务。仅管理员。
async fn collect(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<BookReq>,
) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&u)?;
    let book_url = q.book_url.trim().to_string();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    let created = library::collect(&s.library, &book_url).await?;
    let book = library::load_book(&s.library, &book_url).await?;
    let source = q
        .source
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| book.as_ref().and_then(|b| b.origin.clone()))
        .unwrap_or_default();
    let job_id = if source.is_empty() {
        None
    } else {
        Some(library::enqueue_job(&s.library, &book_url, &source, u.id).await?)
    };
    Ok(Json(json!({
        "ok": true,
        "book_url": book_url,
        "newly_collected": created,
        "job_id": job_id,
        // 没记录书源时无法后台下载，前端可提示补 source 后重新入馆
        "queued": job_id.is_some(),
    })))
}

/// 移出馆藏：降级为临时缓存，内容先留着，交给 TTL 或手动删除。仅管理员。
async fn release(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<BookReq>,
) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&u)?;
    let book_url = q.book_url.trim();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    let changed = library::release(&s.library, book_url).await?;
    Ok(Json(json!({
        "ok": true,
        "book_url": book_url,
        "released": changed,
        "tier": library::TIER_TEMP,
    })))
}

/// 整本删除（书目 + 目录 + 正文）。馆藏需先移出；仅管理员。
async fn delete(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<BookReq>,
) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&u)?;
    let book_url = q.book_url.trim();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    let book = library::load_book(&s.library, book_url).await?;
    match book {
        None => return Err(AppError::NotFound("馆藏里没有这本书".into())),
        Some(b) if b.tier == library::TIER_COLLECTION => {
            return Err(AppError::BadRequest(
                "该书仍在馆藏中，请先移出馆藏再删除".into(),
            ))
        }
        Some(_) => {}
    }
    let (books, chapters, contents, bytes) = library::delete_book(&s.library, book_url).await?;
    Ok(Json(json!({
        "ok": true,
        "book_url": book_url,
        "deleted_books": books,
        "deleted_chapters": chapters,
        "deleted_contents": contents,
        "bytes_freed": bytes,
    })))
}

/// 立即回收过期临时缓存（不等定期任务）。仅管理员。
async fn clean_temp(State(s): State<AppState>, u: AuthUser) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&u)?;
    let report = library::cleanup_temp(&s.library, s.cfg.library_temp_ttl_secs).await?;
    Ok(Json(json!({
        "ok": true,
        "books": report.books,
        "chapters": report.chapters,
        "contents": report.contents,
        "bytes_freed": report.bytes,
        "ttl_days": s.cfg.library_temp_ttl_secs / 86_400,
    })))
}

#[derive(Deserialize)]
pub struct JobsQ {
    /// 逗号分隔的状态过滤，缺省返回最近 100 条
    pub status: Option<String>,
    pub limit: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct JobListRow {
    id: String,
    book_url: String,
    source: String,
    status: String,
    total: i64,
    done: i64,
    failed: i64,
    skipped: i64,
    error: Option<String>,
    created_at: i64,
    updated_at: i64,
    finished_at: Option<i64>,
}

/// 采购单列表：按创建时间倒序。仅管理员。
async fn jobs(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<JobsQ>,
) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&u)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let statuses: Vec<String> = q
        .status
        .as_deref()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    // 状态过滤用 IN (...) 动态占位；空列表时退化为全量。
    let mut sql = String::from(
        "SELECT id, book_url, source, status, total, done, failed, skipped, error, \
         created_at, updated_at, finished_at FROM library_jobs",
    );
    if !statuses.is_empty() {
        let marks: Vec<String> = (1..=statuses.len()).map(|i| format!("?{i}")).collect();
        sql.push_str(&format!(" WHERE status IN ({})", marks.join(",")));
    }
    sql.push_str(&format!(
        " ORDER BY created_at DESC LIMIT ?{}",
        statuses.len() + 1
    ));
    let mut query = sqlx::query_as::<_, JobListRow>(&sql);
    for st in &statuses {
        query = query.bind(st);
    }
    let rows = query.bind(limit).fetch_all(&s.library).await?;
    Ok(Json(json!({
        "ok": true,
        "items": rows.iter().map(|j| json!({
            "id": j.id,
            "book_url": j.book_url,
            "source": j.source,
            "status": j.status,
            "total": j.total,
            "done": j.done,
            "failed": j.failed,
            "skipped": j.skipped,
            "error": j.error,
            "created_at": j.created_at,
            "updated_at": j.updated_at,
            "finished_at": j.finished_at,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct CancelReq {
    pub id: String,
}

/// 取消采购单。仅管理员。
async fn cancel_job(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<CancelReq>,
) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&u)?;
    let id = q.id.trim();
    if id.is_empty() {
        return Err(AppError::BadRequest("id 不能为空".into()));
    }
    let cancelled = library::cancel_job(&s.library, id).await?;
    Ok(Json(
        json!({ "ok": true, "id": id, "cancelled": cancelled }),
    ))
}
