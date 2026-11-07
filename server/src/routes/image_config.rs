//! 图片代理的 config 部分（move-only 从 image.rs 拆出）。

use super::*;

/// 单张图片回源上限（字节）；流式读取按块累计，实际判定用 `CACHE_ITEM_MAX`，本常量仅单测引用。
#[allow(dead_code)]
pub(super) const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// 单张图片允许写入 LRU 的上限，超过只转发不缓存
pub(super) const CACHE_ITEM_MAX: usize = 2 * 1024 * 1024;
/// 回源超时（含首字节与整体读取）；v2.1 真机实测约 313 KB/s，无需按 1.8s/张做悲观兜底
pub(super) const IMAGE_TIMEOUT: Duration = Duration::from_secs(5);

/// 图片回源预算：书源显式设置 `timeout_millisecond` 时用它（夹取后），
/// 否则沿用图片代理自身 5s 预算，避免未设置的书源回源等待被放大。
pub(super) fn image_budget(source: &BookSource) -> (Duration, Option<Duration>) {
    match req::source_timeout(source) {
        Some(d) => (d, Some(d)),
        None => (IMAGE_TIMEOUT, None),
    }
}
/// 流式转发前为解析宽高最多预读的字节数（JPEG 的 SOF 通常很靠前）
pub(super) const PEEK_MAX: usize = 64 * 1024;
/// 墨水屏灰度 JPEG 的默认质量。8 页真实漫画的 Pareto 结论：q70 相比 q78
/// 体积 -13.6%、文字区 SSIM 仅 -0.25%（0.9819 vs 0.9844），CER 无显著变差。
/// 可用 `BORANLIB_EINK_JPEG_QUALITY` 覆盖（允许 60–90，越界会被夹到区间内）。
pub(super) const EINK_JPEG_QUALITY_DEFAULT: u8 = 70;
pub(super) const EINK_JPEG_QUALITY_MIN: u8 = 60;
pub(super) const EINK_JPEG_QUALITY_MAX: u8 = 90;
pub(super) const EINK_JPEG_QUALITY_ENV: &str = "BORANLIB_EINK_JPEG_QUALITY";
/// 缩放目标宽度：KPW3 视口 1072 CSS px，1200 约 1.12 倍；1440 只增加字迹超采样，
/// 体积 +31%，Pareto 不占优。可用 `BORANLIB_EINK_JPEG_WIDTH` 覆盖（800–1600）。
pub(super) const EINK_JPEG_WIDTH_DEFAULT: u32 = 1200;
pub(super) const EINK_JPEG_WIDTH_MIN: u32 = 800;
pub(super) const EINK_JPEG_WIDTH_MAX: u32 = 1600;
pub(super) const EINK_JPEG_WIDTH_ENV: &str = "BORANLIB_EINK_JPEG_WIDTH";
/// 自适应质量开关（默认关）：按画面中间调占比判文字页，样本命中 7/8，样本太少不默认开。
pub(super) const EINK_ADAPTIVE_ENV: &str = "BORANLIB_EINK_ADAPTIVE";
pub(super) const EINK_ADAPTIVE_TEXT_Q: u8 = 85;
pub(super) const EINK_ADAPTIVE_ART_Q: u8 = 70;
/// mid_ratio < 0.30 判为文字页；8 页标注样本上 7/8 正确（纯画面 1 页被误判为文字）。
pub(super) const EINK_ADAPTIVE_MID_THRESHOLD: f64 = 0.30;
/// 轻量 USM 开关（默认关）：样本上文字区 SSIM 略降、CER 变差，不启用。
pub(super) const EINK_USM_ENV: &str = "BORANLIB_EINK_USM";
/// 编码器标签（当前只有纯 Rust `image` 编码器；mozjpeg 需要 cmake/nasm，环境不可用）。
/// 8 页样本上 8-bit 灰度 PNG 全部 2.3–3.0 倍于 JPEG，PNG 兜底不采用，也不引入额外 encoder 特性。
pub(super) const EINK_JPEG_ENCODER_TAG: &str = "img";
/// 解码前允许的最大像素数（防解压炸弹：8 MP 解码为 RGB 约 24 MB、Luma8 约 8 MB）。
pub(super) const MAX_TRANSCODE_PIXELS: u64 = 8_000_000;

/// 墨水屏转码参数。所有影响输出的参数都必须进缓存 key，避免旧档串味。
#[derive(Clone, Copy, Debug)]
pub(super) struct EinkConfig {
    pub(super) width: u32,
    pub(super) quality: u8,
    pub(super) adaptive: bool,
    pub(super) usm: bool,
}

impl Default for EinkConfig {
    fn default() -> Self {
        Self {
            width: EINK_JPEG_WIDTH_DEFAULT,
            quality: EINK_JPEG_QUALITY_DEFAULT,
            adaptive: false,
            usm: false,
        }
    }
}

/// 转码输出档：墨水屏走灰度（Luma8，BT.709 亮度系数），彩色档仅用于对比/测试。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EncodeProfile {
    pub(super) grayscale: bool,
    pub(super) quality: u8,
}

impl EncodeProfile {
    /// 生产环境唯一使用的档位：灰度 + 可配置质量。
    pub(super) fn eink() -> Self {
        Self {
            grayscale: true,
            quality: eink_jpeg_quality(),
        }
    }
}

/// 读取墨水屏质量档：非法值回落默认 70，越界夹到 60–90。
pub(super) fn eink_jpeg_quality() -> u8 {
    std::env::var(EINK_JPEG_QUALITY_ENV)
        .ok()
        .and_then(|raw| parse_eink_jpeg_quality(&raw))
        .unwrap_or(EINK_JPEG_QUALITY_DEFAULT)
}

pub(super) fn parse_eink_jpeg_quality(raw: &str) -> Option<u8> {
    let v: u8 = raw.trim().parse().ok()?;
    Some(v.clamp(EINK_JPEG_QUALITY_MIN, EINK_JPEG_QUALITY_MAX))
}

pub(super) fn parse_env_bool(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "on" | "yes" | "eink"
    )
}

/// 从环境变量读取完整转码参数；宽度夹到 800–1600，非法值回落默认。
pub(super) fn eink_config() -> EinkConfig {
    let width = std::env::var(EINK_JPEG_WIDTH_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map(|v| v.clamp(EINK_JPEG_WIDTH_MIN, EINK_JPEG_WIDTH_MAX))
        .unwrap_or(EINK_JPEG_WIDTH_DEFAULT);
    EinkConfig {
        width,
        quality: eink_jpeg_quality(),
        adaptive: std::env::var(EINK_ADAPTIVE_ENV)
            .map(|v| parse_env_bool(&v))
            .unwrap_or(false),
        usm: std::env::var(EINK_USM_ENV)
            .map(|v| parse_env_bool(&v))
            .unwrap_or(false),
    }
}
