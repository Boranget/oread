//! 图片回源的可复用入口：后台整本下载与图片代理共用同一套请求/校验逻辑。
//!
//! 代理路径仍保留流式转发（不整图入内存）；本模块用于需要完整字节的场景：
//! - 馆藏漫画 worker 把图片落盘；
//! - 入馆前的体积抽样探测（只取响应头，不读整图）。

use std::time::Duration;

use axum::http::{header, HeaderMap, StatusCode};
use futures::StreamExt;

use crate::entities::BookSource;
use crate::req;

/// 书源没有显式 `timeout_millisecond` 时的图片回源预算（与图片代理一致）。
const DEFAULT_IMAGE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct FetchedImage {
    pub data: Vec<u8>,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

fn image_budget(source: &BookSource) -> (Duration, Option<Duration>) {
    match req::source_timeout(source) {
        Some(d) => (d, Some(d)),
        None => (DEFAULT_IMAGE_TIMEOUT, None),
    }
}

/// 完整抓取一张图片；`image_decode` 源返回解密后的字节与嗅探出的真实 MIME。
pub async fn fetch(
    source: &BookSource,
    url: &str,
    referer: Option<&str>,
    max_bytes: usize,
) -> anyhow::Result<FetchedImage> {
    let parsed = url::Url::parse(url).map_err(|e| anyhow::anyhow!("URL 解析失败: {e}"))?;
    req::ensure_public_url(&parsed, false).await?;
    let hdrs = crate::routes::image::source_referer(source, referer, None);
    let resp = open(source, url, &hdrs).await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow::anyhow!("源站返回 {status}"));
    }
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let ct_base = ct.split(';').next().unwrap_or("").trim().to_string();
    if !ct_base.to_ascii_lowercase().starts_with("image/") {
        return Err(anyhow::anyhow!("源站返回非图片内容：{ct}"));
    }
    let raw = read_body(resp, max_bytes).await?;
    let (data, mime) = if crate::web_book::has_image_decode(source) {
        let decoded = crate::web_book::decode_image(source, url, raw);
        let mime = crate::routes::image::sniff_image_mime(&decoded).to_string();
        (decoded, mime)
    } else {
        (raw, ct_base)
    };
    let dims = crate::routes::image::parse_dims(&data);
    Ok(FetchedImage {
        data,
        mime,
        width: dims.map(|d| d.width),
        height: dims.map(|d| d.height),
    })
}

/// 只探测图片总字节数：优先 `Range: bytes=0-0` 读 `Content-Range`，源站不支持
/// Range 时退化为 `Content-Length`。只用于入馆前的体积估算，不读响应体。
pub async fn probe_size(
    source: &BookSource,
    url: &str,
    referer: Option<&str>,
) -> anyhow::Result<Option<u64>> {
    let parsed = url::Url::parse(url).map_err(|e| anyhow::anyhow!("URL 解析失败: {e}"))?;
    req::ensure_public_url(&parsed, false).await?;
    let mut hdrs = crate::routes::image::source_referer(source, referer, None);
    hdrs.insert("Range".to_string(), "bytes=0-0".to_string());
    let resp = open(source, url, &hdrs).await?;
    let status = resp.status();
    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        return Ok(content_range_total(resp.headers()));
    }
    if !status.is_success() {
        return Err(anyhow::anyhow!("源站返回 {status}"));
    }
    if let Some(total) = content_range_total(resp.headers()) {
        return Ok(Some(total));
    }
    // 206 的 Content-Length 是这一片（1 字节）的长度，不能当总大小。
    if status == StatusCode::PARTIAL_CONTENT {
        return Ok(None);
    }
    Ok(resp.content_length())
}

async fn open(
    source: &BookSource,
    url: &str,
    hdrs: &std::collections::HashMap<String, String>,
) -> anyhow::Result<reqwest::Response> {
    let (budget, src_timeout) = image_budget(source);
    let opened = tokio::time::timeout(
        budget,
        req::open_stream_for_source_timed(
            url,
            hdrs,
            false,
            src_timeout,
            Some(&source.book_source_url),
            req::source_limit(source),
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("图片回源超时（{}s）", budget.as_secs()))?
    .map_err(|e| anyhow::anyhow!("图片回源失败: {e}"))?;
    Ok(opened.response)
}

async fn read_body(resp: reqwest::Response, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
    if let Some(len) = resp.content_length() {
        if len as usize > max_bytes {
            return Err(anyhow::anyhow!(
                "图片大小 {len} 字节，超过上限 {max_bytes} 字节"
            ));
        }
    }
    let mut raw = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = raw.next().await {
        let c = chunk.map_err(|e| anyhow::anyhow!("读取上游图片失败: {e}"))?;
        if buf.len() + c.len() > max_bytes {
            return Err(anyhow::anyhow!("图片超过上限 {max_bytes} 字节，已拒绝转发"));
        }
        buf.extend_from_slice(&c);
    }
    Ok(buf)
}

fn content_range_total(headers: &HeaderMap) -> Option<u64> {
    let raw = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    raw.rsplit_once('/')?.1.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_total_parses_partial_and_unsatisfied() {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_RANGE, "bytes 0-0/12345".parse().unwrap());
        assert_eq!(content_range_total(&h), Some(12345));
        h.insert(header::CONTENT_RANGE, "bytes */99".parse().unwrap());
        assert_eq!(content_range_total(&h), Some(99));
        h.insert(header::CONTENT_RANGE, "bytes 0-0/*".parse().unwrap());
        assert_eq!(content_range_total(&h), None);
    }
}
