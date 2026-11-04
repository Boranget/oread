//! 已缓存正文导出为 TXT。
//!
//! 约束（低内存机型）：
//! - **逐章流式写盘**：一次只在内存里持有一章的正文，绝不整本 join 后落盘；
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
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::auth::{AuthUser, UserDb};
use crate::error::{AppError, AppResult};
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/book/export/info", get(export_info))
        .route("/api/book/export/txt", get(export_txt))
}

#[derive(Deserialize)]
pub struct ExportQ {
    pub book_url: String,
}

/// 导出前的计划信息：章节总数、已缓存数、缺失数都基于当前用户库实时统计。
#[derive(Debug, Clone, Default)]
struct ExportPlan {
    name: String,
    author: String,
    source: String,
    total_chapters: i64,
    cached_chapters: i64,
    cached_bytes: i64,
}

struct ExportFile {
    path: PathBuf,
    plan: ExportPlan,
    /// 实际写入的已缓存章节数（导出过程中缓存被并发清理时可能略小于计划值）
    written_cached: i64,
    size_bytes: u64,
}

const PLACEHOLDER_TITLE: &str = "【未缓存】";

async fn build_plan(pool: &SqlitePool, book_url: &str) -> AppResult<ExportPlan> {
    let book_url = book_url.trim();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_chapters WHERE book_url = ?1")
        .bind(book_url)
        .fetch_one(pool)
        .await?;
    if rows == 0 {
        // 前置条件不满足（目录还没缓存），用 409 而不是 400
        return Err(AppError::Conflict(
            "目录未缓存，请先调用 GET /api/book/toc".into(),
        ));
    }
    #[allow(clippy::type_complexity)] // 元数据行与 SQL 列一一对应，保持就地元组便于核对。
    let meta: Option<(
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT name, author, origin_name, origin FROM book_meta WHERE book_url = ?1",
    )
    .bind(book_url)
    .fetch_optional(pool)
    .await?;
    let (name, author, origin_name, origin) = meta.unwrap_or_default();
    let name = name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| book_url.to_string());
    let author = normalize_author(author.as_deref().unwrap_or(""));
    let source = origin_name
        .filter(|s| !s.trim().is_empty())
        .or(origin)
        .unwrap_or_default();

    // 分卷行不是正文章节，不计入总数；空正文（只有空白）等同未缓存。
    let (total, cached, cached_bytes): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), \
         COALESCE(SUM(CASE WHEN ct.url IS NOT NULL THEN 1 ELSE 0 END), 0), \
         COALESCE(SUM(ct.size_bytes), 0) \
         FROM book_chapters c LEFT JOIN book_content ct \
           ON ct.url = c.url AND ct.content IS NOT NULL AND length(trim(ct.content)) > 0 \
         WHERE c.book_url = ?1 AND c.is_volume = 0 AND c.url NOT LIKE 'volume:%'",
    )
    .bind(book_url)
    .fetch_one(pool)
    .await?;

    Ok(ExportPlan {
        name,
        author,
        source,
        total_chapters: total,
        cached_chapters: cached,
        cached_bytes,
    })
}

/// 规范化书源里的作者字段：书源常自带 `作者：` / `作 者：` / `Authors:` 等标签，
/// 导出时统一剥掉标签与首尾空白，空值回落为“未知”。
fn normalize_author(raw: &str) -> String {
    let mut s = raw.trim();
    for label in ["作者", "作 者", "Authors", "authors", "AUTHOR", "AUTHORS"] {
        if let Some(rest) = s.strip_prefix(label) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix(':').or_else(|| rest.strip_prefix('：')) {
                s = rest.trim();
                break;
            }
        }
    }
    if s.is_empty() {
        "未知".into()
    } else {
        s.to_string()
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
    format!("attachment; filename=\"export.txt\"; filename*=UTF-8''{encoded}")
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

/// 逐章查询正文并流式写入磁盘。内存峰值 ≈ 单章正文 + 16 KiB 缓冲。
async fn write_export(
    pool: &SqlitePool,
    data_dir: &str,
    uid: i64,
    book_url: &str,
) -> AppResult<ExportFile> {
    let plan = build_plan(pool, book_url).await?;
    let dir = Path::new(data_dir).join("exports").join(uid.to_string());
    fs::create_dir_all(&dir).await?;
    cleanup_stale(&dir).await;

    let seq = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{}-{}.txt", sanitize_filename(&plan.name), seq));

    let missing = (plan.total_chapters - plan.cached_chapters).max(0);
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut header = format!(
        "书名：{}\n作者：{}\n书源：{}\n书籍地址：{}\n导出时间：{}\n\
         章节：已缓存 {}/{} 章，缺 {} 章\n",
        plan.name,
        plan.author,
        plan.source,
        book_url.trim(),
        now,
        plan.cached_chapters,
        plan.total_chapters,
        missing
    );
    if missing > 0 {
        header.push_str("提示：缺失章节已用占位说明标出，不会静默丢章。\n");
    }
    header.push('\n');
    header.push_str(&"=".repeat(60));
    header.push('\n');

    let mut file = File::create(&path).await?;
    file.write_all(header.as_bytes()).await?;

    let chapters: Vec<(String, Option<String>, bool)> = sqlx::query_as(
        "SELECT url, title, is_volume FROM book_chapters \
         WHERE book_url = ?1 ORDER BY \"index\"",
    )
    .bind(book_url.trim())
    .fetch_all(pool)
    .await?;

    let mut written_cached = 0i64;
    let mut seq_no = 0i64;
    for (url, title, is_volume) in chapters {
        if is_volume || url.starts_with("volume:") {
            if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
                file.write_all(format!("\n\n【分卷】{}\n", t.trim()).as_bytes())
                    .await?;
            }
            continue;
        }
        seq_no += 1;
        let title = title
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("第 {seq_no} 章"));
        let heading = if plan.total_chapters > 0 {
            format!(
                "\n\n===== [{}/{}] {} =====\n\n",
                seq_no,
                plan.total_chapters,
                title.trim()
            )
        } else {
            format!("\n\n===== {} =====\n\n", title.trim())
        };
        file.write_all(heading.as_bytes()).await?;

        // 一次只取一章正文；LIMIT 1 保证不会把同 URL 的多余行读进内存。
        let body: Option<String> = sqlx::query_scalar(
            "SELECT content FROM book_content WHERE url = ?1 AND content IS NOT NULL LIMIT 1",
        )
        .bind(&url)
        .fetch_optional(pool)
        .await?;
        match body.filter(|c| !c.trim().is_empty()) {
            Some(body) => {
                file.write_all(body.trim().as_bytes()).await?;
                file.write_all(b"\n").await?;
                written_cached += 1;
            }
            None => {
                file.write_all(
                    format!(
                        "{}本章正文尚未缓存。章节地址：{}\n\
                         请先在阅读端打开本章，或执行“缓存全部”后重新导出。\n",
                        PLACEHOLDER_TITLE, url
                    )
                    .as_bytes(),
                )
                .await?;
            }
        }
    }

    let footer = format!(
        "\n\n===== 导出结束：实际写入 {}/{} 章 =====\n",
        written_cached, plan.total_chapters
    );
    file.write_all(footer.as_bytes()).await?;
    file.flush().await?;
    drop(file);

    let size_bytes = fs::metadata(&path).await?.len();
    Ok(ExportFile {
        path,
        plan,
        written_cached,
        size_bytes,
    })
}

async fn export_info(
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<ExportQ>,
) -> AppResult<Json<serde_json::Value>> {
    let plan = build_plan(&db.0, &q.book_url).await?;
    let export_url = format!(
        "/api/book/export/txt?book_url={}",
        url::form_urlencoded::byte_serialize(q.book_url.trim().as_bytes()).collect::<String>()
    );
    Ok(Json(json!({
        "book_url": q.book_url.trim(),
        "name": plan.name,
        "author": plan.author,
        "source": plan.source,
        "total_chapters": plan.total_chapters,
        "cached_chapters": plan.cached_chapters,
        "missing_chapters": (plan.total_chapters - plan.cached_chapters).max(0),
        "cached_bytes": plan.cached_bytes,
        "complete": plan.cached_chapters >= plan.total_chapters,
        "export_url": export_url,
    })))
}

async fn export_txt(
    State(s): State<AppState>,
    u: AuthUser,
    db: UserDb,
    Query(q): Query<ExportQ>,
) -> AppResult<Response> {
    let out = write_export(&db.0, &s.cfg.data_dir, u.id, &q.book_url).await?;

    let filename = format!(
        "{}-{}.txt",
        sanitize_filename(&out.plan.name),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let file = File::open(&out.path).await?;
    // 流式回传：读一块写一块；正常读完即删临时文件；中断时靠 cleanup_stale 兜底。
    let body_stream = stream::try_unfold((file, out.path.clone()), |(mut file, path)| async move {
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
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&content_disposition(&filename))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment; filename=\"export.txt\"")),
    );
    h.insert(
        "x-total-chapters",
        HeaderValue::from_str(&out.plan.total_chapters.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-cached-chapters",
        HeaderValue::from_str(&out.plan.cached_chapters.to_string()).expect("数字 header"),
    );
    h.insert(
        "x-missing-chapters",
        HeaderValue::from_str(
            &(out.plan.total_chapters - out.plan.cached_chapters)
                .max(0)
                .to_string(),
        )
        .expect("数字 header"),
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

    async fn export_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE book_meta (book_url TEXT PRIMARY KEY, name TEXT, author TEXT, \
             origin TEXT, origin_name TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE book_chapters (url TEXT PRIMARY KEY, book_url TEXT NOT NULL, \
             title TEXT, is_volume INTEGER NOT NULL DEFAULT 0, \"index\" INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE book_content (url TEXT PRIMARY KEY, book_url TEXT NOT NULL, \
             title TEXT, content TEXT, size_bytes INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn insert_book(pool: &SqlitePool, book: &str) {
        sqlx::query(
            "INSERT INTO book_meta (book_url, name, author, origin, origin_name) \
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
                "INSERT INTO book_chapters (url, book_url, title, is_volume, \"index\") \
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
                "INSERT INTO book_content (url, book_url, content, size_bytes) \
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
        assert_eq!(text.matches(PLACEHOLDER_TITLE).count(), 1);
        assert!(text.contains("章节地址：https://x/c2"));

        assert_eq!(out.plan.total_chapters, 3, "分卷行不计入章节总数");
        assert_eq!(out.plan.cached_chapters, 2);
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
        sqlx::query("UPDATE book_content SET content = '   ' WHERE url = 'https://x/c1'")
            .execute(&pool)
            .await
            .unwrap();
        let plan = build_plan(&pool, book).await.unwrap();
        assert_eq!(plan.cached_chapters, 1, "空白正文按缺失统计");
        assert_eq!(plan.total_chapters, 3);
    }

    #[tokio::test]
    async fn empty_toc_is_rejected_with_conflict() {
        let pool = export_pool().await;
        let err = build_plan(&pool, "https://x/none").await.err().unwrap();
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
        sqlx::query("UPDATE book_meta SET author = '作者：无语森' WHERE book_url = ?1")
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
        let plan = build_plan(&pool, book).await.unwrap();
        assert_eq!(plan.total_chapters, out.plan.total_chapters);
        assert_eq!(plan.cached_chapters, out.plan.cached_chapters);
        assert_eq!(plan.cached_bytes, out.plan.cached_bytes);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
