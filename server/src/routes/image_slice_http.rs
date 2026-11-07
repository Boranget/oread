//! 图片代理的 slice_http 部分（move-only 从 image.rs 拆出）。

use super::*;

/// 长条页切片：`slice=1&i=<k>` 的完整路径（缓存 → 回源 → 解码 → 切点 → 裁片 → 编码）。
///
/// 确定性：切点只由「原图 + 切片参数 + 转码宽」决定，计划缓存在 `#{label}-plan` 下；
/// 片 key 带 `i{i}of{n}`，不会出现「缓存 3 片、请求要 2 片」的错位。
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_slice(
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
                res.headers_mut()
                    .insert(HeaderName::from_static("x-image-slices"), v);
            }
            if let Ok(v) = HeaderValue::from_str(&i.to_string()) {
                res.headers_mut()
                    .insert(HeaderName::from_static("x-image-slice"), v);
            }
            return res;
        }
        if let Some(etag) = matching_key_etag(headers, &keys) {
            return not_modified(etag);
        }
    }

    // 2) 回源
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
    let ct_base = ct.split(';').next().unwrap_or("image/*").trim().to_string();
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
            &format!(
                "切片不支持的图片类型：{}",
                if ct.is_empty() { "未知" } else { &ct }
            ),
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
    let label = if out.n <= 1 {
        "slice:whole-gray"
    } else {
        "slice:gray"
    };
    serve_slice_response(out, &etag, headers, label, source_bytes, started.elapsed())
}
