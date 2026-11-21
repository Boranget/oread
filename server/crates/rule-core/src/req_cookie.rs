//! req 的按书源分桶的 Cookie（move-only 从 req.rs 拆出）。

use super::*;

// ---------------- Cookie：按书源分桶的进程内 LRU ----------------

#[derive(Default)]
pub(super) struct CookieBucket {
    /// 保持最近更新顺序；超出单桶上限时从最旧的条目开始淘汰
    pub(super) entries: Vec<(String, String)>,
    pub(super) bytes: usize,
    pub(super) touched: u64,
}

#[derive(Default)]
pub(super) struct CookieStore {
    pub(super) buckets: HashMap<String, CookieBucket>,
    pub(super) clock: u64,
}

pub(super) static COOKIE_JAR: Lazy<Mutex<CookieStore>> =
    Lazy::new(|| Mutex::new(CookieStore::default()));

/// Cookie 分桶键：优先用书源唯一 ID（bookSourceUrl），没有时退化为目标 host。
pub(super) fn cookie_scope(url: &str, source_key: Option<&str>) -> String {
    if let Some(k) = source_key.map(str::trim).filter(|k| !k.is_empty()) {
        return format!("source:{k}");
    }
    match Url::parse(url) {
        Ok(u) => format!("host:{}", u.host_str().unwrap_or_default()),
        Err(_) => "host:".to_string(),
    }
}

pub(super) fn cookie_pairs(scope: &str) -> Vec<(String, String)> {
    let mut store = COOKIE_JAR.lock().unwrap_or_else(|e| e.into_inner());
    store.clock = store.clock.wrapping_add(1);
    let tick = store.clock;
    let Some(b) = store.buckets.get_mut(scope) else {
        return Vec::new();
    };
    b.touched = tick;
    b.entries.clone()
}

pub(super) fn cookies_to_header(pairs: &[(String, String)]) -> Option<String> {
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
/// 实际请求走 `cookie_header_for_request`（按 host 分桶）；本函数仅单测使用。
#[allow(dead_code)]
pub(super) fn cookie_header_for(scope: &str) -> Option<String> {
    cookies_to_header(&cookie_pairs(scope))
}

/// 实际请求用的 Cookie：书源桶优先，再合并目标 host 桶（js/ajax、图片等按 host 发起的
/// 子请求也能复用同一站点会话）。同名时书源桶的值优先。
pub(super) fn cookie_header_for_request(scope: &str, url: &str) -> Option<String> {
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
pub(super) fn parse_set_cookie(raw: &str) -> Option<(String, String, bool)> {
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

pub(super) fn remove_cookie_entry(bucket: &mut CookieBucket, name: &str) {
    if let Some(pos) = bucket.entries.iter().position(|(k, _)| k == name) {
        let (k, v) = bucket.entries.remove(pos);
        bucket.bytes = bucket.bytes.saturating_sub(k.len() + v.len() + 2);
    }
}

/// 回写一条 Set-Cookie：同名覆盖；超出单桶 8KB 时按插入顺序淘汰旧条目。
pub(super) fn record_set_cookie(scope: &str, raw: &str) {
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

pub(super) fn record_response_cookies(scope: &str, headers: &reqwest::header::HeaderMap) {
    for raw in headers.get_all(reqwest::header::SET_COOKIE).iter() {
        if let Ok(s) = raw.to_str() {
            record_set_cookie(scope, s);
        }
    }
}

/// JS `cookie.getCookie(url)`：返回 `name=value; ...`，无值时空串。
/// 书源桶优先，其次目标 host 桶（与 `cookie_header_for_request` 同一套数据）。
pub(crate) fn js_cookie_get(source_key: &str, url: &str) -> String {
    let scope = cookie_scope(url, Some(source_key));
    cookie_header_for_request(&scope, url).unwrap_or_default()
}

/// JS `cookie.setCookie(url, value)`：`value` 允许 `a=b; c=d` 多段，写入当前书源桶
/// （无书源键时写目标 host 桶）。仅进程内，不持久化到库。
pub(crate) fn js_cookie_set(source_key: &str, url: &str, value: &str) {
    let scope = cookie_scope(url, Some(source_key));
    for seg in value.split(';') {
        let seg = seg.trim();
        if seg.contains('=') {
            record_set_cookie(&scope, seg);
        }
    }
}

/// JS `cookie.removeCookie(url|domain)`：清理当前书源桶与目标 host/域名桶。
/// 本项目桶内不保存 domain 元数据，因此按“桶整体清理”实现（语义上覆盖该 URL/域名的会话）。
pub(crate) fn js_cookie_remove(source_key: &str, url_or_domain: &str) {
    let arg = url_or_domain.trim();
    let mut scopes = vec![cookie_scope(arg, Some(source_key))];
    let host = if arg.contains("://") {
        cookie_scope(arg, None)
    } else {
        format!("host:{}", arg.trim_start_matches('.'))
    };
    if !host.ends_with(':') && !scopes.contains(&host) {
        scopes.push(host);
    }
    let mut store = COOKIE_JAR.lock().unwrap_or_else(|e| e.into_inner());
    for s in scopes {
        store.buckets.remove(&s);
    }
}

/// JS `cookie.clear()`：只清当前书源桶（不触碰其它源，避免并发串味）。
pub(crate) fn js_cookie_clear(source_key: &str) {
    let key = source_key.trim();
    if key.is_empty() {
        return;
    }
    let scope = format!("source:{key}");
    let mut store = COOKIE_JAR.lock().unwrap_or_else(|e| e.into_inner());
    store.buckets.remove(&scope);
}

#[cfg(test)]
pub(super) fn cookie_reset() {
    let mut store = COOKIE_JAR.lock().unwrap_or_else(|e| e.into_inner());
    store.buckets.clear();
    store.clock = 0;
}
