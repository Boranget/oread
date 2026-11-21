//! TXT 写入器：与旧 `/api/book/export/txt` 的输出逐字一致。
//!
//! 约束（低内存机型）：
//! - **逐章流式写盘**：一次只在内存里持有一章正文；
//! - 未缓存章节必须显式占位，不能静默丢章；
//! - 只读缓存，不发起任何网络抓取。

use std::path::Path;

use tokio::fs::File;
use tokio::io::AsyncWriteExt;

use crate::model::{BookInput, Format, WriteReport};
use crate::pipeline::{ChapterReader, FormatWriter};

const PLACEHOLDER_TITLE: &str = "【未缓存】";

/// TXT 写入器。
pub struct TxtWriter;

#[async_trait::async_trait]
impl FormatWriter for TxtWriter {
    fn format(&self) -> Format {
        Format::Txt
    }

    async fn write(
        &self,
        input: &BookInput,
        reader: &dyn ChapterReader,
        out: &Path,
    ) -> anyhow::Result<WriteReport> {
        let meta = &input.meta;
        let author = normalize_author(&meta.author);
        let source = meta.source.clone().unwrap_or_default();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");

        let mut header = format!(
            "书名：{}\n作者：{}\n书源：{}\n书籍地址：{}\n导出时间：{}\n\
             章节：已缓存 {}/{} 章，缺 {} 章\n",
            meta.name,
            author,
            source,
            meta.book_url,
            now,
            input.cached_chapters,
            input.total_chapters,
            input.missing_chapters
        );
        if input.missing_chapters > 0 {
            header.push_str("提示：缺失章节已用占位说明标出，不会静默丢章。\n");
        }
        header.push('\n');
        header.push_str(&"=".repeat(60));
        header.push('\n');

        let mut file = File::create(out).await?;
        file.write_all(header.as_bytes()).await?;

        let mut written_chapters = 0i64;
        for ch in &input.chapters {
            if ch.is_volume {
                if !ch.title.trim().is_empty() {
                    file.write_all(format!("\n\n【分卷】{}\n", ch.title.trim()).as_bytes())
                        .await?;
                }
                continue;
            }
            let title = ch.title.trim();
            let heading = if input.total_chapters > 0 {
                format!(
                    "\n\n===== [{}/{}] {} =====\n\n",
                    ch.seq, input.total_chapters, title
                )
            } else {
                format!("\n\n===== {} =====\n\n", title)
            };
            file.write_all(heading.as_bytes()).await?;

            match reader.chapter_text(&ch.url).await? {
                Some(body) => {
                    file.write_all(body.trim().as_bytes()).await?;
                    file.write_all(b"\n").await?;
                    written_chapters += 1;
                }
                None => {
                    file.write_all(
                        format!(
                            "{}本章正文尚未缓存。章节地址：{}\n\
                             请先在阅读端打开本章，或执行“缓存全部”后重新导出。\n",
                            PLACEHOLDER_TITLE, ch.url
                        )
                        .as_bytes(),
                    )
                    .await?;
                }
            }
        }

        let footer = format!(
            "\n\n===== 导出结束：实际写入 {}/{} 章 =====\n",
            written_chapters, input.total_chapters
        );
        file.write_all(footer.as_bytes()).await?;
        file.flush().await?;
        drop(file);

        let size_bytes = tokio::fs::metadata(out).await?.len();
        Ok(WriteReport {
            written_chapters,
            size_bytes,
            ..Default::default()
        })
    }
}

/// 规范化书源里的作者字段：书源常自带 `作者：` / `作 者：` / `Authors:` 等标签，
/// 导出时统一剥掉标签与首尾空白，空值回落为“未知”。
pub fn normalize_author(raw: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::{normalize_author, TxtWriter};
    use crate::model::{BookInput, BookMeta, ChapterKind, ChapterRef};
    use crate::pipeline::{ChapterReader, FormatWriter};
    use std::collections::HashMap;
    use std::path::PathBuf;

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

    struct MapReader(HashMap<String, String>);

    #[async_trait::async_trait]
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
            "oread-txt-{tag}-{}-{stamp}.txt",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn writes_header_volumes_and_placeholders() {
        let input = BookInput {
            meta: BookMeta {
                book_url: "https://x/book".into(),
                name: "测试书".into(),
                author: "作者：某人".into(),
                source: Some("测试源".into()),
                ..Default::default()
            },
            chapters: vec![
                ChapterRef {
                    url: "volume:b#0".into(),
                    title: "第一卷".into(),
                    seq: 0,
                    is_volume: true,
                    kind: ChapterKind::Text,
                },
                ChapterRef {
                    url: "https://x/c1".into(),
                    title: "第一章".into(),
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
            total_chapters: 2,
            cached_chapters: 1,
            missing_chapters: 1,
            cached_bytes: 4,
            text_chapters: 2,
            cached_text_chapters: 1,
        };
        let reader = MapReader(HashMap::from([(
            "https://x/c1".to_string(),
            "正文".to_string(),
        )]));
        let out = temp_path("write");
        let report = TxtWriter.write(&input, &reader, &out).await.unwrap();
        let text = tokio::fs::read_to_string(&out).await.unwrap();
        let _ = tokio::fs::remove_file(&out).await;

        assert_eq!(report.written_chapters, 1);
        assert!(report.size_bytes > 0);
        assert!(text.contains("作者：某人\n"), "作者标签只出现一次: {text}");
        assert!(!text.contains("作者：作者："));
        assert!(text.contains("【分卷】第一卷"));
        assert!(text.contains("[1/2] 第一章"));
        assert!(text.contains("正文"));
        assert_eq!(text.matches("【未缓存】").count(), 1);
        assert!(text.contains("章节地址：https://x/c2"));
        assert!(text.contains("实际写入 1/2 章"));
    }
}
