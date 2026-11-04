use axum::{
    extract::{Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::time::Instant;

use crate::auth::{AuthUser, UserDb};
use crate::entities::ReplaceRule;
use crate::error::{AppError, AppResult};
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/replace-rules", get(list).post(add))
        .route("/api/replace-rules/export", get(export_rules))
        .route("/api/replace-rules/import", post(import_rules))
        .route("/api/replace-rules/preview", post(preview_rules))
        .route("/api/replace-rules/delete", delete(remove))
        .route("/api/settings", get(get_settings).post(set_settings))
}

#[derive(Deserialize)]
pub struct IdQ {
    id: i64,
}

/// 用户设置。字段名与前端 `Settings` 页保持一致；新增字段均为可选，旧前端不传时保持原值。
#[derive(Deserialize, Default)]
pub struct SettingsQ {
    use_replace_rule: Option<bool>,
    image_style: Option<i64>,
    theme: Option<String>,
    font_size: Option<i64>,
    line_height: Option<f64>,
    cache_limit_mb: Option<i64>,
    auto_refresh: Option<bool>,
    read_mode: Option<String>,
    /// 阅读页宽（CSS px）
    page_width: Option<i64>,
    /// 字体族枚举（跨设备同步）
    font_family: Option<String>,
    /// 内置正文净化规则集开关（默认关闭）
    builtin_clean: Option<bool>,
}

/// 设置默认值表（GET 时缺省返回，前端可直接绑定）
const SETTINGS_DEFAULTS: &[(&str, &str)] = &[
    ("use_replace_rule", "1"),
    ("image_style", "0"),
    ("theme", "light"),
    ("font_size", "18"),
    ("line_height", "1.8"),
    ("cache_limit_mb", "50"),
    ("auto_refresh", "1"),
    ("read_mode", "scroll"),
    ("page_width", "800"),
    ("font_family", "serif"),
    ("builtin_clean", "0"),
];

pub async fn list(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
) -> AppResult<Json<Vec<ReplaceRule>>> {
    let rows = sqlx::query_as::<_, ReplaceRule>("SELECT * FROM replace_rules ORDER BY \"order\"")
        .fetch_all(&db.0)
        .await?;
    Ok(Json(rows))
}

/// 新增或更新（带 id 时按 id 更新，避免前端编辑产生重复记录）
pub async fn add(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(r): Json<ReplaceRule>,
) -> AppResult<Json<serde_json::Value>> {
    if let Some(id) = r.id {
        sqlx::query(
            "UPDATE replace_rules SET name=?2, \"group\"=?3, pattern=?4, replacement=?5, scope=?6, \
             is_regex=?7, is_enabled=?8, scope_title=?9, scope_content=?10, exclude_scope=?11, \
             \"order\"=?12, timeout_millisecond=?13 WHERE id=?1",
        )
        .bind(id)
        .bind(&r.name)
        .bind(&r.group)
        .bind(&r.pattern)
        .bind(&r.replacement)
        .bind(&r.scope)
        .bind(r.is_regex)
        .bind(r.is_enabled)
        .bind(r.scope_title)
        .bind(r.scope_content)
        .bind(&r.exclude_scope)
        .bind(r.order)
        .bind(r.timeout_millisecond)
        .execute(&db.0)
        .await?;
        return Ok(Json(json!({"ok": true, "id": id})));
    }
    let id = sqlx::query(
        "INSERT INTO replace_rules \
         (name, \"group\", pattern, replacement, scope, is_regex, is_enabled, scope_title, scope_content, exclude_scope, \"order\", timeout_millisecond) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
    )
    .bind(&r.name)
    .bind(&r.group)
    .bind(&r.pattern)
    .bind(&r.replacement)
    .bind(&r.scope)
    .bind(r.is_regex)
    .bind(r.is_enabled)
    .bind(r.scope_title)
    .bind(r.scope_content)
    .bind(&r.exclude_scope)
    .bind(r.order)
    .bind(r.timeout_millisecond)
    .execute(&db.0)
    .await?
    .last_insert_rowid();
    Ok(Json(json!({"ok": true, "id": id})))
}

pub async fn remove(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<IdQ>,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query("DELETE FROM replace_rules WHERE id = ?")
        .bind(q.id)
        .execute(&db.0)
        .await?;
    Ok(Json(json!({"ok": true})))
}

// ---------------- 规则包导入 / 导出（Legado 原生 ReplaceRule 格式） ----------------

#[derive(Deserialize, Default)]
pub struct ExportQ {
    /// 指定 id 时导出单条（对象），缺省导出全部（数组，Legado 可直接导入）
    pub id: Option<i64>,
}

/// 单条规则转 Legado camelCase 对象；去掉本地自增 id（Legado 导出不含该字段）。
fn rule_to_legado_value(r: &ReplaceRule) -> Value {
    let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
    if let Some(obj) = v.as_object_mut() {
        obj.remove("id");
    }
    crate::serde_util::snake_to_camel_value(v)
}

pub async fn export_rules(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<ExportQ>,
) -> AppResult<Json<Value>> {
    if let Some(id) = q.id {
        let r: Option<ReplaceRule> =
            sqlx::query_as("SELECT * FROM replace_rules WHERE id = ?")
                .bind(id)
                .fetch_optional(&db.0)
                .await?;
        let r = r.ok_or_else(|| AppError::NotFound("replace rule not found".into()))?;
        return Ok(Json(rule_to_legado_value(&r)));
    }
    let rows: Vec<ReplaceRule> =
        sqlx::query_as("SELECT * FROM replace_rules ORDER BY \"order\", id")
            .fetch_all(&db.0)
            .await?;
    Ok(Json(Value::Array(
        rows.iter().map(rule_to_legado_value).collect(),
    )))
}

fn has_field(v: &Value, camel: &str, snake: &str) -> bool {
    v.as_object()
        .map(|o| o.contains_key(camel) || o.contains_key(snake))
        .unwrap_or(false)
}

/// 把 Legado 规则对象规整为内部实体：
/// - 去掉本地 id（导入永远新增）；
/// - Legado 缺省的 `isEnabled` 视为 true；`scopeTitle`/`scopeContent` 都缺省时置 true，
///   避免导入后的规则因为两个作用域都为 false 而完全不生效；
/// - `timeoutMillisecond` 缺省/非正数补 1000（Legado 默认值）。
fn normalize_import_rule(item: Value) -> AppResult<ReplaceRule> {
    let has_enabled = has_field(&item, "isEnabled", "is_enabled");
    let has_regex = has_field(&item, "isRegex", "is_regex");
    let has_title = has_field(&item, "scopeTitle", "scope_title");
    let has_content = has_field(&item, "scopeContent", "scope_content");
    let snake = crate::serde_util::camel_to_snake_value(item);
    let mut r: ReplaceRule = serde_json::from_value(snake)
        .map_err(|e| AppError::BadRequest(format!("invalid replace rule: {e}")))?;
    r.id = None;
    if !has_enabled {
        r.is_enabled = true;
    }
    if !has_regex {
        r.is_regex = false;
    }
    if !has_title && !has_content {
        r.scope_title = true;
        r.scope_content = true;
    }
    if r.timeout_millisecond <= 0 {
        r.timeout_millisecond = 1000;
    }
    Ok(r)
}

/// 兼容三种入参：裸数组、`{name,rules:[...]}` 包装、单条对象。
fn import_items(body: Value) -> AppResult<Vec<Value>> {
    match body {
        Value::Array(arr) => Ok(arr),
        Value::Object(mut obj) => {
            if let Some(rules) = obj.remove("rules") {
                return Ok(match rules {
                    Value::Array(a) => a,
                    Value::Null => vec![],
                    one => vec![one],
                });
            }
            Ok(vec![Value::Object(obj)])
        }
        Value::Null => Err(AppError::BadRequest("规则包不能为空".into())),
        one => Ok(vec![one]),
    }
}

pub async fn import_rules(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(body): Json<Value>,
) -> AppResult<Json<Value>> {
    let items = import_items(body)?;
    let mut rules: Vec<ReplaceRule> = Vec::new();
    let mut skipped = 0usize;
    for (i, item) in items.into_iter().enumerate() {
        let r = normalize_import_rule(item).map_err(|e| match e {
            AppError::BadRequest(m) => {
                AppError::BadRequest(format!("第 {} 条规则: {m}", i + 1))
            }
            other => other,
        })?;
        if r.pattern.trim().is_empty() {
            skipped += 1;
            continue;
        }
        rules.push(r);
    }
    for r in &rules {
        sqlx::query(
            "INSERT INTO replace_rules \
             (name, \"group\", pattern, replacement, scope, is_regex, is_enabled, scope_title, scope_content, exclude_scope, \"order\", timeout_millisecond) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        )
        .bind(&r.name)
        .bind(&r.group)
        .bind(&r.pattern)
        .bind(&r.replacement)
        .bind(&r.scope)
        .bind(r.is_regex)
        .bind(r.is_enabled)
        .bind(r.scope_title)
        .bind(r.scope_content)
        .bind(&r.exclude_scope)
        .bind(r.order)
        .bind(r.timeout_millisecond)
        .execute(&db.0)
        .await?;
    }
    Ok(Json(json!({
        "ok": true,
        "inserted": rules.len(),
        "skipped": skipped,
    })))
}

// ---------------- 规则试跑预览 ----------------

/// 预览请求：`url` 与 `input` 二选一（同时给出时优先 url）。
/// `rules`/`rule` 支持 Legado camelCase 或内部 snake_case。
#[derive(Deserialize, Default)]
pub struct PreviewReq {
    #[serde(default)]
    pub rules: Option<Value>,
    #[serde(default)]
    pub rule: Option<Value>,
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Option<Value>,
    #[serde(default)]
    pub book: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub for_title: bool,
}

/// 与抓取层一致的预算：响应/输入 5 MiB、单次预览 15 s。
const PREVIEW_MAX_BYTES: usize = 5 * 1024 * 1024;
const PREVIEW_TIMEOUT_MS: u64 = 15_000;
/// 单个中间值最多返回 2 万字符，最终值最多 20 万字符，超出截断并置 truncated 标记。
const PREVIEW_STEP_OUTPUT_MAX: usize = 20_000;
const PREVIEW_FINAL_MAX: usize = 200_000;

#[derive(Debug, Clone, Serialize)]
pub struct PreviewStep {
    pub index: usize,
    pub name: Option<String>,
    pub pattern: String,
    pub replacement: String,
    pub is_regex: bool,
    pub applied: bool,
    pub skipped_reason: Option<String>,
    pub hits: usize,
    pub output: String,
    pub output_truncated: bool,
}

/// 按 UTF-8 字符边界安全截断。
fn clip_output(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// 依次应用规则并记录每一步的中间值（与 `apply_rules` 的生效条件保持一致）。
pub(crate) fn preview_apply(
    rules: &[ReplaceRule],
    input: &str,
    book: &str,
    origin: &str,
    for_title: bool,
) -> (String, Vec<PreviewStep>) {
    let mut out = input.to_string();
    let mut steps: Vec<PreviewStep> = Vec::with_capacity(rules.len());
    for (index, r) in rules.iter().enumerate() {
        let mut step = PreviewStep {
            index,
            name: r.name.clone(),
            pattern: r.pattern.clone(),
            replacement: r.replacement.clone(),
            is_regex: r.is_regex,
            applied: false,
            skipped_reason: None,
            hits: 0,
            output: String::new(),
            output_truncated: false,
        };
        if !r.is_enabled {
            step.skipped_reason = Some("disabled".into());
            step.output = out.clone();
            steps.push(step);
            continue;
        }
        if for_title && !r.scope_title || !for_title && !r.scope_content {
            step.skipped_reason = Some("scope_flag".into());
            step.output = out.clone();
            steps.push(step);
            continue;
        }
        if r.pattern.trim().is_empty() {
            step.skipped_reason = Some("empty_pattern".into());
            step.output = out.clone();
            steps.push(step);
            continue;
        }
        if let Some(scope) = r.scope.as_deref() {
            if !scope.trim().is_empty() && !scope_hit(&r.scope, book, origin) {
                step.skipped_reason = Some("scope_mismatch".into());
                step.output = out.clone();
                steps.push(step);
                continue;
            }
        }
        if scope_hit(&r.exclude_scope, book, origin) {
            step.skipped_reason = Some("excluded".into());
            step.output = out.clone();
            steps.push(step);
            continue;
        }
        if r.is_regex {
            if r.pattern.len() > 512 {
                step.skipped_reason = Some("pattern_too_long".into());
                step.output = out.clone();
                steps.push(step);
                continue;
            }
            match regex::Regex::new(&r.pattern) {
                Ok(re) => {
                    step.hits = re.find_iter(&out).count();
                    out = re.replace_all(&out, r.replacement.as_str()).into_owned();
                    step.applied = true;
                }
                Err(e) => {
                    step.skipped_reason = Some(format!("invalid_regex: {e}"));
                }
            }
        } else {
            step.hits = out.matches(&r.pattern).count();
            out = out.replace(&r.pattern, &r.replacement);
            step.applied = true;
        }
        let (clipped, truncated) = clip_output(&out, PREVIEW_STEP_OUTPUT_MAX);
        step.output = clipped;
        step.output_truncated = truncated;
        steps.push(step);
    }
    (out, steps)
}

fn preview_rules_from_req(q: &PreviewReq) -> AppResult<Vec<ReplaceRule>> {
    let raw = if let Some(rules) = &q.rules {
        rules.clone()
    } else if let Some(rule) = &q.rule {
        rule.clone()
    } else {
        return Err(AppError::BadRequest(
            "rules 或 rule 至少提供一个".into(),
        ));
    };
    let items = import_items(raw)?;
    let mut out = Vec::new();
    for item in items {
        out.push(normalize_import_rule(item)?);
    }
    Ok(out)
}

pub async fn preview_rules(
    State(s): State<AppState>,
    _u: AuthUser,
    Json(q): Json<PreviewReq>,
) -> AppResult<Json<Value>> {
    let rules = preview_rules_from_req(&q)?;
    let (text, fetched) = match (&q.url, &q.input) {
        (Some(url), _) if !url.trim().is_empty() => {
            let url = url.trim();
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(AppError::BadRequest("url 必须是 http(s) URL".into()));
            }
            let headers = crate::req::parse_headers(&q.headers.as_ref().map(|v| v.to_string()));
            let started = Instant::now();
            // SSRF 防护：默认拒绝回环/私网/链路本地，逐跳校验重定向
            let body =
                crate::req::fetch_checked(url, "GET", &headers, None, s.cfg.preview_allow_private)
                    .await
                    // 抓取层已区分超时/连接失败/非法地址，这里透传为 400，
                    // 而不是 500 的 “internal: request failed: error sending request”
                    .map_err(|e| AppError::BadRequest(format!("预览抓取失败：{e}")))?;
            if started.elapsed().as_millis() as u64 > PREVIEW_TIMEOUT_MS {
                return Err(AppError::BadRequest("预览超时：超过 15 s 预算".into()));
            }
            if body.len() > PREVIEW_MAX_BYTES {
                return Err(AppError::BadRequest("响应超过 5 MiB 预算".into()));
            }
            (body, true)
        }
        (_, Some(input)) => (input.clone(), false),
        _ => {
            return Err(AppError::BadRequest(
                "input 或 url 至少提供一个".into(),
            ))
        }
    };
    if text.len() > PREVIEW_MAX_BYTES {
        return Err(AppError::BadRequest("input 超过 5 MiB 预算".into()));
    }
    let book = q.book.clone().unwrap_or_default();
    let origin = q.origin.clone().unwrap_or_default();
    let (final_out, steps) = preview_apply(&rules, &text, &book, &origin, q.for_title);
    let (final_clipped, final_truncated) = clip_output(&final_out, PREVIEW_FINAL_MAX);
    Ok(Json(json!({
        "ok": true,
        "fetched": fetched,
        "input_length": text.len(),
        "final": final_clipped,
        "output_length": final_out.len(),
        "final_truncated": final_truncated,
        "steps": steps,
    })))
}

pub async fn get_settings(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
) -> AppResult<Json<serde_json::Value>> {
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT key, value FROM user_settings")
            .fetch_all(&db.0)
            .await?;
    let mut map: std::collections::HashMap<String, String> = rows
        .into_iter()
        .map(|(k, v)| (k, v.unwrap_or_default()))
        .collect();
    for (k, v) in SETTINGS_DEFAULTS {
        map.entry(k.to_string()).or_insert_with(|| v.to_string());
    }
    let get = |k: &str| map.get(k).cloned().unwrap_or_default();
    let b = |k: &str| get(k) == "1" || get(k) == "true";
    Ok(Json(json!({
        "use_replace_rule": b("use_replace_rule"),
        "image_style": get("image_style").parse::<i64>().unwrap_or(0),
        "theme": get("theme"),
        "font_size": get("font_size").parse::<i64>().unwrap_or(18),
        "line_height": get("line_height").parse::<f64>().unwrap_or(1.8),
        "cache_limit_mb": get("cache_limit_mb").parse::<i64>().unwrap_or(50),
        "auto_refresh": b("auto_refresh"),
        "read_mode": get("read_mode"),
        "page_width": get("page_width").parse::<i64>().unwrap_or(800),
        "font_family": get("font_family"),
        "builtin_clean": b("builtin_clean"),
    })))
}

async fn put_setting(db: &SqlitePool, key: &str, value: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO user_settings (key,value) VALUES (?1,?2) \
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(db)
    .await?;
    Ok(())
}

/// 服务端校验：枚举白名单 + 数值范围，非法值直接 400（验收 P1）。
/// 范围与前端 `Settings` 页控件保持一致（theme 四档、字号 12–30、行距 1.2–2.6 等）。
fn validate_settings(q: &SettingsQ) -> AppResult<()> {
    if let Some(v) = &q.theme {
        if !matches!(v.as_str(), "light" | "dark" | "sepia" | "eink") {
            return Err(AppError::BadRequest(
                "theme 必须是 light/dark/sepia/eink".into(),
            ));
        }
    }
    if let Some(v) = q.font_size {
        if !(12..=30).contains(&v) {
            return Err(AppError::BadRequest("font_size 必须在 12–30 之间".into()));
        }
    }
    if let Some(v) = q.line_height {
        if !v.is_finite() || !(1.2..=2.6).contains(&v) {
            return Err(AppError::BadRequest("line_height 必须在 1.2–2.6 之间".into()));
        }
    }
    if let Some(v) = q.cache_limit_mb {
        if !(10..=1000).contains(&v) {
            return Err(AppError::BadRequest(
                "cache_limit_mb 必须在 10–1000 之间".into(),
            ));
        }
    }
    if let Some(v) = q.image_style {
        if !(0..=3).contains(&v) {
            return Err(AppError::BadRequest("image_style 必须是 0–3".into()));
        }
    }
    if let Some(v) = &q.read_mode {
        if !matches!(v.as_str(), "scroll" | "paged" | "fade") {
            return Err(AppError::BadRequest(
                "read_mode 必须是 scroll/paged/fade".into(),
            ));
        }
    }
    if let Some(v) = q.page_width {
        if !(320..=2000).contains(&v) {
            return Err(AppError::BadRequest(
                "page_width 必须在 320–2000 之间".into(),
            ));
        }
    }
    if let Some(v) = &q.font_family {
        if !matches!(
            v.as_str(),
            "serif" | "sans" | "sans-serif" | "kai" | "kaiti" | "song" | "songti" | "hei"
                | "heiti" | "mono" | "system"
        ) {
            return Err(AppError::BadRequest(
                "font_family 必须是 serif/sans/kai/song/hei/mono/system 等枚举值".into(),
            ));
        }
    }
    Ok(())
}

pub async fn set_settings(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<SettingsQ>,
) -> AppResult<Json<serde_json::Value>> {
    validate_settings(&q)?;
    if let Some(v) = q.use_replace_rule {
        put_setting(&db.0, "use_replace_rule", if v { "1" } else { "0" }).await?;
    }
    if let Some(v) = q.image_style {
        put_setting(&db.0, "image_style", &v.to_string()).await?;
    }
    if let Some(v) = &q.theme {
        put_setting(&db.0, "theme", v).await?;
    }
    if let Some(v) = q.font_size {
        put_setting(&db.0, "font_size", &v.to_string()).await?;
    }
    if let Some(v) = q.line_height {
        put_setting(&db.0, "line_height", &v.to_string()).await?;
    }
    if let Some(v) = q.cache_limit_mb {
        put_setting(&db.0, "cache_limit_mb", &v.to_string()).await?;
    }
    if let Some(v) = q.auto_refresh {
        put_setting(&db.0, "auto_refresh", if v { "1" } else { "0" }).await?;
    }
    if let Some(v) = &q.read_mode {
        put_setting(&db.0, "read_mode", v).await?;
    }
    if let Some(v) = q.page_width {
        put_setting(&db.0, "page_width", &v.to_string()).await?;
    }
    if let Some(v) = &q.font_family {
        put_setting(&db.0, "font_family", v).await?;
    }
    if let Some(v) = q.builtin_clean {
        put_setting(&db.0, "builtin_clean", if v { "1" } else { "0" }).await?;
    }
    Ok(Json(json!({"ok": true})))
}

// ---------------- 替换规则在正文输出时生效 ----------------

/// 读取用户是否开启替换规则（缺省开启）
pub async fn replace_rule_enabled(db: &SqlitePool) -> bool {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM user_settings WHERE key='use_replace_rule'")
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    row.map(|(v,)| v != "0" && v != "false").unwrap_or(true)
}

/// 读取用户设置的正文缓存上限（MB）；未设置/非法值返回 None（表示沿用全局配置）。
pub async fn user_cache_limit_mb(db: &SqlitePool) -> Option<i64> {
    let v: Option<String> =
        sqlx::query_scalar("SELECT value FROM user_settings WHERE key='cache_limit_mb'")
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    v.and_then(|x| x.trim().parse::<i64>().ok())
        .filter(|m| *m > 0)
}

/// 读取用户图片样式：0 原始 / 1 宽度自适应 / 2 居中 / 3 隐藏。
pub async fn user_image_style(db: &SqlitePool) -> i64 {
    let v: Option<String> =
        sqlx::query_scalar("SELECT value FROM user_settings WHERE key='image_style'")
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    v.and_then(|x| x.trim().parse::<i64>().ok()).unwrap_or(0)
}

/// 按图片样式处理正文 HTML（轻量正则改写，不引入 HTML 解析器）。
pub fn apply_image_style(input: &str, style: i64) -> String {
    use once_cell::sync::OnceCell;
    static IMG_RE: OnceCell<regex::Regex> = OnceCell::new();
    let re = IMG_RE.get_or_init(|| regex::Regex::new(r"(?is)<img\b[^>]*>").unwrap());
    match style {
        3 => re.replace_all(input, "").to_string(),
        1 => re
            .replace_all(input, |c: &regex::Captures| merge_img_style(&c[0], "max-width:100%;height:auto;"))
            .to_string(),
        2 => re
            .replace_all(input, |c: &regex::Captures| merge_img_style(&c[0], "display:block;margin:0 auto;"))
            .to_string(),
        _ => input.to_string(),
    }
}

fn merge_img_style(tag: &str, add: &str) -> String {
    // 已有 style 属性则追加，否则插入一个新的 style 属性
    if let Some(pos) = tag.to_lowercase().find("style=\"") {
        let at = pos + "style=\"".len();
        let mut out = String::with_capacity(tag.len() + add.len());
        out.push_str(&tag[..at]);
        out.push_str(add);
        out.push_str(&tag[at..]);
        return out;
    }
    if let Some(end) = tag.rfind('>') {
        let mut out = String::with_capacity(tag.len() + add.len() + 8);
        out.push_str(&tag[..end]);
        out.push_str(&format!(" style=\"{add}\""));
        out.push_str(&tag[end..]);
        return out;
    }
    tag.to_string()
}

/// Legado 作用域匹配：`scope` 每行一个书名或书源域名，命中任一即生效；
/// `excludeScope` 命中则跳过。空 scope 表示全体书籍。
fn scope_hit(scope: &Option<String>, book: &str, origin: &str) -> bool {
    let raw = match scope.as_deref() {
        Some(s) => s.trim(),
        None => return false,
    };
    if raw.is_empty() {
        return false;
    }
    raw.split(['\n', ';', '|'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .any(|s| book.contains(s) || origin.contains(s))
}

/// 按 `order` 顺序执行已启用规则。`for_title` 决定是否应用 scope_title / scope_content。
pub fn apply_rules(
    rules: &[ReplaceRule],
    book: &str,
    origin: &str,
    input: &str,
    for_title: bool,
) -> String {
    let mut out = input.to_string();
    for r in rules {
        if !r.is_enabled {
            continue;
        }
        if for_title && !r.scope_title || !for_title && !r.scope_content {
            continue;
        }
        if r.pattern.trim().is_empty() {
            continue;
        }
        if let Some(scope) = r.scope.as_deref() {
            if !scope.trim().is_empty() && !scope_hit(&r.scope, book, origin) {
                continue;
            }
        }
        if scope_hit(&r.exclude_scope, book, origin) {
            continue;
        }
        if r.is_regex {
            // 正则回溯风险：限制模式长度并要求可编译；编译失败跳过该条规则
            if r.pattern.len() > 512 {
                continue;
            }
            if let Ok(re) = regex::Regex::new(&r.pattern) {
                out = re.replace_all(&out, r.replacement.as_str()).into_owned();
            }
        } else {
            out = out.replace(&r.pattern, &r.replacement);
        }
    }
    out
}

/// 内置净化规则集（默认关闭；`builtin_clean=1` 时与用户规则走同一条 apply_rules 链路）。
///
/// 原则：**只按整行处理，且图案必须落在行首/行尾**，宁可漏删也不误删正文。
/// “武动乾坤”这类插入句中广告保留（实测错层小说网出现在“月如银盘，漫天繁星…”句中，
/// 若按全文匹配会连正文一起删掉）。
fn builtin_rules() -> Vec<ReplaceRule> {
    let mk = |name: &str, pattern: &str| ReplaceRule {
        id: None,
        name: Some(name.to_string()),
        group: Some("内置净化".to_string()),
        pattern: pattern.to_string(),
        replacement: String::new(),
        scope: None,
        scope_title: false,
        scope_content: true,
        exclude_scope: None,
        is_enabled: true,
        is_regex: true,
        timeout_millisecond: 1000,
        order: 0,
    };
    vec![
        // 一秒记住【xxx】www.xxx.com，更新快，无弹窗！
        mk(
            "去“一秒记住/本站首发”提示",
            r"(?m)^[^\n]*(?:一秒记住|记住本站|请记住本书首发域名|本书首发来自)[^\n]*$",
        ),
        // 本章未完请点击下一页 / 请点击下一页继续阅读
        mk(
            "去“下一页/本章未完”引导",
            r"(?m)^[^\n]*(?:本章未完|点击下一页|下一页继续阅读|请点击下一页|翻页继续阅读)[^\n]*$",
        ),
        // 独立成行的网址/域名广告（含 www 或裸域名）
        mk(
            "去网址/域名广告行",
            r"(?m)^[^\n]*(?:https?://|www\.[A-Za-z0-9.-]+\.[A-Za-z]{2,10}|(?:[A-Za-z0-9-]+\.)+(?:com|net|org|cc|info|la|xyz|top|cn|tw|me|io|vip))(?:/\S*)?[^\n]*$",
        ),
        // 求票/举报/收藏等短推广句（整行不超过 80 字）
        mk(
            "去推广/求票短句",
            r"(?m)^[^\n]{0,40}(?:天才一秒记住|手机用户请浏览|章节错误[，,]?\s*点此举报|求推荐票|求月票|加入书签|投推荐票|无弹窗|无广告|请收藏本站)[^\n]{0,40}$",
        ),
        // 独立成行的书名推广（仅当整行只有书名+标点）
        mk(
            "去独立书名推广行",
            r"(?m)^\s*(?:<[^>]+>|\s)*[（(【\[]?(?:武动乾坤|斗破苍穹)[）)】\]]?[。！!．.]?(?:<[^>]+>|\s)*$",
        ),
    ]
}

/// 内置净化开关（缺省关闭）
async fn builtin_clean_enabled(db: &SqlitePool) -> bool {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM user_settings WHERE key='builtin_clean'")
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    row.map(|(v,)| v == "1" || v == "true").unwrap_or(false)
}

/// 正文输出前的统一净化入口：读取用户规则并应用到标题/正文。
pub async fn purify(
    db: &SqlitePool,
    book: &str,
    origin: &str,
    title: Option<String>,
    content: Option<String>,
) -> (Option<String>, Option<String>) {
    if !replace_rule_enabled(db).await {
        return (title, content);
    }
    let rules = active_rules(db).await;
    if rules.is_empty() {
        return (title, content);
    }
    let t = title.map(|t| apply_rules(&rules, book, origin, &t, true));
    let c = content.map(|c| apply_rules(&rules, book, origin, &c, false));
    (t, c)
}

/// 当前生效的全部替换规则（内置净化开关打开时排在最前，用户规则随后）。
/// 目录与正文共用同一份加载逻辑，避免两处行为分叉；调用方自行决定是否启用。
pub async fn active_rules(db: &SqlitePool) -> Vec<ReplaceRule> {
    let mut rules: Vec<ReplaceRule> =
        match sqlx::query_as("SELECT * FROM replace_rules WHERE is_enabled=1 ORDER BY \"order\", id")
            .fetch_all(db)
            .await
        {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
    if builtin_clean_enabled(db).await {
        let mut all = builtin_rules();
        all.append(&mut rules);
        rules = all;
    }
    rules
}

/// 目录标题净化：规则只加载一次，按 `scope_title` 逐章应用。
/// 只影响返回给用户的标题，`book_chapters` 缓存保持原文，开关可随时切换。
pub async fn purify_titles(
    db: &SqlitePool,
    book: &str,
    origin: &str,
    titles: &mut [String],
) {
    let rules = active_rules(db).await;
    if rules.is_empty() {
        return;
    }
    for t in titles.iter_mut() {
        *t = apply_rules(&rules, book, origin, t, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(name: &str, pat: &str, repl: &str, regex: bool) -> ReplaceRule {
        ReplaceRule {
            id: None,
            name: Some(name.into()),
            group: None,
            pattern: pat.into(),
            replacement: repl.into(),
            scope: None,
            scope_title: true,
            scope_content: true,
            exclude_scope: None,
            is_enabled: true,
            is_regex: regex,
            timeout_millisecond: 1000,
            order: 0,
        }
    }

    // 内置净化：整行广告删除，句中插句（“武动乾坤”）必须保留，不能误删正文
    #[test]
    fn builtin_clean_removes_ad_lines_but_keeps_body() {
        let text = "\n\t\t一秒记住【顶点小说】 www.dingdian888.com，更新快，无弹窗！\n\t\t痛！好痛！\n本章未完请点击下一页继续阅读\n月如银盘，漫天繁星武动乾坤。\n武动乾坤\n";
        let out = apply_rules(&builtin_rules(), "", "", text, false);
        assert!(!out.contains("一秒记住"), "站点提示行应删除: {out}");
        assert!(!out.contains("dingdian888"), "域名广告应删除: {out}");
        assert!(!out.contains("本章未完"), "翻页引导应删除: {out}");
        assert!(
            out.contains("月如银盘，漫天繁星武动乾坤。"),
            "句中出现书名是正文，不能误删: {out}"
        );
        let lines: Vec<&str> = out
            .lines()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            lines,
            vec!["痛！好痛！", "月如银盘，漫天繁星武动乾坤。"],
            "仅独立成行的书名推广应删除"
        );
    }

    // 内置规则必须全部是合法正则，且默认关闭（开关在 /api/settings）
    #[test]
    fn builtin_clean_rules_compile_and_default_off() {
        let rules = builtin_rules();
        assert!(rules.len() >= 5);
        for r in &rules {
            assert!(
                regex::Regex::new(&r.pattern).is_ok(),
                "非法正则: {}",
                r.pattern
            );
            assert!(r.is_enabled && r.scope_content && !r.scope_title);
        }
        assert!(SETTINGS_DEFAULTS
            .iter()
            .any(|(k, v)| *k == "builtin_clean" && *v == "0"));
    }

    // 导出为 Legado 原生 camelCase，且不带本地自增 id
    #[test]
    fn export_uses_legado_camel_case_and_drops_id() {
        let mut r = mk("去广告", "广告", "", false);
        r.id = Some(7);
        r.scope = Some("某书".into());
        let v = rule_to_legado_value(&r);
        assert!(v.get("id").is_none());
        assert_eq!(v["name"], "去广告");
        assert_eq!(v["isRegex"], false);
        assert_eq!(v["isEnabled"], true);
        assert_eq!(v["scopeTitle"], true);
        assert_eq!(v["scopeContent"], true);
        assert_eq!(v["timeoutMillisecond"], 1000);
        assert_eq!(v["scope"], "某书");
    }

    // 导入 Legado 字段：缺省 isEnabled=true、作用域两者都缺省时置 true、timeout 补 1000
    #[test]
    fn import_applies_legado_defaults() {
        let r = normalize_import_rule(serde_json::json!({
            "name": "去广告",
            "pattern": "广告",
            "replacement": "",
            "isRegex": false,
            "scope": "某书"
        }))
        .unwrap();
        assert_eq!(r.id, None);
        assert!(r.is_enabled);
        assert!(r.scope_title && r.scope_content);
        assert!(!r.is_regex);
        assert_eq!(r.timeout_millisecond, 1000);
        assert_eq!(r.scope.as_deref(), Some("某书"));
    }

    // 显式给出的作用域标记不会被默认值覆盖
    #[test]
    fn import_respects_explicit_scope_flags() {
        let r = normalize_import_rule(serde_json::json!({
            "pattern": "x",
            "replacement": "y",
            "scopeTitle": false,
            "scopeContent": true
        }))
        .unwrap();
        assert!(!r.scope_title);
        assert!(r.scope_content);
    }

    // 包装对象 `{name, rules:[...]}` 与裸数组两种规则包都能解析
    #[test]
    fn import_accepts_wrapper_and_array() {
        let wrapped = serde_json::json!({
            "name": "我的规则包",
            "rules": [{"pattern": "a", "replacement": "b"}]
        });
        assert_eq!(import_items(wrapped).unwrap().len(), 1);
        let arr = serde_json::json!([
            {"pattern": "a", "replacement": "b"},
            {"pattern": "c", "replacement": "d"}
        ]);
        assert_eq!(import_items(arr).unwrap().len(), 2);
    }

    // 预览要返回每一步的中间值，而不只是最终值
    #[test]
    fn preview_records_intermediate_values() {
        let rules = vec![
            mk("章号", r"第(\d+)章", "Chapter$1", true),
            mk("去广告", "广告", "", false),
        ];
        let (out, steps) = preview_apply(&rules, "第12章 广告内容", "", "", false);
        assert_eq!(out, "Chapter12 内容");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].output, "Chapter12 广告内容");
        assert_eq!(steps[0].hits, 1);
        assert_eq!(steps[0].index, 0);
        assert!(steps[0].applied);
        assert_eq!(steps[1].output, out);
        assert_eq!(steps[1].hits, 1);
    }

    // 未启用/作用域不匹配的规则要记录跳过原因，不参与输出
    #[test]
    fn preview_reports_skipped_rules() {
        let mut disabled = mk("关", "x", "y", false);
        disabled.is_enabled = false;
        let mut wrong_scope = mk("限定", "x", "y", false);
        wrong_scope.scope = Some("别的书".into());
        let (out, steps) =
            preview_apply(&[disabled, wrong_scope], "x", "某书", "site", false);
        assert_eq!(out, "x");
        assert_eq!(steps[0].skipped_reason.as_deref(), Some("disabled"));
        assert_eq!(steps[1].skipped_reason.as_deref(), Some("scope_mismatch"));
        assert!(!steps[1].applied);
    }

    // 超长中间值按 UTF-8 边界截断，并标记 truncated
    #[test]
    fn preview_truncates_long_output() {
        let long = "中".repeat(PREVIEW_STEP_OUTPUT_MAX);
        let (_, steps) = preview_apply(&[mk("noop", "zzz", "", false)], &long, "", "", false);
        assert!(steps[0].output_truncated);
        assert!(steps[0].output.len() <= PREVIEW_STEP_OUTPUT_MAX);
    }

    // 新增的 page_width/font_family：旧客户端不传必须放行（保持向后兼容）
    #[test]
    fn settings_new_fields_are_optional_for_old_clients() {
        assert!(validate_settings(&SettingsQ::default()).is_ok());
        let legacy: SettingsQ = serde_json::from_value(serde_json::json!({
            "theme": "dark",
            "font_size": 16,
            "read_mode": "scroll"
        }))
        .unwrap();
        assert_eq!(legacy.page_width, None);
        assert_eq!(legacy.font_family, None);
        assert!(validate_settings(&legacy).is_ok());
    }

    // page_width 范围 320–2000，font_family 走枚举白名单；非法值 400
    #[test]
    fn settings_page_width_and_font_family_are_validated() {
        for bad in [319_i64, 2001] {
            let q = SettingsQ {
                page_width: Some(bad),
                ..Default::default()
            };
            assert!(
                validate_settings(&q).is_err(),
                "page_width={bad} 应被拒绝"
            );
        }
        for ok in [320_i64, 800, 2000] {
            let q = SettingsQ {
                page_width: Some(ok),
                ..Default::default()
            };
            assert!(validate_settings(&q).is_ok(), "page_width={ok} 应通过");
        }
        for bad in ["comic", "Arial", ""] {
            let q = SettingsQ {
                font_family: Some(bad.into()),
                ..Default::default()
            };
            assert!(
                validate_settings(&q).is_err(),
                "font_family={bad:?} 应被拒绝"
            );
        }
        for ok in ["serif", "sans", "kaiti", "songti", "heiti", "mono", "system"] {
            let q = SettingsQ {
                font_family: Some(ok.into()),
                ..Default::default()
            };
            assert!(validate_settings(&q).is_ok(), "font_family={ok} 应通过");
        }
    }
}
