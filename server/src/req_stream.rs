//! req 的流式响应、表单与请求头（move-only 从 req.rs 拆出）。

use super::*;

/// 流式打开的响应：调用方逐块读取，不整图进内存。
pub struct StreamOpen {
    pub response: reqwest::Response,
    // 重定向后的最终地址；当前调用方只用 response，字段保留给诊断输出。
    #[allow(dead_code)]
    pub final_url: String,
    /// 按源在飞许可：随 `StreamOpen` 一起存活到响应体读完，避免同一源被并发打满。
    pub(super) _source_permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

/// 带 SSRF 防护的流式 GET（图片代理用）：逐跳校验重定向，返回未读尽的上游响应。
/// 非 2xx 也原样返回，由调用方区分 403/404/5xx；网络错误/超时已归类到错误信息。
/// 生产路径使用带超时的 `open_stream_timed`；本封装保留默认超时语义。
#[allow(dead_code)]
pub async fn open_stream(
    url: &str,
    headers: &HashMap<String, String>,
    allow_private: bool,
) -> anyhow::Result<StreamOpen> {
    open_stream_timed(url, headers, allow_private, None).await
}

/// 与 [`open_stream`] 相同，但可指定单源超时（`None` 回退全局默认）。
pub async fn open_stream_timed(
    url: &str,
    headers: &HashMap<String, String>,
    allow_private: bool,
    timeout: Option<Duration>,
) -> anyhow::Result<StreamOpen> {
    open_stream_for_source_timed(
        url,
        headers,
        allow_private,
        timeout,
        None,
        SourceLimit::concurrency_only(source_concurrency_default()),
    )
    .await
}

/// 与 [`open_stream_timed`] 相同，但按书源限流（图片代理等多请求场景）。
pub async fn open_stream_for_source_timed(
    url: &str,
    headers: &HashMap<String, String>,
    allow_private: bool,
    timeout: Option<Duration>,
    source_key: Option<&str>,
    limit: SourceLimit,
) -> anyhow::Result<StreamOpen> {
    let source_permit = match source_key {
        Some(k) => {
            acquire_source_rate(k, limit).await?;
            Some(acquire_source_slot(k, limit.concurrency).await?)
        }
        None => None,
    };
    let permit = acquire_permit().await?;
    let parsed = Url::parse(url).map_err(|e| anyhow!("URL 解析失败: {e}"))?;
    let mut current = parsed;
    for _ in 0..=MAX_REDIRECTS {
        ensure_public_url(&current, allow_private).await?;
        let resp = build_request(
            checked_client(),
            "GET",
            current.as_str(),
            headers,
            None,
            timeout,
        )
        .send()
        .await
        .map_err(|e| send_failure(e, current.as_str(), timeout).1)?;
        let status = resp.status();
        if matches!(
            status,
            StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT
        ) {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow!("重定向缺少 Location"))?;
            current = current
                .join(&loc)
                .map_err(|e| anyhow!("重定向地址非法: {e}"))?;
            continue;
        }
        drop(permit);
        return Ok(StreamOpen {
            response: resp,
            final_url: current.to_string(),
            _source_permit: source_permit,
        });
    }
    Err(anyhow!("重定向超过 {MAX_REDIRECTS} 跳"))
}

/// POST 携带 body 且未显式声明 `Content-Type` 时补默认表单头（Legado AnalyzeUrl 等价行为）。
pub(super) fn ensure_form_content_type(
    hm: &mut reqwest::header::HeaderMap,
    method: &str,
    has_body: bool,
) {
    if has_body
        && method.eq_ignore_ascii_case("POST")
        && !hm.contains_key(reqwest::header::CONTENT_TYPE)
    {
        hm.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
    }
}

/// 解析书源 header 字段：优先作为 JSON 对象解析（Legado 书源 header 为 JSON），
/// 失败则回退为多行 `key: value` 格式。
pub fn parse_headers(header: &Option<String>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(h) = header {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(h) {
            if let Some(obj) = v.as_object() {
                for (k, val) in obj {
                    if let Some(s) = val.as_str() {
                        map.insert(k.clone(), s.to_string());
                    }
                }
                return map;
            }
        }
        for line in h.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    map
}

/// 按书源解析请求头：普通 JSON/多行形式直接解析；`@js:` 形式先求值再解析。
/// JS 求值失败时回退为空表（沿用浏览器 UA 兜底），不阻断整源请求。
pub fn source_headers(source: &BookSource, base_url: &str) -> HashMap<String, String> {
    let mut headers = match source.header.as_deref() {
        Some(raw) if raw.trim().starts_with("@js:") => {
            let mut ar = crate::rule_engine::AnalyzeRule::new();
            ar.js_lib = source.js_lib.clone().unwrap_or_default();
            ar.source_key.replace(source.book_source_url.clone());
            ar.source_login_url
                .replace(source.login_url.clone().unwrap_or_default());
            // 保留 `@js:` 前缀交给规则引擎识别（它负责把 JS 段拆出来执行）
            match ar.get_string("", raw.trim(), false, base_url) {
                Ok(s) if !s.trim().is_empty() => parse_headers(&Some(s)),
                _ => HashMap::new(),
            }
        }
        Some(raw) => parse_headers(&Some(raw.to_string())),
        None => HashMap::new(),
    };
    // 漫画源大多是移动站：未显式声明 UA 时补移动端 UA，避免只拿到桌面 JS 框架页
    if source.book_source_type == 2 && !headers.keys().any(|k| k.eq_ignore_ascii_case("user-agent"))
    {
        headers.insert(
            "User-Agent".to_string(),
            "Mozilla/5.0 (Linux; Android 10; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/78.0.3904.108 Mobile Safari/537.36".to_string(),
        );
    }
    headers
}
