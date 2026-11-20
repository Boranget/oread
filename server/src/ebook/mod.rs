//! 电子书领域层：把「馆藏内容」转成可带走的格式产物。
//!
//! 借鉴 calibre 的分层（input → 中间表示 → output），但不引入它的插件系统：
//! 当前只有 TXT，CBZ / EPUB 按同一个 [`FormatWriter`] 接口逐个加。
//!
//! 纪律：
//! - **只读馆藏**，从不回写内容层；
//! - writer 按章读正文（[`ChapterReader`]），整本不驻留内存；
//! - 产物路径由调用方决定，格式模块不碰 `data_dir` 布局。

pub mod format;
pub mod model;
pub mod pipeline;
pub mod source;

pub use model::{BookInput, ChapterRef, Format, WriteReport};
pub use pipeline::{ChapterReader, FormatWriter};
