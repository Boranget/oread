//! req 的SSRF 防护与公共地址校验（move-only 从 req.rs 拆出）。

use super::*;

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
    fetch_text(
        url,
        method,
        headers,
        body,
        None,
        allow_private,
        true,
        None,
        SourceLimit::concurrency_only(0),
    )
    .await
}
