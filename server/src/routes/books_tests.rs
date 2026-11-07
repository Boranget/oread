//! books 路由单测（move-only 从 books.rs 拆出）。

use super::*;

use sqlx::sqlite::SqlitePoolOptions;

fn mk_content(url: &str, book: &str, idx: i64, body: &str) -> BookContent {
    BookContent {
        book_url: book.into(),
        url: url.into(),
        title: Some(format!("第{idx}章")),
        content: Some(body.into()),
        chapter_index: idx,
        resource_url: None,
        is_local: false,
        ..Default::default()
    }
}

async fn mem_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE book_content (url TEXT NOT NULL, origin TEXT NOT NULL DEFAULT '', \
             book_url TEXT NOT NULL, title TEXT, content TEXT, \
             chapter_index INTEGER NOT NULL DEFAULT 0, resource_url TEXT, \
             is_local INTEGER NOT NULL DEFAULT 0, content_type TEXT NOT NULL DEFAULT 'text', \
             images TEXT, size_bytes INTEGER NOT NULL DEFAULT 0, cache_version TEXT, \
             updated_at TEXT, accessed_at TEXT, PRIMARY KEY (url, origin))",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

// 空正文/零图章节不落缓存；修复前残留的空缓存按未命中处理。
#[tokio::test]
async fn empty_chapter_is_not_cached_and_old_empty_cache_is_miss() {
    let pool = mem_pool().await;
    let empty = mk_content("u1", "b1", 1, "   ");
    save_content_cached(&pool, &empty, 1 << 20).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_content WHERE url='u1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "空文本不应写入缓存");

    // 模拟修复前已经落库的空缓存：读取时应当作未命中并重新抓取。
    sqlx::query(
            "INSERT INTO book_content (url, book_url, title, content, chapter_index, content_type, \
             updated_at, accessed_at) VALUES ('u1','b1','t','   ',1,'text',datetime('now'),datetime('now'))",
        )
        .execute(&pool)
        .await
        .unwrap();
    assert!(load_cached_content(&pool, "u1", "", 7 * 24 * 3600)
        .await
        .unwrap()
        .is_none());

    // 图片章节零图同样不落缓存。
    let img_empty = BookContent {
        url: "u3".into(),
        book_url: "b1".into(),
        content_type: "image".into(),
        content: Some(String::new()),
        images_json: Some("[]".into()),
        ..Default::default()
    };
    save_content_cached(&pool, &img_empty, 1 << 20)
        .await
        .unwrap();
    let n3: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_content WHERE url='u3'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n3, 0);

    // 只有标签没有可见文字（@textNodes 空结果回退 @html 的产物）同样不落库。
    let markup = mk_content("u4", "b1", 4, "<div class=\"text\">   </div>");
    save_content_cached(&pool, &markup, 1 << 20).await.unwrap();
    let n4: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_content WHERE url='u4'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n4, 0, "标签包裹的空白不应写入缓存");

    // 带可见文字的 HTML 片段仍然落库并命中。
    let html = mk_content("u5", "b1", 5, "<p>有正文的段落</p>");
    save_content_cached(&pool, &html, 1 << 20).await.unwrap();
    assert!(load_cached_content(&pool, "u5", "", 7 * 24 * 3600)
        .await
        .unwrap()
        .is_some());

    // 正常正文仍然命中。
    let ok = mk_content("u2", "b1", 2, "正文内容");
    save_content_cached(&pool, &ok, 1 << 20).await.unwrap();
    assert!(load_cached_content(&pool, "u2", "", 7 * 24 * 3600)
        .await
        .unwrap()
        .is_some());
}

// 解析管线指纹不符 → 旧缓存按未命中处理，重建后按当前指纹回写并正常命中。
#[tokio::test]
async fn content_cache_version_mismatch_is_miss_and_rebuilds() {
    let pool = mem_pool().await;
    let body = "当前管线抓到的完整正文";
    let c = mk_content("u1", "b1", 1, body);
    save_content_cached(&pool, &c, 1 << 20).await.unwrap();
    let hit = load_cached_content(&pool, "u1", "", 7 * 24 * 3600)
        .await
        .unwrap()
        .expect("新写入的缓存应命中");
    assert_eq!(hit.cache_version.as_deref(), Some(content_cache_version()));

    // 模拟旧解析管线写下的残缺正文（短但非空，之前只能等 TTL）。
    sqlx::query("UPDATE book_content SET cache_version = 'p1-0000000000000000', content = '半截正文' WHERE url = 'u1'")
            .execute(&pool)
            .await
            .unwrap();
    assert!(
        load_cached_content(&pool, "u1", "", 7 * 24 * 3600)
            .await
            .unwrap()
            .is_none(),
        "指纹不符必须视为未命中"
    );

    // 重建（重新抓取后写入）应回到命中，且指纹为当前版本。
    save_content_cached(&pool, &c, 1 << 20).await.unwrap();
    let rebuilt = load_cached_content(&pool, "u1", "", 7 * 24 * 3600)
        .await
        .unwrap()
        .expect("重建后应命中");
    assert_eq!(rebuilt.content.as_deref(), Some(body));
    assert_eq!(
        rebuilt.cache_version.as_deref(),
        Some(content_cache_version())
    );
}

// 旧记录没有 cache_version（迁移前写入）→ 未命中，首次读取重建后回写指纹。
#[tokio::test]
async fn legacy_content_without_version_is_miss_and_rewritten() {
    let pool = mem_pool().await;
    sqlx::query(
        "INSERT INTO book_content (url, book_url, title, content, chapter_index, content_type, \
             updated_at, accessed_at) VALUES ('legacy','b1','t','迁移前的正文',1,'text',\
             datetime('now'),datetime('now'))",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        load_cached_content(&pool, "legacy", "", 7 * 24 * 3600)
            .await
            .unwrap()
            .is_none(),
        "无指纹的旧记录必须视为未命中"
    );

    let rebuilt = mk_content("legacy", "b1", 1, "迁移前的正文");
    save_content_cached(&pool, &rebuilt, 1 << 20).await.unwrap();
    let hit = load_cached_content(&pool, "legacy", "", 7 * 24 * 3600)
        .await
        .unwrap()
        .expect("回写后应命中");
    assert_eq!(hit.cache_version.as_deref(), Some(content_cache_version()));
}

// 短但完整（<100 字符）的正文必须正常落库并命中，不能按长度误判为空。
#[tokio::test]
async fn short_complete_content_is_cached_and_hit() {
    let pool = mem_pool().await;
    let short = "短章也有完整正文：这一段不足一百字符，但内容完整，缓存必须正常命中。";
    assert!(short.chars().count() < 100);
    assert!(short.chars().count() > 20);
    let c = mk_content("short", "b1", 1, short);
    save_content_cached(&pool, &c, 1 << 20).await.unwrap();
    let hit = load_cached_content(&pool, "short", "", 7 * 24 * 3600)
        .await
        .unwrap()
        .expect("短且非空的正文应命中");
    assert_eq!(hit.content.as_deref(), Some(short));
    assert_eq!(hit.cache_version.as_deref(), Some(content_cache_version()));
}

// 同一章节 URL 在不同来源下必须分区缓存，不能互相串正文。
#[tokio::test]
async fn content_cache_is_partitioned_by_origin() {
    let pool = mem_pool().await;
    let url = "https://mirror.example/chapter/1";
    let mut a = mk_content(url, "book-a", 1, "来源 A 的正文");
    a.origin = "https://source-a.example".into();
    let mut b = mk_content(url, "book-b", 1, "来源 B 的正文");
    b.origin = "https://source-b.example".into();
    save_content_cached(&pool, &a, 1 << 20).await.unwrap();
    save_content_cached(&pool, &b, 1 << 20).await.unwrap();

    let hit_a = load_cached_content(&pool, url, &a.origin, 7 * 24 * 3600)
        .await
        .unwrap()
        .expect("来源 A 应命中");
    assert_eq!(hit_a.content.as_deref(), Some("来源 A 的正文"));
    let hit_b = load_cached_content(&pool, url, &b.origin, 7 * 24 * 3600)
        .await
        .unwrap()
        .expect("来源 B 应命中");
    assert_eq!(hit_b.content.as_deref(), Some("来源 B 的正文"));

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_content WHERE url = ?1")
        .bind(url)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 2, "不同来源各自保留一行缓存");
}

// 搜索调度：两批共用一个总预算，不再给 preferred 单独切分时段
#[test]
fn pipeline_uses_single_total_budget_for_both_batches() {
    // 两批同时进调度：调度时间窗就是整批总预算（不再按 pct 提前截断）。
    assert_eq!(
        crate::req::search_total_budget(crate::req::SearchScope::Curated),
        Duration::from_secs(6)
    );
    assert_eq!(
        crate::req::search_total_budget(crate::req::SearchScope::All),
        Duration::from_secs(10)
    );
}

// 慢源后置：有延迟样本且正常的按延迟升序，无观测居中，近期站点失败垫底。
#[test]
fn slow_and_failing_sources_queue_last() {
    let mk_row = |last: &str, p90: Option<i64>, avg: Option<i64>| {
        let mut h = source_health::HealthRow::default();
        h.last_status = last.to_string();
        h.ttfb_ms_p90 = p90;
        h.avg_latency_ms = avg;
        h
    };
    let mut health = HashMap::new();
    health.insert(
        "https://fast.example".to_string(),
        mk_row(source_health::STATUS_OK, Some(120), Some(90)),
    );
    health.insert(
        "https://slow.example".to_string(),
        mk_row(source_health::STATUS_OK, None, Some(5200)),
    );
    health.insert(
        "https://fail.example".to_string(),
        mk_row(source_health::STATUS_FAIL, Some(800), None),
    );
    let mk = |name: &str| {
        let mut s = crate::entities::BookSource::default();
        s.book_source_url = format!("https://{name}.example");
        s.book_source_name = name.into();
        s
    };
    let mut list = [mk("slow"), mk("fail"), mk("unknown"), mk("fast")];
    list.sort_by_key(|s| health_order_key(health.get(&s.book_source_url)));
    let names: Vec<&str> = list.iter().map(|s| s.book_source_name.as_str()).collect();
    assert_eq!(
        names,
        vec!["fast", "slow", "unknown", "fail"],
        "快源先占槽：正常按延迟升序 → 无观测 → 失败源垫底"
    );
    assert_eq!(health_order_key(None), (1, 0), "无健康行按无观测处理");
}

fn mk_book(name: String, url: String) -> SearchBook {
    let mut b = SearchBook::default();
    b.name = Some(name);
    b.author = Some("作者".into());
    b.origin = Some("http://s".into());
    b.book_url = url;
    b
}

fn stream_job(books: Vec<SearchBook>, pending: usize) -> SearchJob {
    let mut s = crate::entities::BookSource::default();
    s.book_source_url = "http://s".into();
    SearchJob {
        id: "j-stream".into(),
        uid: 1,
        key: "剑来".into(),
        at: Instant::now(),
        created_unix_ms: 0,
        sources: vec![s],
        source_total: 2,
        scope: crate::req::SearchScope::Curated,
        skipped_cooldown: 0,
        cooldown_total: 0,
        coverage: "curated".into(),
        source_mode: "curated".into(),
        scope_filtered: 0,
        curated_total: 0,
        state: Mutex::new(SearchJobState {
            books,
            pending,
            ..Default::default()
        }),
    }
}

// 搜索进行中连翻 3 页：游标不重不漏；迟到结果只有在游标之后才会补进后续页。
#[test]
fn cursor_pagination_no_dup_no_gap_while_streaming() {
    let initial: Vec<SearchBook> = (0..10)
        .map(|i| mk_book(format!("剑来{i}"), format!("u{i}")))
        .collect();
    let job = stream_job(initial, 3);
    let view = job_view(&job);
    let page1: Vec<SearchBook> = view.books.iter().take(3).cloned().collect();
    let mut seen: Vec<String> = page1.iter().map(|b| b.book_url.clone()).collect();
    let mut cursor = page1.last().and_then(cursor_of);

    // 模拟“搜索还没结束，新结果陆续到达”
    {
        let mut st = job.state.lock().unwrap();
        for i in 10..18 {
            st.books
                .push(mk_book(format!("剑来篇{i}"), format!("u{i}")));
        }
    }
    while let Some(cur) = cursor.clone() {
        let after = parse_cursor(&cur).expect("游标可解析");
        let view = job_view(&job);
        let page: Vec<SearchBook> = view
            .books
            .iter()
            .filter(|b| book_after_cursor(b, after))
            .take(3)
            .cloned()
            .collect();
        if page.is_empty() {
            break;
        }
        seen.extend(page.iter().map(|b| b.book_url.clone()));
        cursor = page.last().and_then(cursor_of);
    }

    let final_view = job_view(&job);
    let all: HashSet<String> = final_view
        .books
        .iter()
        .map(|b| b.book_url.clone())
        .collect();
    assert_eq!(seen.len(), all.len(), "逐条不重复");
    for u in &all {
        assert_eq!(
            seen.iter().filter(|x| *x == u).count(),
            1,
            "{u} 应恰好出现一次"
        );
    }
    assert_eq!(seen.len(), 18, "首屏 + 后续页应覆盖全部 18 条");
}

// 首次翻页后冻结快照：后续迟到结果不再改变同一会话的页内容。
#[test]
fn frozen_snapshot_keeps_pages_stable() {
    let books: Vec<SearchBook> = (0..6)
        .map(|i| mk_book(format!("剑来{i}"), format!("u{i}")))
        .collect();
    let job = stream_job(books, 2);
    let frozen = frozen_job_view(&job);
    assert_eq!(frozen.books.len(), 6);
    {
        let mut st = job.state.lock().unwrap();
        for i in 6..10 {
            st.books
                .push(mk_book(format!("剑来篇{i}"), format!("u{i}")));
        }
    }
    let again = frozen_job_view(&job);
    assert_eq!(again.books.len(), 6, "快照冻结后不再吸收新结果");
    assert_eq!(job_view(&job).books.len(), 10, "实时视图仍可看到新结果");
}

// 回归：enabled=0 与 quarantined 都不能进入搜索池（source_total / origin 的源头）。
#[test]
fn disabled_and_quarantined_sources_never_enter_search_pool() {
    let mk = |name: &str, enabled: bool| {
        let mut s = crate::entities::BookSource::default();
        s.book_source_url = format!("https://{name}.example");
        s.book_source_name = name.into();
        s.enabled = enabled;
        s.book_source_type = 0;
        s.search_url = Some(format!("https://{name}.example/search?q={{key}}"));
        s.rule_search = Some(crate::entities::SearchRule::default());
        s
    };
    let mut quarantined = HashSet::new();
    quarantined.insert("https://quarantined.example".to_string());
    let mut health = HashMap::new();
    let mut h = source_health::HealthRow::default();
    h.tier = crate::source_quality::TIER_QUARANTINED.to_string();
    health.insert("https://quality.example".to_string(), h);

    let pool = searchable_sources(
        vec![
            mk("enabled", true),
            mk("disabled", false),
            mk("quarantined", true),
            mk("quality", true),
        ],
        &quarantined,
        &health,
    );
    let names: Vec<&str> = pool.iter().map(|s| s.book_source_name.as_str()).collect();
    assert_eq!(names, vec!["enabled"], "停用/隔离源不得进入搜索池");
}

// 预算预占：并发下必须原子扣减，且耗尽后不再放行
#[test]
fn budget_reservation_is_atomic_and_bounded() {
    let remaining = AtomicI64::new(100);
    assert!(reserve_budget(&remaining, 0));
    assert!(reserve_budget(&remaining, 40));
    assert!(reserve_budget(&remaining, 60));
    assert_eq!(remaining.load(Ordering::SeqCst), 0);
    assert!(!reserve_budget(&remaining, 1));
    // 单章超过总预算也直接拒绝，不出现负数
    let small = AtomicI64::new(10);
    assert!(!reserve_budget(&small, 11));
    assert_eq!(small.load(Ordering::SeqCst), 10);
}

// LIKE 通配符必须按字面量匹配，避免用户输入 `%`/`_` 把全库搜出来
#[test]
fn like_wildcards_are_escaped() {
    assert_eq!(escape_like("100%"), "100\\%");
    assert_eq!(escape_like("a_b"), "a\\_b");
    assert_eq!(escape_like("c:\\x"), "c:\\\\x");
    assert_eq!(escape_like("普通文本"), "普通文本");
}

// 片段：命中位置按字符计、前后 40 字上下文、命中次数正确（含 ASCII 大小写回退）
#[test]
fn snippet_has_context_offset_and_count() {
    let body = format!(
        "{}关键词{}关键词{}",
        "前".repeat(100),
        "中".repeat(50),
        "后".repeat(100)
    );
    let (snippet, offset, count) = find_snippet(&body, "关键词").unwrap();
    assert_eq!(count, 2);
    assert_eq!(offset, 100, "首个命中应位于第 100 个字符");
    assert!(snippet.starts_with('…'), "左侧被截断应有省略号");
    assert!(snippet.ends_with('…'), "右侧被截断应有省略号");
    assert!(snippet.contains("关键词"));

    let ascii = "Hello World, hello rust";
    let (_, offset2, count2) = find_snippet(ascii, "hello").unwrap();
    assert_eq!(offset2, 13, "小写回退也应返回真实字符偏移");
    assert_eq!(count2, 2, "大小写不敏感统计应命中 2 次");
    assert!(find_snippet("无关内容", "关键词").is_none());
}

// 目录/正文共用：use_replace 参数只接受显式布尔，未知值回落到账号设置（None）
#[test]
fn parse_use_replace_accepts_explicit_switch_only() {
    assert_eq!(parse_use_replace(Some("1")), Some(true));
    assert_eq!(parse_use_replace(Some(" TRUE ")), Some(true));
    assert_eq!(parse_use_replace(Some("on")), Some(true));
    assert_eq!(parse_use_replace(Some("0")), Some(false));
    assert_eq!(parse_use_replace(Some("false")), Some(false));
    assert_eq!(parse_use_replace(Some("off")), Some(false));
    assert_eq!(parse_use_replace(None), None);
    assert_eq!(parse_use_replace(Some("")), None);
    assert_eq!(parse_use_replace(Some("yes")), None);
}

// 整本缓存写入路径：同一 URL 二次写入只保留一行，size_bytes 随正文更新
#[tokio::test]
async fn upsert_content_keeps_single_row_and_updates_size() {
    let pool = mem_pool().await;
    let c = mk_content("https://x/1", "https://x/book", 1, "短正文");
    upsert_content(&pool, &c).await.unwrap();
    let c2 = mk_content("https://x/1", "https://x/book", 1, "更长的正文内容");
    upsert_content(&pool, &c2).await.unwrap();
    let (rows, size): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size_bytes),0) FROM book_content")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rows, 1);
    assert_eq!(size, "更长的正文内容".len() as i64);
    let title: String =
        sqlx::query_scalar("SELECT title FROM book_content WHERE url = 'https://x/1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(title, "第1章");
}

async fn meta_pool() -> SqlitePool {
    let pool = mem_pool().await;
    sqlx::query(
        "CREATE TABLE book_meta (book_url TEXT PRIMARY KEY, toc_url TEXT, origin TEXT, \
             origin_name TEXT, name TEXT, author TEXT, kind TEXT, cover_url TEXT, intro TEXT, \
             charset TEXT, book_source_type INTEGER, \"group\" INTEGER, latest_chapter_title TEXT, \
             latest_chapter_time INTEGER, total_chapter_num INTEGER NOT NULL DEFAULT 0, \
             dur_chapter_title TEXT, dur_chapter_index INTEGER, dur_chapter_pos INTEGER, \
             dur_chapter_time INTEGER, word_count INTEGER, can_update INTEGER, \
             \"order\" INTEGER NOT NULL DEFAULT 0, origin_order INTEGER NOT NULL DEFAULT 0, \
             variable TEXT, custom_tag TEXT, custom_cover_url TEXT, custom_intro TEXT)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

// total_chapter_num：目录回写后，详情保存（Book 默认 0）不得把它清零；空目录也不得清零
#[tokio::test]
async fn total_chapter_num_survives_detail_save_and_empty_refresh() {
    let pool = meta_pool().await;
    let url = "https://x/book";
    sync_total_chapter_num(&pool, url, 500).await.unwrap();

    let detail = Book {
        book_url: url.into(),
        name: Some("斗破苍穹".into()),
        ..Default::default()
    };
    save_book_meta(&pool, &detail).await.unwrap();
    let total: i64 =
        sqlx::query_scalar("SELECT total_chapter_num FROM book_meta WHERE book_url = ?1")
            .bind(url)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(total, 500, "详情保存不应把总章节数写回 0");

    sync_total_chapter_num(&pool, url, 0).await.unwrap();
    let total: i64 =
        sqlx::query_scalar("SELECT total_chapter_num FROM book_meta WHERE book_url = ?1")
            .bind(url)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(total, 500, "空目录不应清掉已有总章节数");

    sync_total_chapter_num(&pool, url, 520).await.unwrap();
    let total: i64 =
        sqlx::query_scalar("SELECT total_chapter_num FROM book_meta WHERE book_url = ?1")
            .bind(url)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(total, 520, "目录刷新应更新为最新章节数");
}

// 解析路径只更新可重新抓取的字段：用户/阅读状态字段必须原样保留，
// 且规则返回空值时不能用 NULL 清掉已有书名/作者。
#[allow(clippy::type_complexity)]
#[tokio::test]
async fn detail_save_preserves_user_fields_and_null_fallbacks() {
    let pool = meta_pool().await;
    let url = "https://x/book";
    let initial = Book {
        book_url: url.into(),
        origin: Some("https://src".into()),
        origin_name: Some("测试源".into()),
        name: Some("原书名".into()),
        author: Some("原作者".into()),
        book_source_type: 2,
        group: Some("收藏".into()),
        can_update: true,
        order: 7,
        origin_order: 3,
        variable: Some("{\"v\":1}".into()),
        custom_tag: Some("自定义标签".into()),
        custom_cover_url: Some("https://cover".into()),
        custom_intro: Some("自定义简介".into()),
        dur_chapter_title: Some("第 10 章".into()),
        dur_chapter_index: 10,
        dur_chapter_pos: 42,
        dur_chapter_time: 123,
        ..Default::default()
    };
    save_book_meta(&pool, &initial).await.unwrap();

    // 详情刷新：没有用户字段，名字/封面等也为空（规则缺字段）。
    let parsed = Book {
        book_url: url.into(),
        origin: Some("https://src".into()),
        author: Some("新作者".into()),
        book_source_type: 2,
        ..Default::default()
    };
    save_book_meta(&pool, &parsed).await.unwrap();

    let row: (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        bool,
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT name, author, \"group\", variable, can_update, \"order\", origin_order, \
                 custom_tag, custom_cover_url, custom_intro, dur_chapter_title, origin \
                 FROM book_meta WHERE book_url = ?1",
    )
    .bind(url)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0.as_deref(), Some("原书名"), "空书名不得清掉旧值");
    assert_eq!(row.1.as_deref(), Some("新作者"), "非空作者应更新");
    assert_eq!(row.2.as_deref(), Some("收藏"), "用户分组不得被覆盖");
    assert_eq!(row.3.as_deref(), Some("{\"v\":1}"), "书籍变量不得被清空");
    assert!(row.4, "can_update 不得被默认 false 覆盖");
    assert_eq!(row.5, 7, "书架排序不得被覆盖");
    assert_eq!(row.6, 3, "来源排序不得被覆盖");
    assert_eq!(row.7.as_deref(), Some("自定义标签"));
    assert_eq!(row.8.as_deref(), Some("https://cover"));
    assert_eq!(row.9.as_deref(), Some("自定义简介"));
    assert_eq!(row.10.as_deref(), Some("第 10 章"), "阅读进度不得被覆盖");
    assert_eq!(row.11.as_deref(), Some("https://src"));
}

async fn chapters_pool() -> SqlitePool {
    let pool = mem_pool().await;
    sqlx::query(
        "CREATE TABLE book_chapters (url TEXT PRIMARY KEY, book_url TEXT NOT NULL, \
             title TEXT, is_volume INTEGER NOT NULL DEFAULT 0, base_url TEXT, \
             \"index\" INTEGER NOT NULL DEFAULT 0, is_vip INTEGER NOT NULL DEFAULT 0, \
             is_pay INTEGER NOT NULL DEFAULT 0, resource_url TEXT, tag TEXT, \
             start INTEGER NOT NULL DEFAULT 0, \"end\" INTEGER NOT NULL DEFAULT 0, \
             update_time TEXT, word_count INTEGER NOT NULL DEFAULT 0, updated_at TEXT)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

// 目录元信息往返：is_volume / is_vip / update_time / word_count 落库并读回；分卷分组正确
#[tokio::test]
async fn chapter_meta_roundtrip_and_volume_grouping() {
    let pool = chapters_pool().await;
    let book = "https://x/book";
    let chapters = vec![
        BookChapter {
            url: "volume:https://x/book#0".into(),
            title: Some("第一卷 风起".into()),
            book_url: Some(book.into()),
            is_volume: true,
            index: 0,
            update_time: Some("2026-01-01 10:00".into()),
            ..Default::default()
        },
        BookChapter {
            url: "https://x/c1".into(),
            title: Some("第一章".into()),
            book_url: Some(book.into()),
            is_vip: true,
            is_pay: true,
            index: 1,
            update_time: Some("2026-01-02".into()),
            ..Default::default()
        },
        BookChapter {
            url: "https://x/c2".into(),
            title: Some("第二章".into()),
            book_url: Some(book.into()),
            index: 2,
            ..Default::default()
        },
    ];
    save_chapters(&pool, book, &chapters).await.unwrap();
    let mut loaded = load_cached_chapters(&pool, book, 0).await.unwrap().unwrap();
    assert_eq!(loaded.len(), 3);
    assert!(loaded[0].is_volume);
    assert_eq!(loaded[0].update_time.as_deref(), Some("2026-01-01 10:00"));
    assert!(loaded[1].is_vip);
    assert!(loaded[1].is_pay, "is_pay 应随目录落库并读回");
    assert!(!loaded[2].is_pay);
    assert_eq!(loaded[2].word_count, 0);

    enrich_volumes(&mut loaded);
    assert_eq!(loaded[0].volume_title.as_deref(), Some("第一卷 风起"));
    assert_eq!(loaded[1].volume_title.as_deref(), Some("第一卷 风起"));
    assert_eq!(loaded[2].volume_title.as_deref(), Some("第一卷 风起"));

    // 正文落库回填字数；第 2 章在下一个分卷下
    upsert_content(
        &pool,
        &mk_content("https://x/c1", book, 1, "第一章正文内容"),
    )
    .await
    .unwrap();
    let reloaded = load_cached_chapters(&pool, book, 0).await.unwrap().unwrap();
    assert_eq!(
        reloaded[1].word_count,
        "第一章正文内容".chars().count() as i64
    );
}

// 清理：按书只删该书；不传 book_id 清空全部；bytes_freed 为被删行的 size_bytes 之和
#[tokio::test]
async fn clear_cache_by_book_and_by_user() {
    let pool = mem_pool().await;
    let a = mk_content("https://x/a1", "https://x/a", 1, "aaa");
    let b = mk_content("https://x/a2", "https://x/a", 2, "bbbb");
    let other = mk_content("https://x/b1", "https://x/b", 1, "ccccc");
    for c in [&a, &b, &other] {
        upsert_content(&pool, c).await.unwrap();
    }
    let (deleted, freed) = clear_content_cache(&pool, Some("https://x/a"))
        .await
        .unwrap();
    assert_eq!(deleted, 2);
    assert_eq!(freed, 7);
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_content")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 1, "另一本书的缓存不应被清掉");

    let (deleted_all, freed_all) = clear_content_cache(&pool, None).await.unwrap();
    assert_eq!(deleted_all, 1);
    assert_eq!(freed_all, 5);
    let left2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_content")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left2, 0);
}

// 两段式搜索的读视图：已完成源先可读、去重按权重、done 由 pending 决定
#[test]
fn job_view_dedupes_by_weight_and_reports_progress() {
    let mut hi = crate::entities::BookSource::default();
    hi.book_source_url = "http://hi".into();
    hi.weight = 10;
    let mut lo = crate::entities::BookSource::default();
    lo.book_source_url = "http://lo".into();
    lo.weight = 0;

    let mut a = SearchBook::default();
    a.book_url = "http://hi/1".into();
    a.origin = Some("http://hi".into());
    a.name = Some("剑来".into());
    a.author = Some("烽火戏诸侯".into());
    let mut b = SearchBook::default();
    b.book_url = "http://lo/1".into();
    b.origin = Some("http://lo".into());
    b.name = Some("剑来".into());
    b.author = Some("烽火戏诸侯".into());

    let job = SearchJob {
        id: "j-test".into(),
        uid: 1,
        key: "剑来".into(),
        at: Instant::now(),
        created_unix_ms: 0,
        sources: vec![hi, lo],
        source_total: 2,
        scope: crate::req::SearchScope::Curated,
        skipped_cooldown: 0,
        cooldown_total: 0,
        coverage: "curated".into(),
        source_mode: "curated".into(),
        scope_filtered: 0,
        curated_total: 0,
        state: Mutex::new(SearchJobState {
            books: vec![a, b],
            errors: vec![],
            finished: 1,
            pending: 1,
            ..Default::default()
        }),
    };
    let view = job_view(&job);
    assert_eq!(view.books.len(), 1, "同名同作者应去重为一条");
    assert_eq!(
        view.books[0].origin.as_deref(),
        Some("http://hi"),
        "权重高的源应排在前面"
    );
    assert!(view.errors.is_empty());
    assert_eq!((view.finished, view.pending), (1, 1));
    assert!(!view.done, "还有源未完成时 done 必须为 false");

    job.state.lock().unwrap().pending = 0;
    let view = job_view(&job);
    assert!(view.done);
    assert_eq!(view.pending, 0);
}

// curated：只保留体检精选集合；集合为空时退回全量（退化路径，避免 0 命中）
#[test]
fn curated_scope_filters_by_bench_set_and_falls_back_to_all() {
    use crate::req::SearchScope;
    let candidates = vec![
        src("精选", "http://curated", 1, true),
        src("非精选", "http://observing", 1, true),
    ];
    let set: std::collections::HashSet<String> =
        ["http://curated".to_string()].into_iter().collect();

    let curated = filter_sources_by_scope(candidates.clone(), Some(&set), SearchScope::Curated);
    assert_eq!(curated.len(), 1, "curated 只跑精选源");
    assert_eq!(curated[0].book_source_url, "http://curated");

    let all = filter_sources_by_scope(candidates.clone(), Some(&set), SearchScope::All);
    assert_eq!(all.len(), 2, "all 保留全量源");

    // 无精选集合（冷启动）：先按 preferred 退化，仍为空再退全量（由调用方处理）
    let mut health = HashMap::new();
    let mut h = source_health::HealthRow::default();
    h.tier = crate::source_quality::TIER_PREFERRED.to_string();
    health.insert("http://curated".to_string(), h);
    let preferred = preferred_sources(candidates.clone(), &health);
    assert_eq!(preferred.len(), 1, "冷启动先退 preferred");
    let fallback = filter_sources_by_scope(candidates, None, SearchScope::Curated);
    assert_eq!(fallback.len(), 2, "filter 本身不做 tier 退化");
}

// 统一源过滤：默认 all；显式 source_ids 跳过 curated 收缩（非优选源不能被静默丢弃）；
// 计数口径与 /api/sources 的 searchable 一致（禁用/缺规则的不进候选）。
#[tokio::test]
async fn explicit_source_ids_skip_curated_and_searchable_gate_matches() {
    use crate::req::SearchScope;
    // 惰性内存池：健康/隔离表缺失时按空集合处理，正好测“无健康行=observing”。
    let pool = SqlitePool::connect_lazy("sqlite::memory:").unwrap();
    let mut no_rule = src("缺规则", "http://norule", 1, true);
    no_rule.rule_search = None;
    let sources = vec![
        src("优选", "http://preferred", 1, true),
        src("观察", "http://observing", 1, true),
        src("禁用", "http://off", 1, false),
        no_rule,
    ];

    let all = resolve_search_sources(
        &pool,
        sources.clone(),
        &[],
        SearchScope::All,
        false,
        CuratedMissingFallback::All,
    )
    .await;
    assert_eq!(all.source_mode, "all");
    assert_eq!(all.scope, SearchScope::All);
    assert_eq!(all.active.len(), 2, "禁用/缺规则源不可搜索");
    assert_eq!(all.scope_filtered, 0);

    let user = resolve_search_sources(
        &pool,
        sources,
        &["http://observing".to_string()],
        SearchScope::Curated,
        false,
        CuratedMissingFallback::All,
    )
    .await;
    assert_eq!(user.source_mode, "user");
    assert_eq!(user.scope, SearchScope::All, "显式源跳过 curated 收缩");
    assert_eq!(user.active.len(), 1);
    assert_eq!(
        user.active[0].book_source_url, "http://observing",
        "非优选源必须真实进入执行集合"
    );
    assert!(user.ignored.is_empty());
    assert_eq!(user.scope_filtered, 0, "显式源模式不存在 scope 过滤");
}

// `/k` 语义：精选集合缺失时只退 preferred 层（不退全量）；`/api` 语义仍退全量。
#[tokio::test]
async fn curated_missing_falls_back_to_preferred_for_kindle() {
    use crate::req::SearchScope;
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    // 精选表留空（curated_total=0）；健康表给出 preferred/observing 两档。
    sqlx::query(
        "CREATE TABLE source_curated (source_url TEXT PRIMARY KEY, \
             curated INTEGER NOT NULL DEFAULT 0, meta_json TEXT)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
            "CREATE TABLE source_health ( \
             source_url TEXT PRIMARY KEY, last_checked_at INTEGER, \
             last_status TEXT NOT NULL DEFAULT 'never', last_error TEXT, \
             consecutive_failures INTEGER NOT NULL DEFAULT 0, last_ok_at INTEGER, \
             cooldown_until INTEGER, avg_latency_ms INTEGER, check_total INTEGER NOT NULL DEFAULT 0, \
             tier TEXT NOT NULL DEFAULT 'observing', score INTEGER NOT NULL DEFAULT 50, \
             evidence_json TEXT, last_eval_at INTEGER, \
             search_count INTEGER NOT NULL DEFAULT 0, hit_count INTEGER NOT NULL DEFAULT 0, \
             parse_ok_count INTEGER NOT NULL DEFAULT 0, \
             fail_site_count INTEGER NOT NULL DEFAULT 0, \
             fail_engine_count INTEGER NOT NULL DEFAULT 0, \
             ttfb_ms_p90 INTEGER, ttfb_samples_json TEXT, \
             promote_streak INTEGER NOT NULL DEFAULT 0, \
             demote_streak INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO source_health (source_url, tier) \
             VALUES ('http://preferred', 'preferred'), ('http://observing', 'observing')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let sources = vec![
        src("优选", "http://preferred", 1, true),
        src("观察", "http://observing", 1, true),
    ];

    let api = resolve_search_sources(
        &pool,
        sources.clone(),
        &[],
        SearchScope::Curated,
        true,
        CuratedMissingFallback::All,
    )
    .await;
    assert_eq!(api.source_mode, "curated_fallback");
    assert_eq!(api.scope, SearchScope::All, "/api 精选缺失退全量");
    assert_eq!(api.active.len(), 2);
    assert_eq!(api.curated_total, 0);

    let kindle = resolve_search_sources(
        &pool,
        sources,
        &[],
        SearchScope::Curated,
        true,
        CuratedMissingFallback::Preferred,
    )
    .await;
    assert_eq!(kindle.source_mode, "curated_fallback");
    assert_eq!(kindle.scope, SearchScope::Curated, "/k 精选缺失不退全量");
    assert_eq!(kindle.active.len(), 1, "只跑 preferred 层");
    assert_eq!(kindle.active[0].book_source_url, "http://preferred");
    assert_eq!(kindle.curated_total, 0);
}

// 全新部署：精选与 preferred 都为空时，`/k` 退到最后一层「全部可搜索源」；
// 带 login_url 的源不参与，不算进 source_total。
#[tokio::test]
async fn preferred_empty_falls_back_to_all_and_skips_login_sources() {
    use crate::req::SearchScope;
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let mut login_only = src("需登录", "http://login", 1, true);
    login_only.login_url = Some("http://login/login".into());
    let sources = vec![src("正常源", "http://normal", 1, true), login_only];

    let plan = resolve_search_sources(
        &pool,
        sources,
        &[],
        SearchScope::Curated,
        true,
        CuratedMissingFallback::Preferred,
    )
    .await;
    assert_eq!(plan.source_mode, "all_fallback", "preferred 为空退全部源");
    assert_eq!(plan.scope, SearchScope::All);
    assert_eq!(plan.source_total, 1, "login_url 源不参与可搜索统计");
    assert_eq!(plan.active.len(), 1);
    assert_eq!(plan.active[0].book_source_url, "http://normal");
    assert_eq!(plan.curated_total, 0);
}

// 0 源（全新空库）不 panic：返回空计划，source_total=0，页面据此显示明确文案。
#[tokio::test]
async fn zero_sources_returns_empty_plan_without_panic() {
    use crate::req::SearchScope;
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let plan = resolve_search_sources(
        &pool,
        Vec::new(),
        &[],
        SearchScope::Curated,
        false,
        CuratedMissingFallback::Preferred,
    )
    .await;
    assert_eq!(plan.source_total, 0);
    assert!(plan.active.is_empty());
    assert_eq!(plan.curated_total, 0);
}

// 空库/冷启动：0 源时各层退化链都返回空集合，不 panic、不依赖库规模。
#[test]
fn empty_source_pool_degrades_safely() {
    use crate::req::SearchScope;
    use std::collections::{HashMap, HashSet};

    let empty: Vec<crate::entities::BookSource> = vec![];
    assert!(
        searchable_sources(empty.clone(), &HashSet::new(), &HashMap::new()).is_empty(),
        "空库的搜索池必须为空"
    );
    assert!(preferred_sources(empty.clone(), &HashMap::new()).is_empty());
    assert!(filter_sources_by_scope(empty, None, SearchScope::Curated).is_empty());
}

// 需登录源不进搜索池；去掉登录字段后同一加载结果自动恢复（纯运行时派生）。
#[test]
fn login_source_is_excluded_from_search_pool() {
    use std::collections::{HashMap, HashSet};

    let mut login = src("登录源", "http://login", 1, true);
    login.login_url = Some("http://login/login".into());
    let open = src("开放源", "http://open", 1, true);

    let pool = searchable_sources(
        vec![login.clone(), open.clone()],
        &HashSet::new(),
        &HashMap::new(),
    );
    assert_eq!(pool.len(), 1, "需登录源不应进入搜索池");
    assert_eq!(pool[0].book_source_url, "http://open");

    let mut restored = login;
    restored.login_url = None;
    let pool = searchable_sources(vec![restored, open], &HashSet::new(), &HashMap::new());
    assert_eq!(pool.len(), 2, "去掉 loginUrl 后应自动回到搜索池");
}

// phase：pending>0 → first（partial=true），pending==0 → final
#[test]
fn phase_follows_pending() {
    assert_eq!(search_phase(3), "first");
    assert_eq!(search_phase(0), "final");
    let job = test_job("phase", 0, 3);
    let view = job_view(&job);
    assert!(!view.done);
    assert_eq!(view.pending, 3);
    assert_eq!(search_phase(view.pending), "first");
    assert_eq!(view.finished, 0, "没有任何源完成时 finished=0");
    job.state.lock().unwrap().pending = 0;
    let view = job_view(&job);
    assert!(view.done);
    assert_eq!(search_phase(view.pending), "final");
}

// 计数器算术关系：attempted <= scheduled <= source_total；pending = total - scheduled
#[test]
fn source_counters_satisfy_invariant() {
    let job = test_job("counters", 0, 3);
    {
        let mut st = job.state.lock().unwrap();
        st.scheduled = 2;
        st.attempted = 1;
        st.finished = 2;
        st.pending = 1;
    }
    let view = job_view(&job);
    assert!(
        view.attempted <= view.scheduled,
        "attempted 不能大于 scheduled"
    );
    assert!(
        view.scheduled <= job.source_total,
        "scheduled 不能大于 total"
    );
    assert_eq!(
        job.source_total - view.scheduled,
        1,
        "pending_sources = total - scheduled"
    );
}

// 排队到 deadline 仍未拿到槽：计入 scheduled/finished，但不算 attempted
#[tokio::test]
async fn queued_but_never_executed_is_not_attempted() {
    let job = test_job("queue", 0, 1);
    let past = tokio::time::Instant::now() - Duration::from_secs(1);
    // 排队阶段不会真正执行任务，这里给一个惰性池即可满足参数类型。
    let book_pool = SqlitePool::connect_lazy("sqlite::memory:").unwrap();
    let store = Arc::new(web_book::BookStore::new());
    let outcomes = run_search_batch(
        &job,
        vec![src("排队源", "http://queue.example", 1, true)],
        "书",
        past,
        Duration::from_millis(100),
        &HashMap::new(),
        crate::req::SearchScope::Curated,
        &book_pool,
        &store,
    )
    .await;
    assert!(outcomes
        .iter()
        .all(|o| o.status != source_health::STATUS_OK));
    assert_eq!(
        job.state.lock().unwrap().attempted,
        0,
        "未拿到槽不算 attempted"
    );
    let view = job_view(&job);
    assert_eq!(view.scheduled, 1);
    assert_eq!(view.attempted, 0, "排队未执行不计入 attempted");
    assert_eq!(view.finished, 1);
    assert_eq!(view.pending, 0);
    assert!(outcomes.is_empty(), "排队未执行的源不写健康记录");
}

// 回归：legacy `/api/search` 走的 `search_all_with_errors_budgeted` 不做 tier 过滤，
// 非 preferred 源照常参与并被解析（默认行为/返回结构不变）。
#[tokio::test]
async fn legacy_aggregate_path_keeps_all_sources() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let srv = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = srv.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = srv.accept().await {
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await;
            let body = r#"<div class="item"><a href="/b/1">测试书</a></div>"#;
            let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let mut s = crate::entities::BookSource::default();
    s.enabled = true;
    s.book_source_url = format!("http://{addr}");
    s.book_source_name = "observing-only".into();
    s.search_url = Some(format!("http://{addr}/s?q={{{{key}}}}"));
    s.rule_search = Some(crate::entities::SearchRule {
        book_list: Some("class.item".into()),
        name: Some("a@text".into()),
        book_url: Some("a@href".into()),
        ..Default::default()
    });

    // 没有健康行（等价于 observing）：legacy 路径仍要搜它
    let out =
        web_book::search_all_with_errors_budgeted(&[s], "书", 1, Duration::from_millis(800)).await;
    assert!(
        out.errors.is_empty(),
        "非 preferred 源不应被过滤: {:?}",
        out.errors
    );
    assert_eq!(out.books.len(), 1);
    assert_eq!(out.books[0].name.as_deref(), Some("测试书"));
}

fn test_job(id: &str, age_secs: u64, pending: usize) -> Arc<SearchJob> {
    Arc::new(SearchJob {
        id: id.into(),
        uid: 7,
        key: "k".into(),
        at: Instant::now()
            .checked_sub(Duration::from_secs(age_secs))
            .unwrap(),
        created_unix_ms: 0,
        sources: Vec::new(),
        source_total: pending,
        scope: crate::req::SearchScope::Curated,
        skipped_cooldown: 0,
        cooldown_total: 0,
        coverage: "curated".into(),
        source_mode: "curated".into(),
        scope_filtered: 0,
        curated_total: 0,
        state: Mutex::new(SearchJobState {
            pending,
            ..Default::default()
        }),
    })
}

/// 已完成的旧任务超过上限时，只淘汰已完成的最旧任务；进行中的新 job 必须保留。
#[test]
fn prune_jobs_evicts_completed_but_keeps_active() {
    let mut list: Vec<Arc<SearchJob>> = (0..SEARCH_JOB_MAX)
        .map(|i| test_job(&format!("done{i}"), (SEARCH_JOB_MAX - i) as u64, 0))
        .collect();
    // 新 job 仍在进行中（pending=5）
    let active = test_job("active", 0, 5);
    list.push(active.clone());

    prune_jobs(&mut list);

    assert!(list.len() <= SEARCH_JOB_MAX, "应回到已完成任务上限");
    assert_eq!(
        list.iter().filter(|j| j.id.starts_with("done")).count(),
        SEARCH_JOB_MAX - 1
    );
    assert!(
        list.iter().any(|j| j.id == active.id),
        "进行中的 job 不能被完成后淘汰误杀"
    );
    assert!(
        !list.iter().any(|j| j.id == "done0"),
        "应淘汰最旧的已完成任务"
    );
}

/// 并发 4~64 个进行中的 job 时，一个都不应被淘汰（旧实现按总数 8 直接误杀）。
#[test]
fn prune_jobs_keeps_all_active_under_hard_cap() {
    let mut list: Vec<Arc<SearchJob>> = (0..SEARCH_JOB_MAX_ACTIVE - 1)
        .map(|i| test_job(&format!("run{i}"), 10, 3))
        .collect();
    let fresh = test_job("fresh", 0, 3);
    list.push(fresh.clone());

    prune_jobs(&mut list);

    assert_eq!(list.len(), SEARCH_JOB_MAX_ACTIVE, "硬上限内全部保留");
    assert!(list.iter().any(|j| j.id == fresh.id));
}

/// 超过硬上限才淘汰最旧的活跃任务（兜底防内存膨胀）。
#[test]
fn prune_jobs_evicts_active_only_beyond_hard_cap() {
    let mut list: Vec<Arc<SearchJob>> = (0..SEARCH_JOB_MAX_ACTIVE + 1)
        .map(|i| test_job(&format!("run{i}"), if i == 0 { 11 } else { 10 }, 2))
        .collect();
    prune_jobs(&mut list);
    assert_eq!(list.len(), SEARCH_JOB_MAX_ACTIVE);
    assert!(
        !list.iter().any(|j| j.id == "run0"),
        "最旧的活跃任务被硬上限淘汰"
    );
}

fn src(name: &str, url: &str, weight: i64, enabled: bool) -> crate::entities::BookSource {
    crate::entities::BookSource {
        book_source_url: url.into(),
        book_source_name: name.into(),
        weight,
        enabled,
        search_url: Some("https://x/s?q=".into()),
        rule_search: Some(Default::default()),
        ..Default::default()
    }
}

/// `q` 是 `key` 的别名；两者都在时以 `key` 为准，缺两者为空（由 handler 返回 400）。
#[test]
fn search_keyword_accepts_q_alias() {
    let only_q = SearchQ {
        key: None,
        q: Some(" 剑来 ".into()),
        source_ids: None,
        include_cooled: None,
        scope: None,
        page: None,
        page_size: None,
    };
    assert_eq!(only_q.keyword(), "剑来");

    let both = SearchQ {
        key: Some("斗破".into()),
        q: Some("剑来".into()),
        source_ids: None,
        include_cooled: None,
        scope: None,
        page: None,
        page_size: None,
    };
    assert_eq!(both.keyword(), "斗破");

    let none = SearchQ {
        key: None,
        q: None,
        source_ids: None,
        include_cooled: None,
        scope: None,
        page: None,
        page_size: None,
    };
    assert_eq!(none.keyword(), "");
}

#[test]
fn search_source_ids_filter_and_report_ignored() {
    // 排序：权重降序 → custom_order 升序 → 名称
    let mut all = vec![
        src("乙", "http://b", 1, true),
        src("甲", "http://a", 2, true),
        src("丙", "http://c", 0, false),
    ];
    crate::routes::sources::sort_sources(&mut all);
    let enabled: Vec<_> = all
        .into_iter()
        .filter(|x| {
            x.enabled
                && crate::entities::is_supported_source_type(x.book_source_type)
                && x.rule_search.is_some()
                && x.search_url.is_some()
        })
        .collect();
    assert_eq!(enabled[0].book_source_url, "http://a");

    // 数字按排序后的 1-based 序号：1,2 → 前两个启用源
    let (sel, ign) = select_sources(&enabled, &parse_source_ids(Some("1,2")));
    assert_eq!(sel.len(), 2);
    assert_eq!(sel[0].book_source_url, "http://a");
    assert_eq!(sel[1].book_source_url, "http://b");
    assert!(ign.is_empty());

    // 精确 URL id 优先；重复项去重；禁用源与不存在的 id 进入 ignored
    let (sel, ign) = select_sources(
        &enabled,
        &parse_source_ids(Some("http://b,http://b,http://c,999,abc")),
    );
    assert_eq!(sel.len(), 1);
    assert_eq!(sel[0].book_source_url, "http://b");
    assert_eq!(
        ign,
        vec!["http://c".to_string(), "999".to_string(), "abc".to_string()]
    );

    // 未传 source_ids 时保持原有“全部启用源”语义
    let (sel, ign) = select_sources(&enabled, &parse_source_ids(None));
    assert_eq!(sel.len(), 2);
    assert!(ign.is_empty());
}

// 搜索任务归属：别人的 job_id 与不存在都返回 404，防越权读取与存在性探测。
#[tokio::test]
async fn search_result_rejects_other_users_job() {
    use axum::response::IntoResponse;

    // test_job 默认 uid=7；先注入一条真实存在的任务。
    let job = test_job("owned-by-7", 0, 0);
    job_registry().lock().unwrap().push(job);
    let state = || {
        State(AppState {
            shared: sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap(),
            cfg: crate::config::Config::from_env(),
            user_pools: Default::default(),
            fetcher: std::sync::Arc::new(crate::web_book::ReqFetcher),
        })
    };
    let q = |job_id: &str| {
        Query(JobResultQ {
            job_id: job_id.to_string(),
            page: Some(1),
            page_size: None,
            cursor: None,
        })
    };

    let bob = AuthUser {
        id: 2,
        username: "bob".into(),
        role: "user".into(),
    };
    match search_result(state(), bob, q("owned-by-7")).await {
        Ok(_) => panic!("别人的搜索任务不应可读"),
        Err(e) => assert_eq!(
            e.into_response().status(),
            axum::http::StatusCode::NOT_FOUND
        ),
    }

    let owner = AuthUser {
        id: 7,
        username: "owner".into(),
        role: "user".into(),
    };
    let ok = search_result(state(), owner, q("owned-by-7"))
        .await
        .expect("任务创建者应可读");
    assert_eq!(ok.into_response().status(), axum::http::StatusCode::OK);
}
