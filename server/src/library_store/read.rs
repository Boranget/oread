//! 从「目录即书」读取：OPF 定位章节、章节 XHTML 还原成阅读器/导出可用的正文。
//!
//! 这里不做任何网络或数据库访问：目录里有什么就读什么。调用方拿不到内容时
//! 自行回退到旧的 `library.db` 路径（迁移期）。

use std::io;
use std::path::{Path, PathBuf};

use boranlib_ebook::html::{code_to_html, list_to_html, to_blocks, Block};
use boranlib_ebook::xml::xml_escape;

use super::{comic, is_pending_html, ChapterItem, OpfMeta, Store, Tier};

/// 一本已经打开的书：目录路径 + 解析后的 OPF。
#[derive(Debug, Clone)]
pub struct OpenedBook {
    pub tier: Tier,
    pub dir: PathBuf,
    pub meta: OpfMeta,
}

/// 一本已经打开的漫画：目录路径 + `oread.json`（或反推出来的）目录语义。
#[derive(Debug, Clone)]
pub struct OpenedComic {
    pub tier: Tier,
    pub dir: PathBuf,
    pub meta: comic::ComicMeta,
}

impl Store {
    /// 打开一本书目录；目录不存在、或不是 EPUB（漫画）时返回 `None`。
    pub async fn open_book(&self, book_url: &str) -> io::Result<Option<OpenedBook>> {
        let Some((tier, dir)) = self.find(book_url) else {
            return Ok(None);
        };
        if !dir.join("OEBPS").join("content.opf").is_file() {
            return Ok(None);
        }
        let meta = self.read_opf(&dir).await?;
        Ok(Some(OpenedBook { tier, dir, meta }))
    }

    /// 打开一本漫画目录；不是漫画（没有 ComicInfo.xml / oread.json）时返回 `None`。
    pub async fn open_comic(&self, book_url: &str) -> io::Result<Option<OpenedComic>> {
        let Some((tier, dir)) = self.find(book_url) else {
            return Ok(None);
        };
        if !comic::is_comic_dir(&dir) {
            return Ok(None);
        }
        let meta = comic::read_meta(&dir, book_url).await?;
        Ok(Some(OpenedComic { tier, dir, meta }))
    }
}

impl OpenedComic {
    /// 按书源章节 URL 或本地合成 URL（`local:<book>#<id>`）找章节。
    pub fn chapter_by_ref(
        &self,
        book_url: &str,
        chapter_url: &str,
    ) -> Option<&comic::ComicChapter> {
        let chapter_url = chapter_url.trim();
        self.meta.chapter_by_url(chapter_url).or_else(|| {
            let id = crate::ebook_source::local_chapter_id(book_url, chapter_url)?;
            self.meta.chapter_by_id(id)
        })
    }
}

impl OpfMeta {
    /// 按书源章节 URL 找目录项，返回它在 spine 里的位置。
    pub fn chapter_index_by_url(&self, url: &str) -> Option<usize> {
        let url = url.trim();
        self.chapters
            .iter()
            .position(|c| c.url.as_deref().map(str::trim) == Some(url))
    }

    pub fn chapter_by_url(&self, url: &str) -> Option<&ChapterItem> {
        self.chapter_index_by_url(url).map(|i| &self.chapters[i])
    }

    pub fn chapter_index_by_id(&self, id: &str) -> Option<usize> {
        self.chapters.iter().position(|c| c.id == id)
    }

    /// 按书源 URL 或本地合成 URL（`local:<book>#<id>`）定位章节。
    pub fn chapter_index_by_ref(&self, book_url: &str, url: &str) -> Option<usize> {
        self.chapter_index_by_url(url).or_else(|| {
            let id = crate::ebook_source::local_chapter_id(book_url, url)?;
            self.chapter_index_by_id(id)
        })
    }
}

/// 一章正文的三种视图：给网页端/导出 writer 的 HTML、给搜索的纯文本，以及结构化块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterBody {
    pub html: String,
    pub text: String,
    pub blocks: Vec<Block>,
}

/// 读章节文件；不存在返回 `Ok(None)`。
pub async fn read_chapter_file(dir: &Path, href: &str) -> io::Result<Option<String>> {
    let path = super::safe_join(&dir.join("OEBPS"), href)?;
    match tokio::fs::read_to_string(&path).await {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 取 `<body>` 内部 HTML；没有 body 时退回整份文档。
fn body_inner(xhtml: &str) -> String {
    let doc = scraper::Html::parse_document(xhtml);
    let Ok(selector) = scraper::Selector::parse("body") else {
        return xhtml.to_string();
    };
    doc.select(&selector)
        .next()
        .map(|body| body.inner_html())
        .unwrap_or_else(|| xhtml.to_string())
}

/// 章节 XHTML → 正文块。首行标题与 OPF 里的标题相同时去掉，避免阅读器重复显示。
pub fn body_blocks(xhtml: &str, title: &str) -> Vec<Block> {
    let mut blocks = to_blocks(&body_inner(xhtml));
    if let Some(Block::Heading(_, heading)) = blocks.first() {
        if !title.trim().is_empty() && heading.trim() == title.trim() {
            blocks.remove(0);
        }
    }
    blocks
}

pub fn blocks_to_html(blocks: &[Block]) -> String {
    let mut out = String::with_capacity(blocks.len() * 96);
    for block in blocks {
        match block {
            Block::Heading(level, text) => {
                let level = (*level).clamp(1, 6);
                out.push_str(&format!("<h{level}>{}</h{level}>", xml_escape(text)));
            }
            Block::Paragraph(text) => {
                out.push_str(&format!("<p>{}</p>", xml_escape(text)));
            }
            Block::List(items) => out.push_str(&list_to_html(items)),
            Block::Code(text) => out.push_str(&code_to_html(text)),
            Block::Image { src, alt } => {
                out.push_str(&format!(
                    "<div class=\"oread-img\"><img src=\"{}\" alt=\"{}\"/></div>",
                    xml_escape(src),
                    xml_escape(alt)
                ));
            }
        }
    }
    out
}

pub fn blocks_to_text(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        let text = match block {
            Block::Heading(_, text) | Block::Paragraph(text) | Block::Code(text) => text.as_str(),
            Block::List(items) => {
                for item in items {
                    if item.text.trim().is_empty() {
                        continue;
                    }
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&item.text);
                }
                continue;
            }
            Block::Image { .. } => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(text);
    }
    out
}

/// 把一份章节文件解析成正文；占位章、没有正文的卷标题都返回 `None`。
pub fn parse_chapter(xhtml: &str, title: &str) -> Option<ChapterBody> {
    if is_pending_html(xhtml) {
        return None;
    }
    let blocks = body_blocks(xhtml, title);
    if blocks.is_empty() {
        return None;
    }
    let text = blocks_to_text(&blocks);
    // 纯图片章节也可以有内容；只有既没有文字也没有图片时才视为空章。
    if text.trim().is_empty() && !blocks.iter().any(Block::is_image) {
        return None;
    }
    Some(ChapterBody {
        html: blocks_to_html(&blocks),
        text,
        blocks,
    })
}

/// 读取并解析一章；文件不存在时返回 `Ok(None)`。
pub async fn load_chapter(dir: &Path, item: &ChapterItem) -> io::Result<Option<ChapterBody>> {
    let Some(xhtml) = read_chapter_file(dir, &item.href).await? else {
        return Ok(None);
    };
    Ok(parse_chapter(&xhtml, &item.title))
}

#[cfg(test)]
mod tests {
    use super::{body_blocks, parse_chapter};
    use boranlib_ebook::html::Block;

    #[test]
    fn drops_duplicate_title_heading() {
        let xhtml = "<html><head><title>第一章</title></head><body>\
                     <h2>第一章</h2><p>正文一</p><p>正文二</p></body></html>";
        let blocks = body_blocks(xhtml, "第一章");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph("正文一".into()),
                Block::Paragraph("正文二".into()),
            ]
        );
    }

    #[test]
    fn head_title_is_not_content() {
        let xhtml = "<html><head><title>不该出现</title></head><body><p>正文</p></body></html>";
        let body = parse_chapter(xhtml, "标题").unwrap();
        assert!(!body.text.contains("不该出现"));
        assert_eq!(body.text, "正文");
    }

    #[test]
    fn pending_marker_is_not_content() {
        let xhtml = "<html><body><h2>第一章</h2>\
                     <p class=\"oread-pending\">本章内容尚未缓存</p></body></html>";
        assert!(parse_chapter(xhtml, "第一章").is_none());
    }
}
