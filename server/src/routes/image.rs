//! 图片代理：`GET /api/book/image?url=<abs>&book_url=<source_id>&chapter_url=<ch>`
//!
//! 带书源 Referer/UA/Cookie 回源，复用 `req::ensure_public_url` 做 SSRF 拦截；
//! 只允许 `image/*`；单图超过上限（默认 8 MiB）返回 413（长度未知时流式截断）；
//! 流式转发不整图入内存，同时把不超过 2 MiB 的图片写入进程内 LRU（默认 64 MiB）。

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

/// KPW3 内核不支持的图片格式（真机实测 WebP 解码失败）。
fn unsupported_image_format(ct: &str) -> Option<&'static str> {
    let base = ct.split(';').next().unwrap_or("").trim().to_lowercase();
    match base.as_str() {
        "image/webp" => Some("webp"),
        _ => None,
    }
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

/// 从图片头部字节解析宽高：支持 PNG / GIF / JPEG / BMP；解析不出来返回 None（不报错）。
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
    if let Some(fmt) = unsupported_image_format(&ct) {
        return err_item(
            &url,
            "unsupported_image_format",
            &format!(
                "源图为 {}（KPW3 不支持），前端应跳过",
                fmt.to_ascii_uppercase()
            ),
        );
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
    let etag = etag_of(&q.url);
    if let Some((data, ct)) = image_cache::get(&q.url) {
        return serve_bytes(data, &ct, &etag, true, &headers);
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
    if let Some(fmt) = unsupported_image_format(&ct) {
        // 明确降级：不把一张 KPW3 永远解码失败的图丢给前端，前端据此跳过或换源。
        let mut res = error_body(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_image_format",
            &format!(
                "源图为 {}（Kindle KPW3 内核不支持该格式），已跳过这张图",
                fmt.to_ascii_uppercase()
            ),
        );
        if let Ok(v) = HeaderValue::from_str(fmt) {
            res.headers_mut()
                .insert(HeaderName::from_static("x-image-unsupported"), v);
        }
        return res;
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
    if matches_if_none_match(&headers, &etag) {
        return not_modified(&etag);
    }

    let ct_base = ct
        .split(';')
        .next()
        .unwrap_or("image/*")
        .trim()
        .to_string();

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
    let key = q.url.clone();
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
    b = build_etag_headers(b, &etag)
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

    #[test]
    fn webp_is_declared_unsupported() {
        assert_eq!(unsupported_image_format("image/webp"), Some("webp"));
        assert_eq!(unsupported_image_format("IMAGE/WEBP; charset=binary"), Some("webp"));
        assert!(unsupported_image_format("image/jpeg").is_none());
        assert!(unsupported_image_format("image/png").is_none());
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
