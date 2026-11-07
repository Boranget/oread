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

#[path = "k_metrics_log.rs"]
mod log;
pub(crate) use log::*;
#[path = "k_metrics_prune.rs"]
mod prune;
pub(crate) use prune::*;
#[path = "k_metrics_metrics.rs"]
mod metrics;
pub use metrics::*;
#[path = "k_metrics_probe.rs"]
mod probe;
pub use probe::*;

#[cfg(test)]
#[path = "k_metrics_tests.rs"]
mod tests;
