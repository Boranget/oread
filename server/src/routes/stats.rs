//! 阅读统计：按天聚合阅读时长/字数，推导连续阅读天数，并提供单本完成度。
//!
//! 数据全部落在用户桶（`reading_stats` / `book_stats` / `reading_streak`），
//! 天然按用户隔离；聚合查询只扫描当前用户的小表。

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use chrono::{Datelike, Duration, Local, NaiveDate};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{AuthUser, UserDb};
use crate::error::AppResult;
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/stats/reading", post(record))
        .route("/api/stats/overview", get(overview))
}

#[derive(Deserialize)]
pub struct ReadingQ {
    book_id: String,
    name: Option<String>,
    /// 本次上报的阅读时长（秒）
    seconds: Option<i64>,
    /// 本次上报的阅读字数
    chars: Option<i64>,
    chapter_index: Option<i64>,
    total_chapters: Option<i64>,
}

#[derive(Deserialize)]
pub struct OverviewQ {
    /// 聚合天数（默认 7，上限 365）；显式给出时 `books` 也按该区间过滤
    days: Option<i64>,
}

/// `days` 缺省：保持旧行为，最近 50 本，不做区间过滤
const BOOKS_ALL_SQL: &str = "SELECT book_id, name, total_chapters, read_chapters, last_read_at \
     FROM book_stats ORDER BY last_read_at DESC LIMIT 50";
/// `days` 显式给出：按 `last_read_at` 的日期部分过滤
const BOOKS_SINCE_SQL: &str = "SELECT book_id, name, total_chapters, read_chapters, last_read_at \
     FROM book_stats WHERE substr(last_read_at, 1, 10) >= ?1 \
     ORDER BY last_read_at DESC LIMIT 50";

fn today() -> NaiveDate {
    Local::now().date_naive()
}

fn day_str(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// 记录一次阅读：更新当日时长/字数、单本完成度与连续天数。
pub async fn record(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<ReadingQ>,
) -> AppResult<Json<serde_json::Value>> {
    let seconds = q.seconds.unwrap_or(0).clamp(0, 24 * 3600);
    let chars = q.chars.unwrap_or(0).clamp(0, 10_000_000);
    let day = day_str(today());

    // 当日聚合
    sqlx::query(
        "INSERT INTO reading_stats (day, seconds, chars, updated_at) \
         VALUES (?1,?2,?3, datetime('now')) ON CONFLICT(day) DO UPDATE SET \
         seconds = reading_stats.seconds + excluded.seconds, \
         chars = reading_stats.chars + excluded.chars, \
         updated_at = datetime('now')",
    )
    .bind(&day)
    .bind(seconds)
    .bind(chars)
    .execute(&db.0)
    .await?;

    // 单本完成度
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
    let name = match q.name.clone() {
        Some(n) => Some(n),
        None => {
            sqlx::query_scalar::<_, Option<String>>("SELECT name FROM book_meta WHERE book_url = ?")
                .bind(&q.book_id)
                .fetch_optional(&db.0)
                .await?
                .flatten()
        }
    };
    let read = q.chapter_index.map(|i| i + 1).unwrap_or(0);
    sqlx::query(
        "INSERT INTO book_stats (book_id, name, total_chapters, read_chapters, last_read_at) \
         VALUES (?1,?2,?3,?4, datetime('now')) ON CONFLICT(book_id) DO UPDATE SET \
         name=COALESCE(excluded.name, book_stats.name), \
         total_chapters=CASE WHEN excluded.total_chapters>0 THEN excluded.total_chapters ELSE book_stats.total_chapters END, \
         read_chapters=MAX(book_stats.read_chapters, excluded.read_chapters), \
         last_read_at=datetime('now')",
    )
    .bind(&q.book_id)
    .bind(&name)
    .bind(total)
    .bind(read)
    .execute(&db.0)
    .await?;

    // 连续天数：昨天读过则 +1，否则重置为 1
    let cur: Option<(i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT current_days, last_day, longest_days FROM reading_streak WHERE id=1",
    )
    .fetch_optional(&db.0)
    .await?;
    let (mut current, last_day, longest) = cur.unwrap_or((0, None, 0));
    if last_day.as_deref() != Some(day.as_str()) {
        let yesterday = day_str(today() - Duration::days(1));
        current = if last_day.as_deref() == Some(yesterday.as_str()) {
            current + 1
        } else {
            1
        };
        let longest = longest.max(current);
        sqlx::query(
            "INSERT INTO reading_streak (id, current_days, last_day, longest_days) \
             VALUES (1,?1,?2,?3) ON CONFLICT(id) DO UPDATE SET \
             current_days=excluded.current_days, last_day=excluded.last_day, \
             longest_days=excluded.longest_days",
        )
        .bind(current)
        .bind(&day)
        .bind(longest)
        .execute(&db.0)
        .await?;
    }

    Ok(Json(json!({
        "ok": true,
        "day": day,
        "streak_days": current,
    })))
}

/// 统计总览：按天/周聚合 + 连续天数 + 单本完成度。
pub async fn overview(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<OverviewQ>,
) -> AppResult<Json<serde_json::Value>> {
    let days = q.days.unwrap_or(7).clamp(1, 365);
    let since = day_str(today() - Duration::days(days - 1));

    let daily: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT day, seconds, chars FROM reading_stats WHERE day >= ?1 ORDER BY day",
    )
    .bind(&since)
    .fetch_all(&db.0)
    .await?;

    let total_seconds: i64 = daily.iter().map(|x| x.1).sum();
    let total_chars: i64 = daily.iter().map(|x| x.2).sum();
    let today_seconds = daily
        .iter()
        .find(|x| x.0 == day_str(today()))
        .map(|x| x.1)
        .unwrap_or(0);

    // 周聚合（ISO 周，按 7 天窗口近似：以周一为起点）
    let mut weekly: Vec<(String, i64, i64)> = Vec::new();
    for (day, secs, chars) in &daily {
        let d = NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap_or_else(|_| today());
        let monday = d - Duration::days(d.weekday().num_days_from_monday() as i64);
        let key = day_str(monday);
        match weekly.last_mut() {
            Some((k, s, c)) if k == &key => {
                *s += *secs;
                *c += *chars;
            }
            _ => weekly.push((key, *secs, *chars)),
        }
    }

    let streak: Option<(i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT current_days, last_day, longest_days FROM reading_streak WHERE id=1",
    )
    .fetch_optional(&db.0)
    .await?;
    let (current, last_day, longest) = streak.unwrap_or((0, None, 0));
    // 昨天之前没读过则连续天数已中断
    let active_streak = match last_day.as_deref() {
        Some(d) if d == day_str(today()) || d == day_str(today() - Duration::days(1)) => current,
        _ => 0,
    };

    // `days` 缺省时保持原行为（最近 50 本，不按区间过滤）；显式给出才按 last_read_at 过滤。
    let books: Vec<(String, Option<String>, i64, i64, String)> = if q.days.is_some() {
        sqlx::query_as(BOOKS_SINCE_SQL)
            .bind(&since)
            .fetch_all(&db.0)
            .await?
    } else {
        sqlx::query_as(BOOKS_ALL_SQL).fetch_all(&db.0).await?
    };
    let books_json: Vec<serde_json::Value> = books
        .into_iter()
        .map(|(book_id, name, total, read, last)| {
            let pct = if total > 0 {
                (read as f64 / total as f64 * 100.0).min(100.0)
            } else {
                0.0
            };
            json!({
                "book_id": book_id,
                "name": name,
                "total_chapters": total,
                "read_chapters": read,
                "completion": pct,
                "last_read_at": last,
            })
        })
        .collect();

    Ok(Json(json!({
        "days": days,
        "daily": daily.iter().map(|(d, s, c)| json!({"day": d, "seconds": s, "chars": c})).collect::<Vec<_>>(),
        "weekly": weekly.iter().map(|(w, s, c)| json!({"week": w, "seconds": s, "chars": c})).collect::<Vec<_>>(),
        "total_seconds": total_seconds,
        "total_chars": total_chars,
        "today_seconds": today_seconds,
        "streak_days": active_streak,
        "longest_streak_days": longest,
        "books": books_json,
    })))
}

#[cfg(test)]
mod tests {
    use super::{BOOKS_ALL_SQL, BOOKS_SINCE_SQL};
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open memory db");
        sqlx::query(
            "CREATE TABLE book_stats (book_id TEXT PRIMARY KEY, name TEXT, total_chapters INTEGER, \
             read_chapters INTEGER, last_read_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("ddl");
        for (id, day) in [
            ("old", "2026-01-01 10:00:00"),
            ("recent", "2026-11-02 10:00:00"),
            ("today", "2026-11-03 09:00:00"),
        ] {
            sqlx::query(
                "INSERT INTO book_stats (book_id, name, total_chapters, read_chapters, last_read_at) \
                 VALUES (?1, ?1, 10, 5, ?2)",
            )
            .bind(id)
            .bind(day)
            .execute(&pool)
            .await
            .expect("seed");
        }
        pool
    }

    /// days 缺省：保持旧行为，不做区间过滤。
    #[tokio::test]
    async fn overview_books_without_days_returns_all() {
        let p = pool().await;
        let rows: Vec<(String, Option<String>, i64, i64, String)> =
            sqlx::query_as(BOOKS_ALL_SQL).fetch_all(&p).await.unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, "today");
    }

    /// days 显式给出：只返回 last_read_at 落在区间内的书。
    #[tokio::test]
    async fn overview_books_with_days_filters_by_date() {
        let p = pool().await;
        let rows: Vec<(String, Option<String>, i64, i64, String)> = sqlx::query_as(BOOKS_SINCE_SQL)
            .bind("2026-11-01")
            .fetch_all(&p)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2, "只保留 11-01 及之后");
        let ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert!(ids.contains(&"recent"));
        assert!(ids.contains(&"today"));
        assert!(!ids.contains(&"old"));
    }
}
