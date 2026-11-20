//! EPUB 3 写入器：正文章节 → XHTML → store-only ZIP。
//!
//! 结构（最小但合规）：
//! ```text
//! mimetype                     (必须首条、不压缩)
//! META-INF/container.xml
//! OEBPS/content.opf            (EPUB 3 package)
//! OEBPS/nav.xhtml              (EPUB 3 目录)
//! OEBPS/style.css
//! OEBPS/ch001.xhtml ...        (一章一文件)
//! ```
//!
//! 正文先经 [`crate::ebook::html::to_blocks`] 归一化为语义段落，再生成良构 XHTML；
//! 未缓存章节写占位段落，与 TXT 导出口径一致，不静默丢章。
//! 图片暂不内联（漫画走 CBZ；文字书图片后续单独做）。

use std::path::Path;

use sha2::{Digest, Sha256};
use tokio::fs::File;

use crate::ebook::format::txt::normalize_author;
use crate::ebook::html::{to_blocks, Block};
use crate::ebook::model::{BookInput, BookMeta, ChapterRef, Format, WriteReport};
use crate::ebook::pipeline::{ChapterReader, FormatWriter};
use crate::ebook::xml::xml_escape;
use crate::ebook::zip::ZipWriter;

const STYLE_CSS: &str = "\
body { margin: 0 1em; line-height: 1.7; }
h1, h2, h3, h4, h5, h6 { line-height: 1.3; margin: 1.2em 0 0.6em; }
p { margin: 0 0 0.7em; text-indent: 2em; }
ul, ol { margin: 0 0 0.7em 1.4em; padding: 0; }
li { margin: 0 0 0.3em; }
blockquote { margin: 0.6em 1em; }
";

/// EPUB 写入器。
pub struct EpubWriter;

#[axum::async_trait]
impl FormatWriter for EpubWriter {
    fn format(&self) -> Format {
        Format::Epub
    }

    async fn write(
        &self,
        input: &BookInput,
        reader: &dyn ChapterReader,
        out: &Path,
    ) -> anyhow::Result<WriteReport> {
        let chapters: Vec<&ChapterRef> = input
            .chapters
            .iter()
            .filter(|c| !c.is_volume && !c.kind.is_image())
            .collect();
        if chapters.is_empty() {
            anyhow::bail!("这本书没有可导出的正文章节，无法生成 EPUB");
        }

        let file = File::create(out).await?;
        let mut zip = ZipWriter::new(file);
        // OCF 要求：mimetype 必须是第一条，且不得压缩。
        zip.add_stored("mimetype", b"application/epub+zip").await?;
        zip.add_stored("META-INF/container.xml", container_xml().as_bytes())
            .await?;
        zip.add_stored("OEBPS/style.css", STYLE_CSS.as_bytes())
            .await?;

        let mut written = 0i64;
        for (i, ch) in chapters.iter().enumerate() {
            let body = reader.chapter_text(&ch.url).await?;
            let blocks = match body {
                Some(text) => {
                    written += 1;
                    to_blocks(&text)
                }
                None => vec![Block::Paragraph(
                    "本章正文尚未缓存，请在阅读器打开本章后重新导出。".into(),
                )],
            };
            let xhtml = chapter_xhtml(&input.meta, ch, &blocks);
            zip.add_stored(&format!("OEBPS/ch{:03}.xhtml", i + 1), xhtml.as_bytes())
                .await?;
        }

        let nav = nav_xhtml(&input.meta, &chapters);
        zip.add_stored("OEBPS/nav.xhtml", nav.as_bytes()).await?;
        let opf = content_opf(&input.meta, &chapters);
        zip.add_stored("OEBPS/content.opf", opf.as_bytes()).await?;

        let size_bytes = zip.finish().await?;
        Ok(WriteReport {
            written_chapters: written,
            written_pages: 0,
            missing_pages: 0,
            size_bytes,
        })
    }
}

fn language(meta: &BookMeta) -> String {
    meta.language
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("und")
        .to_string()
}

fn container_xml() -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
     <container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n\
       <rootfiles>\n\
         <rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/>\n\
       </rootfiles>\n\
     </container>\n"
        .to_string()
}

fn chapter_xhtml(meta: &BookMeta, ch: &ChapterRef, blocks: &[Block]) -> String {
    let lang = language(meta);
    let mut body = String::new();
    for block in blocks {
        match block {
            Block::Heading(level, text) => {
                let level = (*level).clamp(1, 6);
                body.push_str(&format!("<h{level}>{}</h{level}>\n", xml_escape(text)));
            }
            Block::Paragraph(text) => {
                body.push_str(&format!("<p>{}</p>\n", xml_escape(text)));
            }
        }
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <!DOCTYPE html>\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"{lang}\" lang=\"{lang}\">\n\
         <head>\n\
           <meta charset=\"utf-8\"/>\n\
           <title>{title}</title>\n\
           <link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/>\n\
         </head>\n\
         <body>\n\
           <h1>{title}</h1>\n\
         {body}</body>\n\
         </html>\n",
        lang = xml_escape(&lang),
        title = xml_escape(ch.title.trim()),
        body = body
    )
}

fn nav_xhtml(meta: &BookMeta, chapters: &[&ChapterRef]) -> String {
    let lang = language(meta);
    let mut items = String::new();
    for (i, ch) in chapters.iter().enumerate() {
        items.push_str(&format!(
            "<li><a href=\"ch{:03}.xhtml\">{}</a></li>\n",
            i + 1,
            xml_escape(ch.title.trim())
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <!DOCTYPE html>\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" \
         xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"{lang}\" lang=\"{lang}\">\n\
         <head><meta charset=\"utf-8\"/><title>目录</title></head>\n\
         <body>\n\
           <nav epub:type=\"toc\" id=\"toc\">\n\
             <h1>目录</h1>\n\
             <ol>\n{items}</ol>\n\
           </nav>\n\
         </body>\n\
         </html>\n",
        lang = xml_escape(&lang),
        items = items
    )
}

fn content_opf(meta: &BookMeta, chapters: &[&ChapterRef]) -> String {
    let lang = language(meta);
    let mut ids = String::new();
    let mut first = true;
    for (scheme, value) in &meta.identifiers {
        let value = if scheme.eq_ignore_ascii_case("isbn") {
            format!("urn:isbn:{}", value.trim())
        } else if value.contains(':') {
            value.trim().to_string()
        } else {
            format!("urn:{}:{}", scheme.trim(), value.trim())
        };
        if first {
            ids.push_str(&format!(
                "    <dc:identifier id=\"bookid\">{}</dc:identifier>\n",
                xml_escape(&value)
            ));
            first = false;
        } else {
            ids.push_str(&format!(
                "    <dc:identifier>{}</dc:identifier>\n",
                xml_escape(&value)
            ));
        }
    }
    if first {
        ids.push_str(&format!(
            "    <dc:identifier id=\"bookid\">urn:sha256:{}</dc:identifier>\n",
            sha256_hex(meta.book_url.as_bytes())
        ));
    }

    let mut extra = String::new();
    let author = normalize_author(&meta.author);
    if !meta.author.trim().is_empty() {
        extra.push_str(&format!(
            "    <dc:creator>{}</dc:creator>\n",
            xml_escape(&author)
        ));
    }
    if let Some(publisher) = meta
        .publisher
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        extra.push_str(&format!(
            "    <dc:publisher>{}</dc:publisher>\n",
            xml_escape(publisher)
        ));
    }
    if let Some(pubdate) = meta
        .pubdate
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        extra.push_str(&format!("    <dc:date>{}</dc:date>\n", xml_escape(pubdate)));
    }
    if let Some(intro) = meta
        .intro
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        extra.push_str(&format!(
            "    <dc:description>{}</dc:description>\n",
            xml_escape(intro)
        ));
    }
    for tag in &meta.tags {
        if !tag.trim().is_empty() {
            extra.push_str(&format!(
                "    <dc:subject>{}</dc:subject>\n",
                xml_escape(tag.trim())
            ));
        }
    }
    if let Some(series) = meta
        .series_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        extra.push_str(&format!(
            "    <meta property=\"belongs-to-collection\" id=\"series\">{}</meta>\n\
             \x20   <meta refines=\"#series\" property=\"collection-type\">series</meta>\n",
            xml_escape(series)
        ));
        if let Some(index) = meta.series_index {
            extra.push_str(&format!(
                "    <meta refines=\"#series\" property=\"group-position\">{}</meta>\n",
                format_index(index)
            ));
        }
    }
    extra.push_str(&format!(
        "    <meta property=\"dcterms:modified\">{}</meta>\n",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
    ));

    let mut manifest = String::from(
        "    <item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" \
         properties=\"nav\"/>\n\
         \x20   <item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>\n",
    );
    let mut spine = String::new();
    for (i, _) in chapters.iter().enumerate() {
        manifest.push_str(&format!(
            "    <item id=\"ch{}\" href=\"ch{:03}.xhtml\" media-type=\"application/xhtml+xml\"/>\n",
            i + 1,
            i + 1
        ));
        spine.push_str(&format!("    <itemref idref=\"ch{}\"/>\n", i + 1));
    }

    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" \
         unique-identifier=\"bookid\" xml:lang=\"{lang}\">\n\
           <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
         {ids}    <dc:title>{title}</dc:title>\n\
             <dc:language>{lang}</dc:language>\n\
         {extra}  </metadata>\n\
           <manifest>\n{manifest}  </manifest>\n\
           <spine>\n{spine}  </spine>\n\
         </package>\n",
        lang = xml_escape(&lang),
        title = xml_escape(meta.name.trim()),
        ids = ids,
        extra = extra,
        manifest = manifest,
        spine = spine
    )
}

fn sha256_hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(64);
    for b in Sha256::digest(data) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn format_index(index: f64) -> String {
    if index.fract() == 0.0 && index.abs() < 1e15 {
        format!("{}", index as i64)
    } else {
        format!("{index}")
    }
}

#[cfg(test)]
mod tests {
    use super::{content_opf, EpubWriter};
    use crate::ebook::model::{BookInput, BookMeta, ChapterKind, ChapterRef, Format, WriteReport};
    use crate::ebook::pipeline::{ChapterReader, FormatWriter};
    use std::collections::HashMap;
    use std::path::PathBuf;

    struct MapReader(HashMap<String, String>);

    #[axum::async_trait]
    impl ChapterReader for MapReader {
        async fn chapter_text(&self, url: &str) -> anyhow::Result<Option<String>> {
            Ok(self.0.get(url).cloned())
        }
    }

    fn temp_path(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "oread-epub-{tag}-{}-{stamp}.epub",
            std::process::id()
        ))
    }

    fn input() -> BookInput {
        BookInput {
            meta: BookMeta {
                book_url: "https://x/book".into(),
                name: "A & B".into(),
                author: "作者：某人".into(),
                language: Some("zh".into()),
                publisher: Some("测试社".into()),
                pubdate: Some("2020-01-01".into()),
                series_name: Some("测试系列".into()),
                series_index: Some(2.0),
                tags: vec!["科幻".into()],
                ..Default::default()
            },
            chapters: vec![
                ChapterRef {
                    url: "https://x/c1".into(),
                    title: "第一章 <开始>".into(),
                    seq: 1,
                    is_volume: false,
                    kind: ChapterKind::Text,
                },
                ChapterRef {
                    url: "https://x/c2".into(),
                    title: "第二章".into(),
                    seq: 2,
                    is_volume: false,
                    kind: ChapterKind::Text,
                },
            ],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn writes_epub_structure_and_converts_html() {
        let reader = MapReader(HashMap::from([(
            "https://x/c1".to_string(),
            "<p>第一段</p><p>第二段 &amp; 更多</p>".to_string(),
        )]));
        let out = temp_path("write");
        let report: WriteReport = EpubWriter.write(&input(), &reader, &out).await.unwrap();
        let bytes = tokio::fs::read(&out).await.unwrap();
        let _ = tokio::fs::remove_file(&out).await;
        let text = String::from_utf8_lossy(&bytes).to_string();

        assert_eq!(EpubWriter.format(), Format::Epub);
        assert_eq!(report.written_chapters, 1, "未缓存章节不计入写入数");
        assert_eq!(report.size_bytes, bytes.len() as u64);
        assert!(bytes.starts_with(b"PK\x03\x04"));
        // mimetype 必须是第一条且 store（method=0）
        let name_len = u16::from_le_bytes([bytes[26], bytes[27]]) as usize;
        assert_eq!(&bytes[30..30 + name_len], b"mimetype");
        assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 0);
        assert!(text.contains("application/epub+zip"));
        assert!(text.contains("META-INF/container.xml"));
        assert!(text.contains("OEBPS/content.opf"));
        assert!(text.contains("OEBPS/nav.xhtml"));
        assert!(text.contains("<dc:title>A &amp; B</dc:title>"));
        assert!(text.contains("<dc:creator>某人</dc:creator>"));
        assert!(text.contains("<dc:subject>科幻</dc:subject>"));
        assert!(text.contains("belongs-to-collection"));
        assert!(text.contains("<p>第一段</p>"));
        assert!(text.contains("<p>第二段 &amp; 更多</p>"));
        // 未缓存章节仍是占位段落
        assert!(text.contains("本章正文尚未缓存"));
        // XHTML 标题转义
        assert!(text.contains("<h1>第一章 &lt;开始&gt;</h1>"));
    }

    #[test]
    fn opf_uses_isbn_or_sha_and_escapes() {
        let mut meta = input().meta;
        meta.identifiers = vec![("isbn".into(), "9787532754687".into())];
        meta.name = "书名 & <测试>".into();
        let chapters = input().chapters;
        let refs: Vec<&ChapterRef> = chapters.iter().collect();
        let opf = content_opf(&meta, &refs);
        assert!(opf.contains("urn:isbn:9787532754687"));
        assert!(opf.contains("<dc:title>书名 &amp; &lt;测试&gt;</dc:title>"));
        assert!(opf.contains("<dc:language>zh</dc:language>"));
        assert!(opf.contains("<itemref idref=\"ch1\"/>"));
        assert!(opf.contains("dcterms:modified"));
    }
}
