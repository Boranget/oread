//! `/k` 服务端性能埋点：默认开启、无参数依赖，按天写 JSONL，有界保留。
//!
//! - 文件：`DATA_DIR/logs/k-metrics-YYYYMMDD.jsonl`
//! - 保留最近 7 天；总占用超过 20MB 时从最旧文件开始删
//! - 写失败只 warn，绝不影响请求；凭据不落盘（cookie 只记哈希前 8 位）

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Local, SecondsFormat};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::auth::{ensure_admin, AuthUser};
use crate::error::{AppError, AppResult};
use crate::routes::AppState;

/// 保留天数
const KEEP_DAYS: usize = 7;
/// 日志目录总占用上限
const MAX_BYTES: u64 = 20 * 1024 * 1024;
/// 每写多少条做一次清理（另加进程内首次写）
const PRUNE_EVERY: u64 = 64;

static LAST_SEEN: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
static WRITES: AtomicU64 = AtomicU64::new(0);

/// 一行 JSONL 埋点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KLine {
    pub ts: String,
    pub path: String,
    pub status: u16,
    pub render_ms: u64,
    pub bytes: u64,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub book: Option<String>,
    #[serde(default)]
    pub ch: Option<String>,
    #[serde(default)]
    pub p: Option<String>,
    #[serde(default)]
    pub fs: Option<String>,
    #[serde(default)]
    pub vw: Option<String>,
    #[serde(default)]
    pub vh: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    pub ua_class: String,
    pub ua: String,
    #[serde(default)]
    pub sid: Option<String>,
    #[serde(default)]
    pub since_prev_ms: Option<i64>,
}

/// `/k` 埋点中间件：所有 `/k` 请求（含 4xx/5xx、登录页、空态）都记录。
pub async fn record(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if !path.starts_with("/k") {
        return next.run(req).await;
    }
    let started = Instant::now();
    let params = parse_params(req.uri().query().unwrap_or(""));
    let ua_raw = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let sid = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(session_token)
        .map(|t| sid_hash(&t));
    let now_ms = epoch_ms();
    let since_prev_ms = sid.as_ref().and_then(|s| {
        let map = LAST_SEEN.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = map.lock().ok()?;
        let prev = map.insert(s.clone(), now_ms);
        prev.map(|p| now_ms.saturating_sub(p))
    });

    let resp = next.run(req).await;
    // “进入 handler 到响应构造完成”
    let render_ms = started.elapsed().as_millis() as u64;
    let status = resp.status().as_u16();
    // `/k` 响应都是完整 body（Html/String/JSON），这里收一次字节数再原样放回；
    // 不计入 render_ms。
    let (parts, body) = resp.into_parts();
    let body_bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            warn!(error = %e, "k-metrics: 读取响应体失败");
            Vec::new()
        }
    };
    let bytes = body_bytes.len() as u64;
    let resp = Response::from_parts(parts, Body::from(body_bytes));

    let line = KLine {
        ts: Local::now().to_rfc3339_opts(SecondsFormat::Secs, false),
        path,
        status,
        render_ms,
        bytes,
        key: take_param(&params, "key", 64),
        book: take_param(&params, "book", 120),
        ch: take_param(&params, "ch", 120),
        p: take_param(&params, "p", 64),
        fs: take_param(&params, "fs", 64),
        vw: take_param(&params, "vw", 64),
        vh: take_param(&params, "vh", 64),
        scope: take_param(&params, "scope", 64),
        tag: take_param(&params, "tag", 64),
        ua_class: classify_ua(&ua_raw).to_string(),
        ua: truncate(&ua_raw, 120),
        sid,
        since_prev_ms,
    };
    spawn_write(PathBuf::from(&state.cfg.data_dir), line);
    resp
}

fn parse_params(query: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

fn take_param(params: &HashMap<String, String>, key: &str, limit: usize) -> Option<String> {
    params.get(key).map(|v| truncate(v, limit))
}

fn truncate(s: &str, limit: usize) -> String {
    s.chars().take(limit).collect()
}

fn session_token(cookie_header: &str) -> Option<String> {
    cookie_header
        .split(';')
        .map(str::trim)
        .find_map(|kv| kv.strip_prefix("k_session="))
        .map(|v| v.to_string())
}

/// cookie 短哈希：sha256 前 8 位十六进制，不落原文。
pub(crate) fn sid_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    format!("{:x}", digest)[..8].to_string()
}

fn classify_ua(ua: &str) -> &'static str {
    let l = ua.to_ascii_lowercase();
    if l.is_empty() {
        return "unknown";
    }
    if l.contains("kindle") || l.contains("silk") {
        "kindle"
    } else if l.contains("mobile")
        || l.contains("android")
        || l.contains("iphone")
        || l.contains("ipad")
        || l.contains("mobi")
    {
        "mobile"
    } else {
        "desktop"
    }
}

fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn spawn_write(dir: PathBuf, line: KLine) {
    tokio::spawn(async move {
        let Ok(json) = serde_json::to_string(&line) else {
            return;
        };
        let logs = dir.join("logs");
        if let Err(e) = tokio::fs::create_dir_all(&logs).await {
            warn!(error = %e, "k-metrics: 创建日志目录失败");
            return;
        }
        let day = Local::now().format("%Y%m%d").to_string();
        let file = logs.join(format!("k-metrics-{day}.jsonl"));
        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .await
        {
            Ok(mut f) => {
                use tokio::io::AsyncWriteExt;
                if let Err(e) = f.write_all(format!("{json}\n").as_bytes()).await {
                    warn!(error = %e, "k-metrics: 写入失败");
                }
            }
            Err(e) => warn!(error = %e, "k-metrics: 打开文件失败"),
        }
        let n = WRITES.fetch_add(1, Ordering::Relaxed);
        if n % PRUNE_EVERY == 0 {
            let logs = logs.clone();
            tokio::task::spawn_blocking(move || prune_with(&logs, KEEP_DAYS, MAX_BYTES));
        }
    });
}

fn metric_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("k-metrics-") && n.ends_with(".jsonl"))
                .unwrap_or(false)
        })
        .collect();
    // 文件名按日期升序
    files.sort();
    files
}

fn file_len(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// 有界清理：保留最近 `keep_days` 个日文件；总占用超过 `max_bytes` 时删最旧。
pub(crate) fn prune_with(dir: &Path, keep_days: usize, max_bytes: u64) {
    let mut files = metric_files(dir);
    while files.len() > keep_days {
        let old = files.remove(0);
        if let Err(e) = std::fs::remove_file(&old) {
            warn!(error = %e, path = %old.display(), "k-metrics: 删除过期日志失败");
        }
    }
    let mut total: u64 = files.iter().map(|p| file_len(p)).sum();
    while total > max_bytes && files.len() > 1 {
        let old = files.remove(0);
        total = total.saturating_sub(file_len(&old));
        if let Err(e) = std::fs::remove_file(&old) {
            warn!(error = %e, path = %old.display(), "k-metrics: 删除超限日志失败");
        }
    }
}

pub(crate) fn default_date() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn valid_date(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn metrics_file(data_dir: &Path, date: &str) -> PathBuf {
    data_dir
        .join("logs")
        .join(format!("k-metrics-{}.jsonl", date.replace('-', "")))
}

#[derive(Deserialize)]
pub struct MetricsQ {
    pub date: Option<String>,
}

#[derive(Serialize)]
struct Stat {
    count: usize,
    render_ms: Pct,
    bytes_avg: f64,
    since_prev_ms: Pct,
    ua_class: BTreeMap<String, usize>,
}

#[derive(Serialize)]
struct Pct {
    p50: u64,
    p95: u64,
    max: u64,
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[idx.saturating_sub(1).min(sorted.len() - 1)]
}

/// `GET /api/k/metrics?date=YYYY-MM-DD`：当日聚合 + 最近慢请求（>1000ms）。
pub async fn metrics(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<MetricsQ>,
) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&user)?;
    let date = q.date.unwrap_or_else(default_date);
    if !valid_date(&date) {
        return Err(AppError::BadRequest("date 需为 YYYY-MM-DD".into()));
    }
    let text = tokio::fs::read_to_string(metrics_file(
        Path::new(&state.cfg.data_dir),
        &date,
    ))
    .await
    .unwrap_or_default();
    let lines: Vec<KLine> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<KLine>(l).ok())
        .collect();

    let mut stats: BTreeMap<String, Stat> = BTreeMap::new();
    for line in &lines {
        let entry = stats.entry(line.path.clone()).or_insert_with(|| Stat {
            count: 0,
            render_ms: Pct { p50: 0, p95: 0, max: 0 },
            bytes_avg: 0.0,
            since_prev_ms: Pct { p50: 0, p95: 0, max: 0 },
            ua_class: BTreeMap::new(),
        });
        entry.count += 1;
        *entry.ua_class.entry(line.ua_class.clone()).or_default() += 1;
    }
    for (path, stat) in stats.iter_mut() {
        let mut render: Vec<u64> = lines
            .iter()
            .filter(|l| &l.path == path)
            .map(|l| l.render_ms)
            .collect();
        render.sort_unstable();
        let mut since: Vec<u64> = lines
            .iter()
            .filter(|l| &l.path == path)
            .filter_map(|l| l.since_prev_ms.map(|v| v.max(0) as u64))
            .collect();
        since.sort_unstable();
        let bytes_sum: u64 = lines
            .iter()
            .filter(|l| &l.path == path)
            .map(|l| l.bytes)
            .sum();
        stat.render_ms = Pct {
            p50: percentile(&render, 50.0),
            p95: percentile(&render, 95.0),
            max: render.last().copied().unwrap_or(0),
        };
        stat.bytes_avg = if stat.count == 0 {
            0.0
        } else {
            bytes_sum as f64 / stat.count as f64
        };
        stat.since_prev_ms = Pct {
            p50: percentile(&since, 50.0),
            p95: percentile(&since, 95.0),
            max: since.last().copied().unwrap_or(0),
        };
    }

    // 最近 N 条慢请求（文件按时间追加，取尾部）
    let slow: Vec<KLine> = lines
        .iter()
        .filter(|l| l.render_ms > 1000)
        .rev()
        .take(20)
        .cloned()
        .collect();

    Ok(Json(serde_json::json!({
        "date": date,
        "requests": stats.values().map(|s| s.count).sum::<usize>(),
        "paths": stats,
        "slow": slow,
    })))
}

/// `GET /api/k/metrics/export?date=YYYY-MM-DD`：直接下载该日 JSONL 原文。
pub async fn export(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<MetricsQ>,
) -> AppResult<Response> {
    ensure_admin(&user)?;
    let date = q.date.unwrap_or_else(default_date);
    if !valid_date(&date) {
        return Err(AppError::BadRequest("date 需为 YYYY-MM-DD".into()));
    }
    let body = tokio::fs::read_to_string(metrics_file(
        Path::new(&state.cfg.data_dir),
        &date,
    ))
    .await
    .map_err(|_| AppError::NotFound(format!("该日没有埋点文件：{date}")))?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/x-ndjson".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=k-metrics-{date}.jsonl"),
            ),
        ],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_line() -> KLine {
        KLine {
            ts: "2026-11-04T10:00:00+08:00".into(),
            path: "/k/search".into(),
            status: 200,
            render_ms: 321,
            bytes: 4096,
            key: Some("百年孤独".into()),
            book: None,
            ch: None,
            p: Some("1".into()),
            fs: Some("2".into()),
            vw: None,
            vh: None,
            scope: Some("curated".into()),
            tag: None,
            ua_class: "kindle".into(),
            ua: "Mozilla/5.0 (Kindle)".into(),
            sid: Some(sid_hash("super-secret-token")),
            since_prev_ms: Some(2500),
        }
    }

    #[test]
    fn line_has_all_fields_and_sid_is_hash() {
        let line = sample_line();
        let v = serde_json::to_value(&line).unwrap();
        for key in [
            "ts", "path", "status", "render_ms", "bytes", "key", "book", "ch", "p", "fs", "vw",
            "vh", "scope", "tag", "ua_class", "ua", "sid", "since_prev_ms",
        ] {
            assert!(v.get(key).is_some(), "缺字段 {key}");
        }
        let sid = v["sid"].as_str().unwrap();
        assert_eq!(sid.len(), 8, "sid 只保留 8 位");
        assert!(!sid.contains("super-secret"), "sid 不能是原文");
        assert_eq!(sid, sid_hash("super-secret-token"), "同一 token 哈希稳定");
        assert!(v["key"].as_str().unwrap().chars().count() <= 64);
    }

    #[test]
    fn key_is_truncated() {
        let long = "长".repeat(100);
        assert_eq!(truncate(&long, 64).chars().count(), 64);
        let mut params = HashMap::new();
        params.insert("key".to_string(), long);
        assert_eq!(take_param(&params, "key", 64).unwrap().chars().count(), 64);
    }

    #[test]
    fn prune_removes_old_files_and_oversize() {
        let dir = std::env::temp_dir().join(format!("k-metrics-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 1..=9 {
            std::fs::write(dir.join(format!("k-metrics-2026010{i}.jsonl")), "x".repeat(600))
                .unwrap();
        }
        // 保留 7 天 → 删最旧 2 个
        prune_with(&dir, 7, MAX_BYTES);
        assert!(!dir.join("k-metrics-20260101.jsonl").exists());
        assert!(!dir.join("k-metrics-20260102.jsonl").exists());
        assert!(dir.join("k-metrics-20260109.jsonl").exists());
        // 总量超限 → 继续从最旧删，直到 <= 1000
        prune_with(&dir, 7, 1000);
        let files = metric_files(&dir);
        let total: u64 = files.iter().map(|p| file_len(p)).sum();
        assert!(total <= 1000, "超限后总量应 <= 1000，实际 {total}");
        assert!(files.len() < 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_admin_gets_forbidden() {
        let user = AuthUser {
            id: 1,
            username: "u".into(),
            role: "user".into(),
        };
        let err = ensure_admin(&user).unwrap_err();
        assert!(matches!(err, AppError::Forbidden));
        assert_eq!(
            AppError::Forbidden.into_response().status(),
            StatusCode::FORBIDDEN
        );
    }
}
