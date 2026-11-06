//! `/k` 服务端性能埋点 + 客户端自报视口探针：默认开启、无参数依赖，按天写 JSONL，有界保留。
//!
//! - 文件：`DATA_DIR/logs/k-metrics-YYYYMMDD.jsonl`（请求级）与 `k-probe-YYYYMMDD.jsonl`（视口探针）
//! - 各自保留最近 7 天；目录内同类文件总占用超过 20MB 时从最旧文件开始删
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
    spawn_write(PathBuf::from(&state.cfg.data_dir), "k-metrics", line);
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

fn spawn_write<T: Serialize + Send + 'static>(dir: PathBuf, prefix: &'static str, line: T) {
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
        let file = logs.join(format!("{prefix}-{day}.jsonl"));
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
        if n.is_multiple_of(PRUNE_EVERY) {
            let logs = logs.clone();
            let prefix = prefix;
            tokio::task::spawn_blocking(move || prune_with(&logs, prefix, KEEP_DAYS, MAX_BYTES));
        }
    });
}

fn log_files(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix) && n.ends_with(".jsonl"))
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

/// 有界清理：保留最近 `keep_days` 个同类日文件；总占用超过 `max_bytes` 时删最旧。
pub(crate) fn prune_with(dir: &Path, prefix: &str, keep_days: usize, max_bytes: u64) {
    let mut files = log_files(dir, prefix);
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

fn log_file(data_dir: &Path, prefix: &str, date: &str) -> PathBuf {
    data_dir
        .join("logs")
        .join(format!("{prefix}-{}.jsonl", date.replace('-', "")))
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
    let text =
        tokio::fs::read_to_string(log_file(Path::new(&state.cfg.data_dir), "k-metrics", &date))
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
            render_ms: Pct {
                p50: 0,
                p95: 0,
                max: 0,
            },
            bytes_avg: 0.0,
            since_prev_ms: Pct {
                p50: 0,
                p95: 0,
                max: 0,
            },
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
    let body =
        tokio::fs::read_to_string(log_file(Path::new(&state.cfg.data_dir), "k-metrics", &date))
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

/// `GET /api/k/probe`：页内自报视口探针（ES5 XHR，失败静默，不重载页面）。
///
/// 字段同时记录「URL 参数视口」与「页面实测视口」，用来判断 Kindle 报的
/// `innerHeight` 是否等于真正参与排版的布局视口高（`clientHeight`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeLine {
    pub ts: String,
    pub path: String,
    #[serde(default)]
    pub vw_param: Option<String>,
    #[serde(default)]
    pub vh_param: Option<String>,
    #[serde(default)]
    pub inner_w: Option<String>,
    #[serde(default)]
    pub inner_h: Option<String>,
    #[serde(default)]
    pub client_w: Option<String>,
    #[serde(default)]
    pub client_h: Option<String>,
    #[serde(default)]
    pub scroll_w: Option<String>,
    #[serde(default)]
    pub scroll_h: Option<String>,
    #[serde(default)]
    pub dpr: Option<String>,
    /// v2：`screen.width`，用于判断系统栏/工具栏是否占掉了可视视口
    #[serde(default)]
    pub screen_w: Option<String>,
    #[serde(default)]
    pub screen_h: Option<String>,
    /// v2：`screen.availHeight`（去掉系统栏后的可用高）
    #[serde(default)]
    pub avail_h: Option<String>,
    /// v2：`outerWidth/outerHeight`
    #[serde(default)]
    pub outer_w: Option<String>,
    #[serde(default)]
    pub outer_h: Option<String>,
    /// v2：`document.body.clientHeight`
    #[serde(default)]
    pub body_h: Option<String>,
    /// v2：`#k-stage` 的 `clientHeight`，页面无该元素时为 `-`
    #[serde(default)]
    pub stage_h: Option<String>,
    /// v3：`calib_js` 标定并写入 `k_vh` 的真实可见高
    #[serde(default)]
    pub visible_h: Option<String>,
    /// v3：底栏 `#k-bar` 实际 top（`getBoundingClientRect().top`）
    #[serde(default)]
    pub bar_top: Option<String>,
    #[serde(default)]
    pub fs: Option<String>,
    pub ua_class: String,
    #[serde(default)]
    pub ua: Option<String>,
    #[serde(default)]
    pub sid: Option<String>,
}

/// 探针单字段上限（防超长 query 落盘）
const PROBE_LIMIT: usize = 32;

#[derive(Deserialize)]
pub struct ProbeQ {
    pub vw_param: Option<String>,
    pub vh_param: Option<String>,
    pub inner_w: Option<String>,
    pub inner_h: Option<String>,
    pub client_w: Option<String>,
    pub client_h: Option<String>,
    pub scroll_w: Option<String>,
    pub scroll_h: Option<String>,
    pub dpr: Option<String>,
    pub screen_w: Option<String>,
    pub screen_h: Option<String>,
    pub avail_h: Option<String>,
    pub outer_w: Option<String>,
    pub outer_h: Option<String>,
    pub body_h: Option<String>,
    pub stage_h: Option<String>,
    pub visible_h: Option<String>,
    pub bar_top: Option<String>,
    pub fs: Option<String>,
    pub path: Option<String>,
}

fn norm(v: Option<String>) -> Option<String> {
    v.map(|s| truncate(s.trim(), PROBE_LIMIT))
        .filter(|s| !s.is_empty())
}

pub async fn probe(
    State(state): State<AppState>,
    _u: AuthUser,
    headers: axum::http::HeaderMap,
    Query(q): Query<ProbeQ>,
) -> StatusCode {
    let ua_raw = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let sid = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(session_token)
        .map(|t| sid_hash(&t));
    let line = ProbeLine {
        ts: Local::now().to_rfc3339_opts(SecondsFormat::Secs, false),
        path: norm(q.path).unwrap_or_else(|| "/".into()),
        vw_param: norm(q.vw_param),
        vh_param: norm(q.vh_param),
        inner_w: norm(q.inner_w),
        inner_h: norm(q.inner_h),
        client_w: norm(q.client_w),
        client_h: norm(q.client_h),
        scroll_w: norm(q.scroll_w),
        scroll_h: norm(q.scroll_h),
        dpr: norm(q.dpr),
        screen_w: norm(q.screen_w),
        screen_h: norm(q.screen_h),
        avail_h: norm(q.avail_h),
        outer_w: norm(q.outer_w),
        outer_h: norm(q.outer_h),
        body_h: norm(q.body_h),
        stage_h: norm(q.stage_h),
        visible_h: norm(q.visible_h),
        bar_top: norm(q.bar_top),
        fs: norm(q.fs),
        ua_class: classify_ua(&ua_raw).to_string(),
        ua: Some(truncate(&ua_raw, 120)),
        sid,
    };
    spawn_write(PathBuf::from(&state.cfg.data_dir), "k-probe", line);
    StatusCode::NO_CONTENT
}

#[derive(Serialize)]
struct ProbeStat {
    count: usize,
    /// vw/vh 参数组合（`vw_param`x`vh_param`）
    param: BTreeMap<String, usize>,
    /// innerWidth×innerHeight
    inner: BTreeMap<String, usize>,
    /// documentElement.clientWidth×clientHeight（真正的布局视口）
    client: BTreeMap<String, usize>,
    /// documentElement.scrollWidth×scrollHeight
    scroll: BTreeMap<String, usize>,
    /// `scroll_h > client_h` 的条数（有纵向滚动）
    overflow_v: usize,
    /// `scroll_w > client_w` 的条数（有横向滚动）
    overflow_h: usize,
    /// v2：`screen_h − client_h` 的分布；正值表示屏幕比布局视口高，
    /// 差额就是系统栏/工具栏可能占掉的可视高度
    gap: BTreeMap<String, usize>,
}

fn num(v: &Option<String>) -> Option<i64> {
    v.as_deref().and_then(|s| s.parse::<i64>().ok())
}

/// `GET /api/k/probe/summary?date=YYYY-MM-DD`：管理员查看探针聚合 + 最近 20 条。
pub async fn probe_summary(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<MetricsQ>,
) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&user)?;
    let date = q.date.unwrap_or_else(default_date);
    if !valid_date(&date) {
        return Err(AppError::BadRequest("date 需为 YYYY-MM-DD".into()));
    }
    let text =
        tokio::fs::read_to_string(log_file(Path::new(&state.cfg.data_dir), "k-probe", &date))
            .await
            .unwrap_or_default();
    let lines: Vec<ProbeLine> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<ProbeLine>(l).ok())
        .collect();

    let mut stats: BTreeMap<String, ProbeStat> = BTreeMap::new();
    for line in &lines {
        let stat = stats.entry(line.path.clone()).or_insert_with(|| ProbeStat {
            count: 0,
            param: BTreeMap::new(),
            inner: BTreeMap::new(),
            client: BTreeMap::new(),
            scroll: BTreeMap::new(),
            overflow_v: 0,
            overflow_h: 0,
            gap: BTreeMap::new(),
        });
        stat.count += 1;
        let dim = |a: &Option<String>, b: &Option<String>| match (a.as_deref(), b.as_deref()) {
            (Some(x), Some(y)) => format!("{x}x{y}"),
            (Some(x), None) => x.to_string(),
            (None, Some(y)) => format!("?x{y}"),
            (None, None) => "?".into(),
        };
        *stat
            .param
            .entry(dim(&line.vw_param, &line.vh_param))
            .or_default() += 1;
        *stat
            .inner
            .entry(dim(&line.inner_w, &line.inner_h))
            .or_default() += 1;
        *stat
            .client
            .entry(dim(&line.client_w, &line.client_h))
            .or_default() += 1;
        *stat
            .scroll
            .entry(dim(&line.scroll_w, &line.scroll_h))
            .or_default() += 1;
        let ch = num(&line.client_h);
        let sh = num(&line.scroll_h);
        let cw = num(&line.client_w);
        let sw = num(&line.scroll_w);
        if let (Some(a), Some(b)) = (sh, ch) {
            if b > 0 && a > b {
                stat.overflow_v += 1;
            }
        }
        if let (Some(a), Some(b)) = (sw, cw) {
            if b > 0 && a > b {
                stat.overflow_h += 1;
            }
        }
        if let (Some(s), Some(c)) = (num(&line.screen_h), ch) {
            *stat.gap.entry((s - c).to_string()).or_default() += 1;
        }
    }

    let recent: Vec<ProbeLine> = lines.iter().rev().take(20).cloned().collect();
    // 最近一条的完整字典（含 v2 新字段），方便直接判断"可视视口是否更小"
    let latest = lines.last().cloned();
    Ok(Json(serde_json::json!({
        "date": date,
        "probes": lines.len(),
        "paths": stats,
        "latest": latest,
        "recent": recent,
    })))
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
            "ts",
            "path",
            "status",
            "render_ms",
            "bytes",
            "key",
            "book",
            "ch",
            "p",
            "fs",
            "vw",
            "vh",
            "scope",
            "tag",
            "ua_class",
            "ua",
            "sid",
            "since_prev_ms",
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
            std::fs::write(
                dir.join(format!("k-metrics-2026010{i}.jsonl")),
                "x".repeat(600),
            )
            .unwrap();
        }
        // 保留 7 天 → 删最旧 2 个
        prune_with(&dir, "k-metrics", 7, MAX_BYTES);
        assert!(!dir.join("k-metrics-20260101.jsonl").exists());
        assert!(!dir.join("k-metrics-20260102.jsonl").exists());
        assert!(dir.join("k-metrics-20260109.jsonl").exists());
        // 总量超限 → 继续从最旧删，直到 <= 1000
        prune_with(&dir, "k-metrics", 7, 1000);
        let files = log_files(&dir, "k-metrics");
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

    #[test]
    fn probe_summary_rejects_non_admin() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // 探针聚合只读文件系统，连接池仅用于构造 AppState
            let pool = sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap();
            let state = AppState {
                shared: pool,
                cfg: crate::config::Config::from_env(),
                user_pools: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::routes::UserPoolCache::default(),
                )),
            };
            let user = AuthUser {
                id: 2,
                username: "u2".into(),
                role: "user".into(),
            };
            let res = probe_summary(State(state), user, Query(MetricsQ { date: None })).await;
            match res {
                Err(AppError::Forbidden) => {}
                Err(e) => panic!("非管理员必须是 403，实得 {e:?}"),
                Ok(_) => panic!("非管理员不得拿到探针聚合"),
            }
        });
    }

    #[test]
    fn probe_line_keeps_all_fields() {
        let line = ProbeLine {
            ts: "2026-11-05T17:09:36+08:00".into(),
            path: "/k/read".into(),
            vw_param: Some("1072".into()),
            vh_param: Some("1268".into()),
            inner_w: Some("1072".into()),
            inner_h: Some("1268".into()),
            client_w: Some("1055".into()),
            client_h: Some("1200".into()),
            scroll_w: Some("1072".into()),
            scroll_h: Some("1244".into()),
            dpr: Some("1".into()),
            screen_w: Some("1072".into()),
            screen_h: Some("1268".into()),
            avail_h: Some("1268".into()),
            outer_w: Some("1072".into()),
            outer_h: Some("1268".into()),
            body_h: Some("1244".into()),
            stage_h: Some("1056".into()),
            visible_h: Some("1180".into()),
            bar_top: Some("1092".into()),
            fs: Some("2".into()),
            ua_class: "kindle".into(),
            ua: Some("Mozilla/5.0 Kindle Silk".into()),
            sid: Some("deadbeef".into()),
        };
        let v: serde_json::Value = serde_json::to_value(&line).unwrap();
        for k in [
            "ts",
            "path",
            "vw_param",
            "vh_param",
            "inner_w",
            "inner_h",
            "client_w",
            "client_h",
            "scroll_w",
            "scroll_h",
            "dpr",
            "screen_w",
            "screen_h",
            "avail_h",
            "outer_w",
            "outer_h",
            "body_h",
            "stage_h",
            "visible_h",
            "bar_top",
            "fs",
            "ua_class",
            "ua",
            "sid",
        ] {
            assert!(v.get(k).is_some(), "探针字段缺失：{k}");
        }
        // 反序列化也能还原（聚合读取依赖它）
        let back: ProbeLine = serde_json::from_value(v).unwrap();
        assert_eq!(back.client_h.as_deref(), Some("1200"));
        assert_eq!(back.screen_h.as_deref(), Some("1268"));
        assert_eq!(back.stage_h.as_deref(), Some("1056"));
        assert_eq!(back.visible_h.as_deref(), Some("1180"));
        assert_eq!(back.bar_top.as_deref(), Some("1092"));
    }

    /// v1 的历史行没有 v2 字段，聚合读旧文件不能整行丢掉。
    #[test]
    fn probe_old_line_without_v2_fields_still_parses() {
        let old = r#"{"ts":"2026-11-05T19:26:59+08:00","path":"/k/read","vw_param":"1072","vh_param":"1268","inner_w":"1072","inner_h":"1268","client_w":"1072","client_h":"1268","scroll_w":"1072","scroll_h":"1268","dpr":"1.7964072227478027","fs":"2","ua_class":"kindle","ua":"Mozilla/5.0 Kindle","sid":"ad620038"}"#;
        let back: ProbeLine = serde_json::from_str(old).unwrap();
        assert_eq!(back.client_h.as_deref(), Some("1268"));
        assert_eq!(back.screen_h, None);
    }

    #[test]
    fn probe_values_are_truncated() {
        let long = "1".repeat(200);
        let p = ProbeQ {
            vw_param: Some(long.clone()),
            vh_param: Some(" 1268 ".into()),
            inner_w: None,
            inner_h: None,
            client_w: None,
            client_h: None,
            scroll_w: None,
            scroll_h: None,
            dpr: None,
            screen_w: Some(long.clone()),
            screen_h: Some(" 1268 ".into()),
            avail_h: Some(long.clone()),
            outer_w: Some(long.clone()),
            outer_h: None,
            body_h: Some(long.clone()),
            stage_h: Some("-".into()),
            visible_h: Some("1180".into()),
            bar_top: Some("1092".into()),
            fs: None,
            path: Some("/k/read".repeat(30)),
        };
        assert_eq!(norm(p.vw_param).unwrap().chars().count(), PROBE_LIMIT);
        assert_eq!(norm(p.vh_param).as_deref(), Some("1268"));
        assert_eq!(norm(p.path).unwrap().chars().count(), PROBE_LIMIT);
        for v in [
            norm(p.screen_w),
            norm(p.avail_h),
            norm(p.outer_w),
            norm(p.body_h),
        ] {
            assert_eq!(
                v.unwrap().chars().count(),
                PROBE_LIMIT,
                "v2 字段同样 32 字符截断"
            );
        }
        assert_eq!(norm(p.screen_h).as_deref(), Some("1268"));
        assert_eq!(norm(p.stage_h).as_deref(), Some("-"));
        assert_eq!(norm(p.visible_h).as_deref(), Some("1180"));
        assert_eq!(norm(Some("   ".into())), None);
    }
}
