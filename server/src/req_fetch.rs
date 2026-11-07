//! req 的请求执行与分级重试（move-only 从 req.rs 拆出）。

use super::*;

// ---------------- 请求执行（逐跳手动跟随 + 按源 Cookie） ----------------

pub(super) struct HttpResponse {
    pub(super) status: StatusCode,
    pub(super) body: Vec<u8>,
    pub(super) content_type: Option<String>,
}

pub(super) type AttemptError = (SendKind, anyhow::Error);

pub(super) fn is_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

/// 执行一次完整请求（含重定向链）。每跳都按源注入 Cookie、回写 Set-Cookie，
/// 需要时逐跳做私网校验（预览/图片代理场景）。
#[allow(clippy::too_many_arguments)] // 请求上下文（方法/体/头/Cookie/超时等）逐项传入；收拢成结构体会引入不必要的生命周期与可变借用的复杂度。
pub(super) async fn execute_request(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    cookie_key: &str,
    allow_private: bool,
    validate_private: bool,
    max_redirects: usize,
    timeout: Option<Duration>,
) -> Result<HttpResponse, AttemptError> {
    let c = conf();
    let mut current =
        Url::parse(url).map_err(|e| (SendKind::Other, anyhow!("URL 解析失败: {e}")))?;
    let mut method = method.to_uppercase();
    let mut body = body.map(|s| s.to_string());

    for _ in 0..=max_redirects {
        if validate_private {
            ensure_public_url(&current, allow_private)
                .await
                .map_err(|e| (SendKind::Other, e))?;
        }
        let mut req_headers = headers.clone();
        if !req_headers.keys().any(|k| k.eq_ignore_ascii_case("cookie")) {
            if let Some(cookie) = cookie_header_for_request(cookie_key, current.as_str()) {
                req_headers.insert("Cookie".to_string(), cookie);
            }
        }
        let resp = build_request(
            client(),
            &method,
            current.as_str(),
            &req_headers,
            body.as_deref(),
            timeout,
        )
        .send()
        .await
        .map_err(|e| {
            let (kind, err) = send_failure(e, current.as_str(), timeout);
            // 非法 URL/请求构造属于书源配置问题，不算“站点不可达”；
            // 连接失败、超时与其他发送错误则标记为站点侧故障，供冷却判定使用。
            let err = if matches!(kind, SendKind::Builder) {
                err
            } else {
                anyhow::Error::new(SourceUnavailable(err.to_string()))
            };
            (kind, err)
        })?;
        record_response_cookies(cookie_key, resp.headers());
        // 同步镜像到目标 host 桶：JS/AJAX 与图片代理是按 host 发起的子请求，
        // 需要复用同一站点的会话 Cookie（否则登录态在 java.ajax 里丢失）。
        let host_scope = cookie_scope(current.as_str(), None);
        if host_scope != cookie_key {
            record_response_cookies(&host_scope, resp.headers());
        }
        let status = resp.status();
        if is_redirect(status) {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .ok_or_else(|| (SendKind::Other, anyhow!("重定向缺少 Location")))?;
            // 303，以及 301/302 下非 GET/HEAD 的请求按浏览器语义改为 GET 且丢弃 body
            if status == StatusCode::SEE_OTHER
                || (matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND)
                    && !matches!(method.as_str(), "GET" | "HEAD"))
            {
                method = "GET".to_string();
                body = None;
            }
            current = current
                .join(&loc)
                .map_err(|e| (SendKind::Other, anyhow!("重定向地址非法: {e}")))?;
            continue;
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let buf = read_body_limited(resp, c.max_body_bytes)
            .await
            .map_err(|e| (SendKind::Other, e))?;
        return Ok(HttpResponse {
            status,
            body: buf,
            content_type,
        });
    }
    Err((SendKind::Other, anyhow!("重定向超过 {max_redirects} 跳")))
}

/// 统一的文本抓取入口：按源 Cookie 分桶 + 分级重试 + 具名解码。
#[allow(clippy::too_many_arguments)] // 与 `execute_request` 同源参数集，保持逐项传入以便调用点显式可见。
pub(super) async fn fetch_text(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    source_key: Option<&str>,
    allow_private: bool,
    validate_private: bool,
    timeout: Option<Duration>,
    limit: SourceLimit,
) -> anyhow::Result<String> {
    // 先按源限流（次数/间隔），再抢全局 16 槽：某个源的排队不会把全局槽占死。
    let _source_permit = match source_key {
        Some(k) => {
            acquire_source_rate(k, limit).await?;
            Some(acquire_source_slot(k, limit.concurrency).await?)
        }
        None => None,
    };
    let _permit = acquire_permit().await?;
    let scope = cookie_scope(url, source_key);
    let max_redirects = if validate_private {
        MAX_REDIRECTS
    } else {
        FOLLOW_REDIRECTS
    };
    let mut retried_5xx = false;
    loop {
        match execute_request(
            url,
            method,
            headers,
            body,
            &scope,
            allow_private,
            validate_private,
            max_redirects,
            timeout,
        )
        .await
        {
            Ok(resp) => {
                // 5xx 重试 1 次；4xx/3xx 不再重试
                if resp.status.is_server_error() && !retried_5xx {
                    retried_5xx = true;
                    continue;
                }
                if !resp.status.is_success() {
                    return Err(anyhow::Error::new(SourceUnavailable(format!(
                        "http status {}",
                        resp.status
                    ))));
                }
                return decode_body(&resp.body, resp.content_type.as_deref());
            }
            // 连接失败/超时/URL 非法都不重试
            Err((_kind, e)) => return Err(e),
        }
    }
}

/// 发起 HTTP 请求，返回响应文本（未指定书源，Cookie 按目标 host 分桶）。
/// 失败原因（超时/并发等待超时/响应超限/解码失败）都会带明确文案。
/// 生产路径调用 `fetch_text`/`fetch_page_for_source_timed`；本便捷封装仅单测使用。
#[allow(dead_code)]
pub async fn fetch(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
) -> anyhow::Result<String> {
    fetch_text(
        url,
        method,
        headers,
        body,
        None,
        false,
        false,
        None,
        SourceLimit::concurrency_only(0),
    )
    .await
}

/// HEAD 响应：只保留状态码、最终 URL 与响应头（供 `java.head` / jsLib `Reload` 读 etag）。
pub struct HeadResponse {
    pub status: u16,
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// `java.head(url[, headers])` 的原生实现：不跟随重定向、不读正文，与上游 okhttp 的
/// `followRedirects(false)` 一致。与 `java.ajax` 同源，不注入书源 Cookie 桶。
pub async fn head(url: &str, headers: &HashMap<String, String>) -> anyhow::Result<HeadResponse> {
    let _permit = acquire_permit().await?;
    let parsed = Url::parse(url).map_err(|e| anyhow!("URL 解析失败: {e}"))?;
    ensure_public_url(&parsed, false).await?;
    let resp = build_request(client(), "HEAD", url, headers, None, None)
        .send()
        .await
        .map_err(|e| anyhow!("HEAD 请求失败: {e}"))?;
    let mut out = Vec::new();
    for (k, v) in resp.headers() {
        if let Ok(s) = v.to_str() {
            out.push((k.to_string(), s.to_string()));
        }
    }
    Ok(HeadResponse {
        status: resp.status().as_u16(),
        url: resp.url().to_string(),
        headers: out,
    })
}

/// 按书源发起请求：Cookie 按 `bookSourceUrl` 分桶，其余与 `fetch` 一致。
pub async fn fetch_for_source(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    source_key: &str,
) -> anyhow::Result<String> {
    fetch_for_source_timed(url, method, headers, body, source_key, None).await
}

/// 按书源发起请求，并显式指定本次超时（`None` 回退全局 `HTTP_TIMEOUT_SECS`）。
/// 搜索/详情/目录/正文四类抓取都通过它消费书源 `timeout_millisecond`。
pub async fn fetch_for_source_timed(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    source_key: &str,
    timeout: Option<Duration>,
) -> anyhow::Result<String> {
    fetch_text(
        url,
        method,
        headers,
        body,
        Some(source_key),
        false,
        false,
        timeout,
        SourceLimit::concurrency_only(source_concurrency_default()),
    )
    .await
}

/// 与 [`fetch_for_source_timed`] 相同，但调用方已知书源的在飞上限（`concurrentRate`）。
pub async fn fetch_for_source_limited(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    source_key: &str,
    timeout: Option<Duration>,
    limit: SourceLimit,
) -> anyhow::Result<String> {
    fetch_text(
        url,
        method,
        headers,
        body,
        Some(source_key),
        false,
        false,
        timeout,
        limit,
    )
    .await
}

/// 页面抓取结果：保留状态码与 Content-Type，供「按链接导入」区分登录页 / 非 HTML。
pub struct PageResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: String,
}

/// GET 一个页面（按源 Cookie 分桶、逐跳 SSRF 校验、分级解码），不校验 2xx：
/// 状态码与类型交给调用方判定，避免把登录页静默当成书籍详情。
/// 生产路径使用带超时的 `fetch_page_for_source_timed`；本封装保留默认超时语义。
#[allow(dead_code)]
pub async fn fetch_page_for_source(
    url: &str,
    headers: &HashMap<String, String>,
    source_key: &str,
) -> anyhow::Result<PageResponse> {
    fetch_page_for_source_timed(url, headers, source_key, None).await
}

/// 与 [`fetch_page_for_source`] 相同，但按书源超时（`None` 回退全局默认）。
pub async fn fetch_page_for_source_timed(
    url: &str,
    headers: &HashMap<String, String>,
    source_key: &str,
    timeout: Option<Duration>,
) -> anyhow::Result<PageResponse> {
    let source_limit = SourceLimit::concurrency_only(source_concurrency_default());
    fetch_page_for_source_limited(url, headers, source_key, timeout, source_limit).await
}

/// 与 [`fetch_page_for_source_timed`] 相同，但显式指定书源的在飞上限。
pub async fn fetch_page_for_source_limited(
    url: &str,
    headers: &HashMap<String, String>,
    source_key: &str,
    timeout: Option<Duration>,
    limit: SourceLimit,
) -> anyhow::Result<PageResponse> {
    acquire_source_rate(source_key, limit).await?;
    let _source_permit = acquire_source_slot(source_key, limit.concurrency).await?;
    let _permit = acquire_permit().await?;
    let scope = cookie_scope(url, Some(source_key));
    let resp = execute_request(
        url,
        "GET",
        headers,
        None,
        &scope,
        false,
        true,
        MAX_REDIRECTS,
        timeout,
    )
    .await
    .map_err(|(_, e)| e)?;
    let content_type = resp.content_type.clone();
    let body = decode_body(&resp.body, content_type.as_deref())?;
    Ok(PageResponse {
        status: resp.status.as_u16(),
        content_type,
        body,
    })
}
