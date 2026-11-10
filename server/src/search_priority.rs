//! 搜索入队优先级：把站点存活、查询类产出率、延迟合成一个可解释的键。
//!
//! 目标函数（见 `docs/DESIGN_SEARCH.md`）：
//!
//! ```text
//! EV(i, q) = P_yield(i | class(q)) / E[latency(i)]      // 每秒有效结果
//! score    = EV × (1 + c·sqrt(ln(N+1) / (n_i+1)))       // 少量探索加成
//! ```
//!
//! 产出率用**书名匹配后的命中**（`kept_hits`），不是解析条数：能返回 30 条无关结果
//! 的源，对当前查询仍然是 0 产出。

use std::collections::HashMap;

use sqlx::SqlitePool;

use crate::error::AppResult;
use crate::query_class::QueryClass;
use crate::source_bench::meta_hit_rate;
use crate::source_health::{self, HealthRow};
use crate::source_query_stats::{self, QueryStatsRow};

/// 没有任何证据时的产出率先验（与 `smoothed_yield` 保持一致）。
pub const DEFAULT_PRIOR: f64 = 0.30;
/// 查询类证据的伪计数强度：类内证据积累到 4 次后与全局先验等权。
const CLASS_STRENGTH: f64 = 4.0;
/// 探索系数：未观测源的 EV 最多上浮约 `1 + c·sqrt(ln(N+1))`。
const EXPLORE_C: f64 = 0.10;
const MIN_LATENCY_MS: i64 = 250;

/// 一次搜索的排序先验：查询类 + 该类全部源的历史证据 + 体检冷启动先验。
#[derive(Debug, Clone, Default)]
pub struct SearchPrior {
    pub class: QueryClass,
    pub stats: HashMap<String, QueryStatsRow>,
    /// 体检命中率（`source_curated.meta_json`），只在完全没有真实搜索证据时使用。
    pub bench_hit_rate: HashMap<String, f64>,
    /// 该类查询的历史搜索次数总和，用于 UCB 的 `ln(N+1)`。
    pub total_searches: f64,
}

impl SearchPrior {
    /// 无数据库上下文（单测/降级）时的空先验：等价于只看全局健康证据。
    pub fn empty(class: QueryClass) -> Self {
        Self {
            class,
            ..Default::default()
        }
    }

    pub async fn load(pool: &SqlitePool, class: QueryClass) -> AppResult<Self> {
        let stats = source_query_stats::load_class(pool, class).await?;
        let total_searches = stats.values().map(|r| r.search_count).sum();
        let bench_hit_rate = load_bench_hit_rate(pool).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "load bench hit rate failed; cold start prior disabled");
            HashMap::new()
        });
        Ok(Self {
            class,
            stats,
            bench_hit_rate,
            total_searches,
        })
    }

    /// 平滑产出率：类内证据为主，全局健康证据作先验；完全冷启动时用体检命中率。
    pub fn yield_for(&self, url: &str, h: Option<&HealthRow>) -> f64 {
        let global = h
            .map(source_health::smoothed_yield)
            .unwrap_or(DEFAULT_PRIOR);
        let qs = self.stats.get(url);
        let class_n = qs.map(|q| q.search_count).unwrap_or(0.0);
        let mut y = match qs {
            Some(q) if class_n > 0.0 => {
                (q.yield_ok_count + CLASS_STRENGTH * global) / (class_n + CLASS_STRENGTH)
            }
            _ => global,
        };
        // 冷启动：真实搜索证据为 0 时，用体检命中率替代「站点覆盖」先验。
        // 一旦有真实搜索，体检先验立即让位，避免热门语料长期主导冷门查询。
        let searched = h.map(|r| r.search_count).unwrap_or(0) > 0 || class_n > 0.0;
        if !searched {
            if let Some(b) = self.bench_hit_rate.get(url) {
                y = b.clamp(0.02, 0.95);
            }
        }
        y
    }

    /// 入队键（越小越先）：`(bucket, -score)`。
    /// - bucket 0：有延迟样本且最近不是站点失败
    /// - bucket 1：无延迟观测
    /// - bucket 2：最近一次是站点失败（垫底）
    pub fn priority(&self, url: &str, h: Option<&HealthRow>) -> (u8, i64) {
        let Some(h) = h else {
            return (1, 0);
        };
        let latency = h.ttfb_ms_p90.or(h.avg_latency_ms);
        if h.last_status == source_health::STATUS_FAIL {
            return (2, latency.unwrap_or(i64::MAX));
        }
        if latency.is_none() {
            return (1, 0);
        }
        let y = self.yield_for(url, Some(h));
        let ms = h
            .avg_latency_ms
            .or(h.ttfb_ms_p90)
            .unwrap_or(MIN_LATENCY_MS)
            .max(MIN_LATENCY_MS) as f64;
        let ev = y * 1_000_000.0 / (ms / 1000.0);
        let n = self.stats.get(url).map(|q| q.search_count).unwrap_or(0.0);
        let explore = 1.0 + EXPLORE_C * ((self.total_searches + 1.0).ln() / (n + 1.0)).sqrt();
        (0, -((ev * explore) as i64))
    }

    /// 本次搜索完成一个源后，就地更新先验，让后续源的顺序反映最新证据。
    /// 单次观测不做衰减（衰减只在落库时按时间差处理）。
    pub fn observe(&mut self, url: &str, kept_hits: i64) {
        let e = self.stats.entry(url.to_string()).or_default();
        e.source_url = url.to_string();
        e.query_class = self.class.as_str().to_string();
        e.search_count += 1.0;
        if kept_hits > 0 {
            e.yield_ok_count += 1.0;
        }
        e.kept_hit_count += kept_hits.max(0) as f64;
        self.total_searches += 1.0;
    }
}

async fn load_bench_hit_rate(pool: &SqlitePool) -> AppResult<HashMap<String, f64>> {
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT source_url, meta_json FROM source_curated WHERE samples > 0")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(url, meta)| {
            let r = meta_hit_rate(meta.as_deref());
            (r > 0.0).then_some((url, r))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_health::STATUS_OK;

    fn row(avg: i64, search: i64, yield_ok: i64) -> HealthRow {
        HealthRow {
            source_url: "https://a".into(),
            last_status: STATUS_OK.into(),
            avg_latency_ms: Some(avg),
            search_count: search,
            yield_ok_count: yield_ok,
            ..Default::default()
        }
    }

    #[test]
    fn class_evidence_dominates_as_it_accumulates() {
        let mut prior = SearchPrior::empty(QueryClass::Hot);
        let h = row(1000, 20, 2); // 全局产出率很低
        let before = prior.yield_for("https://a", Some(&h));
        assert!(before < 0.15);

        // 该类查询下 20 次里 18 次有产出 → 类内证据应显著抬高
        prior.stats.insert(
            "https://a".into(),
            QueryStatsRow {
                source_url: "https://a".into(),
                query_class: "hot".into(),
                search_count: 20.0,
                yield_ok_count: 18.0,
                kept_hit_count: 40.0,
                updated_at: 0,
            },
        );
        let after = prior.yield_for("https://a", Some(&h));
        assert!(after > 0.7, "类内证据应覆盖全局低产出: {after}");
    }

    #[test]
    fn bench_prior_only_before_real_searches() {
        let mut prior = SearchPrior::empty(QueryClass::Normal);
        prior.bench_hit_rate.insert("https://a".into(), 0.75);
        let cold = HealthRow::default();
        assert!((prior.yield_for("https://a", Some(&cold)) - 0.75).abs() < 1e-9);
        // 有真实搜索后不再使用体检先验
        let warm = row(1000, 10, 0);
        assert!(prior.yield_for("https://a", Some(&warm)) < 0.1);
    }

    #[test]
    fn priority_prefers_yield_and_penalises_failures() {
        let prior = SearchPrior::empty(QueryClass::Normal);
        let fruitful = row(1000, 20, 15);
        let barren = row(1000, 20, 0);
        assert!(
            prior.priority("https://a", Some(&fruitful)).1
                < prior.priority("https://a", Some(&barren)).1
        );

        let mut failed = fruitful.clone();
        failed.last_status = crate::source_health::STATUS_FAIL.into();
        assert_eq!(prior.priority("https://a", Some(&failed)).0, 2);
        assert_eq!(prior.priority("https://a", None), (1, 0));
    }

    #[test]
    fn explore_bonus_shrinks_with_samples() {
        let mut prior = SearchPrior::empty(QueryClass::Normal);
        let h = row(1000, 20, 10);
        let cold = prior.priority("https://a", Some(&h)).1;
        prior.stats.insert(
            "https://a".into(),
            QueryStatsRow {
                source_url: "https://a".into(),
                query_class: "normal".into(),
                search_count: 200.0,
                yield_ok_count: 100.0,
                kept_hit_count: 0.0,
                updated_at: 0,
            },
        );
        let warm = prior.priority("https://a", Some(&h)).1;
        // 两者 EV 相同，样本多的探索加成更小 → 键更小（更靠前）
        assert!(warm < cold, "warm={warm} cold={cold}");
    }

    #[test]
    fn observe_updates_class_stats() {
        let mut prior = SearchPrior::empty(QueryClass::Hot);
        prior.observe("https://a", 2);
        prior.observe("https://a", 0);
        let q = prior.stats.get("https://a").unwrap();
        assert_eq!(q.search_count, 2.0);
        assert_eq!(q.yield_ok_count, 1.0);
        assert_eq!(q.kept_hit_count, 2.0);
        assert_eq!(prior.total_searches, 2.0);
    }
}
