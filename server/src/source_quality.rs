//! 书源质量评分与 tier 分层。
//!
//! 这里是**纯函数**：输入统计 + 规则完整度 + 最近连通性，输出 0–100 分与
//! `preferred / observing / quarantined`。数据库读写留在 `source_health.rs`，
//! 导入与搜索只负责把真实观测喂进来，避免评分规则散落在业务分支里。
//!
//! 所有权重与阈值来自 [`crate::config::Config`]（`BORANLIB_SOURCE_TIER_*`），
//! 不在这里写死任何源名/URL 特判。

use crate::config::Config;
use crate::entities::BookSource;

pub const TIER_PREFERRED: &str = "preferred";
pub const TIER_OBSERVING: &str = "observing";
pub const TIER_QUARANTINED: &str = "quarantined";

/// 最近一次连通性观测。`Skipped` 表示本次导入没拿到网络（静态评估），
/// 它按中性值参与评分，绝不因为“测不到”判坏。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Ok,
    Partial,
    Fail,
    Skipped,
    Never,
}

impl Network {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
            Self::Never => "never",
        }
    }

    pub fn from_status(status: &str, probed: bool) -> Self {
        if !probed {
            return Self::Skipped;
        }
        match status {
            "ok" => Self::Ok,
            "partial" => Self::Partial,
            "fail" => Self::Fail,
            _ => Self::Never,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Weights {
    pub connect: i32,
    pub rule: i32,
    pub hit: i32,
    pub parse: i32,
    pub ttfb: i32,
}

impl Default for Weights {
    fn default() -> Self {
        // 合计 100；命中与解析占大头，连通性只作门槛，避免“快但没结果”的源占优。
        Self {
            connect: 25,
            rule: 15,
            hit: 30,
            parse: 15,
            ttfb: 15,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TierConfig {
    pub preferred_min_score: i32,
    pub quarantine_below_score: i32,
    pub promote_streak: i64,
    pub demote_streak: i64,
    /// 更新覆盖闸门：新版本得分至少不低于本地旧版这么多，才允许覆盖。
    /// 该闸门在导入侧计算，评分结构保留字段以便透传。
    #[allow(dead_code)]
    pub update_score_margin: i32,
    pub weights: Weights,
}

impl TierConfig {
    pub fn from_config(c: &Config) -> Self {
        Self {
            preferred_min_score: c.source_tier_preferred_min_score,
            quarantine_below_score: c.source_tier_quarantine_below_score,
            promote_streak: c.source_tier_promote_streak,
            demote_streak: c.source_tier_demote_streak,
            update_score_margin: c.source_tier_update_margin,
            weights: Weights {
                connect: c.source_score_w_connect,
                rule: c.source_score_w_rule,
                hit: c.source_score_w_hit,
                parse: c.source_score_w_parse,
                ttfb: c.source_score_w_ttfb,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScoreInput {
    pub rule_completeness: f32,
    pub network: Network,
    pub consecutive_failures: i64,
    pub search_count: i64,
    pub hit_count: i64,
    pub parse_ok_count: i64,
    pub fail_site_count: i64,
    pub fail_engine_count: i64,
    pub avg_ttfb_ms: Option<i64>,
    pub p90_ttfb_ms: Option<i64>,
    pub prev_tier: String,
    pub promote_streak: i64,
    pub demote_streak: i64,
}

impl ScoreInput {
    /// 构造「无历史」评分输入；生产路径从数据库行组装，本便捷构造仅单测使用。
    #[allow(dead_code)]
    pub fn new(rule_completeness: f32, network: Network) -> Self {
        Self {
            rule_completeness,
            network,
            consecutive_failures: 0,
            search_count: 0,
            hit_count: 0,
            parse_ok_count: 0,
            fail_site_count: 0,
            fail_engine_count: 0,
            avg_ttfb_ms: None,
            p90_ttfb_ms: None,
            prev_tier: TIER_OBSERVING.to_string(),
            promote_streak: 0,
            demote_streak: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Evaluation {
    pub score: i32,
    pub tier: String,
    pub promote_streak: i64,
    pub demote_streak: i64,
    pub reasons: Vec<String>,
}

/// 规则完整度：只算“能不能搜、能不能解析”的静态条件，不做站点/语言特判。
pub fn rule_completeness(src: &BookSource) -> f32 {
    let mut score = 0.0f32;
    if src
        .search_url
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
    {
        score += 0.35;
    }
    let rule = src.rule_search.as_ref();
    if rule
        .and_then(|r| r.book_list.as_deref())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
    {
        score += 0.35;
    }
    if rule
        .and_then(|r| r.name.as_deref())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
    {
        score += 0.10;
    }
    if src.rule_toc.is_some() {
        score += 0.10;
    }
    if src.rule_content.is_some() {
        score += 0.10;
    }
    score.clamp(0.0, 1.0)
}

fn ratio(part: i64, total: i64) -> f32 {
    if total <= 0 {
        0.5
    } else {
        (part.max(0) as f32 / total as f32).clamp(0.0, 1.0)
    }
}

fn ttfb_component(avg: Option<i64>, p90: Option<i64>) -> f32 {
    let Some(avg) = avg else { return 0.5 };
    let avg_score = (1.0 - avg.max(0) as f32 / 3000.0).clamp(0.0, 1.0);
    let p90_score = p90
        .map(|v| (1.0 - v.max(0) as f32 / 5000.0).clamp(0.0, 1.0))
        .unwrap_or(avg_score);
    (avg_score * 0.6 + p90_score * 0.4).clamp(0.0, 1.0)
}

/// 打分 + 分层（带滞回）。
///
/// 硬隔离：连续站点失败达到阈值（与 `source_health::FAIL_THRESHOLD` 同义）或
/// 最近一次站点失败且累计 >=3 次。引擎失败只进 `fail_engine_count`，不参与隔离。
pub fn evaluate(input: &ScoreInput, cfg: &TierConfig) -> Evaluation {
    let mut reasons: Vec<String> = Vec::new();
    let base_connect = match input.network {
        Network::Ok => 1.0,
        Network::Partial => 0.55,
        Network::Skipped | Network::Never => 0.5,
        Network::Fail => 0.0,
    };
    let site_rate = if input.search_count > 0 {
        ratio(input.fail_site_count, input.search_count)
    } else {
        0.0
    };
    let connect = (base_connect - site_rate * 0.5).clamp(0.0, 1.0);

    let rule = input.rule_completeness.clamp(0.0, 1.0);
    let confidence = (input.search_count.max(0) as f32 / 5.0).clamp(0.0, 1.0);
    let hit = 0.5 * (1.0 - confidence) + ratio(input.hit_count, input.search_count) * confidence;
    let parse =
        0.5 * (1.0 - confidence) + ratio(input.parse_ok_count, input.search_count) * confidence;
    let ttfb = ttfb_component(input.avg_ttfb_ms, input.p90_ttfb_ms);

    let w = &cfg.weights;
    let mut score = (w.connect as f32 * connect
        + w.rule as f32 * rule
        + w.hit as f32 * hit
        + w.parse as f32 * parse
        + w.ttfb as f32 * ttfb)
        .round() as i32;
    score = score.clamp(0, 100);

    reasons.push(format!("connect={connect:.2}"));
    reasons.push(format!("rule={rule:.2}"));
    reasons.push(format!("hit={hit:.2}"));
    reasons.push(format!("parse={parse:.2}"));
    reasons.push(format!("ttfb={ttfb:.2}"));
    if input.fail_engine_count > 0 {
        reasons.push(format!("engine_fails={}", input.fail_engine_count));
    }

    // 硬隔离优先于一切分数。
    if input.consecutive_failures >= 3
        || (input.network == Network::Fail && input.fail_site_count >= 3)
    {
        reasons.push(format!(
            "site_failures={} (threshold=3)",
            input.consecutive_failures.max(input.fail_site_count)
        ));
        return Evaluation {
            score,
            tier: TIER_QUARANTINED.to_string(),
            promote_streak: 0,
            demote_streak: input.demote_streak.saturating_add(1),
            reasons,
        };
    }

    let prev = input.prev_tier.as_str();
    let mut promote = input.promote_streak;
    let mut demote = input.demote_streak;

    // 运行期不自动复活隔离源；复活只允许走导入复检（由 sources.rs 显式传入 observing）。
    if prev == TIER_QUARANTINED {
        reasons.push("already_quarantined".into());
        return Evaluation {
            score,
            tier: TIER_QUARANTINED.to_string(),
            promote_streak: 0,
            demote_streak: demote,
            reasons,
        };
    }

    let tier = if score >= cfg.preferred_min_score {
        promote = promote.saturating_add(1);
        demote = 0;
        if prev == TIER_PREFERRED {
            reasons.push("kept_preferred".into());
            TIER_PREFERRED
        } else if promote >= cfg.promote_streak.max(1) {
            reasons.push(format!("promoted(score={score})"));
            TIER_PREFERRED
        } else {
            reasons.push(format!(
                "promote_pending({promote}/{})",
                cfg.promote_streak.max(1)
            ));
            TIER_OBSERVING
        }
    } else {
        promote = 0;
        if prev == TIER_PREFERRED {
            demote = demote.saturating_add(1);
            if demote >= cfg.demote_streak.max(1) {
                reasons.push(format!("demoted(score={score})"));
                TIER_OBSERVING
            } else {
                reasons.push(format!(
                    "demote_pending({demote}/{})",
                    cfg.demote_streak.max(1)
                ));
                TIER_PREFERRED
            }
        } else if score < cfg.quarantine_below_score && input.search_count >= 2 {
            demote = demote.saturating_add(1);
            if demote >= cfg.demote_streak.max(1) {
                reasons.push(format!("quarantined(score={score})"));
                TIER_QUARANTINED
            } else {
                reasons.push(format!(
                    "quarantine_pending({demote}/{})",
                    cfg.demote_streak.max(1)
                ));
                TIER_OBSERVING
            }
        } else {
            demote = 0;
            reasons.push("kept_observing".into());
            TIER_OBSERVING
        }
    };

    Evaluation {
        score,
        tier: tier.to_string(),
        promote_streak: promote,
        demote_streak: demote,
        reasons,
    }
}

/// 导入响应/证据里展示给人看的一句话原因（不参与计算）。生产路径直接读 `Evaluation.reasons`。
#[allow(dead_code)]
pub fn reason_summary(eval: &Evaluation) -> Vec<String> {
    eval.reasons.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TierConfig {
        TierConfig {
            preferred_min_score: 70,
            quarantine_below_score: 30,
            promote_streak: 3,
            demote_streak: 2,
            update_score_margin: 5,
            weights: Weights::default(),
        }
    }

    #[test]
    fn new_source_without_data_is_observing() {
        let e = evaluate(&ScoreInput::new(1.0, Network::Never), &cfg());
        assert_eq!(e.tier, TIER_OBSERVING);
        assert!(e.score >= 30 && e.score < 70, "score={}", e.score);
    }

    #[test]
    fn three_site_failures_quarantine_even_with_good_history() {
        let mut i = ScoreInput::new(1.0, Network::Fail);
        i.prev_tier = TIER_PREFERRED.into();
        i.consecutive_failures = 3;
        i.search_count = 20;
        i.hit_count = 20;
        i.parse_ok_count = 20;
        let e = evaluate(&i, &cfg());
        assert_eq!(e.tier, TIER_QUARANTINED);
    }

    #[test]
    fn stable_high_hits_promote_after_hysteresis() {
        let mut i = ScoreInput::new(1.0, Network::Ok);
        i.search_count = 10;
        i.hit_count = 8;
        i.parse_ok_count = 10;
        i.avg_ttfb_ms = Some(300);
        i.p90_ttfb_ms = Some(500);
        let mut tier = TIER_OBSERVING.to_string();
        let mut streak = 0;
        for _ in 0..3 {
            i.prev_tier = tier.clone();
            i.promote_streak = streak;
            let e = evaluate(&i, &cfg());
            tier = e.tier;
            streak = e.promote_streak;
        }
        assert_eq!(tier, TIER_PREFERRED);
        assert!(streak >= 3);
    }

    #[test]
    fn engine_failures_never_quarantine() {
        let mut i = ScoreInput::new(1.0, Network::Partial);
        i.search_count = 10;
        i.hit_count = 0;
        i.parse_ok_count = 0;
        i.fail_engine_count = 10;
        let e = evaluate(&i, &cfg());
        assert_ne!(e.tier, TIER_QUARANTINED, "引擎失败不能把源判死: {:?}", e);
    }

    #[test]
    fn low_score_needs_evidence_before_quarantine() {
        let mut i = ScoreInput::new(0.4, Network::Fail);
        i.search_count = 0;
        let e = evaluate(&i, &cfg());
        assert_eq!(e.tier, TIER_OBSERVING, "没有真实使用证据时只观察");
    }

    #[test]
    fn rule_completeness_uses_only_static_rules() {
        let mut src = BookSource::default();
        assert_eq!(rule_completeness(&src), 0.0);
        src.search_url = Some("https://x/search?q={key}".into());
        src.rule_search = Some(crate::entities::SearchRule {
            book_list: Some(".book".into()),
            name: Some(".name".into()),
            ..Default::default()
        });
        src.rule_toc = Some(Default::default());
        src.rule_content = Some(Default::default());
        assert!((rule_completeness(&src) - 1.0).abs() < f32::EPSILON);
    }
}
