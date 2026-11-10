//! 书源健康的健康记录与探测结果模型（move-only 从 source_health.rs 拆出）。

use super::*;

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
    // ---- 搜索产出证据（0015）：书名匹配后的命中，才是「搜到了」----
    pub kept_hit_count: i64,
    pub yield_ok_count: i64,
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
            kept_hit_count: 0,
            yield_ok_count: 0,
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
    /// 本次搜索解析出的条目数（书名过滤之前）；体检时同样记录，但不算真实使用。
    pub hits: i64,
    /// 其中书名真的包含关键词的条数。只有它能证明「搜到了这本书」，
    /// 是搜索产出率（yield）的分子；`usage=false` 时不参与统计。
    pub kept_hits: i64,
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

/// 构造一个“站点不可达”错误（用于搜索超时等外部判定）。仅单测使用。
#[allow(dead_code)]
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
         tier, score, evidence_json, last_eval_at, search_count, hit_count, \
         kept_hit_count, yield_ok_count, parse_ok_count, \
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
