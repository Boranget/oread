//! 管理员编辑：读编辑视图、改正文段落、换封面、换漫画单页。
//!
//! 元数据编辑复用 `POST /api/library/book/meta`（那里会同时写 `content.opf` /
//! `oread.json` 与 `library.db`）。这里只放目录内容的编辑接口。

use std::path::Path;

use axum::{
    extract::{DefaultBodyLimit, Query, State},
    routing::{get, post},
    Json, Router,
};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{ensure_admin, AuthUser};
use crate::error::{AppError, AppResult};
use crate::library_store::comic;
use crate::library_store::edit::{self, EditError};
use crate::library_store::{safe_join, CoverItem, OpfMeta, Store};
use crate::routes::AppState;

const MAX_BODY: usize = 16 * 1024 * 1024;
// base64 会把 10 MB 图片撑到 ~13.3 MB，给 JSON 信封留出余量后仍小于 MAX_BODY。
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_PARAGRAPHS: usize = 5000;
const MAX_PARAGRAPH_CHARS: usize = 20_000;
const MAX_TOTAL_CHARS: usize = 2_000_000;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/library/book/edit", get(edit_view))
        .route(
            "/api/library/book/edit/chapter",
            get(load_chapter).post(save_chapter),
        )
        .route("/api/library/book/edit/cover", post(save_cover))
        .route("/api/library/book/edit/page", post(save_page))
        .layer(DefaultBodyLimit::max(MAX_BODY))
}

pub(super) fn edit_error(e: EditError) -> AppError {
    match e {
        EditError::NotFound(m) => AppError::NotFound(m),
        EditError::BadRequest(m) => AppError::BadRequest(m),
        EditError::Conflict(m) => AppError::Conflict(m),
        EditError::Io(e) => AppError::Io(e),
    }
}

#[derive(Deserialize)]
struct EditQ {
    #[serde(alias = "book_id")]
    book_url: String,
}

fn text_meta(meta: &OpfMeta) -> Value {
    json!({
        "title": meta.title,
        "author": meta.author,
        "language": meta.language,
        "publisher": meta.publisher,
        "pubdate": meta.pubdate,
        "description": meta.description,
        "subjects": meta.subjects,
        "series": meta.series,
        "series_index": meta.series_index,
        "isbn": meta.isbn,
    })
}

fn comic_meta(meta: &comic::ComicMeta) -> Value {
    json!({
        "title": meta.title,
        "author": meta.author,
        "language": meta.language,
        "publisher": meta.publisher,
        "pubdate": meta.pubdate,
        "description": meta.description,
        "subjects": meta.subjects,
        "series": meta.series,
        "series_index": meta.series_index,
        "isbn": meta.isbn,
    })
}

async fn cover_view(dir: &Path, cover: &Option<CoverItem>) -> Value {
    let Some(cover) = cover else {
        return Value::Null;
    };
    let data_url = async {
        let path = safe_join(&dir.join("OEBPS"), &cover.href).ok()?;
        let data = tokio::fs::read(path).await.ok()?;
        (data.len() <= 4 * 1024 * 1024).then(|| {
            format!(
                "data:{};base64,{}",
                cover.media_type,
                base64::engine::general_purpose::STANDARD.encode(data)
            )
        })
    }
    .await;
    json!({
        "href": cover.href,
        "media_type": cover.media_type,
        "data_url": data_url,
    })
}

async fn chapter_cached(dir: &Path, href: &str) -> bool {
    let Ok(path) = safe_join(&dir.join("OEBPS"), href) else {
        return false;
    };
    tokio::fs::metadata(path)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false)
}

/// 编辑视图：目录里的权威元数据 + 章节目录（含漫画页数）。
async fn edit_view(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<EditQ>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let book_url = q.book_url.trim();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    let store = s.store();
    if let Some(opened) = store.open_book(book_url).await? {
        let mut chapters = Vec::with_capacity(opened.meta.chapters.len());
        for c in &opened.meta.chapters {
            chapters.push(json!({
                "id": c.id,
                "title": c.title,
                "url": c.url,
                "is_volume": c.is_volume,
                "cached": chapter_cached(&opened.dir, &c.href).await,
            }));
        }
        return Ok(Json(json!({
            "ok": true,
            "kind": "text",
            "book_url": book_url,
            "book_id": Store::book_id(book_url),
            "tier": opened.tier.as_str(),
            "revision": opened.meta.revision,
            "meta": text_meta(&opened.meta),
            "cover": cover_view(&opened.dir, &opened.meta.cover).await,
            "chapters": chapters,
        })));
    }
    if let Some(opened) = store.open_comic(book_url).await? {
        let chapters: Vec<Value> = opened
            .meta
            .chapters
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "title": c.title,
                    "url": c.url,
                    "is_volume": c.is_volume,
                    "pages": c.pages,
                    "cached": !c.pages.is_empty(),
                })
            })
            .collect();
        return Ok(Json(json!({
            "ok": true,
            "kind": "comic",
            "book_url": book_url,
            "book_id": Store::book_id(book_url),
            "tier": opened.tier.as_str(),
            "revision": opened.meta.revision,
            "meta": comic_meta(&opened.meta),
            "cover": Value::Null,
            "chapters": chapters,
        })));
    }
    Err(AppError::NotFound(
        "书目录不存在或尚未缓存；请先打开一次书，或刷新目录".into(),
    ))
}

#[derive(Deserialize)]
struct ChapterQ {
    #[serde(alias = "book_id")]
    book_url: String,
    chapter_url: String,
}

/// 读一章的段落，供编辑器回显。
async fn load_chapter(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<ChapterQ>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let book_url = q.book_url.trim();
    let chapter_url = q.chapter_url.trim();
    if book_url.is_empty() || chapter_url.is_empty() {
        return Err(AppError::BadRequest(
            "book_url 与 chapter_url 不能为空".into(),
        ));
    }
    let Some((meta, item, paragraphs)) =
        edit::chapter_paragraphs(&s.store(), book_url, chapter_url)
            .await
            .map_err(edit_error)?
    else {
        return Err(AppError::NotFound(
            "本章尚未缓存；请先打开一次，或等整本下载完成".into(),
        ));
    };
    Ok(Json(json!({
        "ok": true,
        "revision": meta.revision,
        "title": item.title,
        "paragraphs": paragraphs,
    })))
}

#[derive(Deserialize)]
struct ChapterBody {
    #[serde(alias = "book_id")]
    book_url: String,
    chapter_url: String,
    revision: u64,
    paragraphs: Vec<String>,
}

fn check_paragraphs(paragraphs: &[String]) -> AppResult<()> {
    if paragraphs.len() > MAX_PARAGRAPHS {
        return Err(AppError::BadRequest(format!(
            "段落过多（最多 {MAX_PARAGRAPHS} 段）"
        )));
    }
    let mut total = 0usize;
    for p in paragraphs {
        let chars = p.chars().count();
        if chars > MAX_PARAGRAPH_CHARS {
            return Err(AppError::BadRequest(format!(
                "单段过长（最多 {MAX_PARAGRAPH_CHARS} 字）"
            )));
        }
        total = total.saturating_add(chars);
    }
    if total > MAX_TOTAL_CHARS {
        return Err(AppError::BadRequest("本章正文过长".into()));
    }
    Ok(())
}

/// 保存一章正文。`revision` 不匹配返回 409。
async fn save_chapter(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<ChapterBody>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let book_url = q.book_url.trim();
    let chapter_url = q.chapter_url.trim();
    if book_url.is_empty() || chapter_url.is_empty() {
        return Err(AppError::BadRequest(
            "book_url 与 chapter_url 不能为空".into(),
        ));
    }
    check_paragraphs(&q.paragraphs)?;
    let meta = edit::save_chapter(&s.store(), book_url, chapter_url, q.revision, &q.paragraphs)
        .await
        .map_err(edit_error)?;
    Ok(Json(json!({ "ok": true, "revision": meta.revision })))
}

fn decode_image(data: &str) -> AppResult<Vec<u8>> {
    let raw = data
        .split_once(";base64,")
        .map(|(_, b)| b)
        .unwrap_or(data)
        .trim();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|_| AppError::BadRequest("图片 base64 解码失败".into()))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(AppError::BadRequest(format!(
            "图片超过上限（最多 {} MB）",
            MAX_IMAGE_BYTES / (1024 * 1024)
        )));
    }
    Ok(bytes)
}

#[derive(Deserialize)]
struct CoverBody {
    #[serde(alias = "book_id")]
    book_url: String,
    revision: u64,
    mime: String,
    data: String,
}

/// 换文字书封面：写目录并同步 `library_books.cover_url`。
async fn save_cover(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<CoverBody>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let book_url = q.book_url.trim();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    let data = decode_image(&q.data)?;
    let (meta, cover) = edit::replace_cover(&s.store(), book_url, q.revision, &q.mime, &data)
        .await
        .map_err(edit_error)?;
    // 封面是全局内容：让馆藏列表也显示新封面。个人书架里的封面副本不在这里批量改。
    // URL 带修订号：换封面后图片缓存与 ETag 自动失效。
    let local = comic::local_image_url_v(
        &Store::book_id(book_url),
        &format!("OEBPS/{}", cover.href),
        meta.revision,
    );
    let _ = sqlx::query("UPDATE library_books SET cover_url = ?2 WHERE book_url = ?1")
        .bind(book_url)
        .bind(&local)
        .execute(&s.library)
        .await;
    Ok(Json(json!({
        "ok": true,
        "revision": meta.revision,
        "cover": {
            "href": cover.href,
            "media_type": cover.media_type,
            "url": local,
        },
    })))
}

#[derive(Deserialize)]
struct PageBody {
    #[serde(alias = "book_id")]
    book_url: String,
    chapter_url: String,
    href: String,
    revision: u64,
    mime: String,
    data: String,
}

/// 换漫画单页：保持页序号，只替换文件与页表。
async fn save_page(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<PageBody>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let book_url = q.book_url.trim();
    let chapter_url = q.chapter_url.trim();
    if book_url.is_empty() || chapter_url.is_empty() || q.href.trim().is_empty() {
        return Err(AppError::BadRequest(
            "book_url / chapter_url / href 不能为空".into(),
        ));
    }
    let data = decode_image(&q.data)?;
    let (meta, page) = edit::replace_page(
        &s.store(),
        book_url,
        chapter_url,
        q.href.trim(),
        q.revision,
        &q.mime,
        &data,
    )
    .await
    .map_err(edit_error)?;
    Ok(Json(json!({
        "ok": true,
        "revision": meta.revision,
        "page": {
            "href": page.href,
            "mime": page.mime,
            "width": page.width,
            "height": page.height,
            "bytes": page.bytes,
        },
    })))
}
