//! 替换规则路由的规则增删查与导出（move-only 从 replace.rs 拆出）。

use super::*;

pub async fn list(
    State(_s): State<AppState>,
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
    State(_s): State<AppState>,
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
    State(_s): State<AppState>,
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
pub(crate) fn rule_to_legado_value(r: &ReplaceRule) -> Value {
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
        let r: Option<ReplaceRule> = sqlx::query_as("SELECT * FROM replace_rules WHERE id = ?")
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
