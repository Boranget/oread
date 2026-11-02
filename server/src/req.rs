//! HTTP 请求层（AnalyzeUrl 等价封装）
//! 简化版：支持 method/header/body，内置 cookie jar 与超时；相对 URL 归一化在 web_book 中处理。

use std::collections::HashMap;
use std::time::Duration;

use reqwest::Client;
use serde_json::Value;

/// 发起 HTTP 请求，返回响应文本
pub async fn fetch(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
) -> anyhow::Result<String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .cookie_store(true)
        .build()?;
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
    rb = rb.headers(hm);
    if let Some(b) = body {
        rb = rb.body(b.to_string());
    }
    let resp = rb.send().await?;
    let text = resp.text().await?;
    Ok(text)
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
