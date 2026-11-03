//! HTTP 请求层（AnalyzeUrl 等价封装）
//! 低资源约束：全进程复用同一个 reqwest Client，全局并发信号量限制抓取数量，
//! 响应体按流式读取并硬性截断，避免大响应把内存吃满。
//!
//! 请求按书源隔离：
//! - `header` 字段（JSON / 多行 / `@js:`）由 `source_headers` 解析后注入每一次请求；
//! - Cookie 用进程内小 LRU 按 `bookSourceUrl` 分桶，桶数 ≤64、单桶 ≤8KB，支持 Set-Cookie 回写，不落盘；
//! - 响应体按 Content-Type / meta charset / 字节嗅探解码（UTF-8、GBK、GB18030、Big5），
//!   解码失败给出具名错误，不做有损替换；
//! - 重试分级：连接失败与超时不重试，5xx 重试 1 次。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use encoding_rs::{Encoding, BIG5, GB18030};
use futures::StreamExt;
use once_cell::sync::{Lazy, OnceCell};
use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode, Url};
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::entities::BookSource;

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

/// Cookie 桶数量上限（按 bookSourceUrl 分桶）
const COOKIE_MAX_BUCKETS: usize = 64;
/// 单桶 Cookie 字节上限（名字 + 值 + 分隔符）
const COOKIE_BUCKET_MAX_BYTES: usize = 8 * 1024;

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

/// 统一客户端：重定向由请求层手动逐跳跟随（为了逐跳注入/回写按源 Cookie），
/// 因此关闭 reqwest 自带的 redirect 与 cookie store（后者无上限且跨书源共享）。
fn client() -> &'static Client {
    CLIENT.get_or_init(|| {
        let c = conf();
        Client::builder()
            .timeout(Duration::from_secs(c.timeout_secs))
            .user_agent(DEFAULT_UA)
            .default_headers(default_headers())
            .redirect(Policy::none())
            .build()
            .unwrap_or_else(|_| Client::new())
    })
}

/// 兼容旧调用点：与 `client()` 同配置（历史上用于关闭自动重定向）。
fn checked_client() -> &'static Client {
    client()
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

/// 发送错误分类：决定是否值得重试。
#[derive(Clone, Copy, Debug, PartialEq)]
enum SendKind {
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
fn send_failure(e: reqwest::Error, url: &str) -> (SendKind, anyhow::Error) {
    if e.is_timeout() {
        (
            SendKind::Timeout,
            anyhow!("请求超时（{}s 预算）: {url}", conf().timeout_secs),
        )
    } else if e.is_builder() {
        (SendKind::Builder, anyhow!("非法请求地址: {url}"))
    } else if e.is_connect() {
        (SendKind::Connect, anyhow!("连接失败: {url}（{e}）"))
    } else {
        (SendKind::Other, anyhow!("请求失败: {e}（{url}）"))
    }
}

fn send_error(e: reqwest::Error, url: &str) -> anyhow::Error {
    send_failure(e, url).1
}

// ---------------- Cookie：按书源分桶的进程内 LRU ----------------

#[derive(Default)]
struct CookieBucket {
    /// 保持最近更新顺序；超出单桶上限时从最旧的条目开始淘汰
    entries: Vec<(String, String)>,
    bytes: usize,
    touched: u64,
}

#[derive(Default)]
struct CookieStore {
    buckets: HashMap<String, CookieBucket>,
    clock: u64,
}

static COOKIE_JAR: Lazy<Mutex<CookieStore>> = Lazy::new(|| Mutex::new(CookieStore::default()));

/// Cookie 分桶键：优先用书源唯一 ID（bookSourceUrl），没有时退化为目标 host。
fn cookie_scope(url: &str, source_key: Option<&str>) -> String {
    if let Some(k) = source_key.map(str::trim).filter(|k| !k.is_empty()) {
        return format!("source:{k}");
    }
    match Url::parse(url) {
        Ok(u) => format!("host:{}", u.host_str().unwrap_or_default()),
        Err(_) => "host:".to_string(),
    }
}

fn cookie_pairs(scope: &str) -> Vec<(String, String)> {
    let mut store = COOKIE_JAR.lock().unwrap_or_else(|e| e.into_inner());
    store.clock = store.clock.wrapping_add(1);
    let tick = store.clock;
    let Some(b) = store.buckets.get_mut(scope) else {
        return Vec::new();
    };
    b.touched = tick;
    b.entries.clone()
}

fn cookies_to_header(pairs: &[(String, String)]) -> Option<String> {
    if pairs.is_empty() {
        return None;
    }
    Some(
        pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// 取出当前会话的 Cookie 请求头（`a=b; c=d`），无内容时返回 None。
fn cookie_header_for(scope: &str) -> Option<String> {
    cookies_to_header(&cookie_pairs(scope))
}

/// 实际请求用的 Cookie：书源桶优先，再合并目标 host 桶（js/ajax、图片等按 host 发起的
/// 子请求也能复用同一站点会话）。同名时书源桶的值优先。
fn cookie_header_for_request(scope: &str, url: &str) -> Option<String> {
    let mut pairs = cookie_pairs(scope);
    let host = cookie_scope(url, None);
    if host != scope {
        for (k, v) in cookie_pairs(&host) {
            if !pairs.iter().any(|(n, _)| n == &k) {
                pairs.push((k, v));
            }
        }
    }
    cookies_to_header(&pairs)
}

/// 解析 Set-Cookie 的首段：返回 (名字, 值, 是否删除)。
fn parse_set_cookie(raw: &str) -> Option<(String, String, bool)> {
    let mut parts = raw.split(';');
    let first = parts.next()?.trim();
    let (name, val) = first.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let value = val.trim().trim_matches('"').to_string();
    let mut delete = false;
    for attr in parts {
        let lower = attr.trim().to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("max-age=") {
            if let Ok(n) = rest.trim().parse::<i64>() {
                if n <= 0 {
                    delete = true;
                }
            }
        }
    }
    Some((name.to_string(), value, delete))
}

fn remove_cookie_entry(bucket: &mut CookieBucket, name: &str) {
    if let Some(pos) = bucket.entries.iter().position(|(k, _)| k == name) {
        let (k, v) = bucket.entries.remove(pos);
        bucket.bytes = bucket.bytes.saturating_sub(k.len() + v.len() + 2);
    }
}

/// 回写一条 Set-Cookie：同名覆盖；超出单桶 8KB 时按插入顺序淘汰旧条目。
fn record_set_cookie(scope: &str, raw: &str) {
    let Some((name, value, delete)) = parse_set_cookie(raw) else {
        return;
    };
    let mut store = COOKIE_JAR.lock().unwrap_or_else(|e| e.into_inner());
    store.clock = store.clock.wrapping_add(1);
    let tick = store.clock;
    let bucket = store.buckets.entry(scope.to_string()).or_default();
    bucket.touched = tick;
    remove_cookie_entry(bucket, &name);
    if delete {
        return;
    }
    let add = name.len() + value.len() + 2;
    if add > COOKIE_BUCKET_MAX_BYTES {
        return;
    }
    while bucket.bytes + add > COOKIE_BUCKET_MAX_BYTES && !bucket.entries.is_empty() {
        let (k, v) = bucket.entries.remove(0);
        bucket.bytes = bucket.bytes.saturating_sub(k.len() + v.len() + 2);
    }
    bucket.entries.push((name, value));
    bucket.bytes += add;
    // 桶数量上限：淘汰最久未访问的桶（新写入的桶 touched 最大，不会被自己淘汰）
    if store.buckets.len() > COOKIE_MAX_BUCKETS {
        if let Some(oldest) = store
            .buckets
            .iter()
            .min_by_key(|(_, b)| b.touched)
            .map(|(k, _)| k.clone())
        {
            store.buckets.remove(&oldest);
        }
    }
}

fn record_response_cookies(scope: &str, headers: &reqwest::header::HeaderMap) {
    for raw in headers.get_all(reqwest::header::SET_COOKIE).iter() {
        if let Ok(s) = raw.to_str() {
            record_set_cookie(scope, s);
        }
    }
}

#[cfg(test)]
fn cookie_reset() {
    let mut store = COOKIE_JAR.lock().unwrap_or_else(|e| e.into_inner());
    store.buckets.clear();
    store.clock = 0;
}

// ---------------- 响应解码 ----------------

/// 从 Content-Type 里取 charset（大小写不敏感，允许引号）。
fn charset_from_content_type(ct: &str) -> Option<String> {
    let lower = ct.to_ascii_lowercase();
    let pos = lower.find("charset=")?;
    let rest = ct[pos + "charset=".len()..].trim_start();
    let rest = rest.trim_start_matches('"').trim_start_matches('\'');
    let end = rest
        .find(|c: char| c == ';' || c == '"' || c == '\'' || c.is_whitespace())
        .unwrap_or(rest.len());
    let label = rest[..end].trim();
    if label.is_empty() {
        None
    } else {
        Some(label.to_string())
    }
}

/// 从 HTML 头部（前 4KB）嗅探 `<meta charset=...>`；覆盖无 Content-Type charset 的站点。
fn meta_charset(buf: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(&buf[..buf.len().min(4096)]);
    let lower = text.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find("charset=") {
        let pos = from + rel + "charset=".len();
        let rest = &text[pos..];
        let rest = rest.trim_start_matches('"').trim_start_matches('\'');
        let end = rest
            .find(|c: char| c == ';' || c == '"' || c == '\'' || c == '>' || c == '/')
            .unwrap_or(rest.len());
        let label = rest[..end].trim();
        if !label.is_empty() {
            return Some(label.to_string());
        }
        from = pos;
    }
    None
}

fn encoding_for_label(label: &str) -> Option<&'static Encoding> {
    Encoding::for_label(
        label
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .as_bytes(),
    )
}

fn decode_with(enc: &'static Encoding, buf: &[u8]) -> (String, bool) {
    let (cow, _, had_errors) = enc.decode(buf);
    (cow.into_owned(), had_errors)
}

/// 常见汉字（简/繁）集合：无 charset 时用“常用字命中率”区分 GB18030 与 Big5。
/// 正确解码得到的正文里高频字占比明显更高，错配编码只会得到零散生僻字。
const COMMON_HAN: &str = "的一是在不了有和人这中大为上个国我以要他时来用们生到作地于出就分对成会可主发年动同工也能下过子说产种面而方后多定行学法所民得经十三之进着等部度家电力里如水化高自二理起小物现实加量都两体制机当使点从业本去把性好应开它合还因由其些然前外天政四日那社义事平形相全表间样与关各重新线内数正心反你明看原又么利比或但质气第向道命此变条只没结解问意建月公无系军很情者最立代想已通并提直题程展果料象员位入常文总次品式活设及管特件长求老头基资边流路级少图山统接知较将组见计别手角期根论运农指几九区强放决西被干做必战先回则任取据处理世车清器美收千称德精板精這們來個國為時會說產種對開關聯實體機發現應該還從業東動過學機關電點錢書語運進連樣見處禮樂龍馬鳥魚車長門問間陽風飛養習寫讀聽覺氣華萬歲廣";

/// 文本可信度：ASCII / 常用汉字权重更高；替换字符（U+FFFD）直接拉低分数。
fn plausibility(text: &str) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let mut good = 0f64;
    let mut total = 0usize;
    for ch in text.chars() {
        total += 1;
        if ch == '\u{FFFD}' {
            continue;
        }
        if ch.is_ascii() && !ch.is_control() {
            good += 1.0;
        } else if ('\u{4E00}'..='\u{9FFF}').contains(&ch) {
            good += if COMMON_HAN.contains(ch) { 3.0 } else { 1.0 };
        } else if ('\u{3000}'..='\u{303F}').contains(&ch)
            || ('\u{FF00}'..='\u{FFEF}').contains(&ch)
        {
            good += 2.0;
        }
    }
    good / total as f64
}

/// 解码响应体：Content-Type charset 优先，其次 meta charset，最后 UTF-8/GB18030/Big5 嗅探。
/// 解码失败一律具名报错，绝不 `from_utf8_lossy` 静默出乱码。
fn decode_body(buf: &[u8], content_type: Option<&str>) -> anyhow::Result<String> {
    if buf.is_empty() {
        return Ok(String::new());
    }
    let declared = content_type
        .and_then(charset_from_content_type)
        .or_else(|| meta_charset(buf));
    if let Some(label) = declared {
        let Some(enc) = encoding_for_label(&label) else {
            return Err(anyhow!("响应解码失败：不支持的字符集 {label}"));
        };
        let (text, had_errors) = decode_with(enc, buf);
        if had_errors {
            return Err(anyhow!(
                "响应解码失败：按 charset={label} 解码出现非法字节（内容长度 {} 字节）",
                buf.len()
            ));
        }
        return Ok(text);
    }
    // 无 charset：先做严格 UTF-8（最可靠），失败再在 GB18030 / Big5 之间择优
    if let Ok(s) = std::str::from_utf8(buf) {
        return Ok(s.to_string());
    }
    let mut best: Option<(f64, &'static Encoding, String)> = None;
    for enc in [GB18030, BIG5] {
        let (text, had_errors) = decode_with(enc, buf);
        let score = plausibility(&text) - if had_errors { 1.0 } else { 0.0 };
        if best.as_ref().map(|(b, _, _)| score > *b).unwrap_or(true) {
            best = Some((score, enc, text));
        }
    }
    match best {
        Some((score, _, text)) if score > 0.0 => Ok(text),
        _ => Err(anyhow!(
            "响应解码失败：未声明 charset，且字节流不符合 UTF-8/GB18030/Big5"
        )),
    }
}

// ---------------- 请求执行（逐跳手动跟随 + 按源 Cookie） ----------------

struct HttpResponse {
    status: StatusCode,
    body: Vec<u8>,
    content_type: Option<String>,
}

type AttemptError = (SendKind, anyhow::Error);

fn is_redirect(status: StatusCode) -> bool {
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
async fn execute_request(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    cookie_key: &str,
    allow_private: bool,
    validate_private: bool,
    max_redirects: usize,
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
        if !req_headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("cookie"))
        {
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
        )
        .send()
        .await
        .map_err(|e| send_failure(e, current.as_str()))?;
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
    Err((
        SendKind::Other,
        anyhow!("重定向超过 {max_redirects} 跳"),
    ))
}

/// 统一的文本抓取入口：按源 Cookie 分桶 + 分级重试 + 具名解码。
async fn fetch_text(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    source_key: Option<&str>,
    allow_private: bool,
    validate_private: bool,
) -> anyhow::Result<String> {
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
                    return Err(anyhow!("http status {}", resp.status));
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
pub async fn fetch(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
) -> anyhow::Result<String> {
    fetch_text(url, method, headers, body, None, false, false).await
}

/// 按书源发起请求：Cookie 按 `bookSourceUrl` 分桶，其余与 `fetch` 一致。
pub async fn fetch_for_source(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    source_key: &str,
) -> anyhow::Result<String> {
    fetch_text(url, method, headers, body, Some(source_key), false, false).await
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
    fetch_text(url, method, headers, body, None, allow_private, true).await
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

/// 按书源解析请求头：普通 JSON/多行形式直接解析；`@js:` 形式先求值再解析。
/// JS 求值失败时回退为空表（沿用浏览器 UA 兜底），不阻断整源请求。
pub fn source_headers(source: &BookSource, base_url: &str) -> HashMap<String, String> {
    let mut headers = match source.header.as_deref() {
        Some(raw) if raw.trim().starts_with("@js:") => {
            let mut ar = crate::rule_engine::AnalyzeRule::new();
            ar.js_lib = source.js_lib.clone().unwrap_or_default();
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
    if source.book_source_type == 2
        && !headers.keys().any(|k| k.eq_ignore_ascii_case("user-agent"))
    {
        headers.insert(
            "User-Agent".to_string(),
            "Mozilla/5.0 (Linux; Android 10; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/78.0.3904.108 Mobile Safari/537.36".to_string(),
        );
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

    // 生产代码的 Cookie 存储是进程级全局单例（COOKIE_JAR），三个 cookie 测试
    // 都会先 cookie_reset() 再断言桶内容/桶数量。并发执行时一个测试的清空
    // 会抹掉另一个测试刚写入的桶，导致“LRU 淘汰后桶数应等于上限”随机失败。
    // 这里给共享单例加锁串行访问；测试 panic 后仍能继续（从 poison 恢复）。
    static COOKIE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_cookie_jar() -> std::sync::MutexGuard<'static, ()> {
        COOKIE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

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

    // ---------------- 编码嗅探 / Cookie / 重试分级 ----------------

    #[test]
    fn decode_gb18030_without_charset() {
        let (bytes, _, _) = GB18030.encode("中文测试：第一章");
        let out = decode_body(&bytes, Some("text/html")).unwrap();
        assert_eq!(out, "中文测试：第一章");
    }

    #[test]
    fn decode_big5_without_charset() {
        let (bytes, _, _) = BIG5.encode("繁體中文測試");
        let out = decode_body(&bytes, Some("text/html")).unwrap();
        assert_eq!(out, "繁體中文測試");
    }

    #[test]
    fn decode_uses_declared_charset() {
        let (bytes, _, _) = GB18030.encode("书源编码声明");
        let out = decode_body(&bytes, Some("text/html; charset=gbk")).unwrap();
        assert_eq!(out, "书源编码声明");
    }

    #[test]
    fn decode_uses_meta_charset() {
        let html = "<html><head><meta charset=\"gb2312\"></head><body>中文</body></html>";
        let (bytes, _, _) = GB18030.encode(html);
        let out = decode_body(&bytes, Some("text/html")).unwrap();
        assert_eq!(out, html);
    }

    #[test]
    fn decode_invalid_declared_charset_is_named_error() {
        let err = decode_body(&[0xC3, 0x28], Some("text/html; charset=utf-8"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("响应解码失败"), "实际: {err}");
        assert!(err.contains("utf-8"), "应点名字符集，实际: {err}");
    }

    #[test]
    fn decode_binary_garbage_is_named_error() {
        let err = decode_body(&[0x81, 0x20, 0x81, 0x20], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("响应解码失败"), "实际: {err}");
    }

    #[test]
    fn cookie_set_and_overwrite_roundtrip() {
        let _guard = lock_cookie_jar();
        cookie_reset();
        let scope = "source:https://cookie-test.example";
        record_set_cookie(scope, "sid=abc; Path=/; HttpOnly");
        record_set_cookie(scope, "token=xyz; Path=/");
        assert_eq!(
            cookie_header_for(scope).as_deref(),
            Some("sid=abc; token=xyz")
        );
        record_set_cookie(scope, "sid=def; Path=/");
        // 同名覆盖后按“最近更新”排序，值必须是最新的
        let header = cookie_header_for(scope).unwrap();
        assert!(header.contains("sid=def"), "实际: {header}");
        assert!(header.contains("token=xyz"), "实际: {header}");
        record_set_cookie(scope, "sid=; Max-Age=0");
        assert_eq!(cookie_header_for(scope).as_deref(), Some("token=xyz"));
    }

    #[test]
    fn cookie_bucket_evicts_lru_bucket() {
        let _guard = lock_cookie_jar();
        cookie_reset();
        for i in 0..COOKIE_MAX_BUCKETS {
            record_set_cookie(&format!("source:lru-{i}"), &format!("c{i}=v{i}"));
        }
        record_set_cookie("source:lru-extra", "c=v");
        assert!(cookie_header_for("source:lru-0").is_none());
        assert!(cookie_header_for("source:lru-extra").is_some());
        assert_eq!(
            COOKIE_JAR
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .buckets
                .len(),
            COOKIE_MAX_BUCKETS
        );
    }

    #[test]
    fn cookie_bucket_respects_8kb_limit() {
        let _guard = lock_cookie_jar();
        cookie_reset();
        let scope = "source:https://cookie-bytes.example";
        for i in 0..12 {
            record_set_cookie(scope, &format!("k{i}={}", "x".repeat(1000)));
        }
        let header = cookie_header_for(scope).unwrap();
        assert!(
            header.len() <= COOKIE_BUCKET_MAX_BYTES,
            "单桶字节数超限: {}",
            header.len()
        );
        assert!(header.contains("k11="), "最新条目应保留: {header}");
        assert!(!header.contains("k0="), "最旧条目应被淘汰: {header}");
    }

    #[tokio::test]
    async fn retries_once_on_5xx() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        static HITS: AtomicUsize = AtomicUsize::new(0);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..2 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 512];
                let _ = sock.read(&mut buf).await;
                let hit = HITS.fetch_add(1, Ordering::Relaxed);
                let resp = if hit == 0 {
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                };
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        let out = fetch(&format!("http://{addr}/x"), "GET", &HashMap::new(), None)
            .await
            .unwrap();
        assert_eq!(out, "ok");
        assert_eq!(HITS.load(Ordering::Relaxed), 2, "5xx 应重试一次");
    }

    #[tokio::test]
    async fn does_not_retry_on_4xx() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        static HITS: AtomicUsize = AtomicUsize::new(0);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 512];
                let _ = sock.read(&mut buf).await;
                HITS.fetch_add(1, Ordering::Relaxed);
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                let _ = sock.shutdown().await;
            }
        });
        let err = fetch(&format!("http://{addr}/x"), "GET", &HashMap::new(), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("404"), "实际: {err}");
        assert_eq!(HITS.load(Ordering::Relaxed), 1, "4xx 不应重试");
    }

    #[test]
    fn source_header_js_is_evaluated() {
        let mut source = BookSource {
            book_source_url: "https://header.example".into(),
            ..Default::default()
        };
        source.header = Some(
            r#"@js: JSON.stringify({"Referer":"https://header.example/","X-Token":"t1"})"#
                .into(),
        );
        let h = source_headers(&source, &source.book_source_url);
        assert_eq!(
            h.get("Referer").map(String::as_str),
            Some("https://header.example/")
        );
        assert_eq!(h.get("X-Token").map(String::as_str), Some("t1"));
    }
}
