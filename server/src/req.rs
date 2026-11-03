//! HTTP 请求层（AnalyzeUrl 等价封装）
//! 低资源约束：全进程复用同一个 reqwest Client（连接池/cookie 共享），
//! 全局并发信号量限制抓取数量，响应体按流式读取并硬性截断，避免大响应把内存吃满。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use futures::StreamExt;
use once_cell::sync::OnceCell;
use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode, Url};
use tokio::sync::Semaphore;

use crate::config::Config;

/// 预览抓取最多跟随的跳数（每跳都重新做一次私网校验）
const MAX_REDIRECTS: usize = 5;
/// 域名解析的独立预算，避免 DNS 卡住整个预览请求
const DNS_TIMEOUT_SECS: u64 = 5;
/// 普通抓取跟随的最大跳数（与浏览器常见的 10 跳一致）。
/// reqwest 默认就是 limited(10)，这里显式写出来，避免以后改默认值造成 301/302 断链。
const FOLLOW_REDIRECTS: usize = 10;
/// 默认 UA。品牌改为 Boranlib 后仍保留浏览器前缀：`Boranlib/0.1` 这类自报 UA
/// 会被部分站点直接判为爬虫并重置连接（快眼看书 botaodz / booksky 实测：
/// 浏览器 UA 200，自报 UA 被 RST），因此只在末尾追加产品标识。
const DEFAULT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Boranlib/0.1";

#[derive(Clone, Copy)]
struct HttpConf {
    concurrency: usize,
    timeout_secs: u64,
    search_timeout_secs: u64,
    max_body_bytes: usize,
}

const DEFAULT_CONF: HttpConf = HttpConf {
    concurrency: 8,
    timeout_secs: 15,
    search_timeout_secs: 10,
    max_body_bytes: 5 * 1024 * 1024,
};

static CONF: OnceCell<HttpConf> = OnceCell::new();
static CLIENT: OnceCell<Client> = OnceCell::new();
static SEM: OnceCell<Arc<Semaphore>> = OnceCell::new();

/// 由 main 在启动时调用一次；未初始化时使用默认预算（单测/工具函数场景）。
pub fn init(cfg: &Config) {
    let _ = CONF.set(HttpConf {
        concurrency: cfg.http_concurrency,
        timeout_secs: cfg.http_timeout_secs,
        search_timeout_secs: cfg.search_timeout_secs,
        max_body_bytes: cfg.http_max_body_bytes,
    });
}

fn conf() -> HttpConf {
    *CONF.get().unwrap_or(&DEFAULT_CONF)
}

/// 单源搜索预算：聚合搜索按“一个书源”整体计时，超时只丢该源。
pub fn search_budget() -> Duration {
    Duration::from_secs(conf().search_timeout_secs)
}

impl HttpConf {
    fn semaphore(self) -> &'static Arc<Semaphore> {
        SEM.get_or_init(|| Arc::new(Semaphore::new(self.concurrency)))
    }
}

fn default_headers() -> reqwest::header::HeaderMap {
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

fn client() -> &'static Client {
    CLIENT.get_or_init(|| {
        let c = conf();
        Client::builder()
            .timeout(Duration::from_secs(c.timeout_secs))
            .cookie_store(true)
            .user_agent(DEFAULT_UA)
            .default_headers(default_headers())
            .redirect(Policy::limited(FOLLOW_REDIRECTS))
            .build()
            .unwrap_or_else(|_| Client::new())
    })
}

/// 关闭自动重定向的客户端：SSRF 校验需要逐跳检查 Location，不能交给 reqwest 静默跟随。
fn checked_client() -> &'static Client {
    static CHECKED: OnceCell<Client> = OnceCell::new();
    CHECKED.get_or_init(|| {
        let c = conf();
        Client::builder()
            .timeout(Duration::from_secs(c.timeout_secs))
            .cookie_store(true)
            .user_agent(DEFAULT_UA)
            .default_headers(default_headers())
            .redirect(Policy::none())
            .build()
            .unwrap_or_else(|_| Client::new())
    })
}

async fn acquire_permit() -> anyhow::Result<tokio::sync::SemaphorePermit<'static>> {
    let c = conf();
    tokio::time::timeout(
        Duration::from_secs(c.timeout_secs),
        c.semaphore().acquire(),
    )
    .await
    .map_err(|_| anyhow!("fetch queue timeout after {}s", c.timeout_secs))?
    .map_err(|_| anyhow!("fetch semaphore closed"))
}

fn build_request(
    client: &Client,
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut rb = match method.to_uppercase().as_str() {
        "POST" => client.post(url),
        _ => client.get(url),
    };
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
async fn read_body_limited(
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

/// 把 reqwest 的发送错误分类成可读文案。
/// 此前一律 `request failed: {e}`，超时和非法 URL 都混在一起（预览里还会
/// 显示 `error sending request` / `builder error`），前端无法区分。
fn send_error(e: reqwest::Error, url: &str) -> anyhow::Error {
    if e.is_timeout() {
        anyhow!("请求超时（{}s 预算）: {url}", conf().timeout_secs)
    } else if e.is_builder() {
        anyhow!("非法请求地址: {url}")
    } else if e.is_connect() {
        anyhow!("连接失败: {url}（{e}）")
    } else {
        anyhow!("request failed: {e}（{url}）")
    }
}

/// 发起 HTTP 请求，返回响应文本。
/// 失败原因（超时/并发等待超时/响应超限）都会带明确文案，便于前端展示。
pub async fn fetch(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
) -> anyhow::Result<String> {
    let c = conf();
    let permit = acquire_permit().await?;

    let rb = build_request(client(), method, url, headers, body);
    let resp = rb.send().await.map_err(|e| send_error(e, url))?;
    let status = resp.status();
    let buf = read_body_limited(resp, c.max_body_bytes).await?;
    drop(permit);

    if !status.is_success() {
        return Err(anyhow!("http status {status}"));
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// ---------------- SSRF 防护（规则试跑预览专用） ----------------

/// 判定 IP 是否属于“不应由外部输入直接访问”的地址段：
/// IPv4 回环/私网/链路本地/CGNAT/广播/组播/未指定；IPv6 回环/ULA/链路本地/组播/未指定/IPv4-mapped。
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                // 100.64.0.0/10（运营商级 NAT）
                || (o[0] == 100 && (64..=127).contains(&o[1]))
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fc00::/7（ULA）
                || (seg[0] & 0xfe00) == 0xfc00
                // fe80::/10（link-local）
                || (seg[0] & 0xffc0) == 0xfe80
                // ::ffff:a.b.c.d（IPv4-mapped，递归按 IPv4 规则判断）
                || (seg[0] == 0
                    && seg[1] == 0
                    && seg[2] == 0
                    && seg[3] == 0
                    && seg[4] == 0
                    && seg[5] == 0xffff
                    && is_private_ip(IpAddr::V4(std::net::Ipv4Addr::new(
                        (seg[6] >> 8) as u8,
                        (seg[6] & 0xff) as u8,
                        (seg[7] >> 8) as u8,
                        (seg[7] & 0xff) as u8,
                    ))))
        }
    }
}

/// 主机名层面的初筛：localhost 系列、mDNS `.local`、云厂商 `.internal`，以及 IP 字面量。
pub fn is_private_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    if h.is_empty() {
        return true;
    }
    if h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    if h.ends_with(".local") || h.ends_with(".internal") {
        return true;
    }
    // IPv6 字面量在 URL 里可能带方括号
    let bare = h
        .strip_prefix('[')
        .and_then(|x| x.strip_suffix(']'))
        .unwrap_or(&h);
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return is_private_ip(ip);
    }
    false
}

/// 校验单个 URL：协议、主机名初筛 + DNS 解析后的每个 IP 都要是公网地址。
/// `allow_private` 为 true 时直接放行（本地调试书源用）。
pub async fn ensure_public_url(url: &Url, allow_private: bool) -> anyhow::Result<()> {
    if allow_private {
        return Ok(());
    }
    match url.scheme() {
        "http" | "https" => {}
        s => return Err(anyhow!("仅允许 http(s) URL，收到 {s}")),
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("URL 缺少 host"))?
        .to_string();
    if is_private_host(&host) {
        return Err(anyhow!("拒绝访问回环/私网/链路本地地址: {host}"));
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = tokio::time::timeout(
        Duration::from_secs(DNS_TIMEOUT_SECS),
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    .map_err(|_| anyhow!("域名解析超时（{DNS_TIMEOUT_SECS}s）：{host}"))?
    .map_err(|e| anyhow!("域名解析失败（{host}）：{e}"))?;
    let mut checked = false;
    for a in addrs {
        checked = true;
        if is_private_ip(a.ip()) {
            return Err(anyhow!("拒绝访问私网地址 {}（{host}）", a.ip()));
        }
    }
    if !checked {
        return Err(anyhow!("域名解析为空：{host}"));
    }
    Ok(())
}

/// 带 SSRF 防护的抓取：先校验 URL，再手动跟随最多 `MAX_REDIRECTS` 跳，每跳重新校验。
/// 与 `fetch` 共用并发信号量、超时、响应体上限。
pub async fn fetch_checked(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    allow_private: bool,
) -> anyhow::Result<String> {
    let c = conf();
    let permit = acquire_permit().await?;
    let parsed = Url::parse(url).map_err(|e| anyhow!("URL 解析失败: {e}"))?;

    let mut current = parsed;
    let mut method = method.to_uppercase();
    let mut body = body.map(|s| s.to_string());
    let mut result: Option<(StatusCode, Vec<u8>)> = None;

    for _ in 0..=MAX_REDIRECTS {
        ensure_public_url(&current, allow_private).await?;
        let rb = build_request(
            checked_client(),
            &method,
            current.as_str(),
            headers,
            body.as_deref(),
        );
        let resp = rb
            .send()
            .await
            .map_err(|e| send_error(e, current.as_str()))?;
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
                .map_err(|e| anyhow!("重定向地址非法: {e}"))?;
            continue;
        }
        let buf = read_body_limited(resp, c.max_body_bytes).await?;
        result = Some((status, buf));
        break;
    }

    drop(permit);
    let (status, buf) = result.ok_or_else(|| anyhow!("重定向超过 {MAX_REDIRECTS} 跳"))?;
    if !status.is_success() {
        return Err(anyhow!("http status {status}"));
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// 流式打开的响应：调用方逐块读取，不整图进内存。
pub struct StreamOpen {
    pub response: reqwest::Response,
    pub final_url: String,
}

/// 带 SSRF 防护的流式 GET（图片代理用）：逐跳校验重定向，返回未读尽的上游响应。
/// 非 2xx 也原样返回，由调用方区分 403/404/5xx；网络错误/超时已归类到错误信息。
pub async fn open_stream(
    url: &str,
    headers: &HashMap<String, String>,
    allow_private: bool,
) -> anyhow::Result<StreamOpen> {
    let permit = acquire_permit().await?;
    let parsed = Url::parse(url).map_err(|e| anyhow!("URL 解析失败: {e}"))?;
    let mut current = parsed;
    for _ in 0..=MAX_REDIRECTS {
        ensure_public_url(&current, allow_private).await?;
        let resp = build_request(checked_client(), "GET", current.as_str(), headers, None)
            .send()
            .await
            .map_err(|e| send_error(e, current.as_str()))?;
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
        });
    }
    Err(anyhow!("重定向超过 {MAX_REDIRECTS} 跳"))
}

/// POST 携带 body 且未显式声明 `Content-Type` 时补默认表单头（Legado AnalyzeUrl 等价行为）。
fn ensure_form_content_type(
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

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

    // B2：`method: POST` 且书源未声明 header 时，必须默认补表单 Content-Type
    #[test]
    fn post_body_defaults_to_form_content_type() {
        let mut hm = HeaderMap::new();
        ensure_form_content_type(&mut hm, "POST", true);
        assert_eq!(
            hm.get(CONTENT_TYPE).unwrap(),
            "application/x-www-form-urlencoded"
        );
    }

    // 书源显式声明的 Content-Type 不能被覆盖（顶点小说会自己带 charset）
    #[test]
    fn explicit_content_type_is_not_overridden() {
        let mut hm = HeaderMap::new();
        hm.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded; charset=UTF-8"),
        );
        ensure_form_content_type(&mut hm, "post", true);
        assert_eq!(
            hm.get(CONTENT_TYPE).unwrap(),
            "application/x-www-form-urlencoded; charset=UTF-8"
        );
    }

    // GET/无 body 不需要补
    #[test]
    fn get_or_empty_body_gets_no_content_type() {
        let mut hm = HeaderMap::new();
        ensure_form_content_type(&mut hm, "GET", true);
        assert!(hm.get(CONTENT_TYPE).is_none());
        let mut hm2 = HeaderMap::new();
        ensure_form_content_type(&mut hm2, "POST", false);
        assert!(hm2.get(CONTENT_TYPE).is_none());
    }

    // SSRF：IPv4 回环/私网/链路本地/CGNAT/组播/广播/未指定必须拒绝
    #[test]
    fn private_ipv4_ranges_are_rejected() {
        use std::net::Ipv4Addr;
        for ip in [
            "127.0.0.1",
            "127.0.1.1",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "100.127.255.255",
            "224.0.0.1",
            "255.255.255.255",
            "0.0.0.0",
        ] {
            let parsed: Ipv4Addr = ip.parse().unwrap();
            assert!(
                is_private_ip(IpAddr::V4(parsed)),
                "{ip} 应判定为私网/受限地址"
            );
        }
        // 边界外的地址不应误伤
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "172.15.0.1",
            "172.32.0.1",
            "100.63.0.1",
            "100.128.0.1",
        ] {
            let parsed: Ipv4Addr = ip.parse().unwrap();
            assert!(!is_private_ip(IpAddr::V4(parsed)), "{ip} 不应被拒绝");
        }
    }

    // SSRF：IPv6 回环/ULA/链路本地/组播/IPv4-mapped 一律拒绝
    #[test]
    fn private_ipv6_ranges_are_rejected() {
        for ip in [
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.1.1",
        ] {
            let parsed: IpAddr = ip.parse().unwrap();
            assert!(is_private_ip(parsed), "{ip} 应判定为私网/受限地址");
        }
        for ip in ["2001:db8::1", "::ffff:8.8.8.8"] {
            let parsed: IpAddr = ip.parse().unwrap();
            assert!(!is_private_ip(parsed), "{ip} 不应被拒绝");
        }
    }

    // 主机名初筛：localhost 系列、.local/.internal、IP 字面量（含 IPv6 方括号）
    #[test]
    fn private_hosts_are_screened() {
        for host in [
            "localhost",
            "LOCALHOST",
            "foo.localhost",
            "foo.local",
            "api.internal",
            "127.0.0.1",
            "[::1]",
            "10.0.0.1",
        ] {
            assert!(is_private_host(host), "{host} 应被拒绝");
        }
        for host in ["example.com", "8.8.8.8", "foo.bar.example.com"] {
            assert!(!is_private_host(host), "{host} 不应被拒绝");
        }
    }

    #[tokio::test]
    async fn ensure_public_url_rejects_private_targets_without_network() {
        for url in [
            "http://127.0.0.1/x",
            "http://localhost/x",
            "http://[::1]/x",
            "https://192.168.1.5/x",
            "http://169.254.1.1/x",
        ] {
            let parsed = Url::parse(url).unwrap();
            assert!(
                ensure_public_url(&parsed, false).await.is_err(),
                "{url} 应被拒绝"
            );
        }
        // 非 http(s) 协议直接拒绝
        let ftp = Url::parse("ftp://example.com/x").unwrap();
        assert!(ensure_public_url(&ftp, false).await.is_err());
        // 放行开关：本地调试书源时允许私网
        let local = Url::parse("http://127.0.0.1/x").unwrap();
        assert!(ensure_public_url(&local, true).await.is_ok());
    }

    // P0-3 回归：301 必须被跟随（botaodz/booksky 先 301 到新域名再 200）
    #[tokio::test]
    async fn fetch_follows_301_redirect() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..3 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 512];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let resp = if req.starts_with("GET /a ") {
                    "HTTP/1.1 301 Moved Permanently\r\nLocation: /b\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
                };
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        let out = fetch(&format!("http://{addr}/a"), "GET", &HashMap::new(), None)
            .await
            .unwrap();
        assert_eq!(out, "hello", "301 应自动跟随到最终 200");
    }

    // P2 文案：超时必须归到“请求超时”，而不是笼统的 error sending request
    #[tokio::test]
    async fn timeout_is_reported_as_timeout() {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((_sock, _)) = listener.accept().await {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
        let cli = Client::builder()
            .timeout(Duration::from_millis(300))
            .build()
            .unwrap();
        let err = cli
            .get(format!("http://{addr}/slow"))
            .send()
            .await
            .unwrap_err();
        assert!(err.is_timeout(), "前置条件：应触发超时");
        let msg = send_error(err, "http://example/slow").to_string();
        assert!(msg.contains("请求超时"), "超时文案应可识别: {msg}");
    }
}
