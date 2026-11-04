//! 书源健康状态：把“源还能不能用”从静态文档搬进运行时数据库。
//!
//! 设计要点：
//! - `book_sources.enabled` 是用户意志（想不想用），本模块的 `source_health` 是客观观测（还能不能用）。
//! - 只有**站点侧故障**（连接失败/超时/HTTP 非 2xx）才累加 `consecutive_failures`；
//!   **规则引擎故障**（选择器/JSONPath/JS）记为 `partial`，既不清零也不累加，更不冷却——
//!   引擎问题修一次就好，不能因此把好源判死。
//! - 连续失败达到 [`FAIL_THRESHOLD`] 后写入 `cooldown_until`；搜索默认跳过冷却中的源。

use std::collections::HashMap;
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

#[derive(Debug, Clone, FromRow)]
pub struct HealthRow {
    pub source_url: String,
    pub last_checked_at: Option<i64>,
    pub last_status: String,
    pub last_error: Option<String>,
    pub consecutive_failures: i64,
    pub last_ok_at: Option<i64>,
    pub cooldown_until: Option<i64>,
    pub avg_latency_ms: Option<i64>,
    pub check_total: i64,
    // ---- 源质量分层（0008）----
    pub tier: String,
    pub score: i64,
    pub evidence_json: Option<String>,
    pub last_eval_at: Option<i64>,
    pub search_count: i64,
    pub hit_count: i64,
    pub parse_ok_count: i64,
    pub fail_site_count: i64,
    pub fail_engine_count: i64,
    pub ttfb_ms_p90: Option<i64>,
    pub ttfb_samples_json: Option<String>,
    pub promote_streak: i64,
    pub demote_streak: i64,
}

impl Default for HealthRow {
    fn default() -> Self {
        Self {
            source_url: String::new(),
            last_checked_at: None,
            last_status: STATUS_NEVER.to_string(),
            last_error: None,
            consecutive_failures: 0,
            last_ok_at: None,
            cooldown_until: None,
            avg_latency_ms: None,
            check_total: 0,
            tier: source_quality::TIER_OBSERVING.to_string(),
            score: 50,
            evidence_json: None,
            last_eval_at: None,
            search_count: 0,
            hit_count: 0,
            parse_ok_count: 0,
            fail_site_count: 0,
            fail_engine_count: 0,
            ttfb_ms_p90: None,
            ttfb_samples_json: None,
            promote_streak: 0,
            demote_streak: 0,
        }
    }
}

impl HealthRow {
    /// 是否处于冷却中。
    pub fn cooling(&self, now: i64) -> bool {
        self.cooldown_until.map(|t| t > now).unwrap_or(false)
    }

    /// 是否“该测了”：从未测过、冷却已到期且距上次检查超过 `recheck_ms`。
    pub fn due(&self, now: i64, recheck_ms: i64) -> bool {
        if self.cooling(now) {
            return false;
        }
        match self.last_checked_at {
            None => true,
            Some(t) => now.saturating_sub(t) >= recheck_ms,
        }
    }
}

/// 一次探测的结果（尚未落库）。
#[derive(Debug, Clone)]
pub struct Outcome {
    pub source_url: String,
    pub status: &'static str,
    pub latency_ms: i64,
    pub error: Option<String>,
    /// 本次搜索返回的命中条数（体检时同样记录，但不算真实使用）。
    pub hits: i64,
    /// 规则解析是否成功（拿得到结构化结果）。
    pub parsed: bool,
    /// true = 真实搜索使用；false = 导入/巡检体检。
    pub usage: bool,
    /// 静态规则完整度（0–1），用于评分。
    pub rule_complete: f32,
}

/// 批量探测的逐源结果。
#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub source_url: String,
    pub name: String,
    /// ok | partial | fail | never
    pub status: &'static str,
    pub latency_ms: i64,
    pub error: Option<String>,
    /// false 表示因总预算用尽等原因未实际体检。
    pub probed: bool,
    /// 体检时规则实际返回的命中条数（ok 要求 ≥1）。
    pub hits: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// 站点不可达（连接失败/超时/HTTP 非成功）。
    Site,
    /// 规则引擎不支持的失败（选择器/JSONPath/JS）。
    Engine,
}

/// 把一次失败归类为“站点故障”或“引擎故障”。
/// 只有 `req::fetch*` 路径会带 [`SourceUnavailable`] 标记，其余（解析/规则）一律按引擎处理。
pub fn classify(err: &Error) -> FailureKind {
    if err.downcast_ref::<SourceUnavailable>().is_some() {
        FailureKind::Site
    } else {
        FailureKind::Engine
    }
}

/// 构造一个“站点不可达”错误（用于搜索超时等外部判定）。
pub fn site_error(msg: impl Into<String>) -> Error {
    Error::new(SourceUnavailable(msg.into()))
}

pub fn truncate_error(msg: &str) -> String {
    let mut out: String = msg.chars().take(MAX_ERROR_CHARS).collect();
    if msg.chars().count() > MAX_ERROR_CHARS {
        out.push('…');
    }
    out
}

/// 从 `bookSourceUrl` 提取主机名，仅用于“源数 vs 站点数”的汇总展示；
/// 绝不作为健康记录键或 prune 条件（同站多源、一源多域都必须按源条目区分）。
pub fn host_of(raw: &str) -> Option<String> {
    let s = raw.trim();
    let (_, rest) = s.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = authority.rsplit('@').next().unwrap_or(authority);
    let host = auth.split(':').next().unwrap_or(auth);
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// 去重后的站点数（按域名归并，仅用于统计口径）。
pub fn distinct_host_count(sources: &[BookSource]) -> usize {
    sources
        .iter()
        .filter_map(|s| host_of(&s.book_source_url))
        .collect::<std::collections::HashSet<_>>()
        .len()
}

pub async fn load_map(pool: &SqlitePool) -> AppResult<HashMap<String, HealthRow>> {
    let rows: Vec<HealthRow> = sqlx::query_as(
        "SELECT source_url, last_checked_at, last_status, last_error, \
         consecutive_failures, last_ok_at, cooldown_until, avg_latency_ms, check_total, \
         tier, score, evidence_json, last_eval_at, search_count, hit_count, parse_ok_count, \
         fail_site_count, fail_engine_count, ttfb_ms_p90, ttfb_samples_json, \
         promote_streak, demote_streak \
         FROM source_health",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.source_url.clone(), r))
        .collect())
}

/// 纯函数：把一次结果合并进已有健康记录（便于单测覆盖失败阈值/清零/引擎失败语义）。
pub fn apply_outcome(
    old: Option<&HealthRow>,
    o: &Outcome,
    now: i64,
    cooldown_secs: u64,
) -> HealthRow {
    let mut row = old.cloned().unwrap_or_default();
    row.source_url = o.source_url.clone();
    if row.last_status.trim().is_empty() {
        row.last_status = STATUS_NEVER.to_string();
    }
    let prev_total = row.check_total.max(0);
    row.check_total = prev_total + 1;
    if o.latency_ms >= 0 {
        row.avg_latency_ms = Some(match row.avg_latency_ms {
            Some(avg) if prev_total > 0 => {
                (avg.saturating_mul(prev_total) + o.latency_ms) / (prev_total + 1)
            }
            _ => o.latency_ms,
        });
    }
    row.last_checked_at = Some(now);
    row.last_status = o.status.to_string();
    row.last_error = o.error.as_deref().map(truncate_error);
    match o.status {
        STATUS_OK => {
            row.consecutive_failures = 0;
            row.last_ok_at = Some(now);
            row.cooldown_until = None;
        }
        STATUS_FAIL => {
            row.consecutive_failures = row.consecutive_failures.saturating_add(1);
            row.fail_site_count = row.fail_site_count.saturating_add(1);
            if row.consecutive_failures >= FAIL_THRESHOLD {
                let until = now.saturating_add((cooldown_secs as i64).saturating_mul(1000));
                row.cooldown_until = Some(row.cooldown_until.map(|t| t.max(until)).unwrap_or(until));
            }
        }
        // partial（引擎故障）：不计站点失败、不清零、不改冷却，只累计引擎失败证据
        STATUS_PARTIAL => {
            row.fail_engine_count = row.fail_engine_count.saturating_add(1);
        }
        _ => {}
    }
    if o.usage {
        row.search_count = row.search_count.saturating_add(1);
        row.hit_count = row.hit_count.saturating_add(o.hits.max(0));
        if o.parsed {
            row.parse_ok_count = row.parse_ok_count.saturating_add(1);
        }
        let (samples, p90) = push_ttfb_sample(row.ttfb_samples_json.as_deref(), o.latency_ms);
        row.ttfb_samples_json = Some(samples);
        row.ttfb_ms_p90 = p90;
    }
    row
}

/// TTFB 环形样本（最多 32 条）+ p90；只用于评分，不做长期存储。
fn push_ttfb_sample(raw: Option<&str>, latency_ms: i64) -> (String, Option<i64>) {
    let mut samples: Vec<i64> = raw
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    if latency_ms > 0 {
        samples.push(latency_ms);
    }
    if samples.len() > 32 {
        samples.drain(0..samples.len() - 32);
    }
    let mut sorted = samples.clone();
    sorted.sort_unstable();
    let p90 = if sorted.is_empty() {
        None
    } else {
        let idx = ((sorted.len() - 1) as f32 * 0.9).round() as usize;
        Some(sorted[idx.min(sorted.len() - 1)])
    };
    (
        serde_json::to_string(&samples).unwrap_or_else(|_| "[]".to_string()),
        p90,
    )
}

fn score_input(row: &HealthRow, rule_complete: f32, network: Network) -> ScoreInput {
    ScoreInput {
        rule_completeness: rule_complete,
        network,
        consecutive_failures: row.consecutive_failures,
        search_count: row.search_count,
        hit_count: row.hit_count,
        parse_ok_count: row.parse_ok_count,
        fail_site_count: row.fail_site_count,
        fail_engine_count: row.fail_engine_count,
        avg_ttfb_ms: row.avg_latency_ms,
        p90_ttfb_ms: row.ttfb_ms_p90,
        prev_tier: if row.tier.trim().is_empty() {
            source_quality::TIER_OBSERVING.to_string()
        } else {
            row.tier.clone()
        },
        promote_streak: row.promote_streak,
        demote_streak: row.demote_streak,
    }
}

fn apply_evaluation(
    row: &mut HealthRow,
    eval: &Evaluation,
    network: Network,
    rule_complete: f32,
    now: i64,
) {
    row.tier = eval.tier.clone();
    row.score = eval.score as i64;
    row.promote_streak = eval.promote_streak;
    row.demote_streak = eval.demote_streak;
    row.last_eval_at = Some(now);
    let evidence = serde_json::json!({
        "network": network.as_str(),
        "rule_completeness": (rule_complete * 100.0).round() / 100.0,
        "search_count": row.search_count,
        "hit_count": row.hit_count,
        "hit_rate": if row.search_count > 0 {
            (row.hit_count as f64 / row.search_count as f64 * 1000.0).round() / 1000.0
        } else { 0.0 },
        "parse_ok_count": row.parse_ok_count,
        "parse_rate": if row.search_count > 0 {
            (row.parse_ok_count as f64 / row.search_count as f64 * 1000.0).round() / 1000.0
        } else { 0.0 },
        "avg_ttfb_ms": row.avg_latency_ms,
        "p90_ttfb_ms": row.ttfb_ms_p90,
        "fail_site_count": row.fail_site_count,
        "fail_engine_count": row.fail_engine_count,
        "consecutive_failures": row.consecutive_failures,
        "score": eval.score,
        "tier": eval.tier,
        "reasons": eval.reasons,
        "last_eval_at": now,
    });
    row.evidence_json = Some(evidence.to_string());
}

/// 批量落库：只在任务收尾时调用一次，避免每本书/每条结果写库。
pub async fn record_outcomes(
    pool: &SqlitePool,
    outcomes: &[Outcome],
    cooldown_secs: u64,
    cfg: &TierConfig,
) -> AppResult<()> {
    if outcomes.is_empty() {
        return Ok(());
    }
    let now = now_ms();
    let existing = load_map(pool).await?;
    let mut tx = pool.begin().await?;
    for o in outcomes {
        let mut row = apply_outcome(existing.get(&o.source_url), o, now, cooldown_secs);
        let network = Network::from_status(o.status, true);
        let eval = source_quality::evaluate(&score_input(&row, o.rule_complete, network), cfg);
        apply_evaluation(&mut row, &eval, network, o.rule_complete, now);
        sqlx::query(
            "INSERT INTO source_health \
             (source_url, last_checked_at, last_status, last_error, consecutive_failures, \
              last_ok_at, cooldown_until, avg_latency_ms, check_total, tier, score, \
              evidence_json, last_eval_at, search_count, hit_count, parse_ok_count, \
              fail_site_count, fail_engine_count, ttfb_ms_p90, ttfb_samples_json, \
              promote_streak, demote_streak) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
              ?17, ?18, ?19, ?20, ?21, ?22) \
             ON CONFLICT(source_url) DO UPDATE SET \
              last_checked_at=excluded.last_checked_at, \
              last_status=excluded.last_status, \
              last_error=excluded.last_error, \
              consecutive_failures=excluded.consecutive_failures, \
              last_ok_at=excluded.last_ok_at, \
              cooldown_until=excluded.cooldown_until, \
              avg_latency_ms=excluded.avg_latency_ms, \
              check_total=excluded.check_total, \
              tier=excluded.tier, score=excluded.score, \
              evidence_json=excluded.evidence_json, last_eval_at=excluded.last_eval_at, \
              search_count=excluded.search_count, hit_count=excluded.hit_count, \
              parse_ok_count=excluded.parse_ok_count, \
              fail_site_count=excluded.fail_site_count, \
              fail_engine_count=excluded.fail_engine_count, \
              ttfb_ms_p90=excluded.ttfb_ms_p90, \
              ttfb_samples_json=excluded.ttfb_samples_json, \
              promote_streak=excluded.promote_streak, \
              demote_streak=excluded.demote_streak",
        )
        .bind(&row.source_url)
        .bind(row.last_checked_at)
        .bind(&row.last_status)
        .bind(&row.last_error)
        .bind(row.consecutive_failures)
        .bind(row.last_ok_at)
        .bind(row.cooldown_until)
        .bind(row.avg_latency_ms)
        .bind(row.check_total)
        .bind(&row.tier)
        .bind(row.score)
        .bind(&row.evidence_json)
        .bind(row.last_eval_at)
        .bind(row.search_count)
        .bind(row.hit_count)
        .bind(row.parse_ok_count)
        .bind(row.fail_site_count)
        .bind(row.fail_engine_count)
        .bind(row.ttfb_ms_p90)
        .bind(&row.ttfb_samples_json)
        .bind(row.promote_streak)
        .bind(row.demote_streak)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 纯评估：导入流程在写库前需要先拿到 tier/score 来决定三态与启用位。
/// `prev_tier_override` 只用于“隔离区复检通过”的显式复活。
pub fn evaluate_source(
    row: Option<&HealthRow>,
    rule_complete: f32,
    network: Network,
    prev_tier_override: Option<&str>,
    cfg: &TierConfig,
) -> Evaluation {
    let owned = HealthRow::default();
    let base = row.unwrap_or(&owned);
    let mut input = score_input(base, rule_complete, network);
    if let Some(t) = prev_tier_override {
        input.prev_tier = t.to_string();
    }
    source_quality::evaluate(&input, cfg)
}

/// 对“没有新观测”的源做静态评估（导入时 network=skipped，不因测不到判坏）。
/// 已隔离的源只有显式 `prev_tier_override=Some("observing")` 才会重评复活。
/// `force_tier` 用于导入决策与评分结论冲突时以隔离决策为准。
pub async fn record_evaluation(
    pool: &SqlitePool,
    source_url: &str,
    rule_complete: f32,
    network: Network,
    prev_tier_override: Option<&str>,
    force_tier: Option<&str>,
    cfg: &TierConfig,
) -> AppResult<Evaluation> {
    let row: Option<HealthRow> = sqlx::query_as(
        "SELECT source_url, last_checked_at, last_status, last_error, \
         consecutive_failures, last_ok_at, cooldown_until, avg_latency_ms, check_total, \
         tier, score, evidence_json, last_eval_at, search_count, hit_count, parse_ok_count, \
         fail_site_count, fail_engine_count, ttfb_ms_p90, ttfb_samples_json, \
         promote_streak, demote_streak \
         FROM source_health WHERE source_url = ?",
    )
    .bind(source_url)
    .fetch_optional(pool)
    .await?;
    let mut row = row.unwrap_or_else(|| {
        let mut r = HealthRow::default();
        r.source_url = source_url.to_string();
        r
    });
    let mut input = score_input(&row, rule_complete, network);
    if let Some(t) = prev_tier_override {
        input.prev_tier = t.to_string();
    }
    let mut eval = source_quality::evaluate(&input, cfg);
    if let Some(t) = force_tier {
        if eval.tier != t {
            eval.tier = t.to_string();
            eval.reasons.push(format!("forced_tier={t}"));
        }
    }
    apply_evaluation(&mut row, &eval, network, rule_complete, now_ms());
    sqlx::query(
        "INSERT INTO source_health \
         (source_url, tier, score, evidence_json, last_eval_at, promote_streak, demote_streak) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(source_url) DO UPDATE SET \
          tier=excluded.tier, score=excluded.score, evidence_json=excluded.evidence_json, \
          last_eval_at=excluded.last_eval_at, promote_streak=excluded.promote_streak, \
          demote_streak=excluded.demote_streak",
    )
    .bind(source_url)
    .bind(&row.tier)
    .bind(row.score)
    .bind(&row.evidence_json)
    .bind(row.last_eval_at)
    .bind(row.promote_streak)
    .bind(row.demote_streak)
    .execute(pool)
    .await?;
    Ok(eval)
}

/// 搜索准入：只允许 `preferred` / `observing`；缺失健康行按 observing 兼容历史数据。
/// 用白名单而不是“非 quarantined”，避免未知/历史脏值（如空串）混进聚合。
pub fn visible_in_search(row: Option<&HealthRow>) -> bool {
    row.map(|h| {
        matches!(
            h.tier.as_str(),
            source_quality::TIER_PREFERRED | source_quality::TIER_OBSERVING
        )
    })
    .unwrap_or(true)
}

/// 该源当前是否 preferred（用于两批调度的第一批）。
pub fn is_preferred(row: Option<&HealthRow>) -> bool {
    row.map(|h| h.tier.as_str() == source_quality::TIER_PREFERRED)
        .unwrap_or(false)
}

/// 清空某源或全部健康记录，返回清理行数。
pub async fn reset(pool: &SqlitePool, source_ids: &[String]) -> AppResult<u64> {
    if source_ids.is_empty() {
        let r = sqlx::query("DELETE FROM source_health").execute(pool).await?;
        return Ok(r.rows_affected());
    }
    let mut cleared = 0u64;
    for id in source_ids {
        let r = sqlx::query("DELETE FROM source_health WHERE source_url = ?")
            .bind(id)
            .execute(pool)
            .await?;
        cleared += r.rows_affected();
    }
    Ok(cleared)
}

/// 只跑一次“搜索”探测来定性（不跑目录 + 首图链路，避免放大源站压力）。
/// 并发 ≤ [`PROBE_CONCURRENCY`]，单源预算沿用 `req::search_budget()`，
/// 整批受 [`PROBE_TOTAL_BUDGET_SECS`] 约束；超时未开始的源返回 `never`。
pub async fn probe_many(sources: Vec<BookSource>, key: &str) -> Vec<Probe> {
    // 探测去重：严格按 bookSourceUrl 精确匹配，每个源只探一次；
    // 同站不同变体的 URL 不同 → 各自独立探测，结果不共享。
    let mut seen = std::collections::HashSet::new();
    let sources: Vec<BookSource> = sources
        .into_iter()
        .filter(|s| seen.insert(s.book_source_url.clone()))
        .collect();
    let total_budget = Duration::from_secs(PROBE_TOTAL_BUDGET_SECS);
    let deadline = Instant::now() + total_budget;
    let per_source = crate::req::search_budget();
    let key = key.trim().to_string();
    let key = if key.is_empty() {
        DEFAULT_PROBE_KEY.to_string()
    } else {
        key
    };

    let results = stream::iter(sources.into_iter().enumerate().map(|(idx, src)| {
        let key = key.clone();
        async move {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining < Duration::from_millis(200) {
                return (
                    idx,
                    Probe {
                        source_url: src.book_source_url.clone(),
                        name: src.book_source_name.clone(),
                        status: STATUS_NEVER,
                        latency_ms: 0,
                        error: Some("未体检：批量预算已用尽".into()),
                        probed: false,
                        hits: 0,
                    },
                );
            }
            let budget = per_source.min(remaining);
            let started = Instant::now();
            let r = tokio::time::timeout(budget, web_book::search(&src, &key, 1)).await;
            let latency_ms = started.elapsed().as_millis() as i64;
            let (status, error, hits) = match r {
                // 规则跑通但 0 条结果：引擎/关键词问题，不判站点坏。
                Ok(Ok(v)) if v.is_empty() => (
                    STATUS_PARTIAL,
                    Some("规则未产出结果".to_string()),
                    0,
                ),
                Ok(Ok(v)) => (STATUS_OK, None, v.len() as i64),
                Ok(Err(e)) => {
                    let kind = classify(&e);
                    (
                        if kind == FailureKind::Site {
                            STATUS_FAIL
                        } else {
                            STATUS_PARTIAL
                        },
                        Some(e.to_string()),
                        0,
                    )
                }
                Err(_) => (
                    STATUS_FAIL,
                    Some(format!(
                        "搜索超时（单源 {}s 预算）",
                        budget.as_secs().max(1)
                    )),
                    0,
                ),
            };
            (
                idx,
                Probe {
                    source_url: src.book_source_url.clone(),
                    name: src.book_source_name.clone(),
                    status,
                    latency_ms,
                    error,
                    probed: true,
                    hits,
                },
            )
        }
    }))
    .buffer_unordered(PROBE_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    let mut results = results;
    results.sort_by_key(|(idx, _)| *idx);
    results.into_iter().map(|(_, p)| p).collect()
}

/// 只把实际体检过的结果转成落库记录；`never` 不写库（保持“未测”）。
pub fn outcomes_from_probes(probes: &[Probe]) -> Vec<Outcome> {
    probes
        .iter()
        .filter(|p| p.probed)
        .map(|p| Outcome {
            source_url: p.source_url.clone(),
            status: p.status,
            latency_ms: p.latency_ms,
            error: p.error.clone(),
            hits: p.hits,
            parsed: p.status == STATUS_OK,
            usage: false,
            rule_complete: 1.0,
        })
        .collect()
}

/// 统计候选源里处于冷却中的数量（用于“另有 N 个源处于冷却”）。
pub fn cooled_count(sources: &[BookSource], health: &HashMap<String, HealthRow>, now: i64) -> usize {
    sources
        .iter()
        .filter(|s| {
            health
                .get(&s.book_source_url)
                .map(|h| h.cooling(now))
                .unwrap_or(false)
        })
        .count()
}

pub struct Partition {
    /// 本次真正参与搜索的源。
    pub active: Vec<BookSource>,
    /// 因冷却被跳过的源数。
    pub skipped: usize,
}

/// 默认剔除冷却中的源；`include_cooled=true` 时全部参与（便于排查）。
pub fn partition_cooldown(
    sources: Vec<BookSource>,
    health: &HashMap<String, HealthRow>,
    include_cooled: bool,
    now: i64,
) -> Partition {
    if include_cooled {
        return Partition {
            active: sources,
            skipped: 0,
        };
    }
    let mut active = Vec::with_capacity(sources.len());
    let mut skipped = 0;
    for s in sources {
        if health
            .get(&s.book_source_url)
            .map(|h| h.cooling(now))
            .unwrap_or(false)
        {
            skipped += 1;
        } else {
            active.push(s);
        }
    }
    Partition { active, skipped }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(url: &str, name: &str) -> BookSource {
        let mut s = BookSource::default();
        s.book_source_url = url.into();
        s.book_source_name = name.into();
        s.enabled = true;
        s
    }

    fn outcome(url: &str, status: &'static str) -> Outcome {
        Outcome {
            source_url: url.into(),
            status,
            latency_ms: 12,
            error: None,
            hits: 0,
            parsed: status == STATUS_OK,
            usage: false,
            rule_complete: 1.0,
        }
    }

    fn test_tier_cfg() -> TierConfig {
        use crate::source_quality::Weights;
        TierConfig {
            preferred_min_score: 70,
            quarantine_below_score: 30,
            promote_streak: 3,
            demote_streak: 2,
            update_score_margin: 5,
            preferred_budget_pct: 70,
            weights: Weights::default(),
        }
    }

    // 站点失败连续 3 次才冷却；第 2 次仍不冷却
    #[test]
    fn site_failures_cool_at_threshold() {
        let mut row: Option<HealthRow> = None;
        for i in 1..=3 {
            let next = apply_outcome(row.as_ref(), &outcome("https://a.com", STATUS_FAIL), 1000, 86400);
            if i < FAIL_THRESHOLD {
                assert!(next.cooldown_until.is_none(), "第 {i} 次不应冷却");
            }
            row = Some(next);
        }
        let row = row.unwrap();
        assert_eq!(row.consecutive_failures, 3);
        assert_eq!(row.cooldown_until, Some(1000 + 86400 * 1000));
    }

    // 成功一次清零并解除冷却
    #[test]
    fn success_resets_failures_and_cooldown() {
        let mut row = apply_outcome(None, &outcome("https://a.com", STATUS_FAIL), 1000, 86400);
        row = apply_outcome(Some(&row), &outcome("https://a.com", STATUS_FAIL), 2000, 86400);
        row = apply_outcome(Some(&row), &outcome("https://a.com", STATUS_FAIL), 3000, 86400);
        assert!(row.cooling(3000));
        let row = apply_outcome(Some(&row), &outcome("https://a.com", STATUS_OK), 4000, 86400);
        assert_eq!(row.consecutive_failures, 0);
        assert_eq!(row.cooldown_until, None);
        assert_eq!(row.last_ok_at, Some(4000));
    }

    // 引擎失败（partial）不计失败、不清零、不冷却——这是与站点故障的关键区别
    #[test]
    fn engine_failures_do_not_cool() {
        let mut row = apply_outcome(None, &outcome("https://a.com", STATUS_FAIL), 1000, 86400);
        row = apply_outcome(Some(&row), &outcome("https://a.com", STATUS_FAIL), 2000, 86400);
        assert_eq!(row.consecutive_failures, 2);
        for _ in 0..10 {
            row = apply_outcome(Some(&row), &outcome("https://a.com", STATUS_PARTIAL), 3000, 86400);
        }
        assert_eq!(row.consecutive_failures, 2, "引擎失败不应累加");
        assert_eq!(row.last_status, STATUS_PARTIAL);
        assert!(row.cooldown_until.is_none(), "引擎失败不应冷却");
    }

    // 冷却中的源默认跳过；include_cooled 时全部参与
    #[test]
    fn partition_skips_cooled_by_default() {
        let sources = vec![src("https://a.com", "A"), src("https://b.com", "B"), src("https://c.com", "C")];
        let mut health = HashMap::new();
        let mut cooled = HealthRow::default();
        cooled.source_url = "https://b.com".into();
        cooled.cooldown_until = Some(10_000);
        health.insert("https://b.com".into(), cooled);

        let p = partition_cooldown(sources.clone(), &health, false, 5_000);
        assert_eq!(p.active.len(), 2);
        assert_eq!(p.skipped, 1);
        assert!(p.active.iter().all(|s| s.book_source_url != "https://b.com"));

        let p = partition_cooldown(sources.clone(), &health, true, 5_000);
        assert_eq!(p.active.len(), 3);
        assert_eq!(p.skipped, 0);
        assert_eq!(cooled_count(&sources, &health, 5_000), 1);
        // 冷却到期后不再计为冷却
        assert_eq!(cooled_count(&sources, &health, 10_000), 0);
    }

    // 站点不可达错误带标记 → Site；普通规则错误 → Engine
    #[test]
    fn classify_site_vs_engine() {
        let site = site_error("http status 404");
        assert_eq!(classify(&site), FailureKind::Site);
        let engine = anyhow::anyhow!("选择器未命中");
        assert_eq!(classify(&engine), FailureKind::Engine);
    }

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE source_health ( \
             source_url TEXT PRIMARY KEY, last_checked_at INTEGER, \
             last_status TEXT NOT NULL DEFAULT 'never', last_error TEXT, \
             consecutive_failures INTEGER NOT NULL DEFAULT 0, last_ok_at INTEGER, \
             cooldown_until INTEGER, avg_latency_ms INTEGER, check_total INTEGER NOT NULL DEFAULT 0, \
             tier TEXT NOT NULL DEFAULT 'observing', score INTEGER NOT NULL DEFAULT 50, \
             evidence_json TEXT, last_eval_at INTEGER, \
             search_count INTEGER NOT NULL DEFAULT 0, hit_count INTEGER NOT NULL DEFAULT 0, \
             parse_ok_count INTEGER NOT NULL DEFAULT 0, \
             fail_site_count INTEGER NOT NULL DEFAULT 0, \
             fail_engine_count INTEGER NOT NULL DEFAULT 0, \
             ttfb_ms_p90 INTEGER, ttfb_samples_json TEXT, \
             promote_streak INTEGER NOT NULL DEFAULT 0, \
             demote_streak INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn db_record_cools_after_threshold_and_success_resets() {
        let pool = test_pool().await;
        for _ in 0..FAIL_THRESHOLD {
            record_outcomes(
                &pool,
                &[Outcome {
                    source_url: "https://dead.example".into(),
                    status: STATUS_FAIL,
                    latency_ms: 30,
                    error: Some("http status 404".into()),
                    hits: 0,
                    parsed: false,
                    usage: false,
                    rule_complete: 1.0,
                }],
                86400,
                &test_tier_cfg(),
            )
            .await
            .unwrap();
        }
        let map = load_map(&pool).await.unwrap();
        let row = map.get("https://dead.example").unwrap();
        assert_eq!(row.consecutive_failures, FAIL_THRESHOLD);
        assert!(row.cooldown_until.unwrap() > now_ms());

        record_outcomes(
            &pool,
            &[Outcome {
                source_url: "https://dead.example".into(),
                status: STATUS_OK,
                latency_ms: 12,
                error: None,
                hits: 0,
                parsed: true,
                usage: false,
                rule_complete: 1.0,
            }],
            86400,
            &test_tier_cfg(),
        )
        .await
        .unwrap();
        let map = load_map(&pool).await.unwrap();
        let row = map.get("https://dead.example").unwrap();
        assert_eq!(row.consecutive_failures, 0);
        assert_eq!(row.cooldown_until, None);
        assert_eq!(row.check_total, FAIL_THRESHOLD + 1);
    }

    #[tokio::test]
    async fn db_engine_failure_does_not_cool() {
        let pool = test_pool().await;
        for _ in 0..2 {
            record_outcomes(
                &pool,
                &[Outcome {
                    source_url: "https://engine.example".into(),
                    status: STATUS_FAIL,
                    latency_ms: 20,
                    error: None,
                    hits: 0,
                    parsed: false,
                    usage: false,
                    rule_complete: 1.0,
                }],
                86400,
                &test_tier_cfg(),
            )
            .await
            .unwrap();
        }
        for _ in 0..10 {
            record_outcomes(
                &pool,
                &[Outcome {
                    source_url: "https://engine.example".into(),
                    status: STATUS_PARTIAL,
                    latency_ms: 20,
                    error: Some("selector miss".into()),
                    hits: 0,
                    parsed: false,
                    usage: false,
                    rule_complete: 1.0,
                }],
                86400,
                &test_tier_cfg(),
            )
            .await
            .unwrap();
        }
        let map = load_map(&pool).await.unwrap();
        let row = map.get("https://engine.example").unwrap();
        assert_eq!(row.consecutive_failures, 2);
        assert_eq!(row.cooldown_until, None);
        assert_eq!(row.last_status, STATUS_PARTIAL);
    }

    #[test]
    fn host_counting_does_not_change_source_keys() {
        let sources = vec![
            src("https://a.com/v1", "A1"),
            src("https://a.com/v2", "A2"),
            src("https://b.com/x", "B"),
        ];
        assert_eq!(distinct_host_count(&sources), 2);
        assert_eq!(host_of("https://a.com/v1"), Some("a.com".into()));
    }

    #[test]
    fn error_truncation_is_char_safe() {
        let long = "错".repeat(MAX_ERROR_CHARS + 5);
        let cut = truncate_error(&long);
        assert_eq!(cut.chars().count(), MAX_ERROR_CHARS + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(truncate_error("短"), "短");
    }
}
