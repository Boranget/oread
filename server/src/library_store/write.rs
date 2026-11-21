//! 写路径：把抓取结果直接写进「目录即书」的书目录。
//!
//! 纪律与 [`super`] 保持一致：
//! - 先写章节/图片文件，再 `nav.xhtml`，最后 `content.opf`；
//! - 同一本书的写入串行化（[`lock_book`]）；
//! - 写目录失败只记日志，旧 `library.db` 仍是兜底，读路径不会因此打不开书。
//!
//! 目录是权威：同步目录时**保留已有章节 id/href**（按章节 URL 匹配），
//! 新章节分配新序号，删除的章节只从 spine 移除，文件名永不复用。

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use sqlx::SqlitePool;

use super::comic::{self, ComicChapter, ComicMeta, ComicPage};
use super::read::{body_blocks, read_chapter_file};
use super::{lock_book, ChapterItem, OpfMeta, Store, Tier};
use crate::entities::BookChapter;
use boranlib_ebook::html::{to_blocks, Block};

/// 旧正文 HTML → 段落。标题块也并入段落（章节标题由 OPF 单独保存，避免重复）。
pub fn markup_to_paragraphs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for block in to_blocks(html) {
        match block {
            Block::Paragraph(text) | Block::Heading(_, text) => {
                if !text.trim().is_empty() {
                    out.push(text);
                }
            }
        }
    }
    out
}

fn paragraphs_of(xhtml: &str, title: &str) -> Vec<String> {
    body_blocks(xhtml, title)
        .into_iter()
        .map(|b| match b {
            Block::Heading(_, text) | Block::Paragraph(text) => text,
        })
        .filter(|text| !text.trim().is_empty())
        .collect()
}

/// 一本书在数据库里的元数据投影：建目录时用它初始化 OPF / `oread.json`。
#[derive(Debug, Clone)]
pub struct TocSeed {
    pub tier: Tier,
    pub is_comic: bool,
    pub opf: OpfMeta,
    pub comic: ComicMeta,
}

#[derive(sqlx::FromRow)]
struct SeedRow {
    name: Option<String>,
    author: Option<String>,
    language: Option<String>,
    publisher: Option<String>,
    pubdate: Option<String>,
    intro: Option<String>,
    series_name: Option<String>,
    series_index: Option<f64>,
    tier: String,
    book_source_type: i64,
}

/// 从 `library_books` / 标签 / 标识组装目录种子。书不存在时返回 `None`。
pub async fn seed_from_db(pool: &SqlitePool, book_url: &str) -> anyhow::Result<Option<TocSeed>> {
    let row: Option<SeedRow> = sqlx::query_as(
        "SELECT name, author, language, publisher, pubdate, intro, series_name, \
                series_index, tier, book_source_type \
         FROM library_books WHERE book_url = ?1",
    )
    .bind(book_url)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let tags: Vec<String> =
        sqlx::query_scalar("SELECT tag FROM library_tags WHERE book_url = ?1 ORDER BY tag")
            .bind(book_url)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let isbn: Option<String> = sqlx::query_scalar(
        "SELECT value FROM library_identifiers \
         WHERE book_url = ?1 AND lower(scheme) = 'isbn' ORDER BY value LIMIT 1",
    )
    .bind(book_url)
    .fetch_optional(pool)
    .await?;
    // 类型字段缺失的老库：只要有过图片章节就按漫画处理（与迁移判定一致）。
    let has_image: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM library_content \
         WHERE book_url = ?1 AND content_type = 'image' LIMIT 1",
    )
    .bind(book_url)
    .fetch_optional(pool)
    .await?;
    let is_comic = row.book_source_type == 2 || has_image.is_some();

    let title = row
        .name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| book_url.to_string());
    let language = row
        .language
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| super::opf::DEFAULT_LANGUAGE.to_string());
    let tier = if row.tier == "collection" {
        Tier::Collection
    } else {
        Tier::Temp
    };

    let mut opf = OpfMeta::new(book_url, &title);
    opf.language = language.clone();
    opf.author = row.author.clone();
    opf.publisher = row.publisher.clone();
    opf.pubdate = row.pubdate.clone();
    opf.description = row.intro.clone();
    opf.series = row.series_name.clone();
    opf.series_index = row.series_index;
    opf.subjects = tags.clone();
    opf.isbn = isbn;

    let mut meta = ComicMeta::new(book_url, &title);
    meta.author = row.author;
    meta.language = Some(language);
    meta.publisher = row.publisher;
    meta.pubdate = row.pubdate;
    meta.description = row.intro;
    meta.series = row.series_name;
    meta.series_index = row.series_index;
    meta.subjects = tags;

    Ok(Some(TocSeed {
        tier,
        is_comic,
        opf,
        comic: meta,
    }))
}

/// 目录同步入口：按书源类型写 OPF（文字书）或 `oread.json`（漫画）。
pub async fn sync_toc(
    pool: &SqlitePool,
    store: &Store,
    book_url: &str,
    chapters: &[BookChapter],
) -> anyhow::Result<()> {
    let Some(seed) = seed_from_db(pool, book_url).await? else {
        return Ok(());
    };
    if seed.is_comic {
        sync_comic_toc(store, &seed, chapters).await?;
    } else {
        sync_text_toc(store, &seed, chapters).await?;
    }
    Ok(())
}

fn chapter_id(is_volume: bool, seq: u32) -> String {
    format!("{}{:04}", if is_volume { "vol-" } else { "ch-" }, seq)
}

/// 只补空字段：OPF 是权威，DB 只负责把书源刚提供的元数据带进来。
fn fill_text_meta(meta: &mut OpfMeta, seed: &OpfMeta) {
    // 目录只建了骨架时 title 还是 URL（`save_chapters` 先插了空书目行），
    // 拿到真实书名后应该补上。
    if meta.title.trim().is_empty() || meta.title == meta.book_url {
        meta.title = seed.title.clone();
    }
    if meta
        .author
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        meta.author = seed.author.clone();
    }
    if meta
        .publisher
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        meta.publisher = seed.publisher.clone();
    }
    if meta
        .pubdate
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        meta.pubdate = seed.pubdate.clone();
    }
    if meta
        .description
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        meta.description = seed.description.clone();
    }
    if meta
        .series
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        meta.series = seed.series.clone();
        meta.series_index = seed.series_index;
    }
    if meta.isbn.as_deref().map(str::trim).unwrap_or("").is_empty() {
        meta.isbn = seed.isbn.clone();
    }
    if meta.subjects.is_empty() {
        meta.subjects = seed.subjects.clone();
    }
}

/// 章节文件名/ID 里的序号：`ch-0007`、`vol-0012`、`ch-0007-qi-dian.xhtml` 都取数字段。
fn id_seq(name: &str) -> Option<u32> {
    let rest = name
        .strip_prefix("ch-")
        .or_else(|| name.strip_prefix("vol-"))?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 4 {
        digits.parse().ok()
    } else {
        None
    }
}

/// 扫描目录里已经存在的章节文件，返回下一个可用序号。
///
/// 只看 spine 不够：被移出目录的章节文件仍留在磁盘上（文件名永不复用），
/// 新章必须跳过这些序号，否则会覆盖旧文件、打断阅读进度。
async fn next_seq_in_dir(dir: &Path, comic_layout: bool) -> io::Result<u32> {
    let mut max = 0u32;
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(1),
        Err(e) => return Err(e),
    };
    while let Some(entry) = rd.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let seq = if comic_layout {
            // CBZ 扁平页名 `0001-001.jpg`：前 4+ 位是章节序号。
            name.split_once('-')
                .map(|(head, _)| head)
                .filter(|head| head.len() >= 4 && head.chars().all(|c| c.is_ascii_digit()))
                .and_then(|head| head.parse::<u32>().ok())
        } else {
            id_seq(&name)
        };
        if let Some(n) = seq {
            max = max.max(n);
        }
    }
    Ok(max + 1)
}

fn chapter_title(ch: &BookChapter, pos: usize) -> String {
    ch.title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("第 {} 章", pos + 1))
}

// ---------------------------------------------------------------------------
// 文字书
// ---------------------------------------------------------------------------

/// 把整份目录同步进 OPF spine。已有章节按 URL 复用 id/href，只更新标题与卷标记。
pub async fn sync_text_toc(
    store: &Store,
    seed: &TocSeed,
    chapters: &[BookChapter],
) -> io::Result<()> {
    let book_url = &seed.opf.book_url;
    let _guard = lock_book(book_url).await;
    let dir = store.ensure_dirs(seed.tier, book_url).await?;
    let mut meta = match store.read_opf(&dir).await {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => seed.opf.clone(),
        Err(e) => {
            tracing::warn!(book = %book_url, error = %e, "read opf failed; rebuilding from db");
            seed.opf.clone()
        }
    };
    // 目录是元数据权威：只补空字段，不覆盖管理员已经编辑过的值。
    fill_text_meta(&mut meta, &seed.opf);
    merge_text_chapters(store, &dir, &mut meta, chapters).await?;
    meta.bump();
    // 顺序固定：章节文件（上面已写）→ nav → opf。
    store.write_nav(&dir, &meta).await?;
    store.write_opf(&dir, &meta).await
}

async fn merge_text_chapters(
    store: &Store,
    dir: &Path,
    meta: &mut OpfMeta,
    incoming: &[BookChapter],
) -> io::Result<()> {
    let language = meta.language.clone();
    let old = std::mem::take(&mut meta.chapters);
    let dir_next = next_seq_in_dir(&dir.join("OEBPS").join("chapters"), false).await?;
    let mut next = super::next_chapter_seq(old.iter().map(|c| c.id.as_str())).max(dir_next);
    let mut by_url: HashMap<String, ChapterItem> = old
        .into_iter()
        .filter_map(|c| {
            let key = c.url.as_ref().map(|u| u.trim().to_string());
            key.map(|k| (k, c))
        })
        .collect();

    let mut out = Vec::with_capacity(incoming.len());
    for (pos, ch) in incoming.iter().enumerate() {
        let url = ch.url.trim();
        if url.is_empty() {
            continue;
        }
        let title = chapter_title(ch, pos);
        match by_url.remove(url) {
            Some(mut item) => {
                if item.title != title {
                    retitle_chapter(store, dir, &language, &item, &title).await?;
                    item.title = title;
                }
                item.is_volume = ch.is_volume;
                out.push(item);
            }
            None => {
                let seq = next;
                next += 1;
                let id = chapter_id(ch.is_volume, seq);
                let item = ChapterItem {
                    id: id.clone(),
                    href: format!("chapters/{id}.xhtml"),
                    title,
                    url: Some(url.to_string()),
                    is_volume: ch.is_volume,
                };
                write_new_chapter(store, dir, &language, &item).await?;
                out.push(item);
            }
        }
    }
    meta.chapters = out;
    Ok(())
}

async fn write_new_chapter(
    store: &Store,
    dir: &Path,
    language: &str,
    item: &ChapterItem,
) -> io::Result<()> {
    if item.is_volume {
        store.write_chapter(dir, language, item, &[]).await
    } else {
        store
            .write_pending_chapter(dir, language, item, &["本章内容尚未缓存".to_string()])
            .await
    }
}

/// 标题变化时重写章节文件：读回已归一化的段落，用新标题重新序列化，
/// 避免旧 `<h2>` 在阅读器里变成正文。
async fn retitle_chapter(
    store: &Store,
    dir: &Path,
    language: &str,
    item: &ChapterItem,
    new_title: &str,
) -> io::Result<()> {
    let updated = ChapterItem {
        title: new_title.to_string(),
        ..item.clone()
    };
    let Some(xhtml) = read_chapter_file(dir, &item.href).await? else {
        return write_new_chapter(store, dir, language, &updated).await;
    };
    let pending = super::is_pending_html(&xhtml);
    let mut paragraphs = paragraphs_of(&xhtml, &item.title);
    if item.is_volume {
        return store.write_chapter(dir, language, &updated, &[]).await;
    }
    if pending {
        if paragraphs.is_empty() {
            paragraphs.push("本章内容尚未缓存".to_string());
        }
        store
            .write_pending_chapter(dir, language, &updated, &paragraphs)
            .await
    } else {
        store
            .write_chapter(dir, language, &updated, &paragraphs)
            .await
    }
}

/// 写一章正文。目录里没有这本书/这一章时返回 `Ok(false)`，调用方继续依赖 DB。
pub async fn save_text_chapter(
    store: &Store,
    book_url: &str,
    chapter_url: &str,
    paragraphs: &[String],
) -> io::Result<bool> {
    if paragraphs.is_empty() {
        return Ok(false);
    }
    let _guard = lock_book(book_url).await;
    let Some(opened) = store.open_book(book_url).await? else {
        return Ok(false);
    };
    let Some(idx) = opened.meta.chapter_index_by_ref(book_url, chapter_url) else {
        return Ok(false);
    };
    let item = &opened.meta.chapters[idx];
    if item.is_volume {
        return Ok(false);
    }
    // 抓取层常把章节标题也放在正文第一段；OPF 已单独保存标题，正文里去掉，
    // 避免阅读器重复显示。
    let mut paragraphs = paragraphs.to_vec();
    if paragraphs
        .first()
        .map(|p| p.trim() == item.title.trim())
        .unwrap_or(false)
    {
        paragraphs.remove(0);
    }
    if paragraphs.is_empty() {
        return Ok(false);
    }
    store
        .write_chapter(&opened.dir, &opened.meta.language, item, &paragraphs)
        .await?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// 漫画
// ---------------------------------------------------------------------------

/// 把整份目录同步进 `oread.json`。已有章节按 URL 复用 id 与已下载页表。
pub async fn sync_comic_toc(
    store: &Store,
    seed: &TocSeed,
    chapters: &[BookChapter],
) -> io::Result<()> {
    let book_url = &seed.comic.book_url;
    let _guard = lock_book(book_url).await;
    let dir = store.dir_for(seed.tier, book_url);
    tokio::fs::create_dir_all(&dir).await?;
    let mut meta = if dir.join(comic::META_FILE).is_file() {
        comic::read_meta(&dir, book_url).await?
    } else {
        seed.comic.clone()
    };
    fill_comic_meta(&mut meta, &seed.comic);

    let old = std::mem::take(&mut meta.chapters);
    let dir_next = next_seq_in_dir(&dir, true).await?;
    let mut next = super::next_chapter_seq(old.iter().map(|c| c.id.as_str())).max(dir_next);
    let mut by_url: HashMap<String, ComicChapter> = old
        .into_iter()
        .filter_map(|c| {
            let key = c.url.as_ref().map(|u| u.trim().to_string());
            key.map(|k| (k, c))
        })
        .collect();

    let mut out = Vec::with_capacity(chapters.len());
    for (pos, ch) in chapters.iter().enumerate() {
        let url = ch.url.trim();
        if url.is_empty() {
            continue;
        }
        let title = chapter_title(ch, pos);
        match by_url.remove(url) {
            Some(mut item) => {
                item.title = title;
                item.is_volume = ch.is_volume;
                out.push(item);
            }
            None => {
                let seq = next;
                next += 1;
                out.push(ComicChapter {
                    id: chapter_id(ch.is_volume, seq),
                    title,
                    url: Some(url.to_string()),
                    is_volume: ch.is_volume,
                    pages: Vec::new(),
                });
            }
        }
    }
    meta.chapters = out;
    meta.bump();
    comic::write_meta(&dir, &meta).await?;
    comic::write_comic_info(&dir, &meta).await
}

/// 漫画版「只补空字段」，口径与 [`fill_text_meta`] 一致。
fn fill_comic_meta(meta: &mut ComicMeta, seed: &ComicMeta) {
    if meta.title.trim().is_empty() || meta.title == meta.book_url {
        meta.title = seed.title.clone();
    }
    if meta
        .author
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        meta.author = seed.author.clone();
    }
    if meta
        .language
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        meta.language = seed.language.clone();
    }
    if meta
        .publisher
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        meta.publisher = seed.publisher.clone();
    }
    if meta
        .pubdate
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        meta.pubdate = seed.pubdate.clone();
    }
    if meta
        .description
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        meta.description = seed.description.clone();
    }
    if meta
        .series
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        meta.series = seed.series.clone();
        meta.series_index = seed.series_index;
    }
    if meta.isbn.as_deref().map(str::trim).unwrap_or("").is_empty() {
        meta.isbn = seed.isbn.clone();
    }
    if meta.subjects.is_empty() {
        meta.subjects = seed.subjects.clone();
    }
}

/// 一章的图片（已解密、已落库的原始字节）。
pub struct ComicPageInput {
    pub data: Vec<u8>,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub source_url: Option<String>,
}

/// 把一整章的图片写进书目录，并更新 `oread.json` / `ComicInfo.xml`。
///
/// 只在**整章图片都拿到**时调用：部分成功不写页表，读路径继续回退 DB，
/// 避免本地目录只显示半章。
pub async fn save_comic_pages(
    store: &Store,
    book_url: &str,
    chapter_url: &str,
    pages: &[ComicPageInput],
) -> io::Result<bool> {
    if pages.is_empty() {
        return Ok(false);
    }
    let _guard = lock_book(book_url).await;
    let Some(opened) = store.open_comic(book_url).await? else {
        return Ok(false);
    };
    let Some(pos) = opened.meta.chapters.iter().position(|c| {
        c.url.as_deref().map(str::trim) == Some(chapter_url.trim())
            || crate::ebook_source::local_chapter_id(book_url, chapter_url)
                .map(|id| id == c.id)
                .unwrap_or(false)
    }) else {
        return Ok(false);
    };
    let chapter = &opened.meta.chapters[pos];
    if chapter.is_volume {
        return Ok(false);
    }
    // 章节序号来自稳定 id（`ch-0007`），与页文件名绑定，插入新章不会改名。
    let no = chapter
        .id
        .rsplit_once('-')
        .and_then(|(_, n)| n.parse::<u32>().ok())
        .unwrap_or(pos as u32 + 1);

    let mut written = Vec::with_capacity(pages.len());
    for (i, page) in pages.iter().enumerate() {
        let ext = comic::ext_for_mime(&page.mime);
        let href = format!("{no:04}-{:03}.{ext}", i + 1);
        super::atomic_write(&opened.dir.join(&href), &page.data).await?;
        written.push(ComicPage {
            href,
            mime: page.mime.clone(),
            width: page.width,
            height: page.height,
            bytes: Some(page.data.len() as u64),
            source_url: page.source_url.clone(),
        });
    }

    // 页数变少时删掉旧的多余页：CBZ 阅读器会把目录里的图片全显示出来，
    // 留在磁盘上的旧页会变成幽灵页。只删本目录下的扁平文件。
    let keep: HashSet<&str> = written.iter().map(|p| p.href.as_str()).collect();
    for old in &chapter.pages {
        if keep.contains(old.href.as_str()) {
            continue;
        }
        let Ok(path) = super::safe_join(&opened.dir, &old.href) else {
            continue;
        };
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(book = %book_url, href = %old.href, error = %e, "remove stale comic page failed")
            }
        }
    }

    let mut meta = opened.meta;
    meta.chapters[pos].pages = written;
    meta.bump();
    comic::write_meta(&opened.dir, &meta).await?;
    comic::write_comic_info(&opened.dir, &meta).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("oread-write-{tag}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed(book_url: &str, tier: Tier) -> TocSeed {
        let mut opf = OpfMeta::new(book_url, "测试书");
        opf.author = Some("作者".into());
        let mut comic = ComicMeta::new(book_url, "测试书");
        comic.author = Some("作者".into());
        TocSeed {
            tier,
            is_comic: false,
            opf,
            comic,
        }
    }

    fn chapter(url: &str, title: &str, is_volume: bool) -> BookChapter {
        BookChapter {
            url: url.into(),
            title: Some(title.into()),
            is_volume,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn toc_sync_reuses_ids_and_appends_new_chapters() {
        let root = temp_root("toc");
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let book_url = "https://x/book/toc";
        let seed = seed(book_url, Tier::Temp);

        let first = vec![chapter("https://x/c1", "第一章", false)];
        sync_text_toc(&store, &seed, &first).await.unwrap();
        let opened = store.open_book(book_url).await.unwrap().unwrap();
        let id1 = opened.meta.chapters[0].id.clone();
        let href1 = opened.meta.chapters[0].href.clone();
        assert_eq!(id1, "ch-0001");
        assert!(opened.dir.join("OEBPS").join(&href1).is_file());

        // 目录前置插入新章：旧章节 id/href 必须原样保留。
        let second = vec![
            chapter("https://x/c0", "序章", false),
            chapter("https://x/c1", "第一章（改）", false),
        ];
        sync_text_toc(&store, &seed, &second).await.unwrap();
        let opened = store.open_book(book_url).await.unwrap().unwrap();
        assert_eq!(opened.meta.chapters.len(), 2);
        assert_eq!(opened.meta.chapters[0].id, "ch-0002");
        assert_eq!(opened.meta.chapters[1].id, id1);
        assert_eq!(opened.meta.chapters[1].href, href1);
        assert_eq!(opened.meta.chapters[1].title, "第一章（改）");
        // 旧章节被重写为新标题，且仍是占位章。
        let html = store
            .read_chapter(&opened.dir, &href1)
            .await
            .unwrap()
            .unwrap();
        assert!(html.contains("第一章（改）"));
        assert!(html.contains("oread-pending"));
        // 标题重写不会把旧标题留在正文里。
        assert!(!html.contains("<p>第一章</p>"));

        // 目录缩短：只从 spine 移除，不删文件、不复用序号。
        sync_text_toc(&store, &seed, &first[..1]).await.unwrap();
        let opened = store.open_book(book_url).await.unwrap().unwrap();
        assert_eq!(opened.meta.chapters.len(), 1);
        let third = vec![
            chapter("https://x/c1", "第一章", false),
            chapter("https://x/c9", "新章", false),
        ];
        sync_text_toc(&store, &seed, &third).await.unwrap();
        let opened = store.open_book(book_url).await.unwrap().unwrap();
        assert_eq!(opened.meta.chapters[1].id, "ch-0003");
        assert!(opened.dir.join("OEBPS").join(&href1).is_file());

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn save_text_chapter_requires_registered_chapter() {
        let root = temp_root("content");
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let book_url = "https://x/book/content";
        let seed = seed(book_url, Tier::Temp);
        sync_text_toc(&store, &seed, &[chapter("https://x/c1", "第一章", false)])
            .await
            .unwrap();

        let wrote = save_text_chapter(
            &store,
            book_url,
            "https://x/c1",
            &["正文一".into(), "正文二".into()],
        )
        .await
        .unwrap();
        assert!(wrote);
        let opened = store.open_book(book_url).await.unwrap().unwrap();
        let body = super::super::read::load_chapter(&opened.dir, &opened.meta.chapters[0])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(body.text, "正文一\n正文二");

        // 未登记章节不落盘。
        assert!(
            !save_text_chapter(&store, book_url, "https://x/missing", &["x".into()])
                .await
                .unwrap()
        );

        // 正文首段与章节标题相同：OPF 已有标题，落盘时去掉，避免重复显示。
        assert!(save_text_chapter(
            &store,
            book_url,
            "https://x/c1",
            &["第一章".into(), "正文三".into()]
        )
        .await
        .unwrap());
        let opened = store.open_book(book_url).await.unwrap().unwrap();
        let body = super::super::read::load_chapter(&opened.dir, &opened.meta.chapters[0])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(body.text, "正文三");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn comic_toc_and_pages_round_trip() {
        let root = temp_root("comic");
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let book_url = "https://x/comic";
        let mut seed = seed(book_url, Tier::Temp);
        seed.is_comic = true;
        let chapters = vec![
            chapter("https://x/c1", "第一话", false),
            chapter("https://x/c2", "第二话", false),
        ];
        sync_comic_toc(&store, &seed, &chapters).await.unwrap();
        let opened = store.open_comic(book_url).await.unwrap().unwrap();
        assert_eq!(opened.meta.chapters.len(), 2);
        assert_eq!(opened.meta.chapters[0].id, "ch-0001");
        assert!(opened.dir.join(comic::META_FILE).is_file());
        assert!(opened.dir.join(comic::COMIC_INFO_FILE).is_file());

        let pages = vec![
            ComicPageInput {
                data: b"one".to_vec(),
                mime: "image/jpeg".into(),
                width: Some(100),
                height: Some(200),
                source_url: Some("https://cdn/1.jpg".into()),
            },
            ComicPageInput {
                data: b"two".to_vec(),
                mime: "image/png".into(),
                width: Some(120),
                height: Some(240),
                source_url: Some("https://cdn/2.png".into()),
            },
        ];
        assert!(save_comic_pages(&store, book_url, "https://x/c1", &pages)
            .await
            .unwrap());
        let opened = store.open_comic(book_url).await.unwrap().unwrap();
        let ch = &opened.meta.chapters[0];
        assert_eq!(ch.pages.len(), 2);
        assert_eq!(ch.pages[0].href, "0001-001.jpg");
        assert_eq!(ch.pages[1].href, "0001-002.png");
        assert_eq!(
            comic::read_page(&opened.dir, &ch.pages[1].href)
                .await
                .unwrap()
                .unwrap(),
            b"two"
        );

        // 再次同步目录：页表与章节 id 都要保留。
        sync_comic_toc(&store, &seed, &chapters).await.unwrap();
        let opened = store.open_comic(book_url).await.unwrap().unwrap();
        assert_eq!(opened.meta.chapters[0].id, "ch-0001");
        assert_eq!(opened.meta.chapters[0].pages.len(), 2);

        // 页数变少：旧的多余页文件要删掉，否则 CBZ 阅读器会显示幽灵页。
        let fewer = vec![ComicPageInput {
            data: b"one".to_vec(),
            mime: "image/jpeg".into(),
            width: Some(100),
            height: Some(200),
            source_url: None,
        }];
        assert!(save_comic_pages(&store, book_url, "https://x/c1", &fewer)
            .await
            .unwrap());
        assert!(opened.dir.join("0001-001.jpg").is_file());
        assert!(!opened.dir.join("0001-002.png").exists());
        let opened = store.open_comic(book_url).await.unwrap().unwrap();
        assert_eq!(opened.meta.chapters[0].pages.len(), 1);

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn markup_normalization_keeps_paragraph_order() {
        let out = markup_to_paragraphs("<h1>标题</h1><p>甲<br/>乙</p><script>x</script>");
        assert_eq!(out, vec!["标题", "甲", "乙"]);
    }
}
