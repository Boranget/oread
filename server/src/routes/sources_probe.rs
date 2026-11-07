//! sources 路由的导入体检请求与报告（move-only 从 sources.rs 拆出）。

use super::*;

#[derive(Deserialize, Default)]
pub struct ProbeQ {
    /// `probe=0` 可关闭导入体检；默认对“新增 + 规则变化 + 隔离区复活检查”的源自动体检。
    pub(super) probe: Option<String>,
    /// 导入来源标识（合集 URL / 文件名）；用于同来源 diff 与 raw_sha 去重。
    pub(super) source: Option<String>,
    /// collection | file | api | inline
    pub(super) source_kind: Option<String>,
    /// `force=1`：人工确认后强制覆盖 `update_rejected` / `conflict`。
    pub(super) force: Option<String>,
    /// 体检关键词（默认“测试”）。
    pub(super) key: Option<String>,
}

impl ProbeQ {
    pub(super) fn truthy(&self, raw: Option<&String>) -> bool {
        matches!(
            raw.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
            Some("1" | "true" | "yes" | "on")
        )
    }

    pub(super) fn on(&self) -> bool {
        self.truthy(self.probe.as_ref())
    }

    pub(super) fn probe_disabled(&self) -> bool {
        matches!(
            self.probe
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase())
                .as_deref(),
            Some("0" | "false" | "no" | "off")
        )
    }

    pub(super) fn force(&self) -> bool {
        self.truthy(self.force.as_ref())
    }

    pub(super) fn source_label(&self, default: &str) -> String {
        self.source
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(default)
            .to_string()
    }

    pub(super) fn kind_label(&self, default: &str) -> String {
        self.source_kind
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(default)
            .to_string()
    }
}

/// 只有“支持的类型 + 有搜索规则与搜索地址 + 不要求登录”的源才能做搜索体检。
pub(super) fn is_probeable(src: &BookSource) -> bool {
    crate::entities::is_supported_source_type(src.book_source_type)
        && src.rule_search.is_some()
        && src.search_url.is_some()
        && !crate::entities::requires_login(src)
}

/// 导入/单条保存后的体检：逐条结果 + 汇总。失败源**不会自动禁用**，
/// 只通过 `suggested_enabled` 给出建议；引擎性失败（partial）建议保持启用。
pub(super) async fn probe_report(
    pool: &SqlitePool,
    cfg: &crate::config::Config,
    list: &[BookSource],
    key: &str,
) -> (Vec<Value>, Value) {
    let started = Instant::now();
    let ineligible = list.iter().filter(|s| !is_probeable(s)).count();
    let targets: Vec<BookSource> = list.iter().filter(|s| is_probeable(s)).cloned().collect();
    let probes = source_health::probe_many(targets, key).await;
    if let Err(e) = source_health::record_outcomes(
        pool,
        &source_health::outcomes_from_probes(&probes),
        cfg.source_cooldown_secs,
        &crate::source_quality::TierConfig::from_config(cfg),
    )
    .await
    {
        tracing::warn!(error = %e, "record import probe health failed");
    }
    let health = source_health::load_map(pool).await.unwrap_or_default();

    let mut probed = 0usize;
    let mut ok = 0usize;
    let mut fail_site = 0usize;
    let mut engine_limited = 0usize;
    let mut kept_enabled = 0usize;
    let mut disable_suggested = 0usize;
    let mut results = Vec::with_capacity(list.len());
    for src in list {
        let p = probes.iter().find(|p| p.source_url == src.book_source_url);
        let (probe_status, engine, suggested) = match p {
            Some(p) if p.probed => {
                if p.status == source_health::STATUS_OK {
                    ("ok", false, true)
                } else if p.status == source_health::STATUS_PARTIAL {
                    // 站点能连上、只是规则跑不通：绝不能因为引擎缺陷把源标成废源
                    ("fail", true, true)
                } else {
                    ("fail", false, false)
                }
            }
            _ => ("never", false, src.enabled),
        };
        if p.map(|p| p.probed).unwrap_or(false) {
            probed += 1;
            if probe_status == "ok" {
                ok += 1;
            } else if engine {
                engine_limited += 1;
            } else {
                fail_site += 1;
            }
            if suggested {
                kept_enabled += 1;
            } else {
                disable_suggested += 1;
            }
        }
        let h = health.get(&src.book_source_url);
        results.push(json!({
            "book_source_url": src.book_source_url.clone(),
            "name": src.book_source_name.clone(),
            "probe_status": probe_status,
            "engine_limited": engine,
            "latency_ms": p.map(|p| p.latency_ms).unwrap_or(0),
            "error": p.and_then(|p| p.error.clone()),
            "suggested_enabled": suggested,
            "consecutive_failures": h.map(|x| x.consecutive_failures).unwrap_or(0),
            "cooldown_until": h.and_then(|x| x.cooldown_until),
        }));
    }
    let total = list.len();
    let unprobed = total - probed;
    let timed_out = unprobed.saturating_sub(ineligible);
    let summary = json!({
        "probe_total": total,
        "probed": probed,
        "unprobed": unprobed,
        "ineligible": ineligible,
        "probe_timed_out": timed_out > 0,
        "ok": ok,
        "fail_site": fail_site,
        "engine_limited": engine_limited,
        "kept_enabled": kept_enabled,
        "disable_suggested": disable_suggested,
        "probe_elapsed_ms": started.elapsed().as_millis() as u64,
        "probe_budget_secs": source_health::PROBE_TOTAL_BUDGET_SECS,
    });
    (results, summary)
}

pub(super) fn merge_probe(out: &mut Value, results: Vec<Value>, summary: Value) {
    if let Some(obj) = out.as_object_mut() {
        obj.insert("probe_results".into(), Value::Array(results));
        if let Some(obj2) = summary.as_object() {
            for (k, v) in obj2 {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
}
