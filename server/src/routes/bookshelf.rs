use axum::{
    extract::{Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;

use crate::auth::{AuthUser, UserDb};
use crate::error::{AppError, AppResult};
use crate::models::BookshelfItem;
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/bookshelf", get(list).post(add).delete(remove))
        .route("/api/bookshelf/group", post(set_book_group))
        .route("/api/bookshelf/batch", post(batch))
        .route("/api/bookshelf/seen", post(mark_seen))
        .route("/api/bookshelf/stats", get(shelf_stats))
        .route(
            "/api/bookshelf/groups",
            get(list_groups).post(add_group).delete(delete_group),
        )
        .route("/api/bookshelf/groups/delete", delete(delete_group))
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
    /// 加入书架时可选分组（Legado `Book.group`）
    group: Option<String>,
}
#[derive(Deserialize)]
pub struct BookIdQ {
    book_id: String,
}
#[derive(Deserialize)]
pub struct ShelfQ {
    book_id: Option<String>,
    group: Option<String>,
}
#[derive(Deserialize)]
pub struct ProgressQ {
    book_id: String,
    chapter_index: Option<i64>,
    position: Option<f64>,
    total_chapters: Option<i64>,
}
#[derive(Deserialize)]
pub struct GroupQ {
    id: Option<i64>,
    name: Option<String>,
    order: Option<i64>,
    book_sort: Option<i64>,
    /// 分组封面；未传/空串保持原值（COALESCE），传非空才覆盖
    cover: Option<String>,
    /// 是否在书架展示；未传保持原值
    show: Option<bool>,
}
#[derive(Deserialize)]
pub struct GroupIdQ {
    id: i64,
}
/// 把书籍移入/移出分组：支持单本 `{book_id}` 或批量 `{book_ids}`；`group=null` 表示移出分组
#[derive(Deserialize)]
pub struct SetGroupQ {
    book_id: Option<String>,
    book_ids: Option<Vec<String>>,
    group: Option<String>,
}

/// 书架列表 SQL。`position` 必须显式 `CAST(... AS REAL)`：`COALESCE(rp.position, 0)`
/// 在无进度记录时会得到 SQLite INTEGER，而结构体字段是 f64，sqlx 严格解码会报
/// `mismatched types` 把整个列表打成 500（验收 P0）。
/// 总章节数优先 `book_meta.total_chapter_num`，为 0 时回退 `book_stats.total_chapters`。
pub(crate) const SHELF_LIST_SQL: &str = "SELECT bs.book_id, COALESCE(bm.name, bs.title) as title, \
     COALESCE(bm.author, bs.author) as author, COALESCE(bm.cover_url, bs.cover) as cover, \
     bs.source_id, COALESCE(bs.\"group\", bm.\"group\") as \"group\", \
     COALESCE(rp.chapter_index, 0) as chapter_index, CAST(COALESCE(rp.position, 0) AS REAL) as position, \
     COALESCE(NULLIF(bm.total_chapter_num, 0), bst.total_chapters, 0) as total_chapter_num, \
     COALESCE(NULLIF(bm.total_chapter_num, 0), bst.total_chapters, 0) as total_chapters, \
     CASE WHEN COALESCE(NULLIF(bm.total_chapter_num, 0), bst.total_chapters, 0) > 0 \
          THEN MIN(100.0, (COALESCE(rp.chapter_index, 0) \
               + MIN(1.0, MAX(0.0, CAST(COALESCE(rp.position, 0) AS REAL)))) * 100.0 \
               / COALESCE(NULLIF(bm.total_chapter_num, 0), bst.total_chapters, 0)) \
          ELSE 0.0 END as reading_progress, \
     bs.added_at, rp.updated_at as last_read_at \
     FROM bookshelf bs \
     LEFT JOIN book_meta bm ON bs.book_id = bm.book_url \
     LEFT JOIN book_stats bst ON bs.book_id = bst.book_id \
     LEFT JOIN reading_progress rp ON bs.book_id = rp.book_id \
     WHERE (?1 IS NULL OR COALESCE(bs.\"group\", bm.\"group\") = ?1) \
     ORDER BY COALESCE(rp.updated_at, bs.added_at) DESC";

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
    let flags: Vec<(String, i64)> =
        sqlx::query_as("SELECT book_id, enable_update FROM bookshelf")
            .fetch_all(&db.0)
            .await?;
    let badges: Vec<(String, i64)> = sqlx::query_as(
        "SELECT book_id, MAX(0, total - seen_total) FROM book_updates",
    )
    .fetch_all(&db.0)
    .await?;
    let flags: std::collections::HashMap<String, bool> = flags
        .into_iter()
        .map(|(id, v)| (id, v != 0))
        .collect();
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

/// 记录一次目录刷新的结果：首次拿到目录时只建基线（不弹角标），之后 total 变化
/// 由 `new_chapters = MAX(0, total - seen_total)` 自动体现。
pub(crate) async fn record_toc_delta(pool: &SqlitePool, book_id: &str, after_total: i64) {
    if after_total <= 0 {
        return;
    }
    let row: Result<Option<(i64, i64)>, _> = sqlx::query_as(
        "SELECT total, seen_total FROM book_updates WHERE book_id = ?1",
    )
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
    Ok(Json(json!({"ok": true, "seen_total": total, "new_chapters": 0})))
}

/// 设置书籍分组（单本或批量）；`group` 为空表示移出分组。
pub async fn set_book_group(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<SetGroupQ>,
) -> AppResult<Json<serde_json::Value>> {
    let mut ids: Vec<String> = q.book_ids.unwrap_or_default();
    if let Some(id) = q.book_id {
        ids.push(id);
    }
    if ids.is_empty() {
        return Err(crate::error::AppError::BadRequest(
            "book_id or book_ids required".into(),
        ));
    }
    let group = q.group.filter(|g| !g.trim().is_empty());
    for id in &ids {
        sqlx::query("UPDATE bookshelf SET \"group\"=?2 WHERE book_id=?1")
            .bind(id)
            .bind(&group)
            .execute(&db.0)
            .await?;
        sqlx::query("UPDATE book_meta SET \"group\"=?2 WHERE book_url=?1")
            .bind(id)
            .bind(&group)
            .execute(&db.0)
            .await?;
    }
    Ok(Json(json!({"ok": true, "updated": ids.len()})))
}

#[derive(Deserialize)]
pub struct BatchQ {
    /// `set_group`（含 add_group）/ `remove_group` / `remove`
    action: String,
    #[serde(default)]
    book_ids: Vec<String>,
    /// set_group 时的目标分组；null/空串表示移出分组
    group: Option<String>,
    /// set_update 时是否启用目录更新
    enabled: Option<bool>,
}

pub(crate) const SHELF_BATCH_MAX: usize = 500;

#[derive(Default)]
pub(crate) struct BatchItemResult {
    pub book_id: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// 逐项执行书架批量操作：每项独立事务，失败只影响该项（不做全有或全无）。
pub(crate) async fn apply_shelf_batch(
    pool: &SqlitePool,
    action: &str,
    book_ids: &[String],
    group: Option<&str>,
    enabled: Option<bool>,
) -> Vec<BatchItemResult> {
    let mut out = Vec::with_capacity(book_ids.len());
    for id in book_ids {
        let mut res = BatchItemResult {
            book_id: id.clone(),
            ok: false,
            error: None,
        };
        let mut tx = match pool.begin().await {
            Ok(t) => t,
            Err(e) => {
                res.error = Some(format!("打开事务失败：{e}"));
                out.push(res);
                continue;
            }
        };
        let result = match action {
            "remove" => sqlx::query("DELETE FROM bookshelf WHERE book_id = ?1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map(|r| r.rows_affected()),
            "set_update" => sqlx::query("UPDATE bookshelf SET enable_update = ?2 WHERE book_id = ?1")
                .bind(id)
                .bind(if enabled.unwrap_or(true) { 1i64 } else { 0i64 })
                .execute(&mut *tx)
                .await
                .map(|r| r.rows_affected()),
            _ => sqlx::query("UPDATE bookshelf SET \"group\" = ?2 WHERE book_id = ?1")
                .bind(id)
                .bind(group)
                .execute(&mut *tx)
                .await
                .map(|r| r.rows_affected()),
        };
        match result {
            // 删除是幂等的：书已不在书架也视为成功（重复批量删除不会报失败）
            Ok(0) if action == "remove" => match tx.commit().await {
                Ok(_) => res.ok = true,
                Err(e) => res.error = Some(format!("提交失败：{e}")),
            },
            Ok(0) => {
                res.error = Some("书籍不在书架".into());
                let _ = tx.rollback().await;
            }
            Ok(_) => {
                if action != "remove" && action != "set_update" {
                    if let Err(e) = sqlx::query("UPDATE book_meta SET \"group\" = ?2 WHERE book_url = ?1")
                        .bind(id)
                        .bind(group)
                        .execute(&mut *tx)
                        .await
                    {
                        res.error = Some(format!("更新 book_meta 失败：{e}"));
                        let _ = tx.rollback().await;
                        out.push(res);
                        continue;
                    }
                }
                match tx.commit().await {
                    Ok(_) => res.ok = true,
                    Err(e) => res.error = Some(format!("提交失败：{e}")),
                }
            }
            Err(e) => {
                res.error = Some(format!("写入失败：{e}"));
                let _ = tx.rollback().await;
            }
        }
        out.push(res);
    }
    out
}

/// 批量移入/移出分组、批量移除；返回逐项成功/失败，不做全有或全无。
pub async fn batch(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<BatchQ>,
) -> AppResult<Json<serde_json::Value>> {
    let action = q.action.trim().to_ascii_lowercase();
    let action = match action.as_str() {
        "set_group" | "add_group" => "set_group",
        "remove_group" => "remove_group",
        "remove" => "remove",
        "set_update" | "enable_update" | "set_refresh" | "enable_refresh" => "set_update",
        other => {
            return Err(AppError::BadRequest(format!(
                "不支持的 action：{other}（可用 set_group/remove_group/remove/set_update）"
            )))
        }
    };
    if action == "set_update" && q.enabled.is_none() {
        return Err(AppError::BadRequest(
            "set_update 需要 enabled=true/false".into(),
        ));
    }
    let mut ids: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in q.book_ids {
        let id = raw.trim().to_string();
        if !id.is_empty() && seen.insert(id.clone()) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err(AppError::BadRequest("book_ids 不能为空".into()));
    }
    if ids.len() > SHELF_BATCH_MAX {
        return Err(AppError::BadRequest(format!(
            "单次最多 {SHELF_BATCH_MAX} 本，请分批调用"
        )));
    }
    let group = if action == "remove_group" {
        None
    } else {
        q.group
            .as_deref()
            .map(str::trim)
            .filter(|g| !g.is_empty())
    };
    let results = apply_shelf_batch(&db.0, action, &ids, group, q.enabled).await;
    let updated = results.iter().filter(|r| r.ok).count();
    let failed = results.len() - updated;
    Ok(Json(json!({
        "ok": true,
        "action": action,
        "group": group,
        "enabled": q.enabled,
        "updated": updated,
        "failed": failed,
        "results": results.into_iter().map(|r| json!({
            "book_id": r.book_id,
            "ok": r.ok,
            "error": r.error,
        })).collect::<Vec<_>>(),
    })))
}

/// 书架统计：连续阅读天数 + 每本完成度（供书架页内联展示，避免逐本请求进度）。
pub async fn shelf_stats(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
) -> AppResult<Json<serde_json::Value>> {
    let streak: Option<(i64, Option<String>, i64)> =
        sqlx::query_as("SELECT current_days, last_day, longest_days FROM reading_streak WHERE id=1")
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
    let rows: Vec<(i64, String, Option<String>, i64, i64, bool, bool, i64)> =
        sqlx::query_as(
            "SELECT id, name, cover, \"order\", book_sort, enable_refresh, show, \"order\" \
             FROM book_groups ORDER BY \"order\", id",
        )
        .fetch_all(&db.0)
        .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, cover, order, book_sort, enable_refresh, show, _)| {
                json!({
                    "group_id": id,
                    "group_name": name,
                    "cover": cover,
                    "order": order,
                    "book_sort": book_sort,
                    "enable_refresh": enable_refresh,
                    "show": show,
                })
            })
            .collect(),
    ))
}

/// 分组局部更新：只覆盖请求里给出的字段（COALESCE），空 `name` 视为未给出。
pub(crate) async fn update_group(
    pool: &SqlitePool,
    id: i64,
    name: Option<String>,
    order: Option<i64>,
    book_sort: Option<i64>,
    cover: Option<String>,
    show: Option<bool>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE book_groups SET name=COALESCE(?2, name), \"order\"=COALESCE(?3, \"order\"), \
         book_sort=COALESCE(?4, book_sort), cover=COALESCE(?5, cover), \
         show=COALESCE(?6, show) WHERE id=?1",
    )
    .bind(id)
    .bind(name.filter(|s| !s.is_empty()))
    .bind(order)
    .bind(book_sort)
    .bind(cover.filter(|s| !s.is_empty()))
    .bind(show)
    .execute(pool)
    .await?;
    Ok(())
}

/// 新增或更新分组（带 id 时更新）
pub async fn add_group(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<GroupQ>,
) -> AppResult<Json<serde_json::Value>> {
    let name = q.name.clone().unwrap_or_default();
    if let Some(id) = q.id {
        update_group(&db.0, id, q.name, q.order, q.book_sort, q.cover, q.show).await?;
        return Ok(Json(json!({"ok": true, "id": id})));
    }
    if name.is_empty() {
        return Err(crate::error::AppError::BadRequest(
            "group name required".into(),
        ));
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

/// 统一进度百分比：`(chapter_index + clamp(position,0,1)) / total * 100`。
/// 两个接口（书架列表 `reading_progress`、`GET /api/bookshelf/progress`）必须同口径。
pub(crate) fn progress_percent(chapter_index: i64, position: f64, total: i64) -> f64 {
    if total <= 0 {
        return 0.0;
    }
    let ci = chapter_index.max(0) as f64;
    let pos = if position.is_finite() {
        position.clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((ci + pos) / total as f64 * 100.0).clamp(0.0, 100.0)
}

/// 校验并返回章内比例：只接受 0..=1 的有限值（`0`、`1`、`1.0` 合法；`61`、负数、NaN 拒绝）。
fn validate_position(position: f64) -> Result<f64, AppError> {
    if position.is_finite() && (0.0..=1.0).contains(&position) {
        Ok(position)
    } else {
        Err(AppError::BadRequest(
            "position 必须是 0..1 的章内比例".to_string(),
        ))
    }
}

pub async fn get_progress(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<BookIdQ>,
) -> AppResult<Json<serde_json::Value>> {
    let row: Option<(i64, f64, Option<String>)> = sqlx::query_as(
        "SELECT chapter_index, position, updated_at FROM reading_progress WHERE book_id = ?",
    )
    .bind(&q.book_id)
    .fetch_optional(&db.0)
    .await?;
    // 目录数可能只落在 book_stats（进度上报带 total_chapters 时），book_meta 为 0 时回退
    let mut total: i64 =
        sqlx::query_scalar("SELECT COALESCE(total_chapter_num,0) FROM book_meta WHERE book_url = ?")
            .bind(&q.book_id)
            .fetch_optional(&db.0)
            .await?
            .unwrap_or(0);
    if total <= 0 {
        total = sqlx::query_scalar("SELECT COALESCE(total_chapters,0) FROM book_stats WHERE book_id = ?")
            .bind(&q.book_id)
            .fetch_optional(&db.0)
            .await?
            .unwrap_or(0);
    }
    let percent = progress_percent(
        row.as_ref().map(|x| x.0).unwrap_or(0),
        row.as_ref().map(|x| x.1).unwrap_or(0.0),
        total,
    );
    match row {
        Some((ci, pos, updated_at)) => Ok(Json(json!({
            "chapter_index": ci,
            "position": pos,
            "total_chapters": total,
            "percent": percent,
            "updated_at": updated_at,
        }))),
        None => Ok(Json(json!({
            "chapter_index": 0, "position": 0, "total_chapters": total,
            "percent": 0.0, "updated_at": null
        }))),
    }
}

/// 保存进度：`position` 为章节内滚动百分比（0~1），透传保存供阅读器恢复。
pub async fn set_progress(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<ProgressQ>,
) -> AppResult<Json<serde_json::Value>> {
    let chapter_index = q.chapter_index.unwrap_or(0).max(0);
    let position = validate_position(q.position.unwrap_or(0.0))?;
    sqlx::query(
        "INSERT INTO reading_progress (book_id, chapter_index, position) \
         VALUES (?1,?2,?3) ON CONFLICT(book_id) DO UPDATE SET \
         chapter_index=excluded.chapter_index, position=excluded.position, updated_at=datetime('now')",
    )
    .bind(&q.book_id)
    .bind(chapter_index)
    .bind(position)
    .execute(&db.0)
    .await?;

    // 同步单本完成度与总章节数（供阅读统计聚合）
    let total = match q.total_chapters {
        Some(t) if t > 0 => t,
        _ => sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(total_chapter_num,0) FROM book_meta WHERE book_url = ?",
        )
        .bind(&q.book_id)
        .fetch_optional(&db.0)
        .await?
        .unwrap_or(0),
    };
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM book_meta WHERE book_url = ?")
        .bind(&q.book_id)
        .fetch_optional(&db.0)
        .await?;
    sqlx::query(
        "INSERT INTO book_stats (book_id, name, total_chapters, read_chapters, last_read_at) \
         VALUES (?1,?2,?3,?4, datetime('now')) ON CONFLICT(book_id) DO UPDATE SET \
         name=COALESCE(excluded.name, book_stats.name), \
         total_chapters=CASE WHEN excluded.total_chapters>0 THEN excluded.total_chapters ELSE book_stats.total_chapters END, \
         read_chapters=MAX(book_stats.read_chapters, excluded.read_chapters), \
         last_read_at=datetime('now')",
    )
    .bind(&q.book_id)
    .bind(name)
    .bind(total)
    .bind(chapter_index + 1)
    .execute(&db.0)
    .await?;

    Ok(Json(json!({
        "ok": true,
        "chapter_index": chapter_index,
        "position": position,
    })))
}

#[cfg(test)]
mod tests {
    use super::{apply_shelf_batch, progress_percent, validate_position, SHELF_LIST_SQL};
    use crate::models::BookshelfItem;
    use sqlx::sqlite::SqlitePoolOptions;

    /// 验收 P0 回归：书架里存在「未上报进度」的书时，列表 SQL 不能因
    /// INTEGER → f64 解码失败而 500；同时验证 total_chapters 回退到 book_stats。
    #[tokio::test]
    async fn shelf_list_decodes_without_progress() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open memory db");
        for ddl in [
            "CREATE TABLE bookshelf (book_id TEXT PRIMARY KEY, title TEXT, author TEXT, \
             cover TEXT, source_id TEXT, \"group\" TEXT, added_at TEXT)",
            "CREATE TABLE book_meta (book_url TEXT PRIMARY KEY, name TEXT, author TEXT, \
             cover_url TEXT, \"group\" TEXT, total_chapter_num INTEGER)",
            "CREATE TABLE book_stats (book_id TEXT PRIMARY KEY, total_chapters INTEGER)",
            "CREATE TABLE reading_progress (book_id TEXT PRIMARY KEY, chapter_index INTEGER, \
             position REAL, updated_at TEXT)",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }
        sqlx::query(
            "INSERT INTO bookshelf (book_id, title, author, \"group\", added_at) \
             VALUES ('b1','书1','作者','默认', datetime('now'))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO book_meta (book_url, name, author, total_chapter_num) \
             VALUES ('b1','书1','作者',0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO book_stats (book_id, total_chapters) VALUES ('b1', 30)")
            .execute(&pool)
            .await
            .unwrap();

        let rows = sqlx::query_as::<_, BookshelfItem>(SHELF_LIST_SQL)
            .bind(Option::<String>::None)
            .fetch_all(&pool)
            .await
            .expect("shelf list must decode without reading_progress row");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].position, 0.0);
        assert_eq!(rows[0].total_chapters, 30, "fallback to book_stats");
        assert_eq!(rows[0].reading_progress, 0.0);
    }

    /// 批量操作逐项独立：存在的书成功、不存在的书失败，整体不因个别失败回滚。
    #[tokio::test]
    async fn batch_ops_are_per_item_and_not_all_or_nothing() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open memory db");
        for ddl in [
            "CREATE TABLE bookshelf (book_id TEXT PRIMARY KEY, title TEXT, author TEXT, \
             cover TEXT, source_id TEXT, \"group\" TEXT, added_at TEXT, \
             enable_update INTEGER NOT NULL DEFAULT 1)",
            "CREATE TABLE book_meta (book_url TEXT PRIMARY KEY, name TEXT, author TEXT, \
             cover_url TEXT, \"group\" TEXT, total_chapter_num INTEGER)",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }
        for id in ["b1", "b2"] {
            sqlx::query(
                "INSERT INTO bookshelf (book_id, title, added_at) VALUES (?, ?, datetime('now'))",
            )
            .bind(id)
            .bind(format!("书{id}"))
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO book_meta (book_url, name) VALUES (?, ?)")
                .bind(id)
                .bind(format!("书{id}"))
                .execute(&pool)
                .await
                .unwrap();
        }

        let ids = vec!["b1".to_string(), "b2".to_string(), "missing".to_string()];
        let res = apply_shelf_batch(&pool, "set_group", &ids, Some("科幻"), None).await;
        assert_eq!(res.len(), 3);
        assert!(res[0].ok && res[1].ok);
        assert!(!res[2].ok, "不存在的书必须单独失败");
        assert_eq!(res[2].error.as_deref(), Some("书籍不在书架"));
        let g: Option<String> = sqlx::query_scalar("SELECT \"group\" FROM book_meta WHERE book_url='b1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(g.as_deref(), Some("科幻"));

        // 移出分组：group 置 NULL
        let res = apply_shelf_batch(&pool, "remove_group", &["b1".to_string()], None, None).await;
        assert!(res[0].ok);
        let g: Option<String> = sqlx::query_scalar("SELECT \"group\" FROM bookshelf WHERE book_id='b1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(g.is_none());

        // 批量移除幂等：b2 成功；missing 本来就不在书架，也视为成功（可安全重试）
        let res = apply_shelf_batch(&pool, "remove", &["b2".to_string(), "missing".to_string()], None, None).await;
        assert!(res[0].ok);
        assert!(res[1].ok, "重复删除必须幂等");
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bookshelf")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 1);

        // 批量启用/停用更新：逐本写 bookshelf.enable_update
        let res = apply_shelf_batch(&pool, "set_update", &["b1".to_string()], None, Some(false)).await;
        assert!(res[0].ok);
        let flag: i64 = sqlx::query_scalar("SELECT enable_update FROM bookshelf WHERE book_id='b1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(flag, 0);
        let res = apply_shelf_batch(&pool, "set_update", &["b1".to_string()], None, Some(true)).await;
        assert!(res[0].ok);
        let flag: i64 = sqlx::query_scalar("SELECT enable_update FROM bookshelf WHERE book_id='b1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(flag, 1);
    }

    /// 角标：首次目录只建基线；目录增长后 new = 增量；打开章节后清零。
    #[tokio::test]
    async fn shelf_badge_tracks_new_chapters_then_clears() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open memory db");
        for ddl in [
            "CREATE TABLE bookshelf (book_id TEXT PRIMARY KEY, title TEXT, added_at TEXT, \
             enable_update INTEGER NOT NULL DEFAULT 1)",
            "CREATE TABLE book_meta (book_url TEXT PRIMARY KEY, total_chapter_num INTEGER)",
            "CREATE TABLE book_chapters (url TEXT PRIMARY KEY, book_url TEXT)",
            "CREATE TABLE book_updates (book_id TEXT PRIMARY KEY, total INTEGER NOT NULL DEFAULT 0, \
             seen_total INTEGER NOT NULL DEFAULT 0, updated_at TEXT)",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }
        sqlx::query("INSERT INTO bookshelf (book_id, title, added_at) VALUES ('b1','书1',datetime('now'))")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO book_meta (book_url, total_chapter_num) VALUES ('b1',103)")
            .execute(&pool)
            .await
            .unwrap();

        // 第一次拿到 100 章：建基线，不冒角标
        super::record_toc_delta(&pool, "b1", 100).await;
        let (total, seen): (i64, i64) =
            sqlx::query_as("SELECT total, seen_total FROM book_updates WHERE book_id='b1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((total, seen), (100, 100));

        // 目录涨到 103：角标 3
        super::record_toc_delta(&pool, "b1", 103).await;
        let badge: i64 =
            sqlx::query_scalar("SELECT MAX(0, total - seen_total) FROM book_updates WHERE book_id='b1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(badge, 3);

        // 打开章节 → 清零
        sqlx::query("INSERT INTO book_chapters (url, book_url) VALUES ('c1','b1')")
            .execute(&pool)
            .await
            .unwrap();
        let seen = super::mark_shelf_seen(&pool, "b1").await.unwrap();
        assert_eq!(seen, 103);
        let badge: i64 =
            sqlx::query_scalar("SELECT MAX(0, total - seen_total) FROM book_updates WHERE book_id='b1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(badge, 0);
    }

    /// 进度百分比必须纳入章内 `position`：第 10 章读到 50% → (10+0.5)/30 = 35%。
    #[tokio::test]
    async fn shelf_progress_includes_intra_chapter_position() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open memory db");
        for ddl in [
            "CREATE TABLE bookshelf (book_id TEXT PRIMARY KEY, title TEXT, author TEXT, \
             cover TEXT, source_id TEXT, \"group\" TEXT, added_at TEXT)",
            "CREATE TABLE book_meta (book_url TEXT PRIMARY KEY, name TEXT, author TEXT, \
             cover_url TEXT, \"group\" TEXT, total_chapter_num INTEGER)",
            "CREATE TABLE book_stats (book_id TEXT PRIMARY KEY, total_chapters INTEGER)",
            "CREATE TABLE reading_progress (book_id TEXT PRIMARY KEY, chapter_index INTEGER, \
             position REAL, updated_at TEXT)",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }
        sqlx::query(
            "INSERT INTO bookshelf (book_id, title, added_at) \
             VALUES ('b1','书1', datetime('now'))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO book_meta (book_url, name, total_chapter_num) VALUES ('b1','书1',30)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO reading_progress (book_id, chapter_index, position, updated_at) \
             VALUES ('b1', 10, 0.5, datetime('now'))",
        )
        .execute(&pool)
        .await
        .unwrap();

        let rows = sqlx::query_as::<_, BookshelfItem>(SHELF_LIST_SQL)
            .bind(Option::<String>::None)
            .fetch_all(&pool)
            .await
            .expect("shelf list with progress");
        assert!(
            (rows[0].reading_progress - 35.0).abs() < 1e-9,
            "got {}",
            rows[0].reading_progress
        );
    }

    /// 两个接口共用同一口径；position 越界时 clamp 到 0..1。
    #[test]
    fn progress_percent_uses_intra_chapter_position() {
        assert_eq!(progress_percent(0, 0.0, 100), 0.0);
        let p = progress_percent(1, 0.61, 100);
        assert!((p - 1.61).abs() < 1e-9, "got {p}");
        assert_eq!(progress_percent(99, 1.0, 100), 100.0);
        assert_eq!(progress_percent(0, 2.0, 10), 10.0, "position 上越界 clamp");
        assert_eq!(progress_percent(0, -1.0, 10), 0.0, "position 下越界 clamp");
        assert_eq!(progress_percent(5, 0.5, 0), 0.0, "无总章数时为 0");
        assert_eq!(progress_percent(100, 0.0, 100), 100.0);
    }

    /// 量纲边界：`0`、`1`、`1.0` 合法，`61`/负数/NaN 一律 400。
    #[test]
    fn progress_position_must_be_fraction() {
        for ok in [0.0, 1.0, 1.0_f64] {
            assert!(validate_position(ok).is_ok(), "{ok} 应通过");
        }
        for bad in [61.0, 100.0, -0.01, f64::NAN, f64::INFINITY] {
            assert!(validate_position(bad).is_err(), "{bad} 应被拒绝");
        }
    }

    async fn group_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open memory db");
        sqlx::query(
            "CREATE TABLE book_groups (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, \
             cover TEXT, \"order\" INTEGER NOT NULL DEFAULT 0, enable_refresh INTEGER NOT NULL DEFAULT 1, \
             show INTEGER NOT NULL DEFAULT 1, book_sort INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .expect("ddl");
        sqlx::query(
            "INSERT INTO book_groups (id, name, cover, \"order\", show, book_sort) \
             VALUES (1, '追更', 'old.png', 3, 0, 2)",
        )
        .execute(&pool)
        .await
        .expect("seed");
        pool
    }

    /// 只传 cover：name/show/order/book_sort 必须保持原值。
    #[tokio::test]
    async fn group_update_only_cover_keeps_name() {
        let pool = group_pool().await;
        super::update_group(&pool, 1, None, None, None, Some("new.png".into()), None)
            .await
            .expect("update cover");
        let row: (String, Option<String>, i64, i64, i64) = sqlx::query_as(
            "SELECT name, cover, \"order\", show, book_sort FROM book_groups WHERE id=1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "追更", "name 不应被改动");
        assert_eq!(row.1.as_deref(), Some("new.png"));
        assert_eq!(row.2, 3);
        assert_eq!(row.3, 0);
        assert_eq!(row.4, 2);
    }

    /// 只传 show：cover/name/order/book_sort 必须保持原值。
    #[tokio::test]
    async fn group_update_only_show_keeps_other_fields() {
        let pool = group_pool().await;
        super::update_group(&pool, 1, None, None, None, None, Some(true))
            .await
            .expect("update show");
        let row: (String, Option<String>, i64, i64, i64) = sqlx::query_as(
            "SELECT name, cover, \"order\", show, book_sort FROM book_groups WHERE id=1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "追更");
        assert_eq!(row.1.as_deref(), Some("old.png"));
        assert_eq!(row.2, 3);
        assert_eq!(row.3, 1);
        assert_eq!(row.4, 2);
    }

    /// 空 name 视为未给出，不能把已有分组名清空。
    #[tokio::test]
    async fn group_update_empty_name_is_ignored() {
        let pool = group_pool().await;
        super::update_group(&pool, 1, Some(String::new()), Some(9), None, None, None)
            .await
            .expect("update");
        let (name, order): (String, i64) =
            sqlx::query_as("SELECT name, \"order\" FROM book_groups WHERE id=1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(name, "追更");
        assert_eq!(order, 9);
    }
}
