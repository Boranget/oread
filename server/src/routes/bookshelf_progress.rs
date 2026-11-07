//! 书架路由的阅读进度（move-only 从 bookshelf.rs 拆出）。

use super::*;

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

/// 写入阅读进度：`/k/read` 渲染路径与 Web 的进度接口共用同一份落库逻辑。
///
/// - `reading_progress` 每本书一行（`book_id` 主键），`position` 为章内比例 0..1；
/// - 同步 `book_stats`：已读章节数、总章节数、最近阅读时间（`read_chapters` 只增不减）；
/// - **幂等**：章号与比例都没变时不产生 UPDATE（幂等由 `WHERE` 保证），
///   打开阅读页反复渲染不会写脏，也不会持续放大 WAL。
pub(crate) async fn record_progress(
    pool: &SqlitePool,
    book_id: &str,
    chapter_index: i64,
    position: f64,
    total_chapters: Option<i64>,
) -> sqlx::Result<()> {
    let ci = chapter_index.max(0);
    let pos = if position.is_finite() {
        position.clamp(0.0, 1.0)
    } else {
        0.0
    };
    // 内容没变 → rows_affected = 0，后面据此决定是否刷新 book_stats.last_read_at
    let changed = sqlx::query(
        "INSERT INTO reading_progress (book_id, chapter_index, position) VALUES (?1,?2,?3) \
         ON CONFLICT(book_id) DO UPDATE SET chapter_index=excluded.chapter_index, \
         position=excluded.position, updated_at=datetime('now') \
         WHERE reading_progress.chapter_index IS NOT excluded.chapter_index \
            OR reading_progress.position IS NOT excluded.position",
    )
    .bind(book_id)
    .bind(ci)
    .bind(pos)
    .execute(pool)
    .await?
    .rows_affected()
        > 0;

    let total = match total_chapters {
        Some(t) if t > 0 => t,
        _ => sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(total_chapter_num,0) FROM book_meta WHERE book_url = ?",
        )
        .bind(book_id)
        .fetch_optional(pool)
        .await?
        .unwrap_or(0),
    };
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM book_meta WHERE book_url = ?")
        .bind(book_id)
        .fetch_optional(pool)
        .await?;
    sqlx::query(
        "INSERT INTO book_stats (book_id, name, total_chapters, read_chapters, last_read_at) \
         VALUES (?1,?2,?3,?4, datetime('now')) \
         ON CONFLICT(book_id) DO UPDATE SET \
         name=COALESCE(excluded.name, book_stats.name), \
         total_chapters=CASE WHEN excluded.total_chapters>0 THEN excluded.total_chapters ELSE book_stats.total_chapters END, \
         read_chapters=MAX(book_stats.read_chapters, excluded.read_chapters), \
         last_read_at=datetime('now') \
         WHERE ?5=1 \
            OR book_stats.name IS NOT COALESCE(excluded.name, book_stats.name) \
            OR excluded.total_chapters > book_stats.total_chapters \
            OR excluded.read_chapters > book_stats.read_chapters",
    )
    .bind(book_id)
    .bind(name)
    .bind(total)
    .bind(ci + 1)
    .bind(changed as i64)
    .execute(pool)
    .await?;
    Ok(())
}

/// 校验并返回章内比例：只接受 0..=1 的有限值（`0`、`1`、`1.0` 合法；`61`、负数、NaN 拒绝）。
pub(crate) fn validate_position(position: f64) -> Result<f64, AppError> {
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
    let mut total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(total_chapter_num,0) FROM book_meta WHERE book_url = ?",
    )
    .bind(&q.book_id)
    .fetch_optional(&db.0)
    .await?
    .unwrap_or(0);
    if total <= 0 {
        total = sqlx::query_scalar(
            "SELECT COALESCE(total_chapters,0) FROM book_stats WHERE book_id = ?",
        )
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
    record_progress(&db.0, &q.book_id, chapter_index, position, q.total_chapters).await?;

    Ok(Json(json!({
        "ok": true,
        "chapter_index": chapter_index,
        "position": position,
    })))
}
