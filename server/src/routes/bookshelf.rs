use axum::{
    extract::{Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;

use crate::auth::{AuthUser, UserDb};
use crate::error::AppResult;
use crate::models::BookshelfItem;
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/bookshelf", get(list).post(add).delete(remove))
        .route("/api/bookshelf/groups", get(list_groups).post(add_group))
        .route(
            "/api/bookshelf/progress",
            get(get_progress).post(set_progress),
        )
}

#[derive(Deserialize)]
pub struct AddQ {
    book_id: String,
    source_id: Option<String>,
    name: Option<String>,
    author: Option<String>,
    cover: Option<String>,
}
#[derive(Deserialize)]
pub struct BookIdQ {
    book_id: String,
}
#[derive(Deserialize)]
pub struct ProgressQ {
    book_id: String,
    chapter_index: Option<i64>,
    position: Option<f64>,
}
#[derive(Deserialize)]
pub struct GroupQ {
    name: String,
}

pub async fn list(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
) -> AppResult<Json<Vec<BookshelfItem>>> {
    let rows = sqlx::query_as::<_, BookshelfItem>(
        "SELECT bs.book_id, COALESCE(bm.name, bs.title) as title, \
         COALESCE(bm.author, bs.author) as author, COALESCE(bm.cover_url, bs.cover) as cover, \
         bs.source_id FROM bookshelf bs LEFT JOIN book_meta bm ON bs.book_id = bm.book_url \
         ORDER BY bs.added_at DESC",
    )
    .fetch_all(&db.0)
    .await?;
    Ok(Json(rows))
}

pub async fn add(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<AddQ>,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query(
        "INSERT INTO bookshelf (book_id, title, author, cover, source_id) \
         VALUES (?1,?2,?3,?4,?5) ON CONFLICT(book_id) DO NOTHING",
    )
    .bind(&q.book_id)
    .bind(&q.name)
    .bind(&q.author)
    .bind(&q.cover)
    .bind(&q.source_id)
    .execute(&db.0)
    .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn remove(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<BookIdQ>,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query("DELETE FROM bookshelf WHERE book_id = ?")
        .bind(&q.book_id)
        .execute(&db.0)
        .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn list_groups(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
) -> AppResult<Json<Vec<(i64, String)>>> {
    let rows = sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM book_groups ORDER BY id")
        .fetch_all(&db.0)
        .await?;
    Ok(Json(rows))
}

pub async fn add_group(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<GroupQ>,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query("INSERT INTO book_groups (name) VALUES (?)")
        .bind(&q.name)
        .execute(&db.0)
        .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn get_progress(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<BookIdQ>,
) -> AppResult<Json<serde_json::Value>> {
    let row: Option<(i64, f64)> =
        sqlx::query_as("SELECT chapter_index, position FROM reading_progress WHERE book_id = ?")
            .bind(&q.book_id)
            .fetch_optional(&db.0)
            .await?;
    match row {
        Some((ci, pos)) => Ok(Json(json!({"chapter_index": ci, "position": pos}))),
        None => Ok(Json(json!({"chapter_index": 0, "position": 0}))),
    }
}

pub async fn set_progress(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<ProgressQ>,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query(
        "INSERT INTO reading_progress (book_id, chapter_index, position) \
         VALUES (?1,?2,?3) ON CONFLICT(book_id) DO UPDATE SET \
         chapter_index=excluded.chapter_index, position=excluded.position, updated_at=datetime('now')",
    )
    .bind(&q.book_id)
    .bind(q.chapter_index.unwrap_or(0))
    .bind(q.position.unwrap_or(0.0))
    .execute(&db.0)
    .await?;
    Ok(Json(json!({"ok": true})))
}
