//! 书源健康状态：把“源还能不能用”从静态文档搬进运行时数据库。
//!
//! 设计要点：
//! - `book_sources.enabled` 是用户意志（想不想用），本模块的 `source_health` 是客观观测（还能不能用）。
//! - 只有**站点侧故障**（连接失败/超时/HTTP 非 2xx）才累加 `consecutive_failures`；
//!   **规则引擎故障**（选择器/JSONPath/JS）记为 `partial`，既不清零也不累加，更不冷却——
//!   引擎问题修一次就好，不能因此把好源判死。
//! - 连续失败达到 [`FAIL_THRESHOLD`] 后写入 `cooldown_until`；搜索默认跳过冷却中的源。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Error;
use futures::stream::{self, StreamExt};
use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

use crate::entities::BookSource;
use crate::error::AppResult;
use crate::req::SourceUnavailable;
use crate::source_quality::{self, Evaluation, Network, ScoreInput, TierConfig};
use crate::web_book;

/// 连续站点失败达到该值后进入冷却。
pub const FAIL_THRESHOLD: i64 = 3;
/// 批量探测的并发上限。
pub const PROBE_CONCURRENCY: usize = 8;
/// 一次批量探测的总时长上限（秒）；超时后其余源保持“未测”。
pub const PROBE_TOTAL_BUDGET_SECS: u64 = 90;
/// 巡检/体检默认使用的探测关键词（只验证“搜索请求能不能通”，不关心命中质量）。
pub const DEFAULT_PROBE_KEY: &str = "测试";
/// `last_error` 落库前的最大字符数。
pub const MAX_ERROR_CHARS: usize = 300;

pub const STATUS_OK: &str = "ok";
pub const STATUS_PARTIAL: &str = "partial";
pub const STATUS_FAIL: &str = "fail";
pub const STATUS_NEVER: &str = "never";

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------- 字段级规则异常台账（进程内） ----------------
///
/// 与 `last_status = partial` 的区别：partial 只表示「这个源在引擎层出过故障」，
/// 而这里精确记录**哪个字段的规则求值抛了异常**（如 JS TypeError），用于排障与
/// `/api/sources` 展示。不落库、不参与冷却，进程重启即清空。
#[derive(Debug, Clone, Default, Serialize)]
pub struct RuleAnomaly {
    pub count: i64,
    pub last_field: String,
    pub last_error: String,
    pub last_at: i64,
}

static RULE_ANOMALIES: OnceLock<Mutex<HashMap<String, RuleAnomaly>>> = OnceLock::new();

fn rule_anomalies() -> &'static Mutex<HashMap<String, RuleAnomaly>> {
    RULE_ANOMALIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 记录一次字段级规则求值异常。字段异常只意味着该字段返回空值，
/// **不改变源的健康状态**（由调用方继续按“必需字段为空”的语义处理）。
pub fn note_rule_error(source_url: &str, field: &str, error: &str) {
    let mut map = match rule_anomalies().lock() {
        Ok(m) => m,
        Err(p) => p.into_inner(),
    };
    let e = map.entry(source_url.to_string()).or_default();
    e.count = e.count.saturating_add(1);
    e.last_field = field.to_string();
    e.last_error = truncate_error(error);
    e.last_at = now_ms();
}

pub fn rule_anomaly(source_url: &str) -> Option<RuleAnomaly> {
    let map = match rule_anomalies().lock() {
        Ok(m) => m,
        Err(p) => p.into_inner(),
    };
    map.get(source_url).cloned()
}

pub fn rule_anomaly_map() -> HashMap<String, RuleAnomaly> {
    let map = match rule_anomalies().lock() {
        Ok(m) => m,
        Err(p) => p.into_inner(),
    };
    map.clone()
}

/// 清空台账；`source_urls` 为空时清全部（供健康状态重置使用）。
pub fn reset_rule_anomalies(source_urls: &[String]) {
    let mut map = match rule_anomalies().lock() {
        Ok(m) => m,
        Err(p) => p.into_inner(),
    };
    if source_urls.is_empty() {
        map.clear();
    } else {
        for u in source_urls {
            map.remove(u);
        }
    }
}

#[path = "source_health_model.rs"]
mod model;
pub use model::*;
#[path = "source_health_scoring.rs"]
mod scoring;
pub use scoring::*;
#[path = "source_health_policy.rs"]
mod policy;
pub use policy::*;
#[path = "source_health_probe.rs"]
mod probe;
pub use probe::*;

#[allow(clippy::field_reassign_with_default)]
#[cfg(test)]
#[path = "source_health_tests.rs"]
mod tests;
