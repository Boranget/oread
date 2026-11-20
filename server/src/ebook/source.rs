//! 从馆藏库读取「可导出的书视图」。
//!
//! 只读：不抓网络、不写任何表。目录缺失时返回 409（与旧 TXT 导出口径一致），
//! 由路由层直接透传给客户端。

use sqlx::SqlitePool;

use crate::error::{AppError, AppResult};

use super::model::{BookInput, BookMeta, ChapterRef};
use super::pipeline::ChapterReader;

/// 一次取齐书目与目录，组装成导出输入。正文按章懒读（见 [`LibraryReader`]）。
pub async fn load_book_input(pool: &SqlitePool, book_url: &str) -> AppResult<BookInput> {
    let book_url = book_url.trim();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }

    let rows: Vec<(String, Option<String>, bool)> = sqlx::query_as(
        "SELECT url, title, is_volume FROM library_chapters \
         WHERE book_url = ?1 ORDER BY \"index\"",
    )
    .bind(book_url)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        // 前置条件不满足（目录还没缓存），用 409 而不是 400
        return Err(AppError::Conflict(
            "目录未缓存，请先调用 GET /api/book/toc".into(),
        ));
    }

    let meta_row: Option<MetaRow> = sqlx::query_as(
        "SELECT name, author, origin, origin_name, intro, language, publisher, pubdate, \
                series_name, series_index \
         FROM library_books WHERE book_url = ?1",
    )
    .bind(book_url)
    .fetch_optional(pool)
    .await?;
    let meta_row = meta_row.unwrap_or_default();

    let mut chapters = Vec::with_capacity(rows.len());
    let mut seq = 0i64;
    for (url, title, is_volume) in rows {
        // `volume:` 前缀是历史写法，和 is_volume 一起判定，避免分卷被当成正文。
        let is_volume = is_volume || url.starts_with("volume:");
        let (seq_no, title) = if is_volume {
            (0, title.unwrap_or_default())
        } else {
            seq += 1;
            let title = title
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| format!("第 {seq} 章"));
            (seq, title)
        };
        chapters.push(ChapterRef {
            url,
            title,
            seq: seq_no,
            is_volume,
        });
    }

    // 分卷行不是正文章节，不计入总数；空正文（只有空白）等同未缓存。
    let (total_chapters, cached_chapters, cached_bytes): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), \
         COALESCE(SUM(CASE WHEN ct.url IS NOT NULL THEN 1 ELSE 0 END), 0), \
         COALESCE(SUM(ct.size_bytes), 0) \
         FROM library_chapters c LEFT JOIN library_content ct \
           ON ct.url = c.url AND ct.book_url = c.book_url \
              AND ct.content IS NOT NULL AND length(trim(ct.content)) > 0 \
         WHERE c.book_url = ?1 AND c.is_volume = 0 AND c.url NOT LIKE 'volume:%'",
    )
    .bind(book_url)
    .fetch_one(pool)
    .await?;

    let name = meta_row
        .name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| book_url.to_string());
    let source = meta_row
        .origin_name
        .filter(|s| !s.trim().is_empty())
        .or(meta_row.origin);

    Ok(BookInput {
        meta: BookMeta {
            book_url: book_url.to_string(),
            name,
            author: meta_row.author.unwrap_or_default(),
            language: meta_row.language,
            publisher: meta_row.publisher,
            pubdate: meta_row.pubdate,
            series_name: meta_row.series_name,
            series_index: meta_row.series_index,
            intro: meta_row.intro.filter(|s| !s.trim().is_empty()),
            source,
        },
        chapters,
        total_chapters,
        cached_chapters,
        missing_chapters: (total_chapters - cached_chapters).max(0),
        cached_bytes,
    })
}

#[derive(Debug, Default, sqlx::FromRow)]
struct MetaRow {
    name: Option<String>,
    author: Option<String>,
    origin: Option<String>,
    origin_name: Option<String>,
    intro: Option<String>,
    language: Option<String>,
    publisher: Option<String>,
    pubdate: Option<String>,
    series_name: Option<String>,
    series_index: Option<f64>,
}

/// 馆藏正文读取器：writer 只看到「按 URL 取一章」，不知道 SQLite 结构。
pub struct LibraryReader<'a> {
    pool: &'a SqlitePool,
    book_url: &'a str,
}

impl<'a> LibraryReader<'a> {
    pub fn new(pool: &'a SqlitePool, book_url: &'a str) -> Self {
        Self {
            pool,
            book_url: book_url.trim(),
        }
    }
}

#[axum::async_trait]
impl ChapterReader for LibraryReader<'_> {
    async fn chapter_text(&self, url: &str) -> anyhow::Result<Option<String>> {
        // LIMIT 1：同 URL 同书的重复行不会读进内存。
        let body: Option<String> = sqlx::query_scalar(
            "SELECT content FROM library_content \
             WHERE url = ?1 AND book_url = ?2 AND content IS NOT NULL LIMIT 1",
        )
        .bind(url)
        .bind(self.book_url)
        .fetch_optional(self.pool)
        .await?;
        Ok(body.filter(|c| !c.trim().is_empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
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
             title TEXT, is_volume INTEGER NOT NULL DEFAULT 0, \
             \"index\" INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (book_url, url))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE library_content (url TEXT NOT NULL, origin TEXT NOT NULL DEFAULT '', \
             book_url TEXT NOT NULL, title TEXT, content TEXT, \
             size_bytes INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (url, origin))",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn insert_book(pool: &SqlitePool) {
        sqlx::query(
            "INSERT INTO library_books (book_url, name, author, origin, origin_name, intro, \
             language, publisher, pubdate, series_name, series_index) \
             VALUES ('https://x/book', '测试书', '作者：某人', 'https://src', '测试源', '简介', \
             'zh', '测试社', '2020-01-01', '测试系列', 2.5)",
        )
        .execute(pool)
        .await
        .unwrap();
        for (url, title, is_volume, idx) in [
            ("volume:book#0", "第一卷", 1, 0),
            ("https://x/c1", "第一章", 0, 1),
            ("https://x/c2", "", 0, 2),
        ] {
            sqlx::query(
                "INSERT INTO library_chapters (book_url, url, title, is_volume, \"index\") \
                 VALUES ('https://x/book', ?1, ?2, ?3, ?4)",
            )
            .bind(url)
            .bind(title)
            .bind(is_volume)
            .bind(idx)
            .execute(pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO library_content (url, book_url, content, size_bytes) \
             VALUES ('https://x/c1', 'https://x/book', '正文', 6)",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO library_content (url, book_url, content, size_bytes) \
             VALUES ('https://x/c2', 'https://x/book', '   ', 3)",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn loads_meta_chapters_and_cache_stats() {
        let p = pool().await;
        insert_book(&p).await;
        let input = load_book_input(&p, "https://x/book").await.unwrap();

        assert_eq!(input.meta.name, "测试书");
        assert_eq!(
            input.meta.author, "作者：某人",
            "元数据原样保存，输出时再规范化"
        );
        assert_eq!(input.meta.source.as_deref(), Some("测试源"));
        assert_eq!(input.meta.series_name.as_deref(), Some("测试系列"));
        assert_eq!(input.meta.series_index, Some(2.5));
        assert_eq!(input.meta.language.as_deref(), Some("zh"));

        assert_eq!(input.chapters.len(), 3);
        assert!(input.chapters[0].is_volume);
        assert_eq!(input.chapters[0].seq, 0);
        assert_eq!(input.chapters[1].seq, 1, "分卷行不占正文序号");
        assert_eq!(input.chapters[2].seq, 2);
        assert_eq!(input.chapters[2].title, "第 2 章", "空标题补默认名");

        assert_eq!(input.total_chapters, 2);
        assert_eq!(input.cached_chapters, 1, "纯空白正文不算已缓存");
        assert_eq!(input.missing_chapters, 1);
        assert_eq!(input.cached_bytes, 6, "只统计非空正文的字节");
        assert!(!input.complete());
    }

    #[tokio::test]
    async fn missing_meta_row_falls_back_to_book_url() {
        let p = pool().await;
        sqlx::query(
            "INSERT INTO library_chapters (book_url, url, title, is_volume, \"index\") \
             VALUES ('https://x/nometa', 'https://x/nometa/c1', '章', 0, 0)",
        )
        .execute(&p)
        .await
        .unwrap();
        let input = load_book_input(&p, "https://x/nometa").await.unwrap();
        assert_eq!(input.meta.name, "https://x/nometa");
        assert!(input.meta.author.is_empty());
        assert_eq!(input.total_chapters, 1);
        assert_eq!(input.cached_chapters, 0);
    }

    #[tokio::test]
    async fn empty_toc_is_conflict() {
        let p = pool().await;
        let err = load_book_input(&p, "https://x/none").await.unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
    }

    #[tokio::test]
    async fn reader_hides_blank_content() {
        let p = pool().await;
        insert_book(&p).await;
        let reader = LibraryReader::new(&p, "https://x/book");
        assert_eq!(
            reader
                .chapter_text("https://x/c1")
                .await
                .unwrap()
                .as_deref(),
            Some("正文")
        );
        assert!(reader.chapter_text("https://x/c2").await.unwrap().is_none());
        assert!(reader
            .chapter_text("https://x/none")
            .await
            .unwrap()
            .is_none());
    }
}
