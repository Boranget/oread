//! 导出/转换的领域模型：一本书的只读视图、格式枚举与写入结果。
//!
//! 这里只描述「内容是什么」，不关心格式。格式差异全部落在 `format/` 下的 writer；
//! 将来 EPUB 需要的标签/标识也先放进 [`BookMeta`]，避免每加一种格式就改一次模型。

use std::path::PathBuf;

/// 支持的产物格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Txt,
}

impl Format {
    /// API 里的格式名（小写、无点）。
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Txt => "txt",
        }
    }

    /// 文件名后缀（不含点）。
    pub fn extension(self) -> &'static str {
        match self {
            Format::Txt => "txt",
        }
    }

    /// HTTP 响应用的 Content-Type。
    pub fn content_type(self) -> &'static str {
        match self {
            Format::Txt => "text/plain; charset=utf-8",
        }
    }
}

/// 书级元数据（OPF 语义字段的超集；缺失值为 `None`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BookMeta {
    pub book_url: String,
    pub name: String,
    /// 原样保存；TXT 等 writer 负责剥掉“作者：”之类的标签
    pub author: String,
    pub language: Option<String>,
    pub publisher: Option<String>,
    /// 出版日期原样保存（`YYYY-MM-DD` 或书写源给的字符串）
    pub pubdate: Option<String>,
    pub series_name: Option<String>,
    pub series_index: Option<f64>,
    pub intro: Option<String>,
    /// 展示用来源名（`origin_name` 优先，回落 `origin`）
    pub source: Option<String>,
}

/// 目录中的一章。分卷行也保留，writer 自行决定怎么输出。
#[derive(Debug, Clone, PartialEq)]
pub struct ChapterRef {
    pub url: String,
    pub title: String,
    /// 正文章节序号（从 1 开始）；分卷行为 0
    pub seq: i64,
    pub is_volume: bool,
}

/// 一本书的导出输入：元数据 + 目录 + 缓存统计。
#[derive(Debug, Clone, Default)]
pub struct BookInput {
    pub meta: BookMeta,
    /// 与目录顺序一致（含分卷行）
    pub chapters: Vec<ChapterRef>,
    /// 正文章节总数（不含分卷行）
    pub total_chapters: i64,
    pub cached_chapters: i64,
    pub missing_chapters: i64,
    /// 已缓存正文的 `size_bytes` 之和（不含图片）
    pub cached_bytes: i64,
}

impl BookInput {
    /// 是否所有正文章节都已缓存（分卷行不计）。
    pub fn complete(&self) -> bool {
        self.cached_chapters >= self.total_chapters
    }
}

/// 一次写入的结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteReport {
    /// 实际写入的已缓存章节数（并发清理时可能略小于计划值）
    pub written_chapters: i64,
    pub size_bytes: u64,
}

/// 一个产物：不可变文件 + 定位信息。
///
/// TXT 导出目前是临时文件、读完即删；CBZ/EPUB 起会配合 artifact 表做 hash 复用。
#[derive(Debug, Clone)]
pub struct Artifact {
    pub path: PathBuf,
    pub format: Format,
    pub size_bytes: u64,
}
