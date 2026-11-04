//! 图片代理：`GET /api/book/image?url=<abs>&book_url=<source_id>&chapter_url=<ch>`
//!
//! 带书源 Referer/UA/Cookie 回源，复用 `req::ensure_public_url` 做 SSRF 拦截；
//! 只允许 `image/*`；单图超过上限（默认 8 MiB）返回 413（长度未知时流式截断）；
//! 流式转发不整图入内存，同时把不超过 2 MiB 的图片写入进程内 LRU（默认 64 MiB）。
//!
//! WebP 兼容：KPW3 内核不能解码 WebP，但不少漫画源（如 content.mkzcdn.com）无论
//! Accept 怎么声明都返回 WebP。这里按客户端能力分流：老 Kindle/Silk 或 Accept 明确
//! 不含 `image/webp` 的客户端，服务端把 WebP 解码后缩到 1200px 宽、转成**灰度 JPEG**
//! （Luma8 + BT.709 亮度系数，质量默认 78，可用 `BORANLIB_EINK_JPEG_QUALITY` 调整）；
//! 其余客户端原样透传 WebP（不做无谓的 CPU 开销）。缓存 key 带色彩模式/质量后缀，
//! 避免不同档位互相覆盖。
//!
//! 墨水屏灰度：源图本身是 JPEG 时（图文小说、漫画的多数源都是 JPEG），墨水屏请求
//! 也走「解码 → Luma8 → 重编码 JPEG」，质量沿用 `BORANLIB_EINK_JPEG_QUALITY`；
//! 缓存变体用 `#jpegsrc-w1200-gq78` 独立后缀，彩色端始终原图直出、不会命中灰度图。

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;

use axum::body::{Body, Bytes};
use axum::extract::{Json, Query, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::json;

use crate::auth::AuthUser;
use crate::entities::BookSource;
use crate::routes::sources::load_source;
use crate::routes::AppState;
use crate::{image_cache, req};

/// 单张图片回源上限（字节）
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// 单张图片允许写入 LRU 的上限，超过只转发不缓存
const CACHE_ITEM_MAX: usize = 2 * 1024 * 1024;
/// 回源超时（含首字节与整体读取）；v2.1 真机实测约 313 KB/s，无需按 1.8s/张做悲观兜底
const IMAGE_TIMEOUT: Duration = Duration::from_secs(5);
/// 流式转发前为解析宽高最多预读的字节数（JPEG 的 SOF 通常很靠前）
const PEEK_MAX: usize = 64 * 1024;
/// 墨水屏灰度 JPEG 的默认质量。8 页真实漫画的 Pareto 结论：q70 相比 q78
/// 体积 -13.6%、文字区 SSIM 仅 -0.25%（0.9819 vs 0.9844），CER 无显著变差。
/// 可用 `BORANLIB_EINK_JPEG_QUALITY` 覆盖（允许 60–90，越界会被夹到区间内）。
const EINK_JPEG_QUALITY_DEFAULT: u8 = 70;
const EINK_JPEG_QUALITY_MIN: u8 = 60;
const EINK_JPEG_QUALITY_MAX: u8 = 90;
const EINK_JPEG_QUALITY_ENV: &str = "BORANLIB_EINK_JPEG_QUALITY";
/// 缩放目标宽度：KPW3 视口 1072 CSS px，1200 约 1.12 倍；1440 只增加字迹超采样，
/// 体积 +31%，Pareto 不占优。可用 `BORANLIB_EINK_JPEG_WIDTH` 覆盖（800–1600）。
const EINK_JPEG_WIDTH_DEFAULT: u32 = 1200;
const EINK_JPEG_WIDTH_MIN: u32 = 800;
const EINK_JPEG_WIDTH_MAX: u32 = 1600;
const EINK_JPEG_WIDTH_ENV: &str = "BORANLIB_EINK_JPEG_WIDTH";
/// 自适应质量开关（默认关）：按画面中间调占比判文字页，样本命中 7/8，样本太少不默认开。
const EINK_ADAPTIVE_ENV: &str = "BORANLIB_EINK_ADAPTIVE";
const EINK_ADAPTIVE_TEXT_Q: u8 = 85;
const EINK_ADAPTIVE_ART_Q: u8 = 70;
/// mid_ratio < 0.30 判为文字页；8 页标注样本上 7/8 正确（纯画面 1 页被误判为文字）。
const EINK_ADAPTIVE_MID_THRESHOLD: f64 = 0.30;
/// 轻量 USM 开关（默认关）：样本上文字区 SSIM 略降、CER 变差，不启用。
const EINK_USM_ENV: &str = "BORANLIB_EINK_USM";
/// 编码器标签（当前只有纯 Rust `image` 编码器；mozjpeg 需要 cmake/nasm，环境不可用）。
/// 8 页样本上 8-bit 灰度 PNG 全部 2.3–3.0 倍于 JPEG，PNG 兜底不采用，也不引入额外 encoder 特性。
const EINK_JPEG_ENCODER_TAG: &str = "img";
/// 解码前允许的最大像素数（防解压炸弹：8 MP 解码为 RGB 约 24 MB、Luma8 约 8 MB）。
const MAX_TRANSCODE_PIXELS: u64 = 8_000_000;

/// 墨水屏转码参数。所有影响输出的参数都必须进缓存 key，避免旧档串味。
#[derive(Clone, Copy, Debug)]
struct EinkConfig {
    width: u32,
    quality: u8,
    adaptive: bool,
    usm: bool,
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
struct EncodeProfile {
    grayscale: bool,
    quality: u8,
}

impl EncodeProfile {
    /// 生产环境唯一使用的档位：灰度 + 可配置质量。
    fn eink() -> Self {
        Self {
            grayscale: true,
            quality: eink_jpeg_quality(),
        }
    }
}

/// 读取墨水屏质量档：非法值回落默认 70，越界夹到 60–90。
fn eink_jpeg_quality() -> u8 {
    std::env::var(EINK_JPEG_QUALITY_ENV)
        .ok()
        .and_then(|raw| parse_eink_jpeg_quality(&raw))
        .unwrap_or(EINK_JPEG_QUALITY_DEFAULT)
}

fn parse_eink_jpeg_quality(raw: &str) -> Option<u8> {
    let v: u8 = raw.trim().parse().ok()?;
    Some(v.clamp(EINK_JPEG_QUALITY_MIN, EINK_JPEG_QUALITY_MAX))
}

fn parse_env_bool(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "on" | "yes" | "eink"
    )
}

/// 从环境变量读取完整转码参数；宽度夹到 800–1600，非法值回落默认。
fn eink_config() -> EinkConfig {
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

#[derive(Deserialize)]
pub struct ImageQ {
    pub url: String,
    /// 书源 id（Legado 的 `bookSourceUrl`）
    pub source: Option<String>,
    /// 契约字段名：`book_url=<src>`，语义与 `source` 一致
    pub book_url: Option<String>,
    /// 章节页地址，用作 Referer
    pub chapter_url: Option<String>,
    /// 墨水屏模式显式开关：`eink=1`（等价于请求头 `X-Device-Mode: eink`）
    pub eink: Option<String>,
    /// 长条页切片开关：`slice=1`（仅墨水屏生效）。默认不切，`slice=0` 的字节输出与旧版一致。
    pub slice: Option<String>,
    /// 第几片（0 起），越界会被夹到 `[0, n-1]`
    pub i: Option<usize>,
    /// 切片参数覆盖（CSS px）：版心宽 / 可用高 / 片间 overlap
    pub dw: Option<u32>,
    pub vh: Option<u32>,
    pub ov: Option<u32>,
}

/// 批量元数据请求：一次返回整章图片的宽高/字节数，避免“一张一个额外请求”（KPW3 每次请求约 0.3s TTFB）。
#[derive(Deserialize)]
pub struct ImageMetaReq {
    pub source: Option<String>,
    pub book_url: Option<String>,
    pub chapter_url: Option<String>,
    pub urls: Vec<String>,
}

pub fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/book/image", axum::routing::get(proxy))
        .route("/api/book/images/meta", axum::routing::post(images_meta))
}

fn error_body(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        json!({"error": code, "message": message}).to_string(),
    )
        .into_response()
}

fn etag_of(url: &str) -> String {
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    format!("\"{:016x}\"", h.finish())
}

fn is_image_content_type(ct: &str) -> bool {
    let base = ct.split(';').next().unwrap_or("").trim().to_lowercase();
    base.starts_with("image/")
}

fn is_webp_content_type(ct: &str) -> bool {
    let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    base == "image/webp"
}

fn is_jpeg_content_type(ct: &str) -> bool {
    let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    matches!(base.as_str(), "image/jpeg" | "image/jpg" | "image/pjpeg")
}

/// 是否需要把 WebP 转成 JPEG：老 Kindle/Silk 一律转；其他客户端仅在 Accept 明确
/// 不含 `image/webp` 时转。Accept 缺失按现代客户端处理，原样透传。
fn wants_webp_transcode(headers: &HeaderMap) -> bool {
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if crate::routes::kindle::is_kindle_ua(ua) {
        return true;
    }
    match headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()) {
        Some(accept) => !accept.to_ascii_lowercase().contains("image/webp"),
        None => false,
    }
}

/// 是否墨水屏客户端：Kindle/Silk 真机 UA，或前端显式声明 eink 模式
/// （`X-Device-Mode: eink` 头 / `?eink=1` 参数，二者等价）。
/// 普通桌面/手机浏览器不开 eink 模式时一律原图直出，不做灰度重编码。
fn is_eink_client(headers: &HeaderMap, q_eink: Option<&str>) -> bool {
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if crate::routes::kindle::is_kindle_ua(ua) {
        return true;
    }
    if let Some(v) = q_eink {
        let v = v.trim().to_ascii_lowercase();
        if v == "1" || v == "true" || v == "eink" {
            return true;
        }
    }
    headers
        .get("x-device-mode")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().eq_ignore_ascii_case("eink"))
        .unwrap_or(false)
}

/// 缓存 key 的客户端变体：转码后的 JPEG 与原图必须分开存，宽度/质量/编码器/自适应
/// 也必须分开，否则桌面/墨水屏、灰度/彩色、不同调参档会互相覆盖。
/// `jpeg_source` 区分「WebP 转码」与「JPEG 源灰度」两条路径。
fn variant_key(url: &str, jpeg_source: bool, cfg: &EinkConfig, quality: u8) -> String {
    let kind = if jpeg_source { "jpegsrc" } else { "jpeg" };
    format!(
        "{url}#{kind}-w{}-q{}-enc{}-a{}{}",
        cfg.width,
        quality,
        EINK_JPEG_ENCODER_TAG,
        u8::from(cfg.adaptive),
        if cfg.usm { "-usm" } else { "" }
    )
}

/// 自适应开启时质量由画面内容决定，缓存查找需要同时试两个档位。
fn candidate_qualities(cfg: &EinkConfig) -> Vec<u8> {
    if cfg.adaptive {
        vec![EINK_ADAPTIVE_TEXT_Q, EINK_ADAPTIVE_ART_Q]
    } else {
        vec![cfg.quality]
    }
}

// ---------------- 长条页切片（墨水屏） ----------------

/// 切片参数（CSS px / 亮度阈值）：全部进缓存 key，参数一变 key 必变。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SliceParams {
    pub dw: u32,
    pub vh: u32,
    pub ov: u32,
    /// 近白行亮度均值下限
    pub mu: u8,
    /// 近白行带内标准差上限
    pub sd: u8,
    /// 切点搜索半径（CSS px）
    pub r: u32,
}

impl Default for SliceParams {
    fn default() -> Self {
        Self {
            dw: SLICE_DW_DEFAULT,
            vh: SLICE_VH_DEFAULT,
            ov: SLICE_OV_DEFAULT,
            mu: SLICE_MU,
            sd: SLICE_SD_U8,
            r: SLICE_R_CSS,
        }
    }
}

impl SliceParams {
    /// 非法/越界值夹到安全区间：版心 320–2000、可用高 320–2000、overlap ≤ 1/4 窗高。
    #[cfg(test)]
    fn new(dw: Option<u32>, vh: Option<u32>, ov: Option<u32>) -> Self {
        let d = Self::default();
        Self::resolve(d.dw, d.vh, d.ov, dw, vh, ov, d.mu, d.sd, d.r)
    }

    /// 生效值：环境变量给出的默认值（见 `Config`），查询参数可覆盖版心宽/可用高/重叠。
    pub(crate) fn from_config(cfg: &crate::config::Config, dw: Option<u32>, vh: Option<u32>, ov: Option<u32>) -> Self {
        Self::resolve(
            cfg.comic_slice_dw,
            cfg.comic_slice_vh,
            cfg.comic_slice_ov,
            dw,
            vh,
            ov,
            cfg.comic_slice_mu,
            cfg.comic_slice_sd,
            cfg.comic_slice_r,
        )
    }

    fn resolve(
        dw: u32,
        vh: u32,
        ov: u32,
        dw_q: Option<u32>,
        vh_q: Option<u32>,
        ov_q: Option<u32>,
        mu: u8,
        sd: u8,
        r: u32,
    ) -> Self {
        let dw = dw_q.unwrap_or(dw).clamp(320, 2000);
        let vh = vh_q.unwrap_or(vh).clamp(320, 2000);
        let ov = ov_q.unwrap_or(ov).clamp(0, vh / 4);
        Self {
            dw,
            vh,
            ov,
            mu,
            sd,
            r: r.clamp(8, 480),
        }
    }

    /// 缓存 key 里的参数段；参数一变 key 必变，旧档不会串味。
    /// `sl2` + `ik2-g10`：新增切点邻域墨量守卫后升版，旧 `sl1` 切片一律不再命中。
    fn label(&self) -> String {
        format!(
            "sl2-dw{}-vh{}-ov{}-mu{}-sd{}-r{}-ik{}-g{}",
            self.dw, self.vh, self.ov, self.mu, self.sd, self.r, SLICE_INK_PCT, SLICE_G_CSS
        )
    }
}

/// 版心宽（CSS px）
const SLICE_DW_DEFAULT: u32 = 928;
/// 可用高（CSS px）= 1268 舞台 − 72 底栏留白
const SLICE_VH_DEFAULT: u32 = 1196;
/// 片间 overlap（CSS px）：硬切时保证上下片在切点附近不丢行
const SLICE_OV_DEFAULT: u32 = 64;
/// 切点搜索半径（CSS px）
const SLICE_R_CSS: u32 = 96;
/// 近白行判定：3 行带 × 64 列采样的亮度均值下限
const SLICE_MU: u8 = 200;
/// 近白行判定：带内标准差上限（缓存 key 用整数标签，切片时转 f64）
const SLICE_SD_U8: u8 = 30;
/// 采样列数（整行扫描 1200×2872 要 340 万次读取，采样后每候选行约 200 次）
const SLICE_SAMPLE_COLS: usize = 64;
/// 切点邻域校验带宽（CSS px）：±10px 内出现暗像素即判「切在画面上」
const SLICE_G_CSS: u32 = 10;
/// 切点邻域暗像素占比上限（%）：切点行自身 ≤2%，带内均值 ≤4×2%
const SLICE_INK_PCT: u8 = 2;
/// 带内均值相对切点行阈值的倍数
const SLICE_INK_BAND_MULT: f64 = 4.0;
/// 暗像素亮度阈值（0–255）：低于此值算「画面内容」
const SLICE_DARK_LUMA: u8 = 140;
/// 末片最小高度（相对窗高的比例）：避免只剩一条 64–128px 的「薄片」
const SLICE_TAIL_MIN: f64 = 0.3;
/// 相邻切点最小间距（相对窗高的比例）：防御性下限，防止裁剪出过短的片
const SLICE_MIN_GAP: f64 = 0.35;
/// 超过 1.3 屏才切：1.1 屏页切两片字节反而 +6.7%，首片已是整图 92%
const SLICE_MAX_SCREENS: f64 = 1.3;

/// `slice=1/true/on` 才启用切片；缺省与 `slice=0` 走整图路径，字节输出与旧版一致。
fn slice_requested(raw: Option<&str>) -> bool {
    raw.map(|v| {
        let v = v.trim().to_ascii_lowercase();
        v == "1" || v == "true" || v == "on"
    })
    .unwrap_or(false)
}

/// 显示高度（CSS px）= 原图高 × 版心宽 / 原图宽。
fn display_height(w: u32, h: u32, p: &SliceParams) -> f64 {
    if w == 0 {
        return 0.0;
    }
    h as f64 * p.dw as f64 / w as f64
}

/// 是否值得切：显示高度超过 1.3 屏才切。
pub fn worth_slicing(w: u32, h: u32, p: &SliceParams) -> bool {
    display_height(w, h, p) > p.vh as f64 * SLICE_MAX_SCREENS
}

/// 源像素空间的（窗高, overlap, 搜索半径, 守卫带宽）：按显示缩放比换算，各至少 1px。
fn src_metrics(img_w: u32, p: &SliceParams) -> (usize, usize, usize, usize) {
    let css_to_src = img_w.max(1) as f64 / p.dw as f64;
    let win = ((p.vh as f64 * css_to_src).round() as usize).max(64);
    let ov = ((p.ov as f64 * css_to_src).round() as usize).min(win / 4);
    let r = ((p.r as f64 * css_to_src).round() as usize).max(1);
    let g = ((SLICE_G_CSS as f64 * css_to_src).round() as usize).max(1);
    (win, ov, r, g)
}

/// 3 行带 × 64 列采样的亮度（μ, σ）；行越界返回 `None`。
fn band_stats(raw: &[u8], w: usize, h: usize, y: usize) -> Option<(u8, f64)> {
    if y >= h || w == 0 {
        return None;
    }
    let y0 = y.saturating_sub(1);
    let y1 = (y + 1).min(h - 1);
    let step = (w / SLICE_SAMPLE_COLS).max(1);
    let mut sum = 0f64;
    let mut sum2 = 0f64;
    let mut n = 0u64;
    for row in y0..=y1 {
        let base = row * w;
        let mut x = 0usize;
        while x < w {
            let v = raw[base + x] as f64;
            sum += v;
            sum2 += v * v;
            n += 1;
            x += step;
        }
    }
    if n == 0 {
        return None;
    }
    let mean = sum / n as f64;
    let var = (sum2 / n as f64 - mean * mean).max(0.0);
    Some((mean.round().clamp(0.0, 255.0) as u8, var.sqrt()))
}

/// 单行暗像素（`luma < 140`）占比，全宽隔列采样；越界返回 `None`。
fn row_ink_ratio(raw: &[u8], w: usize, h: usize, y: usize) -> Option<f64> {
    if y >= h || w == 0 {
        return None;
    }
    let step = (w / 256).max(1);
    let base = y * w;
    let mut ink = 0u64;
    let mut n = 0u64;
    let mut x = 0usize;
    while x < w {
        if raw[base + x] < SLICE_DARK_LUMA {
            ink += 1;
        }
        n += 1;
        x += step;
    }
    if n == 0 {
        return None;
    }
    Some(ink as f64 / n as f64)
}

/// 切点邻域守卫：切点行自身墨量 ≤ `ink_pct`%，±`g` 带内均值 ≤ `ink_pct × 4`%。
///
/// 空白沟槽墨量 ≈ 0；脸/眼/发区域即便切点行恰好落在高光，带内也必有暗像素而被拒。
/// 只读已解码的灰度缓冲，不额外分配内存。
fn cut_guard_ok(raw: &[u8], w: usize, h: usize, y: usize, g: usize, ink_pct: f64) -> bool {
    let Some(own) = row_ink_ratio(raw, w, h, y) else {
        return false;
    };
    if own * 100.0 > ink_pct {
        return false;
    }
    let y0 = y.saturating_sub(g);
    let y1 = (y + g).min(h - 1);
    let mut sum = 0.0;
    let mut n = 0usize;
    for row in y0..=y1 {
        if let Some(v) = row_ink_ratio(raw, w, h, row) {
            sum += v;
            n += 1;
        }
    }
    n > 0 && (sum / n as f64) * 100.0 <= ink_pct * SLICE_INK_BAND_MULT
}

/// 纯函数切点规划：返回升序切点，首尾必为 `0` 与 `h`。
///
/// 目标切点是本片最远端 `start + win`（再往下就超过屏高）；在 `[target − r, target]`
/// 内找近白行（3 行带 × 64 列采样，μ ≥ `mu` 且 σ ≤ `sd`），再按「切点邻域墨量」二次校验：
/// 切点行自身暗像素 ≤ `ink_pct`%，±`g` 带内均值 ≤ 4×`ink_pct`%。全部不过时回退到不校验的
/// 最近近白行（与旧行为一致），再无近白行则硬切在 `target`。
/// 切点不超过 `start + win` 且间距 ≥ 0.35×窗高，保证每片显示高 ≤ `vh`；末刀优先让末片高度
/// ≥ 0.3×窗高（免得出 64–128px 的薄片）。同一图同一参数结果恒定。
pub fn plan_cuts(
    luma: &image::GrayImage,
    win_src: usize,
    ov_src: usize,
    r_src: usize,
    g_src: usize,
    mu: u8,
    sd: f64,
    ink_pct: f64,
) -> Vec<usize> {
    let w = luma.width() as usize;
    let h = luma.height() as usize;
    if h == 0 || w == 0 {
        return vec![0, h];
    }
    let win = win_src.max(64);
    let min_gap = ((win as f64 * SLICE_MIN_GAP).ceil() as usize).max(1);
    let min_tail = ((win as f64 * SLICE_TAIL_MIN).ceil() as usize).max(1);
    let ov = ov_src.min(win / 4);
    if h <= win {
        return vec![0, h];
    }
    let raw = luma.as_raw();
    let mut cuts = vec![0usize];
    let mut start = 0usize;
    let mut guard = 0usize;
    while start + win < h && guard < 512 {
        guard += 1;
        let target = start + win;
        // 间距下限：r 很大时也不许把切点贴到上一刀旁边
        let lo = target.saturating_sub(r_src).max(start + min_gap);
        let hi = (target + r_src).min(start + win).min(h - 1);
        // 落在 [h − win + ov, h) 的切点就是最后一刀，此时末片高度直接决定观感
        let last_from = h.saturating_sub(win.saturating_sub(ov));
        // (距离, y, 均值, 末片是否够高)：优先守卫通过 + 末片够高，再比距离/亮度
        let mut best_ok: Option<(usize, usize, u8, bool)> = None;
        // 守卫全不过时的回退：旧的「最近近白行」，保证不劣于现状
        let mut best_raw: Option<(usize, usize, u8)> = None;
        for y in lo..=hi {
            let Some((m, s)) = band_stats(raw, w, h, y) else {
                continue;
            };
            if m < mu || s > sd {
                continue;
            }
            let d = y.abs_diff(target);
            let better_raw = match best_raw {
                None => true,
                Some((bd, _, bm)) => d < bd || (d == bd && m > bm),
            };
            if better_raw {
                best_raw = Some((d, y, m));
            }
            if !cut_guard_ok(raw, w, h, y, g_src, ink_pct) {
                continue;
            }
            let tail_ok = if y >= last_from { h - y >= min_tail } else { true };
            let better = match best_ok {
                None => true,
                Some((bd, _, bm, bt)) => {
                    if tail_ok != bt {
                        tail_ok
                    } else {
                        d < bd || (d == bd && m > bm)
                    }
                }
            };
            if better {
                best_ok = Some((d, y, m, tail_ok));
            }
        }
        let cut = best_ok
            .map(|(_, y, _, _)| y)
            .or_else(|| best_raw.map(|(_, y, _)| y))
            .unwrap_or(target)
            .clamp(start + min_gap, h);
        debug_assert!(cut - start >= min_gap.min(h - start), "切点间距下限失效");
        cuts.push(cut);
        if cut + ov >= h {
            break;
        }
        start = cut.saturating_sub(ov);
    }
    if cuts.last().copied() != Some(h) {
        cuts.push(h);
    }
    cuts
}

/// 由切点展开成每片的源行区间：第 i>0 片向前多吃 `ov` 行，形成片间重叠。
pub fn slice_ranges(cuts: &[usize], ov_src: usize, h: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for i in 0..cuts.len().saturating_sub(1) {
        let start = if i == 0 {
            0
        } else {
            cuts[i].saturating_sub(ov_src)
        };
        let end = cuts[i + 1].min(h);
        if end > start {
            out.push((start, end));
        }
    }
    out
}

/// 只按窗口算片数（不读像素）：`1 + ceil((h − win) / (win − ov))`，供 meta/目录预判。
pub fn slice_count_window(w: u32, h: u32, p: &SliceParams) -> usize {
    if !worth_slicing(w, h, p) {
        return 1;
    }
    let (win, ov, _, _) = src_metrics(w, p);
    let hh = h as usize;
    if hh <= win {
        return 1;
    }
    let step = win.saturating_sub(ov).max(1);
    let extra = (hh - win).div_ceil(step);
    1 + extra
}

/// 每片显示高度（CSS px）估算：非末片按窗高，末片按剩余。实际白切点会在此附近 ±r。
pub fn slice_display_heights(w: u32, h: u32, p: &SliceParams) -> Vec<u32> {
    let scale = p.dw as f64 / w.max(1) as f64;
    let n = slice_count_window(w, h, p);
    if n <= 1 {
        return vec![(h as f64 * scale).round().min(p.vh as f64) as u32];
    }
    let (win, ov, _, _) = src_metrics(w, p);
    let hh = h as usize;
    let mut out = Vec::with_capacity(n);
    let mut start = 0usize;
    while start + win < hh {
        out.push(p.vh);
        start += win - ov;
        if start >= hh {
            break;
        }
    }
    let remain = hh.saturating_sub(start);
    if remain > 0 {
        out.push(((remain as f64 * scale).round() as u32).min(p.vh));
    }
    out.truncate(n);
    out
}

/// 切片缓存 key：`i{i}of{n}` 必须与片数一起进 key，避免「缓存 3 片、请求要 2 片」错位。
fn slice_variant_key(base: &str, p: &SliceParams, i: usize, n: usize) -> String {
    format!("{base}#{}-i{}of{}", p.label(), i, n)
}

/// 切点计划缓存 key：只依赖原图与切片参数（质量/自适应不影响切点）。
fn slice_plan_key(url: &str, cfg: &EinkConfig, p: &SliceParams) -> String {
    format!("{url}#{}-plan-w{}-a{}", p.label(), cfg.width, u8::from(cfg.adaptive))
}

/// 切点计划：片数 + 源行切点，随原图确定性生成，可跨请求复用。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct SlicePlan {
    n: usize,
    cuts: Vec<usize>,
}

fn plan_get(key: &str) -> Option<SlicePlan> {
    image_cache::get(key).and_then(|(b, _)| serde_json::from_slice(&b).ok())
}

fn plan_put(key: &str, plan: &SlicePlan) {
    if let Ok(b) = serde_json::to_vec(plan) {
        image_cache::put(key, b, "application/json".to_string());
    }
}

/// 从灰度图裁出 `[y0, y1)` 行的一段（切片编码用）。
fn crop_luma_rows(luma: &image::GrayImage, y0: usize, y1: usize) -> Option<image::GrayImage> {
    let w = luma.width() as usize;
    let h = luma.height() as usize;
    if y0 >= y1 || y1 > h {
        return None;
    }
    let buf = luma.as_raw()[y0 * w..y1 * w].to_vec();
    image::GrayImage::from_raw(luma.width(), (y1 - y0) as u32, buf)
}

/// 切片结果：整图退化时 `n == 1`，与 `slice=0` 的字节完全一致。
struct SliceOutcome {
    data: Vec<u8>,
    dims: Dims,
    quality: u8,
    n: usize,
    index: usize,
    plan: Option<SlicePlan>,
}

/// 解码 → 灰度/缩放 → 切点规划 → 裁片 → JPEG q70 重编码。
/// 显示高度 ≤ 1.3 屏时直接退化成整图（不切），保证 1.1 屏页字节不变。
fn transcode_slice(
    raw: &[u8],
    format: image::ImageFormat,
    profile: EncodeProfile,
    cfg: EinkConfig,
    p: &SliceParams,
    want: usize,
) -> Result<SliceOutcome, String> {
    let reader = image::ImageReader::with_format(std::io::Cursor::new(raw), format);
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| format!("图片头部解析失败: {e}"))?;
    if w == 0 || h == 0 {
        return Err("图片尺寸为 0".into());
    }
    if !worth_slicing(w, h, p) {
        let (data, dims, quality) = transcode_to_jpeg(raw, format, profile, cfg)?;
        return Ok(SliceOutcome {
            data,
            dims,
            quality,
            n: 1,
            index: 0,
            plan: None,
        });
    }
    let img = image::load_from_memory_with_format(raw, format)
        .map_err(|e| format!("图片解码失败: {e}"))?;
    let luma = img.to_luma8();
    let (tw, th) = transcode_target_size(luma.width(), luma.height(), cfg.width);
    let resized = if (tw, th) == (luma.width(), luma.height()) {
        luma
    } else {
        resize_lanczos_linear(&luma, tw, th)
    };
    let (win, ov, r, g) = src_metrics(resized.width(), p);
    let cuts = plan_cuts(&resized, win, ov, r, g, p.mu, p.sd as f64, SLICE_INK_PCT as f64);
    let ranges = slice_ranges(&cuts, ov, resized.height() as usize);
    if ranges.len() <= 1 {
        let out = if cfg.usm { usm_luma(&resized) } else { resized };
        let dims = Dims {
            width: out.width(),
            height: out.height(),
        };
        let mut buf = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, profile.quality)
            .encode_image(&out)
            .map_err(|e| format!("JPEG 编码失败: {e}"))?;
        return Ok(SliceOutcome {
            data: buf,
            dims,
            quality: profile.quality,
            n: 1,
            index: 0,
            plan: None,
        });
    }
    let n = ranges.len();
    let index = want.min(n - 1);
    let (y0, y1) = ranges[index];
    let slice = crop_luma_rows(&resized, y0, y1).ok_or_else(|| "切片越界".to_string())?;
    let out = if cfg.usm { usm_luma(&slice) } else { slice };
    let dims = Dims {
        width: out.width(),
        height: out.height(),
    };
    let quality = profile.quality;
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
        .encode_image(&out)
        .map_err(|e| format!("JPEG 编码失败: {e}"))?;
    Ok(SliceOutcome {
        data: buf,
        dims,
        quality,
        n,
        index,
        plan: Some(SlicePlan {
            n,
            cuts,
        }),
    })
}


fn key_pairs(url: &str, jpeg_source: bool, cfg: &EinkConfig) -> Vec<(String, String)> {
    candidate_qualities(cfg)
        .into_iter()
        .map(|q| {
            let key = variant_key(url, jpeg_source, cfg, q);
            let etag = etag_of(&key);
            (key, etag)
        })
        .collect()
}

fn cache_lookup(keys: &[(String, String)]) -> Option<(Vec<u8>, String, String)> {
    for (key, etag) in keys {
        if let Some((data, ct)) = image_cache::get(key) {
            return Some((data, ct, etag.clone()));
        }
    }
    None
}

/// 找到客户端 `If-None-Match` 命中的那个变体 ETag（用于 304 响应）。
fn matching_key_etag<'a>(headers: &HeaderMap, keys: &'a [(String, String)]) -> Option<&'a String> {
    keys.iter()
        .find(|(_, etag)| matches_if_none_match(headers, etag))
        .map(|(_, etag)| etag)
}

/// 目标尺寸：宽度超过上限时等比缩小，绝不放大（1024 宽的手机源保持原尺寸）。
fn transcode_target_size(w: u32, h: u32, max_width: u32) -> (u32, u32) {
    if w <= max_width || h == 0 {
        return (w.max(1), h.max(1));
    }
    let nh = ((h as u64 * max_width as u64) / w as u64).max(1) as u32;
    (max_width, nh)
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn sinc(x: f32) -> f32 {
    if x.abs() < 1e-6 {
        1.0
    } else {
        let px = std::f32::consts::PI * x;
        px.sin() / px
    }
}

fn lanczos3(x: f32) -> f32 {
    if x.abs() < 3.0 {
        sinc(x) * sinc(x / 3.0)
    } else {
        0.0
    }
}

/// 分离式 Lanczos3，在线性光空间缩放（gamma 正确），避免暗部被过度压暗。
fn resize_lanczos_linear(src: &image::GrayImage, tw: u32, th: u32) -> image::GrayImage {
    let sw = src.width() as usize;
    let sh = src.height() as usize;
    let twu = tw as usize;
    let thu = th as usize;
    let a = src.as_raw();
    let mut lin = vec![0f32; sw * sh];
    for i in 0..sw * sh {
        lin[i] = srgb_to_linear(a[i] as f32 / 255.0);
    }

    let mut tmp = vec![0f32; twu * sh];
    let scale_x = sw as f32 / twu as f32;
    let support_x = 3.0 * scale_x.max(1.0);
    for x in 0..twu {
        let center = (x as f32 + 0.5) * scale_x - 0.5;
        let start = ((center - support_x).floor() as isize).max(0);
        let end = ((center + support_x).ceil() as isize).min(sw as isize - 1);
        let mut weights = Vec::new();
        let mut sum = 0f32;
        for s in start..=end {
            let d = (s as f32 - center) / scale_x.max(1.0);
            let w = lanczos3(d);
            weights.push((s as usize, w));
            sum += w;
        }
        if sum.abs() < 1e-9 {
            weights.clear();
            let s = center.round().clamp(0.0, (sw - 1) as f32) as usize;
            weights.push((s, 1.0));
            sum = 1.0;
        }
        for y in 0..sh {
            let mut v = 0f32;
            for &(s, w) in &weights {
                v += lin[y * sw + s] * w;
            }
            tmp[y * twu + x] = (v / sum).clamp(0.0, 1.0);
        }
    }

    let mut out = vec![0u8; twu * thu];
    let scale_y = sh as f32 / thu as f32;
    let support_y = 3.0 * scale_y.max(1.0);
    for y in 0..thu {
        let center = (y as f32 + 0.5) * scale_y - 0.5;
        let start = ((center - support_y).floor() as isize).max(0);
        let end = ((center + support_y).ceil() as isize).min(sh as isize - 1);
        let mut weights = Vec::new();
        let mut sum = 0f32;
        for s in start..=end {
            let d = (s as f32 - center) / scale_y.max(1.0);
            let w = lanczos3(d);
            weights.push((s as usize, w));
            sum += w;
        }
        if sum.abs() < 1e-9 {
            weights.clear();
            let s = center.round().clamp(0.0, (sh - 1) as f32) as usize;
            weights.push((s, 1.0));
            sum = 1.0;
        }
        for x in 0..twu {
            let mut v = 0f32;
            for &(s, w) in &weights {
                v += tmp[s * twu + x] * w;
            }
            out[y * twu + x] = (linear_to_srgb((v / sum).clamp(0.0, 1.0)) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
    image::GrayImage::from_raw(tw, th, out).expect("尺寸与缓冲一致")
}

/// 轻量 USM：5×5 高斯近似（sigma≈0.8），量 0.3，满足「半径≤1、量≤0.3」。
fn usm_luma(src: &image::GrayImage) -> image::GrayImage {
    let w = src.width() as usize;
    let h = src.height() as usize;
    let a = src.as_raw();
    let kernel = [0.0545f32, 0.2442, 0.4026, 0.2442, 0.0545];
    let mut tmp = vec![0f32; w * h];
    let mut blur = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut v = 0f32;
            for (k, kw) in kernel.iter().enumerate() {
                let sx = (x as isize + k as isize - 2).clamp(0, w as isize - 1) as usize;
                v += a[y * w + sx] as f32 * kw;
            }
            tmp[y * w + x] = v;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut v = 0f32;
            for (k, kw) in kernel.iter().enumerate() {
                let sy = (y as isize + k as isize - 2).clamp(0, h as isize - 1) as usize;
                v += tmp[sy * w + x] * kw;
            }
            blur[y * w + x] = v;
        }
    }
    let mut out = vec![0u8; w * h];
    for i in 0..w * h {
        let v = a[i] as f32;
        out[i] = (v + 0.3 * (v - blur[i]).round()).round().clamp(0.0, 255.0) as u8;
    }
    image::GrayImage::from_raw(src.width(), src.height(), out).expect("尺寸与缓冲一致")
}

/// 自适应判定：中间调（80–200）像素占比 < 0.30 视为文字页，走 q85；否则画面页 q70。
/// 按 2 像素步长采样，避免整页 Sobel 的 CPU 开销。
fn adaptive_quality(luma: &image::GrayImage) -> u8 {
    let w = luma.width() as usize;
    let h = luma.height() as usize;
    let a = luma.as_raw();
    let mut mid = 0u64;
    let mut n = 0u64;
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let v = a[y * w + x];
            if (80..200).contains(&v) {
                mid += 1;
            }
            n += 1;
        }
    }
    if (mid as f64 / n.max(1) as f64) < EINK_ADAPTIVE_MID_THRESHOLD {
        EINK_ADAPTIVE_TEXT_Q
    } else {
        EINK_ADAPTIVE_ART_Q
    }
}

/// 通用「解码 → 灰度/彩色 → 缩放 → JPEG 重编码」，WebP 源与 JPEG 源共用。
/// 返回 (输出字节, 尺寸, 实际质量)。灰度档先转 Luma8 再缩放，缩放量与编码量
/// 都只有 RGB 的 1/3。
fn transcode_to_jpeg(
    raw: &[u8],
    format: image::ImageFormat,
    profile: EncodeProfile,
    cfg: EinkConfig,
) -> Result<(Vec<u8>, Dims, u8), String> {
    let label = match format {
        image::ImageFormat::WebP => "WebP",
        image::ImageFormat::Jpeg => "JPEG",
        _ => "图片",
    };
    let reader = image::ImageReader::with_format(std::io::Cursor::new(raw), format);
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| format!("{label} 头部解析失败: {e}"))?;
    if w == 0 || h == 0 {
        return Err(format!("{label} 尺寸为 0"));
    }
    if w as u64 * h as u64 > MAX_TRANSCODE_PIXELS {
        return Err(format!(
            "源图 {w}x{h} 超过解码像素上限 {MAX_TRANSCODE_PIXELS}"
        ));
    }
    let img = image::load_from_memory_with_format(raw, format)
        .map_err(|e| format!("{label} 解码失败: {e}"))?;
    let (src_w, src_h) = (img.width(), img.height());
    let (tw, th) = transcode_target_size(src_w, src_h, cfg.width);
    let mut buf: Vec<u8> = Vec::new();
    if profile.grayscale {
        // to_luma8 使用 BT.709 亮度系数（0.2126R + 0.7152G + 0.0722B），不是通道平均
        let luma = img.to_luma8();
        let quality = if cfg.adaptive {
            adaptive_quality(&luma)
        } else {
            profile.quality
        };
        let resized = if (tw, th) == (luma.width(), luma.height()) {
            luma
        } else {
            resize_lanczos_linear(&luma, tw, th)
        };
        let out = if cfg.usm { usm_luma(&resized) } else { resized };
        let dims = Dims {
            width: out.width(),
            height: out.height(),
        };
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
            .encode_image(&out)
            .map_err(|e| format!("JPEG 编码失败: {e}"))?;
        Ok((buf, dims, quality))
    } else {
        let rgb = img.to_rgb8();
        let out = if (tw, th) == (rgb.width(), rgb.height()) {
            rgb
        } else {
            image::imageops::resize(&rgb, tw, th, image::imageops::FilterType::Triangle)
        };
        let dims = Dims {
            width: out.width(),
            height: out.height(),
        };
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, profile.quality)
            .encode_image(&out)
            .map_err(|e| format!("JPEG 编码失败: {e}"))?;
        Ok((buf, dims, profile.quality))
    }
}

/// WebP → JPEG（内存内完成，不落盘）。仅测试用便捷封装；生产路径直接调用 `transcode_to_jpeg`。
#[cfg(test)]
fn transcode_webp_to_jpeg(
    raw: &[u8],
    profile: EncodeProfile,
) -> Result<(Vec<u8>, Dims), String> {
    let (data, dims, _) =
        transcode_to_jpeg(raw, image::ImageFormat::WebP, profile, EinkConfig::default())?;
    Ok((data, dims))
}

/// JPEG 源 → 灰度 JPEG：仅墨水屏使用，强制 Luma8。
fn transcode_jpeg_to_gray(
    raw: &[u8],
    profile: EncodeProfile,
    cfg: EinkConfig,
) -> Result<(Vec<u8>, Dims, u8), String> {
    let gray = EncodeProfile {
        grayscale: true,
        quality: profile.quality,
    };
    transcode_to_jpeg(raw, image::ImageFormat::Jpeg, gray, cfg)
}

/// 把整段响应体读入内存（受单图上限约束），仅转码路径使用。
async fn collect_image_body(
    resp: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, Response> {
    if let Some(len) = resp.content_length() {
        if len as usize > max_bytes {
            return Err(error_body(
                StatusCode::PAYLOAD_TOO_LARGE,
                "image_too_large",
                &format!("图片大小 {len} 字节，超过上限 {max_bytes} 字节"),
            ));
        }
    }
    let mut raw = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = raw.next().await {
        match chunk {
            Ok(c) => {
                if buf.len() + c.len() > max_bytes {
                    return Err(error_body(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "image_too_large",
                        &format!("图片超过上限 {max_bytes} 字节，已拒绝转发"),
                    ));
                }
                buf.extend_from_slice(&c);
            }
            Err(e) => {
                return Err(error_body(
                    StatusCode::BAD_GATEWAY,
                    "upstream_error",
                    &format!("读取上游图片失败: {e}"),
                ))
            }
        }
    }
    Ok(buf)
}

/// 图片像素尺寸（服务端解析后随响应头返回，前端无需 object-fit/transform 即可排版）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dims {
    pub width: u32,
    pub height: u32,
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
}

fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn le_i32(b: &[u8], at: usize) -> Option<i32> {
    b.get(at..at + 4)
        .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn le_u24(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 3)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], 0]))
}

/// 从图片头部字节解析宽高：支持 PNG / GIF / JPEG / BMP / WebP；解析不出来返回 None（不报错）。
pub fn parse_dims(buf: &[u8]) -> Option<Dims> {
    // PNG: 89 50 4E 47 0D 0A 1A 0A + IHDR
    if buf.starts_with(b"\x89PNG\r\n\x1a\n") && buf.len() >= 24 && &buf[12..16] == b"IHDR" {
        let width = be_u32(buf, 16)?;
        let height = be_u32(buf, 20)?;
        return Some(Dims { width, height });
    }
    // GIF87a / GIF89a
    if (buf.starts_with(b"GIF87a") || buf.starts_with(b"GIF89a")) && buf.len() >= 10 {
        let width = le_u16(buf, 6)? as u32;
        let height = le_u16(buf, 8)? as u32;
        return Some(Dims { width, height });
    }
    // BMP
    if buf.starts_with(b"BM") && buf.len() >= 26 {
        let width = le_i32(buf, 18)?.unsigned_abs();
        let height = le_i32(buf, 22)?.unsigned_abs();
        return Some(Dims { width, height });
    }
    // WebP: RIFF....WEBP，首个 chunk 可能是 VP8（有损）/ VP8L（无损）/ VP8X（扩展）
    if buf.len() >= 30 && buf.starts_with(b"RIFF") && &buf[8..12] == b"WEBP" {
        match &buf[12..16] {
            b"VP8 " => {
                // payload 偏移 20；3 字节帧标签后是起始码 9D 01 2A，再后面 2+2 字节为宽高
                if &buf[23..26] == [0x9D, 0x01, 0x2A] {
                    let width = le_u16(buf, 26)? as u32 & 0x3FFF;
                    let height = le_u16(buf, 28)? as u32 & 0x3FFF;
                    if width > 0 && height > 0 {
                        return Some(Dims { width, height });
                    }
                }
            }
            b"VP8L" => {
                // payload 偏移 20：0x2F 签名 + 14 位宽-1 + 14 位高-1
                if buf[20] == 0x2F {
                    let bits = le_u32(buf, 21)?;
                    let width = (bits & 0x3FFF) + 1;
                    let height = ((bits >> 14) & 0x3FFF) + 1;
                    return Some(Dims { width, height });
                }
            }
            b"VP8X" => {
                // payload 偏移 20：1 字节标志 + 3 字节保留，随后 24 位宽-1、24 位高-1
                let width = le_u24(buf, 24)? + 1;
                let height = le_u24(buf, 27)? + 1;
                return Some(Dims { width, height });
            }
            _ => {}
        }
    }
    // JPEG: 扫描到 SOFn（C0-C3/C5-C7/C9-CB/CD-CF），段内为 精度(1)+高(2)+宽(2)
    if buf.starts_with(&[0xFF, 0xD8]) {
        let mut pos = 2usize;
        while pos + 4 <= buf.len() {
            if buf[pos] != 0xFF {
                pos += 1;
                continue;
            }
            let marker = buf[pos + 1];
            if marker == 0xFF {
                pos += 1;
                continue;
            }
            if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                pos += 2;
                continue;
            }
            if marker == 0xD9 || marker == 0xDA {
                break; // EOI 或已进入扫描数据，SOF 不会出现在后面
            }
            // JPEG 段长度与 SOF 里的宽高都是大端
            let seg_len = be_u16(buf, pos + 2)? as usize;
            if seg_len < 2 {
                break;
            }
            let is_sof = matches!(
                marker,
                0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
            );
            if is_sof {
                let h = be_u16(buf, pos + 5)? as u32;
                let w = be_u16(buf, pos + 7)? as u32;
                if w > 0 && h > 0 {
                    return Some(Dims { width: w, height: h });
                }
                return None;
            }
            pos = pos.saturating_add(2 + seg_len);
        }
    }
    None
}

fn build_etag_headers(builder: axum::http::response::Builder, etag: &str) -> axum::http::response::Builder {
    builder
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .header(header::ETAG, etag)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            HeaderName::from_static("content-security-policy"),
            "default-src 'none'; sandbox",
        )
}

fn not_modified(etag: &str) -> Response {
    let mut b = Response::builder().status(StatusCode::NOT_MODIFIED);
    b = build_etag_headers(b, etag);
    b.body(Body::empty()).unwrap_or_else(|_| StatusCode::NOT_MODIFIED.into_response())
}

fn matches_if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').any(|s| s.trim() == etag))
        .unwrap_or(false)
}

fn serve_bytes(data: Vec<u8>, ct: &str, etag: &str, hit: bool, headers: &HeaderMap) -> Response {
    if matches_if_none_match(headers, etag) {
        return not_modified(etag);
    }
    let dims = parse_dims(&data);
    let len = data.len();
    let mut b = Response::builder().status(StatusCode::OK);
    b = build_etag_headers(b, etag)
        .header(header::CONTENT_TYPE, ct)
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(
            HeaderName::from_static("x-image-cache"),
            if hit { "HIT" } else { "MISS" },
        )
        .header(HeaderName::from_static("x-image-bytes"), len.to_string());
    if let Some(d) = dims {
        b = b
            .header(HeaderName::from_static("x-image-width"), d.width.to_string())
            .header(HeaderName::from_static("x-image-height"), d.height.to_string());
    }
    b.body(Body::from(data))
        .unwrap_or_else(|_| error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal", "构建响应失败"))
}

/// 转码响应：在通用响应上补 `x-image-transcoded`（路径/宽度/质量）、
/// `x-image-source-bytes` 与 `x-image-transcode-ms`，便于前端与压测量化收益。
#[allow(clippy::too_many_arguments)]
fn serve_transcoded(
    data: Vec<u8>,
    ct: &str,
    etag: &str,
    headers: &HeaderMap,
    label: &str,
    dims: Dims,
    quality: u8,
    source_bytes: usize,
    elapsed: Duration,
) -> Response {
    let mut res = serve_bytes(data, ct, etag, false, headers);
    if let Ok(v) = HeaderValue::from_str(&format!("{label};w={};q={}", dims.width, quality)) {
        res.headers_mut()
            .insert(HeaderName::from_static("x-image-transcoded"), v);
    }
    if let Ok(v) = HeaderValue::from_str(&source_bytes.to_string()) {
        res.headers_mut()
            .insert(HeaderName::from_static("x-image-source-bytes"), v);
    }
    if let Ok(v) = HeaderValue::from_str(&elapsed.as_millis().to_string()) {
        res.headers_mut()
            .insert(HeaderName::from_static("x-image-transcode-ms"), v);
    }
    res
}

/// 单批最多查询的图片数（前端可按 30 张一批，避免一次请求过大）
const MAX_META_BATCH: usize = 30;

/// Content-Type → 解码格式；未知类型返回 `None`（不做切片，走原图直出）。
fn image_format_of(ct: &str) -> Option<image::ImageFormat> {
    let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match base.as_str() {
        "image/webp" => Some(image::ImageFormat::WebP),
        "image/jpeg" | "image/jpg" | "image/pjpeg" => Some(image::ImageFormat::Jpeg),
        "image/png" => Some(image::ImageFormat::Png),
        "image/gif" => Some(image::ImageFormat::Gif),
        "image/bmp" | "image/x-ms-bmp" => Some(image::ImageFormat::Bmp),
        _ => None,
    }
}

/// 切片在缓存里的候选 key：源格式未知时两种（`jpegsrc` / `jpeg`）× 质量档都要试。
fn slice_key_pairs(
    url: &str,
    cfg: &EinkConfig,
    p: &SliceParams,
    i: usize,
    n: usize,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for jpeg_source in [true, false] {
        for qv in candidate_qualities(cfg) {
            let base = variant_key(url, jpeg_source, cfg, qv);
            let key = slice_variant_key(&base, p, i, n);
            let etag = etag_of(&key);
            out.push((key, etag));
        }
    }
    out
}

/// 切片响应：在转码响应上补 `x-image-slices` / `x-image-slice` / `x-image-slice-height`。
fn serve_slice_response(
    out: SliceOutcome,
    etag: &str,
    headers: &HeaderMap,
    label: &str,
    source_bytes: usize,
    elapsed: Duration,
) -> Response {
    let n = out.n;
    let i = out.index;
    let height = out.dims.height;
    let mut res = serve_transcoded(
        out.data,
        "image/jpeg",
        etag,
        headers,
        label,
        out.dims,
        out.quality,
        source_bytes,
        elapsed,
    );
    if let Ok(v) = HeaderValue::from_str(&n.to_string()) {
        res.headers_mut().insert(HeaderName::from_static("x-image-slices"), v);
    }
    if let Ok(v) = HeaderValue::from_str(&i.to_string()) {
        res.headers_mut().insert(HeaderName::from_static("x-image-slice"), v);
    }
    if let Ok(v) = HeaderValue::from_str(&height.to_string()) {
        res.headers_mut()
            .insert(HeaderName::from_static("x-image-slice-height"), v);
    }
    if let Ok(v) = HeaderValue::from_str(&format!("{i}/{n}")) {
        res.headers_mut()
            .insert(HeaderName::from_static("x-image-slice-plan"), v);
    }
    res
}

/// 长条页切片：`slice=1&i=<k>` 的完整路径（缓存 → 回源 → 解码 → 切点 → 裁片 → 编码）。
///
/// 确定性：切点只由「原图 + 切片参数 + 转码宽」决定，计划缓存在 `#{label}-plan` 下；
/// 片 key 带 `i{i}of{n}`，不会出现「缓存 3 片、请求要 2 片」的错位。
#[allow(clippy::too_many_arguments)]
async fn handle_slice(
    source: &BookSource,
    q: &ImageQ,
    headers: &HeaderMap,
    params: SliceParams,
    want: usize,
    cfg: EinkConfig,
    profile: EncodeProfile,
    max_bytes: usize,
) -> Response {
    let plan_key = slice_plan_key(&q.url, &cfg, &params);
    // 1) 计划已知：可以按 i/n 查片缓存，也能做 304
    if let Some(plan) = plan_get(&plan_key) {
        let n = plan.n.max(1);
        let i = want.min(n - 1);
        let keys = slice_key_pairs(&q.url, &cfg, &params, i, n);
        if let Some((data, ct, etag)) = cache_lookup(&keys) {
            let mut res = serve_bytes(data, &ct, &etag, true, headers);
            if let Ok(v) = HeaderValue::from_str(&n.to_string()) {
                res.headers_mut().insert(HeaderName::from_static("x-image-slices"), v);
            }
            if let Ok(v) = HeaderValue::from_str(&i.to_string()) {
                res.headers_mut().insert(HeaderName::from_static("x-image-slice"), v);
            }
            return res;
        }
        if let Some(etag) = matching_key_etag(headers, &keys) {
            return not_modified(etag);
        }
    }

    // 2) 回源
    let hdrs = source_referer(source, q.chapter_url.as_deref(), q.book_url.as_deref());
    let opened = match tokio::time::timeout(IMAGE_TIMEOUT, req::open_stream(&q.url, &hdrs, false)).await
    {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return map_open_error(e),
        Err(_) => {
            return error_body(
                StatusCode::GATEWAY_TIMEOUT,
                "upstream_timeout",
                &format!("图片回源超时（{}s）", IMAGE_TIMEOUT.as_secs()),
            )
        }
    };
    let resp = opened.response;
    let status = resp.status();
    if status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED {
        return error_body(
            StatusCode::FORBIDDEN,
            "upstream_forbidden",
            &format!("源站返回 {status}（可能需要登录、Referer 或防盗链）"),
        );
    }
    if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
        return error_body(
            StatusCode::NOT_FOUND,
            "upstream_not_found",
            &format!("源站返回 {status}，图片不存在"),
        );
    }
    if !status.is_success() {
        return error_body(
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            &format!("源站返回 {status}"),
        );
    }
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let ct_base = ct
        .split(';')
        .next()
        .unwrap_or("image/*")
        .trim()
        .to_string();
    // PNG/GIF/BMP 等非 JPEG/WebP 源：image 依赖没有启用这些解码器，整图路径是原样直出。
    // 切片路径必须同样原样直出，否则 `slice=1` 会对普通图报 502，违反「普通图字节不变」。
    if !matches!(
        image_format_of(&ct),
        Some(image::ImageFormat::Jpeg) | Some(image::ImageFormat::WebP)
    ) {
        let raw = match collect_image_body(resp, max_bytes).await {
            Ok(v) => v,
            Err(r) => return r,
        };
        let orig_key = format!("{}#orig", q.url);
        let etag = etag_of(&orig_key);
        if raw.len() <= CACHE_ITEM_MAX {
            image_cache::put(&orig_key, raw.clone(), ct_base.clone());
        }
        return serve_bytes(raw, &ct_base, &etag, false, headers);
    }
    let Some(format) = image_format_of(&ct) else {
        return error_body(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "slice_unsupported",
            &format!("切片不支持的图片类型：{}", if ct.is_empty() { "未知" } else { &ct }),
        );
    };
    if let Some(len) = resp.content_length() {
        if len as usize > max_bytes {
            return error_body(
                StatusCode::PAYLOAD_TOO_LARGE,
                "image_too_large",
                &format!("图片大小 {len} 字节，超过上限 {max_bytes} 字节"),
            );
        }
    }
    let raw = match collect_image_body(resp, max_bytes).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let source_bytes = raw.len();
    let started = Instant::now();
    let jpeg_source = format == image::ImageFormat::Jpeg;
    let decoded = tokio::task::spawn_blocking(move || {
        transcode_slice(&raw, format, profile, cfg, &params, want)
    })
    .await;
    let out = match decoded {
        Ok(Ok(v)) => v,
        Ok(Err(msg)) => {
            return error_body(
                StatusCode::BAD_GATEWAY,
                "image_transcode_failed",
                &format!("切片转码失败：{msg}"),
            )
        }
        Err(e) => {
            return error_body(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                &format!("转码任务异常：{e}"),
            )
        }
    };

    // 3) 落缓存：不切（n=1）时写整图 key，`slice=0` 的后续请求直接复用同一份字节
    let etag = if out.n <= 1 {
        let key = variant_key(&q.url, jpeg_source, &cfg, out.quality);
        if out.data.len() <= CACHE_ITEM_MAX {
            image_cache::put(&key, out.data.clone(), "image/jpeg".to_string());
        }
        etag_of(&key)
    } else {
        if let Some(plan) = out.plan.as_ref() {
            plan_put(&plan_key, plan);
        }
        let base = variant_key(&q.url, jpeg_source, &cfg, out.quality);
        let key = slice_variant_key(&base, &params, out.index, out.n);
        if out.data.len() <= CACHE_ITEM_MAX {
            image_cache::put(&key, out.data.clone(), "image/jpeg".to_string());
        }
        etag_of(&key)
    };
    let label = if out.n <= 1 { "slice:whole-gray" } else { "slice:gray" };
    serve_slice_response(out, &etag, headers, label, source_bytes, started.elapsed())
}


fn err_item(url: &str, code: &str, message: &str) -> serde_json::Value {
    json!({
        "url": url,
        "ok": false,
        "supported": false,
        "error": code,
        "message": message,
    })
}

/// 单个 URL 的元数据：只预读图片头部若干字节解析宽高，不下载整图。
pub(crate) async fn fetch_image_meta(
    url: String,
    hdrs: HashMap<String, String>,
    max_bytes: usize,
) -> serde_json::Value {
    let parsed = match url::Url::parse(&url) {
        Ok(u) => u,
        Err(e) => return err_item(&url, "invalid_url", &format!("URL 解析失败: {e}")),
    };
    if let Err(e) = req::ensure_public_url(&parsed, false).await {
        let m = e.to_string();
        let code = if m.contains("私网") || m.contains("回环") || m.contains("链路本地") {
            "private_address"
        } else {
            "invalid_url"
        };
        return err_item(&url, code, &m);
    }
    let opened = match tokio::time::timeout(IMAGE_TIMEOUT, req::open_stream(&url, &hdrs, false)).await
    {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return err_item(&url, "upstream_error", &e.to_string()),
        Err(_) => return err_item(&url, "upstream_timeout", "图片回源超时"),
    };
    let resp = opened.response;
    let status = resp.status();
    if status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED {
        return err_item(&url, "upstream_forbidden", &format!("源站返回 {status}"));
    }
    if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
        return err_item(&url, "upstream_not_found", &format!("源站返回 {status}"));
    }
    if !status.is_success() {
        return err_item(&url, "upstream_error", &format!("源站返回 {status}"));
    }
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !is_image_content_type(&ct) {
        return err_item(&url, "not_an_image", &format!("源站返回非图片内容：{ct}"));
    }
    if let Some(len) = resp.content_length() {
        if len as usize > max_bytes {
            return err_item(&url, "image_too_large", &format!("图片大小 {len} 字节超过上限"));
        }
    }
    let declared = resp.content_length();
    let want = declared
        .map(|n| n as usize)
        .unwrap_or(PEEK_MAX)
        .min(PEEK_MAX);
    let mut raw = resp.bytes_stream();
    let mut buf = Vec::new();
    let mut ended = false;
    while buf.len() < want {
        match raw.next().await {
            Some(Ok(chunk)) => {
                buf.extend_from_slice(&chunk);
                if buf.len() >= want || parse_dims(&buf).is_some() {
                    break;
                }
            }
            Some(Err(e)) => return err_item(&url, "upstream_error", &format!("读取上游失败: {e}")),
            None => {
                ended = true;
                break;
            }
        }
    }
    let dims = parse_dims(&buf);
    let bytes = declared.or(if ended { Some(buf.len() as u64) } else { None });
    let ct_base = ct.split(';').next().unwrap_or("").trim().to_string();
    let mut v = json!({
        "url": url,
        "ok": true,
        "supported": true,
        "content_type": ct_base,
        "bytes": bytes,
    });
    if let Some(d) = dims {
        v["width"] = json!(d.width);
        v["height"] = json!(d.height);
        // 切片预判（只按窗口算，不读像素）：`slices=0` 表示整图不切
        let p = SliceParams::default();
        let n = slice_count_window(d.width, d.height, &p);
        v["slices"] = json!(if n > 1 { n } else { 0 });
        v["slice_h"] = json!(slice_display_heights(d.width, d.height, &p));
    }
    v
}

/// `POST /api/book/images/meta`：一次返回整批图片的宽高/字节数，避免每图一次额外请求。
async fn images_meta(
    State(s): State<AppState>,
    _u: AuthUser,
    Json(req): Json<ImageMetaReq>,
) -> Response {
    let source_id = req
        .source
        .as_deref()
        .or(req.book_url.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(source_id) = source_id else {
        return error_body(
            StatusCode::BAD_REQUEST,
            "missing_source",
            "缺少 book_url（书源 id）",
        );
    };
    let source = match load_source(&s.shared, source_id).await {
        Ok(v) => v,
        Err(_) => {
            return error_body(
                StatusCode::NOT_FOUND,
                "source_not_found",
                "书源不存在，无法确定 Referer/UA/Cookie",
            )
        }
    };
    if req.urls.is_empty() {
        return Json(json!({"count": 0, "items": []})).into_response();
    }
    if req.urls.len() > MAX_META_BATCH {
        return error_body(
            StatusCode::BAD_REQUEST,
            "batch_too_large",
            &format!("单批最多 {MAX_META_BATCH} 个 URL"),
        );
    }
    let hdrs = source_referer(&source, req.chapter_url.as_deref(), req.book_url.as_deref());
    let max_bytes = s.cfg.image_max_body_bytes.max(1024);
    let items: Vec<serde_json::Value> = futures::stream::iter(
        req.urls
            .into_iter()
            .map(|u| fetch_image_meta(u, hdrs.clone(), max_bytes)),
    )
    .buffer_unordered(4)
    .collect()
    .await;
    Json(json!({"count": items.len(), "items": items})).into_response()
}

pub(crate) fn source_referer(source: &BookSource, chapter_url: Option<&str>, book_url: Option<&str>) -> HashMap<String, String> {
    // 与正文请求一致：支持 JSON / 多行 / `@js:` 三种 header 形式
    let mut h = req::source_headers(source, &source.book_source_url);
    let referer = chapter_url
        .map(str::trim)
        .filter(|s| !s.is_empty() && (s.starts_with("http://") || s.starts_with("https://")))
        .or_else(|| {
            book_url
                .map(str::trim)
                .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
        });
    if let Some(r) = referer {
        h.insert("Referer".to_string(), r.to_string());
    }
    // 墨水屏只认 JPEG/PNG/GIF：显式声明偏好，部分 CDN 会据此返回 JPEG 而不是 WebP
    if !h.keys().any(|k| k.eq_ignore_ascii_case("accept")) {
        h.insert(
            "Accept".to_string(),
            "image/jpeg,image/png,image/gif,image/*;q=0.8,*/*;q=0.1".to_string(),
        );
    }
    h
}

/// 进程内图片尺寸缓存：`/k` 漫画一屏一页要一次拿到整章尺寸，同章重复打开不应重复回源。
/// 只存「探测成功」的结果；失败不缓存，下次仍可重试。
static DIM_CACHE: Lazy<Mutex<HashMap<String, (u32, u32)>>> = Lazy::new(|| Mutex::new(HashMap::new()));
/// 尺寸缓存上限（条目数），超出直接清空（漫画章节数量有限，不做 LRU）
const DIM_CACHE_MAX: usize = 2000;

/// 批量探测原始图片宽高：只预读图片头，不下载整图；返回与 `urls` 等长、同序的结果。
/// 单批并发 8，命中进程内缓存的不再回源。
pub(crate) async fn probe_dims(
    source: &BookSource,
    chapter_url: Option<&str>,
    book_url: Option<&str>,
    urls: &[String],
    max_bytes: usize,
) -> Vec<Option<(u32, u32)>> {
    if urls.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Option<(u32, u32)>> = {
        let m = DIM_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        urls.iter().map(|u| m.get(u).copied()).collect()
    };
    let missing: Vec<(usize, String)> = out
        .iter()
        .enumerate()
        .filter(|(_, d)| d.is_none())
        .map(|(i, _)| (i, urls[i].clone()))
        .collect();
    if missing.is_empty() {
        return out;
    }
    let hdrs = source_referer(source, chapter_url, book_url);
    let fetched: Vec<(usize, Option<(u32, u32)>)> = futures::stream::iter(missing)
        .map(|(i, u)| {
            let h = hdrs.clone();
            async move {
                let v = fetch_image_meta(u, h, max_bytes).await;
                let d = v
                    .get("width")
                    .and_then(|x| x.as_u64())
                    .zip(v.get("height").and_then(|x| x.as_u64()))
                    .map(|(w, h)| (w as u32, h as u32))
                    .filter(|(w, h)| *w > 0 && *h > 0);
                (i, d)
            }
        })
        .buffer_unordered(8)
        .collect()
        .await;
    let mut cache = DIM_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    for (i, d) in fetched {
        if let Some(dv) = d {
            if cache.len() >= DIM_CACHE_MAX {
                cache.clear();
            }
            cache.insert(urls[i].clone(), dv);
        }
        out[i] = d;
    }
    out
}

fn map_open_error(e: anyhow::Error) -> Response {
    let m = e.to_string();
    if m.contains("私网") || m.contains("回环") || m.contains("仅允许") || m.contains("链路本地") {
        return error_body(
            StatusCode::FORBIDDEN,
            "private_address",
            &format!("拒绝访问私网地址：{m}"),
        );
    }
    if m.contains("URL 解析失败") || m.contains("非法") {
        return error_body(StatusCode::BAD_REQUEST, "invalid_url", &m);
    }
    if m.contains("超时") || m.contains("timeout") {
        return error_body(StatusCode::GATEWAY_TIMEOUT, "upstream_timeout", &m);
    }
    error_body(StatusCode::BAD_GATEWAY, "upstream_error", &m)
}

async fn proxy(
    State(s): State<AppState>,
    _u: AuthUser,
    headers: HeaderMap,
    Query(q): Query<ImageQ>,
) -> Response {
    let max_bytes = s.cfg.image_max_body_bytes.max(1024);
    let source_id = q
        .source
        .as_deref()
        .or(q.book_url.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(source_id) = source_id else {
        return error_body(
            StatusCode::BAD_REQUEST,
            "missing_source",
            "缺少 book_url（书源 id）",
        );
    };
    let source = match load_source(&s.shared, source_id).await {
        Ok(v) => v,
        Err(_) => {
            return error_body(
                StatusCode::NOT_FOUND,
                "source_not_found",
                "书源不存在，无法确定 Referer/UA/Cookie",
            )
        }
    };
    // 先做 SSRF 预检，私网地址在回源前就拒绝（避免把内网请求发出去）
    match url::Url::parse(&q.url) {
        Ok(u) => {
            if let Err(e) = req::ensure_public_url(&u, false).await {
                return map_open_error(e);
            }
        }
        Err(e) => {
            return error_body(
                StatusCode::BAD_REQUEST,
                "invalid_url",
                &format!("URL 解析失败: {e}"),
            )
        }
    }
    let transcode = wants_webp_transcode(&headers);
    let eink = is_eink_client(&headers, q.eink.as_deref());
    let cfg = eink_config();
    let profile = EncodeProfile::eink();
    // 长条页切片：只有「墨水屏 + 切片开关开启 + slice=1」才走；开关关闭、缺省或
    // `slice=0` 时完全沿用整图路径，缓存 key 与字节输出都不变（桌面/手机不受影响）。
    if eink && s.cfg.comic_slice && slice_requested(q.slice.as_deref()) {
        let params = SliceParams::from_config(&s.cfg, q.dw, q.vh, q.ov);
        return handle_slice(
            &source,
            &q,
            &headers,
            params,
            q.i.unwrap_or(0),
            cfg,
            profile,
            max_bytes,
        )
        .await;
    }
    // 两条转码路径的缓存 key 分开：WebP 源用 `#jpeg-…`，JPEG 源用 `#jpegsrc-…`，
    // 宽度/质量/编码器/自适应标记全部编进后缀，旧档不会串味。
    let webp_keys = key_pairs(&q.url, false, &cfg);
    let jpeg_keys = key_pairs(&q.url, true, &cfg);
    let orig_key = format!("{}#orig", q.url);
    let orig_etag = etag_of(&orig_key);
    // 缓存命中：按变体查。墨水屏优先「JPEG 源灰度」，其次「WebP 转码」；
    // 非 WebP/JPEG 的原始格式（PNG/GIF）墨水屏可直接复用原图。
    // 普通客户端要么复用原图，要么（不认 WebP 时）复用 WebP 转码变体。
    if eink {
        if let Some((data, ct, etag)) = cache_lookup(&jpeg_keys) {
            return serve_bytes(data, &ct, &etag, true, &headers);
        }
        if let Some((data, ct, etag)) = cache_lookup(&webp_keys) {
            return serve_bytes(data, &ct, &etag, true, &headers);
        }
        if let Some((data, ct)) = image_cache::get(&orig_key) {
            if !is_webp_content_type(&ct) && !is_jpeg_content_type(&ct) {
                return serve_bytes(data, &ct, &orig_etag, true, &headers);
            }
        }
    } else if transcode {
        if let Some((data, ct, etag)) = cache_lookup(&webp_keys) {
            return serve_bytes(data, &ct, &etag, true, &headers);
        }
        if let Some((data, ct)) = image_cache::get(&orig_key) {
            if !is_webp_content_type(&ct) {
                return serve_bytes(data, &ct, &orig_etag, true, &headers);
            }
        }
    } else if let Some((data, ct)) = image_cache::get(&orig_key) {
        return serve_bytes(data, &ct, &orig_etag, true, &headers);
    }

    let hdrs = source_referer(&source, q.chapter_url.as_deref(), q.book_url.as_deref());
    let opened = match tokio::time::timeout(IMAGE_TIMEOUT, req::open_stream(&q.url, &hdrs, false)).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return map_open_error(e),
        Err(_) => {
            return error_body(
                StatusCode::GATEWAY_TIMEOUT,
                "upstream_timeout",
                &format!("图片回源超时（{}s）", IMAGE_TIMEOUT.as_secs()),
            )
        }
    };
    let resp = opened.response;
    let status = resp.status();
    if status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED {
        return error_body(
            StatusCode::FORBIDDEN,
            "upstream_forbidden",
            &format!("源站返回 {status}（可能需要登录、Referer 或防盗链）"),
        );
    }
    if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
        return error_body(
            StatusCode::NOT_FOUND,
            "upstream_not_found",
            &format!("源站返回 {status}，图片不存在"),
        );
    }
    if !status.is_success() {
        return error_body(
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            &format!("源站返回 {status}"),
        );
    }
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !is_image_content_type(&ct) {
        return error_body(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "not_an_image",
            &format!("源站返回非图片内容：{}", if ct.is_empty() { "未知" } else { &ct }),
        );
    }
    if let Some(len) = resp.content_length() {
        if len as usize > max_bytes {
            return error_body(
                StatusCode::PAYLOAD_TOO_LARGE,
                "image_too_large",
                &format!("图片大小 {len} 字节，超过上限 {max_bytes} 字节"),
            );
        }
    }
    let upstream_len = resp.content_length();

    let ct_base = ct
        .split(';')
        .next()
        .unwrap_or("image/*")
        .trim()
        .to_string();

    // 墨水屏的变体 ETag 与普通 WebP 降级档的 ETag 都可能在客户端手里：
    // 命中任意一个就直接 304，省掉回源与解码。
    let variant_keys: Vec<(String, String)> = if eink {
        jpeg_keys.iter().chain(webp_keys.iter()).cloned().collect()
    } else if transcode {
        webp_keys.clone()
    } else {
        Vec::new()
    };
    if let Some(etag) = matching_key_etag(&headers, &variant_keys) {
        return not_modified(etag);
    }

    // WebP 且客户端不认 WebP：整图读入内存 → 纯 Rust 解码 → 缩放 → JPEG，不落盘。
    // 源站无视 Accept 一律返回 WebP 的情况（如 content.mkzcdn.com）只能走这条路。
    if is_webp_content_type(&ct_base) && (transcode || eink) {
        let raw = match collect_image_body(resp, max_bytes).await {
            Ok(v) => v,
            Err(r) => return r,
        };
        let source_bytes = raw.len();
        let started = Instant::now();
        let decoded = tokio::task::spawn_blocking(move || {
            transcode_to_jpeg(&raw, image::ImageFormat::WebP, profile, cfg)
        })
        .await;
        let (out, dims, quality) = match decoded {
            Ok(Ok(v)) => v,
            Ok(Err(msg)) => {
                return error_body(
                    StatusCode::BAD_GATEWAY,
                    "image_transcode_failed",
                    &format!("WebP 转码失败：{msg}"),
                )
            }
            Err(e) => {
                return error_body(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    &format!("转码任务异常：{e}"),
                )
            }
        };
        let key = variant_key(&q.url, false, &cfg, quality);
        let etag = etag_of(&key);
        if out.len() <= CACHE_ITEM_MAX {
            image_cache::put(&key, out.clone(), "image/jpeg".to_string());
        }
        let label = format!("webp->jpeg-{}", if profile.grayscale { "gray" } else { "color" });
        return serve_transcoded(
            out,
            "image/jpeg",
            &etag,
            &headers,
            &label,
            dims,
            quality,
            source_bytes,
            started.elapsed(),
        );
    }

    // JPEG 源 + 墨水屏：解码 → Luma8 → （可选 Lanczos/USM）→ 重编码灰度 JPEG。
    // 普通浏览器不进这个分支，仍然原图直出。
    if is_jpeg_content_type(&ct_base) && eink {
        let raw = match collect_image_body(resp, max_bytes).await {
            Ok(v) => v,
            Err(r) => return r,
        };
        let source_bytes = raw.len();
        let started = Instant::now();
        let decoded = tokio::task::spawn_blocking(move || match transcode_jpeg_to_gray(
            &raw, profile, cfg,
        ) {
            Ok(v) => Ok(v),
            Err(msg) => Err((msg, raw)),
        })
        .await;
        let (out, dims, quality) = match decoded {
            Ok(Ok(v)) => v,
            Ok(Err((msg, raw))) => {
                // 单图转码失败不影响整章：降级为原图直出，前端仍能看到这一页。
                let mut res = serve_bytes(raw, &ct_base, &orig_etag, false, &headers);
                if let Ok(v) = HeaderValue::from_str(&format!("jpeg->gray-failed;reason={msg}")) {
                    res.headers_mut()
                        .insert(HeaderName::from_static("x-image-transcoded"), v);
                }
                return res;
            }
            Err(e) => {
                return error_body(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    &format!("转码任务异常：{e}"),
                )
            }
        };
        let key = variant_key(&q.url, true, &cfg, quality);
        let etag = etag_of(&key);
        if out.len() <= CACHE_ITEM_MAX {
            image_cache::put(&key, out.clone(), "image/jpeg".to_string());
        }
        return serve_transcoded(
            out,
            "image/jpeg",
            &etag,
            &headers,
            "jpeg->gray",
            dims,
            quality,
            source_bytes,
            started.elapsed(),
        );
    }

    if matches_if_none_match(&headers, &orig_etag) {
        return not_modified(&orig_etag);
    }

    // 流式转发前预读一小段用于解析宽高；已读块会原样拼回响应流，不额外缓冲整图。
    let mut raw = resp.bytes_stream();
    let mut head: Vec<u8> = Vec::new();
    let mut dims: Option<Dims> = None;
    let mut ended = false;
    while head.len() < PEEK_MAX {
        match raw.next().await {
            Some(Ok(chunk)) => {
                head.extend_from_slice(&chunk);
                if head.len() > max_bytes {
                    return error_body(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "image_too_large",
                        &format!("图片超过上限 {max_bytes} 字节，已拒绝转发"),
                    );
                }
                if let Some(d) = parse_dims(&head) {
                    dims = Some(d);
                    break;
                }
            }
            Some(Err(e)) => {
                return error_body(
                    StatusCode::BAD_GATEWAY,
                    "upstream_error",
                    &format!("读取上游图片失败: {e}"),
                )
            }
            None => {
                ended = true;
                break;
            }
        }
    }
    let known_len = upstream_len.or(if ended {
        Some(head.len() as u64)
    } else {
        None
    });
    let upstream: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>> = if head.is_empty() {
        Box::pin(raw)
    } else {
        Box::pin(
            futures::stream::iter(vec![Ok::<Bytes, reqwest::Error>(Bytes::from(head))]).chain(raw),
        )
    };
    let key = orig_key.clone();
    let ct_for_cache = ct_base.clone();
    // 上游给了 Content-Length 时，hyper 读完正好这么多字节就结束，不会再 poll 到 `None`，
    // 导致原本只在流结束才写入的 LRU 永远为空。这里在累计字节数到达已知长度时也落缓存。
    let cache_len = known_len;
    let stream = futures::stream::unfold(
        (upstream, Vec::<u8>::new(), 0usize, true),
        move |(mut up, mut buf, mut total, mut keep)| {
            let key = key.clone();
            let ct = ct_for_cache.clone();
            async move {
                match up.next().await {
                    None => {
                        if keep && !buf.is_empty() {
                            image_cache::put(&key, std::mem::take(&mut buf), ct);
                        }
                        None
                    }
                    Some(Ok(chunk)) => {
                        total += chunk.len();
                        if total > max_bytes {
                            // 长度未知时超限：立即截断，不再继续读取上游
                            return None;
                        }
                        if keep && buf.len() + chunk.len() <= CACHE_ITEM_MAX {
                            buf.extend_from_slice(&chunk);
                            if keep && cache_len == Some(total as u64) {
                                image_cache::put(&key, std::mem::take(&mut buf), ct);
                                keep = false;
                            }
                        } else {
                            keep = false;
                            buf.clear();
                        }
                        Some((Ok(chunk), (up, buf, total, keep)))
                    }
                    Some(Err(e)) => Some((
                        Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("读取上游图片失败: {e}"),
                        )),
                        (up, buf, total, keep),
                    )),
                }
            }
        },
    );

    let mut b = Response::builder().status(StatusCode::OK);
    b = build_etag_headers(b, &orig_etag)
        .header(header::CONTENT_TYPE, ct_base)
        .header(HeaderName::from_static("x-image-cache"), "MISS")
        .header(
            HeaderName::from_static("x-image-max-bytes"),
            max_bytes.to_string(),
        );
    if let Some(len) = known_len {
        b = b
            .header(header::CONTENT_LENGTH, len.to_string())
            .header(HeaderName::from_static("x-image-bytes"), len.to_string());
    }
    if let Some(d) = dims {
        b = b
            .header(HeaderName::from_static("x-image-width"), d.width.to_string())
            .header(HeaderName::from_static("x-image-height"), d.height.to_string());
    }
    b.body(Body::from_stream(stream))
        .unwrap_or_else(|_| error_body(StatusCode::INTERNAL_SERVER_ERROR, "internal", "构建响应失败"))
}

/// 只允许图片响应；SVG 等文本型图片由 CSP sandbox + nosniff 兜底。
pub fn allowed_content_type(ct: &str) -> bool {
    is_image_content_type(ct)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_content_type_filter() {
        assert!(allowed_content_type("image/jpeg"));
        assert!(allowed_content_type("IMAGE/PNG; charset=binary"));
        assert!(allowed_content_type("image/webp"));
        assert!(!allowed_content_type("text/html"));
        assert!(!allowed_content_type("application/json"));
        assert!(!allowed_content_type(""));
    }

    #[test]
    fn private_hosts_rejected() {
        assert!(req::is_private_host("127.0.0.1"));
        assert!(req::is_private_host("localhost"));
        assert!(req::is_private_host("10.0.0.1"));
        assert!(req::is_private_host("192.168.1.2"));
        assert!(req::is_private_host("169.254.169.254"));
        assert!(!req::is_private_host("example.com"));
    }

    fn make_webp(w: u32, h: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([(x % 251) as u8, (y % 251) as u8, ((x + y) % 251) as u8]);
        }
        let mut buf: Vec<u8> = Vec::new();
        image::ImageEncoder::write_image(
            image::codecs::webp::WebPEncoder::new_lossless(&mut buf),
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .expect("encode webp");
        buf
    }

    #[test]
    fn webp_dims_parsed_from_vp8l_and_vp8x() {
        // 真实编码器产出的 VP8L
        let webp = make_webp(1500, 2573);
        assert_eq!(parse_dims(&webp), Some(Dims { width: 1500, height: 2573 }));

        // 合成 VP8L 头
        let mut vp8l = b"RIFF\0\0\0\0WEBPVP8L".to_vec();
        vp8l.resize(20, 0);
        vp8l.push(0x2F);
        let bits: u32 = (799 - 1) | ((1199 - 1) << 14);
        vp8l.extend_from_slice(&bits.to_le_bytes());
        vp8l.resize(30, 0);
        assert_eq!(parse_dims(&vp8l), Some(Dims { width: 799, height: 1199 }));

        // 合成 VP8X 头（宽高以 24 位小端存 width-1 / height-1）
        let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        vp8x.resize(24, 0);
        vp8x.extend_from_slice(&(799u32).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(1199u32).to_le_bytes()[..3]);
        assert_eq!(parse_dims(&vp8x), Some(Dims { width: 800, height: 1200 }));
    }

    fn gray_profile(quality: u8) -> EncodeProfile {
        EncodeProfile { grayscale: true, quality }
    }

    fn color_profile(quality: u8) -> EncodeProfile {
        EncodeProfile { grayscale: false, quality }
    }

    /// 统计 JPEG SOF 段里的分量数：1 = 灰度，3 = RGB。
    fn jpeg_components(jpeg: &[u8]) -> Option<u8> {
        let mut pos = 2usize;
        while pos + 4 <= jpeg.len() {
            if jpeg[pos] != 0xFF {
                return None;
            }
            let marker = jpeg[pos + 1];
            if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
                pos += 2;
                continue;
            }
            let len = u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]) as usize;
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                return jpeg.get(pos + 9).copied();
            }
            pos += 2 + len;
        }
        None
    }

    #[test]
    fn webp_transcodes_to_width_capped_jpeg() {
        let webp = make_webp(1500, 2400);
        let (jpeg, dims) = transcode_webp_to_jpeg(&webp, gray_profile(78)).expect("transcode");
        assert_eq!(dims, Dims { width: 1200, height: 1920 });
        assert!(jpeg.starts_with(&[0xFF, 0xD8]), "输出应为 JPEG");
        // JPEG 头部里的 SOF 宽高也应与转码结果一致
        assert_eq!(parse_dims(&jpeg), Some(Dims { width: 1200, height: 1920 }));
        // 墨水屏档必须是单分量灰度 JPEG
        assert_eq!(jpeg_components(&jpeg), Some(1));
    }

    #[test]
    fn small_webp_keeps_dimensions_and_corrupt_is_rejected() {
        let webp = make_webp(600, 900);
        let (jpeg, dims) = transcode_webp_to_jpeg(&webp, gray_profile(78)).expect("transcode");
        assert_eq!(dims, Dims { width: 600, height: 900 });
        assert!(!jpeg.is_empty());
        assert!(transcode_webp_to_jpeg(b"not a webp", gray_profile(78)).is_err());
    }

    #[test]
    fn eink_grayscale_keeps_reasonable_tones() {
        let webp = make_webp(800, 1200);
        let (jpeg, dims) = transcode_webp_to_jpeg(&webp, gray_profile(78)).expect("transcode");
        assert_eq!(dims, Dims { width: 800, height: 1200 });
        assert_eq!(jpeg_components(&jpeg), Some(1));

        // 解码回来统计亮度：既不能是全黑/全白，也不能反相
        let decoded =
            image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
        let luma = decoded.to_luma8();
        let (mut min, mut max, mut sum) = (255u8, 0u8, 0u64);
        for p in luma.pixels() {
            let v = p.0[0];
            min = min.min(v);
            max = max.max(v);
            sum += v as u64;
        }
        let n = u64::from(luma.width()) * u64::from(luma.height());
        let mean = sum / n;
        assert!(min < 60, "最小值 {min} 说明可能全黑");
        assert!(max > 200, "最大值 {max} 说明可能全黑或被压扁");
        assert!((60..=200).contains(&mean), "平均亮度 {mean} 应居中，反相/全黑都不满足");
    }

    #[test]
    fn color_profile_still_three_components() {
        let (jpeg, _) = transcode_webp_to_jpeg(&make_webp(300, 400), color_profile(82))
            .expect("transcode");
        assert_eq!(jpeg_components(&jpeg), Some(3));
    }

    #[test]
    fn eink_quality_env_parsing_clamps_to_60_90() {
        assert_eq!(parse_eink_jpeg_quality("78"), Some(78));
        assert_eq!(parse_eink_jpeg_quality(" 85 "), Some(85));
        assert_eq!(parse_eink_jpeg_quality("40"), Some(60));
        assert_eq!(parse_eink_jpeg_quality("95"), Some(90));
        assert_eq!(parse_eink_jpeg_quality("abc"), None);
    }

    #[test]
    fn transcode_decision_follows_client_capability() {
        fn headers(ua: Option<&str>, accept: Option<&str>) -> HeaderMap {
            let mut h = HeaderMap::new();
            if let Some(ua) = ua {
                h.insert(header::USER_AGENT, HeaderValue::from_str(ua).unwrap());
            }
            if let Some(a) = accept {
                h.insert(header::ACCEPT, HeaderValue::from_str(a).unwrap());
            }
            h
        }
        let kindle = "Mozilla/5.0 (Linux; U; Android 2.3.4) AppleWebKit/531.2+ (KHTML, like Gecko) Version/5.0 Safari/531.2 Kindle/3.0+";
        let silk = "Mozilla/5.0 (Linux; Android 4.4; KFAPWI Build/KTU84P) AppleWebKit/537.36 Silk/3.68 like Chrome/34.0";
        let desktop = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36";

        // Kindle/Silk 一律转，即使 Accept 声明支持 webp
        assert!(wants_webp_transcode(&headers(Some(kindle), Some("image/webp,image/*"))));
        assert!(wants_webp_transcode(&headers(Some(silk), Some("image/webp"))));
        // 桌面 UA + Accept 含 webp：原样透传
        assert!(!wants_webp_transcode(&headers(Some(desktop), Some("image/avif,image/webp,*/*"))));
        // 现代 UA 但 Accept 明确不含 webp：转
        assert!(wants_webp_transcode(&headers(Some(desktop), Some("image/jpeg,image/png"))));
        // 无 Accept 头按现代客户端处理
        assert!(!wants_webp_transcode(&headers(Some(desktop), None)));
    }

    #[test]
    fn cache_variants_do_not_collide() {
        let u = "https://img.example/a.jpg";
        let base = EinkConfig::default();
        let webp_key = variant_key(u, false, &base, 78);
        let jpeg_key = variant_key(u, true, &base, 78);
        let orig = format!("{u}#orig");
        assert_ne!(webp_key, jpeg_key, "WebP 转码与 JPEG 源灰度不能共用 key");
        assert_ne!(jpeg_key, orig, "转码档与原图不能共用 key");
        assert_ne!(etag_of(&webp_key), etag_of(&jpeg_key));

        let wide = EinkConfig { width: 1440, ..base };
        let adaptive = EinkConfig { adaptive: true, ..base };
        assert_ne!(
            variant_key(u, true, &base, 70),
            variant_key(u, true, &wide, 70),
            "宽度必须进 key"
        );
        assert_ne!(
            variant_key(u, true, &base, 70),
            variant_key(u, true, &base, 85),
            "质量必须进 key"
        );
        assert_ne!(
            variant_key(u, true, &base, 70),
            variant_key(u, true, &adaptive, 70),
            "自适应标记必须进 key"
        );
    }

    #[test]
    fn adaptive_lookup_covers_both_qualities() {
        let cfg = EinkConfig {
            adaptive: true,
            ..EinkConfig::default()
        };
        let pairs = key_pairs("https://img.example/a.jpg", true, &cfg);
        assert_eq!(pairs.len(), 2, "自适应要同时查文字档与画面档");
        assert!(pairs.iter().any(|(k, _)| k.contains("-q85-")));
        assert!(pairs.iter().any(|(k, _)| k.contains("-q70-")));
        assert!(pairs
            .iter()
            .all(|(k, _)| k.starts_with("https://img.example/a.jpg#jpegsrc-w1200-")));
    }

    fn make_jpeg(w: u32, h: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([(x % 251) as u8, (y % 251) as u8, ((x + y) % 251) as u8]);
        }
        let mut buf: Vec<u8> = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 92)
            .encode_image(&img)
            .expect("encode jpeg");
        buf
    }

    #[test]
    fn jpeg_source_transcodes_to_width_capped_gray() {
        let jpeg = make_jpeg(1500, 2400);
        assert_eq!(jpeg_components(&jpeg), Some(3));
        let cfg = EinkConfig::default();
        let (out, dims, quality) =
            transcode_jpeg_to_gray(&jpeg, gray_profile(EINK_JPEG_QUALITY_DEFAULT), cfg)
                .expect("transcode");
        assert_eq!(dims, Dims { width: 1200, height: 1920 });
        assert_eq!(quality, EINK_JPEG_QUALITY_DEFAULT);
        assert_eq!(jpeg_components(&out), Some(1), "墨水屏输出必须是单分量灰度");
        assert_eq!(parse_dims(&out), Some(dims));
        assert!(transcode_jpeg_to_gray(b"not a jpeg", gray_profile(70), cfg).is_err());
    }

    #[test]
    fn jpeg_source_with_env_width_is_honored() {
        let cfg = EinkConfig {
            width: 1440,
            ..EinkConfig::default()
        };
        let (out, dims, _) =
            transcode_jpeg_to_gray(&make_jpeg(2000, 3000), gray_profile(70), cfg).expect("transcode");
        assert_eq!(dims, Dims { width: 1440, height: 2160 });
        assert_eq!(parse_dims(&out), Some(dims));
    }

    #[test]
    fn eink_detection_covers_ua_header_and_query() {
        fn h(ua: Option<&str>) -> HeaderMap {
            let mut h = HeaderMap::new();
            if let Some(ua) = ua {
                h.insert(header::USER_AGENT, HeaderValue::from_str(ua).unwrap());
            }
            h
        }
        let kindle_ua = "Mozilla/5.0 (Linux; U; Android 2.3.4) AppleWebKit/531.2+ (KHTML, like Gecko) Version/5.0 Safari/531.2 Kindle/3.0+";
        let desktop_ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36";

        let desktop = h(Some(desktop_ua));
        assert!(!is_eink_client(&desktop, None), "普通浏览器默认不转灰度");
        assert!(!is_eink_client(&desktop, Some("0")));
        assert!(is_eink_client(&desktop, Some("1")));
        assert!(is_eink_client(&desktop, Some("eink")));

        let mut mode = h(Some(desktop_ua));
        mode.insert(
            HeaderName::from_static("x-device-mode"),
            HeaderValue::from_static("eink"),
        );
        assert!(is_eink_client(&mode, None), "X-Device-Mode: eink 等价于 ?eink=1");

        assert!(is_eink_client(&h(Some(kindle_ua)), None), "Kindle UA 无需额外声明");
    }

    #[test]
    fn dims_parsed_from_png_gif_jpeg_bmp() {
        let png = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H',
            b'D', b'R', 0x00, 0x00, 0x02, 0x80, 0x00, 0x00, 0x01, 0xE0,
        ];
        assert_eq!(parse_dims(&png), Some(Dims { width: 640, height: 480 }));

        let gif = [b'G', b'I', b'F', b'8', b'9', b'a', 0x40, 0x01, 0xF0, 0x00];
        assert_eq!(parse_dims(&gif), Some(Dims { width: 320, height: 240 }));

        // SOI + 一个 SOF0 段（长度 0x0011 = 17，精度 8，高 480，宽 640）
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xE0, 0x02, 0x80, 0x03, 0x01, 0x22,
            0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01,
        ];
        assert_eq!(parse_dims(&jpeg), Some(Dims { width: 640, height: 480 }));

        let mut bmp = vec![b'B', b'M'];
        bmp.resize(18, 0);
        bmp.extend_from_slice(&640i32.to_le_bytes());
        bmp.extend_from_slice(&480i32.to_le_bytes());
        assert_eq!(parse_dims(&bmp), Some(Dims { width: 640, height: 480 }));

        assert_eq!(parse_dims(b"not an image"), None);
        assert_eq!(parse_dims(&[]), None);
    }

    #[test]
    fn etag_is_stable_and_quoted() {
        let a = etag_of("https://img.example/a.jpg");
        let b = etag_of("https://img.example/a.jpg");
        assert_eq!(a, b);
        assert!(a.starts_with('"') && a.ends_with('"'));
        assert_ne!(a, etag_of("https://img.example/b.jpg"));
    }

    // ---------------- 长条页切片 ----------------

    /// 合成一张「暗底 + 指定行白带」的灰度图，用来验证近白行切点。
    fn luma_with_white_band(w: u32, h: u32, band: std::ops::Range<usize>) -> image::GrayImage {
        image::GrayImage::from_fn(w, h, |x, y| {
            let _ = x;
            let v = if band.contains(&(y as usize)) { 255u8 } else { 40u8 };
            image::Luma([v])
        })
    }

    #[test]
    fn plan_cuts_snaps_to_white_row_within_radius() {
        // 1200×2266 的源像素空间：win≈1547、ov≈83、r≈124，target≈1547
        let luma = luma_with_white_band(1200, 2266, 1480..1491);
        let cuts = plan_cuts(&luma, 1547, 83, 124, 13, SLICE_MU, SLICE_SD_U8 as f64, 2.0);
        assert_eq!(cuts.len(), 3, "两片 + 首尾切点");
        assert_eq!(cuts[0], 0);
        // 白带内离 target 最近的近白行（带的下沿）就是切点
        assert!(cuts[1] >= 1480 && cuts[1] <= 1498, "切点应落在白带内: {cuts:?}");
        assert_eq!(cuts[2], 2266);
        // 上界保证：每片显示高 ≤ vh
        assert!(cuts[1] <= 1547, "切点不得超过窗口上界");
    }

    #[test]
    fn plan_cuts_guard_rejects_cut_inside_face_band() {
        // 100×400：整图近白，但 156..164 行是「脸/眼」暗带（全宽暗像素）。
        // target=150 正好落在暗带上方的白高光行：μ/σ 能过，但 ±10 带内有暗像素 → 必须拒绝。
        let luma = image::GrayImage::from_fn(100, 400, |_x, y| {
            let v = if (156..164).contains(&(y as usize)) { 30u8 } else { 255u8 };
            image::Luma([v])
        });
        let cuts = plan_cuts(&luma, 150, 30, 30, 10, SLICE_MU, SLICE_SD_U8 as f64, 2.0);
        assert_ne!(cuts[1], 150, "切在暗带旁边的候选必须被墨量守卫拒掉: {cuts:?}");
        // 退到带外最近的一行（146：±10 带内已不含 156 行）
        assert_eq!(cuts[1], 146, "应退到墨量达标的最近行: {cuts:?}");
        assert!(cuts.windows(2).all(|w| w[1] > w[0]), "切点必须升序: {cuts:?}");
        assert_eq!(cuts.last().copied(), Some(400));
    }

    #[test]
    fn plan_cuts_prefers_tail_high_enough_on_last_cut() {
        // 100×860 整图纯白、win=400、ov=50：最后一刀 target=750 会剩 110px 薄片
        //（< 0.3×400=120），必须前移到 740，让末片正好 120px。
        let luma = image::GrayImage::from_pixel(100, 860, image::Luma([255]));
        let cuts = plan_cuts(&luma, 400, 50, 30, 10, SLICE_MU, SLICE_SD_U8 as f64, 2.0);
        assert_eq!(cuts[1], 400, "非末刀不受末片规则影响");
        assert_eq!(cuts[2], 740, "末刀前移保证末片 ≥ 0.3 窗高: {cuts:?}");
        assert!(860 - cuts[2] >= 120);
        // 间距下限也在：相邻切点 ≥ 0.35×win
        assert!(cuts[2] - cuts[1] >= 140);
    }

    #[test]
    fn slice_key_uses_sl2_and_ink_params() {
        let p = SliceParams::default();
        let label = p.label();
        assert!(label.starts_with("sl2-dw928-vh1196-ov64-mu200-sd30-r96"), "{label}");
        assert!(label.ends_with("-ik2-g10"), "墨量守卫参数必须进 key: {label}");
        // 守卫带宽/阈值是常量，key 里必须能看出用的是哪一组
        assert_eq!(SLICE_INK_PCT, 2);
        assert_eq!(SLICE_G_CSS, 10);
    }

    #[test]
    fn plan_cuts_hard_cuts_when_no_white_row() {
        // 全暗图：窗口内没有近白行，硬切在本片最远端 target = win = 1547
        let luma = luma_with_white_band(1200, 2266, 0..0);
        let cuts = plan_cuts(&luma, 1547, 83, 124, 13, SLICE_MU, SLICE_SD_U8 as f64, 2.0);
        assert_eq!(cuts, vec![0, 1547, 2266]);
        // 第二片从 1547 − 83 开始，与上片重叠 83 行
        let ranges = slice_ranges(&cuts, 83, 2266);
        assert_eq!(ranges[1].0, 1464);
    }

    #[test]
    fn plan_cuts_ranges_cover_height_and_overlap() {
        let luma = luma_with_white_band(1200, 2872, 0..0);
        let (win, ov, r, g) = src_metrics(1200, &SliceParams::default());
        let cuts = plan_cuts(&luma, win, ov, r, g, SLICE_MU, SLICE_SD_U8 as f64, 2.0);
        let ranges = slice_ranges(&cuts, ov, 2872);
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges.last().unwrap().1, 2872, "必须覆盖到底");
        // 相邻片在 overlap 区重叠：第二片起点 = 切点 − ov
        assert_eq!(ranges[1].0, cuts[1] - ov);
        for (s, e) in &ranges {
            assert!(e - s <= win, "单片源高不得超过窗高: {s}..{e}");
        }
        // 确定性：同一图同一参数必须得到同一 cuts
        assert_eq!(cuts, plan_cuts(&luma, win, ov, r, g, SLICE_MU, SLICE_SD_U8 as f64, 2.0));
    }

    #[test]
    fn slice_count_matches_measured_samples() {
        let p = SliceParams::default();
        // 三条长条页 → 2 片；1.1 屏页与横版 → 不切
        assert_eq!(slice_count_window(1200, 2266, &p), 2);
        assert_eq!(slice_count_window(1200, 2296, &p), 2);
        assert_eq!(slice_count_window(1200, 2872, &p), 2);
        assert_eq!(slice_count_window(1024, 1471, &p), 1);
        assert_eq!(slice_count_window(1024, 736, &p), 1);
        assert_eq!(slice_count_window(900, 1300, &p), 1);
        assert!(!worth_slicing(1024, 1471, &p));
        assert!(worth_slicing(1200, 2266, &p));
        // 估算的片高数组长度与片数一致，且都不超过 vh
        for (w, h) in [(1200u32, 2266u32), (1200, 2872), (1024, 1471)] {
            let hs = slice_display_heights(w, h, &p);
            assert_eq!(hs.len(), slice_count_window(w, h, &p));
            assert!(hs.iter().all(|v| *v <= p.vh), "{w}x{h} 片高越界: {hs:?}");
        }
    }

    #[test]
    fn slice_cache_keys_are_unique_and_deterministic() {
        let u = "https://img.example/long.jpg";
        let cfg = EinkConfig::default();
        let p = SliceParams::default();
        let base = variant_key(u, true, &cfg, 70);
        let k0 = slice_variant_key(&base, &p, 0, 2);
        let k1 = slice_variant_key(&base, &p, 1, 2);
        let k3 = slice_variant_key(&base, &p, 2, 3);
        assert_ne!(k0, k1);
        assert_ne!(k1, k3, "同一片在不同总片数下不能共用 key");
        assert!(k0.contains("sl2-dw928-vh1196-ov64-mu200-sd30-r96-ik2-g10-i0of2"), "{k0}");
        assert_eq!(k0, slice_variant_key(&base, &p, 0, 2), "key 必须可复现");
        let small = SliceParams::new(Some(1072), None, None);
        assert_ne!(
            slice_variant_key(&base, &p, 0, 2),
            slice_variant_key(&base, &small, 0, 2),
            "版心宽必须进 key"
        );
        // 计划 key 与片 key 不撞
        assert_ne!(slice_plan_key(u, &cfg, &p), k0);
    }

    #[test]
    fn transcode_slice_degrades_to_whole_below_1_3_screens() {
        // 1024×1471 = 1.29 屏（928 版心）：必须不切，且字节与整图路径完全一致
        let jpeg = make_jpeg(1024, 1471);
        let cfg = EinkConfig::default();
        let p = SliceParams::default();
        let (whole, whole_dims, whole_q) =
            transcode_jpeg_to_gray(&jpeg, gray_profile(70), cfg).expect("whole");
        let sliced = transcode_slice(
            &jpeg,
            image::ImageFormat::Jpeg,
            gray_profile(70),
            cfg,
            &p,
            0,
        )
        .expect("slice");
        assert_eq!(sliced.n, 1, "1.1 屏页不许切");
        assert_eq!(sliced.index, 0);
        assert_eq!(sliced.dims, whole_dims);
        assert_eq!(sliced.quality, whole_q);
        assert_eq!(sliced.data, whole, "不切时必须与整图字节完全相同");
    }

    #[test]
    fn transcode_slice_splits_long_page_and_keeps_overlap() {
        // 1200×2266 + 白带 → 2 片；第 2 片起点比切点提前 overlap 行
        let w = 1200u32;
        let h = 2266u32;
        let luma = luma_with_white_band(w, h, 1480..1491);
        let mut buf = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 92)
            .encode_image(&luma)
            .expect("encode");
        let cfg = EinkConfig::default();
        let p = SliceParams::default();
        let first =
            transcode_slice(&buf, image::ImageFormat::Jpeg, gray_profile(70), cfg, &p, 0)
                .expect("slice 0");
        assert_eq!(first.n, 2);
        assert_eq!(first.index, 0);
        let plan = first.plan.as_ref().expect("plan");
        let ov = {
            let (win, ov, r, g) = src_metrics(w, &p);
            let expect = plan_cuts(&luma, win, ov, r, g, SLICE_MU, SLICE_SD_U8 as f64, 2.0);
            assert_eq!(expect.len(), 3);
            // JPEG 有损：白带边界可能被振铃污染，允许切点在带内 ±2 行漂移
            assert!(
                plan.cuts[1].abs_diff(expect[1]) <= 2,
                "切点 {:?} 应接近 {:?}",
                plan.cuts,
                expect
            );
            ov
        };
        let ranges = slice_ranges(&plan.cuts, ov, h as usize);
        assert_eq!(ranges.len(), 2);
        assert_eq!(first.dims.height as usize, ranges[0].1 - ranges[0].0);
        let second =
            transcode_slice(&buf, image::ImageFormat::Jpeg, gray_profile(70), cfg, &p, 1)
                .expect("slice 1");
        assert_eq!(second.index, 1);
        assert_eq!(second.dims.height as usize, ranges[1].1 - ranges[1].0);
        // 越界 i 被夹到末片
        let clamped =
            transcode_slice(&buf, image::ImageFormat::Jpeg, gray_profile(70), cfg, &p, 9)
                .expect("clamp");
        assert_eq!(clamped.index, 1, "i 越界夹到 n−1");
    }
}
