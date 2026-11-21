//! 一次性迁移：把旧 `library_content` / `library_images` 里的正文与图片导出成
//! 「目录即书」结构。
//!
//! 迁移只读旧表、只写目录，**不删除**任何数据库行；切换读写路径并验证无误后，
//! 再由后续迁移删表。已存在的书目录默认跳过，避免覆盖管理员刚编辑过的内容。

use std::path::Path;

use sqlx::SqlitePool;

use super::{opf, ChapterItem, CoverItem, OpfMeta, Store, Tier};
use boranlib_ebook::html::{to_blocks, Block};

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

    let mut page_count = 0u32;
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
                    page_count += 1;
                    report.images += 1;
                }
            }
        }
        meta.chapters.push(chapter);
    }

    meta.bump();
    super::comic::write_meta(&dir, &meta).await?;
    let comic_info = comic_info_xml(book, page_count);
    super::atomic_write(&dir.join("ComicInfo.xml"), comic_info.as_bytes()).await?;
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

/// 旧正文 HTML → 段落。标题块并入段落（章节标题已在 OPF 里，避免重复）。
pub fn markup_to_paragraphs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for block in to_blocks(html) {
        match block {
            Block::Paragraph(text) => {
                if !text.trim().is_empty() {
                    out.push(text);
                }
            }
            Block::Heading(_, text) => {
                if !text.trim().is_empty() {
                    out.push(text);
                }
            }
        }
    }
    out
}

fn ext_for_mime(mime: &str) -> &'static str {
    let m = mime.to_ascii_lowercase();
    if m.contains("png") {
        "png"
    } else if m.contains("webp") {
        "webp"
    } else if m.contains("gif") {
        "gif"
    } else if m.contains("avif") {
        "avif"
    } else {
        "jpg"
    }
}

fn comic_info_xml(book: &OldBook, pages: u32) -> String {
    use std::fmt::Write as _;
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <ComicInfo xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">\n",
    );
    if let Some(title) = book.name.as_deref().filter(|t| !t.trim().is_empty()) {
        let _ = writeln!(
            s,
            "  <Title>{}</Title>",
            boranlib_ebook::xml::xml_escape(title)
        );
    }
    if let Some(author) = book.author.as_deref().filter(|t| !t.trim().is_empty()) {
        let _ = writeln!(
            s,
            "  <Writer>{}</Writer>",
            boranlib_ebook::xml::xml_escape(author)
        );
    }
    if let Some(series) = book.series_name.as_deref().filter(|t| !t.trim().is_empty()) {
        let _ = writeln!(
            s,
            "  <Series>{}</Series>",
            boranlib_ebook::xml::xml_escape(series)
        );
    }
    if let Some(intro) = book.intro.as_deref().filter(|t| !t.trim().is_empty()) {
        let _ = writeln!(
            s,
            "  <Summary>{}</Summary>",
            boranlib_ebook::xml::xml_escape(intro)
        );
    }
    if let Some(lang) = book.language.as_deref().filter(|t| !t.trim().is_empty()) {
        let _ = writeln!(
            s,
            "  <LanguageISO>{}</LanguageISO>",
            boranlib_ebook::xml::xml_escape(lang)
        );
    }
    let _ = writeln!(s, "  <PageCount>{pages}</PageCount>");
    s.push_str("</ComicInfo>\n");
    s
}

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
}
