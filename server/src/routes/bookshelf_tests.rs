//! 书架路由单测（move-only 从 bookshelf.rs 拆出）。

use super::{
    apply_shelf_batch, progress_percent, record_progress, validate_position, SHELF_LIST_SQL,
};
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
    let g: Option<String> =
        sqlx::query_scalar("SELECT \"group\" FROM book_meta WHERE book_url='b1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(g.as_deref(), Some("科幻"));

    // 移出分组：group 置 NULL
    let res = apply_shelf_batch(&pool, "remove_group", &["b1".to_string()], None, None).await;
    assert!(res[0].ok);
    let g: Option<String> =
        sqlx::query_scalar("SELECT \"group\" FROM bookshelf WHERE book_id='b1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(g.is_none());

    // 批量移除幂等：b2 成功；missing 本来就不在书架，也视为成功（可安全重试）
    let res = apply_shelf_batch(
        &pool,
        "remove",
        &["b2".to_string(), "missing".to_string()],
        None,
        None,
    )
    .await;
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
    sqlx::query(
        "INSERT INTO bookshelf (book_id, title, added_at) VALUES ('b1','书1',datetime('now'))",
    )
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
    let badge: i64 = sqlx::query_scalar(
        "SELECT MAX(0, total - seen_total) FROM book_updates WHERE book_id='b1'",
    )
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
    let badge: i64 = sqlx::query_scalar(
        "SELECT MAX(0, total - seen_total) FROM book_updates WHERE book_id='b1'",
    )
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

/// 打开阅读页即落库：章号/比例不变时必须是 no-op（不刷新 `updated_at` 与 `last_read_at`），
/// 变了才写；`book_stats.read_chapters` 只增不减。
#[tokio::test]
async fn record_progress_is_idempotent() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open memory db");
    for ddl in [
        "CREATE TABLE book_meta (book_url TEXT PRIMARY KEY, name TEXT, \
         total_chapter_num INTEGER)",
        "CREATE TABLE reading_progress (book_id TEXT PRIMARY KEY, chapter_index INTEGER, \
         position REAL, updated_at TEXT)",
        "CREATE TABLE book_stats (book_id TEXT PRIMARY KEY, name TEXT, total_chapters INTEGER, \
         read_chapters INTEGER, last_read_at TEXT)",
    ] {
        sqlx::query(ddl).execute(&pool).await.expect("ddl");
    }
    sqlx::query("INSERT INTO book_meta (book_url, name, total_chapter_num) VALUES ('b1','书1',30)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO reading_progress (book_id, chapter_index, position, updated_at) \
         VALUES ('b1', 0, 0.0, '2000-01-01 00:00:00')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO book_stats (book_id, name, total_chapters, read_chapters, last_read_at) \
         VALUES ('b1','书1',30,1,'2000-01-01 00:00:00')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // 同一页重复渲染：两行的时间戳都不得变化
    record_progress(&pool, "b1", 0, 0.0, Some(30))
        .await
        .expect("record");
    let (ci, pos, at): (i64, f64, String) =
        sqlx::query_as("SELECT chapter_index, position, updated_at FROM reading_progress")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((ci, pos), (0, 0.0));
    assert_eq!(at, "2000-01-01 00:00:00", "内容未变时不得写脏");
    let last: String = sqlx::query_scalar("SELECT last_read_at FROM book_stats")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(last, "2000-01-01 00:00:00", "内容未变时不得刷新阅读时间");

    // 翻到下一章：进度与统计都要更新，read_chapters 取较大值
    record_progress(&pool, "b1", 1, 0.5, Some(30))
        .await
        .expect("record");
    let (ci2, pos2, at2): (i64, f64, String) =
        sqlx::query_as("SELECT chapter_index, position, updated_at FROM reading_progress")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((ci2, pos2), (1, 0.5));
    assert_ne!(at2, "2000-01-01 00:00:00", "进度变化时必须刷新时间戳");
    let (rc, last2): (i64, String) =
        sqlx::query_as("SELECT read_chapters, last_read_at FROM book_stats")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rc, 2, "read_chapters = 章号 + 1");
    assert_ne!(last2, "2000-01-01 00:00:00", "进度变化时刷新最近阅读");
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
