//! 从书目录统计「已缓存章节数 / 正文占用 / 图片占用」。
//!
//! 这几个数字以前由 `library_content` / `library_images` 聚合（见
//! [`crate::library::refresh_rollup`]）；目录成为权威后，管理员编辑、手工放入的
//! 书目录都不会写旧表，必须能从目录本身算出来，馆藏列表与 OPDS 才不会被旧表绑住。

use sqlx::SqlitePool;

use super::{read, Store};

/// 一本书的目录占用。`cached_chapters` 只数真正有内容的章节（占位章不算）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub cached_chapters: i64,
    pub content_bytes: i64,
    pub image_bytes: i64,
}

/// 按目录计算占用；目录不存在或无法解析时返回 `None`（调用方继续用 DB 回退值）。
pub async fn book_usage(store: &Store, book_url: &str) -> std::io::Result<Option<Usage>> {
    if let Some(comic) = store.open_comic(book_url).await? {
        let mut usage = Usage::default();
        for chapter in &comic.meta.chapters {
            if chapter.is_volume || chapter.pages.is_empty() {
                continue;
            }
            usage.cached_chapters += 1;
            for page in &chapter.pages {
                let Ok(path) = super::safe_join(&comic.dir, &page.href) else {
                    continue;
                };
                if let Ok(meta) = tokio::fs::metadata(&path).await {
                    if meta.is_file() {
                        usage.image_bytes += meta.len() as i64;
                    }
                }
            }
        }
        return Ok(Some(usage));
    }
    if let Some(book) = store.open_book(book_url).await? {
        let mut usage = Usage::default();
        for item in &book.meta.chapters {
            if item.is_volume {
                continue;
            }
            let Ok(Some(xhtml)) = read::read_chapter_file(&book.dir, &item.href).await else {
                continue;
            };
            if read::parse_chapter(&xhtml, &item.title).is_none() {
                continue;
            }
            usage.cached_chapters += 1;
            usage.content_bytes += xhtml.len() as i64;
        }
        return Ok(Some(usage));
    }
    Ok(None)
}

/// 目录里已缓存的章节 URL 集合；目录不存在或无法解析时返回 `None`（调用方回退 DB）。
///
/// 文字书要求章节文件不是占位章（`parse_chapter` 有正文），漫画要求页表非空。
/// 卷标题不算章节。没有 `oread:chapter-url` 的老目录条目直接跳过。
pub async fn cached_chapter_urls(
    store: &Store,
    book_url: &str,
) -> std::io::Result<Option<std::collections::HashSet<String>>> {
    if let Some(comic) = store.open_comic(book_url).await? {
        let mut out = std::collections::HashSet::new();
        for chapter in &comic.meta.chapters {
            if chapter.is_volume || chapter.pages.is_empty() {
                continue;
            }
            if let Some(url) = chapter
                .url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                out.insert(url.to_string());
            }
        }
        return Ok(Some(out));
    }
    if let Some(book) = store.open_book(book_url).await? {
        let mut out = std::collections::HashSet::new();
        for item in &book.meta.chapters {
            if item.is_volume {
                continue;
            }
            let Some(url) = item.url.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
                continue;
            };
            let Ok(Some(xhtml)) = read::read_chapter_file(&book.dir, &item.href).await else {
                continue;
            };
            if read::parse_chapter(&xhtml, &item.title).is_some() {
                out.insert(url.to_string());
            }
        }
        return Ok(Some(out));
    }
    Ok(None)
}

/// 用目录覆盖 `library_books` 里的冗余统计；不刷新访问时间。
///
/// 返回 `false` 表示目录不存在，数据库里的旧值保持原样（迁移期回退）。
pub async fn refresh_rollup(
    pool: &SqlitePool,
    store: &Store,
    book_url: &str,
) -> anyhow::Result<bool> {
    let Some(usage) = book_usage(store, book_url).await? else {
        return Ok(false);
    };
    sqlx::query(
        "UPDATE library_books SET cached_chapters = ?2, content_bytes = ?3, image_bytes = ?4 \
         WHERE book_url = ?1",
    )
    .bind(book_url.trim())
    .bind(usage.cached_chapters)
    .bind(usage.content_bytes)
    .bind(usage.image_bytes)
    .execute(pool)
    .await?;
    Ok(true)
}

/// 逐本刷新所有有目录的书；返回实际刷新的本数。给 `doctor --sync-db` 用。
pub async fn refresh_all(pool: &SqlitePool, store: &Store) -> anyhow::Result<usize> {
    let urls: Vec<String> = sqlx::query_scalar("SELECT book_url FROM library_books")
        .fetch_all(pool)
        .await?;
    let mut refreshed = 0;
    for url in urls {
        if refresh_rollup(pool, store, &url).await? {
            refreshed += 1;
        }
    }
    Ok(refreshed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::BookChapter;
    use crate::library_store::comic;
    use crate::library_store::write::{save_text_chapter, sync_text_toc, TocSeed};
    use crate::library_store::{OpfMeta, Tier};

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("oread-usage-{tag}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn text_usage_counts_only_cached_chapters() {
        let root = temp_root("text");
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let book_url = "https://x/text";
        let seed = TocSeed {
            tier: Tier::Temp,
            is_comic: false,
            opf: OpfMeta::new(book_url, "文字书"),
            comic: comic::ComicMeta::new(book_url, "文字书"),
        };
        let chapter = |url: &str| BookChapter {
            url: url.into(),
            title: Some("章".into()),
            ..Default::default()
        };
        sync_text_toc(
            &store,
            &seed,
            &[chapter("https://x/c1"), chapter("https://x/c2")],
        )
        .await
        .unwrap();
        save_text_chapter(
            &store,
            book_url,
            "https://x/c1",
            &["正文一".into(), "正文二".into()],
        )
        .await
        .unwrap();

        let usage = book_usage(&store, book_url).await.unwrap().unwrap();
        assert_eq!(usage.cached_chapters, 1);
        assert!(usage.content_bytes > 0);
        assert_eq!(usage.image_bytes, 0);
        // worker 用它跳过已落目录的章节：占位章不能算命中。
        let cached = cached_chapter_urls(&store, book_url)
            .await
            .unwrap()
            .unwrap();
        assert!(cached.contains("https://x/c1"));
        assert!(!cached.contains("https://x/c2"));
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn comic_usage_counts_pages_and_file_bytes() {
        let root = temp_root("comic");
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let book_url = "https://x/comic";
        let mut meta = comic::ComicMeta::new(book_url, "漫画书");
        meta.chapters.push(comic::ComicChapter {
            id: "ch-0001".into(),
            title: "第一话".into(),
            url: Some("https://x/c1".into()),
            is_volume: false,
            pages: vec![
                comic::ComicPage {
                    href: "0001-001.jpg".into(),
                    mime: "image/jpeg".into(),
                    width: Some(1),
                    height: Some(1),
                    bytes: Some(4),
                    source_url: None,
                },
                comic::ComicPage {
                    href: "0001-002.jpg".into(),
                    mime: "image/jpeg".into(),
                    width: Some(1),
                    height: Some(1),
                    bytes: Some(5),
                    source_url: None,
                },
            ],
        });
        let dir = store.dir_for(Tier::Temp, book_url);
        comic::write_meta(&dir, &meta).await.unwrap();
        std::fs::write(dir.join("0001-001.jpg"), b"abcd").unwrap();
        std::fs::write(dir.join("0001-002.jpg"), b"abcde").unwrap();

        let usage = book_usage(&store, book_url).await.unwrap().unwrap();
        assert_eq!(usage.cached_chapters, 1);
        assert_eq!(usage.image_bytes, 9);
        assert_eq!(usage.content_bytes, 0);
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn refresh_rollup_updates_only_existing_book_row() {
        let root = temp_root("rollup");
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let book_url = "https://x/text";
        let seed = TocSeed {
            tier: Tier::Temp,
            is_comic: false,
            opf: OpfMeta::new(book_url, "文字书"),
            comic: comic::ComicMeta::new(book_url, "文字书"),
        };
        sync_text_toc(
            &store,
            &seed,
            &[BookChapter {
                url: "https://x/c1".into(),
                title: Some("章".into()),
                ..Default::default()
            }],
        )
        .await
        .unwrap();
        save_text_chapter(&store, book_url, "https://x/c1", &["正文".into()])
            .await
            .unwrap();

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE library_books (book_url TEXT PRIMARY KEY, cached_chapters INTEGER, \
             content_bytes INTEGER, image_bytes INTEGER)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO library_books (book_url, cached_chapters, content_bytes, image_bytes) \
             VALUES (?1, 0, 0, 0)",
        )
        .bind(book_url)
        .execute(&pool)
        .await
        .unwrap();

        assert!(refresh_rollup(&pool, &store, book_url).await.unwrap());
        let (chapters, bytes): (i64, i64) = sqlx::query_as(
            "SELECT cached_chapters, content_bytes FROM library_books WHERE book_url = ?1",
        )
        .bind(book_url)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(chapters, 1);
        assert!(bytes > 0);

        // 没有目录的书保持原值，不误清零。
        assert!(!refresh_rollup(&pool, &store, "https://x/missing")
            .await
            .unwrap());
        std::fs::remove_dir_all(root).ok();
    }
}
