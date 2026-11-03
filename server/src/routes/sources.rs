use axum::{
    extract::{Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::SqlitePool;

use crate::auth::AuthUser;
use crate::entities::BookSource;
use crate::error::{AppError, AppResult};
use crate::models::BookSourceListItem;
use crate::routes::AppState;

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
/// 类型列（`book_source_type`/`unsupported_type`）以数据库为准覆盖 JSON 里的值，
/// 这样历史存量在迁移回填后无需重写 JSON 就能被正确归类。
pub async fn load_sources(pool: &SqlitePool) -> AppResult<Vec<BookSource>> {
    let rows: Vec<(Option<String>, Option<String>, i64, bool)> = sqlx::query_as(
        "SELECT rules, content, book_source_type, unsupported_type FROM book_sources",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(rules, content, ty, unsupported)| {
            parse_source_row(rules, content).map(|mut src| {
                src.book_source_type = ty;
                src.unsupported_type = unsupported;
                src
            })
        })
        .collect())
}

/// 按书源 ID（book_source_url）取完整书源。
pub async fn load_source(pool: &SqlitePool, id: &str) -> AppResult<BookSource> {
    let row: Option<(Option<String>, Option<String>, i64, bool)> = sqlx::query_as(
        "SELECT rules, content, book_source_type, unsupported_type FROM book_sources WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let (rules, content, ty, unsupported) =
        row.ok_or_else(|| AppError::NotFound("source not found".into()))?;
    let mut src = parse_source_row(rules, content)
        .ok_or_else(|| AppError::NotFound("source not found".into()))?;
    src.book_source_type = ty;
    src.unsupported_type = unsupported;
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
    Ok(Json(
        sources
            .into_iter()
            .map(|x| BookSourceListItem {
                book_source_url: x.book_source_url,
                book_source_name: x.book_source_name,
                enabled: x.enabled,
                book_source_group: x.book_source_group,
                weight: x.weight,
                custom_order: x.custom_order,
                book_source_type: x.book_source_type,
                type_name: crate::entities::source_type_name(x.book_source_type).to_string(),
                unsupported_type: x.unsupported_type,
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

pub async fn upsert(
    State(s): State<AppState>,
    u: AuthUser,
    Json(body): Json<Value>,
) -> AppResult<Json<serde_json::Value>> {
    crate::auth::ensure_admin(&u)?;
    let mut src = normalize_source(body)?;
    prepare_import(&mut src);
    store_source(&s.shared, &src).await?;
    Ok(Json(json!({
        "ok": true,
        "type": src.book_source_type,
        "type_name": crate::entities::source_type_name(src.book_source_type),
        "unsupported_type": src.unsupported_type,
        "enabled": src.enabled,
    })))
}

/// 批量导入：同一批内按 book_source_url 去重（后者覆盖前者），并区分新增/更新数量。
pub async fn import(
    State(s): State<AppState>,
    u: AuthUser,
    Json(body): Json<Value>,
) -> AppResult<Json<serde_json::Value>> {
    crate::auth::ensure_admin(&u)?;
    let items: Vec<Value> = match body {
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
    let mut inserted = 0usize;
    let mut updated = 0usize;
    let mut disabled_unsupported = 0usize;
    for src in &mut list {
        if !prepare_import(src) {
            disabled_unsupported += 1;
        }
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_sources WHERE id = ?")
            .bind(&src.book_source_url)
            .fetch_one(&s.shared)
            .await?;
        store_source(&s.shared, src).await?;
        if exists > 0 {
            updated += 1;
        } else {
            inserted += 1;
        }
    }
    Ok(Json(json!({
        "ok": true,
        "count": list.len(),
        "inserted": inserted,
        "updated": updated,
        // 契约变更：导入统计，明确有多少条因类型不受支持被默认禁用
        "imported": list.len(),
        "disabled_unsupported": disabled_unsupported,
    })))
}

/// 导出为 Legado 兼容格式（camelCase、数组），供 Android 客户端直接导入。
pub async fn export_all(
    State(s): State<AppState>,
    _u: AuthUser,
) -> AppResult<Json<Vec<Value>>> {
    let list = load_sources(&s.shared).await?;
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
    let mut v = serde_json::to_value(&src)?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert("type".into(), json!(src.book_source_type));
        obj.insert(
            "type_name".into(),
            json!(crate::entities::source_type_name(src.book_source_type)),
        );
        obj.insert("unsupported_type".into(), json!(src.unsupported_type));
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
}
