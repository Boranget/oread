//! 转换管线：writer 面向「书视图 + 按章读取」，与存储和 HTTP 解耦。
//!
//! 新增一种格式 = 在 `format/` 下加一个 [`FormatWriter`] 实现，并在路由入口
//! 选择它；不需要改这里的管线，也不需要改读取层。

use std::path::Path;

use super::model::{BookInput, Format, WriteReport};

/// 按章读取正文。实现方负责缓存/存储细节，writer 只关心内容。
#[axum::async_trait]
pub trait ChapterReader: Send + Sync {
    /// 返回 `None` 表示该章未缓存或正文为空白（writer 应显式占位，不能静默丢章）。
    async fn chapter_text(&self, url: &str) -> anyhow::Result<Option<String>>;
}

/// 格式写入器。
#[axum::async_trait]
pub trait FormatWriter: Send + Sync {
    fn format(&self) -> Format;

    /// 把 `input` 写到 `out`。`out` 由调用方决定：HTTP 导出用临时目录，
    /// 将来的 artifact 用内容 hash 路径。
    async fn write(
        &self,
        input: &BookInput,
        reader: &dyn ChapterReader,
        out: &Path,
    ) -> anyhow::Result<WriteReport>;
}

/// 用指定 writer 产出文件。
pub async fn run(
    writer: &dyn FormatWriter,
    input: &BookInput,
    reader: &dyn ChapterReader,
    out: &Path,
) -> anyhow::Result<WriteReport> {
    writer.write(input, reader, out).await
}
