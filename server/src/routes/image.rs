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
use crate::{image_cache, req, web_book};

/// 单张图片回源上限（字节）；流式读取按块累计，实际判定用 `CACHE_ITEM_MAX`，本常量仅单测引用。
#[allow(dead_code)]
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

#[path = "image_config.rs"]
mod image_config;
use image_config::*;
#[path = "image_serve.rs"]
mod image_serve;
use image_serve::*;
#[path = "image_slice.rs"]
mod image_slice;
use image_slice::*;
pub use image_slice::{
    plan_cuts, slice_count_window, slice_display_heights, slice_ranges, worth_slicing, SliceParams,
};
#[path = "image_transcode.rs"]
mod image_transcode;
use image_transcode::*;
#[path = "image_body.rs"]
mod image_body;
use image_body::*;
pub use image_body::{parse_dims, Dims};
#[path = "image_response.rs"]
mod image_response;
use image_response::*;
#[path = "image_slice_http.rs"]
mod image_slice_http;
use image_slice_http::*;
#[path = "image_meta.rs"]
mod image_meta;
use image_meta::*;
pub(crate) use image_meta::{probe_dims, source_referer};
#[path = "image_proxy.rs"]
mod image_proxy;
pub use image_proxy::allowed_content_type;
use image_proxy::*;

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
