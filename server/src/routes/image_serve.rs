//! 图片代理的 serve 部分（move-only 从 image.rs 拆出）。

use super::*;

pub(super) fn error_body(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        json!({"error": code, "message": message}).to_string(),
    )
        .into_response()
}

pub(super) fn etag_of(url: &str) -> String {
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    format!("\"{:016x}\"", h.finish())
}

pub(super) fn is_image_content_type(ct: &str) -> bool {
    let base = ct.split(';').next().unwrap_or("").trim().to_lowercase();
    base.starts_with("image/")
}

pub(super) fn is_webp_content_type(ct: &str) -> bool {
    let base = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    base == "image/webp"
}

pub(super) fn is_jpeg_content_type(ct: &str) -> bool {
    let base = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(base.as_str(), "image/jpeg" | "image/jpg" | "image/pjpeg")
}

/// 是否需要把 WebP 转成 JPEG：老 Kindle/Silk 一律转；其他客户端仅在 Accept 明确
/// 不含 `image/webp` 时转。Accept 缺失按现代客户端处理，原样透传。
pub(super) fn wants_webp_transcode(headers: &HeaderMap) -> bool {
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
pub(super) fn is_eink_client(headers: &HeaderMap, q_eink: Option<&str>) -> bool {
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
pub(super) fn variant_key(url: &str, jpeg_source: bool, cfg: &EinkConfig, quality: u8) -> String {
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
pub(super) fn candidate_qualities(cfg: &EinkConfig) -> Vec<u8> {
    if cfg.adaptive {
        vec![EINK_ADAPTIVE_TEXT_Q, EINK_ADAPTIVE_ART_Q]
    } else {
        vec![cfg.quality]
    }
}
