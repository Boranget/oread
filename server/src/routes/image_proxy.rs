//! 图片代理的 proxy 部分（move-only 从 image.rs 拆出）。

use super::*;

/// 按魔数嗅探解码后的图片类型（加密源上游常返回 application/octet-stream）。
pub(super) fn sniff_image_mime(bytes: &[u8]) -> &'static str {
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Jpeg) => "image/jpeg",
        Ok(image::ImageFormat::Png) => "image/png",
        Ok(image::ImageFormat::Gif) => "image/gif",
        Ok(image::ImageFormat::WebP) => "image/webp",
        Ok(image::ImageFormat::Bmp) => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// `ContentRule.image_decode` 源：整图缓冲 → 规则解密 → 按真实格式直出。
/// 解密失败时 `web_book::decode_image` 会回退原字节，不会让整章 502。
pub(super) async fn proxy_decoded(
    source: &BookSource,
    q: &ImageQ,
    headers: &HeaderMap,
    max_bytes: usize,
) -> Response {
    let hdrs = source_referer(source, q.chapter_url.as_deref(), q.book_url.as_deref());
    let (budget, src_timeout) = image_budget(source);
    let opened = match tokio::time::timeout(
        budget,
        req::open_stream_for_source_timed(
            &q.url,
            &hdrs,
            false,
            src_timeout,
            Some(&source.book_source_url),
            req::source_limit(source),
        ),
    )
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return map_open_error(e),
        Err(_) => {
            return error_body(
                StatusCode::GATEWAY_TIMEOUT,
                "upstream_timeout",
                &format!("图片回源超时（{}s）", budget.as_secs()),
            )
        }
    };
    let resp = opened.response;
    let status = resp.status();
    if !status.is_success() {
        return error_body(
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            &format!("源站返回 {status}"),
        );
    }
    let raw = match collect_image_body(resp, max_bytes).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let decoded = web_book::decode_image(source, &q.url, raw);
    let etag = etag_of(&format!("{}#decoded", q.url));
    let ct = sniff_image_mime(&decoded).to_string();
    serve_bytes(decoded, &ct, &etag, false, headers)
}

pub(super) async fn proxy(
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
    // 加密图源（ContentRule.image_decode）：不走切片/转码，先整图解密再直出。
    if web_book::has_image_decode(&source) {
        return proxy_decoded(&source, &q, &headers, max_bytes).await;
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
    let (budget, src_timeout) = image_budget(&source);
    let opened = match tokio::time::timeout(
        budget,
        req::open_stream_for_source_timed(
            &q.url,
            &hdrs,
            false,
            src_timeout,
            Some(&source.book_source_url),
            req::source_limit(&source),
        ),
    )
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return map_open_error(e),
        Err(_) => {
            return error_body(
                StatusCode::GATEWAY_TIMEOUT,
                "upstream_timeout",
                &format!("图片回源超时（{}s）", budget.as_secs()),
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
            &format!(
                "源站返回非图片内容：{}",
                if ct.is_empty() { "未知" } else { &ct }
            ),
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

    let ct_base = ct.split(';').next().unwrap_or("image/*").trim().to_string();

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
        let label = format!(
            "webp->jpeg-{}",
            if profile.grayscale { "gray" } else { "color" }
        );
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
        let decoded =
            tokio::task::spawn_blocking(move || match transcode_jpeg_to_gray(&raw, profile, cfg) {
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
    let known_len = upstream_len.or(if ended { Some(head.len() as u64) } else { None });
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
                        Err(std::io::Error::other(format!("读取上游图片失败: {e}"))),
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
            .header(
                HeaderName::from_static("x-image-width"),
                d.width.to_string(),
            )
            .header(
                HeaderName::from_static("x-image-height"),
                d.height.to_string(),
            );
    }
    b.body(Body::from_stream(stream)).unwrap_or_else(|_| {
        error_body(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "构建响应失败",
        )
    })
}

/// 只允许图片响应；SVG 等文本型图片由 CSP sandbox + nosniff 兜底。
/// 生产路径直接调用 `is_image_content_type`；本函数是语义别名，仅单测使用。
#[allow(dead_code)]
pub fn allowed_content_type(ct: &str) -> bool {
    is_image_content_type(ct)
}
