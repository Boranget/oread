//! sources 路由的导出、启停、详情与校验（move-only 从 sources.rs 拆出）。

use super::*;

/// 导出过滤：`tier=preferred|observing|all`（默认 all，保持旧行为）。
#[derive(Deserialize, Default)]
pub struct ExportTierQ {
    pub(super) tier: Option<String>,
}

/// 导出为 Legado 兼容格式（camelCase、数组），供 Android 客户端直接导入。
pub async fn export_all(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<ExportTierQ>,
) -> AppResult<Json<Vec<Value>>> {
    // 导出包含 header 等源站凭据，只允许管理员。
    crate::auth::ensure_admin(&u)?;
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
    tracing::info!(
        actor = %u.username,
        tier = want_tier,
        count = list.len(),
        "source export"
    );
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
    pub(super) url: Option<String>,
    pub(super) urls: Option<Vec<String>>,
    pub(super) enabled: bool,
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
                    store_source(&s.shared, &src, &u.username).await?;
                    unsupported.push(id);
                    continue;
                }
                src.enabled = q.enabled;
                store_source(&s.shared, &src, &u.username).await?;
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
    u: AuthUser,
    Query(q): Query<UrlQ>,
) -> AppResult<Json<serde_json::Value>> {
    // 详情含 header/rules，普通用户不可见。
    crate::auth::ensure_admin(&u)?;
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
        obj.insert(
            "last_eval_at".into(),
            json!(row.and_then(|h| h.last_eval_at)),
        );
        obj.insert(
            "evidence".into(),
            json!(row
                .and_then(|h| h.evidence_json.as_deref())
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())),
        );
        // 字段级规则异常只在详情里给出全部上下文（列表接口只给计数/时间）。
        obj.insert(
            "rule_errors".into(),
            json!(crate::source_health::rule_anomaly(&src.book_source_url)),
        );
    }
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
pub struct ValidateReq {
    /// 待校验书源（camelCase 或 snake_case 均可）；缺省时直接解析整个请求体
    #[serde(default)]
    pub(super) source: Option<Value>,
    /// 是否做 URL 可达性检查（默认关闭，避免校验接口被拖慢）
    #[serde(default)]
    pub(super) check_url: bool,
}

/// 书源校验：规则必填项 + 可选 URL 可达性。返回 errors/warnings，不阻塞保存。
/// `check_url=true` 会代服务端发起外连，仅管理员可用，避免普通用户驱动 SSRF/探测。
pub async fn validate(
    State(_s): State<AppState>,
    u: AuthUser,
    body: Option<Json<Value>>,
) -> AppResult<Json<Value>> {
    let raw = body.map(|Json(v)| v).unwrap_or(Value::Null);
    let parsed: ValidateReq = serde_json::from_value(raw.clone()).unwrap_or_default();
    if parsed.check_url {
        crate::auth::ensure_admin(&u)?;
    }
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
        match src
            .rule_search
            .as_ref()
            .and_then(|r| r.book_list.as_deref())
        {
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
