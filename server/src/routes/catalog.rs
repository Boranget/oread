//! 个人图书馆读接口。
//!
//! 内容层（`library.db`）全局共享，个人状态（阅读状态 / 分组 / 进度）在
//! `users/<uid>.db`。两个库物理分离，因此这里先把个人状态读成内存映射，
//! 再与馆藏行合并、过滤、排序、分页。个人馆藏规模有限，换来的是一套
//! 与落盘结构解耦、无需跨库 ATTACH 的读模型。

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;

use crate::auth::{AuthUser, UserDb};
use crate::error::{AppError, AppResult};
use crate::library::{self, LibraryBook};
use crate::routes::bookshelf::progress_percent;
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/library/catalog", get(catalog))
        .route("/api/library/book", get(book_detail))
        .route("/api/library/facets", get(facets))
        .route("/api/library/home", get(home))
        .route("/api/bookshelf/status", post(set_status))
}

const STATUS_WANT: &str = "want";
const STATUS_READING: &str = "reading";
const STATUS_FINISHED: &str = "finished";

fn valid_status(s: &str) -> bool {
    matches!(s, STATUS_WANT | STATUS_READING | STATUS_FINISHED)
}

fn non_empty(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn like_pattern(term: &str) -> String {
    format!(
        "%{}%",
        term.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

// ---------------------------------------------------------------- 用户状态

#[derive(Default)]
struct UserState {
    status: HashMap<String, String>,
    group: HashMap<String, String>,
    on_shelf: HashSet<String>,
    added_at: HashMap<String, String>,
    /// book_id -> (chapter_index, position, updated_at)
    progress: HashMap<String, (i64, f64, Option<String>)>,
    /// book_id -> total_chapters（book_stats 口径）
    totals: HashMap<String, i64>,
}

async fn load_user_state(pool: &SqlitePool) -> AppResult<UserState> {
    let mut st = UserState::default();

    let statuses: Vec<(String, String)> =
        sqlx::query_as("SELECT book_id, status FROM reading_status")
            .fetch_all(pool)
            .await?;
    st.status.extend(statuses);

    let shelf: Vec<(String, Option<String>, String)> =
        sqlx::query_as("SELECT book_id, \"group\", added_at FROM bookshelf")
            .fetch_all(pool)
            .await?;
    for (id, group, added) in shelf {
        st.added_at.insert(id.clone(), added);
        st.on_shelf.insert(id.clone());
        if let Some(g) = group.filter(|g| !g.trim().is_empty()) {
            st.group.insert(id, g);
        }
    }

    let progress: Vec<(String, i64, f64, Option<String>)> =
        sqlx::query_as("SELECT book_id, chapter_index, position, updated_at FROM reading_progress")
            .fetch_all(pool)
            .await?;
    for (id, ci, pos, at) in progress {
        st.progress.insert(id, (ci, pos, at));
    }

    let totals: Vec<(String, i64)> =
        sqlx::query_as("SELECT book_id, total_chapters FROM book_stats")
            .fetch_all(pool)
            .await?;
    st.totals.extend(totals);

    Ok(st)
}

// ---------------------------------------------------------------- 馆藏行

#[derive(Debug, sqlx::FromRow)]
struct CatalogRow {
    book_url: String,
    name: Option<String>,
    author: Option<String>,
    kind: Option<String>,
    book_source_type: i64,
    cover_url: Option<String>,
    origin: Option<String>,
    origin_name: Option<String>,
    language: Option<String>,
    publisher: Option<String>,
    pubdate: Option<String>,
    series_name: Option<String>,
    series_index: Option<f64>,
    total_chapter_num: i64,
    cached_chapters: i64,
    collected_at: Option<i64>,
}

const CATALOG_COLS: &str = "book_url, name, author, kind, book_source_type, cover_url, \
     origin, origin_name, language, publisher, pubdate, series_name, series_index, \
     total_chapter_num, cached_chapters, collected_at";

fn row_total(row: &CatalogRow, user: &UserState) -> i64 {
    if row.total_chapter_num > 0 {
        row.total_chapter_num
    } else {
        user.totals.get(&row.book_url).copied().unwrap_or(0)
    }
}

fn row_percent(row: &CatalogRow, user: &UserState) -> f64 {
    let (ci, pos) = user
        .progress
        .get(&row.book_url)
        .map(|(ci, pos, _)| (*ci, *pos))
        .unwrap_or((0, 0.0));
    progress_percent(ci, pos, row_total(row, user))
}

fn row_json(row: &CatalogRow, user: &UserState, tags: &[String]) -> serde_json::Value {
    let (ci, pos, last_read) = user
        .progress
        .get(&row.book_url)
        .cloned()
        .unwrap_or((0, 0.0, None));
    let total = row_total(row, user);
    let status = user.status.get(&row.book_url).cloned().unwrap_or_default();
    json!({
        "book_url": row.book_url,
        "name": row.name,
        "author": row.author,
        "kind": row.kind,
        "book_source_type": row.book_source_type,
        "cover_url": row.cover_url,
        "origin": row.origin,
        "origin_name": row.origin_name,
        "language": row.language,
        "publisher": row.publisher,
        "pubdate": row.pubdate,
        "series_name": row.series_name,
        "series_index": row.series_index,
        "total_chapter_num": row.total_chapter_num,
        "cached_chapters": row.cached_chapters,
        "collected_at": row.collected_at,
        "status": status,
        "group": user.group.get(&row.book_url).cloned().unwrap_or_default(),
        "on_shelf": user.on_shelf.contains(&row.book_url),
        "added_at": user.added_at.get(&row.book_url),
        "chapter_index": ci,
        "position": pos,
        "total_chapters": total,
        "reading_progress": progress_percent(ci, pos, total),
        "last_read_at": last_read,
        "tags": tags,
    })
}

async fn tags_for(pool: &SqlitePool, urls: &[String]) -> AppResult<HashMap<String, Vec<String>>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    if urls.is_empty() {
        return Ok(out);
    }
    let placeholders = (1..=urls.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT book_url, tag FROM library_tags WHERE book_url IN ({placeholders}) ORDER BY tag"
    );
    let mut q = sqlx::query_as::<_, (String, String)>(&sql);
    for url in urls {
        q = q.bind(url);
    }
    for (url, tag) in q.fetch_all(pool).await? {
        out.entry(url).or_default().push(tag);
    }
    Ok(out)
}

/// 按 `book_url` 批量取馆藏行，保持入参顺序。
async fn load_rows_by_urls(
    pool: &SqlitePool,
    urls: &[String],
) -> AppResult<HashMap<String, CatalogRow>> {
    if urls.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = (1..=urls.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql =
        format!("SELECT {CATALOG_COLS} FROM library_books WHERE book_url IN ({placeholders})");
    let mut q = sqlx::query_as::<_, CatalogRow>(&sql);
    for url in urls {
        q = q.bind(url);
    }
    let rows = q.fetch_all(pool).await?;
    Ok(rows.into_iter().map(|r| (r.book_url.clone(), r)).collect())
}

// ---------------------------------------------------------------- 目录

#[derive(Deserialize)]
pub struct CatalogQ {
    /// `all`（默认）/ `want` / `reading` / `finished` / `none`
    pub status: Option<String>,
    /// 分组名；`ungrouped` 表示未分组
    pub group: Option<String>,
    pub author: Option<String>,
    pub series: Option<String>,
    pub tag: Option<String>,
    pub publisher: Option<String>,
    pub language: Option<String>,
    pub q: Option<String>,
    /// `1` 只看书架内的书；缺省/空表示全部馆藏
    pub shelf: Option<String>,
    /// `recent`（默认）/ `title` / `author` / `added` / `progress` / `series`
    pub sort: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

fn status_matches(actual: &str, want: &str) -> bool {
    match want {
        "" | "all" => true,
        "none" => actual.is_empty(),
        other => actual == other,
    }
}

fn group_matches(actual: &str, want: &str) -> bool {
    match want {
        "" | "all" => true,
        "ungrouped" => actual.is_empty(),
        other => actual == other,
    }
}

async fn catalog(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<CatalogQ>,
) -> AppResult<Json<serde_json::Value>> {
    let limit = q.limit.unwrap_or(24).clamp(1, 200);
    let page = q.page.unwrap_or(1).max(1);
    let sort = q.sort.as_deref().unwrap_or("recent");
    if !matches!(
        sort,
        "recent" | "title" | "author" | "added" | "progress" | "series"
    ) {
        return Err(AppError::BadRequest(
            "sort 只能是 recent/title/author/added/progress/series".into(),
        ));
    }
    let status_filter = q.status.as_deref().unwrap_or("all");
    if !matches!(
        status_filter,
        "all" | "want" | "reading" | "finished" | "none"
    ) {
        return Err(AppError::BadRequest(
            "status 只能是 all/want/reading/finished/none".into(),
        ));
    }

    let term = non_empty(q.q.as_deref());
    let author = non_empty(q.author.as_deref());
    let series = non_empty(q.series.as_deref());
    let tag = non_empty(q.tag.as_deref());
    let publisher = non_empty(q.publisher.as_deref());
    let language = non_empty(q.language.as_deref());

    let sql = format!(
        "SELECT {CATALOG_COLS} FROM library_books \
         WHERE tier = 'collection' \
           AND (?1 = '' OR COALESCE(name,'') LIKE ?1 ESCAPE '\\' \
                OR COALESCE(author,'') LIKE ?1 ESCAPE '\\' \
                OR COALESCE(series_name,'') LIKE ?1 ESCAPE '\\' \
                OR EXISTS (SELECT 1 FROM library_tags t \
                           WHERE t.book_url = library_books.book_url \
                             AND t.tag LIKE ?1 ESCAPE '\\')) \
           AND (?2 = '' OR author = ?2) \
           AND (?3 = '' OR series_name = ?3) \
           AND (?4 = '' OR publisher = ?4) \
           AND (?5 = '' OR language = ?5) \
           AND (?6 = '' OR EXISTS (SELECT 1 FROM library_tags t \
                                   WHERE t.book_url = library_books.book_url AND t.tag = ?6))"
    );
    let mut rows = sqlx::query_as::<_, CatalogRow>(&sql)
        .bind(term.map(|t| like_pattern(&t)).unwrap_or_default())
        .bind(author.unwrap_or_default())
        .bind(series.unwrap_or_default())
        .bind(publisher.unwrap_or_default())
        .bind(language.unwrap_or_default())
        .bind(tag.unwrap_or_default())
        .fetch_all(&s.library)
        .await?;

    let user = load_user_state(&db.0).await?;
    let group_filter = q.group.as_deref().unwrap_or("all");
    let shelf_only = matches!(
        non_empty(q.shelf.as_deref()).as_deref(),
        Some("1" | "true" | "yes")
    );

    rows.retain(|row| {
        let status = user
            .status
            .get(&row.book_url)
            .map(String::as_str)
            .unwrap_or("");
        let group = user
            .group
            .get(&row.book_url)
            .map(String::as_str)
            .unwrap_or("");
        (!shelf_only || user.on_shelf.contains(&row.book_url))
            && status_matches(status, status_filter)
            && group_matches(group, group_filter)
    });

    match sort {
        "title" => rows.sort_by(|a, b| {
            let ka = a.name.as_deref().unwrap_or("").to_lowercase();
            let kb = b.name.as_deref().unwrap_or("").to_lowercase();
            ka.cmp(&kb).then(a.book_url.cmp(&b.book_url))
        }),
        "author" => rows.sort_by(|a, b| {
            let ka = a.author.as_deref().unwrap_or("").to_lowercase();
            let kb = b.author.as_deref().unwrap_or("").to_lowercase();
            ka.cmp(&kb)
                .then(
                    a.series_index
                        .partial_cmp(&b.series_index)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(a.book_url.cmp(&b.book_url))
        }),
        "added" => rows.sort_by(|a, b| {
            b.collected_at
                .unwrap_or(0)
                .cmp(&a.collected_at.unwrap_or(0))
                .then(a.book_url.cmp(&b.book_url))
        }),
        "progress" => rows.sort_by(|a, b| {
            row_percent(b, &user)
                .partial_cmp(&row_percent(a, &user))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.book_url.cmp(&b.book_url))
        }),
        "series" => rows.sort_by(|a, b| {
            let ka = a.series_name.as_deref().unwrap_or("").to_lowercase();
            let kb = b.series_name.as_deref().unwrap_or("").to_lowercase();
            ka.cmp(&kb)
                .then(
                    a.series_index
                        .partial_cmp(&b.series_index)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(a.book_url.cmp(&b.book_url))
        }),
        _ => rows.sort_by(|a, b| {
            let ta = user
                .progress
                .get(&a.book_url)
                .and_then(|p| p.2.clone())
                .unwrap_or_default();
            let tb = user
                .progress
                .get(&b.book_url)
                .and_then(|p| p.2.clone())
                .unwrap_or_default();
            tb.cmp(&ta)
                .then(
                    b.collected_at
                        .unwrap_or(0)
                        .cmp(&a.collected_at.unwrap_or(0)),
                )
                .then(a.book_url.cmp(&b.book_url))
        }),
    }

    let total = rows.len() as i64;
    let pages = if total == 0 {
        0
    } else {
        (total + limit - 1) / limit
    };
    let start = ((page - 1) * limit).min(total) as usize;
    let end = (start + limit as usize).min(rows.len());
    let page_rows = &rows[start..end];
    let urls = page_rows
        .iter()
        .map(|r| r.book_url.clone())
        .collect::<Vec<_>>();
    let tag_map = tags_for(&s.library, &urls).await?;

    let items = page_rows
        .iter()
        .map(|row| {
            let tags = tag_map.get(&row.book_url).cloned().unwrap_or_default();
            row_json(row, &user, &tags)
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({
        "ok": true,
        "items": items,
        "total": total,
        "page": page,
        "pages": pages,
        "limit": limit,
        "sort": sort,
        "status": status_filter,
        "group": group_filter,
    })))
}

// ---------------------------------------------------------------- 书详情

#[derive(Deserialize)]
pub struct BookQ {
    pub book_url: Option<String>,
    /// 兼容旧调用方的 `url` 别名
    pub url: Option<String>,
}

async fn book_detail(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<BookQ>,
) -> AppResult<Json<serde_json::Value>> {
    let book_url = non_empty(q.book_url.as_deref())
        .or_else(|| non_empty(q.url.as_deref()))
        .ok_or_else(|| AppError::BadRequest("book_url 不能为空".into()))?;
    let book: LibraryBook = library::load_book(&s.library, &book_url)
        .await?
        .ok_or_else(|| AppError::NotFound("馆藏里没有这本书".into()))?;
    let extras = library::load_metadata_extras(&s.library, &book_url).await?;

    let user = load_user_state(&db.0).await?;
    let (ci, pos, last_read) = user
        .progress
        .get(&book_url)
        .cloned()
        .unwrap_or((0, 0.0, None));
    let total = if book.total_chapter_num > 0 {
        book.total_chapter_num
    } else {
        user.totals.get(&book_url).copied().unwrap_or(0)
    };
    let status = user.status.get(&book_url).cloned().unwrap_or_default();
    let group = user.group.get(&book_url).cloned().unwrap_or_default();

    let chapters: Vec<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT url, title, \"index\" FROM library_chapters \
         WHERE book_url = ?1 ORDER BY \"index\" LIMIT 20000",
    )
    .bind(&book_url)
    .fetch_all(&s.library)
    .await?;
    let toc = chapters
        .iter()
        .map(|(url, title, index)| {
            json!({
                "url": url,
                "title": title,
                "index": index,
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({
        "ok": true,
        "book": {
            "book_url": book.book_url,
            "name": book.name,
            "author": book.author,
            "intro": book.intro,
            "kind": book.kind,
            "book_source_type": book.book_source_type,
            "cover_url": book.cover_url,
            "language": book.language,
            "publisher": book.publisher,
            "pubdate": book.pubdate,
            "series_name": book.series_name,
            "series_index": book.series_index,
            "origin": book.origin,
            "origin_name": book.origin_name,
            "tier": book.tier,
            "collected_at": book.collected_at,
            "total_chapter_num": book.total_chapter_num,
            "cached_chapters": book.cached_chapters,
            "content_bytes": book.content_bytes,
            "image_bytes": book.image_bytes,
            "tags": extras.tags,
            "identifiers": extras
                .identifiers
                .iter()
                .map(|(scheme, value)| json!({ "scheme": scheme, "value": value }))
                .collect::<Vec<_>>(),
            "status": status,
            "group": group,
            "on_shelf": user.on_shelf.contains(&book_url),
            "added_at": user.added_at.get(&book_url),
            "chapter_index": ci,
            "position": pos,
            "total_chapters": total,
            "reading_progress": progress_percent(ci, pos, total),
            "last_read_at": last_read,
            "toc": toc,
        }
    })))
}

// ---------------------------------------------------------------- 分面

async fn facet_list(pool: &SqlitePool, sql: &str) -> AppResult<Vec<serde_json::Value>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(sql).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|(value, count)| json!({ "value": value, "count": count }))
        .collect())
}

async fn facets(State(s): State<AppState>, _u: AuthUser) -> AppResult<Json<serde_json::Value>> {
    let authors = facet_list(
        &s.library,
        "SELECT author AS value, COUNT(*) AS n FROM library_books \
         WHERE tier = 'collection' AND author IS NOT NULL AND TRIM(author) <> '' \
         GROUP BY author ORDER BY n DESC, value COLLATE NOCASE LIMIT 500",
    )
    .await?;
    let series = facet_list(
        &s.library,
        "SELECT series_name AS value, COUNT(*) AS n FROM library_books \
         WHERE tier = 'collection' AND series_name IS NOT NULL AND TRIM(series_name) <> '' \
         GROUP BY series_name ORDER BY n DESC, value COLLATE NOCASE LIMIT 500",
    )
    .await?;
    let publishers = facet_list(
        &s.library,
        "SELECT publisher AS value, COUNT(*) AS n FROM library_books \
         WHERE tier = 'collection' AND publisher IS NOT NULL AND TRIM(publisher) <> '' \
         GROUP BY publisher ORDER BY n DESC, value COLLATE NOCASE LIMIT 500",
    )
    .await?;
    let languages = facet_list(
        &s.library,
        "SELECT language AS value, COUNT(*) AS n FROM library_books \
         WHERE tier = 'collection' AND language IS NOT NULL AND TRIM(language) <> '' \
         GROUP BY language ORDER BY n DESC, value COLLATE NOCASE LIMIT 500",
    )
    .await?;
    let tags = facet_list(
        &s.library,
        "SELECT t.tag AS value, COUNT(*) AS n FROM library_tags t \
         JOIN library_books b ON b.book_url = t.book_url \
         WHERE b.tier = 'collection' \
         GROUP BY t.tag ORDER BY n DESC, value COLLATE NOCASE LIMIT 500",
    )
    .await?;

    Ok(Json(json!({
        "ok": true,
        "authors": authors,
        "series": series,
        "tags": tags,
        "publishers": publishers,
        "languages": languages,
    })))
}

// ---------------------------------------------------------------- 首页

fn lite_json(
    row: &CatalogRow,
    user: &UserState,
    tags: &[String],
    section: &str,
) -> serde_json::Value {
    let mut v = row_json(row, user, tags);
    v["section"] = json!(section);
    v
}

async fn home(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
) -> AppResult<Json<serde_json::Value>> {
    let user = load_user_state(&db.0).await?;

    let recent_rows = sqlx::query_as::<_, CatalogRow>(&format!(
        "SELECT {CATALOG_COLS} FROM library_books WHERE tier = 'collection' \
         ORDER BY COALESCE(collected_at, 0) DESC, book_url LIMIT 12"
    ))
    .fetch_all(&s.library)
    .await?;

    // 继续阅读：有进度、且没有标记为已读的书，按最近阅读时间排序。
    let progress: Vec<(String, i64, f64, Option<String>)> = sqlx::query_as(
        "SELECT book_id, chapter_index, position, updated_at FROM reading_progress \
         ORDER BY updated_at DESC LIMIT 40",
    )
    .fetch_all(&db.0)
    .await?;
    let continue_ids: Vec<String> = progress
        .into_iter()
        .filter(|(id, ci, pos, _)| {
            (*ci > 0 || *pos > 0.0)
                && user.status.get(id).map(String::as_str) != Some(STATUS_FINISHED)
        })
        .map(|(id, _, _, _)| id)
        .take(8)
        .collect();
    let continue_rows = load_rows_by_urls(&s.library, &continue_ids).await?;
    let continue_items: Vec<serde_json::Value> = continue_ids
        .iter()
        .filter_map(|id| continue_rows.get(id))
        .map(|row| lite_json(row, &user, &[], "continue"))
        .collect();

    let want_ids: Vec<String> = sqlx::query_scalar(
        "SELECT book_id FROM reading_status WHERE status = 'want' \
         ORDER BY updated_at DESC LIMIT 12",
    )
    .fetch_all(&db.0)
    .await?;
    let want_rows = load_rows_by_urls(&s.library, &want_ids).await?;
    let want_items: Vec<serde_json::Value> = want_ids
        .iter()
        .filter_map(|id| want_rows.get(id))
        .map(|row| lite_json(row, &user, &[], "want"))
        .collect();

    let collection_total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM library_books WHERE tier = 'collection'")
            .fetch_one(&s.library)
            .await?;
    let reading: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reading_status WHERE status = 'reading'")
            .fetch_one(&db.0)
            .await?;
    let finished: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reading_status WHERE status = 'finished'")
            .fetch_one(&db.0)
            .await?;
    let want: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reading_status WHERE status = 'want'")
        .fetch_one(&db.0)
        .await?;
    let shelf: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bookshelf")
        .fetch_one(&db.0)
        .await?;

    Ok(Json(json!({
        "ok": true,
        "stats": {
            "collection": collection_total,
            "shelf": shelf,
            "want": want,
            "reading": reading,
            "finished": finished,
        },
        "continue": continue_items,
        "recent": recent_rows
            .iter()
            .map(|row| lite_json(row, &user, &[], "recent"))
            .collect::<Vec<_>>(),
        "want": want_items,
    })))
}

// ---------------------------------------------------------------- 阅读状态

#[derive(Deserialize)]
pub struct StatusQ {
    pub book_id: Option<String>,
    pub book_ids: Option<Vec<String>>,
    /// want / reading / finished；空串或 null 表示清除状态
    pub status: Option<String>,
}

async fn set_status(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<StatusQ>,
) -> AppResult<Json<serde_json::Value>> {
    let mut ids: Vec<String> = q.book_ids.unwrap_or_default();
    if let Some(id) = q.book_id.and_then(|v| non_empty(Some(&v))) {
        ids.push(id);
    }
    ids.retain(|id| !id.trim().is_empty());
    ids.dedup();
    if ids.is_empty() {
        return Err(AppError::BadRequest("book_id / book_ids 不能为空".into()));
    }
    let status = q.status.unwrap_or_default();
    let status = status.trim();
    if !status.is_empty() && !valid_status(status) {
        return Err(AppError::BadRequest(
            "status 只能是 want/reading/finished 或空".into(),
        ));
    }

    let mut updated = 0i64;
    for id in &ids {
        if status.is_empty() {
            sqlx::query("DELETE FROM reading_status WHERE book_id = ?1")
                .bind(id)
                .execute(&db.0)
                .await?;
        } else {
            sqlx::query(
                "INSERT INTO reading_status (book_id, status, updated_at) \
                 VALUES (?1, ?2, datetime('now')) \
                 ON CONFLICT(book_id) DO UPDATE SET status = excluded.status, \
                 updated_at = datetime('now')",
            )
            .bind(id)
            .bind(status)
            .execute(&db.0)
            .await?;
        }
        updated += 1;
    }

    Ok(Json(json!({
        "ok": true,
        "updated": updated,
        "status": status,
    })))
}
