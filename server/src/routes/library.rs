//! 馆藏浏览与管理接口。
//!
//! 内容层全局共享：任何登录账号都能浏览馆藏、看占用；只有管理员能入馆 / 移出 /
//! 删除整本 / 清理临时缓存。入馆会顺带排一张「采购单」（后台整本下载），
//! 由 `crate::library_worker` 在服务端跑，不依赖前端保持页面打开。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::stream::StreamExt;

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{ensure_admin, AuthUser};
use crate::entities::Book;
use crate::error::{AppError, AppResult};
use crate::image_fetch;
use crate::library::{self, LibraryBook};
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/library/stats", get(stats))
        .route("/api/library/books", get(books))
        .route("/api/library/book/meta", get(book_meta).post(set_book_meta))
        .route("/api/library/comic/estimate", post(comic_estimate))
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
            "text_bytes": st.collection_text_bytes,
            "image_bytes": st.collection_image_bytes,
        },
        "temp": {
            "books": st.temp_books,
            "chapters": st.temp_chapters,
            "bytes": st.temp_bytes,
            "mb": mb(st.temp_bytes),
            "text_bytes": st.temp_text_bytes,
            "image_bytes": st.temp_image_bytes,
            "expired_books": st.temp_expired_books,
            "ttl_days": s.cfg.library_temp_ttl_secs / 86_400,
        },
        "total": {
            "books": st.collection_books + st.temp_books,
            "chapters": st.total_chapters,
            "content_rows": st.content_rows,
            "image_rows": st.image_rows,
            "text_bytes": st.content_bytes,
            "image_bytes": st.image_bytes,
            "bytes": st.total_bytes,
            "mb": mb(st.total_bytes),
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

fn parse_cursor(raw: Option<&str>) -> AppResult<(i64, String)> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok((i64::MAX, String::new()));
    };
    let (ts, url) = raw
        .split_once(':')
        .ok_or_else(|| AppError::BadRequest("cursor 格式应为 <sort_ts>:<book_url>".into()))?;
    let ts = ts
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("cursor 的时间戳不是整数".into()))?;
    if url.is_empty() {
        return Err(AppError::BadRequest("cursor 缺少 book_url".into()));
    }
    Ok((ts, url.to_string()))
}

fn book_item(b: &LibraryBook) -> serde_json::Value {
    json!({
        "book_url": b.book_url,
        "name": b.name,
        "author": b.author,
        "kind": b.kind,
        "book_source_type": b.book_source_type,
        "origin": b.origin,
        "origin_name": b.origin_name,
        "cover_url": b.cover_url,
        "language": b.language,
        "publisher": b.publisher,
        "pubdate": b.pubdate,
        "series_name": b.series_name,
        "series_index": b.series_index,
        "tier": b.tier,
        "collected_at": b.collected_at,
        "last_accessed_at": b.last_accessed_at,
        "checked_at": b.checked_at,
        "total_chapter_num": b.total_chapter_num,
        "cached_chapters": b.cached_chapters,
        "content_bytes": b.content_bytes,
        "content_mb": mb(b.content_bytes),
        "image_bytes": b.image_bytes,
        "image_mb": mb(b.image_bytes),
        // 下载进度用百分比表示，目录未知时为 null（前端显示「未知」）
        "download_percent": if b.total_chapter_num > 0 {
            Some((b.cached_chapters * 100 / b.total_chapter_num).min(100))
        } else {
            None
        },
    })
}

// ---------------------------------------------------------------------------
// 单书元数据（OPF 语义字段）
// ---------------------------------------------------------------------------

/// 元数据的长度上限：只防滥用，不追求完整校验（语言/日期允许书写源原样值）。
const META_MAX_TEXT: usize = 200;
const META_MAX_INTRO: usize = 4000;
const META_MAX_LANGUAGE: usize = 35; // BCP 47 上限
const META_MAX_TAGS: usize = 64;
const META_MAX_TAG_LEN: usize = 64;
const META_MAX_IDS: usize = 32;
const META_MAX_SCHEME_LEN: usize = 32;
const META_MAX_ID_VALUE_LEN: usize = 128;

#[derive(Deserialize)]
pub struct MetaQ {
    #[serde(alias = "book_id")]
    pub book_url: String,
}

#[derive(Deserialize)]
pub struct IdentifierReq {
    pub scheme: String,
    pub value: String,
}

/// 写入请求：缺省字段 = 保持不变；文本传空串 = 清空；数组传 `[]` = 清空该项。
#[derive(Deserialize, Default)]
pub struct MetaPatchReq {
    #[serde(alias = "book_id")]
    pub book_url: String,
    /// 书名 / 作者 / 简介：管理员编辑馆藏内容时也要能改。
    pub name: Option<String>,
    pub author: Option<String>,
    pub intro: Option<String>,
    pub language: Option<String>,
    pub publisher: Option<String>,
    pub pubdate: Option<String>,
    pub series_name: Option<String>,
    pub series_index: Option<f64>,
    pub identifiers: Option<Vec<IdentifierReq>>,
    pub tags: Option<Vec<String>>,
}

fn check_meta_len(label: &str, value: &str, max: usize) -> AppResult<()> {
    if value.chars().count() > max {
        return Err(AppError::BadRequest(format!(
            "{label} 过长（最多 {max} 个字符）"
        )));
    }
    Ok(())
}

/// 把请求体校验后转成领域补丁；非法值直接 400，不落库。
fn to_metadata_patch(req: MetaPatchReq) -> AppResult<library::MetadataPatch> {
    for (label, value, max) in [
        ("name", req.name.as_deref(), META_MAX_TEXT),
        ("author", req.author.as_deref(), META_MAX_TEXT),
        ("intro", req.intro.as_deref(), META_MAX_INTRO),
        ("language", req.language.as_deref(), META_MAX_LANGUAGE),
        ("publisher", req.publisher.as_deref(), META_MAX_TEXT),
        ("pubdate", req.pubdate.as_deref(), META_MAX_TEXT),
        ("series_name", req.series_name.as_deref(), META_MAX_TEXT),
    ] {
        if let Some(v) = value {
            check_meta_len(label, v, max)?;
        }
    }
    if let Some(idx) = req.series_index {
        if !idx.is_finite() || idx < 0.0 {
            return Err(AppError::BadRequest(
                "series_index 必须是 >= 0 的有限数".into(),
            ));
        }
    }
    // 书名是 OPF 的必填项：允许不改，但不允许清空。
    if req.name.as_deref().is_some_and(|v| v.trim().is_empty()) {
        return Err(AppError::BadRequest("书名不能为空".into()));
    }

    let identifiers = match req.identifiers {
        Some(list) => {
            if list.len() > META_MAX_IDS {
                return Err(AppError::BadRequest(format!(
                    "identifiers 最多 {META_MAX_IDS} 条"
                )));
            }
            let mut out = Vec::with_capacity(list.len());
            for id in list {
                let scheme = id.scheme.trim();
                let value = id.value.trim();
                if scheme.is_empty() || value.is_empty() {
                    return Err(AppError::BadRequest(
                        "identifier 的 scheme/value 不能为空".into(),
                    ));
                }
                check_meta_len("identifier.scheme", scheme, META_MAX_SCHEME_LEN)?;
                check_meta_len("identifier.value", value, META_MAX_ID_VALUE_LEN)?;
                out.push((scheme.to_lowercase(), value.to_string()));
            }
            Some(out)
        }
        None => None,
    };

    let tags = match req.tags {
        Some(list) => {
            if list.len() > META_MAX_TAGS {
                return Err(AppError::BadRequest(format!(
                    "tags 最多 {META_MAX_TAGS} 个"
                )));
            }
            let mut out = Vec::with_capacity(list.len());
            for tag in list {
                let tag = tag.trim();
                if tag.is_empty() {
                    continue;
                }
                check_meta_len("tag", tag, META_MAX_TAG_LEN)?;
                out.push(tag.to_string());
            }
            Some(out)
        }
        None => None,
    };

    Ok(library::MetadataPatch {
        name: req.name,
        author: req.author,
        intro: req.intro,
        language: req.language,
        publisher: req.publisher,
        pubdate: req.pubdate,
        series_name: req.series_name,
        series_index: req.series_index,
        identifiers,
        tags,
    })
}

fn metadata_json(b: &LibraryBook, extras: &library::MetadataExtras) -> serde_json::Value {
    json!({
        "book_url": b.book_url,
        "name": b.name,
        "author": b.author,
        "intro": b.intro,
        "tier": b.tier,
        "language": b.language,
        "publisher": b.publisher,
        "pubdate": b.pubdate,
        "series_name": b.series_name,
        "series_index": b.series_index,
        "identifiers": extras
            .identifiers
            .iter()
            .map(|(scheme, value)| json!({ "scheme": scheme, "value": value }))
            .collect::<Vec<_>>(),
        "tags": extras.tags,
    })
}

/// 读一本书的元数据。登录即可读（馆藏全局共享）。
async fn book_meta(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<MetaQ>,
) -> AppResult<Json<serde_json::Value>> {
    let book_url = q.book_url.trim();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    let book = library::load_book(&s.library, book_url)
        .await?
        .ok_or_else(|| AppError::NotFound("馆藏里没有这本书".into()))?;
    let extras = library::load_metadata_extras(&s.library, book_url).await?;
    Ok(Json(json!({
        "ok": true,
        "meta": metadata_json(&book, &extras),
    })))
}

fn patch_to_fields(patch: &library::MetadataPatch) -> crate::library_store::edit::MetaFields {
    use crate::library_store::edit::MetaFields;
    // identifiers 被整体替换时，没有 isbn 就等于清空；没带该字段时才保持原值。
    let isbn = patch.identifiers.as_ref().map(|list| {
        list.iter()
            .find(|(scheme, _)| scheme.eq_ignore_ascii_case("isbn"))
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    });
    MetaFields {
        title: patch.name.clone(),
        author: patch.author.clone(),
        description: patch.intro.clone(),
        language: patch.language.clone(),
        publisher: patch.publisher.clone(),
        pubdate: patch.pubdate.clone(),
        subjects: patch.tags.clone(),
        series: patch.series_name.clone(),
        series_index: patch.series_index,
        isbn,
    }
}

/// 写一本书的元数据。仅管理员：馆藏是全局共享内容，普通账号只能改自己的书架字段。
///
/// 目录是权威：DB 更新后同步写 `content.opf` / `oread.json`；目录尚未建立时只写 DB，
/// 等首次目录同步再建目录。
async fn set_book_meta(
    State(s): State<AppState>,
    u: AuthUser,
    Json(req): Json<MetaPatchReq>,
) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&u)?;
    let book_url = req.book_url.trim().to_string();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    let patch = to_metadata_patch(req)?;
    if patch.is_empty() {
        return Err(AppError::BadRequest("没有可更新的字段".into()));
    }
    library::update_metadata(&s.library, &book_url, &patch).await?;
    let fields = patch_to_fields(&patch);
    let store = s.store();
    if let Err(e) = crate::library_store::edit::save_text_meta(&store, &book_url, &fields).await {
        match e {
            crate::library_store::edit::EditError::NotFound(_) => {
                match crate::library_store::edit::save_comic_meta(&store, &book_url, &fields).await
                {
                    Ok(_) => {}
                    Err(crate::library_store::edit::EditError::NotFound(_)) => {}
                    Err(e) => return Err(super::book_edit::edit_error(e)),
                }
            }
            e => return Err(super::book_edit::edit_error(e)),
        }
    }
    let book = library::load_book(&s.library, &book_url)
        .await?
        .ok_or_else(|| AppError::NotFound("馆藏里没有这本书".into()))?;
    let extras = library::load_metadata_extras(&s.library, &book_url).await?;
    Ok(Json(json!({
        "ok": true,
        "meta": metadata_json(&book, &extras),
    })))
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
    let (ts, after_url) = parse_cursor(q.cursor.as_deref())?;
    let term = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let like = term.map(|s| format!("%{}%", s.replace('%', "\\%").replace('_', "\\_")));

    // index.db 扩展搜索：除书名/作者外还匹配系列、主题与简介；索引不可用时
    // 只退回 library.db 的 name/author，不影响旧书（尚未建目录）被搜到。
    let mut index_urls: Vec<String> = Vec::new();
    if let Some(term) = term {
        let store = s.store();
        match crate::library_store::index::shared(store.root()).await {
            Ok(index) => match index.search(term, 2000).await {
                Ok(urls) => index_urls = urls,
                Err(e) => tracing::warn!(error = %e, "index.db search failed"),
            },
            Err(e) => tracing::warn!(error = %e, "index.db open failed; search falls back to db"),
        }
    }
    let index_filter = if index_urls.is_empty() {
        String::new()
    } else {
        let placeholders = (6..6 + index_urls.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        format!(" OR book_url IN ({placeholders})")
    };

    let sql = format!(
        "SELECT * FROM library_books \
         WHERE (?1 = 'all' OR tier = ?1) \
           AND (?2 = '' OR name LIKE ?2 ESCAPE '\\' OR author LIKE ?2 ESCAPE '\\'{index_filter}) \
           AND ({col} < ?3 OR ({col} = ?3 AND book_url > ?4)) \
         ORDER BY {col} DESC, book_url ASC LIMIT ?5",
        col = sort_expr
    );
    let mut query = sqlx::query_as(&sql)
        .bind(tier_filter)
        .bind(like.clone().unwrap_or_default())
        .bind(ts)
        .bind(&after_url)
        .bind(limit + 1);
    for url in &index_urls {
        query = query.bind(url);
    }
    let rows: Vec<LibraryBook> = query.fetch_all(&s.library).await?;
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

/// 体积预检结果缓存：30 分钟，键含 uid（登录态书源预检用的凭证按账号不同）。
static ESTIMATE_CACHE: OnceLock<Mutex<HashMap<String, (Instant, serde_json::Value)>>> =
    OnceLock::new();
const ESTIMATE_TTL: Duration = Duration::from_secs(30 * 60);
/// 抽样 3 章 × 2 张图：再多是预检本身就把源打爆了。
const ESTIMATE_CHAPTER_SAMPLES: usize = 3;
const ESTIMATE_IMAGE_SAMPLES: usize = 2;

fn estimate_cache() -> &'static Mutex<HashMap<String, (Instant, serde_json::Value)>> {
    ESTIMATE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 在 `[0, len)` 里均匀取至多 `max` 个不重复下标（包含首尾）。
fn sample_indices(len: usize, max: usize) -> Vec<usize> {
    if len == 0 {
        return Vec::new();
    }
    if len <= max || max <= 1 {
        return (0..len.min(max.max(1))).collect();
    }
    (0..max)
        .map(|i| i * (len - 1) / (max - 1))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// 漫画入馆前的体积预估（管理员）：抽 3 章 × 2 张图，给出「预计占 X～Y GB」。
///
/// 只用预检管理员的书源登录态，与采购单 `job.uid` 一致；结果缓存 30 分钟。
/// 只探测响应头（优先 `Range: bytes=0-0`），不下载整图；估算标注「以实际下载为准」。
async fn comic_estimate(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<BookReq>,
) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&u)?;
    let book_url = q.book_url.trim().to_string();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    let book = library::load_book(&s.library, &book_url).await?;
    let source_id = q
        .source
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| book.as_ref().and_then(|b| b.origin.clone()))
        .unwrap_or_default();
    if source_id.is_empty() {
        return Err(AppError::BadRequest(
            "缺少 source：无法确定书源登录态".into(),
        ));
    }

    let cache_key = format!("{book_url}|{source_id}|{}", u.id);
    {
        let cache = estimate_cache();
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, value)) = guard.get(&cache_key) {
            if at.elapsed() < ESTIMATE_TTL {
                return Ok(Json(value.clone()));
            }
        }
    }

    let source = crate::routes::sources::load_source(&s.shared, &source_id)
        .await
        .map_err(|e| AppError::BadRequest(format!("书源不可用：{e}")))?;
    if source.book_source_type != 2 {
        return Ok(Json(json!({
            "ok": true,
            "comic": false,
            "book_url": book_url,
            "note": "该书不是漫画，无需图片体积预估",
        })));
    }

    // 与 worker 一致：借书证里的书源登录态，保证 VIP/登录图也能探到真实大小。
    let user_pool = crate::auth::get_user_pool(&s, u.id).await?;
    let store_key = crate::web_book::book_key(&source, &book_url);
    let store = crate::routes::books::book_store_for(&user_pool, &store_key).await;

    let chapters =
        estimate_chapters(&s, &source, &user_pool, &store, &book_url, book.as_ref()).await?;
    if chapters.is_empty() {
        return Err(AppError::BadRequest("目录为空，无法预估体积".into()));
    }

    let picks = sample_indices(chapters.len(), ESTIMATE_CHAPTER_SAMPLES);
    let mut images_per_chapter: Vec<usize> = Vec::with_capacity(picks.len());
    let mut probes: Vec<(String, String)> = Vec::new();
    for &idx in &picks {
        let ch = &chapters[idx];
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT images FROM library_content \
             WHERE url = ?1 AND origin = ?2 AND content_type = 'image'",
        )
        .bind(&ch.url)
        .bind(&source.book_source_url)
        .fetch_optional(&s.library)
        .await?;
        let imgs = match existing
            .as_deref()
            .and_then(|raw| serde_json::from_str::<crate::web_book::ChapterImages>(raw).ok())
        {
            Some(v) if !v.images.is_empty() => v,
            _ => match crate::web_book::chapter_images_book(
                s.fetcher.as_ref(),
                &source,
                ch,
                Some(&store),
            )
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(url = %ch.url, error = %e, "comic estimate: chapter images failed");
                    continue;
                }
            },
        };
        if imgs.images.is_empty() {
            continue;
        }
        images_per_chapter.push(imgs.images.len());
        for i in sample_indices(imgs.images.len(), ESTIMATE_IMAGE_SAMPLES) {
            let url = imgs.images[i].url.trim();
            if !url.is_empty() {
                probes.push((ch.url.clone(), url.to_string()));
            }
        }
    }
    let _ = crate::routes::books::persist_book_changes(&user_pool, store.take_changes()).await;

    let sizes: Vec<Option<u64>> =
        futures::stream::iter(probes.into_iter().map(|(chapter_url, url)| {
            let src = source.clone();
            async move {
                image_fetch::probe_size(&src, &url, Some(&chapter_url))
                    .await
                    .ok()
                    .flatten()
            }
        }))
        .buffer_unordered(4)
        .collect()
        .await;
    let known: Vec<u64> = sizes.into_iter().flatten().collect();

    let sampled_chapters = images_per_chapter.len() as i64;
    let avg_images = if images_per_chapter.is_empty() {
        None
    } else {
        Some(images_per_chapter.iter().sum::<usize>() as f64 / images_per_chapter.len() as f64)
    };
    let avg_bytes = if known.is_empty() {
        None
    } else {
        Some(known.iter().sum::<u64>() as f64 / known.len() as f64)
    };
    let total = chapters.len() as f64;
    let estimate = match (avg_images, avg_bytes) {
        (Some(i), Some(b)) => Some((i * b * total).round() as i64),
        _ => None,
    };
    let low = estimate.map(|v| (v as f64 * 0.7).round() as i64);
    let high = estimate.map(|v| (v as f64 * 1.4).round() as i64);
    let cached_chapters: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM library_content WHERE book_url = ?1 \
         AND content_type = 'image' AND images_ready = 1",
    )
    .bind(&book_url)
    .fetch_one(&s.library)
    .await?;
    let free = library::free_bytes(std::path::Path::new(&s.cfg.data_dir));
    let floor = s.cfg.library_disk_floor_bytes;
    let enough = match (high, free) {
        (Some(h), Some(f)) => Some(f.saturating_sub(floor) >= u64::try_from(h).unwrap_or(u64::MAX)),
        _ => None,
    };

    let value = json!({
        "ok": true,
        "comic": true,
        "book_url": book_url,
        "source": source_id,
        "total_chapters": chapters.len(),
        "sampled_chapters": sampled_chapters,
        "sampled_images": known.len(),
        "avg_images_per_chapter": avg_images,
        "avg_image_bytes": avg_bytes.map(|b| b.round() as i64),
        "estimated_bytes": estimate,
        "estimated_low_bytes": low,
        "estimated_high_bytes": high,
        "estimated_mb": estimate.map(mb),
        "estimated_low_mb": low.map(mb),
        "estimated_high_mb": high.map(mb),
        "cached_chapters": cached_chapters,
        "disk_free_bytes": free,
        "disk_free_mb": free.map(|b| b / (1024 * 1024)),
        "disk_floor_bytes": floor,
        "enough_space": enough,
        "note": "抽样估算，以实际下载为准",
    });
    {
        let cache = estimate_cache();
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() > 128 {
            guard.clear();
        }
        guard.insert(cache_key, (Instant::now(), value.clone()));
    }
    Ok(Json(value))
}

/// 预估用的目录：优先读馆藏，没有就抓一次（与 worker 同一口径）。
async fn estimate_chapters(
    s: &AppState,
    source: &crate::entities::BookSource,
    user_pool: &sqlx::SqlitePool,
    store: &crate::web_book::BookStore,
    book_url: &str,
    meta: Option<&LibraryBook>,
) -> AppResult<Vec<crate::entities::BookChapter>> {
    let rows: Vec<crate::entities::BookChapter> = sqlx::query_as(
        "SELECT url, title, is_volume, base_url, book_url, \"index\", is_vip, is_pay, \
         resource_url, tag, start, \"end\", update_time, word_count FROM library_chapters \
         WHERE book_url = ?1 ORDER BY \"index\"",
    )
    .bind(book_url)
    .fetch_all(&s.library)
    .await?;
    if !rows.is_empty() {
        return Ok(rows.into_iter().filter(|c| !c.is_volume).collect());
    }
    let book = Book {
        book_url: book_url.to_string(),
        toc_url: meta.and_then(|m| m.toc_url.clone()),
        origin: Some(source.book_source_url.clone()),
        origin_name: Some(source.book_source_name.clone()),
        book_source_type: source.book_source_type,
        name: meta.and_then(|m| m.name.clone()),
        author: meta.and_then(|m| m.author.clone()),
        ..Default::default()
    };
    let chapters =
        crate::web_book::chapter_list_book(s.fetcher.as_ref(), source, &book, Some(store))
            .await
            .map_err(|e| AppError::BadRequest(format!("目录抓取失败：{e}")))?;
    if chapters.is_empty() {
        return Ok(Vec::new());
    }
    let non_volume: Vec<crate::entities::BookChapter> =
        chapters.iter().filter(|c| !c.is_volume).cloned().collect();
    let _ = crate::routes::books::save_chapters(&s.library, book_url, &chapters).await;
    let _ = crate::routes::books::sync_total_chapter_num(
        &s.library,
        user_pool,
        book_url,
        chapters.len() as i64,
    )
    .await;
    Ok(non_volume)
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
    // 目录即书：整目录搬进 collection/。搬不动（Windows 占用等）就留标记，
    // 后台维护循环重试；DB 已是 collection，读路径不受影响。
    if let Err(e) = s.store().promote(&book_url).await {
        tracing::warn!(book = %book_url, error = %e, "promote book dir failed; retry later");
        if let Err(e2) = s.store().mark_promote_pending(&book_url).await {
            tracing::warn!(book = %book_url, error = %e2, "write promote marker failed");
        }
    }
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
    // 先搬目录再改 DB：搬不动就保持原状，避免「DB 说临时、目录还在馆藏」。
    let demoted = s.store().demote(book_url).await?;
    let changed = library::release(&s.library, book_url).await?;
    Ok(Json(json!({
        "ok": true,
        "book_url": book_url,
        "released": changed,
        "moved_to_temp": demoted.is_some(),
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
    // 目录先移进回收站：即使随后 DB 删除失败，正文也还在，可人工恢复。
    let trashed = s.store().move_to_trash(book_url).await?;
    let report = library::delete_book(&s.library, book_url).await?;
    Ok(Json(json!({
        "ok": true,
        "book_url": book_url,
        "trashed": trashed,
        "deleted_books": report.books,
        "deleted_chapters": report.chapters,
        "deleted_contents": report.contents,
        "deleted_images": report.images,
        "bytes_freed": report.bytes,
    })))
}

/// 立即回收过期临时缓存（不等定期任务）。仅管理员。
async fn clean_temp(State(s): State<AppState>, u: AuthUser) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&u)?;
    let outcome = library::cleanup_temp_books(&s.library, s.cfg.library_temp_ttl_secs).await?;
    // DB 行删掉后，书目录也要移进回收站，否则磁盘上留下无主目录。
    let mut trashed = 0u64;
    for book_url in &outcome.books {
        match s.store().move_to_trash(book_url).await {
            Ok(true) => trashed += 1,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(book = %book_url, error = %e, "move expired book dir to trash failed")
            }
        }
    }
    let report = outcome.report;
    Ok(Json(json!({
        "ok": true,
        "books": report.books,
        "trashed": trashed,
        "chapters": report.chapters,
        "contents": report.contents,
        "images": report.images,
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
    const JOB_STATUSES: [&str; 6] = [
        library::JOB_QUEUED,
        library::JOB_RUNNING,
        library::JOB_DONE,
        library::JOB_PARTIAL,
        library::JOB_FAILED,
        library::JOB_CANCELLED,
    ];
    if let Some(bad) = statuses
        .iter()
        .find(|s| !JOB_STATUSES.contains(&s.as_str()))
    {
        return Err(AppError::BadRequest(format!(
            "未知任务状态 {bad}；可选 queued/running/done/partial/failed/cancelled"
        )));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_sampling_is_even_and_bounded() {
        assert!(sample_indices(0, 3).is_empty());
        assert_eq!(sample_indices(2, 3), vec![0, 1]);
        assert_eq!(sample_indices(10, 3), vec![0, 4, 9]);
    }

    fn meta_req(idx: Option<f64>, tags: Option<Vec<&str>>) -> MetaPatchReq {
        MetaPatchReq {
            book_url: "b".into(),
            language: Some(" zh ".into()),
            series_index: idx,
            tags: tags.map(|v| v.into_iter().map(str::to_string).collect()),
            ..Default::default()
        }
    }

    #[test]
    fn metadata_patch_normalizes_identifiers_and_tags() {
        let req = MetaPatchReq {
            identifiers: Some(vec![
                IdentifierReq {
                    scheme: " ISBN ".into(),
                    value: " 9787532754687 ".into(),
                },
                IdentifierReq {
                    scheme: "SOURCE".into(),
                    value: "abc".into(),
                },
            ]),
            ..meta_req(Some(1.0), Some(vec![" 科幻 ", "", "经典"]))
        };
        let p = to_metadata_patch(req).unwrap();
        assert_eq!(
            p.language.as_deref(),
            Some(" zh "),
            "文本原样保留，落库时再 trim"
        );
        assert_eq!(
            p.identifiers.unwrap(),
            vec![
                ("isbn".to_string(), "9787532754687".to_string()),
                ("source".to_string(), "abc".to_string()),
            ]
        );
        assert_eq!(
            p.tags.unwrap(),
            vec!["科幻".to_string(), "经典".to_string()]
        );
    }

    #[test]
    fn metadata_patch_rejects_bad_values() {
        // 负数 / NaN 的系列序号
        assert!(to_metadata_patch(meta_req(Some(-1.0), None)).is_err());
        assert!(to_metadata_patch(meta_req(Some(f64::NAN), None)).is_err());
        // 语言超过 BCP 47 上限
        let long = MetaPatchReq {
            language: Some("x".repeat(META_MAX_LANGUAGE + 1)),
            ..Default::default()
        };
        assert!(to_metadata_patch(long).is_err());
        // 空 scheme
        let bad_id = MetaPatchReq {
            identifiers: Some(vec![IdentifierReq {
                scheme: "  ".into(),
                value: "x".into(),
            }]),
            ..Default::default()
        };
        assert!(to_metadata_patch(bad_id).is_err());
    }
}
