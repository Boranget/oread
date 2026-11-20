//! CBZ 写入器：把馆藏漫画页原样打包成 ZIP（store，不重编码）。
//!
//! - 页面命名 `{章:04}-{页:03}.{ext}`，按目录顺序与页面顺序；
//! - 根目录附最小 `ComicInfo.xml`（标题/系列/作者/简介/语言/页数），
//!   供 Calibre、KOReader 等阅读器识别；
//! - 一次只在内存里持有一页；缺页计入 `missing_pages`，不静默丢章。

use std::path::Path;

use tokio::fs::File;

use crate::ebook::format::txt::normalize_author;
use crate::ebook::model::{BookInput, BookMeta, Format, WriteReport};
use crate::ebook::pipeline::{ChapterReader, FormatWriter};
use crate::ebook::zip::ZipWriter;

/// CBZ 写入器。
pub struct CbzWriter;

#[axum::async_trait]
impl FormatWriter for CbzWriter {
    fn format(&self) -> Format {
        Format::Cbz
    }

    async fn write(
        &self,
        input: &BookInput,
        reader: &dyn ChapterReader,
        out: &Path,
    ) -> anyhow::Result<WriteReport> {
        // 先取页面清单，避免为 0 页的书创建空包。
        let mut chapter_pages = Vec::new();
        let mut declared_pages = 0i64;
        for ch in input.image_chapters() {
            let pages = reader.chapter_pages(&ch.url).await?;
            declared_pages += pages.len() as i64;
            if !pages.is_empty() {
                chapter_pages.push(pages);
            }
        }
        if declared_pages == 0 {
            anyhow::bail!("这本书没有图片章节，无法导出 CBZ");
        }

        let file = File::create(out).await?;
        let mut zip = ZipWriter::new(file);
        let mut written_pages = 0i64;
        let mut written_chapters = 0i64;

        for (ci, pages) in chapter_pages.iter().enumerate() {
            let mut chapter_written = 0i64;
            for (pi, url) in pages.iter().enumerate() {
                let Some(page) = reader.page_data(url).await? else {
                    continue;
                };
                let name = format!("{:04}-{:03}.{}", ci + 1, pi + 1, ext_for_mime(&page.mime));
                zip.add_stored(&name, &page.data).await?;
                written_pages += 1;
                chapter_written += 1;
            }
            if chapter_written > 0 {
                written_chapters += 1;
            }
        }
        if written_pages == 0 {
            anyhow::bail!("没有已落盘的漫画图片，无法导出 CBZ");
        }

        let info = comic_info_xml(&input.meta, written_pages);
        zip.add_stored("ComicInfo.xml", info.as_bytes()).await?;
        let size_bytes = zip.finish().await?;

        Ok(WriteReport {
            written_chapters,
            written_pages,
            missing_pages: (declared_pages - written_pages).max(0),
            size_bytes,
        })
    }
}

/// MIME → 文件扩展名。未知类型保留 `.bin`，不猜成 JPEG 骗阅读器。
pub(crate) fn ext_for_mime(mime: &str) -> &'static str {
    let m = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if m.contains("jpeg") || m.contains("jpg") {
        "jpg"
    } else if m.contains("png") {
        "png"
    } else if m.contains("webp") {
        "webp"
    } else if m.contains("gif") {
        "gif"
    } else if m.contains("avif") {
        "avif"
    } else if m.contains("bmp") {
        "bmp"
    } else {
        "bin"
    }
}

/// 最小 ComicInfo.xml；空字段直接省略，避免把 `None` 写成字符串。
fn comic_info_xml(meta: &BookMeta, pages: i64) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <ComicInfo xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">\n",
    );
    let mut field = |tag: &str, value: &str| {
        if !value.trim().is_empty() {
            xml.push_str(&format!("  <{tag}>{}</{tag}>\n", xml_escape(value.trim())));
        }
    };
    field("Title", &meta.name);
    if let Some(series) = meta.series_name.as_deref() {
        field("Series", series);
    }
    if let Some(index) = meta.series_index {
        field("Number", &format_index(index));
    }
    if !meta.author.trim().is_empty() {
        field("Writer", &normalize_author(&meta.author));
    }
    if let Some(intro) = meta.intro.as_deref() {
        field("Summary", intro);
    }
    if let Some(lang) = meta.language.as_deref() {
        field("LanguageISO", lang);
    }
    field("PageCount", &pages.to_string());
    xml.push_str("</ComicInfo>\n");
    xml
}

fn format_index(index: f64) -> String {
    if index.fract() == 0.0 && index.abs() < 1e15 {
        format!("{}", index as i64)
    } else {
        format!("{index}")
    }
}

/// XML 文本转义（ComicInfo 只用到文本节点与属性值这两种场景）。
pub(crate) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{comic_info_xml, ext_for_mime, xml_escape, CbzWriter};
    use crate::ebook::model::{
        BookInput, BookMeta, ChapterKind, ChapterRef, PageData, WriteReport,
    };
    use crate::ebook::pipeline::{ChapterReader, FormatWriter};
    use std::collections::HashMap;
    use std::path::PathBuf;

    struct MapReader {
        pages: HashMap<String, Vec<String>>,
        data: HashMap<String, (Vec<u8>, &'static str)>,
    }

    #[axum::async_trait]
    impl ChapterReader for MapReader {
        async fn chapter_text(&self, _url: &str) -> anyhow::Result<Option<String>> {
            Ok(None)
        }

        async fn chapter_pages(&self, url: &str) -> anyhow::Result<Vec<String>> {
            Ok(self.pages.get(url).cloned().unwrap_or_default())
        }

        async fn page_data(&self, url: &str) -> anyhow::Result<Option<PageData>> {
            Ok(self.data.get(url).map(|(data, mime)| PageData {
                data: data.clone(),
                mime: (*mime).to_string(),
            }))
        }
    }

    fn temp_path(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "oread-cbz-{tag}-{}-{stamp}.cbz",
            std::process::id()
        ))
    }

    fn input() -> BookInput {
        BookInput {
            meta: BookMeta {
                book_url: "https://x/book".into(),
                name: "A & B <测试>".into(),
                author: "作者：某人".into(),
                series_name: Some("测试系列".into()),
                series_index: Some(2.0),
                intro: Some("简介 & <说明>".into()),
                language: Some("zh".into()),
                ..Default::default()
            },
            chapters: vec![
                ChapterRef {
                    url: "https://x/c1".into(),
                    title: "第一章".into(),
                    seq: 1,
                    is_volume: false,
                    kind: ChapterKind::Image,
                },
                ChapterRef {
                    url: "https://x/c2".into(),
                    title: "第二章".into(),
                    seq: 2,
                    is_volume: false,
                    kind: ChapterKind::Image,
                },
            ],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn writes_pages_comicinfo_and_reports_gaps() {
        let reader = MapReader {
            pages: HashMap::from([
                ("https://x/c1".to_string(), vec!["p1".into(), "p2".into()]),
                // 第二章声明 2 页，只有 1 页落盘
                ("https://x/c2".to_string(), vec!["p3".into(), "p4".into()]),
            ]),
            data: HashMap::from([
                ("p1".to_string(), (b"one".to_vec(), "image/jpeg")),
                ("p2".to_string(), (b"two".to_vec(), "image/webp")),
                ("p3".to_string(), (b"three".to_vec(), "image/png")),
            ]),
        };
        let out = temp_path("write");
        let report: WriteReport = CbzWriter.write(&input(), &reader, &out).await.unwrap();
        let bytes = tokio::fs::read(&out).await.unwrap();
        let _ = tokio::fs::remove_file(&out).await;

        assert_eq!(report.written_chapters, 2);
        assert_eq!(report.written_pages, 3);
        assert_eq!(report.missing_pages, 1);
        assert_eq!(report.size_bytes, bytes.len() as u64);
        assert!(bytes.starts_with(b"PK\x03\x04"));
        // 页面名与扩展名来自真实 MIME
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("0001-001.jpg"));
        assert!(text.contains("0001-002.webp"));
        assert!(text.contains("0002-001.png"));
        assert!(text.contains("ComicInfo.xml"));
        assert!(text.contains("<PageCount>3</PageCount>"));
        assert!(text.contains("<Title>A &amp; B &lt;测试&gt;</Title>"));
        assert!(text.contains("<Writer>某人</Writer>"));
        assert!(text.contains("<Series>测试系列</Series>"));
        assert!(text.contains("<Number>2</Number>"));
    }

    #[tokio::test]
    async fn empty_comic_is_rejected() {
        let reader = MapReader {
            pages: HashMap::new(),
            data: HashMap::new(),
        };
        let out = temp_path("empty");
        let err = CbzWriter.write(&input(), &reader, &out).await.unwrap_err();
        let _ = tokio::fs::remove_file(&out).await;
        assert!(err.to_string().contains("没有图片章节"));
    }

    #[test]
    fn mime_extensions_and_xml_escape() {
        assert_eq!(ext_for_mime("image/jpeg"), "jpg");
        assert_eq!(ext_for_mime("image/jpeg; charset=binary"), "jpg");
        assert_eq!(ext_for_mime("IMAGE/PNG"), "png");
        assert_eq!(ext_for_mime("image/webp"), "webp");
        assert_eq!(ext_for_mime("application/octet-stream"), "bin");
        assert_eq!(xml_escape("a&b<c>\"d'"), "a&amp;b&lt;c&gt;&quot;d&apos;");
    }

    #[test]
    fn comic_info_omits_missing_fields() {
        let meta = BookMeta {
            name: "书".into(),
            author: "".into(),
            ..Default::default()
        };
        let xml = comic_info_xml(&meta, 10);
        assert!(xml.contains("<Title>书</Title>"));
        assert!(xml.contains("<PageCount>10</PageCount>"));
        assert!(!xml.contains("<Series>"));
        assert!(!xml.contains("<Writer>"), "空作者不应写成「未知」");
        assert!(!xml.contains("<Number>"));
    }
}
