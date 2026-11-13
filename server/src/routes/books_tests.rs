//! books 路由单测（move-only 从 books.rs 拆出）。

use super::*;

use sqlx::sqlite::SqlitePoolOptions;

/// 单测用的空查询先验：等价于「只看全局健康证据」，不依赖数据库。
fn test_prior() -> crate::search_priority::SearchPrior {
    crate::search_priority::SearchPrior::empty(crate::query_class::QueryClass::Normal)
}

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
        Duration::from_secs(40)
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
    list.sort_by_key(|s| test_prior().priority(&s.book_source_url, health.get(&s.book_source_url)));
    let names: Vec<&str> = list.iter().map(|s| s.book_source_name.as_str()).collect();
    assert_eq!(
        names,
        vec!["fast", "slow", "unknown", "fail"],
        "快源先占槽：正常按延迟升序 → 无观测 → 失败源垫底"
    );
    assert_eq!(
        test_prior().priority("https://none.example", None),
        (1, 0),
        "无健康行按无观测处理"
    );
}

// P1：同样延迟下，产出率（书名匹配命中）高的先入队；近期失败仍垫底。
#[test]
fn health_order_prefers_yield_at_same_latency() {
    let mk_row = |url: &str, last: &str, avg: i64, ok: i64, n: i64| {
        let mut h = source_health::HealthRow::default();
        h.source_url = url.to_string();
        h.last_status = last.to_string();
        h.avg_latency_ms = Some(avg);
        h.yield_ok_count = ok;
        h.search_count = n;
        h
    };
    let mut health = HashMap::new();
    health.insert(
        "https://barren.example".to_string(),
        mk_row(
            "https://barren.example",
            source_health::STATUS_OK,
            1000,
            0,
            20,
        ),
    );
    health.insert(
        "https://fruitful.example".to_string(),
        mk_row(
            "https://fruitful.example",
            source_health::STATUS_OK,
            1000,
            15,
            20,
        ),
    );
    health.insert(
        "https://failed.example".to_string(),
        mk_row(
            "https://failed.example",
            source_health::STATUS_FAIL,
            500,
            20,
            20,
        ),
    );
    let mk = |name: &str| {
        let mut s = crate::entities::BookSource::default();
        s.book_source_url = format!("https://{name}.example");
        s.book_source_name = name.into();
        s
    };
    let mut list = [mk("barren"), mk("failed"), mk("fruitful")];
    list.sort_by_key(|s| test_prior().priority(&s.book_source_url, health.get(&s.book_source_url)));
    let names: Vec<&str> = list.iter().map(|s| s.book_source_name.as_str()).collect();
    assert_eq!(
        names,
        vec!["fruitful", "barren", "failed"],
        "同延迟按产出率降序；近期失败源即使产出高也垫底"
    );
}

// legacy / aggregate / /k 也必须按健康顺序入队，否则同一关键词会因入口不同而结果不一致。
#[test]
fn all_search_entrypoints_share_health_ordering() {
    let mk_row = |last: &str, p90: Option<i64>| {
        let mut h = source_health::HealthRow::default();
        h.last_status = last.to_string();
        h.ttfb_ms_p90 = p90;
        h
    };
    let mut health = HashMap::new();
    health.insert(
        "https://fast.example".to_string(),
        mk_row(source_health::STATUS_OK, Some(100)),
    );
    health.insert(
        "https://fail.example".to_string(),
        mk_row(source_health::STATUS_FAIL, Some(50)),
    );
    let mk = |name: &str| {
        let mut s = crate::entities::BookSource::default();
        s.book_source_url = format!("https://{name}.example");
        s.book_source_name = name.into();
        s
    };
    let list = order_by_health(
        vec![mk("fail"), mk("unknown"), mk("fast")],
        &health,
        &test_prior(),
    );
    let names: Vec<&str> = list.iter().map(|s| s.book_source_name.as_str()).collect();
    assert_eq!(names, vec!["fast", "unknown", "fail"]);
}

// 搜索超时是「未知」而非「站点已死」：记 partial、不计入搜索统计，
// 否则连续 3 次超时就会把源硬隔离，源池越搜越小，最后所有搜索都是 0 结果。
#[test]
fn search_timeout_is_partial_and_not_counted() {
    let (status, kind, usage) =
        source_health::classify_search_failure("搜索超时（单源 5s 预算）", false);
    assert_eq!(status, source_health::STATUS_PARTIAL);
    assert_eq!(kind, "timeout");
    assert!(!usage, "未完成的请求不能算一次 0 命中");

    // 底层请求超时（含在 Done(Err) 里的文案）同样按超时处理
    let (status, kind, usage) = source_health::classify_search_failure(
        "请求超时（15s 预算）: https://x.example/search",
        false,
    );
    assert_eq!(status, source_health::STATUS_PARTIAL);
    assert_eq!(kind, "timeout");
    assert!(!usage);

    // 明确的站点故障仍然计入失败，避免真死站永远留在池子里
    let (status, kind, usage) =
        source_health::classify_search_failure("连接失败: https://x.example", false);
    assert_eq!(status, source_health::STATUS_FAIL);
    assert_eq!(kind, "site");
    assert!(usage);

    // 引擎故障记 partial 但计入统计：留证据，不判死站点
    let (status, kind, usage) = source_health::classify_search_failure("选择器解析失败", true);
    assert_eq!(status, source_health::STATUS_PARTIAL);
    assert_eq!(kind, "engine");
    assert!(usage);
}

fn mk_book(name: String, url: String) -> SearchBook {
    let mut b = SearchBook::default();
    b.name = Some(name);
    b.author = Some("作者".into());
    b.origin = Some("http://s".into());
    b.book_url = url;
    b
}

// 行表分页：搜索进行中逐页读取，游标不重不漏；迟到结果只从游标之后补进后续页。
#[tokio::test]
async fn cursor_pagination_no_dup_no_gap_while_streaming() {
    let pool = jobs_pool().await;
    let job = test_job_for(1, "j-stream", 0, 2);
    persist_started(&pool, &job).await.unwrap();
    let mk = |i: usize| mk_book(format!("剑来{i}"), format!("u{i}"));

    let first: Vec<SearchBook> = (0..10).map(mk).collect();
    books_search_books::insert_batch(&pool, &job.id, "剑来", 0, first)
        .await
        .unwrap();

    let mut seen: Vec<String> = Vec::new();
    let mut cursor = None;
    loop {
        let (page, has_more) = books_search_books::load_page(&pool, &job.id, None, cursor, 3)
            .await
            .unwrap();
        seen.extend(page.iter().map(|b| b.book_url.clone()));
        cursor = page
            .last()
            .and_then(cursor_of)
            .and_then(|c| parse_cursor(&c));
        if !has_more {
            break;
        }
    }
    assert_eq!(seen.len(), 10, "首轮应覆盖全部已入库结果");

    // 模拟“搜索还没结束，新结果陆续到达”。
    let more: Vec<SearchBook> = (10..18).map(mk).collect();
    books_search_books::insert_batch(&pool, &job.id, "剑来", 0, more)
        .await
        .unwrap();

    while let Some(after) = cursor {
        let (page, has_more) = books_search_books::load_page(&pool, &job.id, None, Some(after), 3)
            .await
            .unwrap();
        seen.extend(page.iter().map(|b| b.book_url.clone()));
        cursor = page
            .last()
            .and_then(cursor_of)
            .and_then(|c| parse_cursor(&c));
        if !has_more {
            break;
        }
    }

    let all: HashSet<String> = seen.iter().cloned().collect();
    assert_eq!(seen.len(), all.len(), "逐条不重复");
    assert_eq!(seen.len(), 18, "首屏 + 后续页应覆盖全部 18 条");
}

// 首次翻页后冻结首屏边界：后台新入库的结果不会挤动已翻过的页。
#[tokio::test]
async fn frozen_boundary_keeps_pages_stable() {
    let pool = jobs_pool().await;
    let job = test_job_for(1, "j-frozen", 0, 2);
    persist_started(&pool, &job).await.unwrap();
    let mk = |i: usize| mk_book(format!("剑来{i}"), format!("u{i}"));

    books_search_books::insert_batch(&pool, &job.id, "剑来", 0, (0..6).map(mk).collect())
        .await
        .unwrap();
    let frozen = books_search_books::freeze(&pool, &job.id).await.unwrap();
    assert_eq!(frozen, 6, "边界 = 已入库条数");

    books_search_books::insert_batch(&pool, &job.id, "剑来", 0, (6..10).map(mk).collect())
        .await
        .unwrap();
    let (page, _) = books_search_books::load_page(&pool, &job.id, Some(frozen), None, 30)
        .await
        .unwrap();
    assert_eq!(page.len(), 6, "边界冻结后不再吸收新结果");
    let (live, _) = books_search_books::load_page(&pool, &job.id, None, None, 30)
        .await
        .unwrap();
    assert_eq!(live.len(), 10, "不传边界时仍可看到全部结果");

    let advanced = books_search_books::advance_freeze(&pool, &job.id)
        .await
        .unwrap();
    assert_eq!(advanced, 10, "「显示更多结果」推进到当前最大序号");
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

// 行表去重：同书多源只合并来源，分数与序号保持首次入库时的值。
#[tokio::test]
async fn result_rows_dedupe_by_book_and_keep_first_score() {
    use crate::web_book::dedupe_key;

    let pool = jobs_pool().await;
    let job = test_job_for(1, "j-dedupe", 0, 2);
    persist_started(&pool, &job).await.unwrap();

    let mut lo = SearchBook::default();
    lo.book_url = "http://lo/1".into();
    lo.origin = Some("http://lo".into());
    lo.origin_name = Some("低权重源".into());
    lo.name = Some("剑来".into());
    lo.author = Some("烽火戏诸侯".into());
    let mut hi = lo.clone();
    hi.book_url = "http://hi/1".into();
    hi.origin = Some("http://hi".into());
    hi.origin_name = Some("高权重源".into());
    assert_eq!(dedupe_key(&lo), dedupe_key(&hi), "同名同作者是同一本书");

    // 低权重源先入库：分数按首次入库时的 1 个来源计算。
    books_search_books::insert_batch(&pool, &job.id, "剑来", 0, vec![lo])
        .await
        .unwrap();
    let (first, _) = books_search_books::load_page(&pool, &job.id, None, None, 10)
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    let first_score = first[0].rank_score;
    assert_eq!(first[0].origins, vec!["低权重源"]);

    // 高权重源后到：只补来源，不重算分数、不新增行。
    let w = books_search_books::insert_batch(&pool, &job.id, "剑来", 10, vec![hi])
        .await
        .unwrap();
    assert_eq!(w.added, 0, "同书不应新增行");
    let (merged, _) = books_search_books::load_page(&pool, &job.id, None, None, 10)
        .await
        .unwrap();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].rank_score, first_score, "分数首次入库后不再改变");
    assert_eq!(merged[0].origins, vec!["低权重源", "高权重源"]);
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
        "测试关键词",
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
        "测试关键词",
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
        "CREATE TABLE source_query_stats (source_url TEXT NOT NULL, query_class TEXT NOT NULL, \
             search_count REAL NOT NULL DEFAULT 0, yield_ok_count REAL NOT NULL DEFAULT 0, \
             kept_hit_count REAL NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL DEFAULT 0, \
             PRIMARY KEY (source_url, query_class))",
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
              kept_hit_count INTEGER NOT NULL DEFAULT 0, \
              yield_ok_count INTEGER NOT NULL DEFAULT 0, \
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
        "测试关键词",
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
        "测试关键词",
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
    sqlx::query(
        "CREATE TABLE source_query_stats (source_url TEXT NOT NULL, query_class TEXT NOT NULL, \
             search_count REAL NOT NULL DEFAULT 0, yield_ok_count REAL NOT NULL DEFAULT 0, \
             kept_hit_count REAL NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL DEFAULT 0, \
             PRIMARY KEY (source_url, query_class))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let plan = resolve_search_sources(
        &pool,
        sources,
        &[],
        "测试关键词",
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
        "测试关键词",
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
    let c = job_counters(&job);
    assert!(!c.done());
    assert_eq!(c.pending, 3);
    assert_eq!(search_phase(c.pending), "first");
    assert_eq!(c.finished, 0, "没有任何源完成时 finished=0");
    job.state.lock().unwrap().pending = 0;
    let c = job_counters(&job);
    assert!(c.done());
    assert_eq!(search_phase(c.pending), "final");
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
    let c = job_counters(&job);
    assert!(c.attempted <= c.scheduled, "attempted 不能大于 scheduled");
    assert!(c.scheduled <= job.source_total, "scheduled 不能大于 total");
    assert_eq!(
        job.source_total - c.scheduled,
        1,
        "pending_sources = total - scheduled"
    );
}

#[test]
fn budget_skip_kinds_are_not_site_failures() {
    assert!(is_budget_skip("truncated"));
    assert!(is_budget_skip("skipped"));
    assert!(!is_budget_skip("timeout"));
    assert!(!is_budget_skip("site"));
    assert!(!is_budget_skip("engine"));
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

// 聚合入口（legacy `/api/search`、`/api/search/aggregate`、`/k/search` 共用）
// 在返回调用方之前做书名包含过滤；缓存只存过滤前结果，命中缓存时仍重新过滤。
#[tokio::test]
async fn aggregated_filters_title_matches_and_refilters_on_cache_hit() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations/shared")
        .run(&pool)
        .await
        .unwrap();
    let tier_cfg =
        crate::source_quality::TierConfig::from_config(&crate::config::Config::from_env());

    let srv = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = srv.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = srv.accept().await {
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await;
            // 两条都能通过相关性排序（partial 以上），但只有第一条书名完整包含关键词。
            let body = r#"<div class="item"><a href="/b/1">百年孤独</a></div><div class="item"><a href="/b/2">百年争战</a></div>"#;
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
    s.book_source_name = "aggregate-filter".into();
    s.search_url = Some(format!("http://{addr}/s?q={{{{key}}}}"));
    s.rule_search = Some(crate::entities::SearchRule {
        book_list: Some("class.item".into()),
        name: Some("a@text".into()),
        book_url: Some("a@href".into()),
        ..Default::default()
    });

    for round in 0..2 {
        let (books, errors, stats) = aggregated(
            &pool,
            86400,
            &tier_cfg,
            &[s.clone()],
            "百年孤独",
            false,
            crate::req::SearchScope::All,
        )
        .await;
        assert!(errors.is_empty(), "round {round}: {:?}", errors);
        assert_eq!(
            books.iter().map(|b| b.name.as_deref()).collect::<Vec<_>>(),
            vec![Some("百年孤独")],
            "round {round}: 只保留书名包含关键词的条目"
        );
        assert_eq!(stats.raw, 2, "round {round}: raw 是过滤前候选");
        assert_eq!(stats.kept, 1, "round {round}: kept 是过滤后候选");
        assert_eq!(stats.dropped, 1, "round {round}: 书名不含关键词的被丢弃");
    }

    // 批量入口也要写证据，但只写真实搜索：第二轮命中 60s 缓存，不得重复计数。
    let health: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT search_count, hit_count, kept_hit_count, yield_ok_count \
         FROM source_health WHERE source_url = ?",
    )
    .bind(s.book_source_url.clone())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        health,
        (1, 2, 1, 1),
        "批量入口应写一次搜索证据：2 条解析、1 条书名命中"
    );
    let qstats: (f64, f64, f64) = sqlx::query_as(
        "SELECT search_count, yield_ok_count, kept_hit_count \
         FROM source_query_stats WHERE source_url = ?",
    )
    .bind(s.book_source_url.clone())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(qstats, (1.0, 1.0, 1.0), "查询类证据同样只记一次");
}

fn test_job(id: &str, age_secs: u64, pending: usize) -> Arc<SearchJob> {
    test_job_for(7, id, age_secs, pending)
}

fn test_job_for(uid: i64, id: &str, age_secs: u64, pending: usize) -> Arc<SearchJob> {
    Arc::new(SearchJob {
        id: id.into(),
        uid,
        key: "k".into(),
        at: Instant::now()
            .checked_sub(Duration::from_secs(age_secs))
            .unwrap(),
        // 用年龄反推创建时刻：让「按创建时间淘汰」的测试可判定。
        created_unix_ms: 1_000_000u64.saturating_sub(age_secs.saturating_mul(1000)),
        source_total: pending,
        scope: crate::req::SearchScope::Curated,
        skipped_cooldown: 0,
        cooldown_total: 0,
        coverage: "curated".into(),
        source_mode: "curated".into(),
        scope_filtered: 0,
        curated_total: 0,
        cancel: Arc::new(tokio::sync::Notify::new()),
        cancelled: AtomicBool::new(false),
        // 测试里用 pending 推导状态：0 = 已完成，>0 = 运行中。
        status: Mutex::new(if pending == 0 { "done" } else { "running" }.into()),
        finished_unix_ms: AtomicU64::new(0),
        state: Mutex::new(SearchJobState {
            pending,
            ..Default::default()
        }),
    })
}

/// 已完成的旧任务超过上限时，只淘汰已完成的最旧任务；进行中的新 job 必须保留。
#[test]
fn prune_jobs_evicts_completed_but_keeps_active() {
    // 每个用户一个已完成任务：避免单用户保留上限先于全局上限生效。
    let mut list: Vec<Arc<SearchJob>> = (0..SEARCH_JOB_MAX)
        .map(|i| {
            test_job_for(
                i as i64,
                &format!("done{i}"),
                (SEARCH_JOB_MAX - i) as u64,
                0,
            )
        })
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

// 两段式结果在出参前做书名包含过滤：只保留书名含关键词的条目，
// 作者/简介不参与；过滤计数进 title_filter，全部被过滤时返回空数组而非报错。
#[tokio::test]
async fn search_result_filters_by_title_at_response_boundary() {
    let pool = jobs_pool().await;
    let mut job = test_job("title-filter", 0, 0);
    Arc::get_mut(&mut job).unwrap().key = "百年孤独".into();
    persist_started(&pool, &job).await.unwrap();
    // 落库时已做相关性门槛 + 书名包含过滤：只留下书名命中的条目。
    let w = books_search_books::insert_batch(
        &pool,
        &job.id,
        "百年孤独",
        0,
        vec![
            SearchBook {
                name: Some("《百年孤独》".into()),
                book_url: "https://x/1".into(),
                origin: Some("https://s/1".into()),
                ..Default::default()
            },
            // 只命中查询片段：相关性通过，但书名不含完整关键词，落库前被丢弃
            SearchBook {
                name: Some("百年争战".into()),
                book_url: "https://x/2".into(),
                origin: Some("https://s/1".into()),
                ..Default::default()
            },
            // 相关性为 0：在门槛阶段就被丢弃，不计入 raw
            SearchBook {
                name: Some("海贼王".into()),
                book_url: "https://x/3".into(),
                origin: Some("https://s/1".into()),
                ..Default::default()
            },
        ],
    )
    .await
    .unwrap();
    {
        let mut st = job.state.lock().unwrap();
        st.result_count = w.added;
        st.raw_count = w.candidates;
    }
    job_registry().lock().unwrap().push(job);

    let state = || {
        State(AppState {
            shared: pool.clone(),
            cfg: crate::config::Config::from_env(),
            user_pools: Default::default(),
            fetcher: std::sync::Arc::new(crate::web_book::ReqFetcher),
        })
    };
    let owner = AuthUser {
        id: 7,
        username: "owner".into(),
        role: "user".into(),
    };
    let ok = search_result(
        state(),
        owner,
        Query(JobResultQ {
            job_id: "title-filter".to_string(),
            page: Some(1),
            page_size: None,
            cursor: None,
        }),
    )
    .await
    .expect("任务创建者应可读");

    let v = ok.0;
    let books = v["books"].as_array().expect("books 应为数组");
    assert_eq!(books.len(), 1, "只保留书名包含关键词的条目");
    assert_eq!(books[0]["name"], "《百年孤独》");
    assert_eq!(v["title_filter"]["raw"], 2);
    assert_eq!(v["title_filter"]["kept"], 1);
    assert_eq!(v["title_filter"]["dropped"], 1);
}

// 全部被过滤时正常返回空列表（200），不报错。
#[tokio::test]
async fn search_result_all_filtered_returns_empty_list() {
    let pool = jobs_pool().await;
    let mut job = test_job("all-filtered", 0, 0);
    Arc::get_mut(&mut job).unwrap().key = "百年孤独".into();
    persist_started(&pool, &job).await.unwrap();
    let w = books_search_books::insert_batch(
        &pool,
        &job.id,
        "百年孤独",
        0,
        vec![SearchBook {
            name: Some("百年争战".into()),
            book_url: "https://x/2".into(),
            origin: Some("https://s/1".into()),
            ..Default::default()
        }],
    )
    .await
    .unwrap();
    {
        let mut st = job.state.lock().unwrap();
        st.result_count = w.added;
        st.raw_count = w.candidates;
    }
    job_registry().lock().unwrap().push(job);

    let state = || {
        State(AppState {
            shared: pool.clone(),
            cfg: crate::config::Config::from_env(),
            user_pools: Default::default(),
            fetcher: std::sync::Arc::new(crate::web_book::ReqFetcher),
        })
    };
    let owner = AuthUser {
        id: 7,
        username: "owner".into(),
        role: "user".into(),
    };
    let ok = search_result(
        state(),
        owner,
        Query(JobResultQ {
            job_id: "all-filtered".to_string(),
            page: Some(1),
            page_size: None,
            cursor: None,
        }),
    )
    .await
    .expect("全部被过滤不是错误");
    assert!(ok.0["books"].as_array().unwrap().is_empty());
    assert_eq!(ok.0["title_filter"]["kept"], 0);
    assert_eq!(ok.0["title_filter"]["dropped"], 1);
}

// 搜索任务归属：别人的 job_id 与不存在都返回 404，防越权读取与存在性探测。
#[tokio::test]
async fn search_result_rejects_other_users_job() {
    use axum::response::IntoResponse;

    // test_job 默认 uid=7；先注入一条真实存在的任务。
    let pool = jobs_pool().await;
    let job = test_job("owned-by-7", 0, 0);
    job_registry().lock().unwrap().push(job);
    let state = || {
        State(AppState {
            shared: pool.clone(),
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

// ---------------- 搜索任务管理（内存 + 数据库） ----------------

/// 只建搜索任务相关迁移涉及的表：直接执行真实迁移文件，避免测试里再抄一份 schema。
async fn jobs_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    for sql in [
        include_str!("../../migrations/shared/0017_search_jobs.sql"),
        include_str!("../../migrations/shared/0018_search_job_books.sql"),
    ] {
        for stmt in sql.split(';') {
            let s = stmt.trim();
            if !s.is_empty() {
                sqlx::query(s).execute(&pool).await.unwrap();
            }
        }
    }
    pool
}

#[tokio::test]
async fn search_job_db_roundtrip_keeps_rows_and_isolates_users() {
    let pool = jobs_pool().await;
    let job = test_job_for(42, "job-db", 0, 2);
    persist_started(&pool, &job).await.unwrap();

    let row = db_get(&pool, 42, "job-db")
        .await
        .unwrap()
        .expect("running 行");
    assert_eq!(row.status, "running");
    assert_eq!(row.keyword, "k");
    // 跨用户不可见：与“不存在”同一结果，避免任务存在性泄露。
    assert!(db_get(&pool, 43, "job-db").await.unwrap().is_none());

    // 结果边搜边落库：不再写整份 snapshot_json。
    let w = books_search_books::insert_batch(
        &pool,
        &job.id,
        "k",
        0,
        vec![mk_book("k".into(), "https://x/1".into())],
    )
    .await
    .unwrap();
    assert_eq!(w.added, 1);
    let mut meta = job.meta();
    meta.status = "done".into();
    meta.finished_unix_ms = 123;
    let counters = JobCounters {
        finished: 2,
        pending: 0,
        scheduled: 2,
        attempted: 2,
        result_count: w.added,
        raw_count: w.candidates,
        result_bytes: w.bytes_added,
        ..Default::default()
    };
    persist_finished(&pool, &meta, &counters).await.unwrap();

    let row = db_get(&pool, 42, "job-db").await.unwrap().unwrap();
    assert_eq!(row.status, "done");
    assert!(row.snapshot_json.is_none(), "新任务不再写整份快照");
    assert_eq!(row.result_count, 1);
    assert_eq!(row.raw_count, 1);
    // 结果行可恢复。
    let (books, _) = books_search_books::load_page(&pool, "job-db", None, None, 10)
        .await
        .unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].book_url, "https://x/1");

    let list = db_list(&pool, 42, 50).await.unwrap();
    assert_eq!(list.len(), 1);

    // 删除任务必须同时删结果行（行表无外键级联）。
    assert!(db_delete(&pool, 42, "job-db").await.unwrap());
    assert!(db_get(&pool, 42, "job-db").await.unwrap().is_none());
    let left: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM search_job_books WHERE job_id = 'job-db'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(left, 0, "结果行应随任务一起删除");
    assert!(!db_delete(&pool, 42, "job-db").await.unwrap());
}

#[tokio::test]
async fn load_job_lookup_falls_back_to_db_rows() {
    let pool = jobs_pool().await;
    let job = test_job_for(42, "job-lookup-db", 0, 0);
    persist_started(&pool, &job).await.unwrap();
    let w = books_search_books::insert_batch(
        &pool,
        &job.id,
        "k",
        0,
        vec![mk_book("k".into(), "https://x/1".into())],
    )
    .await
    .unwrap();
    let mut meta = job.meta();
    meta.status = "done".into();
    let counters = JobCounters {
        result_count: w.added,
        raw_count: w.candidates,
        ..Default::default()
    };
    persist_finished(&pool, &meta, &counters).await.unwrap();

    // 内存里没有这个任务（模拟进程重启 / 内存淘汰）：必须从数据库恢复。
    let lookup = load_job_lookup(&pool, 42, "job-lookup-db")
        .await
        .unwrap()
        .expect("数据库任务行");
    assert_eq!(lookup.meta.status, "done");
    assert!(!lookup.running, "已结束任务不应按 running 返回");
    assert_eq!(lookup.counters.result_count, 1);
    assert!(lookup.legacy_books.is_none(), "新任务没有旧快照");
    let (books, _) = books_search_books::load_page(&pool, "job-lookup-db", None, None, 10)
        .await
        .unwrap();
    assert_eq!(books[0].book_url, "https://x/1");
    // 跨用户与不存在同一结果。
    assert!(load_job_lookup(&pool, 43, "job-lookup-db")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn mark_interrupted_only_touches_running_rows() {
    let pool = jobs_pool().await;
    persist_started(&pool, &test_job_for(7, "running-1", 0, 3))
        .await
        .unwrap();
    persist_started(&pool, &test_job_for(7, "running-2", 0, 1))
        .await
        .unwrap();
    // 先收尾一个任务，确认不会被重启清理误改。
    let done_job = test_job_for(7, "done-1", 0, 0);
    persist_started(&pool, &done_job).await.unwrap();
    let mut done_meta = done_job.meta();
    done_meta.status = "done".into();
    persist_finished(&pool, &done_meta, &JobCounters::default())
        .await
        .unwrap();

    assert_eq!(mark_interrupted(&pool).await.unwrap(), 2);
    let running = db_get(&pool, 7, "running-1").await.unwrap().unwrap();
    assert_eq!(running.status, "interrupted");
    assert_eq!(running.pending, 0);
    let done = db_get(&pool, 7, "done-1").await.unwrap().unwrap();
    assert_eq!(done.status, "done");
}

#[test]
fn per_user_prune_keeps_recent_completed_jobs() {
    let mut list: Vec<Arc<SearchJob>> = (0..SEARCH_JOB_MAX_PER_USER + 5)
        .map(|i| test_job_for(9, &format!("done{i}"), i as u64, 0))
        .collect();
    prune_jobs(&mut list);
    assert_eq!(list.len(), SEARCH_JOB_MAX_PER_USER);
    assert!(list.iter().any(|j| j.id == "done0"), "最新任务必须保留");
    assert!(!list.iter().any(|j| j.id == "done24"), "最旧任务被淘汰");
}

#[test]
fn agg_cache_evicts_oldest_query_not_newest() {
    let now = std::time::Instant::now();
    // q0 最旧、qN 最新；超过上限时应该淘汰 q0，而不是刚写入的 qN。
    let mut cache: Vec<AggEntry> = (0..=AGG_MAX_QUERIES)
        .map(|i| AggEntry {
            key: format!("q{i}"),
            at: now - std::time::Duration::from_secs((AGG_MAX_QUERIES - i) as u64),
            books: vec![],
            errors: vec![],
        })
        .collect();
    prune_cache(&mut cache);
    assert_eq!(cache.len(), AGG_MAX_QUERIES);
    assert!(!cache.iter().any(|e| e.key == "q0"), "最旧查询应被淘汰");
    assert!(
        cache.iter().any(|e| e.key == format!("q{AGG_MAX_QUERIES}")),
        "最新查询必须保留，否则翻页永远命中不了缓存"
    );
}

#[test]
fn result_response_exposes_status_and_cancel_counts() {
    let job = test_job_for(1, "j-cancelled", 0, 2);
    let mut meta = job.meta();
    meta.key = "剑来".into();
    meta.status = "cancelled".into();
    meta.finished_unix_ms = 42;
    let counters = JobCounters {
        finished: 1,
        pending: 0,
        scheduled: 1,
        attempted: 1,
        cancelled: 1,
        result_count: 1,
        raw_count: 1,
        ..Default::default()
    };
    let books = vec![mk_book("剑来".into(), "https://x/1".into())];
    let v = build_result_response(&meta, &counters, books, 1, 30, None);
    assert_eq!(v["status"], "cancelled");
    assert_eq!(v["done"], true);
    assert_eq!(v["cancelled_sources"], 1);
    assert_eq!(v["pending_sources"], 0, "任务结束后不再提示未排上的源");
    assert_eq!(v["total"], 1);
    assert_eq!(v["finished_at"], 42);
}
