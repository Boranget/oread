//! 图片代理的 response 部分（move-only 从 image.rs 拆出）。

use super::*;

pub(super) fn build_etag_headers(
    builder: axum::http::response::Builder,
    etag: &str,
) -> axum::http::response::Builder {
    builder
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .header(header::ETAG, etag)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            HeaderName::from_static("content-security-policy"),
            "default-src 'none'; sandbox",
        )
}

pub(super) fn not_modified(etag: &str) -> Response {
    let mut b = Response::builder().status(StatusCode::NOT_MODIFIED);
    b = build_etag_headers(b, etag);
    b.body(Body::empty())
        .unwrap_or_else(|_| StatusCode::NOT_MODIFIED.into_response())
}

pub(super) fn matches_if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').any(|s| s.trim() == etag))
        .unwrap_or(false)
}

pub(super) fn serve_bytes(
    data: Vec<u8>,
    ct: &str,
    etag: &str,
    hit: bool,
    headers: &HeaderMap,
) -> Response {
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
            .header(
                HeaderName::from_static("x-image-width"),
                d.width.to_string(),
            )
            .header(
                HeaderName::from_static("x-image-height"),
                d.height.to_string(),
            );
    }
    b.body(Body::from(data)).unwrap_or_else(|_| {
        error_body(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "构建响应失败",
        )
    })
}

/// 转码响应：在通用响应上补 `x-image-transcoded`（路径/宽度/质量）、
/// `x-image-source-bytes` 与 `x-image-transcode-ms`，便于前端与压测量化收益。
#[allow(clippy::too_many_arguments)]
pub(super) fn serve_transcoded(
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
pub(super) const MAX_META_BATCH: usize = 30;

/// Content-Type → 解码格式；未知类型返回 `None`（不做切片，走原图直出）。
pub(super) fn image_format_of(ct: &str) -> Option<image::ImageFormat> {
    let base = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
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
pub(super) fn slice_key_pairs(
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
pub(super) fn serve_slice_response(
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
        res.headers_mut()
            .insert(HeaderName::from_static("x-image-slices"), v);
    }
    if let Ok(v) = HeaderValue::from_str(&i.to_string()) {
        res.headers_mut()
            .insert(HeaderName::from_static("x-image-slice"), v);
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
