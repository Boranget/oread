//! 书签 CRUD：按用户库隔离，字段覆盖书籍/章节/章内位置/标题/备注/创建时间。

use axum::{
    extract::{Query, State},
    routing::{delete, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;

use crate::auth::{AuthUser, UserDb};
use crate::error::{AppError, AppResult};
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/bookmarks", get(list).post(add))
        .route("/api/bookmarks/delete", delete(remove))
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "snake_case")]
pub struct Bookmark {
    pub id: i64,
    pub book_id: String,
    pub chapter_index: i64,
    pub position: f64,
    pub chapter_title: Option<String>,
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Deserialize, Default)]
pub struct ListQ {
    pub book_id: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct NewBookmark {
    #[serde(default)]
    pub book_id: String,
    #[serde(default)]
    pub chapter_index: i64,
    #[serde(default)]
    pub position: f64,
    #[serde(default)]
    pub chapter_title: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct IdQ {
    pub id: i64,
}

/// 服务端校验：书籍非空、章节下标非负、`position` 必须是 0..1 的有限数。
/// 越界返回明确 400，不做静默归一化（与 `/api/bookshelf/progress` 口径一致）。
pub(crate) fn validate_new(q: &NewBookmark) -> AppResult<()> {
    if q.book_id.trim().is_empty() {
        return Err(AppError::BadRequest("book_id 不能为空".into()));
    }
    if q.chapter_index < 0 {
        return Err(AppError::BadRequest("chapter_index 必须大于等于 0".into()));
    }
    if !q.position.is_finite() || !(0.0..=1.0).contains(&q.position) {
        return Err(AppError::BadRequest(
            "position 必须是 0..1 的章内比例".into(),
        ));
    }
    if q.note.as_deref().map(|s| s.chars().count()).unwrap_or(0) > 2000 {
        return Err(AppError::BadRequest("note 最多 2000 字".into()));
    }
    Ok(())
}

pub(crate) async fn insert_bookmark(pool: &SqlitePool, q: &NewBookmark) -> sqlx::Result<Bookmark> {
    sqlx::query_as(
        "INSERT INTO bookmarks (book_id, chapter_index, position, chapter_title, note) \
         VALUES (?1,?2,?3,?4,?5) \
         RETURNING id, book_id, chapter_index, position, chapter_title, note, created_at",
    )
    .bind(q.book_id.trim())
    .bind(q.chapter_index)
    .bind(q.position)
    .bind(&q.chapter_title)
    .bind(&q.note)
    .fetch_one(pool)
    .await
}

pub(crate) async fn list_bookmarks(
    pool: &SqlitePool,
    book_id: Option<&str>,
) -> sqlx::Result<Vec<Bookmark>> {
    match book_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => {
            sqlx::query_as(
                "SELECT id, book_id, chapter_index, position, chapter_title, note, created_at \
                 FROM bookmarks WHERE book_id = ?1 ORDER BY created_at DESC, id DESC",
            )
            .bind(id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT id, book_id, chapter_index, position, chapter_title, note, created_at \
                 FROM bookmarks ORDER BY created_at DESC, id DESC",
            )
            .fetch_all(pool)
            .await
        }
    }
}

async fn list(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<ListQ>,
) -> AppResult<Json<Vec<Bookmark>>> {
    Ok(Json(list_bookmarks(&db.0, q.book_id.as_deref()).await?))
}

async fn add(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<NewBookmark>,
) -> AppResult<Json<Bookmark>> {
    validate_new(&q)?;
    Ok(Json(insert_bookmark(&db.0, &q).await?))
}

async fn remove(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<IdQ>,
) -> AppResult<Json<serde_json::Value>> {
    if q.id <= 0 {
        return Err(AppError::BadRequest("id 必须大于 0".into()));
    }
    let deleted = sqlx::query("DELETE FROM bookmarks WHERE id = ?1")
        .bind(q.id)
        .execute(&db.0)
        .await?
        .rows_affected();
    Ok(Json(json!({"ok": true, "deleted": deleted})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    fn valid() -> NewBookmark {
        NewBookmark {
            book_id: "https://x.com/book/1".into(),
            chapter_index: 3,
            position: 0.5,
            chapter_title: Some("第三章".into()),
            note: Some("标记".into()),
        }
    }

    // position 越界必须 400（含 NaN/无穷），边界 0 与 1 必须放行
    #[test]
    fn position_out_of_range_is_rejected() {
        for bad in [-0.1, 1.01, 61.0, f64::NAN, f64::INFINITY] {
            let mut q = valid();
            q.position = bad;
            assert!(validate_new(&q).is_err(), "position={bad} 应被拒绝");
        }
        for ok in [0.0, 1.0, 0.999999] {
            let mut q = valid();
            q.position = ok;
            assert!(validate_new(&q).is_ok(), "position={ok} 应通过");
        }
    }

    #[test]
    fn empty_book_and_negative_chapter_are_rejected() {
        let mut q = valid();
        q.book_id = "  ".into();
        assert!(validate_new(&q).is_err());
        let mut q2 = valid();
        q2.chapter_index = -1;
        assert!(validate_new(&q2).is_err());
    }

    // 按书过滤 + 时间倒序（同一秒内用 id 兜底排序）
    #[tokio::test]
    async fn list_filters_by_book_and_orders_desc() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE bookmarks (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id TEXT NOT NULL, \
             chapter_index INTEGER NOT NULL DEFAULT 0, position REAL NOT NULL DEFAULT 0, \
             chapter_title TEXT, note TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now')))",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut a = valid();
        a.book_id = "b1".into();
        a.chapter_index = 1;
        let mut b = valid();
        b.book_id = "b1".into();
        b.chapter_index = 2;
        let mut other = valid();
        other.book_id = "b2".into();
        insert_bookmark(&pool, &a).await.unwrap();
        insert_bookmark(&pool, &b).await.unwrap();
        insert_bookmark(&pool, &other).await.unwrap();
        let rows = list_bookmarks(&pool, Some("b1")).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].chapter_index, 2);
        assert_eq!(rows[1].chapter_index, 1);
        let all = list_bookmarks(&pool, None).await.unwrap();
        assert_eq!(all.len(), 3);
    }
}
