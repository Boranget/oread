//! 漫画阅读接口：`GET /api/comic/source` 与 `GET /api/comic/chapter`
//!
//! 章节图片列表复用 `book_chapters`（与小说目录同一张表、同一顺序），
//! 图片缓存复用 `book_content`（`content_type=image`、`images` 列存 JSON），
//! 单张图片继续走既有 `/api/book/image` 代理（内存 LRU + 5 MiB/15s HTTP 预算），
//! 不另造一套缓存或抓取层。

use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;

use crate::auth::{AuthUser, UserDb};
use crate::entities::{BookChapter, BookContent, BookImage, BookSource};
use crate::error::{AppError, AppResult};
use crate::routes::books::{load_book_meta, save_content_cached};
use crate::routes::image;
use crate::routes::sources::load_source;
use crate::routes::AppState;
use crate::web_book;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/comic/source", get(source_type))
        .route("/api/comic/chapter", get(chapter_images))
}

#[derive(Deserialize)]
pub struct SourceQ {
    /// 书源 id（Legado `bookSourceUrl`）
    pub source: Option<String>,
    /// 书籍 URL；source 缺失时用 `book_meta.origin` 兜底
    pub book_url: Option<String>,
}

#[derive(Deserialize)]
pub struct ChapterQ {
    /// 书源 id（Legado `bookSourceUrl`）
    pub source: Option<String>,
    pub book_url: Option<String>,
    /// 章节地址；缺省时按 `index` 从 `book_chapters` 取
    pub chapter_url: Option<String>,
    /// 1-based 目录序号（与 `/api/book/toc` 返回数组顺序一致）
    pub index: Option<i64>,
}

fn clean(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

/// 书源 id 解析：显式参数优先，其次书籍元数据里的 origin。
async fn resolve_source_id(
    pool: &SqlitePool,
    source: Option<&str>,
    book_url: Option<&str>,
) -> AppResult<String> {
    if let Some(s) = clean(source) {
        return Ok(s.to_string());
    }
    if let Some(bu) = clean(book_url) {
        if let Some(meta) = load_book_meta(pool, bu).await? {
            if let Some(origin) = clean(meta.origin.as_deref()) {
                return Ok(origin.to_string());
            }
        }
    }
    Err(AppError::BadRequest(
        "缺少书源（source/book_url）：请从搜索结果或书架重新打开".into(),
    ))
}

const CHAPTER_SQL: &str = "SELECT url, title, is_volume, base_url, book_url, \"index\", is_vip, is_pay, \
     resource_url, tag, start, \"end\", update_time, word_count FROM book_chapters";

/// 解析目标章节：优先 `chapter_url`；否则按 1-based `index` 取目录行。
async fn resolve_chapter(
    pool: &SqlitePool,
    book_url: &str,
    chapter_url: Option<&str>,
    index: Option<i64>,
) -> AppResult<BookChapter> {
    if let Some(cu) = clean(chapter_url) {
        if cu.starts_with("volume:") {
            return Err(AppError::BadRequest(
                "该条为分卷标题，请从目录选择章节".into(),
            ));
        }
        let found: Option<BookChapter> =
            sqlx::query_as::<_, BookChapter>(&format!("{CHAPTER_SQL} WHERE url = ?1 LIMIT 1"))
                .bind(cu)
                .fetch_optional(pool)
                .await?;
        if let Some(c) = found {
            return Ok(c);
        }
        // 目录可能还没缓存：仍允许直接抓取该章节地址（标题由前端传入/章节页解析）
        return Ok(BookChapter {
            url: cu.to_string(),
            book_url: Some(book_url.to_string()),
            ..Default::default()
        });
    }
    let idx = index.unwrap_or(1).max(1) - 1;
    let row: Option<BookChapter> = sqlx::query_as::<_, BookChapter>(&format!(
        "{CHAPTER_SQL} WHERE book_url = ?1 ORDER BY \"index\" LIMIT 1 OFFSET ?2"
    ))
    .bind(book_url)
    .bind(idx)
    .fetch_optional(pool)
    .await?;
    let c = row.ok_or_else(|| {
        AppError::NotFound("目录尚未缓存或序号越界，请先打开目录".into())
    })?;
    if c.is_volume {
        return Err(AppError::BadRequest(
            "该序号是分卷标题，请从目录选择章节".into(),
        ));
    }
    Ok(c)
}

/// 章节目录序号（1-based）与总章节数；用于响应里的「第 n 章 / 共 m 章」。
async fn chapter_position(pool: &SqlitePool, book_url: &str, chapter: &BookChapter) -> (i64, i64) {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_chapters WHERE book_url = ?1")
        .bind(book_url)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let ordinal = if chapter.index >= 0 {
        chapter.index + 1
    } else {
        0
    };
    (ordinal, total)
}

/// 读取章节图片缓存。
///
/// 不复用 `books::load_cached_content`：它的 SELECT 选的是列名 `images`，
/// 而 `BookContent.images_json` 带 `#[sqlx(default)]`，列名对不上时会静默取 `None`，
/// 导致图片章节缓存命中后拿到空列表。这里显式起别名 `images AS images_json`。
pub(crate) async fn load_cached_images(
    pool: &SqlitePool,
    url: &str,
    ttl_secs: i64,
) -> anyhow::Result<Option<BookContent>> {
    let row: Option<BookContent> = sqlx::query_as(
        "SELECT url, book_url, title, content, chapter_index, resource_url, is_local, \
         content_type, images AS images_json \
         FROM book_content WHERE url = ?1 AND updated_at >= datetime('now', ?2)",
    )
    .bind(url)
    .bind(format!("-{ttl_secs} seconds"))
    .fetch_optional(pool)
    .await?;
    if row.is_some() {
        let _ = sqlx::query("UPDATE book_content SET accessed_at = datetime('now') WHERE url = ?")
            .bind(url)
            .execute(pool)
            .await;
    }
    Ok(row)
}

/// 解析缓存里的图片 JSON：兼容 `{images,truncated}` 与旧裸数组两种形态。
pub(crate) fn decode_images(raw: Option<&str>) -> (Vec<BookImage>, bool) {
    let Some(raw) = raw else {
        return (Vec::new(), false);
    };
    if let Ok(v) = serde_json::from_str::<web_book::ChapterImages>(raw) {
        return (v.images, v.truncated);
    }
    if let Ok(list) = serde_json::from_str::<Vec<BookImage>>(raw) {
        return (list, false);
    }
    (Vec::new(), false)
}

fn source_json(src: &BookSource) -> serde_json::Value {
    json!({
        "source": src.book_source_url,
        "source_name": src.book_source_name,
        "type": src.book_source_type,
        "type_name": crate::entities::source_type_name(src.book_source_type),
        "content_type": if src.book_source_type == 2 { "image" } else { "text" },
        "comic": src.book_source_type == 2,
        "supported": crate::entities::is_supported_source_type(src.book_source_type),
    })
}

/// `GET /api/comic/source?source=<id>&book_url=<book>`
/// 轻量探测书源类型，前端据此决定进入小说阅读器还是漫画阅读器。
async fn source_type(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<SourceQ>,
) -> AppResult<Json<serde_json::Value>> {
    let id = resolve_source_id(&db.0, q.source.as_deref(), q.book_url.as_deref()).await?;
    let src = load_source(&s.shared, &id).await?;
    Ok(Json(source_json(&src)))
}

/// `GET /api/comic/chapter?source=&book_url=&chapter_url=&index=`
///
/// 返回该章的有序图片 URL 列表；`cached=true` 表示命中 `book_content` 缓存（未回源）。
async fn chapter_images(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<ChapterQ>,
) -> AppResult<Json<serde_json::Value>> {
    let book_url = clean(q.book_url.as_deref())
        .ok_or_else(|| AppError::BadRequest("缺少 book_url".into()))?
        .to_string();
    let source_id =
        resolve_source_id(&db.0, q.source.as_deref(), Some(book_url.as_str())).await?;
    let source = load_source(&s.shared, &source_id).await?;
    if source.book_source_type != 2 {
        return Err(AppError::BadRequest(format!(
            "该书源不是漫画源（type={}），请使用小说阅读器",
            source.book_source_type
        )));
    }
    let chapter = resolve_chapter(
        &db.0,
        &book_url,
        q.chapter_url.as_deref(),
        q.index,
    )
    .await?;

    // 1) 命中章节缓存：直接返回图片列表，不再回源
    let cached = load_cached_images(&db.0, &chapter.url, s.cfg.content_ttl_secs).await?;
    let (images, truncated, from_cache) = match cached {
        Some(c) if c.content_type == "image" => {
            let (v, t) = decode_images(c.images_json.as_deref());
            (v, t, true)
        }
        _ => {
            let parsed = web_book::chapter_images(&source, &chapter)
                .await
                .map_err(fetch_error)?;
            let images = parsed.images;
            let truncated = parsed.truncated;
            // 空图片列表不回写缓存：腾讯等源的章节数据带随机 nonce，偶发的解析失败会得到 0 张图，
            // 一旦缓存就会在 TTL 内「永久」返回空列表，用户需清缓存才能恢复。空结果按失败处理即可。
            if !images.is_empty() {
                let payload = serde_json::to_string(&web_book::ChapterImages {
                    images: images.clone(),
                    truncated,
                })
                .unwrap_or_default();
                let c = BookContent {
                    url: chapter.url.clone(),
                    book_url: book_url.clone(),
                    title: chapter.title.clone(),
                    content: Some(String::new()),
                    chapter_index: chapter.index,
                    content_type: "image".into(),
                    images_json: Some(payload),
                    ..Default::default()
                };
                // 缓存上限与小说正文同源（用户设置优先，否则全局配置）
                let limit = match crate::routes::replace::user_cache_limit_mb(&db.0).await {
                    Some(mb) => (mb * 1024 * 1024).min(s.cfg.cache_limit_bytes()),
                    None => s.cfg.cache_limit_bytes(),
                };
                if let Err(e) = save_content_cached(&db.0, &c, limit).await {
                    tracing::warn!(url = %chapter.url, error = %e, "comic content cache write failed");
                }
            }
            (images, truncated, false)
        }
    };

    let (ordinal, total) = chapter_position(&db.0, &book_url, &chapter).await;
    let placeholder_suspected = !images.is_empty() && (chapter.is_pay || chapter.is_vip);
    // 每张图附 `slices`：>1 表示该图超长条页、需要按屏切片翻页；0 表示整图一屏。
    // 只按源页面给的 width/height 预判（未知尺寸按 1 屏处理，交给 `/k` 打开时再算）。
    let slice_params = image::SliceParams::default();
    let images_out: Vec<serde_json::Value> = images
        .iter()
        .map(|im| {
            let n = match (im.width, im.height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => {
                    image::slice_count_window(w, h, &slice_params)
                }
                _ => 1,
            };
            let mut v = serde_json::to_value(im).unwrap_or_else(|_| json!({}));
            v["slices"] = json!(if n > 1 { n } else { 0 });
            v
        })
        .collect();
    Ok(Json(json!({
        "book_url": book_url,
        "source": source.book_source_url,
        "source_name": source.book_source_name,
        "source_type": source.book_source_type,
        "chapter_url": chapter.url,
        "chapter_title": chapter.title,
        "chapter_index": ordinal,
        "chapter_count": total,
        "content_type": "image",
        "images": images_out,
        "count": images.len(),
        "truncated": truncated,
        "cached": from_cache,
        "placeholder_suspected": placeholder_suspected,
        // 单图代理路径：前端按此拼 `?source=&url=&chapter_url=`
        "image_proxy": "/api/book/image",
    })))
}

/// 抓取失败分类：超时映射 504，其余映射 400（前端可直接展示中文原因）。
fn fetch_error(e: anyhow::Error) -> AppError {
    let msg = e.to_string();
    if msg.contains("超时") || msg.to_ascii_lowercase().contains("timeout") {
        AppError::Timeout(format!("漫画章节抓取超时：{msg}"))
    } else {
        AppError::BadRequest(format!("漫画章节抓取失败：{msg}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_images_accepts_wrapped_and_legacy_shapes() {
        let wrapped = r#"{"images":[{"index":1,"url":"https://cdn/a.jpg"}],"truncated":true}"#;
        let (imgs, truncated) = decode_images(Some(wrapped));
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].url, "https://cdn/a.jpg");
        assert!(truncated);

        let legacy = r#"[{"index":1,"url":"https://cdn/b.jpg"},{"index":2,"url":"https://cdn/c.jpg"}]"#;
        let (imgs, truncated) = decode_images(Some(legacy));
        assert_eq!(imgs.len(), 2);
        assert!(!truncated);

        assert_eq!(decode_images(None).0.len(), 0);
        assert_eq!(decode_images(Some("not json")).0.len(), 0);
    }

    #[test]
    fn source_json_marks_comic() {
        let mut src = BookSource::default();
        src.book_source_url = "https://comic.example".into();
        src.book_source_name = "漫画源".into();
        src.book_source_type = 2;
        let v = source_json(&src);
        assert_eq!(v["content_type"], "image");
        assert_eq!(v["comic"], true);
        src.book_source_type = 0;
        assert_eq!(source_json(&src)["content_type"], "text");
    }
}
