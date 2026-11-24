//! 漫画阅读接口：`GET /api/comic/source` 与 `GET /api/comic/chapter`
//!
//! 章节图片列表优先读「目录即书」的 `oread.json` 页表；目录缺失或本章尚未缓存时
//! 按书源规则回源，单张图片走既有 `/api/book/image` 代理（内存 LRU + 5 MiB/15s
//! HTTP 预算）。不再写任何旧 `library_content` / `library_images` 缓存。

use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;

use crate::auth::{AuthUser, UserDb};
use crate::entities::{BookChapter, BookImage, BookSource};
use crate::error::{AppError, AppResult};
use crate::library_store::{comic, Store};
use crate::routes::books::load_book_meta;
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
    /// 章节地址；缺省时按 `index` 从 `library_chapters` 取
    pub chapter_url: Option<String>,
    /// 1-based 目录序号（与 `/api/book/toc` 返回数组顺序一致）
    pub index: Option<i64>,
}

fn clean(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

/// 书源 id 解析：显式参数优先，其次书籍元数据里的 origin。
async fn resolve_source_id(
    lib: &SqlitePool,
    pool: &SqlitePool,
    source: Option<&str>,
    book_url: Option<&str>,
) -> AppResult<String> {
    if let Some(s) = clean(source) {
        return Ok(s.to_string());
    }
    if let Some(bu) = clean(book_url) {
        if let Some(meta) = load_book_meta(lib, pool, bu).await? {
            if let Some(origin) = clean(meta.origin.as_deref()) {
                return Ok(origin.to_string());
            }
        }
    }
    Err(AppError::BadRequest(
        "缺少书源（source/book_url）：请从搜索结果或书架重新打开".into(),
    ))
}

const CHAPTER_SQL: &str =
    "SELECT url, title, is_volume, base_url, book_url, \"index\", is_vip, is_pay, \
     resource_url, tag, start, \"end\", update_time, word_count FROM library_chapters";

/// 解析目标章节：优先 `chapter_url`；否则按 1-based `index` 取目录行。
async fn resolve_chapter(
    lib: &SqlitePool,
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
                .fetch_optional(lib)
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
    .fetch_optional(lib)
    .await?;
    let c = row.ok_or_else(|| AppError::NotFound("目录尚未缓存或序号越界，请先打开目录".into()))?;
    if c.is_volume {
        return Err(AppError::BadRequest(
            "该序号是分卷标题，请从目录选择章节".into(),
        ));
    }
    Ok(c)
}

/// 章节目录序号（1-based）与总章节数；用于响应里的「第 n 章 / 共 m 章」。
async fn chapter_position(lib: &SqlitePool, book_url: &str, chapter: &BookChapter) -> (i64, i64) {
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM library_chapters WHERE book_url = ?1")
            .bind(book_url)
            .fetch_one(lib)
            .await
            .unwrap_or(0);
    let ordinal = if chapter.index >= 0 {
        chapter.index + 1
    } else {
        0
    };
    (ordinal, total)
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
    // 目录即书：漫画目录存在时不需要书源也能进入漫画阅读器。
    if let Some(book_url) = clean(q.book_url.as_deref()) {
        if matches!(s.store().open_comic(book_url).await, Ok(Some(_))) {
            return Ok(Json(json!({
                "source": clean(q.source.as_deref()).unwrap_or(""),
                "source_name": "馆藏漫画",
                "type": 2,
                "type_name": crate::entities::source_type_name(2),
                "content_type": "image",
                "comic": true,
                "supported": true,
                "local": true,
            })));
        }
    }
    let id = resolve_source_id(
        &s.library,
        &db.0,
        q.source.as_deref(),
        q.book_url.as_deref(),
    )
    .await?;
    let src = load_source(&s.shared, &id).await?;
    Ok(Json(source_json(&src)))
}

/// `GET /api/comic/chapter?source=&book_url=&chapter_url=&index=`
///
/// 返回该章的有序图片 URL 列表；目录命中时 `cached=true`（本地文件，离线可读）。
async fn chapter_images(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<ChapterQ>,
) -> AppResult<Json<serde_json::Value>> {
    let book_url = clean(q.book_url.as_deref())
        .ok_or_else(|| AppError::BadRequest("缺少 book_url".into()))?
        .to_string();
    // 目录即书：漫画目录命中时直接返回本地图片 URL，不回源。
    if let Some(value) =
        comic_chapter_from_dir(&s, &book_url, q.chapter_url.as_deref(), q.index).await?
    {
        return Ok(Json(value));
    }
    let source_id = resolve_source_id(
        &s.library,
        &db.0,
        q.source.as_deref(),
        Some(book_url.as_str()),
    )
    .await?;
    let source = load_source(&s.shared, &source_id).await?;
    if source.book_source_type != 2 {
        return Err(AppError::BadRequest(format!(
            "该书源不是漫画源（type={}），请使用小说阅读器",
            source.book_source_type
        )));
    }
    let chapter = resolve_chapter(&s.library, &book_url, q.chapter_url.as_deref(), q.index).await?;

    // 目录没有这一章的页表：按书源规则回源，不写任何缓存表。
    let parsed = web_book::chapter_images(&source, &chapter)
        .await
        .map_err(fetch_error)?;
    let images = parsed.images;
    let truncated = parsed.truncated;

    let (ordinal, total) = chapter_position(&s.library, &book_url, &chapter).await;
    let placeholder_suspected = !images.is_empty() && (chapter.is_pay || chapter.is_vip);
    // 每张图附 `slices`：>1 表示该图超长条页、需要按屏切片翻页；0 表示整图一屏。
    // 只按源页面给的 width/height 预判（未知尺寸按 1 屏处理，交给 `/k` 打开时再算）。
    // 切片开关关闭时恒为 0，前端按整图一页展示。
    let slice_params = image::SliceParams::from_config(&s.cfg, None, None, None);
    let images_out: Vec<serde_json::Value> = images
        .iter()
        .map(|im| {
            let n = match (im.width, im.height) {
                (Some(w), Some(h)) if w > 0 && h > 0 && s.cfg.comic_slice => {
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
        "cached": false,
        "placeholder_suspected": placeholder_suspected,
        // 单图代理路径：前端按此拼 `?source=&url=&chapter_url=`
        "image_proxy": "/api/book/image",
    })))
}

/// 目录即书的漫画章节：从 `oread.json` 取页表，返回 `oread-img://` 本地 URL。
/// 目录不存在/章节不在目录里返回 `None`，调用方继续按书源回源。
async fn comic_chapter_from_dir(
    s: &AppState,
    book_url: &str,
    chapter_url: Option<&str>,
    index: Option<i64>,
) -> AppResult<Option<serde_json::Value>> {
    let opened = match s.store().open_comic(book_url).await {
        Ok(Some(v)) => v,
        Ok(None) => return Ok(None),
        Err(err) => {
            tracing::warn!(book_url, error = %err, "open comic dir failed; falling back");
            return Ok(None);
        }
    };
    let pos = match clean(chapter_url) {
        Some(cu) => {
            if cu.starts_with("volume:") {
                return Err(AppError::BadRequest(
                    "该条为分卷标题，请从目录选择章节".into(),
                ));
            }
            let Some(pos) = opened.meta.chapters.iter().position(|c| {
                c.url.as_deref().map(str::trim) == Some(cu)
                    || crate::ebook_source::local_chapter_id(book_url, cu)
                        .map(|id| id == c.id)
                        .unwrap_or(false)
            }) else {
                return Ok(None);
            };
            pos
        }
        None => {
            let idx = index.unwrap_or(1).max(1) - 1;
            let Ok(idx) = usize::try_from(idx) else {
                return Ok(None);
            };
            if idx >= opened.meta.chapters.len() {
                return Ok(None);
            }
            idx
        }
    };
    let chapter = &opened.meta.chapters[pos];
    if chapter.is_volume {
        return Err(AppError::BadRequest(
            "该序号是分卷标题，请从目录选择章节".into(),
        ));
    }
    // 本地页表为空 = 这一章还没缓存到目录：回源，而不是返回空图片列表。
    if chapter.pages.is_empty() {
        return Ok(None);
    }

    let book_id = Store::book_id(book_url);
    let mut images = Vec::with_capacity(chapter.pages.len());
    for (i, page) in chapter.pages.iter().enumerate() {
        let mut width = page.width.filter(|v| *v > 0);
        let mut height = page.height.filter(|v| *v > 0);
        // 边车缺尺寸时只读本地图片头补齐，切片预判与 KPW3 一屏一页都依赖它。
        if width.is_none() || height.is_none() {
            if let Some((w, h)) = comic::page_dims_async(opened.dir.join(&page.href)).await {
                width = Some(w);
                height = Some(h);
            }
        }
        images.push(BookImage {
            index: i as i64 + 1,
            url: comic::local_image_url_v(&book_id, &page.href, opened.meta.revision),
            width,
            height,
            bytes: page.bytes,
            priority: Some(if i < 3 { "high" } else { "normal" }.into()),
        });
    }

    let slice_params = image::SliceParams::from_config(&s.cfg, None, None, None);
    let images_out: Vec<serde_json::Value> = images
        .iter()
        .map(|im| {
            let n = match (im.width, im.height) {
                (Some(w), Some(h)) if w > 0 && h > 0 && s.cfg.comic_slice => {
                    image::slice_count_window(w, h, &slice_params)
                }
                _ => 1,
            };
            let mut v = serde_json::to_value(im).unwrap_or_else(|_| json!({}));
            v["slices"] = json!(if n > 1 { n } else { 0 });
            v
        })
        .collect();
    let chapter_ref = chapter
        .url
        .clone()
        .unwrap_or_else(|| crate::ebook_source::local_chapter_url(book_url, &chapter.id));
    Ok(Some(json!({
        "book_url": book_url,
        "source": "",
        "source_name": "馆藏漫画",
        "source_type": 2,
        "chapter_url": chapter_ref,
        "chapter_title": chapter.title,
        "chapter_index": pos + 1,
        "chapter_count": opened.meta.chapters.len(),
        "content_type": "image",
        "images": images_out,
        "count": images.len(),
        "truncated": false,
        "cached": true,
        "local": true,
        "placeholder_suspected": false,
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

#[allow(clippy::field_reassign_with_default)] // 测试里按字段逐个构造对象，便于对照默认值。
#[cfg(test)]
mod tests {
    use super::*;

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
