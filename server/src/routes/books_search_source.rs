//! books 路由的 search_source 部分（move-only 从 books.rs 拆出）。

use super::*;

/// 解析逗号分隔的 source_ids，去空、去重并保留用户输入顺序。
pub(super) fn parse_source_ids(raw: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(raw) = raw {
        for tok in raw.split(',') {
            let t = tok.trim();
            if !t.is_empty() && !out.iter().any(|x| x == t) {
                out.push(t.to_string());
            }
        }
    }
    out
}

/// 在「已排序的启用书源」中按 id 选择：
/// - 优先精确匹配 `book_source_url`（书源主键）；
/// - 纯数字按该列表的 1-based 序号解析（兼容文档里的 `source_ids=1,2`）；
/// - 解析不到/不存在/禁用的 id 一律忽略，由调用方在响应里提示。
pub(super) fn select_sources(
    enabled: &[crate::entities::BookSource],
    requested: &[String],
) -> (Vec<crate::entities::BookSource>, Vec<String>) {
    if requested.is_empty() {
        return (enabled.to_vec(), Vec::new());
    }
    let mut selected: Vec<crate::entities::BookSource> = Vec::new();
    let mut ignored = Vec::new();
    for tok in requested {
        let found = enabled
            .iter()
            .find(|s| s.book_source_url == *tok)
            .or_else(|| {
                tok.parse::<usize>()
                    .ok()
                    .filter(|n| *n >= 1)
                    .and_then(|n| enabled.get(n - 1))
            });
        match found {
            Some(s) => {
                if !selected
                    .iter()
                    .any(|x| x.book_source_url == s.book_source_url)
                {
                    selected.push(s.clone());
                }
            }
            None => ignored.push(tok.clone()),
        }
    }
    (selected, ignored)
}

/// 单个源是否可参与聚合搜索：所有入口（两段式 / legacy / `/k`）必须共用这一口径。
/// 规则层判定（启用/类型/搜索规则/非登录源）直接复用 `entities::is_searchable_source`，
/// 这里再叠加隔离区与健康档；`enabled=0` 与需登录源都不会进入 source_total，也不会被 spawn。
pub(crate) fn is_searchable_source(
    x: &crate::entities::BookSource,
    quarantined: &HashSet<String>,
    health: &HashMap<String, source_health::HealthRow>,
) -> bool {
    crate::entities::is_searchable_source(x)
        && !quarantined.contains(&x.book_source_url)
        && source_health::visible_in_search(health.get(&x.book_source_url))
}

/// 聚合搜索的唯一准入闸门：先规则层 `is_searchable_source`（含非登录源），
/// 再 tier（只放行 preferred / observing）。
pub(super) fn searchable_sources(
    sources: Vec<crate::entities::BookSource>,
    quarantined: &HashSet<String>,
    health: &HashMap<String, source_health::HealthRow>,
) -> Vec<crate::entities::BookSource> {
    sources
        .into_iter()
        .filter(|x| is_searchable_source(x, quarantined, health))
        .collect()
}

/// 精选集合不可用时的第一级退化：只保留 preferred 档。
pub(super) fn preferred_sources(
    candidates: Vec<crate::entities::BookSource>,
    health: &HashMap<String, source_health::HealthRow>,
) -> Vec<crate::entities::BookSource> {
    candidates
        .into_iter()
        .filter(|s| source_health::is_preferred(health.get(&s.book_source_url)))
        .collect()
}

/// 按 scope 过滤候选源：
/// - `all`：原样返回（全量源）；
/// - `curated`：只保留体检精选集合（`source_curated` 表）；集合为空/未就绪则退回全量。
pub(super) fn filter_sources_by_scope(
    candidates: Vec<crate::entities::BookSource>,
    curated: Option<&std::collections::HashSet<String>>,
    scope: crate::req::SearchScope,
) -> Vec<crate::entities::BookSource> {
    if scope == crate::req::SearchScope::All {
        return candidates;
    }
    match curated {
        Some(urls) => candidates
            .into_iter()
            .filter(|s| urls.contains(&s.book_source_url))
            .collect(),
        None => candidates,
    }
}

/// 精选集合缺失（`curated_total == 0`）时的退化目标。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CuratedMissingFallback {
    /// 退化到全量可搜索源（`/api` 语义：Web 端可等，文案标注已回退）。
    All,
    /// 退化到 preferred 层（`/k` 语义：KPW3 服务端渲染必须首屏快）。
    Preferred,
}

/// 统一源过滤结果：`/api/search/start`、legacy `/api/search`、`/k/search` 共用。
pub(crate) struct SearchSourcePlan {
    /// 最终参与搜索的源（scope 过滤 + 冷却分区后）
    pub(crate) active: Vec<crate::entities::BookSource>,
    pub(crate) health: HashMap<String, source_health::HealthRow>,
    /// 用户显式指定的源（空 = 未指定）
    pub(crate) requested: Vec<String>,
    pub(crate) ignored: Vec<String>,
    /// 实际生效的 scope（显式源会提升为 all）
    pub(crate) scope: crate::req::SearchScope,
    /// `user`（显式源，跳过 curated 收缩）/ `curated` / `curated_fallback` / `all_fallback` / `all`
    pub(crate) source_mode: &'static str,
    /// `curated` / `expanded`
    pub(crate) coverage: &'static str,
    pub(crate) curated_total: usize,
    /// 生效源集合大小（冷却分区之前；0 表示确实没有可搜索的源）
    pub(crate) source_total: usize,
    /// 可搜索源中因精选收缩/退化而未进入候选集合的数量（显式源恒为 0）
    pub(crate) scope_filtered: usize,
    pub(crate) skipped_cooldown: usize,
    pub(crate) cooldown_total: usize,
}

/// 统一入队顺序：慢源 / 近期站点失败源垫底，预算紧张时优先把并发槽给可用源。
///
/// 两段式搜索原本就按健康顺序入队；legacy `/api/search`、`/api/search/aggregate`
/// 与 `/k/search` 也必须共用同一顺序，否则同一关键词会因入口不同而拿到不同结果。
pub(crate) fn order_by_health(
    mut sources: Vec<crate::entities::BookSource>,
    health: &HashMap<String, source_health::HealthRow>,
) -> Vec<crate::entities::BookSource> {
    sources.sort_by_key(|s| books_search_job::health_order_key(health.get(&s.book_source_url)));
    sources
}

/// 所有搜索入口唯一的源过滤链路。
///
/// 语义：
/// - 显式 `source_ids`：跳过 curated 收缩（否则非优选源会被静默丢弃），`source_mode=user`；
/// - `scope=curated`：只保留体检精选集合；集合缺失时按 `curated_missing` 退化
///   （`/api` 退全量、`/k` 退 preferred），交集为空时退化为 preferred（再为空退全量）；
/// - `scope=all`：全部可搜索源；
/// - 最后统一做冷却分区（`include_cooled` 为真则全部参与）。
pub(crate) async fn resolve_search_sources(
    pool: &SqlitePool,
    sources: Vec<crate::entities::BookSource>,
    requested: &[String],
    scope: crate::req::SearchScope,
    include_cooled: bool,
    curated_missing: CuratedMissingFallback,
) -> SearchSourcePlan {
    let quarantined = source_ledger::quarantined_set(pool)
        .await
        .unwrap_or_default();
    let health = source_health::load_map(pool).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "load source health failed; search without cooldown filter");
        HashMap::new()
    });
    let searchable_all = searchable_sources(sources, &quarantined, &health);
    let explicit = !requested.is_empty();
    let curated = if explicit || scope == crate::req::SearchScope::Curated {
        // 显式源不需要精选集合；只有真正走 curated 才读表。
        if explicit {
            None
        } else {
            match crate::source_bench::curated_set(pool).await {
                Ok(set) if !set.is_empty() => Some(set),
                Ok(_) => {
                    tracing::info!(target: "search_job", reason = "cold_start_no_curated", "curated empty; fallback to preferred");
                    None
                }
                Err(e) => {
                    tracing::warn!(error = %e, "load curated set failed; fallback to preferred");
                    None
                }
            }
        }
    } else {
        None
    };
    let curated_total = curated.as_ref().map(|s| s.len()).unwrap_or(0);
    let searchable_total = searchable_all.len();
    // 退化链：显式源 → 精选集合 → preferred → 全量。显式选源始终覆盖全部可搜索源。
    let (candidates, source_mode, coverage, effective_scope) = if explicit {
        (
            searchable_all.clone(),
            "user",
            "expanded",
            crate::req::SearchScope::All,
        )
    } else if scope == crate::req::SearchScope::Curated {
        match curated.as_ref() {
            Some(set) => {
                let filtered = filter_sources_by_scope(searchable_all.clone(), Some(set), scope);
                if filtered.is_empty() {
                    tracing::info!(target: "search_job", reason = "curated_no_intersection", "fallback to preferred");
                    (
                        preferred_sources(searchable_all.clone(), &health),
                        "curated_fallback",
                        "expanded",
                        crate::req::SearchScope::Curated,
                    )
                } else {
                    (
                        filtered,
                        "curated",
                        "curated",
                        crate::req::SearchScope::Curated,
                    )
                }
            }
            None => match curated_missing {
                CuratedMissingFallback::All => (
                    searchable_all.clone(),
                    "curated_fallback",
                    "expanded",
                    crate::req::SearchScope::All,
                ),
                // `/k` 不退全量：只跑 preferred 层，保住首屏耗时（预算仍随 curated）。
                CuratedMissingFallback::Preferred => (
                    preferred_sources(searchable_all.clone(), &health),
                    "curated_fallback",
                    "expanded",
                    crate::req::SearchScope::Curated,
                ),
            },
        }
    } else {
        (
            searchable_all.clone(),
            "all",
            "expanded",
            crate::req::SearchScope::All,
        )
    };
    let candidate_len = candidates.len();
    let (mut selected, ignored) = select_sources(&candidates, requested);
    let mut effective = effective_scope;
    let mut source_mode = source_mode;
    let mut coverage = coverage;
    let mut scope_filtered = if explicit || effective == crate::req::SearchScope::All {
        0
    } else {
        searchable_total.saturating_sub(candidate_len)
    };
    if !explicit && effective == crate::req::SearchScope::Curated && selected.is_empty() {
        // preferred 也为空（全新部署/新导入源默认 observing）：退到最后一层「全部可搜索源」。
        effective = crate::req::SearchScope::All;
        source_mode = "all_fallback";
        coverage = "expanded";
        scope_filtered = 0;
        selected = select_sources(&searchable_all, &[]).0;
    }
    let source_total = selected.len();
    let cooldown_base = if effective == crate::req::SearchScope::All {
        &searchable_all
    } else {
        &candidates
    };
    let now = source_health::now_ms();
    let cooldown_total = source_health::cooled_count(cooldown_base, &health, now);
    let partition = source_health::partition_cooldown(selected, &health, include_cooled, now);
    let skipped_cooldown = partition.skipped;
    SearchSourcePlan {
        active: partition.active,
        health,
        requested: requested.to_vec(),
        ignored,
        scope: effective,
        source_mode,
        coverage,
        curated_total,
        source_total,
        scope_filtered,
        skipped_cooldown,
        cooldown_total,
    }
}
