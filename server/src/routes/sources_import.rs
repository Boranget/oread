//! sources 路由的单条保存与批量导入（move-only 从 sources.rs 拆出）。

use super::*;

pub async fn upsert(
    State(s): State<AppState>,
    u: AuthUser,
    Query(pq): Query<ProbeQ>,
    Json(body): Json<Value>,
) -> AppResult<Json<serde_json::Value>> {
    crate::auth::ensure_admin(&u)?;
    let mut src = normalize_source(body)?;
    prepare_import(&mut src);
    store_source(&s.shared, &src, &u.username).await?;
    let login_required = crate::entities::requires_login(&src);
    let mut out = json!({
        "ok": true,
        "type": src.book_source_type,
        "type_name": crate::entities::source_type_name(src.book_source_type),
        "unsupported_type": src.unsupported_type,
        "enabled": src.enabled,
        "login_required": login_required,
        "searchable": crate::entities::is_searchable_source(&src),
    });
    if pq.on() && !login_required {
        let (results, summary) = probe_report(
            &s.shared,
            &s.cfg,
            std::slice::from_ref(&src),
            source_health::DEFAULT_PROBE_KEY,
        )
        .await;
        merge_probe(&mut out, results, summary);
    }
    Ok(Json(out))
}

/// 批量导入：同一批内按 book_source_url 去重（后者覆盖前者），并区分新增/更新数量。
/// 旧版本是否仍“可用”：未被隔离、用户未禁用、最近健康记录不是站点失败。
pub(super) fn local_still_usable(
    existing: Option<&BookSource>,
    state: Option<&LibraryState>,
    health: Option<&source_health::HealthRow>,
) -> bool {
    let Some(e) = existing else {
        return false;
    };
    e.enabled
        && state.map(|st| !st.is_quarantined()).unwrap_or(true)
        && health
            .map(|h| h.last_status != source_health::STATUS_FAIL)
            .unwrap_or(true)
}

/// 批量导入：三层模型（本地源库 + 导入账本 + 隔离区）+ 覆盖三态。
///
/// 1. 先算整份合集的规范化哈希：与同来源上一批相同 → 不发起任何探测（隔离区复活检查除外）。
/// 2. 只探测“新增 + 规则哈希变化 + 隔离区待复活”的条目，未变化的沿用历史健康记录。
/// 3. 覆盖前做质量判断：
///    - 新版站点体检通过 / 引擎性失败 → 覆盖（引擎性失败只标记 engine_limited，绝不隔离）
///    - 新版站点体检失败且本地旧版仍可用 → `update_rejected`，保留旧版
///    - 本地规则与账本快照哈希不一致（人工改过） → `conflict`，不自动覆盖
/// 4. 隔离区只移出活跃库，原始定义仍保留在 `book_sources` 与账本中，后续可复活。
pub async fn import(
    State(s): State<AppState>,
    u: AuthUser,
    Query(pq): Query<ProbeQ>,
    Json(body): Json<Value>,
) -> AppResult<Json<serde_json::Value>> {
    crate::auth::ensure_admin(&u)?;
    let raw = body;
    let items: Vec<Value> = match raw.clone() {
        Value::Array(arr) => arr,
        single => vec![single],
    };
    let mut list: Vec<BookSource> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in items {
        let src = normalize_source(item)?;
        if !seen.insert(src.book_source_url.clone()) {
            if let Some(old) = list
                .iter_mut()
                .find(|x| x.book_source_url == src.book_source_url)
            {
                *old = src;
            }
            continue;
        }
        list.push(src);
    }
    let mut disabled_unsupported = 0usize;
    for src in &mut list {
        if !prepare_import(src) {
            disabled_unsupported += 1;
        }
    }

    let now = source_health::now_ms();
    let raw_sha = source_ledger::sha256_hex(&source_ledger::canonical_json(&raw));
    let source = pq.source_label("inline");
    let source_kind = pq.kind_label("inline");
    let force = pq.force();
    let probe_disabled = pq.probe_disabled();
    let batch_id = format!("{}-{}", now, &raw_sha[..raw_sha.len().min(8)]);

    let same_collection = source_ledger::last_batch_sha(&s.shared, &source)
        .await?
        .map(|x| x == raw_sha)
        .unwrap_or(false);
    let previous_urls = source_ledger::last_batch_urls(&s.shared, &source)
        .await
        .unwrap_or_default();

    let existing_list = load_sources(&s.shared).await?;
    let existing_map: std::collections::HashMap<String, BookSource> = existing_list
        .into_iter()
        .map(|x| (x.book_source_url.clone(), x))
        .collect();
    let states = source_ledger::load_states(&s.shared).await?;
    let health_before = source_health::load_map(&s.shared).await?;

    // ---- 只探测必要条目：新增 / 规则变化 / 隔离区待复活 ----
    let mut targets: Vec<BookSource> = Vec::new();
    if !probe_disabled {
        for src in &list {
            // 需登录源：远端搜索必然不可用，不做无意义的探测，也不进搜索池。
            if crate::entities::requires_login(src) {
                continue;
            }
            let url = &src.book_source_url;
            let existing = existing_map.get(url);
            let quarantined = states
                .get(url)
                .map(|st| st.is_quarantined())
                .unwrap_or(false);
            let changed = existing
                .map(|e| source_ledger::rule_hash(e) != source_ledger::rule_hash(src))
                .unwrap_or(true);
            let need = quarantined || (changed && (!same_collection || existing.is_none()));
            if need {
                targets.push(src.clone());
            }
        }
    }
    let probe_key = pq
        .key
        .as_deref()
        .map(|k| k.trim())
        .filter(|k| !k.is_empty())
        .unwrap_or(source_health::DEFAULT_PROBE_KEY);
    let probe_started = Instant::now();
    let probes = if targets.is_empty() {
        Vec::new()
    } else {
        source_health::probe_many(targets, probe_key).await
    };
    if let Err(e) = source_health::record_outcomes(
        &s.shared,
        &source_health::outcomes_from_probes(&probes),
        s.cfg.source_cooldown_secs,
        &crate::source_quality::TierConfig::from_config(&s.cfg),
    )
    .await
    {
        tracing::warn!(error = %e, "record import probe health failed");
    }
    let health_after = source_health::load_map(&s.shared).await.unwrap_or_default();
    // 导入期的“未知失败”（超时/连接失败）不作为隔离依据：保留健康事件与失败计数，
    // 但未达既有硬阈值前把 tier 拉回 observing，避免新用户导入后立刻搜不到。
    let transient_probe_urls: std::collections::HashSet<String> = probes
        .iter()
        .filter(|p| p.probed && p.status == source_health::STATUS_FAIL)
        .filter(|p| !source_health::is_explicit_site_rejection(p.error.as_deref()))
        .filter(|p| {
            health_after
                .get(&p.source_url)
                .map(|h| {
                    h.consecutive_failures < source_health::FAIL_THRESHOLD
                        && h.fail_site_count < source_health::FAIL_THRESHOLD
                })
                .unwrap_or(true)
        })
        .map(|p| p.source_url.clone())
        .collect();
    let mut health_after = health_after;
    if !transient_probe_urls.is_empty() {
        let urls: Vec<String> = transient_probe_urls.iter().cloned().collect();
        if let Err(e) = source_health::keep_observing_after_transient_probe(&s.shared, &urls).await
        {
            tracing::warn!(error = %e, "keep observing after transient import probe failed");
        }
        health_after = source_health::load_map(&s.shared).await.unwrap_or_default();
    }
    let probe_map: std::collections::HashMap<String, source_health::Probe> = probes
        .into_iter()
        .map(|p| (p.source_url.clone(), p))
        .collect();

    let mut ledger_items: Vec<source_ledger::ItemRecord> = Vec::with_capacity(list.len());
    let mut results: Vec<Value> = Vec::with_capacity(list.len());
    let (mut added, mut updated, mut unchanged, mut update_rejected, mut conflict, mut resurrected) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut skipped_login_required = 0usize;
    let mut newly_quarantined = 0usize;
    let mut probed_count = 0usize;
    let mut probe_ok = 0usize;
    let mut probe_fail_site = 0usize;
    let mut probe_engine_limited = 0usize;
    let mut kept_enabled = 0usize;
    let mut disable_suggested = 0usize;
    let mut type_suggestions = 0usize;
    let (mut tier_preferred, mut tier_observing, mut tier_quarantined) = (0usize, 0usize, 0usize);

    for src in &list {
        let url = src.book_source_url.clone();
        let existing = existing_map.get(&url);
        let state = states.get(&url);
        let incoming_hash = source_ledger::rule_hash(src);
        let existing_hash = existing.map(source_ledger::rule_hash);
        let recorded = state.and_then(|st| st.content_sha.as_deref());
        let local_modified = match (existing, recorded) {
            (Some(_), Some(rec)) => existing_hash.as_deref().map(|h| h != rec).unwrap_or(false),
            _ => false,
        };
        let changed = existing_hash.as_deref() != Some(incoming_hash.as_str());
        let was_quarantined = state.map(|st| st.is_quarantined()).unwrap_or(false);

        let probe = probe_map.get(&url);
        let probed = probe.map(|p| p.probed).unwrap_or(false);
        let login_required = crate::entities::requires_login(src);
        let probe_status = match probe {
            Some(p) if p.probed => p.status,
            _ => source_health::STATUS_NEVER,
        };
        let historical = health_after.get(&url).map(|h| h.last_status.as_str());
        let import_probe_unknown = transient_probe_urls.contains(&url);
        let engine_limited = probe_status == source_health::STATUS_PARTIAL
            || (!probed && historical == Some(source_health::STATUS_PARTIAL));

        // ---- 同一条流水线：规则完整度 + 本次体检连通性 + 既有成功侧统计 → tier/score ----
        let tier_cfg = crate::source_quality::TierConfig::from_config(&s.cfg);
        let rule_complete = crate::source_quality::rule_completeness(src);
        let incoming_network = if probed && !import_probe_unknown {
            source_quality::Network::from_status(probe_status, true)
        } else {
            source_quality::Network::Skipped
        };
        let revive = was_quarantined && probed && probe_status != source_health::STATUS_FAIL;
        // 未知失败按 observing 评估：不因导入期的超时/连接失败降级或隔离。
        let tier_override = if revive || import_probe_unknown {
            Some(source_quality::TIER_OBSERVING)
        } else {
            None
        };
        let incoming_eval = source_health::evaluate_source(
            health_after.get(&url),
            rule_complete,
            incoming_network,
            tier_override,
            &tier_cfg,
        );
        let local_score = health_before.get(&url).map(|h| h.score).unwrap_or(50);

        let mut accepted = true;
        let mut decision = state
            .map(|st| st.decision.clone())
            .unwrap_or_else(|| source_ledger::DECISION_ACTIVE.to_string());
        let mut category = "unchanged";
        let mut reason: Option<String> = None;

        if login_required {
            // 导入管线内的登录墙判定：不探测、不进入可搜索池；仍保留/更新定义，不改写 enabled。
            if local_modified && changed && !force {
                accepted = false;
                category = "conflict";
                reason = Some("本地规则与账本快照不一致，已保留本地版本".into());
            } else {
                category = "skipped_login_required";
                reason = Some(
                    "login_required: 规则声明需登录（loginUrl/loginUi/loginCheckJs），已跳过搜索/体检"
                        .into(),
                );
            }
        } else if local_modified && changed && !force {
            // 人工改过的本地规则优先：不自动覆盖，等待确认
            accepted = false;
            category = "conflict";
            reason = Some("本地规则与账本快照不一致，已保留本地版本".into());
        } else if existing.is_none() {
            category = "added";
            decision = if probed
                && probe_status == source_health::STATUS_FAIL
                && !import_probe_unknown
                && !engine_limited
            {
                source_ledger::DECISION_QUARANTINED.to_string()
            } else {
                source_ledger::DECISION_ACTIVE.to_string()
            };
        } else if changed {
            if force {
                decision = if probe_status == source_health::STATUS_FAIL
                    && !import_probe_unknown
                    && !engine_limited
                {
                    source_ledger::DECISION_QUARANTINED.to_string()
                } else {
                    source_ledger::DECISION_ACTIVE.to_string()
                };
                category = if was_quarantined && decision == source_ledger::DECISION_ACTIVE {
                    "resurrected"
                } else {
                    "updated"
                };
            } else if !probed {
                accepted = false;
                category = "update_rejected";
                reason = Some("新版体检未完成，暂不覆盖本地版本".into());
            } else if probe_status == source_health::STATUS_FAIL
                && !import_probe_unknown
                && !engine_limited
                && local_still_usable(existing, state, health_before.get(&url))
            {
                accepted = false;
                category = "update_rejected";
                reason = Some("新版站点体检失败，本地旧版仍可用，已保留旧版".into());
            } else if local_still_usable(existing, state, health_before.get(&url))
                && i64::from(incoming_eval.score + s.cfg.source_tier_update_margin) < local_score
            {
                // 质量闸门：新版本即使能跑通，评分明显低于本地旧版也不覆盖。
                accepted = false;
                category = "update_rejected";
                reason = Some(format!(
                    "新版评分 {} 低于本地 {}（差值 > 允许 {}），已保留旧版",
                    incoming_eval.score, local_score, s.cfg.source_tier_update_margin
                ));
            } else if local_still_usable(existing, state, health_before.get(&url))
                && src.last_update_time <= existing.map(|e| e.last_update_time).unwrap_or(0)
            {
                // 上游规则变更但版本没有更晚（含时间相同）：保留本地，避免每次导入都抖动换版本。
                accepted = false;
                category = "unchanged";
                reason = Some("本地版本不旧于上游，保留本地版本".into());
            } else {
                decision = if probe_status == source_health::STATUS_FAIL
                    && !import_probe_unknown
                    && !engine_limited
                {
                    source_ledger::DECISION_QUARANTINED.to_string()
                } else {
                    source_ledger::DECISION_ACTIVE.to_string()
                };
                category = if was_quarantined && decision == source_ledger::DECISION_ACTIVE {
                    "resurrected"
                } else {
                    "updated"
                };
            }
        } else if was_quarantined && probed && probe_status != source_health::STATUS_FAIL {
            // 规则未变、但隔离区这次体检通过 → 复活
            decision = source_ledger::DECISION_ACTIVE.to_string();
            category = "resurrected";
            reason = Some("隔离区复检通过，已回到活跃库".into());
        }

        // 三态落定后，评分与隔离决策必须一致：隔离由任一一方判定即可；
        // 被拒绝覆盖的条目按“本地旧版”评估，避免用新版坏数据误杀旧版。
        let mut final_eval = incoming_eval.clone();
        let mut final_network = incoming_network;
        let mut final_rule = rule_complete;
        if !accepted {
            let local_src = existing.unwrap_or(src);
            final_rule = crate::source_quality::rule_completeness(local_src);
            final_network = health_before
                .get(&url)
                .filter(|h| h.last_status != source_health::STATUS_NEVER)
                .map(|h| source_quality::Network::from_status(&h.last_status, true))
                .unwrap_or(source_quality::Network::Skipped);
            final_eval = source_health::evaluate_source(
                health_before.get(&url),
                final_rule,
                final_network,
                None,
                &tier_cfg,
            );
        }
        if final_eval.tier == source_quality::TIER_QUARANTINED
            && decision == source_ledger::DECISION_ACTIVE
            && !login_required
            && !import_probe_unknown
        {
            decision = source_ledger::DECISION_QUARANTINED.to_string();
            if reason.is_none() {
                reason = Some(format!("quality_quarantine: score={}", final_eval.score));
            }
        }
        if decision == source_ledger::DECISION_QUARANTINED {
            final_eval.tier = source_quality::TIER_QUARANTINED.to_string();
        }

        if decision == source_ledger::DECISION_QUARANTINED
            && !was_quarantined
            && (category == "added" || category == "updated")
        {
            newly_quarantined += 1;
        }
        if decision == source_ledger::DECISION_QUARANTINED && reason.is_none() {
            reason = Some(match probe.and_then(|p| p.error.clone()) {
                Some(e) => format!("site_unreachable: {e}"),
                None => "site_unreachable".into(),
            });
        } else if engine_limited && reason.is_none() {
            reason = Some(match probe.and_then(|p| p.error.clone()) {
                Some(e) => format!("engine_limited: {e}"),
                None => "engine_limited".into(),
            });
        }

        let mut stored = src.clone();
        if login_required {
            // 登录墙只影响“能否搜索”，不得顺带改写用户的启停列。
            if let Some(e) = existing {
                stored.enabled = e.enabled;
            }
        }
        if decision == source_ledger::DECISION_QUARANTINED {
            // 隔离源不仅要有 ledger decision，还必须 enabled=0，避免任何旧路径把它带进搜索。
            stored.enabled = false;
        }
        if accepted {
            store_source(&s.shared, &stored, &u.username).await?;
            source_ledger::upsert_state(
                &s.shared,
                &url,
                &decision,
                reason.as_deref(),
                Some(&incoming_hash),
                &batch_id,
                now,
            )
            .await?;
            source_ledger::clear_review(&s.shared, &url).await?;
        } else if category == "unchanged" {
            // 上游版本没有更晚：保留本地版本，不写 review、不覆盖。
        } else {
            let review = if category == "conflict" {
                source_ledger::REVIEW_CONFLICT
            } else {
                source_ledger::REVIEW_UPDATE_REJECTED
            };
            source_ledger::set_review(&s.shared, &url, review, reason.as_deref(), now).await?;
        }

        if !accepted && decision == source_ledger::DECISION_QUARANTINED {
            // 覆盖被拒但本地评分已跌破隔离线：保留旧规则，同时把旧行停用。
            if let Some(mut local) = existing.cloned() {
                local.enabled = false;
                store_source(&s.shared, &local, &u.username).await?;
            }
        }

        // 每次导入都落一次质量评估（含未体检的源：network=skipped 只做静态评估）。
        match source_health::record_evaluation(
            &s.shared,
            &url,
            final_rule,
            final_network,
            if accepted && revive {
                Some(source_quality::TIER_OBSERVING)
            } else {
                None
            },
            if decision == source_ledger::DECISION_QUARANTINED {
                Some(source_quality::TIER_QUARANTINED)
            } else {
                None
            },
            &tier_cfg,
        )
        .await
        {
            Ok(e) => final_eval = e,
            Err(e) => tracing::warn!(error = %e, source = %url, "record source quality failed"),
        }

        match final_eval.tier.as_str() {
            source_quality::TIER_PREFERRED => tier_preferred += 1,
            source_quality::TIER_QUARANTINED => tier_quarantined += 1,
            _ => tier_observing += 1,
        }

        match category {
            "added" => added += 1,
            "updated" => updated += 1,
            "update_rejected" => update_rejected += 1,
            "conflict" => conflict += 1,
            "resurrected" => resurrected += 1,
            "skipped_login_required" => skipped_login_required += 1,
            _ => unchanged += 1,
        }
        if probed {
            probed_count += 1;
            match probe_status {
                source_health::STATUS_OK => probe_ok += 1,
                source_health::STATUS_PARTIAL => probe_engine_limited += 1,
                _ => probe_fail_site += 1,
            }
            if decision == source_ledger::DECISION_ACTIVE {
                kept_enabled += 1;
            } else {
                disable_suggested += 1;
            }
        }

        let type_suggestion = suggest_comic_type(src);
        if type_suggestion.is_some() {
            type_suggestions += 1;
        }
        let ledger_probe = match probe_status {
            source_health::STATUS_OK => "ok",
            source_health::STATUS_PARTIAL => "fail",
            source_health::STATUS_FAIL => "fail",
            _ => "never",
        };
        let h = health_after.get(&url);
        ledger_items.push(source_ledger::ItemRecord {
            source_url: url.clone(),
            definition: source_ledger::snapshot_json(src),
            content_sha: incoming_hash.clone(),
            probe_status: ledger_probe.into(),
            decision: decision.clone(),
            reason: Some(match &reason {
                Some(r) => format!(
                    "{category}: {r} | tier={} score={}",
                    final_eval.tier, final_eval.score
                ),
                None => format!(
                    "{category} | tier={} score={}",
                    final_eval.tier, final_eval.score
                ),
            }),
        });
        results.push(json!({
            "book_source_url": url,
            "name": src.book_source_name.clone(),
            // 仅建议、不改库：命中漫画特征但当前类型不是 2 时给出 suggested_type=2
            "type_suggestion": type_suggestion,
            "probe_status": if probed {
                if probe_status == source_health::STATUS_OK { "ok" } else { "fail" }
            } else { "never" },
            "engine_limited": engine_limited,
            "latency_ms": probe.map(|p| p.latency_ms).unwrap_or(0),
            "error": probe.and_then(|p| p.error.clone()),
            "suggested_enabled": decision == source_ledger::DECISION_ACTIVE,
            "decision": decision,
            "category": category,
            "reason": reason,
            "login_required": login_required,
            "searchable": crate::entities::is_searchable_source(&stored),
            "tier": final_eval.tier,
            "score": final_eval.score,
            "reasons": final_eval.reasons,
            "consecutive_failures": h.map(|x| x.consecutive_failures).unwrap_or(0),
            "cooldown_until": h.and_then(|x| x.cooldown_until),
        }));
    }

    let current_urls: std::collections::HashSet<&str> =
        list.iter().map(|x| x.book_source_url.as_str()).collect();
    let local_only: Vec<String> = previous_urls
        .iter()
        .filter(|u| !current_urls.contains(u.as_str()))
        .cloned()
        .collect();

    let probe_summary = json!({
        "probed": probed_count,
        "unprobed": list.len().saturating_sub(probed_count),
        "ok": probe_ok,
        "fail_site": probe_fail_site,
        "engine_limited": probe_engine_limited,
        "kept_enabled": kept_enabled,
        "disable_suggested": disable_suggested,
        "probe_elapsed_ms": probe_started.elapsed().as_millis() as u64,
        "probe_budget_secs": source_health::PROBE_TOTAL_BUDGET_SECS,
        "probe_skipped_same_collection": same_collection,
        "tier_preferred": tier_preferred,
        "tier_observing": tier_observing,
        "tier_quarantined": tier_quarantined,
        "skipped_login_required": skipped_login_required,
    });
    let probe_summary_str = serde_json::to_string(&probe_summary).ok();
    source_ledger::record_batch(
        &s.shared,
        &source_ledger::BatchRecord {
            id: batch_id.clone(),
            source: source.clone(),
            source_kind: source_kind.clone(),
            imported_at: now,
            item_count: list.len(),
            raw_sha: Some(raw_sha.clone()),
            probe_summary: probe_summary_str,
        },
        &ledger_items,
    )
    .await?;

    Ok(Json(json!({
        "ok": true,
        "batch_id": batch_id,
        "source": source,
        "source_kind": source_kind,
        "collection_unchanged": same_collection,
        "count": list.len(),
        "imported": list.len(),
        "inserted": added,
        "added": added,
        "updated": updated,
        "unchanged": unchanged,
        "update_rejected": update_rejected,
        "conflict": conflict,
        "resurrected": resurrected,
        "skipped_login_required": skipped_login_required,
        "local_only": local_only.len(),
        "local_only_urls": local_only,
        "newly_quarantined": newly_quarantined,
        "force": force,
        "disabled_unsupported": disabled_unsupported,
        "type_suggestions": type_suggestions,
        "distinct_hosts": source_health::distinct_host_count(&list),
        "probe_summary": probe_summary,
        "probe_results": results,
    })))
}
