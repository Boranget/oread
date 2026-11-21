//! 已缓存正文导出为 TXT。
//!
//! HTTP 层只负责三件事：算文件名、流式回传、清理临时文件。真正的写入逻辑在
//! [`crate::ebook::format::txt`]；增删格式只需换 writer，这里不再关心正文字段。
//!
//! 约束（低内存机型）：
//! - **逐章流式写盘**：一次只在内存里持有一章正文，绝不整本 join 后落盘；
//! - 未缓存章节必须显式占位，不能静默丢章；
//! - 导出接口本身只读缓存、不发起任何网络抓取。
//!
//! 多本导出不引入 zip 依赖：由调用方逐本请求本接口即可（每个响应就是一个独立 TXT）。

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::{header, HeaderValue, StatusCode},
    response::Response,
    routing::get,
    Json, Router,
};
use futures::stream;
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
use tokio::fs::{self, File};
use tokio::io::AsyncReadExt;

use crate::auth::AuthUser;
use crate::ebook::format::cbz::CbzWriter;
use crate::ebook::format::epub::EpubWriter;
use crate::ebook::format::txt::{normalize_author, TxtWriter};
use crate::ebook::pipeline::FormatWriter;
use crate::ebook::{BookInput, ComicInput, Format, WriteReport};
use crate::ebook_source::{load_comic_input, LibraryReader};
use crate::error::{AppError, AppResult};
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/book/export/info", get(export_info))
        .route("/api/book/export/txt", get(export_txt))
        .route("/api/book/export/cbz/info", get(export_cbz_info))
        .route("/api/book/export/cbz", get(export_cbz))
        .route("/api/book/export/epub/info", get(export_epub_info))
        .route("/api/book/export/epub", get(export_epub))
}

#[derive(Deserialize)]
pub struct ExportQ {
    pub book_url: String,
}

#[derive(Debug)]
struct ExportFile {
    path: PathBuf,
    input: BookInput,
    /// 实际写入的已缓存章节数（导出过程中缓存被并发清理时可能略小于计划值）
    written_cached: i64,
    size_bytes: u64,
}

/// 导出前的计划 = 只读的书视图（元数据 + 目录 + 缓存统计）。目录优先。
async fn build_plan(
    pool: &SqlitePool,
    store: &crate::library_store::Store,
    book_url: &str,
) -> AppResult<BookInput> {
    crate::ebook_source::load_book_input_with_store(store, pool, book_url).await
}

/// 有书目录时让 writer 直接读磁盘；否则保持旧 `library_content` 读取。
async fn export_reader<'a>(
    pool: &'a SqlitePool,
    store: &crate::library_store::Store,
    book_url: &'a str,
) -> LibraryReader<'a> {
    let reader = LibraryReader::new(pool, book_url);
    match store.open_book(book_url).await {
        Ok(Some(book)) => reader.with_dir(book),
        Ok(None) => reader,
        Err(err) => {
            tracing::warn!(book_url, error = %err, "open book dir failed; export falls back to db");
            reader
        }
    }
}

/// 去掉路径分隔符与控制字符，避免书名被当成目录/文件名注入；过长时截断。
fn sanitize_filename(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    let trimmed = out.trim().trim_matches('.').trim();
    let name: String = trimmed.chars().take(60).collect();
    if name.is_empty() {
        "book".into()
    } else {
        name
    }
}

/// RFC 5987 编码：ASCII 兜底名 + UTF-8 原名，兼容老浏览器与中文书名。
fn content_disposition(filename: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(filename.as_bytes()).collect();
    let fallback: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// 清理 1 小时前的导出残留（客户端中断时流式删除不会执行，靠这里兜底）。
async fn cleanup_stale(dir: &Path) {
    if let Ok(mut rd) = fs::read_dir(dir).await {
        while let Ok(Some(ent)) = rd.next_entry().await {
            let stale = match ent.metadata().await.and_then(|m| m.modified()) {
                Ok(t) => t.elapsed().map(|d| d.as_secs() > 3600).unwrap_or(false),
                Err(_) => false,
            };
            if stale {
                let _ = fs::remove_file(ent.path()).await;
            }
        }
    }
}

/// 逐章读取并流式写入磁盘：内存峰值 ≈ 单章正文 + 16 KiB 回传缓冲。
async fn write_export(
    pool: &SqlitePool,
    data_dir: &str,
    uid: i64,
    book_url: &str,
) -> AppResult<ExportFile> {
    let store = crate::library_store::Store::new(data_dir);
    let input = build_plan(pool, &store, book_url).await?;
    let dir = Path::new(data_dir).join("exports").join(uid.to_string());
    fs::create_dir_all(&dir).await?;
    cleanup_stale(&dir).await;

    let seq = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!(
        "{}-{}.txt",
        sanitize_filename(&input.meta.name),
        seq
    ));

    let reader = export_reader(pool, &store, book_url).await;
    let report = TxtWriter.write(&input, &reader, &path).await?;

    Ok(ExportFile {
        path,
        input,
        written_cached: report.written_chapters,
        size_bytes: report.size_bytes,
    })
}

/// EPUB 与 TXT 共用同一套临时文件与统计口径；没有正文的漫画书明确 409。
async fn write_epub(
    pool: &SqlitePool,
    data_dir: &str,
    uid: i64,
    book_url: &str,
) -> AppResult<ExportFile> {
    let store = crate::library_store::Store::new(data_dir);
    let input = build_plan(pool, &store, book_url).await?;
    if input.text_chapters == 0 {
        return Err(AppError::Conflict(
            "这本书没有可导出的正文章节，无法生成 EPUB".into(),
        ));
    }

    let dir = Path::new(data_dir).join("exports").join(uid.to_string());
    fs::create_dir_all(&dir).await?;
    cleanup_stale(&dir).await;

    let seq = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!(
        "{}-{}.epub",
        sanitize_filename(&input.meta.name),
        seq
    ));

    let reader = export_reader(pool, &store, book_url).await;
    let report = EpubWriter.write(&input, &reader, &path).await?;

    Ok(ExportFile {
        path,
        input,
        written_cached: report.written_chapters,
        size_bytes: report.size_bytes,
    })
}

#[derive(Debug)]
struct CbzExportFile {
    path: PathBuf,
    input: ComicInput,
    report: WriteReport,
}

/// 逐页读取已落盘图片并打包 CBZ：内存峰值 ≈ 单页 + 16 KiB 回传缓冲。
async fn write_cbz(
    pool: &SqlitePool,
    data_dir: &str,
    uid: i64,
    book_url: &str,
) -> AppResult<CbzExportFile> {
    let input = load_comic_input(pool, book_url).await?;
    if input.total_pages == 0 {
        return Err(AppError::Conflict(
            "这本书没有图片章节，无法导出 CBZ".into(),
        ));
    }
    if input.available_pages == 0 {
        return Err(AppError::Conflict(
            "没有已落盘的漫画图片，请先缓存全书".into(),
        ));
    }

    let dir = Path::new(data_dir).join("exports").join(uid.to_string());
    fs::create_dir_all(&dir).await?;
    cleanup_stale(&dir).await;

    let seq = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!(
        "{}-{}.cbz",
        sanitize_filename(&input.book.meta.name),
        seq
    ));

    let reader = LibraryReader::new(pool, &input.book.meta.book_url);
    let report = CbzWriter.write(&input.book, &reader, &path).await?;
    Ok(CbzExportFile {
        path,
        input,
        report,
    })
}

/// 把临时文件 16 KiB 一块地流式回传，正常读完即删；中断时靠 [`cleanup_stale`] 兜底。
async fn temp_file_response(
    path: PathBuf,
    filename: &str,
    content_type: &'static str,
) -> AppResult<Response> {
    let file = File::open(&path).await?;
    let body_stream = stream::try_unfold((file, path), |(mut file, path)| async move {
        let mut buf = vec![0u8; 16 * 1024];
        match file.read(&mut buf).await {
            Ok(0) => {
                let _ = fs::remove_file(&path).await;
                Ok(None)
            }
            Ok(n) => {
                buf.truncate(n);
                Ok(Some((Bytes::from(buf), (file, path))))
            }
            Err(e) => {
                let _ = fs::remove_file(&path).await;
                Err(e)
            }
        }
    });

    let mut res = Response::new(Body::from_stream(body_stream));
    *res.status_mut() = StatusCode::OK;
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&content_disposition(filename))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment; filename=\"export.bin\"")),
    );
    Ok(res)
}

async fn export_info(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<ExportQ>,
) -> AppResult<Json<serde_json::Value>> {
    let plan = build_plan(&s.library, &s.store(), &q.book_url).await?;
    let export_url = format!(
        "/api/book/export/txt?book_url={}",
        url::form_urlencoded::byte_serialize(q.book_url.trim().as_bytes()).collect::<String>()
    );
    Ok(Json(json!({
        "book_url": q.book_url.trim(),
        "name": plan.meta.name,
        // 与 TXT 头部同一口径：剥掉“作者：”标签
        "author": normalize_author(&plan.meta.author),
        "source": plan.meta.source.clone().unwrap_or_default(),
        "total_chapters": plan.total_chapters,
        "cached_chapters": plan.cached_chapters,
        "missing_chapters": plan.missing_chapters,
        "cached_bytes": plan.cached_bytes,
        "complete": plan.complete(),
        "export_url": export_url,
    })))
}

async fn export_txt(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<ExportQ>,
) -> AppResult<Response> {
    let out = write_export(&s.library, &s.cfg.data_dir, u.id, &q.book_url).await?;

    let filename = format!(
        "{}-{}.txt",
        sanitize_filename(&out.input.meta.name),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let mut res =
        temp_file_response(out.path.clone(), &filename, Format::Txt.content_type()).await?;
    let h = res.headers_mut();
    h.insert(
        "x-total-chapters",
        HeaderValue::from_str(&out.input.total_chapters.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-cached-chapters",
        HeaderValue::from_str(&out.input.cached_chapters.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-missing-chapters",
        HeaderValue::from_str(&out.input.missing_chapters.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-written-chapters",
        HeaderValue::from_str(&out.written_cached.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-file-bytes",
        HeaderValue::from_str(&out.size_bytes.to_string()).expect("数字 header"),
    );
    Ok(res)
}

async fn export_cbz_info(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<ExportQ>,
) -> AppResult<Json<serde_json::Value>> {
    let input = load_comic_input(&s.library, &q.book_url).await?;
    let export_url = format!(
        "/api/book/export/cbz?book_url={}",
        url::form_urlencoded::byte_serialize(q.book_url.trim().as_bytes()).collect::<String>()
    );
    Ok(Json(json!({
        "book_url": q.book_url.trim(),
        "name": input.book.meta.name,
        "author": normalize_author(&input.book.meta.author),
        "source": input.book.meta.source.clone().unwrap_or_default(),
        "image_chapters": input.book.image_chapters().count(),
        "total_pages": input.total_pages,
        "available_pages": input.available_pages,
        "missing_pages": input.missing_pages(),
        "available_bytes": input.available_bytes,
        "complete": input.complete(),
        "export_url": export_url,
    })))
}

async fn export_cbz(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<ExportQ>,
) -> AppResult<Response> {
    cbz_download(&s, u.id, &q.book_url).await
}

/// CBZ 下载的 HTTP 产物（文件名 / 流式回传 / 统计头）。
///
/// OPDS 的 acquisition 链接复用同一入口，保证浏览器导出与阅读器下载的文件名、
/// 头字段、临时文件清理完全一致。
pub(crate) async fn cbz_download(s: &AppState, uid: i64, book_url: &str) -> AppResult<Response> {
    let out = write_cbz(&s.library, &s.cfg.data_dir, uid, book_url).await?;

    let filename = format!(
        "{}-{}.cbz",
        sanitize_filename(&out.input.book.meta.name),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let mut res =
        temp_file_response(out.path.clone(), &filename, Format::Cbz.content_type()).await?;
    let h = res.headers_mut();
    h.insert(
        "x-total-pages",
        HeaderValue::from_str(&out.input.total_pages.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-available-pages",
        HeaderValue::from_str(&out.input.available_pages.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-missing-pages",
        HeaderValue::from_str(&out.report.missing_pages.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-written-chapters",
        HeaderValue::from_str(&out.report.written_chapters.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-written-pages",
        HeaderValue::from_str(&out.report.written_pages.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-file-bytes",
        HeaderValue::from_str(&out.report.size_bytes.to_string()).expect("数字 header"),
    );
    Ok(res)
}

async fn export_epub_info(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<ExportQ>,
) -> AppResult<Json<serde_json::Value>> {
    let plan = build_plan(&s.library, &s.store(), &q.book_url).await?;
    let export_url = format!(
        "/api/book/export/epub?book_url={}",
        url::form_urlencoded::byte_serialize(q.book_url.trim().as_bytes()).collect::<String>()
    );
    Ok(Json(json!({
        "book_url": q.book_url.trim(),
        "name": plan.meta.name,
        "author": normalize_author(&plan.meta.author),
        "source": plan.meta.source.clone().unwrap_or_default(),
        "text_chapters": plan.text_chapters,
        "cached_chapters": plan.cached_text_chapters,
        "missing_chapters": plan.text_missing_chapters(),
        "cached_bytes": plan.cached_bytes,
        "complete": plan.text_complete(),
        "export_url": export_url,
    })))
}

async fn export_epub(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<ExportQ>,
) -> AppResult<Response> {
    epub_download(&s, u.id, &q.book_url).await
}

/// EPUB 下载的 HTTP 产物；OPDS 与浏览器导出共用（见 [`cbz_download`]）。
pub(crate) async fn epub_download(s: &AppState, uid: i64, book_url: &str) -> AppResult<Response> {
    let out = write_epub(&s.library, &s.cfg.data_dir, uid, book_url).await?;

    let filename = format!(
        "{}-{}.epub",
        sanitize_filename(&out.input.meta.name),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let mut res =
        temp_file_response(out.path.clone(), &filename, Format::Epub.content_type()).await?;
    let h = res.headers_mut();
    h.insert(
        "x-text-chapters",
        HeaderValue::from_str(&out.input.text_chapters.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-cached-chapters",
        HeaderValue::from_str(&out.input.cached_text_chapters.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-missing-chapters",
        HeaderValue::from_str(&out.input.text_missing_chapters().to_string()).expect("数字 header"),
    );
    h.insert(
        "x-written-chapters",
        HeaderValue::from_str(&out.written_cached.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-file-bytes",
        HeaderValue::from_str(&out.size_bytes.to_string()).expect("数字 header"),
    );
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    /// 只用来证明「目录不存在时回退数据库」；不创建任何书目录。
    fn empty_store() -> crate::library_store::Store {
        crate::library_store::Store::new(
            std::env::temp_dir().join(format!("oread-export-empty-{}", std::process::id())),
        )
    }

    async fn export_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE library_books (book_url TEXT PRIMARY KEY, name TEXT, author TEXT, \
             origin TEXT, origin_name TEXT, intro TEXT, language TEXT, publisher TEXT, \
             pubdate TEXT, series_name TEXT, series_index REAL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE library_chapters (book_url TEXT NOT NULL, url TEXT NOT NULL, \
             title TEXT, is_volume INTEGER NOT NULL DEFAULT 0, \"index\" INTEGER NOT NULL DEFAULT 0, \
             PRIMARY KEY (book_url, url))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE library_content (url TEXT NOT NULL, origin TEXT NOT NULL DEFAULT '', \
             book_url TEXT NOT NULL, title TEXT, content TEXT, \
             content_type TEXT NOT NULL DEFAULT 'text', images TEXT, \
             size_bytes INTEGER NOT NULL DEFAULT 0, \
             images_ready INTEGER NOT NULL DEFAULT 0, \
             updated_at TEXT NOT NULL DEFAULT '1970-01-01 00:00:00', \
             PRIMARY KEY (url, origin))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE library_images (url TEXT NOT NULL, origin TEXT NOT NULL, \
             book_url TEXT NOT NULL, mime TEXT NOT NULL, data BLOB NOT NULL, \
             size_bytes INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (url, origin))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE library_identifiers (book_url TEXT NOT NULL, scheme TEXT NOT NULL, \
             value TEXT NOT NULL, PRIMARY KEY (book_url, scheme, value))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE library_tags (book_url TEXT NOT NULL, tag TEXT NOT NULL, \
             PRIMARY KEY (book_url, tag))",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn insert_book(pool: &SqlitePool, book: &str) {
        sqlx::query(
            "INSERT INTO library_books (book_url, name, author, origin, origin_name) \
             VALUES (?1, '测试书', '测试作者', 'https://src', '测试源')",
        )
        .bind(book)
        .execute(pool)
        .await
        .unwrap();
        let rows = [
            ("volume:book#0", "第一卷 风起", 1, 0),
            ("https://x/c1", "第一章", 0, 1),
            ("https://x/c2", "第二章", 0, 2),
            ("https://x/c3", "第三章", 0, 3),
        ];
        for (url, title, is_volume, idx) in rows {
            sqlx::query(
                "INSERT INTO library_chapters (url, book_url, title, is_volume, \"index\") \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(url)
            .bind(book)
            .bind(title)
            .bind(is_volume)
            .bind(idx)
            .execute(pool)
            .await
            .unwrap();
        }
        for (url, body) in [
            ("https://x/c1", "第一章正文\n第二段"),
            ("https://x/c3", "第三章正文内容"),
        ] {
            sqlx::query(
                "INSERT INTO library_content (url, book_url, content, size_bytes) \
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(url)
            .bind(book)
            .bind(body)
            .bind(body.len() as i64)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("oread-export-{tag}-{}-{stamp}", std::process::id()))
    }

    #[tokio::test]
    async fn export_streams_cached_chapters_in_order_with_placeholder() {
        let pool = export_pool().await;
        let book = "https://x/book";
        insert_book(&pool, book).await;
        let dir = temp_dir("order");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let out = write_export(&pool, dir.to_str().unwrap(), 42, book)
            .await
            .unwrap();
        let text = tokio::fs::read_to_string(&out.path).await.unwrap();

        assert!(text.contains("书名：测试书"));
        assert!(text.contains("作者：测试作者"));
        assert!(text.contains("书源：测试源"));
        assert!(text.contains("已缓存 2/3 章"), "头部应说明缓存进度: {text}");
        assert!(text.contains("【分卷】第一卷 风起"));
        // 章节顺序：第一章 → 第二章 → 第三章
        let p1 = text.find("第一章").unwrap();
        let p2 = text.find("第二章").unwrap();
        let p3 = text.find("第三章").unwrap();
        assert!(p1 < p2 && p2 < p3);
        assert!(text.contains("第一章正文\n第二段"), "命中的正文应原样保留");
        assert!(text.contains("第三章正文内容"));
        // 未缓存章节只有一个占位块，且没有被跳过
        assert_eq!(text.matches("【未缓存】").count(), 1);
        assert!(text.contains("章节地址：https://x/c2"));

        assert_eq!(out.input.total_chapters, 3, "分卷行不计入章节总数");
        assert_eq!(out.input.cached_chapters, 2);
        assert_eq!(out.written_cached, 2);
        assert!(out.size_bytes > 0);
        // UTF-8 往返：中文内容读回后完全一致
        assert!(text.chars().any(|c| c == '风'));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn blank_content_is_treated_as_missing() {
        let pool = export_pool().await;
        let book = "https://x/book";
        insert_book(&pool, book).await;
        // 空串正文不应算“已缓存”
        sqlx::query("UPDATE library_content SET content = '   ' WHERE url = 'https://x/c1'")
            .execute(&pool)
            .await
            .unwrap();
        let plan = build_plan(&pool, &empty_store(), book).await.unwrap();
        assert_eq!(plan.cached_chapters, 1, "空白正文按缺失统计");
        assert_eq!(plan.total_chapters, 3);
    }

    #[tokio::test]
    async fn empty_toc_is_rejected_with_conflict() {
        let pool = export_pool().await;
        let err = build_plan(&pool, &empty_store(), "https://x/none")
            .await
            .err()
            .unwrap();
        assert!(
            matches!(err, AppError::Conflict(_)),
            "目录未缓存应映射 409，而不是 400"
        );
    }

    #[test]
    fn normalize_author_strips_labels_and_falls_back() {
        assert_eq!(normalize_author("作者：无语森"), "无语森");
        assert_eq!(normalize_author("作 者： 无语森 "), "无语森");
        assert_eq!(normalize_author("Authors: 无语森"), "无语森");
        assert_eq!(normalize_author("  无语森  "), "无语森");
        assert_eq!(normalize_author(""), "未知");
        assert_eq!(normalize_author("作者："), "未知");
        // 非行首标签不应误伤
        assert_eq!(normalize_author("原著作者：无语森"), "原著作者：无语森");
    }

    #[tokio::test]
    async fn export_header_prints_author_once() {
        let pool = export_pool().await;
        let book = "https://x/book";
        insert_book(&pool, book).await;
        sqlx::query("UPDATE library_books SET author = '作者：无语森' WHERE book_url = ?1")
            .bind(book)
            .execute(&pool)
            .await
            .unwrap();
        let dir = temp_dir("author");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let out = write_export(&pool, dir.to_str().unwrap(), 42, book)
            .await
            .unwrap();
        let text = tokio::fs::read_to_string(&out.path).await.unwrap();
        assert!(text.contains("作者：无语森"));
        assert!(
            !text.contains("作者：作者："),
            "不应重复输出作者标签: {text}"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn filename_sanitize_and_content_disposition() {
        assert_eq!(sanitize_filename("../../etc:pass"), "_.._etc_pass");
        assert_eq!(sanitize_filename("   "), "book");
        let long: String = "长".repeat(100);
        assert_eq!(sanitize_filename(&long).chars().count(), 60);

        let d = content_disposition("测试书-2026.txt");
        assert!(d.starts_with("attachment;"));
        assert!(d.contains("filename*=UTF-8''"));
        // 中文必须被百分号编码，响应头才能是合法 ASCII
        assert!(!d.contains('测'));
        assert!(d.contains("%E6%B5%8B"));
    }

    // 计划信息与导出实际统计应一致（供 /api/book/export/info 复用）
    #[tokio::test]
    async fn plan_matches_export_stats() {
        let pool = export_pool().await;
        let book = "https://x/book";
        insert_book(&pool, book).await;
        let dir = temp_dir("plan");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let out = write_export(&pool, dir.to_str().unwrap(), 42, book)
            .await
            .unwrap();
        let plan = build_plan(&pool, &empty_store(), book).await.unwrap();
        assert_eq!(plan.total_chapters, out.input.total_chapters);
        assert_eq!(plan.cached_chapters, out.input.cached_chapters);
        assert_eq!(plan.cached_bytes, out.input.cached_bytes);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    async fn insert_comic(pool: &SqlitePool) {
        sqlx::query(
            "INSERT INTO library_books (book_url, name, author, series_name, series_index) \
             VALUES ('https://c/book', '漫画书', '作者：画师', '漫画系列', 1)",
        )
        .execute(pool)
        .await
        .unwrap();
        for (url, idx) in [("https://c/ch1", 0), ("https://c/ch2", 1)] {
            sqlx::query(
                "INSERT INTO library_chapters (book_url, url, title, is_volume, \"index\") \
                 VALUES ('https://c/book', ?1, ?1, 0, ?2)",
            )
            .bind(url)
            .bind(idx)
            .execute(pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO library_content (url, origin, book_url, content_type, images, \
             images_ready) VALUES ('https://c/ch1', 'src', 'https://c/book', 'image', \
             '{\"images\":[{\"url\":\"https://c/p1\"},{\"url\":\"https://c/p2\"}]}', 1)",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO library_content (url, origin, book_url, content_type, images, \
             images_ready) VALUES ('https://c/ch2', 'src', 'https://c/book', 'image', \
             '{\"images\":[{\"url\":\"https://c/p3\"}]}', 1)",
        )
        .execute(pool)
        .await
        .unwrap();
        for (url, data) in [
            ("https://c/p1", b"one".as_slice()),
            ("https://c/p2", b"two".as_slice()),
            ("https://c/p3", b"three".as_slice()),
        ] {
            sqlx::query(
                "INSERT INTO library_images (url, origin, book_url, mime, data, size_bytes) \
                 VALUES (?1, 'src', 'https://c/book', 'image/jpeg', ?2, ?3)",
            )
            .bind(url)
            .bind(data)
            .bind(data.len() as i64)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn cbz_export_packages_pages_and_comicinfo() {
        let pool = export_pool().await;
        insert_comic(&pool).await;
        let dir = temp_dir("cbz");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let out = write_cbz(&pool, dir.to_str().unwrap(), 42, "https://c/book")
            .await
            .unwrap();
        let bytes = tokio::fs::read(&out.path).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();

        assert_eq!(out.input.total_pages, 3);
        assert_eq!(out.report.written_pages, 3);
        assert_eq!(out.report.written_chapters, 2);
        assert_eq!(out.report.missing_pages, 0);
        assert!(bytes.starts_with(b"PK\x03\x04"));
        assert!(text.contains("0001-001.jpg"));
        assert!(text.contains("0002-001.jpg"));
        assert!(text.contains("ComicInfo.xml"));
        assert!(text.contains("<Title>漫画书</Title>"));
        assert!(text.contains("<Writer>画师</Writer>"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn cbz_export_rejects_text_only_book() {
        let pool = export_pool().await;
        insert_book(&pool, "https://x/book").await;
        let dir = temp_dir("cbz-none");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let err = write_cbz(&pool, dir.to_str().unwrap(), 42, "https://x/book")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn epub_export_writes_zip_and_text_chapters() {
        let pool = export_pool().await;
        let book = "https://x/book";
        insert_book(&pool, book).await;
        let dir = temp_dir("epub");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let out = write_epub(&pool, dir.to_str().unwrap(), 42, book)
            .await
            .unwrap();
        let bytes = tokio::fs::read(&out.path).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();

        assert_eq!(out.input.text_chapters, 3);
        assert_eq!(out.input.cached_text_chapters, 2);
        assert_eq!(out.written_cached, 2);
        assert_eq!(out.size_bytes, bytes.len() as u64);
        assert!(bytes.starts_with(b"PK\x03\x04"), "EPUB 必须是 ZIP");
        // mimetype 是 OCF 要求的第一条，且必须 store（method=0）
        let name_len = u16::from_le_bytes([bytes[26], bytes[27]]) as usize;
        assert_eq!(&bytes[30..30 + name_len], b"mimetype");
        assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 0);
        assert!(text.contains("META-INF/container.xml"));
        assert!(text.contains("OEBPS/content.opf"));
        assert!(text.contains("OEBPS/nav.xhtml"));
        assert!(text.contains("<dc:title>测试书</dc:title>"));
        assert!(text.contains("第一章正文"));
        assert!(text.contains("本章正文尚未缓存"), "未缓存章节必须显式占位");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn epub_export_rejects_comic_only_book() {
        let pool = export_pool().await;
        insert_comic(&pool).await;
        let dir = temp_dir("epub-none");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let err = write_epub(&pool, dir.to_str().unwrap(), 42, "https://c/book")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
