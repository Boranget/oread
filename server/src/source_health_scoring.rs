//! 书源健康的结果合并与评分（move-only 从 source_health.rs 拆出）。

use super::*;

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
                row.cooldown_until =
                    Some(row.cooldown_until.map(|t| t.max(until)).unwrap_or(until));
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
        row.kept_hit_count = row.kept_hit_count.saturating_add(o.kept_hits.max(0));
        if o.kept_hits > 0 {
            row.yield_ok_count = row.yield_ok_count.saturating_add(1);
        }
        if o.parsed {
            row.parse_ok_count = row.parse_ok_count.saturating_add(1);
        }
        let (samples, p90) = push_ttfb_sample(row.ttfb_samples_json.as_deref(), o.latency_ms);
        // 平均延迟只统计**成功**请求，并且用最近 32 次样本的均值（环形样本，
        // 历史污染会被自动淘汰）。超时/截断的 latency 等于单源预算（8s），
        // 若计入平均，会让「偶尔慢」的可用源平均延迟一路涨到预算值，最终所有源
        // 挤在同一延迟档，入队顺序退化成随机，好源被排到预算之外。
        if o.status == STATUS_OK {
            row.avg_latency_ms = mean_latency(&samples).or(Some(o.latency_ms));
        }
        row.ttfb_samples_json = Some(samples);
        row.ttfb_ms_p90 = p90;
    } else if o.status == STATUS_OK && o.latency_ms >= 0 {
        // 不计入搜索统计的结果（体检）：仍记录真实延迟；这类结果都是成功探测，
        // 没有超时污染，直接按次数滑动平均即可。
        row.avg_latency_ms = Some(match row.avg_latency_ms {
            Some(avg) if prev_total > 0 => {
                (avg.saturating_mul(prev_total) + o.latency_ms) / (prev_total + 1)
            }
            _ => o.latency_ms,
        });
    }
    row
}

/// 延迟样本（JSON 数组）的均值；空样本返回 `None`。
fn mean_latency(raw: &str) -> Option<i64> {
    let samples: Vec<i64> = serde_json::from_str(raw).ok()?;
    if samples.is_empty() {
        return None;
    }
    Some(samples.iter().sum::<i64>() / samples.len() as i64)
}

/// 平滑产出率：`kept_hits > 0` 的搜索占比，带伪计数先验。
///
/// 先验 0.30（约三成搜索能出书名匹配）、强度 4 次观测：新源/低频源不会被
/// 一两次未命中永久压底，高频源的真实产出率会迅速覆盖先验。
pub fn smoothed_yield(row: &HealthRow) -> f64 {
    const PRIOR: f64 = 0.30;
    const STRENGTH: f64 = 4.0;
    let ok = row.yield_ok_count.max(0) as f64;
    let n = row.search_count.max(0) as f64;
    (ok + PRIOR * STRENGTH) / (n + STRENGTH)
}

/// 期望「每秒有效结果」（ppm）：平滑产出率 / 成功请求的平均延迟（秒）。
///
/// 这是搜索入队的排序依据：同样快时产出高的在前；同样产出时快的在前。
/// 只用成功请求的平均延迟（[`apply_outcome`] 保证超时不计入），
/// 避免慢源被预算值污染后全体挤在同一档。
pub fn search_ev_ppm(row: &HealthRow) -> i64 {
    const MIN_LATENCY_MS: i64 = 250;
    let latency = row
        .avg_latency_ms
        .or(row.ttfb_ms_p90)
        .unwrap_or(MIN_LATENCY_MS)
        .max(MIN_LATENCY_MS);
    (smoothed_yield(row) * 1_000_000.0 / (latency as f64 / 1000.0)) as i64
}

/// TTFB 环形样本（最多 32 条）+ p90；只用于评分，不做长期存储。
pub(crate) fn push_ttfb_sample(raw: Option<&str>, latency_ms: i64) -> (String, Option<i64>) {
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

pub(crate) fn score_input(row: &HealthRow, rule_complete: f32, network: Network) -> ScoreInput {
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

pub(crate) fn apply_evaluation(
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
    let mut components = serde_json::Map::new();
    for r in &eval.reasons {
        if let Some((k, v)) = r.split_once('=') {
            if matches!(k, "connect" | "rule" | "hit" | "parse" | "ttfb") {
                if let Ok(n) = v.parse::<f64>() {
                    components.insert(k.to_string(), serde_json::json!(n));
                }
            }
        }
    }
    let evidence = serde_json::json!({
        "components": components,
        "network": network.as_str(),
        "rule_completeness": (rule_complete * 100.0).round() / 100.0,
        "search_count": row.search_count,
        "hit_count": row.hit_count,
        "kept_hit_count": row.kept_hit_count,
        "yield_ok_count": row.yield_ok_count,
        "yield_rate": if row.search_count > 0 {
            (row.yield_ok_count as f64 / row.search_count as f64 * 1000.0).round() / 1000.0
        } else { 0.0 },
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

/// tier 由 preferred 降级时，后台对该源做一次轻量精选重算（不阻塞当前请求）。
pub(crate) fn trigger_curated_revalidate(pool: &SqlitePool, urls: Vec<String>) {
    if urls.is_empty() {
        return;
    }
    let pool = pool.clone();
    tokio::spawn(async move {
        for url in urls {
            if let Err(e) = crate::source_bench::revalidate_curated_source(&pool, &url).await {
                tracing::warn!(error = %e, source_url = %url, "light curated recompute failed");
            }
        }
    });
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
    let mut demoted: Vec<String> = Vec::new();
    for o in outcomes {
        let mut row = apply_outcome(existing.get(&o.source_url), o, now, cooldown_secs);
        let prev_tier = row.tier.clone();
        let network = Network::from_status(o.status, true);
        let eval = source_quality::evaluate(&score_input(&row, o.rule_complete, network), cfg);
        apply_evaluation(&mut row, &eval, network, o.rule_complete, now);
        if prev_tier == source_quality::TIER_PREFERRED
            && eval.tier != source_quality::TIER_PREFERRED
        {
            demoted.push(o.source_url.clone());
        }
        sqlx::query(
            "INSERT INTO source_health \
             (source_url, last_checked_at, last_status, last_error, consecutive_failures, \
              last_ok_at, cooldown_until, avg_latency_ms, check_total, tier, score, \
              evidence_json, last_eval_at, search_count, hit_count, parse_ok_count, \
              kept_hit_count, yield_ok_count, \
              fail_site_count, fail_engine_count, ttfb_ms_p90, ttfb_samples_json, \
              promote_streak, demote_streak) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
              ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24) \
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
              kept_hit_count=excluded.kept_hit_count, \
              yield_ok_count=excluded.yield_ok_count, \
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
        .bind(row.kept_hit_count)
        .bind(row.yield_ok_count)
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
    trigger_curated_revalidate(pool, demoted);
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
         tier, score, evidence_json, last_eval_at, search_count, hit_count, \
         kept_hit_count, yield_ok_count, parse_ok_count, \
         fail_site_count, fail_engine_count, ttfb_ms_p90, ttfb_samples_json, \
         promote_streak, demote_streak \
         FROM source_health WHERE source_url = ?",
    )
    .bind(source_url)
    .fetch_optional(pool)
    .await?;
    let mut row = row.unwrap_or_else(|| HealthRow {
        source_url: source_url.to_string(),
        ..Default::default()
    });
    let mut input = score_input(&row, rule_complete, network);
    if let Some(t) = prev_tier_override {
        input.prev_tier = t.to_string();
    }
    let prev_tier = row.tier.clone();
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
    if prev_tier == source_quality::TIER_PREFERRED && eval.tier != source_quality::TIER_PREFERRED {
        trigger_curated_revalidate(pool, vec![source_url.to_string()]);
    }
    Ok(eval)
}
