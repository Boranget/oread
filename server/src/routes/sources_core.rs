//! sources 路由的读取、列表与规范化（move-only 从 sources.rs 拆出）。

use super::*;

#[derive(Deserialize)]
pub struct UrlQ {
    pub(super) url: String,
}

/// 从 `rules` / `content` 两列中取可用 JSON。
/// 历史上 upsert 只写 `rules`，而 `get_one` 读 `content`，导致详情永远为空；
/// 这里两列都尝试（优先 `content`），并配合 shared 迁移 0003 回填，兼容旧数据。
pub(super) fn parse_source_row(
    rules: Option<String>,
    content: Option<String>,
) -> Option<BookSource> {
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
    #[allow(clippy::type_complexity)]
    // 行类型与 SELECT 列一一对应；抽成别名反而在列变更时不易察觉。
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
    #[allow(clippy::type_complexity)] // 行类型与 SELECT 列一一对应；抽成别名反而在列变更时不易察觉。
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
    let quarantined = source_ledger::quarantined_set(&s.shared)
        .await
        .unwrap_or_default();
    Ok(Json(
        sources
            .into_iter()
            .map(|x| {
                let h = health.get(&x.book_source_url);
                // 与聚合搜索同一口径：前端计数/全选必须用它，不能只看 enabled。
                let searchable =
                    crate::routes::books::is_searchable_source(&x, &quarantined, &health);
                let anomaly = crate::source_health::rule_anomaly(&x.book_source_url);
                BookSourceListItem {
                    tier: h
                        .map(|h| h.tier.clone())
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| crate::source_quality::TIER_OBSERVING.to_string()),
                    score: h.map(|h| h.score).unwrap_or(50),
                    last_eval_at: h.and_then(|h| h.last_eval_at),
                    login_required: crate::entities::requires_login(&x),
                    searchable,
                    rule_error_count: anomaly.as_ref().map(|a| a.count).unwrap_or(0),
                    last_rule_error_at: anomaly.as_ref().map(|a| a.last_at),
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
pub(super) fn normalize_source(body: Value) -> AppResult<BookSource> {
    let v = crate::serde_util::camel_to_snake_value(body);
    serde_json::from_value(v).map_err(|e| {
        // 解析细节可能暴露字段名/类型，只进日志；对外给稳定文案。
        tracing::warn!(error = %e, "normalize source failed");
        AppError::BadRequest("书源 JSON 解析失败".into())
    })
}

pub(super) async fn store_source(
    pool: &SqlitePool,
    src: &BookSource,
    actor: &str,
) -> AppResult<()> {
    let content = serde_json::to_string(src)?;
    // rules 与 content 同时写入：旧读取路径（content）与新读取路径（rules）都能拿到数据
    sqlx::query(
        "INSERT INTO book_sources \
         (id, name, url, rules, content, enabled, book_source_type, unsupported_type, updated_by) \
         VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, url=excluded.url, \
         rules=excluded.rules, content=excluded.content, enabled=excluded.enabled, \
         book_source_type=excluded.book_source_type, \
         unsupported_type=excluded.unsupported_type, \
         updated_by=excluded.updated_by, \
         updated_at=datetime('now')",
    )
    .bind(&src.book_source_url)
    .bind(&src.book_source_name)
    .bind(&src.book_source_url)
    .bind(&content)
    .bind(src.enabled)
    .bind(src.book_source_type)
    .bind(src.unsupported_type)
    .bind(actor)
    .execute(pool)
    .await?;
    Ok(())
}

/// 导入策略：仅 0(小说)/2(漫画) 正常落库；1(音频) 及其他类型也落库，
/// 但强制禁用并标记 `unsupported_type`，避免进入搜索/发现。
pub(super) fn prepare_import(src: &mut BookSource) -> bool {
    let supported = crate::entities::is_supported_source_type(src.book_source_type);
    src.unsupported_type = !supported;
    if !supported {
        src.enabled = false;
    }
    supported
}

/// 漫画特征词：用于导入时给出 `type=2` 建议（不做自动改库）。
/// 中文“漫”单独出现会误伤（如“浪漫”），因此必须同时满足内容规则含 `<img` / `@js`。
pub(super) const COMIC_NAME_HINTS: [&str; 13] = [
    "漫画",
    "漫",
    "mkz",
    "manhua",
    "comic",
    "manga",
    "baozimh",
    "kanman",
    "kuaikan",
    "ac.qq",
    "1kkk",
    "哔哩哔哩漫画",
    "腾讯动漫",
];

pub(super) fn comic_name_hit(src: &BookSource) -> Option<String> {
    let name = src.book_source_name.to_lowercase();
    let url = src.book_source_url.to_lowercase();
    COMIC_NAME_HINTS
        .iter()
        .find(|h| name.contains(*h) || url.contains(*h))
        .map(|h| h.to_string())
}

pub(super) fn comic_content_hit(src: &BookSource) -> bool {
    let Some(rc) = src.rule_content.as_ref() else {
        return false;
    };
    let joined = [
        rc.content.as_deref(),
        rc.image_style.as_deref(),
        rc.image_decode.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n");
    joined.contains("<img") || joined.contains("@js")
}

/// 导入时的类型推断：只产出“建议”，绝不修改 `src.book_source_type`。
pub(super) fn suggest_comic_type(src: &BookSource) -> Option<Value> {
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
