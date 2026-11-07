//! 替换规则路由的Legado 规则导入（move-only 从 replace.rs 拆出）。

use super::*;

pub(crate) fn has_field(v: &Value, camel: &str, snake: &str) -> bool {
    v.as_object()
        .map(|o| o.contains_key(camel) || o.contains_key(snake))
        .unwrap_or(false)
}

/// 把 Legado 规则对象规整为内部实体：
/// - 去掉本地 id（导入永远新增）；
/// - Legado 缺省的 `isEnabled` 视为 true；`scopeTitle`/`scopeContent` 都缺省时置 true，
///   避免导入后的规则因为两个作用域都为 false 而完全不生效；
/// - `timeoutMillisecond` 缺省/非正数补 1000（Legado 默认值）。
pub(crate) fn normalize_import_rule(item: Value) -> AppResult<ReplaceRule> {
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
pub(crate) fn import_items(body: Value) -> AppResult<Vec<Value>> {
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
            AppError::BadRequest(m) => AppError::BadRequest(format!("第 {} 条规则: {m}", i + 1)),
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
