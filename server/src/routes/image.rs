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

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::time::Duration;

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
/// WebP→JPEG 转码的目标宽度上限（KPW3 视口 1072 CSS px，按 ~1.12 倍留余地）
const TRANSCODE_MAX_WIDTH: u32 = 1200;
/// 墨水屏灰度 JPEG 的默认质量：灰度在电子墨水屏上对压缩伪影不敏感，78 是可看下限。
/// 可用 `BORANLIB_EINK_JPEG_QUALITY` 覆盖（允许 60–90，越界会被夹到区间内）。
const EINK_JPEG_QUALITY_DEFAULT: u8 = 78;
const EINK_JPEG_QUALITY_MIN: u8 = 60;
const EINK_JPEG_QUALITY_MAX: u8 = 90;
const EINK_JPEG_QUALITY_ENV: &str = "BORANLIB_EINK_JPEG_QUALITY";
/// 解码前允许的最大像素数（防解压炸弹：20 MP 的灰度缓冲约 20 MB、RGB 约 60 MB）
const MAX_TRANSCODE_PIXELS: u64 = 20_000_000;

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

    /// 缓存 key 后缀，把色彩模式与质量并入，避免不同档互相覆盖。
    fn key_suffix(self) -> String {
        let mode = if self.grayscale { "g" } else { "c" };
        format!("{mode}q{}", self.quality)
    }
}

/// 读取墨水屏质量档：非法值回落默认 78，越界夹到 60–90。
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

#[derive(Deserialize)]
pub struct ImageQ {
    pub url: String,
    /// 书源 id（Legado 的 `bookSourceUrl`）
    pub source: Option<String>,
    /// 契约字段名：`book_url=<src>`，语义与 `source` 一致
    pub book_url: Option<String>,
    /// 章节页地址，用作 Referer
    pub chapter_url: Option<String>,
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

/// 缓存 key 的客户端变体：转码后的 JPEG 与原图必须分开存，不同编码档（灰度/彩色、
/// 质量）也必须分开，否则桌面/墨水屏、灰度/彩色会互相覆盖。
fn cache_variant_key(url: &str, profile: Option<EncodeProfile>) -> String {
    match profile {
        Some(p) => format!("{url}#jpeg-w{TRANSCODE_MAX_WIDTH}-{}", p.key_suffix()),
        None => format!("{url}#orig"),
    }
}

/// 目标尺寸：宽度超过上限时等比缩小，否则保持原尺寸。
fn transcode_target_size(w: u32, h: u32) -> (u32, u32) {
    if w <= TRANSCODE_MAX_WIDTH || h == 0 {
        return (w.max(1), h.max(1));
    }
    let nw = TRANSCODE_MAX_WIDTH;
    let nh = ((h as u64 * nw as u64) / w as u64).max(1) as u32;
    (nw, nh)
}

/// WebP → JPEG（内存内完成，不落盘）：解码用 `image` crate 的纯 Rust 实现，
/// 解码前先只读头部拿尺寸做像素上限校验，避免解压炸弹把设备内存打爆。
/// 灰度档先转 Luma8 再缩放，缩放量与编码量都只有 RGB 的 1/3。
fn transcode_webp_to_jpeg(
    raw: &[u8],
    profile: EncodeProfile,
) -> Result<(Vec<u8>, Dims), String> {
    let reader = image::ImageReader::with_format(
        std::io::Cursor::new(raw),
        image::ImageFormat::WebP,
    );
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| format!("WebP 头部解析失败: {e}"))?;
    if w == 0 || h == 0 {
        return Err("WebP 尺寸为 0".to_string());
    }
    if w as u64 * h as u64 > MAX_TRANSCODE_PIXELS {
        return Err(format!(
            "源图 {w}x{h} 超过解码像素上限 {MAX_TRANSCODE_PIXELS}"
        ));
    }
    let img = image::load_from_memory_with_format(raw, image::ImageFormat::WebP)
        .map_err(|e| format!("WebP 解码失败: {e}"))?;
    let (src_w, src_h) = (img.width(), img.height());
    let (tw, th) = transcode_target_size(src_w, src_h);
    let mut buf: Vec<u8> = Vec::new();
    if profile.grayscale {
        // to_luma8 使用 BT.709 亮度系数（0.2126R + 0.7152G + 0.0722B），不是通道平均
        let luma = img.to_luma8();
        let out = if (tw, th) == (luma.width(), luma.height()) {
            luma
        } else {
            image::imageops::resize(&luma, tw, th, image::imageops::FilterType::Triangle)
        };
        let dims = Dims {
            width: out.width(),
            height: out.height(),
        };
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, profile.quality)
            .encode_image(&out)
            .map_err(|e| format!("JPEG 编码失败: {e}"))?;
        Ok((buf, dims))
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
        Ok((buf, dims))
    }
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

/// 单批最多查询的图片数（前端可按 30 张一批，避免一次请求过大）
const MAX_META_BATCH: usize = 30;

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
async fn fetch_image_meta(
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

fn source_referer(source: &BookSource, chapter_url: Option<&str>, book_url: Option<&str>) -> HashMap<String, String> {
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
    let profile = EncodeProfile::eink();
    let eink_key = cache_variant_key(&q.url, Some(profile));
    let orig_key = cache_variant_key(&q.url, None);
    let eink_etag = etag_of(&eink_key);
    let orig_etag = etag_of(&orig_key);
    // 缓存命中：按变体查。墨水屏客户端还可以复用原图变体里已有的 JPEG/PNG/GIF，
    // 但绝不能复用其中的 WebP（那正是它解不了的格式）。
    if transcode {
        if let Some((data, ct)) = image_cache::get(&eink_key) {
            return serve_bytes(data, &ct, &eink_etag, true, &headers);
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

    // WebP 且客户端不认 WebP：整图读入内存 → 纯 Rust 解码 → 缩放 → JPEG，不落盘。
    // 源站无视 Accept 一律返回 WebP 的情况（如 content.mkzcdn.com）只能走这条路。
    if is_webp_content_type(&ct_base) && transcode {
        if matches_if_none_match(&headers, &eink_etag) {
            return not_modified(&eink_etag);
        }
        let raw = match collect_image_body(resp, max_bytes).await {
            Ok(v) => v,
            Err(r) => return r,
        };
        let source_bytes = raw.len();
        let decoded =
            tokio::task::spawn_blocking(move || transcode_webp_to_jpeg(&raw, profile)).await;
        let (jpeg, dims) = match decoded {
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
        if jpeg.len() <= CACHE_ITEM_MAX {
            image_cache::put(&eink_key, jpeg.clone(), "image/jpeg".to_string());
        }
        let mut res = serve_bytes(jpeg, "image/jpeg", &eink_etag, false, &headers);
        if let Ok(v) = HeaderValue::from_str(&format!(
            "webp->jpeg-{};w={};q={}",
            if profile.grayscale { "gray" } else { "color" },
            dims.width,
            profile.quality
        )) {
            res.headers_mut()
                .insert(HeaderName::from_static("x-image-transcoded"), v);
        }
        if let Ok(v) = HeaderValue::from_str(&source_bytes.to_string()) {
            res.headers_mut()
                .insert(HeaderName::from_static("x-image-source-bytes"), v);
        }
        return res;
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
        let u = "https://img.example/a.webp";
        let g78 = cache_variant_key(u, Some(gray_profile(78)));
        let c82 = cache_variant_key(u, Some(color_profile(82)));
        let g60 = cache_variant_key(u, Some(gray_profile(60)));
        let orig = cache_variant_key(u, None);
        assert_ne!(g78, c82, "灰度/彩色档不能共用 key");
        assert_ne!(g78, g60, "不同质量档不能共用 key");
        assert_ne!(g78, orig, "转码档与原图不能共用 key");
        assert_ne!(etag_of(&g78), etag_of(&c82));
        assert_ne!(etag_of(&g78), etag_of(&orig));
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
}
