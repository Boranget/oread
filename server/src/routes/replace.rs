use axum::{
    extract::{Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;

use crate::auth::{AuthUser, UserDb};
use crate::entities::ReplaceRule;
use crate::error::AppResult;
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/replace-rules", get(list).post(add))
        .route("/api/replace-rules/delete", delete(remove))
        .route("/api/settings", get(get_settings).post(set_settings))
}

#[derive(Deserialize)]
pub struct IdQ {
    id: i64,
}
#[derive(Deserialize)]
pub struct SettingsQ {
    use_replace_rule: Option<bool>,
    image_style: Option<i64>,
}

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

pub async fn add(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(r): Json<ReplaceRule>,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query(
        "INSERT INTO replace_rules \
         (name, \"group\", pattern, replacement, scope, is_regex, is_enabled, scope_title, scope_content, exclude_scope, \"order\") \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
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
    .execute(&db.0)
    .await?;
    Ok(Json(json!({"ok": true})))
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

pub async fn get_settings(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
) -> AppResult<Json<serde_json::Value>> {
    let use_rr: Option<(String,)> =
        sqlx::query_as("SELECT value FROM user_settings WHERE key='use_replace_rule'")
            .fetch_optional(&db.0)
            .await?;
    let img: Option<(String,)> =
        sqlx::query_as("SELECT value FROM user_settings WHERE key='image_style'")
            .fetch_optional(&db.0)
            .await?;
    Ok(Json(json!({
        "use_replace_rule": use_rr.map(|x| x.0 == "1").unwrap_or(true),
        "image_style": img.and_then(|x| x.0.parse::<i64>().ok()).unwrap_or(0)
    })))
}

pub async fn set_settings(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<SettingsQ>,
) -> AppResult<Json<serde_json::Value>> {
    if let Some(v) = q.use_replace_rule {
        sqlx::query(
            "INSERT INTO user_settings (key,value) VALUES ('use_replace_rule',?1) \
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        )
        .bind(if v { "1" } else { "0" })
        .execute(&db.0)
        .await?;
    }
    if let Some(v) = q.image_style {
        sqlx::query(
            "INSERT INTO user_settings (key,value) VALUES ('image_style',?1) \
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        )
        .bind(v.to_string())
        .execute(&db.0)
        .await?;
    }
    Ok(Json(json!({"ok": true})))
}
