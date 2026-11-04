use axum::{
    extract::{Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use std::time::Instant;

use crate::auth::AuthUser;
use crate::entities::BookSource;
use crate::error::{AppError, AppResult};
use crate::models::BookSourceListItem;
use crate::routes::AppState;
use crate::source_health;
use crate::source_ledger::{self, LibraryState};
use crate::source_quality;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/sources", get(list).post(upsert))
        .route("/api/sources/import", post(import))
        .route("/api/sources/enabled", post(set_enabled))
        .route("/api/sources/export", get(export_all))
        .route("/api/sources/check-updates", post(check_updates))
        .route("/api/sources/detail", get(get_one))
        .route("/api/sources/validate", post(validate))
        .route("/api/sources/delete", delete(delete_one))
        .route("/api/sources/health", get(health_summary))
        .route("/api/sources/health/check", post(health_check))
        .route("/api/sources/health/reset", post(health_reset))
        .route("/api/sources/prune", post(prune))
        .route("/api/sources/quarantine", post(quarantine_sources))
        // 体检语料：GET/POST/DELETE 同一路径（DELETE 用 ?id=），内置条目由后端保护
        .route(
            "/api/sources/bench/corpus",
            get(bench_corpus)
                .post(bench_corpus_add)
                .delete(bench_corpus_delete),
        )
        .route("/api/sources/bench/corpus/update", post(bench_corpus_update))
        .route("/api/sources/bench/corpus/toggle", post(bench_corpus_toggle))
        .route("/api/sources/bench/run", post(bench_run))
        .route("/api/sources/bench/curated", get(bench_curated))
}

#[derive(Deserialize)]
pub struct UrlQ {
    url: String,
}

/// 从 `rules` / `content` 两列中取可用 JSON。
/// 历史上 upsert 只写 `rules`，而 `get_one` 读 `content`，导致详情永远为空；
/// 这里两列都尝试（优先 `content`），并配合 shared 迁移 0003 回填，兼容旧数据。
fn parse_source_row(rules: Option<String>, content: Option<String>) -> Option<BookSource> {
    for raw in [content, rules].into_iter().flatten() {
        if raw.trim().is_empty() {
            continue;
        }
        if let Ok(s) = serde_json::from_str::<BookSource>(&raw) {
            return Some(s);
        }
    }
    None
}

/// 加载所有书源（含完整规则对象）。Legado 导出用 camelCase，导入时已统一为 snake_case。
/// 类型列（`book_source_type`/`unsupported_type`）与启停列（`enabled`）都以数据库为准覆盖 JSON 里的值，
/// 这样历史存量在迁移回填后无需重写 JSON 就能被正确归类。
///
/// `enabled` 必须取列：迁移（0005 停用 unsupported）和维护脚本只 `UPDATE ... SET enabled=0`，
/// 不会重写 JSON；而 JSON 里 `enabled` 缺失时反序列化默认 `true`（见 entities 的 `default_true`）。
/// 若信任 JSON，列上已停用的源会继续进入聚合搜索（`source_total` 偏大、结果 `origin` 出现停用源）。
pub async fn load_sources(pool: &SqlitePool) -> AppResult<Vec<BookSource>> {
    let rows: Vec<(Option<String>, Option<String>, i64, bool, bool)> = sqlx::query_as(
        "SELECT rules, content, book_source_type, unsupported_type, enabled FROM book_sources",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(rules, content, ty, unsupported, enabled)| {
            parse_source_row(rules, content).map(|mut src| {
                src.book_source_type = ty;
                src.unsupported_type = unsupported;
                src.enabled = enabled;
                src
            })
        })
        .collect())
}

/// 按书源 ID（book_source_url）取完整书源。
pub async fn load_source(pool: &SqlitePool, id: &str) -> AppResult<BookSource> {
    let row: Option<(Option<String>, Option<String>, i64, bool, bool)> = sqlx::query_as(
        "SELECT rules, content, book_source_type, unsupported_type, enabled FROM book_sources WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let (rules, content, ty, unsupported, enabled) =
        row.ok_or_else(|| AppError::NotFound("source not found".into()))?;
    let mut src = parse_source_row(rules, content)
        .ok_or_else(|| AppError::NotFound("source not found".into()))?;
    src.book_source_type = ty;
    src.unsupported_type = unsupported;
    src.enabled = enabled;
    Ok(src)
}

/// 列表按「权重降序 → 自定义排序升序 → 名称」排列（Legado 的默认书源顺序）。
/// 搜索按 `source_ids` 过滤时也复用该顺序（数字序号按此顺序的 1-based 位置解析）。
pub fn sort_sources(sources: &mut [BookSource]) {
    sources.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then_with(|| a.custom_order.cmp(&b.custom_order))
            .then_with(|| a.book_source_name.cmp(&b.book_source_name))
    });
}

pub async fn list(
    State(s): State<AppState>,
    _u: AuthUser,
) -> AppResult<Json<Vec<BookSourceListItem>>> {
    let mut sources = load_sources(&s.shared).await?;
    sort_sources(&mut sources);
    let health = crate::source_health::load_map(&s.shared)
        .await
        .unwrap_or_default();
    Ok(Json(
        sources
            .into_iter()
            .map(|x| {
                let h = health.get(&x.book_source_url);
                BookSourceListItem {
                    tier: h
                        .map(|h| h.tier.clone())
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| crate::source_quality::TIER_OBSERVING.to_string()),
                    score: h.map(|h| h.score).unwrap_or(50),
                    last_eval_at: h.and_then(|h| h.last_eval_at),
                    book_source_url: x.book_source_url,
                    book_source_name: x.book_source_name,
                    enabled: x.enabled,
                    book_source_group: x.book_source_group,
                    weight: x.weight,
                    custom_order: x.custom_order,
                    book_source_type: x.book_source_type,
                    type_name: crate::entities::source_type_name(x.book_source_type).to_string(),
                    unsupported_type: x.unsupported_type,
                }
            })
            .collect(),
    ))
}

/// 把 Legado 导出的 camelCase JSON 转为内部 snake_case 实体
fn normalize_source(body: Value) -> AppResult<BookSource> {
    let v = crate::serde_util::camel_to_snake_value(body);
    serde_json::from_value(v).map_err(|e| AppError::BadRequest(format!("invalid source: {e}")))
}

async fn store_source(pool: &SqlitePool, src: &BookSource) -> AppResult<()> {
    let content = serde_json::to_string(src)?;
    // rules 与 content 同时写入：旧读取路径（content）与新读取路径（rules）都能拿到数据
    sqlx::query(
        "INSERT INTO book_sources \
         (id, name, url, rules, content, enabled, book_source_type, unsupported_type, updated_by) \
         VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, 'admin') \
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, url=excluded.url, \
         rules=excluded.rules, content=excluded.content, enabled=excluded.enabled, \
         book_source_type=excluded.book_source_type, \
         unsupported_type=excluded.unsupported_type, \
         updated_at=datetime('now')",
    )
    .bind(&src.book_source_url)
    .bind(&src.book_source_name)
    .bind(&src.book_source_url)
    .bind(&content)
    .bind(src.enabled)
    .bind(src.book_source_type)
    .bind(src.unsupported_type)
    .execute(pool)
    .await?;
    Ok(())
}

/// 导入策略：仅 0(小说)/2(漫画) 正常落库；1(音频) 及其他类型也落库，
/// 但强制禁用并标记 `unsupported_type`，避免进入搜索/发现。
fn prepare_import(src: &mut BookSource) -> bool {
    let supported = crate::entities::is_supported_source_type(src.book_source_type);
    src.unsupported_type = !supported;
    if !supported {
        src.enabled = false;
    }
    supported
}

/// 漫画特征词：用于导入时给出 `type=2` 建议（不做自动改库）。
/// 中文“漫”单独出现会误伤（如“浪漫”），因此必须同时满足内容规则含 `<img` / `@js`。
const COMIC_NAME_HINTS: [&str; 13] = [
    "漫画", "漫", "mkz", "manhua", "comic", "manga", "baozimh", "kanman", "kuaikan",
    "ac.qq", "1kkk", "哔哩哔哩漫画", "腾讯动漫",
];

fn comic_name_hit(src: &BookSource) -> Option<String> {
    let name = src.book_source_name.to_lowercase();
    let url = src.book_source_url.to_lowercase();
    COMIC_NAME_HINTS
        .iter()
        .find(|h| name.contains(*h) || url.contains(*h))
        .map(|h| h.to_string())
}

fn comic_content_hit(src: &BookSource) -> bool {
    let Some(rc) = src.rule_content.as_ref() else {
        return false;
    };
    let joined = [rc.content.as_deref(), rc.image_style.as_deref(), rc.image_decode.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
    joined.contains("<img") || joined.contains("@js")
}

/// 导入时的类型推断：只产出“建议”，绝不修改 `src.book_source_type`。
fn suggest_comic_type(src: &BookSource) -> Option<Value> {
    if src.book_source_type == 2 {
        return None;
    }
    let hint = comic_name_hit(src)?;
    if !comic_content_hit(src) {
        return None;
    }
    Some(json!({
        "suggested_type": 2,
        "current_type": src.book_source_type,
        "matched": hint,
        "reason": "名称/URL 命中漫画特征且内容规则含 <img 或 @js 拼图",
    }))
}

#[derive(Deserialize, Default)]
pub struct ProbeQ {
    /// `probe=0` 可关闭导入体检；默认对“新增 + 规则变化 + 隔离区复活检查”的源自动体检。
    probe: Option<String>,
    /// 导入来源标识（合集 URL / 文件名）；用于同来源 diff 与 raw_sha 去重。
    source: Option<String>,
    /// collection | file | api | inline
    source_kind: Option<String>,
    /// `force=1`：人工确认后强制覆盖 `update_rejected` / `conflict`。
    force: Option<String>,
    /// 体检关键词（默认“测试”）。
    key: Option<String>,
}

impl ProbeQ {
    fn truthy(&self, raw: Option<&String>) -> bool {
        matches!(
            raw.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
            Some("1" | "true" | "yes" | "on")
        )
    }

    fn on(&self) -> bool {
        self.truthy(self.probe.as_ref())
    }

    fn probe_disabled(&self) -> bool {
        matches!(
            self.probe
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase())
                .as_deref(),
            Some("0" | "false" | "no" | "off")
        )
    }

    fn force(&self) -> bool {
        self.truthy(self.force.as_ref())
    }

    fn source_label(&self, default: &str) -> String {
        self.source
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(default)
            .to_string()
    }

    fn kind_label(&self, default: &str) -> String {
        self.source_kind
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(default)
            .to_string()
    }
}

/// 只有“支持的类型 + 有搜索规则与搜索地址”的源才能做搜索体检。
fn is_probeable(src: &BookSource) -> bool {
    crate::entities::is_supported_source_type(src.book_source_type)
        && src.rule_search.is_some()
        && src.search_url.is_some()
}

/// 导入/单条保存后的体检：逐条结果 + 汇总。失败源**不会自动禁用**，
/// 只通过 `suggested_enabled` 给出建议；引擎性失败（partial）建议保持启用。
async fn probe_report(
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

fn merge_probe(out: &mut Value, results: Vec<Value>, summary: Value) {
    if let Some(obj) = out.as_object_mut() {
        obj.insert("probe_results".into(), Value::Array(results));
        if let Some(obj2) = summary.as_object() {
            for (k, v) in obj2 {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
}

pub async fn upsert(
    State(s): State<AppState>,
    u: AuthUser,
    Query(pq): Query<ProbeQ>,
    Json(body): Json<Value>,
) -> AppResult<Json<serde_json::Value>> {
    crate::auth::ensure_admin(&u)?;
    let mut src = normalize_source(body)?;
    prepare_import(&mut src);
    store_source(&s.shared, &src).await?;
    let mut out = json!({
        "ok": true,
        "type": src.book_source_type,
        "type_name": crate::entities::source_type_name(src.book_source_type),
        "unsupported_type": src.unsupported_type,
        "enabled": src.enabled,
    });
    if pq.on() {
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
fn local_still_usable(
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
            let url = &src.book_source_url;
            let existing = existing_map.get(url);
            let quarantined = states.get(url).map(|st| st.is_quarantined()).unwrap_or(false);
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
    let probe_map: std::collections::HashMap<String, source_health::Probe> = probes
        .into_iter()
        .map(|p| (p.source_url.clone(), p))
        .collect();

    let mut ledger_items: Vec<source_ledger::ItemRecord> = Vec::with_capacity(list.len());
    let mut results: Vec<Value> = Vec::with_capacity(list.len());
    let (mut added, mut updated, mut unchanged, mut update_rejected, mut conflict, mut resurrected) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut newly_quarantined = 0usize;
    let mut probed_count = 0usize;
    let mut probe_ok = 0usize;
    let mut probe_fail_site = 0usize;
    let mut probe_engine_limited = 0usize;
    let mut kept_enabled = 0usize;
    let mut disable_suggested = 0usize;
    let mut type_suggestions = 0usize;
    let (mut tier_preferred, mut tier_observing, mut tier_quarantined) =
        (0usize, 0usize, 0usize);

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
        let probe_status = match probe {
            Some(p) if p.probed => p.status,
            _ => source_health::STATUS_NEVER,
        };
        let historical = health_after.get(&url).map(|h| h.last_status.as_str());
        let engine_limited = probe_status == source_health::STATUS_PARTIAL
            || (!probed && historical == Some(source_health::STATUS_PARTIAL));

        // ---- 同一条流水线：规则完整度 + 本次体检连通性 + 既有成功侧统计 → tier/score ----
        let tier_cfg = crate::source_quality::TierConfig::from_config(&s.cfg);
        let rule_complete = crate::source_quality::rule_completeness(src);
        let incoming_network = if probed {
            source_quality::Network::from_status(probe_status, true)
        } else {
            source_quality::Network::Skipped
        };
        let revive = was_quarantined && probed && probe_status != source_health::STATUS_FAIL;
        let incoming_eval = source_health::evaluate_source(
            health_after.get(&url),
            rule_complete,
            incoming_network,
            if revive {
                Some(source_quality::TIER_OBSERVING)
            } else {
                None
            },
            &tier_cfg,
        );
        let local_score = health_before.get(&url).map(|h| h.score).unwrap_or(50);

        let mut accepted = true;
        let mut decision = state
            .map(|st| st.decision.clone())
            .unwrap_or_else(|| source_ledger::DECISION_ACTIVE.to_string());
        let mut category = "unchanged";
        let mut reason: Option<String> = None;

        if local_modified && changed && !force {
            // 人工改过的本地规则优先：不自动覆盖，等待确认
            accepted = false;
            category = "conflict";
            reason = Some("本地规则与账本快照不一致，已保留本地版本".into());
        } else if existing.is_none() {
            category = "added";
            decision = if probed && probe_status == source_health::STATUS_FAIL
                && !engine_limited
            {
                source_ledger::DECISION_QUARANTINED.to_string()
            } else {
                source_ledger::DECISION_ACTIVE.to_string()
            };
        } else if changed {
            if force {
                decision = if probe_status == source_health::STATUS_FAIL && !engine_limited {
                    source_ledger::DECISION_QUARANTINED.to_string()
                } else {
                    source_ledger::DECISION_ACTIVE.to_string()
                };
                category = if was_quarantined
                    && decision == source_ledger::DECISION_ACTIVE
                {
                    "resurrected"
                } else {
                    "updated"
                };
            } else if !probed {
                accepted = false;
                category = "update_rejected";
                reason = Some("新版体检未完成，暂不覆盖本地版本".into());
            } else if probe_status == source_health::STATUS_FAIL
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
                decision = if probe_status == source_health::STATUS_FAIL && !engine_limited {
                    source_ledger::DECISION_QUARANTINED.to_string()
                } else {
                    source_ledger::DECISION_ACTIVE.to_string()
                };
                category = if was_quarantined
                    && decision == source_ledger::DECISION_ACTIVE
                {
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
        {
            decision = source_ledger::DECISION_QUARANTINED.to_string();
            if reason.is_none() {
                reason = Some(format!(
                    "quality_quarantine: score={}",
                    final_eval.score
                ));
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
        if decision == source_ledger::DECISION_QUARANTINED {
            // 隔离源不仅要有 ledger decision，还必须 enabled=0，避免任何旧路径把它带进搜索。
            stored.enabled = false;
        }
        if accepted {
            store_source(&s.shared, &stored).await?;
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
                store_source(&s.shared, &local).await?;
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

/// 导出过滤：`tier=preferred|observing|all`（默认 all，保持旧行为）。
#[derive(Deserialize, Default)]
pub struct ExportTierQ {
    tier: Option<String>,
}

/// 导出为 Legado 兼容格式（camelCase、数组），供 Android 客户端直接导入。
pub async fn export_all(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<ExportTierQ>,
) -> AppResult<Json<Vec<Value>>> {
    // 安全口径：导出本地源库时排除隔离区条目，避免把源里自带的认证头再分发出去。
    let quarantined = source_ledger::quarantined_set(&s.shared)
        .await
        .unwrap_or_default();
    let health = crate::source_health::load_map(&s.shared)
        .await
        .unwrap_or_default();
    let want_tier = q.tier.as_deref().map(str::trim).unwrap_or("all");
    let list: Vec<BookSource> = load_sources(&s.shared)
        .await?
        .into_iter()
        .filter(|x| !quarantined.contains(&x.book_source_url))
        .filter(|x| match want_tier {
            "preferred" => health
                .get(&x.book_source_url)
                .map(|h| h.tier == crate::source_quality::TIER_PREFERRED)
                .unwrap_or(false),
            "observing" => health
                .get(&x.book_source_url)
                .map(|h| h.tier == crate::source_quality::TIER_OBSERVING)
                .unwrap_or(true),
            _ => true,
        })
        .collect();
    let out = list
        .into_iter()
        .filter_map(|src| serde_json::to_value(src).ok())
        .map(crate::serde_util::snake_to_camel_value)
        .collect();
    Ok(Json(out))
}

/// 批量/单个启停书源：只改 `enabled`，不要求前端回取详情再整份 import。
#[derive(Deserialize)]
pub struct EnabledQ {
    url: Option<String>,
    urls: Option<Vec<String>>,
    enabled: bool,
}

pub async fn set_enabled(
    State(s): State<AppState>,
    u: AuthUser,
    Json(q): Json<EnabledQ>,
) -> AppResult<Json<serde_json::Value>> {
    crate::auth::ensure_admin(&u)?;
    let mut ids: Vec<String> = q.urls.unwrap_or_default();
    if let Some(u) = q.url {
        ids.push(u);
    }
    if ids.is_empty() {
        return Err(AppError::BadRequest("url or urls required".into()));
    }
    let mut updated = 0usize;
    let mut missing: Vec<String> = Vec::new();
    let mut unsupported: Vec<String> = Vec::new();
    for id in ids {
        match load_source(&s.shared, &id).await {
            Ok(mut src) => {
                // 非小说/漫画源不允许启用，避免进入搜索/发现
                if q.enabled && !crate::entities::is_supported_source_type(src.book_source_type) {
                    src.enabled = false;
                    src.unsupported_type = true;
                    store_source(&s.shared, &src).await?;
                    unsupported.push(id);
                    continue;
                }
                src.enabled = q.enabled;
                store_source(&s.shared, &src).await?;
                updated += 1;
            }
            Err(_) => missing.push(id),
        }
    }
    Ok(Json(json!({
        "ok": true,
        "enabled": q.enabled,
        "updated": updated,
        "missing": missing,
    })))
}

pub async fn get_one(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<UrlQ>,
) -> AppResult<Json<serde_json::Value>> {
    let src = load_source(&s.shared, &q.url).await?;
    let health = crate::source_health::load_map(&s.shared)
        .await
        .unwrap_or_default();
    let row = health.get(&src.book_source_url);
    let mut v = serde_json::to_value(&src)?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert("type".into(), json!(src.book_source_type));
        obj.insert(
            "type_name".into(),
            json!(crate::entities::source_type_name(src.book_source_type)),
        );
        obj.insert("unsupported_type".into(), json!(src.unsupported_type));
        obj.insert(
            "tier".into(),
            json!(row
                .map(|h| h.tier.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| crate::source_quality::TIER_OBSERVING.to_string())),
        );
        obj.insert("score".into(), json!(row.map(|h| h.score).unwrap_or(50)));
        obj.insert("last_eval_at".into(), json!(row.and_then(|h| h.last_eval_at)));
        obj.insert(
            "evidence".into(),
            json!(row
                .and_then(|h| h.evidence_json.as_deref())
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())),
        );
    }
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
pub struct ValidateReq {
    /// 待校验书源（camelCase 或 snake_case 均可）；缺省时直接解析整个请求体
    #[serde(default)]
    source: Option<Value>,
    /// 是否做 URL 可达性检查（默认关闭，避免校验接口被拖慢）
    #[serde(default)]
    check_url: bool,
}

/// 书源校验：规则必填项 + 可选 URL 可达性。返回 errors/warnings，不阻塞保存。
pub async fn validate(
    State(s): State<AppState>,
    _u: AuthUser,
    body: Option<Json<Value>>,
) -> AppResult<Json<Value>> {
    let raw = body.map(|Json(v)| v).unwrap_or(Value::Null);
    let parsed: ValidateReq = serde_json::from_value(raw.clone()).unwrap_or_default();
    let payload = parsed.source.unwrap_or(raw);
    let src = match normalize_source(payload) {
        Ok(s) => s,
        Err(e) => {
            return Ok(Json(json!({
                "valid": false, "errors": [e.to_string()], "warnings": []
            })))
        }
    };
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if !src.book_source_url.starts_with("http://") && !src.book_source_url.starts_with("https://") {
        errors.push("bookSourceUrl 必须是 http(s) URL".into());
    }
    if src.book_source_name.trim().is_empty() {
        errors.push("bookSourceName 不能为空".into());
    }
    let search_url = src.search_url.as_deref().unwrap_or("").trim();
    let explore_url = src.explore_url.as_deref().unwrap_or("").trim();
    if search_url.is_empty() && explore_url.is_empty() {
        warnings.push("既无 searchUrl 也无 exploreUrl，书源不可用于搜索/发现".into());
    }
    if !search_url.is_empty() {
        match src.rule_search.as_ref().and_then(|r| r.book_list.as_deref()) {
            Some(b) if !b.trim().is_empty() => {}
            _ => errors.push("配置了 searchUrl 但缺少 ruleSearch.bookList".into()),
        }
    }
    if let Some(r) = &src.rule_toc {
        if r.chapter_list.as_deref().unwrap_or("").trim().is_empty() {
            errors.push("ruleToc 缺少 chapterList".into());
        }
    }
    if let Some(r) = &src.rule_content {
        if r.content.as_deref().unwrap_or("").trim().is_empty() {
            errors.push("ruleContent 缺少 content".into());
        }
    }
    if !explore_url.is_empty() && src.rule_explore.is_none() {
        errors.push("配置了 exploreUrl 但缺少 ruleExplore".into());
    }

    if parsed.check_url && errors.is_empty() {
        let headers = crate::req::source_headers(&src, &src.book_source_url);
        match crate::req::fetch_for_source(
            &src.book_source_url,
            "GET",
            &headers,
            None,
            &src.book_source_url,
        )
        .await
        {
            Ok(_) => warnings.push("bookSourceUrl 可访问".into()),
            Err(e) => warnings.push(format!("bookSourceUrl 不可访问: {e}")),
        }
    }

    Ok(Json(json!({
        "valid": errors.is_empty(),
        "errors": errors,
        "warnings": warnings,
    })))
}

pub async fn delete_one(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<UrlQ>,
) -> AppResult<Json<serde_json::Value>> {
    crate::auth::ensure_admin(&u)?;
    sqlx::query("DELETE FROM book_sources WHERE id = ?")
        .bind(&q.url)
        .execute(&s.shared)
        .await?;
    Ok(Json(json!({"ok": true})))
}

// ---------------- 书源更新检查（只读） ----------------

#[derive(Debug, Clone, Serialize)]
pub struct SourceUpdateItem {
    pub book_source_url: String,
    pub book_source_name: String,
    pub local_update_time: i64,
    pub incoming_update_time: i64,
}

/// 只读比对：按 `bookSourceUrl` 对齐本地与导入包，产出
/// `updated`（导入包更新）/ `new`（本地没有）/ `local_newer`（本地更新）/ `same`。
/// 不写库，调用方自行决定是否 import。
pub(crate) fn diff_sources(local: &[BookSource], incoming: &[BookSource]) -> Value {
    use std::collections::HashMap;
    let map: HashMap<&str, &BookSource> = local
        .iter()
        .map(|s| (s.book_source_url.as_str(), s))
        .collect();
    let mut updated: Vec<SourceUpdateItem> = Vec::new();
    let mut added: Vec<SourceUpdateItem> = Vec::new();
    let mut local_newer: Vec<SourceUpdateItem> = Vec::new();
    let mut same = 0usize;
    for src in incoming {
        match map.get(src.book_source_url.as_str()) {
            None => added.push(SourceUpdateItem {
                book_source_url: src.book_source_url.clone(),
                book_source_name: src.book_source_name.clone(),
                local_update_time: 0,
                incoming_update_time: src.last_update_time,
            }),
            Some(old) => {
                let item = SourceUpdateItem {
                    book_source_url: src.book_source_url.clone(),
                    book_source_name: if src.book_source_name.is_empty() {
                        old.book_source_name.clone()
                    } else {
                        src.book_source_name.clone()
                    },
                    local_update_time: old.last_update_time,
                    incoming_update_time: src.last_update_time,
                };
                if src.last_update_time > old.last_update_time {
                    updated.push(item);
                } else if src.last_update_time < old.last_update_time {
                    local_newer.push(item);
                } else {
                    same += 1;
                }
            }
        }
    }
    json!({
        "updated": updated,
        "new": added,
        "local_newer": local_newer,
        "same": same,
        "has_updates": !updated.is_empty() || !added.is_empty(),
    })
}

/// 批量比对书源更新时间：请求体为 Legado 导出数组，或 `{ "sources": [...] }`。
pub async fn check_updates(
    State(s): State<AppState>,
    _u: AuthUser,
    Json(body): Json<Value>,
) -> AppResult<Json<Value>> {
    let raw = match body {
        Value::Object(mut o) => o
            .remove("sources")
            .unwrap_or(Value::Array(Vec::new())),
        other => other,
    };
    let items = match raw {
        Value::Array(a) => a,
        Value::Null => vec![],
        one => vec![one],
    };
    let mut incoming: Vec<BookSource> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        match normalize_source(item) {
            Ok(src) => incoming.push(src),
            Err(e) => errors.push(format!("第 {} 条: {e}", i + 1)),
        }
    }
    let local = load_sources(&s.shared).await?;
    let mut out = diff_sources(&local, &incoming);
    if let Some(obj) = out.as_object_mut() {
        obj.insert("ok".into(), json!(true));
        obj.insert("checked".into(), json!(incoming.len()));
        obj.insert("errors".into(), json!(errors));
    }
    Ok(Json(out))
}

fn split_ids(raw: Option<&str>) -> Vec<String> {
    raw.map(|s| {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

fn truthy(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

#[derive(Deserialize, Default)]
pub struct HealthCheckQ {
    /// 逗号分隔的 bookSourceUrl；为空则检查全部“启用且可搜索”的源。
    source_ids: Option<String>,
    /// `1` 时只测“未测过 / 冷却到期 / 距上次超过冷却时长”的源。
    only_due: Option<String>,
    key: Option<String>,
}

/// `POST /api/sources/health/check`：只跑一次“搜索”探测，并发 ≤8，整批 ≤90s。
pub async fn health_check(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<HealthCheckQ>,
) -> AppResult<Json<Value>> {
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
    detail: Option<String>,
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
    let (mut total, mut ok, mut partial, mut fail, mut cooling, mut never) = (
        0usize, 0usize, 0usize, 0usize, 0usize, 0usize,
    );
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
        let decision =
            decision.unwrap_or_else(|| source_ledger::DECISION_ACTIVE.to_string());
        if decision == source_ledger::DECISION_QUARANTINED {
            quarantined += 1;
        } else {
            active += 1;
        }
        if detail_on {
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
        "now": now,
        "detail": detail_on,
        "sources": if detail_on { Value::Array(details) } else { Value::Null },
    })))
}

#[derive(Deserialize, Default)]
pub struct ResetReq {
    source_ids: Option<Vec<String>>,
    url: Option<String>,
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
    Ok(Json(json!({
        "ok": true,
        "cleared": cleared,
        "scope": if ids.is_empty() { "all" } else { "selected" },
    })))
}

#[derive(Deserialize, Default)]
pub struct PruneQ {
    within_days: Option<i64>,
    action: Option<String>,
    confirm: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct PruneReq {
    within_days: Option<i64>,
    action: Option<String>,
    confirm: Option<bool>,
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
    let confirm = truthy(q.confirm.as_deref())
        || body.as_ref().and_then(|b| b.confirm).unwrap_or(false);

    let now = source_health::now_ms();
    let since = now.saturating_sub((days as i64).saturating_mul(86_400_000));
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
        let id = c.get("book_source_url").and_then(|v| v.as_str()).unwrap_or("");
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
                    if let Err(e) = store_source(&s.shared, &src).await {
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
    source_ids: Vec<String>,
    reason: Option<String>,
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

#[derive(Deserialize, Default)]
pub struct BenchCorpusInput {
    pub title: Option<String>,
    pub expected_author: Option<String>,
    pub category: Option<String>,
    #[serde(default, alias = "type")]
    pub source_type: Option<i64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

impl BenchCorpusInput {
    fn into_corpus_input(self, enabled_default: bool) -> crate::source_bench::CorpusInput {
        crate::source_bench::CorpusInput {
            title: self.title.unwrap_or_default(),
            expected_author: self.expected_author,
            category: self.category.unwrap_or_default(),
            source_type: self.source_type.unwrap_or(crate::source_bench::TYPE_NOVEL),
            enabled: self.enabled.unwrap_or(enabled_default),
        }
    }
}

/// `GET /api/sources/bench/corpus`：内置 ∪ 用户语料（内置带 builtin 标志）。
pub async fn bench_corpus(
    State(s): State<AppState>,
    _u: AuthUser,
) -> AppResult<Json<Value>> {
    let corpus = crate::source_bench::load_corpus(&s.shared).await?;
    let builtin = corpus.iter().filter(|c| c.builtin).count();
    let user = corpus.len() - builtin;
    let novel = corpus.iter().filter(|c| c.source_type == crate::source_bench::TYPE_NOVEL).count();
    let comic = corpus.iter().filter(|c| c.source_type == crate::source_bench::TYPE_COMIC).count();
    Ok(Json(json!({
        "items": corpus,
        "total": corpus.len(),
        "builtin_count": builtin,
        "user_count": user,
        "novel_count": novel,
        "comic_count": comic,
    })))
}

/// `POST /api/sources/bench/corpus`：新增用户语料（撞内置书名返回 400）。
pub async fn bench_corpus_add(
    State(s): State<AppState>,
    u: AuthUser,
    Json(req): Json<BenchCorpusInput>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    let item = crate::source_bench::add_user_corpus(&s.shared, req.into_corpus_input(true)).await?;
    Ok(Json(json!({ "ok": true, "item": item })))
}

#[derive(Deserialize)]
pub struct BenchCorpusUpdateReq {
    pub id: String,
    #[serde(flatten)]
    pub input: BenchCorpusInput,
}

/// `POST /api/sources/bench/corpus/update`：编辑用户语料（内置条目返回 400）。
pub async fn bench_corpus_update(
    State(s): State<AppState>,
    u: AuthUser,
    Json(req): Json<BenchCorpusUpdateReq>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    let item = crate::source_bench::update_user_corpus(
        &s.shared,
        &req.id,
        req.input.into_corpus_input(true),
    )
    .await?;
    Ok(Json(json!({ "ok": true, "item": item })))
}

#[derive(Deserialize)]
pub struct BenchToggleReq {
    pub id: String,
    pub enabled: bool,
}

/// `POST /api/sources/bench/corpus/toggle`：内置可停用（写覆盖行），用户条目直接改。
pub async fn bench_corpus_toggle(
    State(s): State<AppState>,
    u: AuthUser,
    Json(req): Json<BenchToggleReq>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    let item = crate::source_bench::toggle_corpus(&s.shared, &req.id, req.enabled).await?;
    Ok(Json(json!({ "ok": true, "item": item })))
}

#[derive(Deserialize)]
pub struct BenchCorpusIdQ {
    pub id: String,
}

/// `DELETE /api/sources/bench/corpus?id=...`：内置条目一律 400，不做静默忽略。
pub async fn bench_corpus_delete(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<BenchCorpusIdQ>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    crate::source_bench::delete_corpus(&s.shared, &q.id).await?;
    Ok(Json(json!({ "ok": true, "id": q.id })))
}

#[derive(Deserialize, Default)]
pub struct BenchRunReq {
    /// "all" 或 ["https://..."]；省略等价于 all
    #[serde(default)]
    pub source_ids: Option<Value>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `POST /api/sources/bench/run`：跑一次体检，返回每源命中/延迟/错误与精选清单。
pub async fn bench_run(
    State(s): State<AppState>,
    u: AuthUser,
    body: Option<Json<BenchRunReq>>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    let req = body.map(|Json(v)| v).unwrap_or_default();
    let ids = match req.source_ids {
        None => None,
        Some(Value::String(v)) => {
            if v.eq_ignore_ascii_case("all") {
                None
            } else {
                Some(vec![v])
            }
        }
        Some(Value::Array(arr)) => {
            let mut list = Vec::with_capacity(arr.len());
            for item in arr {
                let sid = item
                    .as_str()
                    .ok_or_else(|| AppError::BadRequest("source_ids 必须是字符串数组或 \"all\"".into()))?;
                list.push(sid.to_string());
            }
            Some(list)
        }
        _ => return Err(AppError::BadRequest("source_ids 必须是字符串数组或 \"all\"".into())),
    };
    let report = crate::source_bench::run_bench(&s.shared, &s.cfg, ids, req.limit).await?;
    Ok(Json(serde_json::to_value(report).unwrap_or(Value::Null)))
}

/// `GET /api/sources/bench/curated`：当前精选清单（小说 / 漫画两套）。
pub async fn bench_curated(
    State(s): State<AppState>,
    _u: AuthUser,
) -> AppResult<Json<Value>> {
    let (novel, comic) = crate::source_bench::curated_lists(&s.shared).await?;
    let total = novel.len() + comic.len();
    Ok(Json(json!({
        "novel": novel,
        "comic": comic,
        "curated_total": total,
        "novel_total": novel.len(),
        "comic_total": comic.len(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(url: &str, name: &str, t: i64) -> BookSource {
        let mut s = BookSource::default();
        s.book_source_url = url.into();
        s.book_source_name = name.into();
        s.last_update_time = t;
        s
    }

    // 只读比对：更新 / 新增 / 本地更新 / 相同 四类都能区分
    #[test]
    fn diff_reports_update_new_and_same() {
        let local = vec![
            src("https://a.com", "A", 100),
            src("https://b.com", "B", 200),
            src("https://c.com", "C", 300),
        ];
        let incoming = vec![
            src("https://a.com", "A", 101),
            src("https://b.com", "B", 199),
            src("https://c.com", "C", 300),
            src("https://d.com", "D", 50),
        ];
        let v = diff_sources(&local, &incoming);
        assert_eq!(v["updated"].as_array().unwrap().len(), 1);
        assert_eq!(v["updated"][0]["book_source_url"], "https://a.com");
        assert_eq!(v["local_newer"].as_array().unwrap().len(), 1);
        assert_eq!(v["new"].as_array().unwrap().len(), 1);
        assert_eq!(v["new"][0]["local_update_time"], 0);
        assert_eq!(v["same"], 1);
        assert_eq!(v["has_updates"], true);
    }

    // 没有任何变化时 has_updates=false
    #[test]
    fn diff_without_updates() {
        let local = vec![src("https://a.com", "A", 100)];
        let incoming = vec![src("https://a.com", "A", 100)];
        let v = diff_sources(&local, &incoming);
        assert_eq!(v["has_updates"], false);
        assert_eq!(v["same"], 1);
    }

    // 导入策略：0/2 正常启用；1 及其他类型强制禁用并标记不支持
    #[test]
    fn import_policy_disables_unsupported_types() {
        for (ty, supported) in [(0, true), (2, true), (1, false), (3, false), (99, false)] {
            let mut s = BookSource::default();
            s.book_source_type = ty;
            s.enabled = true;
            let kept = prepare_import(&mut s);
            assert_eq!(kept, supported, "type {ty} 的支持性判断错误");
            assert_eq!(s.unsupported_type, !supported);
            assert_eq!(s.enabled, supported, "type {ty} 的默认启用状态错误");
        }
    }

    /// 回归：现有 422 源里 9 条“漫画源存成 type=0”必须全部被判定为漫画（仅建议，不改库）。
    #[test]
    fn suggests_type2_for_mislabelled_comic_sources() {
        // (名称, URL, 真实 rule_content.content 片段)
        let cases: [(&str, &str, &str); 9] = [
            ("拷贝漫画（优++）", "https://www.mangacopy.com/", "$..url\n<js>\nresult.split(\"\\n\").map(x=>'<img src=\"'+x+'\">').join(\"\\n\")"),
            ("爱看漫画（优+）", "https://m.kanman.com#♤Haxc", "$.data.current_chapter.chapter_img_list@js:\nheader={\"Referer\":baseUrl}"),
            ("爱看漫画（优）", "https://m.kanman.com/", "$.data.current_chapter.chapter_img_list@js:\nheader={\"Referer\":baseUrl}"),
            ("爱看漫画（优）", "https://m.kanman.com#", "$.data.current_chapter.chapter_img_list@js:\nheader={\"Referer\":baseUrl}"),
            ("酷看漫画（优）", "https://www.kukk.net#♤Haxc", ".lazy@html##src=.*\"@js:result.replace(/data-original/g,\"src"),
            ("奇漫屋子（优）", "https://m.qimanwu.app", "img@data-src||img@src\n@js:\nresult.split(\"\\n\").map(url => `<img src=\"${url}\">`)"),
            ("星际漫画（优）", "http://www.xmanhua.com#♤Haxc", "@js:\ntry{header={\"Referer\":baseUrl};headers={\"headers\":JS"),
            ("爱漫客栈", "https://www.mkzhan.com", "$..image@js:result.split(\"\\n\").map(x=>'<img src=\"'+x+'\">')"),
            ("爱漫客栈", "https://www.mkzhan.com#yc1101", "$..image@js:result.split(\"\\n\").map(x=>'<img src=\"'+x+'\">')"),
        ];
        for (name, url, content) in cases {
            let s = BookSource {
                book_source_name: name.to_string(),
                book_source_url: url.to_string(),
                book_source_type: 0,
                rule_content: Some(crate::entities::ContentRule {
                    content: Some(content.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let sug = suggest_comic_type(&s).unwrap_or_else(|| panic!("{name} {url} 未被判为漫画"));
            assert_eq!(sug["suggested_type"], 2, "{name} 建议类型错误");
            assert_eq!(sug["current_type"], 0, "{name} 当前类型不应被改写");
            assert_eq!(s.book_source_type, 0, "建议不得修改源本身");
        }
    }

    #[test]
    fn no_comic_suggestion_for_novel_or_already_comic() {
        // 小说源：名字不含漫画特征
        let novel = BookSource {
            book_source_name: "企鹅浏览".to_string(),
            book_source_url: "https://qbnovel.qq.com".to_string(),
            book_source_type: 0,
            rule_content: Some(crate::entities::ContentRule {
                content: Some("class.content@html".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(suggest_comic_type(&novel).is_none());

        // 名字像漫画但内容规则没有图片特征 → 不盲判
        let text = BookSource {
            book_source_name: "漫画小说站".to_string(),
            book_source_url: "https://example.com".to_string(),
            book_source_type: 0,
            rule_content: Some(crate::entities::ContentRule {
                content: Some("class.content@text".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(suggest_comic_type(&text).is_none());

        // 已经是 type=2 → 无需建议
        let comic = BookSource {
            book_source_name: "酸奶漫画".to_string(),
            book_source_url: "https://m.1kkk.com/".to_string(),
            book_source_type: 2,
            rule_content: Some(crate::entities::ContentRule {
                content: Some("@js:img".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(suggest_comic_type(&comic).is_none());
    }

    /// 回归（真库 2026-11 实测漂移）：`book_sources.enabled=0` 但 JSON 里 `enabled` 仍为 true
    /// （迁移 0005 与维护脚本只 `UPDATE ... SET enabled=0`，不重写 JSON）。
    /// 停用源不得进入 `source_total`（准入集合），也不得出现在搜索结果 `origin`（errors.origin 均无）。
    #[tokio::test]
    async fn disabled_column_wins_over_drifted_json() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE book_sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT, \
             rules TEXT NOT NULL, content TEXT, enabled INTEGER NOT NULL DEFAULT 1, \
             book_source_type INTEGER NOT NULL DEFAULT 0, unsupported_type INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // 两条规则除 URL 外完全相同（JSON 里都写 enabled=true），只有 enabled 列不同 → 模拟漂移。
        for (id, enabled) in [("https://on.example.com", 1), ("https://off.example.com", 0)] {
            let rules = serde_json::json!({
                "book_source_url": id,
                "book_source_name": id,
                "enabled": true,
                "search_url": "http://127.0.0.1:1/search?key=%s",
                "rule_search": {"book_list": ""}
            })
            .to_string();
            sqlx::query(
                "INSERT INTO book_sources (id, name, url, rules, content, enabled) \
                 VALUES (?1, ?2, ?1, ?3, ?3, ?4)",
            )
            .bind(id)
            .bind(id)
            .bind(&rules)
            .bind(enabled)
            .execute(&pool)
            .await
            .unwrap();
        }

        let loaded = load_sources(&pool).await.unwrap();
        assert_eq!(loaded.len(), 2);
        let off = loaded
            .iter()
            .find(|s| s.book_source_url == "https://off.example.com")
            .unwrap();
        assert!(!off.enabled, "列 enabled=0 必须覆盖 JSON 的 enabled=true");
        assert!(!load_source(&pool, "https://off.example.com")
            .await
            .unwrap()
            .enabled);

        // 与 search_start 相同的准入条件：source_total 只数启用源。
        let eligible: Vec<BookSource> = loaded
            .iter()
            .filter(|x| {
                x.enabled
                    && crate::entities::is_supported_source_type(x.book_source_type)
                    && x.rule_search.is_some()
                    && x.search_url.is_some()
            })
            .cloned()
            .collect();
        assert_eq!(eligible.len(), 1, "source_total 不应包含停用源");
        assert_eq!(eligible[0].book_source_url, "https://on.example.com");

        // origin：把「加载后的全量」交给聚合搜索，停用源必须一次请求都不发。
        let out = crate::web_book::search_all_with_errors_budgeted(
            &loaded,
            "test",
            1,
            std::time::Duration::from_millis(800),
        )
        .await;
        assert!(
            out.errors.iter().all(|e| e.source != "https://off.example.com"),
            "停用源出现在搜索结果 origin 中"
        );
    }
}
