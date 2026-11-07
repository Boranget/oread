//! req 的Client、请求构造与失败分类（move-only 从 req.rs 拆出）。

use super::*;

impl HttpConf {
    pub(super) fn semaphore(self) -> &'static Arc<Semaphore> {
        SEM.get_or_init(|| Arc::new(Semaphore::new(self.concurrency)))
    }
}

pub(super) fn default_headers() -> reqwest::header::HeaderMap {
    let mut hm = reqwest::header::HeaderMap::new();
    hm.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        ),
    );
    hm.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        reqwest::header::HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.6"),
    );
    hm
}

/// 统一客户端：重定向由请求层手动逐跳跟随（为了逐跳注入/回写按源 Cookie），
/// 因此关闭 reqwest 自带的 redirect 与 cookie store（后者无上限且跨书源共享）。
pub(super) fn client() -> &'static Client {
    CLIENT.get_or_init(|| {
        // 客户端只保留最大兜底；每次请求在 `build_request` 里显式设置
        // “源级超时或全局默认”，避免单源超时受全局 15s 上限截断。
        Client::builder()
            .timeout(Duration::from_millis(SOURCE_TIMEOUT_MAX_MS as u64))
            .user_agent(DEFAULT_UA)
            .default_headers(default_headers())
            .redirect(Policy::none())
            .build()
            .unwrap_or_else(|_| Client::new())
    })
}

/// 兼容旧调用点：与 `client()` 同配置（历史上用于关闭自动重定向）。
pub(super) fn checked_client() -> &'static Client {
    client()
}

pub(super) async fn acquire_permit() -> anyhow::Result<tokio::sync::SemaphorePermit<'static>> {
    let c = conf();
    tokio::time::timeout(Duration::from_secs(c.timeout_secs), c.semaphore().acquire())
        .await
        .map_err(|_| anyhow!("fetch queue timeout after {}s", c.timeout_secs))?
        .map_err(|_| anyhow!("fetch semaphore closed"))
}

/// 构造请求并设置本次请求的超时：`timeout=None` 时用全局 `HTTP_TIMEOUT_SECS`，
/// 让“未设置单源超时”的请求行为与历史一致。
pub(super) fn build_request(
    client: &Client,
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    timeout: Option<Duration>,
) -> reqwest::RequestBuilder {
    let budget = timeout.unwrap_or_else(|| Duration::from_secs(conf().timeout_secs));
    let mut rb = match method.to_uppercase().as_str() {
        "POST" => client.post(url),
        "HEAD" => client.head(url),
        _ => client.get(url),
    }
    .timeout(budget);
    let mut hm = reqwest::header::HeaderMap::new();
    for (k, v) in headers {
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            hm.insert(name, val);
        }
    }
    if body.is_some() {
        // POST 带 body 且书源未显式声明 Content-Type 时，按 Legado 语义默认按表单提交；
        // 否则站点不解析请求体，搜索会拿到空结果页（得奇小说网实测）。
        ensure_form_content_type(&mut hm, method, true);
    }
    rb = rb.headers(hm);
    if let Some(b) = body {
        rb = rb.body(b.to_string());
    }
    rb
}

/// 流式读取响应体，边读边判上限；超限立即返回，不把整个响应缓冲进内存。
pub(super) async fn read_body_limited(
    resp: reqwest::Response,
    max_body_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    if let Some(len) = resp.content_length() {
        if len as usize > max_body_bytes {
            return Err(anyhow!(
                "response too large: {} bytes (limit {} bytes)",
                len,
                max_body_bytes
            ));
        }
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow!("read body failed: {e}"))?;
        if buf.len() + chunk.len() > max_body_bytes {
            return Err(anyhow!(
                "response body exceeds {} bytes limit",
                max_body_bytes
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// 书源侧故障标记：连接失败/超时/HTTP 非成功等“站点不可用”。
/// 与规则引擎故障（选择器/JSONPath/JS）区分，供书源健康状态判定使用。
#[derive(Debug)]
pub struct SourceUnavailable(pub String);

impl std::fmt::Display for SourceUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SourceUnavailable {}

/// 发送错误分类：决定是否值得重试。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum SendKind {
    /// 连接建立失败（DNS/拒连/RST）：重试无意义
    Connect,
    /// 超时：按现有预算，不重试
    Timeout,
    /// URL/构造错误：不重试
    Builder,
    Other,
}

/// 把 reqwest 的发送错误分类成可读文案。
/// 此前一律 `request failed: {e}`，超时和非法 URL 都混在一起（预览里还会
/// 显示 `error sending request` / `builder error`），前端无法区分。
pub(super) fn send_failure(
    e: reqwest::Error,
    url: &str,
    timeout: Option<Duration>,
) -> (SendKind, anyhow::Error) {
    if e.is_timeout() {
        let label = match timeout {
            Some(d) if d.as_millis() % 1000 != 0 => format!("{}ms", d.as_millis()),
            Some(d) => format!("{}s", d.as_secs()),
            None => format!("{}s", conf().timeout_secs),
        };
        (
            SendKind::Timeout,
            anyhow!("请求超时（{label} 预算）: {url}"),
        )
    } else if e.is_builder() {
        (SendKind::Builder, anyhow!("非法请求地址: {url}"))
    } else if e.is_connect() {
        (SendKind::Connect, anyhow!("连接失败: {url}（{e}）"))
    } else {
        (SendKind::Other, anyhow!("请求失败: {e}（{url}）"))
    }
}

#[cfg(test)]
pub(super) fn send_error(e: reqwest::Error, url: &str) -> anyhow::Error {
    send_failure(e, url, None).1
}
