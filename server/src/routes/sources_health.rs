//! sources 路由的健康检查、清理与隔离（move-only 从 sources.rs 拆出）。

use super::*;

#[derive(Deserialize, Default)]
pub struct HealthCheckQ {
    /// 逗号分隔的 bookSourceUrl；为空则检查全部“启用且可搜索”的源。
    pub(super) source_ids: Option<String>,
    /// `1` 时只测“未测过 / 冷却到期 / 距上次超过冷却时长”的源。
    pub(super) only_due: Option<String>,
    pub(super) key: Option<String>,
}

/// `POST /api/sources/health/check`：只跑一次“搜索”探测，并发 ≤8，整批 ≤90s。
pub async fn health_check(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<HealthCheckQ>,
) -> AppResult<Json<Value>> {
    // 会批量外连并写全局健康状态，仅管理员可触发。
    crate::auth::ensure_admin(&u)?;
    let sources = load_sources(&s.shared).await?;
    let quarantined = source_ledger::quarantined_set(&s.shared)
        .await
        .unwrap_or_default();
    let ids = split_ids(q.source_ids.as_deref());
    let mut selected: Vec<BookSource> = sources
        .into_iter()
        .filter(|x| {
            is_probeable(x)
                && if ids.is_empty() {
                    // 默认巡检不消耗隔离区；显式 source_ids 可用于复查复活。
                    x.enabled && !quarantined.contains(&x.book_source_url)
                } else {
                    ids.iter().any(|id| id == &x.book_source_url)
                }
        })
        .collect();
    if truthy(q.only_due.as_deref()) {
        let health = source_health::load_map(&s.shared).await.unwrap_or_default();
        let recheck_ms = (s.cfg.source_cooldown_secs as i64).saturating_mul(1000);
        let now = source_health::now_ms();
        selected.retain(|x| {
            health
                .get(&x.book_source_url)
                .map(|h| h.due(now, recheck_ms))
                .unwrap_or(true)
            // 同时满足“无记录 → 该测”
        });
        // retain 的 unwrap_or 已覆盖“无记录”，这里保持简单明确
    }
    selected.sort_by(|a, b| a.book_source_name.cmp(&b.book_source_name));

    let key = q
        .key
        .as_deref()
        .map(|k| k.trim())
        .filter(|k| !k.is_empty())
        .unwrap_or(source_health::DEFAULT_PROBE_KEY);
    let started = Instant::now();
    let probes = source_health::probe_many(selected.clone(), key).await;
    if let Err(e) = source_health::record_outcomes(
        &s.shared,
        &source_health::outcomes_from_probes(&probes),
        s.cfg.source_cooldown_secs,
        &crate::source_quality::TierConfig::from_config(&s.cfg),
    )
    .await
    {
        tracing::warn!(error = %e, "record health check failed");
    }
    // 巡检拿到有效响应（ok/partial）后同步复活账本自动隔离源：
    // 默认巡检不选隔离区，显式 source_ids 复查时必须能真正回到搜索池。
    let revive_urls: Vec<String> = probes
        .iter()
        .filter(|p| p.probed && p.status != source_health::STATUS_FAIL)
        .map(|p| p.source_url.clone())
        .collect();
    match source_ledger::revive_auto(&s.shared, &revive_urls).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(revived = n, "health check revived auto-quarantined sources"),
        Err(e) => tracing::warn!(error = %e, "health check ledger revival failed"),
    }
    let health = source_health::load_map(&s.shared).await.unwrap_or_default();
    let results: Vec<Value> = probes
        .iter()
        .map(|p| {
            let h = health.get(&p.source_url);
            json!({
                "source_url": p.source_url.clone(),
                "name": p.name.clone(),
                "status": p.status,
                "engine_limited": p.status == source_health::STATUS_PARTIAL,
                "latency_ms": p.latency_ms,
                "error": p.error.clone(),
                "consecutive_failures": h.map(|x| x.consecutive_failures).unwrap_or(0),
                "cooldown_until": h.and_then(|x| x.cooldown_until),
            })
        })
        .collect();
    let checked = probes.iter().filter(|p| p.probed).count();
    Ok(Json(json!({
        "ok": true,
        "selected": selected.len(),
        "checked": checked,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "budget_secs": source_health::PROBE_TOTAL_BUDGET_SECS,
        "cooldown_secs": s.cfg.source_cooldown_secs,
        "fail_threshold": source_health::FAIL_THRESHOLD,
        "results": results,
    })))
}

#[derive(Deserialize, Default)]
pub struct HealthSummaryQ {
    /// `detail=1` 时返回逐源明细。
    pub(super) detail: Option<String>,
}

/// `GET /api/sources/health`：汇总（源数 / 去重站点数 / ok / partial / fail / 冷却 / 未测）。
pub async fn health_summary(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<HealthSummaryQ>,
) -> AppResult<Json<Value>> {
    let rows = sqlx::query(
        "SELECT s.id AS id, s.name AS name, s.enabled AS enabled, \
         h.last_status AS last_status, h.last_checked_at AS last_checked_at, \
         h.last_error AS last_error, h.consecutive_failures AS consecutive_failures, \
         h.last_ok_at AS last_ok_at, h.cooldown_until AS cooldown_until, \
         h.avg_latency_ms AS avg_latency_ms, \
         l.decision AS decision, l.review_state AS review_state \
         FROM book_sources s LEFT JOIN source_health h ON h.source_url = s.id \
         LEFT JOIN source_library_state l ON l.source_url = s.id \
         ORDER BY s.name",
    )
    .fetch_all(&s.shared)
    .await?;

    let now = source_health::now_ms();
    let detail_on = truthy(q.detail.as_deref());
    let rule_anomalies = source_health::rule_anomaly_map();
    let (mut total, mut ok, mut partial, mut fail, mut cooling, mut never) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut active, mut quarantined) = (0usize, 0usize);
    let mut hosts: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut details: Vec<Value> = Vec::new();
    for r in rows {
        total += 1;
        let id: String = r.get("id");
        if let Some(h) = source_health::host_of(&id) {
            hosts.insert(h);
        }
        let status: Option<String> = r.try_get("last_status").ok().flatten();
        let status = status.unwrap_or_else(|| source_health::STATUS_NEVER.to_string());
        match status.as_str() {
            source_health::STATUS_OK => ok += 1,
            source_health::STATUS_PARTIAL => partial += 1,
            source_health::STATUS_FAIL => fail += 1,
            _ => never += 1,
        }
        let cooldown_until: Option<i64> = r.try_get("cooldown_until").ok().flatten();
        let cooling_now = cooldown_until.map(|t| t > now).unwrap_or(false);
        if cooling_now {
            cooling += 1;
        }
        let decision: Option<String> = r.try_get("decision").ok().flatten();
        let decision = decision.unwrap_or_else(|| source_ledger::DECISION_ACTIVE.to_string());
        if decision == source_ledger::DECISION_QUARANTINED {
            quarantined += 1;
        } else {
            active += 1;
        }
        if detail_on {
            let anomaly = rule_anomalies.get(&id);
            details.push(json!({
                "book_source_url": id,
                "name": r.try_get::<String, _>("name").unwrap_or_default(),
                "enabled": r.try_get::<i64, _>("enabled").unwrap_or(0) != 0,
                "last_status": status,
                "last_checked_at": r.try_get::<Option<i64>, _>("last_checked_at").ok().flatten(),
                "last_error": r.try_get::<Option<String>, _>("last_error").ok().flatten(),
                "consecutive_failures": r.try_get::<i64, _>("consecutive_failures").unwrap_or(0),
                "last_ok_at": r.try_get::<Option<i64>, _>("last_ok_at").ok().flatten(),
                "cooldown_until": cooldown_until,
                "avg_latency_ms": r.try_get::<Option<i64>, _>("avg_latency_ms").ok().flatten(),
                "cooling": cooling_now,
                "decision": decision,
                "review_state": r.try_get::<Option<String>, _>("review_state").ok().flatten(),
                "rule_error_count": anomaly.map(|a| a.count).unwrap_or(0),
                "last_rule_error_at": anomaly.map(|a| a.last_at),
            }));
        }
    }
    Ok(Json(json!({
        "ok": true,
        "total": total,
        "distinct_hosts": hosts.len(),
        "active": active,
        "quarantined": quarantined,
        "ok": ok,
        "partial": partial,
        "fail": fail,
        "cooling": cooling,
        "never": never,
        "cooldown_secs": s.cfg.source_cooldown_secs,
        "fail_threshold": source_health::FAIL_THRESHOLD,
        "rule_anomaly_sources": rule_anomalies.len(),
        "now": now,
        "detail": detail_on,
        "sources": if detail_on { Value::Array(details) } else { Value::Null },
    })))
}

#[derive(Deserialize, Default)]
pub struct ResetReq {
    pub(super) source_ids: Option<Vec<String>>,
    pub(super) url: Option<String>,
}

/// `POST /api/sources/health/reset`：清空某源或全部健康记录（手动“重新信任”）。
pub async fn health_reset(
    State(s): State<AppState>,
    u: AuthUser,
    body: Option<Json<ResetReq>>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    let mut ids = body
        .as_ref()
        .and_then(|b| b.source_ids.clone())
        .unwrap_or_default();
    if let Some(url) = body.as_ref().and_then(|b| b.url.clone()) {
        ids.push(url);
    }
    ids.retain(|x| !x.trim().is_empty());
    let cleared = source_health::reset(&s.shared, &ids).await?;
    // 健康状态重置时一并清掉字段级规则异常台账（手动“重新信任”）。
    source_health::reset_rule_anomalies(&ids);
    Ok(Json(json!({
        "ok": true,
        "cleared": cleared,
        "scope": if ids.is_empty() { "all" } else { "selected" },
    })))
}

#[derive(Deserialize, Default)]
pub struct PruneQ {
    pub(super) within_days: Option<i64>,
    pub(super) action: Option<String>,
    pub(super) confirm: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct PruneReq {
    pub(super) within_days: Option<i64>,
    pub(super) action: Option<String>,
    pub(super) confirm: Option<bool>,
}

/// `POST /api/sources/prune`：按“站点性失败 + 最近 N 天”筛出源条目，先预览、确认后禁用/删除。
/// 引擎性失败（`partial`）永远不在候选内；只按 `bookSourceUrl` 操作，不按域名批量。
pub async fn prune(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<PruneQ>,
    body: Option<Json<PruneReq>>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    let days = body
        .as_ref()
        .and_then(|b| b.within_days)
        .or(q.within_days)
        .unwrap_or(7)
        .clamp(1, 365);
    let action = body
        .as_ref()
        .and_then(|b| b.action.clone())
        .or(q.action.clone())
        .unwrap_or_else(|| "quarantine".to_string())
        .to_ascii_lowercase();
    if !matches!(action.as_str(), "quarantine" | "disable") {
        return Err(AppError::BadRequest(
            "action 仅支持 quarantine（默认）或 disable；隔离保留原始定义，不提供物理删除".into(),
        ));
    }
    let confirm =
        truthy(q.confirm.as_deref()) || body.as_ref().and_then(|b| b.confirm).unwrap_or(false);

    let now = source_health::now_ms();
    let since = now.saturating_sub(days.saturating_mul(86_400_000));
    let rows = sqlx::query(
        "SELECT s.id AS id, s.name AS name, s.enabled AS enabled, \
         h.last_status AS last_status, h.last_checked_at AS last_checked_at, \
         h.consecutive_failures AS consecutive_failures, h.last_error AS last_error, \
         h.cooldown_until AS cooldown_until \
         FROM book_sources s JOIN source_health h ON h.source_url = s.id \
         LEFT JOIN source_library_state l ON l.source_url = s.id \
         WHERE h.last_status = 'fail' AND h.last_checked_at IS NOT NULL AND h.last_checked_at >= ? \
           AND (l.decision IS NULL OR l.decision <> 'quarantined') \
         ORDER BY s.name",
    )
    .bind(since)
    .fetch_all(&s.shared)
    .await?;

    let mut hosts: std::collections::HashSet<String> = std::collections::HashSet::new();
    let candidates: Vec<Value> = rows
        .iter()
        .map(|r| {
            let id: String = r.get("id");
            if let Some(h) = source_health::host_of(&id) {
                hosts.insert(h);
            }
            json!({
                "book_source_url": id,
                "name": r.try_get::<String, _>("name").unwrap_or_default(),
                "enabled": r.try_get::<i64, _>("enabled").unwrap_or(0) != 0,
                "probe_status": "fail_site",
                "last_status": r.try_get::<Option<String>, _>("last_status").ok().flatten(),
                "last_checked_at": r.try_get::<Option<i64>, _>("last_checked_at").ok().flatten(),
                "consecutive_failures": r.try_get::<i64, _>("consecutive_failures").unwrap_or(0),
                "last_error": r.try_get::<Option<String>, _>("last_error").ok().flatten(),
                "cooldown_until": r.try_get::<Option<i64>, _>("cooldown_until").ok().flatten(),
            })
        })
        .collect();

    if !confirm {
        return Ok(Json(json!({
            "ok": true,
            "confirm_required": true,
            "within_days": days,
            "action": action,
            "would_affect": candidates.len(),
            "affected_hosts": hosts.len(),
            "engine_limited_excluded": true,
            "sources": candidates,
        })));
    }

    let mut affected = 0usize;
    let mut failed = 0usize;
    for c in &candidates {
        let id = c
            .get("book_source_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if id.is_empty() {
            continue;
        }
        if let Err(e) =
            source_ledger::quarantine(&s.shared, id, Some("pruned_site_fail"), now).await
        {
            failed += 1;
            tracing::warn!(source = id, error = %e, "prune quarantine failed");
            continue;
        }
        if action == "disable" {
            match load_source(&s.shared, id).await {
                Ok(mut src) => {
                    src.enabled = false;
                    if let Err(e) = store_source(&s.shared, &src, &u.username).await {
                        failed += 1;
                        tracing::warn!(source = id, error = %e, "prune disable failed");
                        continue;
                    }
                }
                Err(e) => {
                    failed += 1;
                    tracing::warn!(source = id, error = %e, "prune disable load failed");
                    continue;
                }
            }
        }
        affected += 1;
    }
    Ok(Json(json!({
        "ok": true,
        "confirm_required": false,
        "within_days": days,
        "action": action,
        "affected": affected,
        "failed": failed,
        "affected_hosts": hosts.len(),
        "engine_limited_excluded": true,
        "physical_delete": false,
        "sources": candidates,
    })))
}

#[derive(Deserialize)]
pub struct QuarantineReq {
    pub(super) source_ids: Vec<String>,
    pub(super) reason: Option<String>,
}

/// `POST /api/sources/quarantine`：手动把指定源移入隔离区（保留定义，不物理删除）。
pub async fn quarantine_sources(
    State(s): State<AppState>,
    u: AuthUser,
    Json(req): Json<QuarantineReq>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    let mut affected = 0usize;
    let mut missing: Vec<String> = Vec::new();
    let now = source_health::now_ms();
    let reason = req
        .reason
        .as_deref()
        .unwrap_or("manual_quarantine")
        .to_string();
    for id in &req.source_ids {
        if load_source(&s.shared, id).await.is_err() {
            missing.push(id.clone());
            continue;
        }
        if let Err(e) = source_ledger::quarantine(&s.shared, id, Some(&reason), now).await {
            tracing::warn!(source = %id, error = %e, "manual quarantine failed");
            missing.push(id.clone());
            continue;
        }
        affected += 1;
    }
    Ok(Json(json!({
        "ok": true,
        "affected": affected,
        "missing": missing,
        "reason": reason,
    })))
}
