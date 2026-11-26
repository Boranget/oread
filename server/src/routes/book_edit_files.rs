//! calibre 式目录编辑：文件树、源码读写、跨文件替换、预览与 Check book。
//!
//! 与 [`super::book_edit`] 的 v1 正文编辑并存：v1 只改段落，这里直接改书目录里的
//! 归一化文件。所有写操作都要求 `revision`，由落盘层串行化并推进修订号。

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
use crate::library_store::files::{self, FileContent, ReplaceOptions};
use crate::library_store::Store;
use crate::routes::AppState;

const MAX_BODY: usize = 16 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/library/book/edit/files", get(list_files))
        .route(
            "/api/library/book/edit/file",
            get(read_file).post(write_file),
        )
        .route("/api/library/book/edit/file/image", post(write_image))
        .route("/api/library/book/edit/file/delete", post(delete_file))
        .route("/api/library/book/edit/replace", post(replace_all))
        .route("/api/library/book/edit/check", get(check_book))
        .route("/api/library/book/edit/preview", post(preview_file))
        .layer(DefaultBodyLimit::max(MAX_BODY))
}

fn edit_error(e: crate::library_store::edit::EditError) -> AppError {
    super::book_edit::edit_error(e)
}

fn text_meta(meta: &crate::library_store::OpfMeta) -> Value {
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
        "cover": meta.cover.as_ref().map(|c| c.href.clone()),
    })
}

fn blocks_json(blocks: Vec<boranlib_ebook::html::Block>) -> Value {
    use boranlib_ebook::html::Block;
    Value::Array(
        blocks
            .into_iter()
            .map(|block| match block {
                Block::Heading(level, text) => {
                    json!({"type": "heading", "level": level, "text": text})
                }
                Block::Paragraph(text) => json!({"type": "paragraph", "text": text}),
                Block::List(items) => json!({
                    "type": "list",
                    "items": items
                        .into_iter()
                        .map(|i| json!({"level": i.level, "ordered": i.ordered, "text": i.text}))
                        .collect::<Vec<_>>()
                }),
                Block::Code(text) => json!({"type": "code", "text": text}),
                Block::Image { src, alt } => json!({"type": "image", "src": src, "alt": alt}),
                Block::Table(table) => json!({
                    "type": "table",
                    "caption": table.caption,
                    "rows": table.rows.into_iter().map(|row| json!({
                        "header": row.header,
                        "cells": row.cells.into_iter().map(|cell| json!({
                            "text": cell.text,
                            "colspan": cell.colspan,
                            "rowspan": cell.rowspan,
                            "header": cell.header,
                        })).collect::<Vec<_>>()
                    })).collect::<Vec<_>>()
                }),
                Block::Divider => json!({"type": "divider"}),
                Block::Math { mathml, text } => {
                    json!({"type": "math", "mathml": mathml, "text": text})
                }
            })
            .collect(),
    )
}

#[derive(Deserialize)]
struct BookQ {
    #[serde(alias = "book_id")]
    book_url: String,
}

#[derive(Deserialize)]
struct FileQ {
    #[serde(alias = "book_id")]
    book_url: String,
    path: String,
}

async fn list_files(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<BookQ>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let book_url = q.book_url.trim();
    let (meta, files) = files::list_files(&s.store(), book_url)
        .await
        .map_err(edit_error)?;
    let items: Vec<Value> = files
        .into_iter()
        .map(|f| {
            json!({
                "path": f.path,
                "size": f.size,
                "media_type": f.media_type,
                "kind": f.kind.as_str(),
                "editable": f.editable,
            })
        })
        .collect();
    Ok(Json(json!({
        "ok": true,
        "kind": "text",
        "book_url": book_url,
        "book_id": Store::book_id(book_url),
        "revision": meta.revision,
        "meta": text_meta(&meta),
        "files": items,
    })))
}

async fn read_file(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<FileQ>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let (meta, file) = files::read_file(&s.store(), q.book_url.trim(), &q.path)
        .await
        .map_err(edit_error)?;
    let mut out = json!({
        "ok": true,
        "revision": meta.revision,
        "path": file.path,
        "size": file.size,
        "media_type": file.media_type,
    });
    match file.content {
        FileContent::Text(text) => {
            out["kind"] = json!("text");
            out["text"] = json!(text);
        }
        FileContent::Image(url) => {
            out["kind"] = json!("image");
            out["image_url"] = json!(url);
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct WriteFileBody {
    #[serde(alias = "book_id")]
    book_url: String,
    path: String,
    revision: u64,
    content: String,
}

async fn write_file(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<WriteFileBody>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let out = files::write_text_file(
        &s.store(),
        q.book_url.trim(),
        &q.path,
        q.revision,
        &q.content,
    )
    .await
    .map_err(edit_error)?;
    super::book_edit::refresh_usage(&s, q.book_url.trim()).await;
    Ok(Json(json!({
        "ok": true,
        "revision": out.revision,
        "size": out.size,
    })))
}

#[derive(Deserialize)]
struct WriteImageBody {
    #[serde(alias = "book_id")]
    book_url: String,
    path: String,
    revision: u64,
    data: String,
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

async fn write_image(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<WriteImageBody>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let data = decode_image(&q.data)?;
    let out = files::write_image_file(&s.store(), q.book_url.trim(), &q.path, q.revision, &data)
        .await
        .map_err(edit_error)?;
    super::book_edit::refresh_usage(&s, q.book_url.trim()).await;
    Ok(Json(json!({
        "ok": true,
        "revision": out.revision,
        "size": out.size,
        "image_url": out.image_url,
    })))
}

#[derive(Deserialize)]
struct DeleteBody {
    #[serde(alias = "book_id")]
    book_url: String,
    path: String,
    revision: u64,
}

async fn delete_file(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<DeleteBody>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let revision = files::delete_file(&s.store(), q.book_url.trim(), &q.path, q.revision)
        .await
        .map_err(edit_error)?;
    super::book_edit::refresh_usage(&s, q.book_url.trim()).await;
    Ok(Json(json!({ "ok": true, "revision": revision })))
}

#[derive(Deserialize)]
struct ReplaceBody {
    #[serde(alias = "book_id")]
    book_url: String,
    revision: u64,
    query: String,
    replacement: String,
    #[serde(default)]
    regex: bool,
    #[serde(default = "default_true")]
    case_sensitive: bool,
    paths: Option<Vec<String>>,
    #[serde(default)]
    dry_run: bool,
}

fn default_true() -> bool {
    true
}

async fn replace_all(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<ReplaceBody>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let opts = ReplaceOptions {
        query: &q.query,
        replacement: &q.replacement,
        regex: q.regex,
        case_sensitive: q.case_sensitive,
        paths: q.paths.as_deref(),
        dry_run: q.dry_run,
    };
    let out = files::replace_all(&s.store(), q.book_url.trim(), q.revision, &opts)
        .await
        .map_err(edit_error)?;
    if out.applied {
        super::book_edit::refresh_usage(&s, q.book_url.trim()).await;
    }
    let hits: Vec<Value> = out
        .hits
        .iter()
        .map(|h| {
            json!({
                "path": h.path,
                "line": h.line,
                "text": h.text,
                "matched": h.matched,
                "replacement": h.replacement,
            })
        })
        .collect();
    Ok(Json(json!({
        "ok": true,
        "total": out.total,
        "changed_files": out.changed_files,
        "hits": hits,
        "truncated": out.truncated,
        "applied": out.applied,
        "revision": out.revision,
    })))
}

async fn check_book(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<BookQ>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let report = files::check_book(&s.store(), q.book_url.trim())
        .await
        .map_err(edit_error)?;
    let errors = report
        .issues
        .iter()
        .filter(|i| i.severity == "error")
        .count();
    let warnings = report.issues.len() - errors;
    let issues: Vec<Value> = report
        .issues
        .iter()
        .map(|i| {
            json!({
                "severity": i.severity,
                "code": i.code,
                "path": i.path,
                "message": i.message,
            })
        })
        .collect();
    Ok(Json(json!({
        "ok": true,
        "revision": report.revision,
        "checked_files": report.checked_files,
        "errors": errors,
        "warnings": warnings,
        "issues": issues,
    })))
}

#[derive(Deserialize)]
struct PreviewBody {
    #[serde(alias = "book_id")]
    book_url: String,
    path: String,
    content: Option<String>,
}

async fn preview_file(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<PreviewBody>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let out = files::preview_file(&s.store(), q.book_url.trim(), &q.path, q.content.as_deref())
        .await
        .map_err(edit_error)?;
    Ok(Json(json!({
        "ok": true,
        "revision": out.revision,
        "blocks": blocks_json(out.blocks),
        "truncated": out.truncated,
    })))
}
