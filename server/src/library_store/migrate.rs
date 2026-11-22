//! 一次性迁移：把旧 `library_content` / `library_images` 里的正文与图片导出成
//! 「目录即书」结构。
//!
//! 迁移只读旧表、只写目录，**不删除**任何数据库行；切换读写路径并验证无误后，
//! 再由后续迁移删表。已存在的书目录默认跳过，避免覆盖管理员刚编辑过的内容。

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use sqlx::SqlitePool;

use super::comic::ext_for_mime;
use super::write::markup_to_paragraphs;
use super::{opf, ChapterItem, CoverItem, OpfMeta, Store, Tier};

#[derive(Debug, Default, Clone)]
pub struct MigrateReport {
    pub books: u64,
    pub skipped: u64,
    pub chapters: u64,
    pub images: u64,
    pub comics: u64,
    pub failures: Vec<String>,
}

impl MigrateReport {
    fn fail(&mut self, book_url: &str, err: impl std::fmt::Display) {
        self.failures.push(format!("{book_url}: {err}"));
    }
}

/// 旧书目行（只取迁移需要的列）。
#[derive(Debug, sqlx::FromRow)]
struct OldBook {
    book_url: String,
    name: Option<String>,
    author: Option<String>,
    cover_url: Option<String>,
    intro: Option<String>,
    language: Option<String>,
    publisher: Option<String>,
    pubdate: Option<String>,
    series_name: Option<String>,
    series_index: Option<f64>,
    tier: String,
}

#[derive(Debug, sqlx::FromRow)]
struct OldChapter {
    url: String,
    title: Option<String>,
    is_volume: i64,
    index: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct OldContent {
    content: Option<String>,
    images: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct OldImage {
    mime: String,
    data: Vec<u8>,
    width: Option<i64>,
    height: Option<i64>,
}

pub async fn migrate_from_db(pool: &SqlitePool, store: &Store) -> anyhow::Result<MigrateReport> {
    store.create_layout()?;
    let books: Vec<OldBook> = sqlx::query_as(
        "SELECT book_url, name, author, cover_url, intro, language, publisher, pubdate, \
                series_name, series_index, tier \
         FROM library_books ORDER BY collected_at IS NULL, collected_at, book_url",
    )
    .fetch_all(pool)
    .await?;

    let mut report = MigrateReport::default();
    for book in books {
        report.books += 1;
        if store.find(&book.book_url).is_some() {
            report.skipped += 1;
            continue;
        }
        if let Err(err) = migrate_book(pool, store, &book, &mut report).await {
            report.fail(&book.book_url, err);
        }
    }
    Ok(report)
}

/// 启动时后台补齐：把仍只存在于旧表里的书导出成目录。
///
/// 这是「删旧表」的前置步骤：一本书一旦有目录，读写就都走目录。已存在目录的书
/// 直接跳过，因此重复启动不会重复写盘；失败只记日志，读路径继续回退 `library.db`。
pub fn spawn_bootstrap(pool: SqlitePool, store: Store) {
    tokio::spawn(async move {
        match legacy_content_present(&pool).await {
            Ok(false) => {}
            Ok(true) => {
                tracing::info!("legacy content tables found; bootstrapping book dirs");
                match migrate_from_db(&pool, &store).await {
                    Ok(r) => tracing::info!(
                        books = r.books,
                        skipped = r.skipped,
                        chapters = r.chapters,
                        images = r.images,
                        comics = r.comics,
                        failures = r.failures.len(),
                        "legacy db → book dirs finished"
                    ),
                    Err(e) => tracing::warn!(error = %e, "legacy db → book dirs failed"),
                }
            }
            Err(e) => tracing::warn!(error = %e, "legacy content probe failed"),
        }
    });
}

/// 旧内容表是否还在且有行。未来删表后返回 `false`，启动不再扫描。
async fn legacy_content_present(pool: &SqlitePool) -> anyhow::Result<bool> {
    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' \
         AND name IN ('library_content', 'library_images')",
    )
    .fetch_one(pool)
    .await?;
    if tables < 2 {
        return Ok(false);
    }
    let rows: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM library_content) + (SELECT COUNT(*) FROM library_images)",
    )
    .fetch_one(pool)
    .await?;
    Ok(rows > 0)
}

/// 旧缓存表相对书目录的覆盖情况。用来判断「现在能不能删旧表」：
/// 只有「仅 DB」的行数为 0，目录才是正文/图片的唯一副本。
#[derive(Debug, Default, Clone)]
pub struct LegacyCoverage {
    pub content_rows: i64,
    pub content_covered: i64,
    pub image_rows: i64,
    pub image_covered: i64,
    /// 旧缓存行涉及的、完全没有书目录的书（去重排序）。
    pub books_without_dir: Vec<String>,
    /// 有目录、但仍有缓存行没被目录覆盖的书（去重排序）。
    pub books_partial: Vec<String>,
}

impl LegacyCoverage {
    pub fn content_only_db(&self) -> i64 {
        (self.content_rows - self.content_covered).max(0)
    }

    pub fn image_only_db(&self) -> i64 {
        (self.image_rows - self.image_covered).max(0)
    }

    /// 目录已经能重建全部旧缓存行，删表不会丢内容。
    pub fn complete(&self) -> bool {
        self.content_only_db() == 0 && self.image_only_db() == 0
    }
}

/// 逐本比对旧缓存行与书目录：章节文件有正文才算覆盖，漫画页按原始图片 URL 比对。
///
/// 只读，不修改任何表。没有目录的书、目录不完整的书都会被列出来，方便先跑
/// `migrate-store` 再删表。
pub async fn legacy_coverage(pool: &SqlitePool, store: &Store) -> anyhow::Result<LegacyCoverage> {
    let content: Vec<(String, String)> =
        sqlx::query_as("SELECT url, book_url FROM library_content")
            .fetch_all(pool)
            .await?;
    let images: Vec<(String, String)> = sqlx::query_as("SELECT url, book_url FROM library_images")
        .fetch_all(pool)
        .await?;

    let mut coverage = LegacyCoverage {
        content_rows: content.len() as i64,
        image_rows: images.len() as i64,
        ..Default::default()
    };
    let mut by_book: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
    for (url, book_url) in content {
        by_book.entry(book_url).or_default().0.push(url);
    }
    for (url, book_url) in images {
        by_book.entry(book_url).or_default().1.push(url);
    }

    for (book_url, (content_urls, image_urls)) in by_book {
        if book_url.trim().is_empty() {
            coverage.books_without_dir.push("(book_url 为空)".into());
            continue;
        }
        if store.find(&book_url).is_none() {
            coverage.books_without_dir.push(book_url);
            continue;
        }
        let (covered_content, covered_images) = dir_covered(store, &book_url, &content_urls).await;
        let mut partial = false;
        for url in content_urls {
            if covered_content.contains(&url) {
                coverage.content_covered += 1;
            } else {
                partial = true;
            }
        }
        for url in image_urls {
            if covered_images.contains(&url) {
                coverage.image_covered += 1;
            } else {
                partial = true;
            }
        }
        if partial {
            coverage.books_partial.push(book_url);
        }
    }
    Ok(coverage)
}

/// 目录里已经能读到的章节 URL / 原始图片 URL。解析失败按「没覆盖」处理，
/// 宁可让 operator 再跑一次迁移，也不要误报可以删表。
async fn dir_covered(
    store: &Store,
    book_url: &str,
    content_urls: &[String],
) -> (HashSet<String>, HashSet<String>) {
    let mut content = HashSet::new();
    let mut images = HashSet::new();
    if let Ok(Some(comic)) = store.open_comic(book_url).await {
        for chapter in &comic.meta.chapters {
            if chapter.pages.is_empty() {
                continue;
            }
            if let Some(url) = chapter.url.as_deref() {
                content.insert(url.trim().to_string());
            }
            for page in &chapter.pages {
                if let Some(url) = page.source_url.as_deref() {
                    images.insert(url.trim().to_string());
                }
            }
        }
        return (content, images);
    }
    if let Ok(Some(book)) = store.open_book(book_url).await {
        for url in content_urls {
            let Some(item) = book.meta.chapter_by_url(url) else {
                continue;
            };
            if super::read::load_chapter(&book.dir, item)
                .await
                .ok()
                .flatten()
                .is_some()
            {
                content.insert(url.trim().to_string());
            }
        }
    }
    (content, images)
}

async fn migrate_book(
    pool: &SqlitePool,
    store: &Store,
    book: &OldBook,
    report: &mut MigrateReport,
) -> anyhow::Result<()> {
    let tier = if book.tier == "collection" {
        Tier::Collection
    } else {
        Tier::Temp
    };
    let chapters: Vec<OldChapter> = sqlx::query_as(
        "SELECT url, title, is_volume, \"index\" FROM library_chapters \
         WHERE book_url = ?1 ORDER BY \"index\"",
    )
    .bind(&book.book_url)
    .fetch_all(pool)
    .await?;

    // 只要有任一章节是图片，整本按漫画（CBZ 目录）导出。
    let mut comic = false;
    for ch in &chapters {
        let kind: Option<String> = sqlx::query_scalar(
            "SELECT content_type FROM library_content WHERE url = ?1 AND book_url = ?2 LIMIT 1",
        )
        .bind(&ch.url)
        .bind(&book.book_url)
        .fetch_optional(pool)
        .await?;
        if kind.as_deref() == Some("image") {
            comic = true;
            break;
        }
    }
    if comic {
        report.comics += 1;
        migrate_comic(pool, store, book, tier, &chapters, report).await?;
    } else {
        migrate_text_book(pool, store, book, tier, &chapters, report).await?;
    }
    Ok(())
}

async fn migrate_text_book(
    pool: &SqlitePool,
    store: &Store,
    book: &OldBook,
    tier: Tier,
    chapters: &[OldChapter],
    report: &mut MigrateReport,
) -> anyhow::Result<()> {
    let mut meta = OpfMeta::new(
        &book.book_url,
        book.name.as_deref().unwrap_or(&book.book_url),
    );
    meta.author = book.author.clone();
    meta.language = book
        .language
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| opf::DEFAULT_LANGUAGE.into());
    meta.publisher = book.publisher.clone();
    meta.pubdate = book.pubdate.clone();
    meta.description = book.intro.clone();
    meta.series = book.series_name.clone();
    meta.series_index = book.series_index;

    let tags: Vec<String> =
        sqlx::query_scalar("SELECT tag FROM library_tags WHERE book_url = ?1 ORDER BY tag")
            .bind(&book.book_url)
            .fetch_all(pool)
            .await?;
    meta.subjects = tags;

    let ids: Vec<(String, String)> = sqlx::query_as(
        "SELECT scheme, value FROM library_identifiers WHERE book_url = ?1 ORDER BY scheme, value",
    )
    .bind(&book.book_url)
    .fetch_all(pool)
    .await?;
    meta.isbn = ids
        .iter()
        .find(|(scheme, _)| scheme.eq_ignore_ascii_case("isbn"))
        .map(|(_, value)| value.clone());

    // 封面：只有已经落盘的图片才能随书带走；在线封面继续走图片代理。
    if let Some(cover_url) = book.cover_url.as_deref() {
        if let Some(img) = load_image(pool, cover_url, &book.book_url).await? {
            let ext = ext_for_mime(&img.mime);
            let href = format!("images/cover.{ext}");
            let dir = store.dir_for(tier, &book.book_url);
            let path = dir.join("OEBPS").join(&href);
            super::atomic_write(&path, &img.data).await?;
            meta.cover = Some(CoverItem {
                href,
                media_type: img.mime,
            });
            report.images += 1;
        }
    }

    let dir = store
        .ensure_skeleton(tier, &OpfMeta::new(&book.book_url, &meta.title))
        .await?;

    let mut seq = 0u32;
    for ch in chapters {
        let content = load_content(pool, &ch.url, &book.book_url).await?;
        seq += 1;
        let id = format!("{:04}", seq);
        let id = if ch.is_volume != 0 {
            format!("vol-{id}")
        } else {
            format!("ch-{id}")
        };
        let title = ch
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| format!("第 {} 章", ch.index + 1));
        let slug = super::slug(&title, 24);
        let href = if slug.is_empty() {
            format!("chapters/{id}.xhtml")
        } else {
            format!("chapters/{id}-{slug}.xhtml")
        };
        let item = ChapterItem {
            id,
            href,
            title,
            url: Some(ch.url.clone()),
            is_volume: ch.is_volume != 0,
        };
        // 目录里的每一章都要有文件：未缓存的显式占位，卷标题只写标题，
        // 绝不静默丢章——否则阅读进度与目录会错位。
        let pending = !item.is_volume && content.is_none();
        let paragraphs = if item.is_volume {
            Vec::new()
        } else {
            match content {
                Some(c) => markup_to_paragraphs(c.content.as_deref().unwrap_or("")),
                None => vec!["本章内容尚未缓存".to_string()],
            }
        };
        if pending {
            store
                .write_pending_chapter(&dir, &meta.language, &item, &paragraphs)
                .await?;
        } else {
            store
                .write_chapter(&dir, &meta.language, &item, &paragraphs)
                .await?;
        }
        meta.chapters.push(item);
        report.chapters += 1;
    }

    meta.bump();
    store.write_opf(&dir, &meta).await?;
    store.write_nav(&dir, &meta).await?;
    super::index::sync_book(store, &meta.book_url).await;
    Ok(())
}

async fn migrate_comic(
    pool: &SqlitePool,
    store: &Store,
    book: &OldBook,
    tier: Tier,
    chapters: &[OldChapter],
    report: &mut MigrateReport,
) -> anyhow::Result<()> {
    use super::comic::{ComicChapter, ComicMeta, ComicPage};

    let dir = store.dir_for(tier, &book.book_url);
    std::fs::create_dir_all(&dir)?;

    let title = book.name.as_deref().unwrap_or(&book.book_url);
    let mut meta = ComicMeta::new(&book.book_url, title);
    meta.author = book.author.clone();
    meta.language = book.language.clone();
    meta.publisher = book.publisher.clone();
    meta.pubdate = book.pubdate.clone();
    meta.description = book.intro.clone();
    meta.series = book.series_name.clone();
    meta.series_index = book.series_index;
    meta.subjects =
        sqlx::query_scalar("SELECT tag FROM library_tags WHERE book_url = ?1 ORDER BY tag")
            .bind(&book.book_url)
            .fetch_all(pool)
            .await?;

    for (ci, ch) in chapters.iter().enumerate() {
        let no = ci + 1;
        let is_volume = ch.is_volume != 0;
        let title = ch
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| format!("第 {no} 章"));
        let mut chapter = ComicChapter {
            id: if is_volume {
                format!("vol-{no:04}")
            } else {
                format!("ch-{no:04}")
            },
            title,
            url: Some(ch.url.clone()),
            is_volume,
            pages: Vec::new(),
        };
        // 分卷标题没有图片；未缓存的章节也保留空页列表，保证目录顺序与阅读进度不错位。
        if !is_volume {
            if let Some(content) = load_content(pool, &ch.url, &book.book_url).await? {
                let urls = image_urls(content.images.as_deref());
                for (pi, url) in urls.iter().enumerate() {
                    let Some(img) = load_image(pool, url, &book.book_url).await? else {
                        continue;
                    };
                    let name = format!("{no:04}-{:03}.{}", pi + 1, ext_for_mime(&img.mime));
                    let path = dir.join(&name);
                    super::atomic_write(&path, &img.data).await?;
                    // 旧库可能没存宽高：迁移时顺手读一次图片头，KPW3 切片与一屏一页都要用。
                    let width = img.width.and_then(|v| u32::try_from(v).ok());
                    let height = img.height.and_then(|v| u32::try_from(v).ok());
                    let dims = match (width, height) {
                        (Some(w), Some(h)) => Some((w, h)),
                        _ => super::comic::page_dims(&path),
                    };
                    chapter.pages.push(ComicPage {
                        href: name,
                        mime: img.mime.clone(),
                        width: dims.map(|(w, _)| w).or(width),
                        height: dims.map(|(_, h)| h).or(height),
                        bytes: Some(img.data.len() as u64),
                        source_url: Some(url.clone()),
                    });
                    report.images += 1;
                }
            }
        }
        meta.chapters.push(chapter);
    }

    meta.bump();
    super::comic::write_meta(&dir, &meta).await?;
    super::comic::write_comic_info(&dir, &meta).await?;
    super::index::sync_book(store, &meta.book_url).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// 读取旧库
// ---------------------------------------------------------------------------

async fn load_content(
    pool: &SqlitePool,
    chapter_url: &str,
    book_url: &str,
) -> anyhow::Result<Option<OldContent>> {
    Ok(sqlx::query_as(
        "SELECT content, images FROM library_content \
         WHERE url = ?1 AND book_url = ?2 LIMIT 1",
    )
    .bind(chapter_url)
    .bind(book_url)
    .fetch_optional(pool)
    .await?)
}

async fn load_image(
    pool: &SqlitePool,
    url: &str,
    book_url: &str,
) -> anyhow::Result<Option<OldImage>> {
    Ok(sqlx::query_as(
        "SELECT mime, data, width, height FROM library_images \
         WHERE url = ?1 AND book_url = ?2 LIMIT 1",
    )
    .bind(url)
    .bind(book_url)
    .fetch_optional(pool)
    .await?)
}

fn image_urls(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    if let Ok(list) = serde_json::from_str::<Vec<crate::entities::BookImage>>(raw) {
        return list.into_iter().map(|i| i.url).collect();
    }
    if let Ok(v) = serde_json::from_str::<crate::web_book::ChapterImages>(raw) {
        return v.images.into_iter().map(|i| i.url).collect();
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// 归一化
// ---------------------------------------------------------------------------

/// 迁移后扫描一遍目录，确认每本书至少有一份可打开的入口文件。
pub fn verify_tree(store: &Store, report: &mut MigrateReport) {
    for tier in [Tier::Collection, Tier::Temp] {
        let dir = store.root().join(tier.as_str());
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let epub = path.join("OEBPS/content.opf");
            if !epub.is_file() && !super::comic::is_comic_dir(&path) {
                report.fail(
                    path.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
                    "目录里既没有 OEBPS/content.opf 也没有漫画元数据",
                );
            }
        }
    }
}

/// 只给测试与 doctor 用：判断某个目录是否是漫画目录。
pub fn is_comic_dir(dir: &Path) -> bool {
    super::comic::is_comic_dir(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_drop_markup_but_keep_text() {
        let out = markup_to_paragraphs("<h1>标题</h1><p>第一段<br/>第二行</p><script>x</script>");
        assert!(out.contains(&"标题".to_string()));
        assert!(out.iter().any(|p| p.contains("第一段")));
        assert!(!out.iter().any(|p| p.contains("script")));
    }

    #[test]
    fn image_urls_accepts_both_shapes() {
        let flat = r#"[{"index":0,"url":"https://a/1.jpg"}]"#;
        assert_eq!(image_urls(Some(flat)), vec!["https://a/1.jpg".to_string()]);
        let wrapped = r#"{"images":[{"index":0,"url":"https://a/2.jpg"}],"truncated":false}"#;
        assert_eq!(
            image_urls(Some(wrapped)),
            vec!["https://a/2.jpg".to_string()]
        );
    }

    #[test]
    fn ext_mapping_is_safe() {
        assert_eq!(ext_for_mime("image/png"), "png");
        assert_eq!(ext_for_mime("image/webp"), "webp");
        assert_eq!(ext_for_mime("image/jpeg"), "jpg");
        assert_eq!(ext_for_mime(""), "jpg");
    }

    #[tokio::test]
    async fn legacy_content_probe_handles_missing_and_empty_tables() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        // 未来删表后启动不应报错，也不应尝试迁移。
        assert!(!legacy_content_present(&pool).await.unwrap());

        sqlx::query("CREATE TABLE library_content (url TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE library_images (url TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        assert!(!legacy_content_present(&pool).await.unwrap());

        sqlx::query("INSERT INTO library_content (url) VALUES ('https://x/c1')")
            .execute(&pool)
            .await
            .unwrap();
        assert!(legacy_content_present(&pool).await.unwrap());
    }

    #[tokio::test]
    async fn legacy_coverage_separates_covered_and_db_only_rows() {
        use crate::entities::BookChapter;
        use crate::library_store::comic;
        use crate::library_store::write::{save_text_chapter, sync_text_toc, TocSeed};

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("oread-coverage-{}-{stamp}", std::process::id()));
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();

        // 文字书：t1 已落盘，t2 只在 DB。
        let text_url = "https://x/text";
        let seed = TocSeed {
            tier: Tier::Temp,
            is_comic: false,
            opf: OpfMeta::new(text_url, "文字书"),
            comic: super::super::comic::ComicMeta::new(text_url, "文字书"),
        };
        let chapter = |url: &str| BookChapter {
            url: url.into(),
            title: Some("章".into()),
            ..Default::default()
        };
        sync_text_toc(
            &store,
            &seed,
            &[chapter("https://x/t1"), chapter("https://x/t2")],
        )
        .await
        .unwrap();
        assert!(
            save_text_chapter(&store, text_url, "https://x/t1", &["正文".into()])
                .await
                .unwrap()
        );

        // 漫画书：i1 已落盘，i2 只在 DB。
        let comic_url = "https://x/comic";
        let mut meta = super::super::comic::ComicMeta::new(comic_url, "漫画书");
        meta.chapters.push(super::super::comic::ComicChapter {
            id: "ch-0001".into(),
            title: "第一话".into(),
            url: Some("https://x/c1".into()),
            is_volume: false,
            pages: vec![super::super::comic::ComicPage {
                href: "0001-001.jpg".into(),
                mime: "image/jpeg".into(),
                width: Some(1),
                height: Some(1),
                bytes: Some(1),
                source_url: Some("https://x/i1.jpg".into()),
            }],
        });
        comic::write_meta(&store.dir_for(Tier::Temp, comic_url), &meta)
            .await
            .unwrap();

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for sql in [
            "CREATE TABLE library_content (url TEXT, book_url TEXT)",
            "CREATE TABLE library_images (url TEXT, book_url TEXT)",
        ] {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        for (url, book) in [
            ("https://x/t1", text_url),
            ("https://x/t2", text_url),
            ("https://x/c1", comic_url),
            ("https://x/z1", "https://x/none"),
        ] {
            sqlx::query("INSERT INTO library_content (url, book_url) VALUES (?1, ?2)")
                .bind(url)
                .bind(book)
                .execute(&pool)
                .await
                .unwrap();
        }
        for (url, book) in [
            ("https://x/i1.jpg", comic_url),
            ("https://x/i2.jpg", comic_url),
        ] {
            sqlx::query("INSERT INTO library_images (url, book_url) VALUES (?1, ?2)")
                .bind(url)
                .bind(book)
                .execute(&pool)
                .await
                .unwrap();
        }

        let coverage = legacy_coverage(&pool, &store).await.unwrap();
        assert_eq!(coverage.content_rows, 4);
        assert_eq!(coverage.content_covered, 2);
        assert_eq!(coverage.content_only_db(), 2);
        assert_eq!(coverage.image_rows, 2);
        assert_eq!(coverage.image_covered, 1);
        assert_eq!(coverage.image_only_db(), 1);
        assert_eq!(
            coverage.books_without_dir,
            vec!["https://x/none".to_string()]
        );
        assert!(coverage.books_partial.contains(&text_url.to_string()));
        assert!(coverage.books_partial.contains(&comic_url.to_string()));
        assert!(!coverage.complete());

        std::fs::remove_dir_all(root).ok();
    }
}
