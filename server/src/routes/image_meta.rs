//! 图片代理的 meta 部分（move-only 从 image.rs 拆出）。

use super::*;

pub(super) fn err_item(url: &str, code: &str, message: &str) -> serde_json::Value {
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
    timeout: Option<Duration>,
    source_key: Option<&str>,
    limit: req::SourceLimit,
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
    let budget = timeout.unwrap_or(IMAGE_TIMEOUT);
    let opened = match tokio::time::timeout(
        budget,
        req::open_stream_for_source_timed(&url, &hdrs, false, timeout, source_key, limit),
    )
    .await
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
            return err_item(
                &url,
                "image_too_large",
                &format!("图片大小 {len} 字节超过上限"),
            );
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
pub(super) async fn images_meta(
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
    let timeout = req::source_timeout(&source);
    let source_limit = req::source_limit(&source);
    let items: Vec<serde_json::Value> = futures::stream::iter(req.urls.into_iter().map(|u| {
        fetch_image_meta(
            u,
            hdrs.clone(),
            max_bytes,
            timeout,
            Some(&source.book_source_url),
            source_limit,
        )
    }))
    .buffer_unordered(4)
    .collect()
    .await;
    Json(json!({"count": items.len(), "items": items})).into_response()
}

pub(crate) fn source_referer(
    source: &BookSource,
    chapter_url: Option<&str>,
    book_url: Option<&str>,
) -> HashMap<String, String> {
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
pub(super) static DIM_CACHE: Lazy<Mutex<HashMap<String, (u32, u32)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
/// 尺寸缓存上限（条目数），超出直接清空（漫画章节数量有限，不做 LRU）
pub(super) const DIM_CACHE_MAX: usize = 2000;

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
    let timeout = req::source_timeout(source);
    let source_limit = req::source_limit(source);
    let source_key = source.book_source_url.clone();
    let fetched: Vec<(usize, Option<(u32, u32)>)> = futures::stream::iter(missing)
        .map(|(i, u)| {
            let h = hdrs.clone();
            let sk = source_key.clone();
            async move {
                let v = fetch_image_meta(u, h, max_bytes, timeout, Some(&sk), source_limit).await;
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

pub(super) fn map_open_error(e: anyhow::Error) -> Response {
    let m = e.to_string();
    if m.contains("私网") || m.contains("回环") || m.contains("仅允许") || m.contains("链路本地")
    {
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
