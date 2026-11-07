//! 书源健康的批量探测与冷却分区（move-only 从 source_health.rs 拆出）。

use super::*;

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
                Ok(Ok(v)) if v.is_empty() => {
                    (STATUS_PARTIAL, Some("规则未产出结果".to_string()), 0)
                }
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
pub fn cooled_count(
    sources: &[BookSource],
    health: &HashMap<String, HealthRow>,
    now: i64,
) -> usize {
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
