//! 馆藏领域逻辑单测：分层、TTL、采购单幂等与删除一致性。

use super::*;
use sqlx::sqlite::SqlitePoolOptions;

async fn pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open memory db");
    sqlx::migrate!("./migrations/library")
        .run(&pool)
        .await
        .expect("library migrations");
    pool
}

async fn insert_book(
    pool: &SqlitePool,
    url: &str,
    tier: &str,
    collected: Option<i64>,
    accessed: i64,
) {
    sqlx::query(
        "INSERT INTO library_books (book_url, name, tier, collected_at, last_accessed_at) \
         VALUES (?1, ?1, ?2, ?3, ?4)",
    )
    .bind(url)
    .bind(tier)
    .bind(collected)
    .bind(accessed)
    .execute(pool)
    .await
    .expect("insert book");
}

async fn insert_content(pool: &SqlitePool, book: &str, url: &str, bytes: i64) {
    sqlx::query(
        "INSERT INTO library_content (url, origin, book_url, content, size_bytes) \
         VALUES (?1, 'src', ?2, 'x', ?3)",
    )
    .bind(url)
    .bind(book)
    .bind(bytes)
    .execute(pool)
    .await
    .expect("insert content");
}

async fn insert_image(pool: &SqlitePool, book: &str, url: &str, bytes: i64) {
    sqlx::query(
        "INSERT INTO library_images (url, origin, book_url, mime, data, size_bytes) \
         VALUES (?1, 'src', ?2, 'image/jpeg', X'00', ?3)",
    )
    .bind(url)
    .bind(book)
    .bind(bytes)
    .execute(pool)
    .await
    .expect("insert image");
}

async fn insert_image_chapter(pool: &SqlitePool, book: &str, url: &str, ready: bool) {
    sqlx::query(
        "INSERT INTO library_content (url, origin, book_url, content, content_type, images, \
           size_bytes, images_ready) \
         VALUES (?1, 'src', ?2, '', 'image', '[]', 2, ?3)",
    )
    .bind(url)
    .bind(book)
    .bind(i64::from(ready))
    .execute(pool)
    .await
    .expect("insert image chapter");
}

#[tokio::test]
async fn collect_and_release_are_idempotent() {
    let p = pool().await;
    assert!(collect(&p, "b1").await.unwrap(), "首次入馆");
    assert!(!collect(&p, "b1").await.unwrap(), "重复入馆是幂等的");

    let b = load_book(&p, "b1").await.unwrap().expect("书目存在");
    assert_eq!(b.tier, TIER_COLLECTION);
    assert!(b.collected_at.is_some());

    assert!(release(&p, "b1").await.unwrap(), "首次移出");
    assert!(!release(&p, "b1").await.unwrap(), "重复移出是幂等的");

    let b = load_book(&p, "b1").await.unwrap().expect("书目仍存在");
    assert_eq!(b.tier, TIER_TEMP);
    assert!(b.collected_at.is_none(), "移出后清空入馆时间");
}

#[tokio::test]
async fn release_cancels_active_job() {
    let p = pool().await;
    collect(&p, "b1").await.unwrap();
    let job = enqueue_job(&p, "b1", "src", 1).await.unwrap();
    assert!(release(&p, "b1").await.unwrap());
    let status: String = sqlx::query_scalar("SELECT status FROM library_jobs WHERE id = ?1")
        .bind(&job)
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(status, JOB_CANCELLED, "移出馆藏应停止后台下载");
}

#[tokio::test]
async fn cleanup_temp_only_removes_expired_temp_books() {
    let p = pool().await;
    let old = now_ms() - 40 * 86_400_000;
    insert_book(&p, "keep-collection", TIER_COLLECTION, Some(old), old).await;
    insert_book(&p, "drop-temp", TIER_TEMP, None, old).await;
    insert_book(&p, "keep-temp", TIER_TEMP, None, now_ms()).await;
    insert_content(&p, "drop-temp", "c1", 100).await;
    insert_content(&p, "keep-collection", "c2", 200).await;
    insert_content(&p, "keep-temp", "c3", 300).await;

    insert_image(&p, "drop-temp", "i1", 50).await;

    let report = cleanup_temp(&p, 30 * 86_400).await.unwrap();
    assert_eq!(report.books, 1);
    assert_eq!(report.contents, 1);
    assert_eq!(report.images, 1);
    assert_eq!(report.bytes, 150);

    assert!(load_book(&p, "drop-temp").await.unwrap().is_none());
    assert!(load_book(&p, "keep-collection").await.unwrap().is_some());
    assert!(load_book(&p, "keep-temp").await.unwrap().is_some());
}

#[tokio::test]
async fn stats_report_layers_and_expired_books() {
    let p = pool().await;
    let old = now_ms() - 40 * 86_400_000;
    insert_book(&p, "c1", TIER_COLLECTION, Some(old), old).await;
    insert_book(&p, "t1", TIER_TEMP, None, old).await;
    insert_book(&p, "t2", TIER_TEMP, None, now_ms()).await;
    insert_content(&p, "c1", "cc1", 10).await;
    insert_content(&p, "t1", "tt1", 20).await;
    insert_image(&p, "c1", "ci1", 300).await;
    insert_image(&p, "t1", "ti1", 40).await;
    // 冗余统计由 rollup 维护；测试里直接写，避免 refresh_rollup 刷新 TTL 时间。
    sqlx::query(
        "UPDATE library_books SET content_bytes = 10, image_bytes = 300 WHERE book_url = 'c1'",
    )
    .execute(&p)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE library_books SET content_bytes = 20, image_bytes = 40 WHERE book_url = 't1'",
    )
    .execute(&p)
    .await
    .unwrap();

    let st = stats(&p, 30 * 86_400).await;
    assert_eq!(st.collection_books, 1);
    assert_eq!(st.temp_books, 2);
    assert_eq!(st.content_rows, 2);
    assert_eq!(st.content_bytes, 30);
    assert_eq!(st.image_rows, 2);
    assert_eq!(st.collection_bytes, 10 + 300);
    assert_eq!(st.temp_bytes, 20 + 40);
    assert_eq!(st.total_bytes, 10 + 20 + 300 + 40);
    assert_eq!(st.temp_expired_books, 1, "只有 t1 过期");
}

#[tokio::test]
async fn clear_content_drops_images_and_keeps_book() {
    let p = pool().await;
    insert_book(&p, "b1", TIER_COLLECTION, Some(now_ms()), now_ms()).await;
    insert_content(&p, "b1", "c1", 100).await;
    insert_image(&p, "b1", "i1", 200).await;

    let r = clear_content(&p, "b1").await.unwrap();
    assert_eq!((r.contents, r.images), (1, 1));
    assert_eq!(r.bytes, 300);
    let b = load_book(&p, "b1").await.unwrap().unwrap();
    assert_eq!(b.content_bytes, 0);
    assert_eq!(b.image_bytes, 0);
    assert_eq!(b.cached_chapters, 0);
}

#[tokio::test]
async fn library_images_save_load_meta_and_ready_marker() {
    let p = pool().await;
    insert_book(&p, "b1", TIER_COLLECTION, Some(now_ms()), now_ms()).await;
    assert!(crate::library_images::load(&p, "i1", "src")
        .await
        .unwrap()
        .is_none());

    let saved = crate::library_images::save(
        &p,
        "b1",
        "i1",
        "src",
        "image/jpeg",
        &[1, 2, 3, 4],
        Some(120),
        Some(80),
    )
    .await
    .unwrap();
    assert_eq!(saved, 4);
    let loaded = crate::library_images::load(&p, "i1", "src")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.data, vec![1, 2, 3, 4]);
    assert_eq!(loaded.mime, "image/jpeg");
    assert_eq!((loaded.width, loaded.height), (Some(120), Some(80)));
    let meta = crate::library_images::meta_of(&p, "i1", "src")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (meta.size_bytes, meta.width, meta.height),
        (4, Some(120), Some(80))
    );

    insert_image_chapter(&p, "b1", "c1", true).await;
    crate::library_images::mark_chapter_pending(&p, "c1", "src")
        .await
        .unwrap();
    assert!(!cached_chapter_urls(&p, "b1", "v1")
        .await
        .unwrap()
        .contains("c1"));
    crate::library_images::mark_chapter_ready(&p, "c1", "src")
        .await
        .unwrap();
    insert_content(&p, "b1", "text", 10).await;
    sqlx::query("UPDATE library_content SET cache_version = 'v1' WHERE book_url = 'b1'")
        .execute(&p)
        .await
        .unwrap();
    let urls = cached_chapter_urls(&p, "b1", "v1").await.unwrap();
    assert!(urls.contains("c1"));
    assert!(urls.contains("text"));
}

#[tokio::test]
async fn cached_chapter_urls_requires_images_ready_for_comics() {
    let p = pool().await;
    insert_content(&p, "b1", "text-ok", 10).await;
    insert_image_chapter(&p, "b1", "img-pending", false).await;
    insert_image_chapter(&p, "b1", "img-ready", true).await;
    sqlx::query("UPDATE library_content SET cache_version = 'v1' WHERE book_url = 'b1'")
        .execute(&p)
        .await
        .unwrap();

    let urls = cached_chapter_urls(&p, "b1", "v1").await.unwrap();
    assert!(urls.contains("text-ok"));
    assert!(urls.contains("img-ready"));
    assert!(!urls.contains("img-pending"), "图片未下完不算已缓存");
}

#[tokio::test]
async fn enqueue_job_is_idempotent_until_finished() {
    let p = pool().await;
    let first = enqueue_job(&p, "b1", "src", 1).await.unwrap();
    let second = enqueue_job(&p, "b1", "src", 1).await.unwrap();
    assert_eq!(first, second, "活动任务不重复创建");

    assert!(cancel_job(&p, &first).await.unwrap());
    let third = enqueue_job(&p, "b1", "src", 1).await.unwrap();
    assert_ne!(first, third, "结束后可重新排队");
}

#[tokio::test]
async fn deferred_job_leaves_the_queue_until_its_time() {
    let p = pool().await;
    let id = enqueue_job(&p, "b1", "src", 1).await.unwrap();
    assert!(next_queued_job(&p).await.unwrap().is_some(), "刚入队可执行");

    let until = now_ms() + 60_000;
    assert!(defer_job(&p, &id, until).await.unwrap());
    assert!(
        next_queued_job(&p).await.unwrap().is_none(),
        "冷却期内不参与调度"
    );

    assert!(defer_job(&p, &id, now_ms() - 1_000).await.unwrap());
    assert!(
        next_queued_job(&p).await.unwrap().is_some(),
        "冷却结束自动回到队列"
    );
}

#[tokio::test]
async fn delete_book_removes_rows_and_cancels_active_jobs() {
    let p = pool().await;
    insert_book(&p, "b1", TIER_TEMP, None, now_ms()).await;
    insert_content(&p, "b1", "c1", 123).await;
    sqlx::query("INSERT INTO library_chapters (book_url, url, \"index\") VALUES ('b1', 'c1', 0)")
        .execute(&p)
        .await
        .unwrap();
    insert_image(&p, "b1", "i1", 456).await;
    let job = enqueue_job(&p, "b1", "src", 1).await.unwrap();

    let r = delete_book(&p, "b1").await.unwrap();
    assert_eq!((r.books, r.chapters, r.contents, r.images), (1, 1, 1, 1));
    assert_eq!(r.bytes, 123 + 456);

    let status: String = sqlx::query_scalar("SELECT status FROM library_jobs WHERE id = ?1")
        .bind(&job)
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(status, JOB_CANCELLED);
}

#[tokio::test]
async fn metadata_patch_writes_clears_and_replaces() {
    let p = pool().await;
    insert_book(&p, "b1", TIER_COLLECTION, Some(now_ms()), now_ms()).await;

    let patch = MetadataPatch {
        language: Some("zh".into()),
        // 纯空白等于清空：管理员用它纠错
        publisher: Some("   ".into()),
        pubdate: Some("2020-01-01".into()),
        series_name: Some("银河系漫游指南".into()),
        series_index: Some(1.5),
        identifiers: Some(vec![
            ("isbn".into(), "9787532754687".into()),
            ("isbn".into(), " 9787532754687 ".into()), // 去重
            ("".into(), "x".into()),                   // 非法项跳过
        ]),
        tags: Some(vec![
            "科幻".into(),
            " 科幻 ".into(), // 去重
            "".into(),       // 跳过
            "经典".into(),
        ]),
    };
    assert!(!patch.is_empty());
    update_metadata(&p, "b1", &patch).await.unwrap();

    let b = load_book(&p, "b1").await.unwrap().unwrap();
    assert_eq!(b.language.as_deref(), Some("zh"));
    assert_eq!(b.publisher, None, "空白文本应落成 NULL");
    assert_eq!(b.pubdate.as_deref(), Some("2020-01-01"));
    assert_eq!(b.series_name.as_deref(), Some("银河系漫游指南"));
    assert_eq!(b.series_index, Some(1.5));

    let extras = load_metadata_extras(&p, "b1").await.unwrap();
    assert_eq!(
        extras.identifiers,
        vec![("isbn".to_string(), "9787532754687".to_string())]
    );
    assert_eq!(extras.tags, vec!["科幻".to_string(), "经典".to_string()]);

    // 缺省字段保持不变；显式空数组表示清空
    let patch2 = MetadataPatch {
        tags: Some(vec![]),
        ..Default::default()
    };
    update_metadata(&p, "b1", &patch2).await.unwrap();
    let b2 = load_book(&p, "b1").await.unwrap().unwrap();
    assert_eq!(b2.language.as_deref(), Some("zh"), "缺省字段应保持不变");
    let extras2 = load_metadata_extras(&p, "b1").await.unwrap();
    assert!(extras2.tags.is_empty());
    assert_eq!(extras2.identifiers.len(), 1, "只清标签，不动标识");

    // 整本删除要清掉扩展表，避免孤儿行
    delete_book(&p, "b1").await.unwrap();
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_tags")
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(left, 0);
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_identifiers")
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[test]
fn empty_metadata_patch_is_detected() {
    assert!(MetadataPatch::default().is_empty());
    assert!(!MetadataPatch {
        tags: Some(vec![]),
        ..Default::default()
    }
    .is_empty());
}

#[tokio::test]
async fn upsert_rollup_creates_book_and_refreshes_stats() {
    let p = pool().await;
    insert_content(&p, "b1", "c1", 100).await;
    insert_content(&p, "b1", "c2", 250).await;

    upsert_rollup(&p, "b1").await.unwrap();
    let b = load_book(&p, "b1").await.unwrap().expect("自动建书目行");
    assert_eq!(b.tier, TIER_TEMP);
    assert_eq!(b.cached_chapters, 2);
    assert_eq!(b.content_bytes, 350);

    insert_content(&p, "b1", "c3", 50).await;
    upsert_rollup(&p, "b1").await.unwrap();
    let b = load_book(&p, "b1").await.unwrap().unwrap();
    assert_eq!(b.cached_chapters, 3);
    assert_eq!(b.content_bytes, 400);
}

#[tokio::test]
async fn adopt_source_fills_only_missing_fields() {
    let p = pool().await;
    insert_book(&p, "b1", TIER_COLLECTION, Some(now_ms()), now_ms()).await;
    let source = crate::entities::BookSource {
        book_source_url: "https://src.example".into(),
        book_source_name: "示例源".into(),
        book_source_type: 2,
        ..Default::default()
    };
    adopt_source(&p, "b1", &source).await.unwrap();
    let b = load_book(&p, "b1").await.unwrap().unwrap();
    assert_eq!(b.origin.as_deref(), Some("https://src.example"));
    assert_eq!(b.origin_name.as_deref(), Some("示例源"));
    assert_eq!(b.book_source_type, 2);

    let other = crate::entities::BookSource {
        book_source_url: "https://other.example".into(),
        book_source_name: "另一个".into(),
        book_source_type: 1,
        ..Default::default()
    };
    adopt_source(&p, "b1", &other).await.unwrap();
    let b = load_book(&p, "b1").await.unwrap().unwrap();
    assert_eq!(
        b.origin.as_deref(),
        Some("https://src.example"),
        "不覆盖已有来源"
    );
    assert_eq!(b.book_source_type, 2);
}
