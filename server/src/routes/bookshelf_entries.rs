//! 书架路由的书架条目（move-only 从 bookshelf.rs 拆出）。

use super::*;

pub async fn list(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<ShelfQ>,
) -> AppResult<Json<Vec<BookshelfItem>>> {
    let group = q.group.clone().filter(|g| !g.trim().is_empty());
    let _ = q.book_id;
    let mut rows = sqlx::query_as::<_, BookshelfItem>(SHELF_LIST_SQL)
        .bind(group)
        .fetch_all(&db.0)
        .await?;
    // 更新开关与角标单独回填：列表 SQL 保持稳定，旧库迁移后也能工作
    let flags: Vec<(String, i64)> = sqlx::query_as("SELECT book_id, enable_update FROM bookshelf")
        .fetch_all(&db.0)
        .await?;
    let badges: Vec<(String, i64)> =
        sqlx::query_as("SELECT book_id, MAX(0, total - seen_total) FROM book_updates")
            .fetch_all(&db.0)
            .await?;
    let flags: std::collections::HashMap<String, bool> =
        flags.into_iter().map(|(id, v)| (id, v != 0)).collect();
    let badges: std::collections::HashMap<String, i64> = badges.into_iter().collect();
    for row in &mut rows {
        row.enable_update = flags.get(&row.book_id).copied().unwrap_or(true);
        row.new_chapters = badges.get(&row.book_id).copied().unwrap_or(0);
    }
    Ok(Json(rows))
}

pub async fn add(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<AddQ>,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query(
        "INSERT INTO bookshelf (book_id, title, author, cover, source_id, \"group\") \
         VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(book_id) DO UPDATE SET \
         \"group\"=COALESCE(excluded.\"group\", bookshelf.\"group\")",
    )
    .bind(&q.book_id)
    .bind(&q.name)
    .bind(&q.author)
    .bind(&q.cover)
    .bind(&q.source_id)
    .bind(&q.group)
    .execute(&db.0)
    .await?;
    // 同步写入 book_meta，保证书架/详情使用的分组一致
    sqlx::query(
        "INSERT INTO book_meta (book_url, name, author, cover_url, \"group\") \
         VALUES (?1,?2,?3,?4,?5) ON CONFLICT(book_url) DO UPDATE SET \
         name=COALESCE(excluded.name, book_meta.name), \
         author=COALESCE(excluded.author, book_meta.author), \
         cover_url=COALESCE(excluded.cover_url, book_meta.cover_url), \
         \"group\"=COALESCE(excluded.\"group\", book_meta.\"group\")",
    )
    .bind(&q.book_id)
    .bind(&q.name)
    .bind(&q.author)
    .bind(&q.cover)
    .bind(&q.group)
    .execute(&db.0)
    .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn remove(
    State(_s): State<AppState>,
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

/// 记录一次目录刷新的结果：首次拿到目录时只建基线（不弹角标），之后 total 变化
/// 由 `new_chapters = MAX(0, total - seen_total)` 自动体现。
pub(crate) async fn record_toc_delta(pool: &SqlitePool, book_id: &str, after_total: i64) {
    if after_total <= 0 {
        return;
    }
    let row: Result<Option<(i64, i64)>, _> =
        sqlx::query_as("SELECT total, seen_total FROM book_updates WHERE book_id = ?1")
            .bind(book_id)
            .fetch_optional(pool)
            .await;
    match row {
        Ok(Some((total, seen))) if total > 0 && seen > 0 => {
            let _ = sqlx::query(
                "UPDATE book_updates SET total = ?2, updated_at = datetime('now') WHERE book_id = ?1",
            )
            .bind(book_id)
            .bind(after_total)
            .execute(pool)
            .await;
        }
        _ => {
            // 没有记录 / 从未建立基线：把当前目录总数视为"已看过"，避免首次加载就冒出角标
            let _ = sqlx::query(
                "INSERT INTO book_updates (book_id, total, seen_total, updated_at) \
                 VALUES (?1, ?2, ?2, datetime('now')) \
                 ON CONFLICT(book_id) DO UPDATE SET total = excluded.total, \
                 seen_total = excluded.seen_total, updated_at = datetime('now')",
            )
            .bind(book_id)
            .bind(after_total)
            .execute(pool)
            .await;
        }
    }
}

/// 打开章节后清除角标：把已看总数对齐到当前目录总数，返回对齐后的总数。
pub(crate) async fn mark_shelf_seen(pool: &SqlitePool, id: &str) -> AppResult<i64> {
    let meta_total: Option<i64> =
        sqlx::query_scalar("SELECT total_chapter_num FROM book_meta WHERE book_url = ?1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    let chapter_total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM book_chapters WHERE book_url = ?1")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .unwrap_or(0);
    let total = meta_total.unwrap_or(0).max(chapter_total).max(0);
    sqlx::query(
        "INSERT INTO book_updates (book_id, total, seen_total, updated_at) \
         VALUES (?1, ?2, ?2, datetime('now')) \
         ON CONFLICT(book_id) DO UPDATE SET \
         total = MAX(book_updates.total, excluded.total), \
         seen_total = MAX(book_updates.seen_total, excluded.total), \
         updated_at = datetime('now')",
    )
    .bind(id)
    .bind(total)
    .execute(pool)
    .await?;
    Ok(total)
}

pub async fn mark_seen(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<BookIdQ>,
) -> AppResult<Json<serde_json::Value>> {
    let id = q.book_id.trim();
    if id.is_empty() {
        return Err(AppError::BadRequest("book_id 不能为空".into()));
    }
    let total = mark_shelf_seen(&db.0, id).await?;
    Ok(Json(
        json!({"ok": true, "seen_total": total, "new_chapters": 0}),
    ))
}
