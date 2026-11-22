//! 管理员编辑：直接改书目录里的权威文件。
//!
//! v1 只做四件事：改元数据、改正文段落、换封面、换漫画单页。章节增删/拆分/合并
//! 不在本期范围。所有写入都走 [`lock_book`]，并按「正文/图片 → nav → OPF」
//! 的顺序落盘；修订号不匹配时返回冲突，避免两个编辑者互相覆盖。

use std::fmt;
use std::io;
use std::path::Path;

use super::comic::{self, ComicMeta, ComicPage};
use super::read::body_blocks;
use super::{atomic_write, lock_book, safe_join, ChapterItem, CoverItem, OpfMeta, Store};
use boranlib_ebook::html::Block;

#[derive(Debug)]
pub enum EditError {
    NotFound(String),
    BadRequest(String),
    Conflict(String),
    Io(io::Error),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::NotFound(m) | EditError::BadRequest(m) | EditError::Conflict(m) => {
                f.write_str(m)
            }
            EditError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for EditError {
    fn from(e: io::Error) -> Self {
        EditError::Io(e)
    }
}

/// 一次元数据编辑。`None` = 保持不变；`Some("")` = 清空。
#[derive(Debug, Clone, Default)]
pub struct MetaFields {
    pub title: Option<String>,
    pub author: Option<String>,
    pub language: Option<String>,
    pub publisher: Option<String>,
    pub pubdate: Option<String>,
    pub description: Option<String>,
    pub subjects: Option<Vec<String>>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub isbn: Option<String>,
}

impl MetaFields {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.author.is_none()
            && self.language.is_none()
            && self.publisher.is_none()
            && self.pubdate.is_none()
            && self.description.is_none()
            && self.subjects.is_none()
            && self.series.is_none()
            && self.series_index.is_none()
            && self.isbn.is_none()
    }
}

fn non_empty(v: &str) -> Option<String> {
    let t = v.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn clean_subjects(list: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for item in list {
        if let Some(tag) = non_empty(item) {
            if !out.contains(&tag) {
                out.push(tag);
            }
        }
    }
    out
}

fn apply_text(meta: &mut OpfMeta, fields: &MetaFields) -> Result<(), EditError> {
    if let Some(v) = &fields.title {
        let title = non_empty(v).ok_or_else(|| EditError::BadRequest("书名不能为空".into()))?;
        meta.title = title;
    }
    if let Some(v) = &fields.author {
        meta.author = non_empty(v);
    }
    if let Some(v) = &fields.language {
        meta.language = non_empty(v).unwrap_or_else(|| super::opf::DEFAULT_LANGUAGE.to_string());
    }
    if let Some(v) = &fields.publisher {
        meta.publisher = non_empty(v);
    }
    if let Some(v) = &fields.pubdate {
        meta.pubdate = non_empty(v);
    }
    if let Some(v) = &fields.description {
        meta.description = non_empty(v);
    }
    if let Some(list) = &fields.subjects {
        meta.subjects = clean_subjects(list);
    }
    if let Some(v) = &fields.series {
        meta.series = non_empty(v);
    }
    if let Some(v) = fields.series_index {
        meta.series_index = Some(v);
    }
    if let Some(v) = &fields.isbn {
        meta.isbn = non_empty(v);
    }
    Ok(())
}

fn apply_comic(meta: &mut ComicMeta, fields: &MetaFields) -> Result<(), EditError> {
    if let Some(v) = &fields.title {
        let title = non_empty(v).ok_or_else(|| EditError::BadRequest("书名不能为空".into()))?;
        meta.title = title;
    }
    if let Some(v) = &fields.author {
        meta.author = non_empty(v);
    }
    if let Some(v) = &fields.language {
        meta.language = non_empty(v);
    }
    if let Some(v) = &fields.publisher {
        meta.publisher = non_empty(v);
    }
    if let Some(v) = &fields.pubdate {
        meta.pubdate = non_empty(v);
    }
    if let Some(v) = &fields.description {
        meta.description = non_empty(v);
    }
    if let Some(list) = &fields.subjects {
        meta.subjects = clean_subjects(list);
    }
    if let Some(v) = &fields.series {
        meta.series = non_empty(v);
    }
    if let Some(v) = fields.series_index {
        meta.series_index = Some(v);
    }
    if let Some(v) = &fields.isbn {
        meta.isbn = non_empty(v);
    }
    Ok(())
}

fn revision_conflict(current: u64) -> EditError {
    EditError::Conflict(format!(
        "书目录已被其他操作更新（当前修订 {current}），请刷新后重试"
    ))
}

fn require_revision(current: u64, expected: u64) -> Result<(), EditError> {
    if current == expected {
        Ok(())
    } else {
        Err(revision_conflict(current))
    }
}

/// 保存文字书元数据到 `content.opf`。
pub async fn save_text_meta(
    store: &Store,
    book_url: &str,
    fields: &MetaFields,
) -> Result<OpfMeta, EditError> {
    if fields.is_empty() {
        return Err(EditError::BadRequest("没有可更新的字段".into()));
    }
    let _guard = lock_book(book_url).await;
    let opened = store
        .open_book(book_url)
        .await?
        .ok_or_else(|| EditError::NotFound("书目录尚未建立，先打开一次或同步目录".into()))?;
    let mut meta = opened.meta;
    apply_text(&mut meta, fields)?;
    meta.bump();
    store.write_opf(&opened.dir, &meta).await?;
    super::index::sync_book(store, book_url).await;
    Ok(meta)
}

/// 保存漫画元数据到 `oread.json` + `ComicInfo.xml`。
pub async fn save_comic_meta(
    store: &Store,
    book_url: &str,
    fields: &MetaFields,
) -> Result<ComicMeta, EditError> {
    if fields.is_empty() {
        return Err(EditError::BadRequest("没有可更新的字段".into()));
    }
    let _guard = lock_book(book_url).await;
    let opened = store
        .open_comic(book_url)
        .await?
        .ok_or_else(|| EditError::NotFound("书目录尚未建立，先打开一次或同步目录".into()))?;
    let mut meta = opened.meta;
    apply_comic(&mut meta, fields)?;
    meta.bump();
    comic::write_meta(&opened.dir, &meta).await?;
    comic::write_comic_info(&opened.dir, &meta).await?;
    super::index::sync_book(store, book_url).await;
    Ok(meta)
}

/// 读一章的段落，供编辑器回显。占位章/卷标题返回 `None`。
pub async fn chapter_paragraphs(
    store: &Store,
    book_url: &str,
    chapter_ref: &str,
) -> Result<Option<(OpfMeta, ChapterItem, Vec<String>)>, EditError> {
    let opened = store
        .open_book(book_url)
        .await?
        .ok_or_else(|| EditError::NotFound("书目录尚未建立".into()))?;
    let idx = opened
        .meta
        .chapter_index_by_ref(book_url, chapter_ref)
        .ok_or_else(|| EditError::NotFound("目录里没有这一章".into()))?;
    let item = opened.meta.chapters[idx].clone();
    if item.is_volume {
        return Err(EditError::BadRequest("卷标题没有正文".into()));
    }
    let Some(xhtml) = store.read_chapter(&opened.dir, &item.href).await? else {
        return Ok(None);
    };
    if super::is_pending_html(&xhtml) {
        return Ok(None);
    }
    let paragraphs = body_blocks(&xhtml, &item.title)
        .into_iter()
        .map(|b| match b {
            Block::Heading(_, text) | Block::Paragraph(text) => text,
        })
        .filter(|text| !text.trim().is_empty())
        .collect();
    Ok(Some((opened.meta, item, paragraphs)))
}

/// 保存一章正文。`revision` 是编辑开始时读到的修订号，用于乐观锁。
pub async fn save_chapter(
    store: &Store,
    book_url: &str,
    chapter_ref: &str,
    revision: u64,
    paragraphs: &[String],
) -> Result<OpfMeta, EditError> {
    if paragraphs.is_empty() {
        return Err(EditError::BadRequest("正文不能为空".into()));
    }
    let _guard = lock_book(book_url).await;
    let opened = store
        .open_book(book_url)
        .await?
        .ok_or_else(|| EditError::NotFound("书目录尚未建立".into()))?;
    require_revision(opened.meta.revision, revision)?;
    let idx = opened
        .meta
        .chapter_index_by_ref(book_url, chapter_ref)
        .ok_or_else(|| EditError::NotFound("目录里没有这一章".into()))?;
    let item = opened.meta.chapters[idx].clone();
    if item.is_volume {
        return Err(EditError::BadRequest("卷标题没有正文".into()));
    }
    let clean: Vec<String> = paragraphs
        .iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if clean.is_empty() {
        return Err(EditError::BadRequest("正文不能为空".into()));
    }
    store
        .write_chapter(&opened.dir, &opened.meta.language, &item, &clean)
        .await?;
    let mut meta = opened.meta;
    meta.bump();
    store.write_nav(&opened.dir, &meta).await?;
    store.write_opf(&opened.dir, &meta).await?;
    super::index::sync_book(store, book_url).await;
    Ok(meta)
}

/// 以文件内容判定图片格式，不信任客户端声明的 MIME：避免把 PNG 存成 .jpg、
/// 或把 SVG/HTML 伪装成图片写进书目录。
///
/// 只接受 JPEG/WebP：本仓库的 `image` 依赖只编译了这两种解码器，墨水屏灰度与
/// 长条切片也只处理这两种；收下 PNG/GIF 会在真机上读不出来。
fn detect_image(data: &[u8]) -> Result<(&'static str, &'static str), EditError> {
    if data.is_empty() {
        return Err(EditError::BadRequest("图片内容为空".into()));
    }
    let fmt = image::guess_format(data)
        .map_err(|_| EditError::BadRequest("无法识别的图片格式".into()))?;
    match fmt {
        image::ImageFormat::Jpeg => Ok(("image/jpeg", "jpg")),
        image::ImageFormat::WebP => Ok(("image/webp", "webp")),
        _ => Err(EditError::BadRequest(
            "只接受 JPEG / WebP（墨水屏链路只解码这两种，请先转换）".into(),
        )),
    }
}

/// 换文字书封面：写 `OEBPS/images/cover.<ext>` 并更新 OPF。
pub async fn replace_cover(
    store: &Store,
    book_url: &str,
    revision: u64,
    _mime: &str,
    data: &[u8],
) -> Result<(OpfMeta, CoverItem), EditError> {
    let (mime, ext) = detect_image(data)?;
    let _guard = lock_book(book_url).await;
    let opened = store
        .open_book(book_url)
        .await?
        .ok_or_else(|| EditError::NotFound("书目录尚未建立".into()))?;
    require_revision(opened.meta.revision, revision)?;
    let href = format!("images/cover.{ext}");
    let path = safe_join(&opened.dir.join("OEBPS"), &href)?;
    atomic_write(&path, data).await?;

    let old = opened.meta.cover.clone();
    let cover = CoverItem {
        href: href.clone(),
        media_type: mime.to_string(),
    };
    let mut meta = opened.meta;
    meta.cover = Some(cover.clone());
    meta.bump();
    store.write_opf(&opened.dir, &meta).await?;
    super::index::sync_book(store, book_url).await;
    // 旧封面文件名不同（例如 png → jpg）时删掉，避免目录里留孤儿文件。
    if let Some(old) = old {
        if old.href != href {
            if let Ok(path) = safe_join(&opened.dir.join("OEBPS"), &old.href) {
                let _ = tokio::fs::remove_file(path).await;
            }
        }
    }
    Ok((meta, cover))
}

fn comic_chapter_index(meta: &ComicMeta, book_url: &str, chapter_ref: &str) -> Option<usize> {
    let chapter_ref = chapter_ref.trim();
    meta.chapters
        .iter()
        .position(|c| c.url.as_deref().map(str::trim) == Some(chapter_ref))
        .or_else(|| {
            let id = crate::ebook_source::local_chapter_id(book_url, chapter_ref)?;
            meta.chapters.iter().position(|c| c.id == id)
        })
}

fn image_dims(data: &[u8]) -> Option<(u32, u32)> {
    let reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let (w, h) = reader.into_dimensions().ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

/// 换漫画单页：保持页序号不变，只替换文件与页表里的扩展名/MIME/尺寸。
pub async fn replace_page(
    store: &Store,
    book_url: &str,
    chapter_ref: &str,
    page_href: &str,
    revision: u64,
    _mime: &str,
    data: &[u8],
) -> Result<(ComicMeta, ComicPage), EditError> {
    let (mime, ext) = detect_image(data)?;
    let _guard = lock_book(book_url).await;
    let opened = store
        .open_comic(book_url)
        .await?
        .ok_or_else(|| EditError::NotFound("漫画目录尚未建立".into()))?;
    require_revision(opened.meta.revision, revision)?;
    let ci = comic_chapter_index(&opened.meta, book_url, chapter_ref)
        .ok_or_else(|| EditError::NotFound("目录里没有这一话".into()))?;
    let chapter = opened
        .meta
        .chapters
        .get(ci)
        .ok_or_else(|| EditError::NotFound("目录里没有这一话".into()))?;
    if chapter.is_volume {
        return Err(EditError::BadRequest("卷标题没有页面".into()));
    }
    let pi = chapter
        .pages
        .iter()
        .position(|p| p.href == page_href)
        .ok_or_else(|| EditError::NotFound("这一话里没有该页".into()))?;
    let old = chapter.pages[pi].clone();
    let stem = Path::new(&old.href)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| EditError::BadRequest("页文件名不合法".into()))?;
    let href = format!("{stem}.{ext}");
    atomic_write(&opened.dir.join(&href), data).await?;
    if href != old.href {
        if let Ok(path) = safe_join(&opened.dir, &old.href) {
            let _ = tokio::fs::remove_file(path).await;
        }
    }
    let (width, height) = match image_dims(data) {
        Some((w, h)) => (Some(w), Some(h)),
        None => (None, None),
    };
    let page = ComicPage {
        href,
        mime: mime.to_string(),
        width,
        height,
        bytes: Some(data.len() as u64),
        source_url: None,
    };
    let mut meta = opened.meta;
    if let Some(ch) = meta.chapters.get_mut(ci) {
        if let Some(slot) = ch.pages.get_mut(pi) {
            *slot = page.clone();
        }
    }
    meta.bump();
    comic::write_meta(&opened.dir, &meta).await?;
    comic::write_comic_info(&opened.dir, &meta).await?;
    super::index::sync_book(store, book_url).await;
    Ok((meta, page))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library_store::Tier;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("oread-edit-{tag}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 1×1 PNG：本仓库未编译 PNG 解码器，用来验证上传会被明确拒绝。
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xB5,
        0x1C, 0x0C, 0x02, 0x00, 0x00, 0x00, 0x0B, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x64,
        0x60, 0x00, 0x00, 0x00, 0x06, 0x00, 0x02, 0x30, 0x81, 0xD0, 0x2F, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// 运行期生成 1×1 JPEG，验证换页后的宽高与扩展名都来自真实内容。
    fn jpeg_1x1() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 80);
        enc.encode(&[0u8, 0, 0], 1, 1, image::ExtendedColorType::Rgb8)
            .unwrap();
        buf
    }

    async fn text_book(root: &Path) -> (Store, String, PathBuf) {
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let book_url = "https://example.com/book/edit".to_string();
        let mut meta = OpfMeta::new(&book_url, "书");
        let item = ChapterItem {
            id: "ch-0001".into(),
            href: "chapters/ch-0001-kai-duan.xhtml".into(),
            title: "开端".into(),
            url: Some("https://example.com/book/edit/1".into()),
            is_volume: false,
        };
        meta.chapters.push(item.clone());
        let dir = store.ensure_skeleton(Tier::Temp, &meta).await.unwrap();
        store
            .write_chapter(&dir, "zh", &item, &["第一段".into(), "第二段".into()])
            .await
            .unwrap();
        meta.bump();
        store.write_opf(&dir, &meta).await.unwrap();
        (store, book_url, dir)
    }

    #[tokio::test]
    async fn chapter_edit_round_trip_and_revision_guard() {
        let root = temp_root("chapter");
        let (store, book_url, _dir) = text_book(&root).await;
        let chapter_url = "https://example.com/book/edit/1";

        let (meta, item, paragraphs) = chapter_paragraphs(&store, &book_url, chapter_url)
            .await
            .unwrap()
            .expect("已缓存章节应可编辑");
        assert_eq!(item.title, "开端");
        assert_eq!(paragraphs, vec!["第一段", "第二段"]);

        let err = save_chapter(
            &store,
            &book_url,
            chapter_url,
            meta.revision + 1,
            &["改".into()],
        )
        .await
        .unwrap_err();
        assert!(matches!(err, EditError::Conflict(_)), "旧修订号必须拒绝");

        let after = save_chapter(
            &store,
            &book_url,
            chapter_url,
            meta.revision,
            &["新第一段".into(), "新第二段".into()],
        )
        .await
        .unwrap();
        assert!(after.revision > meta.revision);

        let (_, _, paragraphs) = chapter_paragraphs(&store, &book_url, chapter_url)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(paragraphs, vec!["新第一段", "新第二段"]);

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn cover_replaces_file_and_detects_format() {
        let root = temp_root("cover");
        let (store, book_url, dir) = text_book(&root).await;
        let meta = store.open_book(&book_url).await.unwrap().unwrap().meta;

        let (after, cover) =
            replace_cover(&store, &book_url, meta.revision, "image/jpeg", &jpeg_1x1())
                .await
                .unwrap();
        assert_eq!(cover.href, "images/cover.jpg");
        assert_eq!(cover.media_type, "image/jpeg");
        assert!(after.revision > meta.revision);
        assert!(dir.join("OEBPS/images/cover.jpg").is_file());

        // 非图片内容与未编译解码器的格式都必须拒绝，不能写进书目录。
        for bad in [b"<html>".as_slice(), PNG_1X1] {
            let err = replace_cover(&store, &book_url, after.revision, "image/jpeg", bad)
                .await
                .unwrap_err();
            assert!(matches!(err, EditError::BadRequest(_)));
        }

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn comic_page_replacement_keeps_stem_and_updates_page() {
        let root = temp_root("page");
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let book_url = "https://example.com/comic/edit";
        let dir = store.ensure_dirs(Tier::Temp, book_url).await.unwrap();
        let mut meta = comic::ComicMeta::new(book_url, "漫画");
        meta.chapters.push(comic::ComicChapter {
            id: "ch-0001".into(),
            title: "第一话".into(),
            url: Some("https://example.com/comic/edit/1".into()),
            is_volume: false,
            pages: vec![comic::ComicPage {
                href: "0001-001.webp".into(),
                mime: "image/webp".into(),
                width: Some(800),
                height: Some(1200),
                bytes: Some(3),
                source_url: None,
            }],
        });
        comic::write_meta(&dir, &meta).await.unwrap();
        tokio::fs::write(dir.join("0001-001.webp"), b"old")
            .await
            .unwrap();

        let revision = comic::read_meta(&dir, book_url).await.unwrap().revision;
        let (after, page) = replace_page(
            &store,
            book_url,
            "https://example.com/comic/edit/1",
            "0001-001.webp",
            revision,
            "image/webp",
            &jpeg_1x1(),
        )
        .await
        .unwrap();
        assert_eq!(page.href, "0001-001.jpg");
        assert_eq!(page.mime, "image/jpeg");
        assert_eq!(
            (page.width, page.height),
            (Some(1), Some(1)),
            "page={page:?}"
        );
        assert!(!dir.join("0001-001.webp").exists(), "旧扩展名文件应删除");
        assert!(dir.join("0001-001.jpg").is_file());
        assert_eq!(after.chapters[0].pages[0].href, "0001-001.jpg");

        std::fs::remove_dir_all(root).ok();
    }
}
