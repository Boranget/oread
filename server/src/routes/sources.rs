use axum::{
    extract::{Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
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
        .route("/api/sources/export", get(export_all))
        .route("/api/sources/detail", get(get_one))
        .route("/api/sources/delete", delete(delete_one))
}

#[derive(Deserialize)]
pub struct UrlQ {
    url: String,
}

/// 加载所有书源（含完整规则对象）。Legado 导出用 camelCase，导入时已统一为 snake_case。
pub async fn load_sources(pool: &SqlitePool) -> AppResult<Vec<BookSource>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT rules FROM book_sources WHERE rules IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    let mut out = Vec::new();
    for (content,) in rows {
        if let Ok(s) = serde_json::from_str::<BookSource>(&content) {
            out.push(s);
        }
    }
    Ok(out)
}

pub async fn list(
    State(s): State<AppState>,
    _u: AuthUser,
) -> AppResult<Json<Vec<BookSourceListItem>>> {
    let rows = sqlx::query_as::<_, BookSourceListItem>(
        "SELECT id, name, enabled FROM book_sources ORDER BY name",
    )
    .fetch_all(&s.shared)
    .await?;
    Ok(Json(rows))
}

/// 把 Legado 导出的 camelCase JSON 转为内部 snake_case 实体
fn normalize_source(body: Value) -> AppResult<BookSource> {
    let v = crate::serde_util::camel_to_snake_value(body);
    serde_json::from_value(v).map_err(|e| AppError::BadRequest(format!("invalid source: {e}")))
}

pub async fn upsert(
    State(s): State<AppState>,
    _u: AuthUser,
    Json(body): Json<Value>,
) -> AppResult<Json<serde_json::Value>> {
    let src = normalize_source(body)?;
    let content = serde_json::to_string(&src)?;
    sqlx::query(
        "INSERT INTO book_sources (id, name, url, rules, enabled, updated_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'admin') \
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, url=excluded.url, rules=excluded.rules, \
         enabled=excluded.enabled, updated_at=datetime('now')",
    )
    .bind(&src.book_source_url)
    .bind(&src.book_source_name)
    .bind(&src.book_source_url)
    .bind(&content)
    .bind(src.enabled)
    .execute(&s.shared)
    .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn import(
    State(s): State<AppState>,
    _u: AuthUser,
    Json(body): Json<Value>,
) -> AppResult<Json<serde_json::Value>> {
    let list: Vec<BookSource> = match body {
        Value::Array(arr) => arr
            .into_iter()
            .map(normalize_source)
            .collect::<AppResult<Vec<_>>>()?,
        single => vec![normalize_source(single)?],
    };
    for src in &list {
        let content = serde_json::to_string(&src)?;
        sqlx::query(
            "INSERT INTO book_sources (id, name, url, rules, enabled, updated_by) \
             VALUES (?1,?2,?3,?4,?5,'admin') \
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, url=excluded.url, rules=excluded.rules, \
             enabled=excluded.enabled, updated_at=datetime('now')",
        )
        .bind(&src.book_source_url)
        .bind(&src.book_source_name)
        .bind(&src.book_source_url)
        .bind(&content)
        .bind(src.enabled)
        .execute(&s.shared)
        .await?;
    }
    Ok(Json(json!({"ok": true, "count": list.len()})))
}

pub async fn export_all(
    State(s): State<AppState>,
    _u: AuthUser,
) -> AppResult<Json<Vec<BookSource>>> {
    let list = load_sources(&s.shared).await?;
    Ok(Json(list))
}

pub async fn get_one(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<UrlQ>,
) -> AppResult<Json<BookSource>> {
    let row: (String,) = sqlx::query_as("SELECT content FROM book_sources WHERE id = ?")
        .bind(&q.url)
        .fetch_optional(&s.shared)
        .await?
        .ok_or_else(|| AppError::NotFound("source not found".into()))?;
    let src = serde_json::from_str(&row.0)?;
    Ok(Json(src))
}

pub async fn delete_one(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<UrlQ>,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query("DELETE FROM book_sources WHERE id = ?")
        .bind(&q.url)
        .execute(&s.shared)
        .await?;
    Ok(Json(json!({"ok": true})))
}
