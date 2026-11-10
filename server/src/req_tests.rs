//! req 单测（move-only 从 req.rs 拆出）。

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

#[test]
fn source_timeout_clamps_and_falls_back_to_global() {
    assert_eq!(parse_source_timeout_ms(0), None, "0 表示未设置");
    assert_eq!(parse_source_timeout_ms(-1), None, "负数表示未设置");
    // Legado 允许任意毫秒值；这里只做极小值保护（100ms），不再把 1–60s 人为夹取
    assert_eq!(
        parse_source_timeout_ms(1),
        Some(Duration::from_millis(100)),
        "低于下限夹到 100ms"
    );
    assert_eq!(
        parse_source_timeout_ms(4200),
        Some(Duration::from_millis(4200)),
        "常规值原样生效"
    );
    assert_eq!(
        parse_source_timeout_ms(80_000),
        Some(Duration::from_millis(80_000)),
        "超出旧 60s 上限也原样生效"
    );
    assert_eq!(
        parse_source_timeout_ms(999_999),
        Some(Duration::from_millis(120_000)),
        "极端值夹到全局上限 120s"
    );
}

#[test]
fn source_timeout_field_is_parsed_leniently() {
    let base = serde_json::json!({
        "book_source_url": "https://example.com",
        "book_source_name": "示例"
    });
    let missing: BookSource = serde_json::from_value(base.clone()).unwrap();
    assert_eq!(missing.timeout_millisecond, 0);
    assert_eq!(source_timeout(&missing), None, "缺失时回退全局");

    let mut with_string = base.clone();
    with_string["timeout_millisecond"] = serde_json::json!("2000");
    let s: BookSource = serde_json::from_value(with_string).unwrap();
    assert_eq!(s.timeout_millisecond, 2000);
    assert_eq!(source_timeout(&s), Some(Duration::from_millis(2000)));

    let mut broken = base;
    broken["timeout_millisecond"] = serde_json::json!("abc");
    let s: BookSource = serde_json::from_value(broken).expect("非法值不应让整份书源解析失败");
    assert_eq!(s.timeout_millisecond, 0, "非法值归 0，回退全局");
    assert_eq!(source_timeout(&s), None);
}

// 每源在飞上限：Legado `concurrentRate` 取整数部分；缺省/0/非法回退默认；夹取 1–8
#[test]
fn concurrent_rate_parsing_clamps_and_defaults() {
    assert_eq!(parse_source_concurrency(None, 8), 8, "无字段 → 默认");
    assert_eq!(parse_source_concurrency(Some(""), 8), 8, "空串 → 默认");
    assert_eq!(parse_source_concurrency(Some("0"), 8), 8, "0 → 默认");
    assert_eq!(parse_source_concurrency(Some("abc"), 8), 8, "非法 → 默认");
    assert_eq!(parse_source_concurrency(Some("-3"), 8), 8, "负数 → 默认");
    assert_eq!(parse_source_concurrency(Some("1"), 8), 1);
    assert_eq!(
        parse_source_concurrency(Some("3/1"), 8),
        3,
        "取斜杠前整数部分"
    );
    assert_eq!(
        parse_source_concurrency(Some(" 2.5 "), 8),
        2,
        "小数取整数部分"
    );
    assert_eq!(parse_source_concurrency(Some("9"), 8), 8, "超过 8 夹回");
    assert_eq!(parse_source_concurrency(Some("100/200"), 8), 8);
}

// `concurrentRate` 的完整语义：`N/interval` → 并发 N + 每 N 次等待 interval
#[test]
fn parse_source_limit_reads_rate_and_interval() {
    let l = parse_source_limit(Some("3/1000"), 8);
    assert_eq!(l.concurrency, 3);
    assert_eq!(l.per, 3);
    assert_eq!(l.interval, Duration::from_millis(1000));

    let l = parse_source_limit(Some("2/200"), 8);
    assert_eq!((l.concurrency, l.per), (2, 2));
    assert_eq!(l.interval, Duration::from_millis(200));

    // 只写整数：退化为纯并发上限，不做速率等待
    let l = parse_source_limit(Some("4"), 8);
    assert_eq!(l.concurrency, 4);
    assert_eq!(l.per, 0);
    assert_eq!(l.interval, Duration::ZERO);

    // 斜杠后非法/0：也不做速率等待
    let l = parse_source_limit(Some("3/abc"), 8);
    assert_eq!(l.concurrency, 3);
    assert_eq!(l.per, 0);
    assert_eq!(l.interval, Duration::ZERO);

    // 间隔上限 10 分钟
    let l = parse_source_limit(Some("1/999999"), 8);
    assert_eq!(
        l.interval,
        Duration::from_millis(SOURCE_RATE_MAX_INTERVAL_MS)
    );

    // 缺省/非法 → 全局默认并发、无速率等待
    let l = parse_source_limit(None, 8);
    assert_eq!((l.concurrency, l.per), (8, 0));
}

// 字段缺失时使用全局默认 8；字段存在时按书源生效
#[test]
fn source_concurrency_reads_field_or_default() {
    let json = r#"{"book_source_url":"https://x","book_source_name":"x"}"#;
    let mut s: BookSource = serde_json::from_str(json).unwrap();
    assert_eq!(source_concurrency(&s), 8, "缺省时默认值生效");
    s.concurrent_rate = Some("4/2".into());
    assert_eq!(source_concurrency(&s), 4);
    s.concurrent_rate = Some("0".into());
    assert_eq!(source_concurrency(&s), 8, "0 回退默认");
    // 原始 Legado JSON（camelCase）也应被接受
    let raw = r#"{"book_source_url":"https://y","book_source_name":"y","concurrentRate":"3/5"}"#;
    let s2: BookSource = serde_json::from_str(raw).unwrap();
    assert_eq!(source_concurrency(&s2), 3, "camelCase concurrentRate 生效");
}

// 同源超限必须排队，释放许可后立刻放行
#[tokio::test]
async fn same_source_over_limit_queues_until_release() {
    let key = "https://tests.oread/same-source";
    let p1 = acquire_source_slot(key, 2).await.unwrap();
    let _p2 = acquire_source_slot(key, 2).await.unwrap();
    let waits_before = source_limit_metrics().0;
    let blocked = tokio::spawn(async move { acquire_source_slot(key, 2).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!blocked.is_finished(), "第三个同源请求应排队");
    drop(p1);
    let p3 = tokio::time::timeout(Duration::from_secs(1), blocked)
        .await
        .expect("释放许可后应放行")
        .unwrap();
    assert!(p3.is_ok());
    assert!(
        source_limit_metrics().0 > waits_before,
        "等待次数指标应随排队增加"
    );
}

// 一个源排队不应阻塞其他源（跨源独立）
#[tokio::test]
async fn cross_source_is_not_blocked() {
    let a = "https://tests.oread/cross-a";
    let b = "https://tests.oread/cross-b";
    let _hold_a = acquire_source_slot(a, 1).await.unwrap();
    let got_b = tokio::time::timeout(Duration::from_millis(200), acquire_source_slot(b, 1))
        .await
        .expect("其他源不应被 A 的限流阻塞");
    assert!(got_b.is_ok());
}

// 上限变更后按源信号量重建（源规则被编辑时不会沿用旧上限）
#[tokio::test]
async fn source_limit_rebuilds_when_changed() {
    let key = "https://tests.oread/rebuild";
    let _p = acquire_source_slot(key, 1).await.unwrap();
    let got = tokio::time::timeout(Duration::from_millis(200), acquire_source_slot(key, 2))
        .await
        .expect("上限提高后应立即可用");
    assert!(got.is_ok());
}

// 源级超时必须真的落到 HTTP 请求上，而不是只解析字段
#[tokio::test]
async fn timed_fetch_applies_source_budget() {
    use tokio::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((_sock, _)) = listener.accept().await else {
                break;
            };
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    let started = std::time::Instant::now();
    let err = fetch_for_source_timed(
        &format!("http://{addr}/slow"),
        "GET",
        &HashMap::new(),
        None,
        "https://example.com",
        Some(Duration::from_secs(1)),
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("请求超时"), "应报超时: {msg}");
    assert!(msg.contains("1s"), "应带源级预算: {msg}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "应在 1s 左右放弃，实际 {:?}",
        started.elapsed()
    );
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

// ---- P4: JS `cookie` 全局底层 ----

#[test]
fn js_cookie_get_missing_returns_empty() {
    let _guard = lock_cookie_jar();
    cookie_reset();
    assert_eq!(js_cookie_get("src-none", "https://none.example/x"), "");
}

#[test]
fn js_cookie_set_then_get_and_remove() {
    let _guard = lock_cookie_jar();
    cookie_reset();
    js_cookie_set("src-p4", "https://p4.example", "sid=R; k=R2");
    assert_eq!(js_cookie_get("src-p4", "https://p4.example"), "sid=R; k=R2");
    js_cookie_remove("src-p4", "https://p4.example");
    assert_eq!(js_cookie_get("src-p4", "https://p4.example"), "");
    // clear 只清当前源
    js_cookie_set("src-p4", "https://p4.example", "again=1");
    js_cookie_clear("src-p4");
    assert_eq!(js_cookie_get("src-p4", "https://p4.example"), "");
}

#[test]
fn js_cookie_scopes_are_isolated_concurrently() {
    let _guard = lock_cookie_jar();
    cookie_reset();
    std::thread::scope(|s| {
        s.spawn(|| {
            js_cookie_set("src-a", "https://a.example", "sid=A");
            js_cookie_set("src-a", "https://a.example", "only_a=1");
        });
        s.spawn(|| {
            js_cookie_set("src-b", "https://b.example", "sid=B");
            js_cookie_set("src-b", "https://b.example", "only_b=1");
        });
    });
    // 两个源对同名 cookie 互不影响（各自桶内独立）
    assert_eq!(
        js_cookie_get("src-a", "https://a.example"),
        "sid=A; only_a=1"
    );
    assert_eq!(
        js_cookie_get("src-b", "https://b.example"),
        "sid=B; only_b=1"
    );
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
    source.header =
        Some(r#"@js: JSON.stringify({"Referer":"https://header.example/","X-Token":"t1"})"#.into());
    let h = source_headers(&source, &source.book_source_url);
    assert_eq!(
        h.get("Referer").map(String::as_str),
        Some("https://header.example/")
    );
    assert_eq!(h.get("X-Token").map(String::as_str), Some("t1"));
}

// scope 解析：缺省/未知一律 all（新默认）；显式 curated 才收紧
#[test]
fn scope_parse_defaults_to_all() {
    assert_eq!(SearchScope::parse(None), SearchScope::All);
    assert_eq!(SearchScope::parse(Some("")), SearchScope::All);
    assert_eq!(SearchScope::parse(Some("curated")), SearchScope::Curated);
    assert_eq!(SearchScope::parse(Some("unknown")), SearchScope::All);
    assert_eq!(SearchScope::parse(Some(" all ")), SearchScope::All);
    assert_eq!(SearchScope::parse(Some("ENHANCED")), SearchScope::All);
}

// 两个预算独立：curated 单源 8s / 整批 6s；all 单源 5s / 整批 25s
#[test]
fn total_budget_defaults_follow_scope_not_per_source() {
    assert_eq!(search_budget(), Duration::from_secs(8), "通用单源默认 8s");
    assert_eq!(
        search_budget_for(SearchScope::Curated),
        Duration::from_secs(8)
    );
    assert_eq!(
        search_budget_for(SearchScope::All),
        Duration::from_secs(5),
        "all 要覆盖上百个源，单源预算必须更短才能跑更多源"
    );
    assert_eq!(
        search_total_budget(SearchScope::Curated),
        Duration::from_secs(6)
    );
    assert_eq!(
        search_total_budget(SearchScope::All),
        Duration::from_secs(25)
    );
    assert_ne!(
        search_total_budget(SearchScope::All),
        search_budget_for(SearchScope::All),
        "整批不能等于单源（拆分前正是这个 bug）"
    );
}

// all 范围的并发槽：未初始化（用户未显式设置）时默认 16
#[test]
fn all_scope_slots_default_to_16() {
    assert_eq!(all_search_slots(), 16, "未显式设置时 all 用 16 槽");
}
