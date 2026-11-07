//! 书架路由的统计与分组管理（move-only 从 bookshelf.rs 拆出）。

use super::*;

/// 书架统计：连续阅读天数 + 每本完成度（供书架页内联展示，避免逐本请求进度）。
pub async fn shelf_stats(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
) -> AppResult<Json<serde_json::Value>> {
    let streak: Option<(i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT current_days, last_day, longest_days FROM reading_streak WHERE id=1",
    )
    .fetch_optional(&db.0)
    .await?;
    let (current, last_day, longest) = streak.unwrap_or((0, None, 0));
    let today = chrono::Local::now().date_naive();
    let today_str = today.format("%Y-%m-%d").to_string();
    let yesterday_str = (today - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let streak_days = match last_day.as_deref() {
        Some(d) if d == today_str || d == yesterday_str => current,
        _ => 0,
    };
    let today_seconds: i64 = sqlx::query_scalar(
        "SELECT COALESCE(seconds,0) FROM reading_stats WHERE day = date('now','localtime')",
    )
    .fetch_optional(&db.0)
    .await?
    .unwrap_or(0);
    #[allow(clippy::type_complexity)]
    // 行类型与 SELECT 列一一对应；抽成别名反而在列变更时不易察觉。
    let books: Vec<(String, Option<String>, i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT book_id, name, total_chapters, read_chapters, last_read_at \
         FROM book_stats ORDER BY last_read_at DESC LIMIT 50",
    )
    .fetch_all(&db.0)
    .await?;
    let books: Vec<serde_json::Value> = books
        .into_iter()
        .map(|(book_id, name, total, read, last)| {
            let completion = if total > 0 {
                (read as f64 / total as f64 * 100.0).min(100.0)
            } else {
                0.0
            };
            json!({
                "book_id": book_id,
                "name": name,
                "total_chapters": total,
                "read_chapters": read,
                "completion": completion,
                "last_read_at": last,
            })
        })
        .collect();
    Ok(Json(json!({
        "streak_days": streak_days,
        "longest_streak_days": longest,
        "today_seconds": today_seconds,
        "books": books,
    })))
}

pub async fn list_groups(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
) -> AppResult<Json<Vec<serde_json::Value>>> {
    #[allow(clippy::type_complexity)]
    // 行类型与 SELECT 列一一对应；抽成别名反而在列变更时不易察觉。
    let rows: Vec<(i64, String, Option<String>, i64, i64, bool, bool, i64)> = sqlx::query_as(
        "SELECT id, name, cover, \"order\", book_sort, enable_refresh, show, \"order\" \
             FROM book_groups ORDER BY \"order\", id",
    )
    .fetch_all(&db.0)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(
                |(id, name, cover, order, book_sort, enable_refresh, show, _)| {
                    json!({
                        "group_id": id,
                        "group_name": name,
                        "cover": cover,
                        "order": order,
                        "book_sort": book_sort,
                        "enable_refresh": enable_refresh,
                        "show": show,
                    })
                },
            )
            .collect(),
    ))
}

/// 分组局部更新：只覆盖请求里给出的字段（COALESCE），空 `name` 视为未给出。
/// 改名时，同一事务内把 `bookshelf` / `book_meta` 里挂在该组名下的书一起迁移，
/// 避免出现“组名已改、书还挂在旧名”的中间态（失败整体回滚）。
pub(crate) async fn update_group(
    pool: &SqlitePool,
    id: i64,
    name: Option<String>,
    order: Option<i64>,
    book_sort: Option<i64>,
    cover: Option<String>,
    show: Option<bool>,
) -> Result<(), sqlx::Error> {
    let new_name = name.filter(|s| !s.is_empty());
    let mut tx = pool.begin().await?;
    let old_name: Option<String> = sqlx::query_scalar("SELECT name FROM book_groups WHERE id = ?1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE book_groups SET name=COALESCE(?2, name), \"order\"=COALESCE(?3, \"order\"), \
         book_sort=COALESCE(?4, book_sort), cover=COALESCE(?5, cover), \
         show=COALESCE(?6, show) WHERE id=?1",
    )
    .bind(id)
    .bind(new_name.as_deref())
    .bind(order)
    .bind(book_sort)
    .bind(cover.filter(|s| !s.is_empty()))
    .bind(show)
    .execute(&mut *tx)
    .await?;
    if let (Some(old), Some(new)) = (old_name.as_deref(), new_name.as_deref()) {
        if old != new {
            sqlx::query("UPDATE bookshelf SET \"group\" = ?2 WHERE \"group\" = ?1")
                .bind(old)
                .bind(new)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE book_meta SET \"group\" = ?2 WHERE \"group\" = ?1")
                .bind(old)
                .bind(new)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// 新增或更新分组（带 id 时更新）
pub async fn add_group(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<GroupQ>,
) -> AppResult<Json<serde_json::Value>> {
    let new_name = q
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    if let Some(id) = q.id {
        if let Some(name) = &new_name {
            // 无唯一索引，靠应用层查重（改名时排除自己）
            let dup: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM book_groups WHERE name = ?1 AND id <> ?2")
                    .bind(name)
                    .bind(id)
                    .fetch_one(&db.0)
                    .await?;
            if dup > 0 {
                return Err(crate::error::AppError::BadRequest("分组名已存在".into()));
            }
        }
        update_group(&db.0, id, new_name, q.order, q.book_sort, q.cover, q.show).await?;
        return Ok(Json(json!({"ok": true, "id": id})));
    }
    let name = new_name.unwrap_or_default();
    if name.is_empty() {
        return Err(crate::error::AppError::BadRequest(
            "group name required".into(),
        ));
    }
    let dup: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_groups WHERE name = ?1")
        .bind(&name)
        .fetch_one(&db.0)
        .await?;
    if dup > 0 {
        return Err(crate::error::AppError::BadRequest("分组名已存在".into()));
    }
    let id = sqlx::query(
        "INSERT INTO book_groups (name, \"order\", book_sort, cover, show) \
         VALUES (?1,?2,?3,?4,COALESCE(?5,1))",
    )
    .bind(&name)
    .bind(q.order.unwrap_or(0))
    .bind(q.book_sort.unwrap_or(0))
    .bind(q.cover.filter(|s| !s.is_empty()))
    .bind(q.show)
    .execute(&db.0)
    .await?
    .last_insert_rowid();
    Ok(Json(json!({"ok": true, "id": id})))
}

/// 删除分组：仅删除分组定义，书架书籍的 group 字段保留（Legado 行为）
pub async fn delete_group(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<GroupIdQ>,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query("DELETE FROM book_groups WHERE id = ?")
        .bind(q.id)
        .execute(&db.0)
        .await?;
    Ok(Json(json!({"ok": true})))
}
