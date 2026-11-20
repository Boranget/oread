//! 电子书领域层：把「馆藏内容」转成可带走的格式产物。
//!
//! 借鉴 calibre 的分层（input → 中间表示 → output），但不引入它的插件系统：
//! 当前有 TXT 与 CBZ，EPUB 按同一个 [`FormatWriter`] 接口继续加。
//!
//! 纪律：
//! - **只读馆藏**，从不回写内容层；
//! - writer 按章/按页读取（[`ChapterReader`]），整本不驻留内存；
//! - 产物路径由调用方决定，格式模块不碰 `data_dir` 布局。

pub mod format;
mod html;
pub mod model;
pub mod pipeline;
pub mod source;
mod xml;
mod zip;

pub use model::{
    Artifact, BookInput, BookMeta, ChapterKind, ChapterRef, ComicInput, Format, PageData,
    WriteReport,
};
pub use pipeline::{ChapterReader, FormatWriter};
