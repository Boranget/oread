//! 书源健康单测（move-only 从 source_health.rs 拆出）。

use super::*;

fn src(url: &str, name: &str) -> BookSource {
    let mut s = BookSource::default();
    s.book_source_url = url.into();
    s.book_source_name = name.into();
    s.enabled = true;
    s
}

fn outcome(url: &str, status: &'static str) -> Outcome {
    Outcome {
        source_url: url.into(),
        status,
        latency_ms: 12,
        error: None,
        hits: 0,
        kept_hits: 0,
        parsed: status == STATUS_OK,
        usage: false,
        query_class: crate::query_class::QueryClass::Normal,
        rule_complete: 1.0,
    }
}

fn test_tier_cfg() -> TierConfig {
    use crate::source_quality::Weights;
    TierConfig {
        preferred_min_score: 70,
        quarantine_below_score: 30,
        promote_streak: 3,
        demote_streak: 2,
        update_score_margin: 5,
        weights: Weights::default(),
    }
}

// 站点失败连续 3 次才冷却；第 2 次仍不冷却
#[test]
fn site_failures_cool_at_threshold() {
    let mut row: Option<HealthRow> = None;
    for i in 1..=3 {
        let next = apply_outcome(
            row.as_ref(),
            &outcome("https://a.com", STATUS_FAIL),
            1000,
            86400,
        );
        if i < FAIL_THRESHOLD {
            assert!(next.cooldown_until.is_none(), "第 {i} 次不应冷却");
        }
        row = Some(next);
    }
    let row = row.unwrap();
    assert_eq!(row.consecutive_failures, 3);
    assert_eq!(row.cooldown_until, Some(1000 + 86400 * 1000));
}

// 成功一次清零并解除冷却
#[test]
fn success_resets_failures_and_cooldown() {
    let mut row = apply_outcome(None, &outcome("https://a.com", STATUS_FAIL), 1000, 86400);
    row = apply_outcome(
        Some(&row),
        &outcome("https://a.com", STATUS_FAIL),
        2000,
        86400,
    );
    row = apply_outcome(
        Some(&row),
        &outcome("https://a.com", STATUS_FAIL),
        3000,
        86400,
    );
    assert!(row.cooling(3000));
    let row = apply_outcome(
        Some(&row),
        &outcome("https://a.com", STATUS_OK),
        4000,
        86400,
    );
    assert_eq!(row.consecutive_failures, 0);
    assert_eq!(row.cooldown_until, None);
    assert_eq!(row.last_ok_at, Some(4000));
}

// 超时不参与延迟统计：否则可用源的平均延迟会一路涨到预算值，
// 所有源挤在同一延迟档，入队顺序退化成随机，好源被排到预算之外。
#[test]
fn timeouts_do_not_pollute_avg_latency() {
    let ok = |lat: i64| Outcome {
        source_url: "https://a.com".into(),
        status: STATUS_OK,
        latency_ms: lat,
        error: None,
        hits: 1,
        kept_hits: 1,
        parsed: true,
        usage: true,
        query_class: crate::query_class::QueryClass::Normal,
        rule_complete: 1.0,
    };
    let timeout = Outcome {
        source_url: "https://a.com".into(),
        status: STATUS_PARTIAL,
        latency_ms: 8000,
        error: Some("搜索超时（单源 8s 预算）".into()),
        hits: 0,
        kept_hits: 0,
        parsed: false,
        usage: false,
        query_class: crate::query_class::QueryClass::Normal,
        rule_complete: 1.0,
    };
    let mut row = apply_outcome(None, &ok(1000), 1000, 86400);
    assert_eq!(row.avg_latency_ms, Some(1000));
    for i in 0..5 {
        row = apply_outcome(Some(&row), &timeout, 2000 + i, 86400);
    }
    assert_eq!(row.avg_latency_ms, Some(1000), "超时不能抬高平均延迟");
    // 环形样本只保留成功样本，均值仍反映真实延迟
    row = apply_outcome(Some(&row), &ok(3000), 9000, 86400);
    assert_eq!(row.avg_latency_ms, Some(2000));
    assert_eq!(row.consecutive_failures, 0);
}

// 产出率只统计「书名真的匹配」的搜索：解析出 30 条无关结果仍是 0 产出。
#[test]
fn yield_counts_only_kept_hits() {
    let hit = |hits: i64, kept: i64, usage: bool| Outcome {
        source_url: "https://a.com".into(),
        status: STATUS_OK,
        latency_ms: 800,
        error: None,
        hits,
        kept_hits: kept,
        parsed: true,
        usage,
        query_class: crate::query_class::QueryClass::Normal,
        rule_complete: 1.0,
    };
    let row = apply_outcome(None, &hit(30, 0, true), 1000, 86400);
    assert_eq!(row.hit_count, 30);
    assert_eq!(row.kept_hit_count, 0);
    assert_eq!(row.yield_ok_count, 0, "解析成功但书名全不匹配 = 无产出");

    let row = apply_outcome(Some(&row), &hit(30, 2, true), 2000, 86400);
    assert_eq!(row.kept_hit_count, 2);
    assert_eq!(row.yield_ok_count, 1);
    assert_eq!(row.search_count, 2);

    // 体检/巡检（usage=false）不写产出证据，避免热门语料抬高冷门查询的产出率。
    let row = apply_outcome(Some(&row), &hit(24, 24, false), 3000, 86400);
    assert_eq!(row.kept_hit_count, 2);
    assert_eq!(row.yield_ok_count, 1);
    assert_eq!(row.search_count, 2);
}

#[test]
fn smoothed_yield_uses_prior_then_evidence() {
    let mut row = HealthRow::default();
    assert!((smoothed_yield(&row) - 0.30).abs() < 1e-9, "无证据时用先验");

    row.search_count = 4;
    row.yield_ok_count = 4;
    assert!((smoothed_yield(&row) - 0.65).abs() < 1e-9);

    row.search_count = 100;
    row.yield_ok_count = 0;
    assert!(smoothed_yield(&row) < 0.02, "大量未命中应压到先验以下");
}

#[test]
fn search_ev_orders_by_yield_per_second() {
    let mut fast_low = HealthRow::default();
    fast_low.avg_latency_ms = Some(500);
    fast_low.search_count = 20;
    fast_low.yield_ok_count = 2;

    let mut slow_high = HealthRow::default();
    slow_high.avg_latency_ms = Some(2000);
    slow_high.search_count = 20;
    slow_high.yield_ok_count = 14;

    // 快但几乎不出结果 vs 慢但常出结果：按「每秒有效结果」后者更高。
    assert!(search_ev_ppm(&slow_high) > search_ev_ppm(&fast_low));

    // 同样产出时，快的在前。
    let mut fast_same = HealthRow::default();
    fast_same.avg_latency_ms = Some(500);
    fast_same.search_count = 20;
    fast_same.yield_ok_count = 14;
    assert!(search_ev_ppm(&fast_same) > search_ev_ppm(&slow_high));
}

// 引擎失败（partial）不计失败、不清零、不冷却——这是与站点故障的关键区别
#[test]
fn engine_failures_do_not_cool() {
    let mut row = apply_outcome(None, &outcome("https://a.com", STATUS_FAIL), 1000, 86400);
    row = apply_outcome(
        Some(&row),
        &outcome("https://a.com", STATUS_FAIL),
        2000,
        86400,
    );
    assert_eq!(row.consecutive_failures, 2);
    for _ in 0..10 {
        row = apply_outcome(
            Some(&row),
            &outcome("https://a.com", STATUS_PARTIAL),
            3000,
            86400,
        );
    }
    assert_eq!(row.consecutive_failures, 2, "引擎失败不应累加");
    assert_eq!(row.last_status, STATUS_PARTIAL);
    assert!(row.cooldown_until.is_none(), "引擎失败不应冷却");
}

// 冷却中的源默认跳过；include_cooled 时全部参与
#[test]
fn partition_skips_cooled_by_default() {
    let sources = vec![
        src("https://a.com", "A"),
        src("https://b.com", "B"),
        src("https://c.com", "C"),
    ];
    let mut health = HashMap::new();
    let mut cooled = HealthRow::default();
    cooled.source_url = "https://b.com".into();
    cooled.cooldown_until = Some(10_000);
    health.insert("https://b.com".into(), cooled);

    let p = partition_cooldown(sources.clone(), &health, false, 5_000);
    assert_eq!(p.active.len(), 2);
    assert_eq!(p.skipped, 1);
    assert!(p
        .active
        .iter()
        .all(|s| s.book_source_url != "https://b.com"));

    let p = partition_cooldown(sources.clone(), &health, true, 5_000);
    assert_eq!(p.active.len(), 3);
    assert_eq!(p.skipped, 0);
    assert_eq!(cooled_count(&sources, &health, 5_000), 1);
    // 冷却到期后不再计为冷却
    assert_eq!(cooled_count(&sources, &health, 10_000), 0);
}

// 站点不可达错误带标记 → Site；普通规则错误 → Engine
#[test]
fn classify_site_vs_engine() {
    let site = site_error("http status 404");
    assert_eq!(classify(&site), FailureKind::Site);
    let engine = anyhow::anyhow!("选择器未命中");
    assert_eq!(classify(&engine), FailureKind::Engine);
}

async fn test_pool() -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
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
        "CREATE TABLE IF NOT EXISTS source_query_stats (\
         source_url TEXT NOT NULL, query_class TEXT NOT NULL, \
         search_count REAL NOT NULL DEFAULT 0, yield_ok_count REAL NOT NULL DEFAULT 0, \
         kept_hit_count REAL NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL DEFAULT 0, \
         PRIMARY KEY (source_url, query_class))",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

#[tokio::test]
async fn db_record_cools_after_threshold_and_success_resets() {
    let pool = test_pool().await;
    for _ in 0..FAIL_THRESHOLD {
        record_outcomes(
            &pool,
            &[Outcome {
                source_url: "https://dead.example".into(),
                status: STATUS_FAIL,
                latency_ms: 30,
                error: Some("http status 404".into()),
                hits: 0,
                kept_hits: 0,
                parsed: false,
                usage: false,
                query_class: crate::query_class::QueryClass::Normal,
                rule_complete: 1.0,
            }],
            86400,
            &test_tier_cfg(),
        )
        .await
        .unwrap();
    }
    let map = load_map(&pool).await.unwrap();
    let row = map.get("https://dead.example").unwrap();
    assert_eq!(row.consecutive_failures, FAIL_THRESHOLD);
    assert!(row.cooldown_until.unwrap() > now_ms());

    record_outcomes(
        &pool,
        &[Outcome {
            source_url: "https://dead.example".into(),
            status: STATUS_OK,
            latency_ms: 12,
            error: None,
            hits: 0,
            kept_hits: 0,
            parsed: true,
            usage: false,
            query_class: crate::query_class::QueryClass::Normal,
            rule_complete: 1.0,
        }],
        86400,
        &test_tier_cfg(),
    )
    .await
    .unwrap();
    let map = load_map(&pool).await.unwrap();
    let row = map.get("https://dead.example").unwrap();
    assert_eq!(row.consecutive_failures, 0);
    assert_eq!(row.cooldown_until, None);
    assert_eq!(row.check_total, FAIL_THRESHOLD + 1);
}

#[tokio::test]
async fn db_engine_failure_does_not_cool() {
    let pool = test_pool().await;
    for _ in 0..2 {
        record_outcomes(
            &pool,
            &[Outcome {
                source_url: "https://engine.example".into(),
                status: STATUS_FAIL,
                latency_ms: 20,
                error: None,
                hits: 0,
                kept_hits: 0,
                parsed: false,
                usage: false,
                query_class: crate::query_class::QueryClass::Normal,
                rule_complete: 1.0,
            }],
            86400,
            &test_tier_cfg(),
        )
        .await
        .unwrap();
    }
    for _ in 0..10 {
        record_outcomes(
            &pool,
            &[Outcome {
                source_url: "https://engine.example".into(),
                status: STATUS_PARTIAL,
                latency_ms: 20,
                error: Some("selector miss".into()),
                hits: 0,
                kept_hits: 0,
                parsed: false,
                usage: false,
                query_class: crate::query_class::QueryClass::Normal,
                rule_complete: 1.0,
            }],
            86400,
            &test_tier_cfg(),
        )
        .await
        .unwrap();
    }
    let map = load_map(&pool).await.unwrap();
    let row = map.get("https://engine.example").unwrap();
    assert_eq!(row.consecutive_failures, 2);
    assert_eq!(row.cooldown_until, None);
    assert_eq!(row.last_status, STATUS_PARTIAL);
}

#[test]
fn host_counting_does_not_change_source_keys() {
    let sources = vec![
        src("https://a.com/v1", "A1"),
        src("https://a.com/v2", "A2"),
        src("https://b.com/x", "B"),
    ];
    assert_eq!(distinct_host_count(&sources), 2);
    assert_eq!(host_of("https://a.com/v1"), Some("a.com".into()));
}

#[test]
fn error_truncation_is_char_safe() {
    let long = "错".repeat(MAX_ERROR_CHARS + 5);
    let cut = truncate_error(&long);
    assert_eq!(cut.chars().count(), MAX_ERROR_CHARS + 1);
    assert!(cut.ends_with('…'));
    assert_eq!(truncate_error("短"), "短");
}

#[test]
fn explicit_site_rejection_only_matches_http_status() {
    assert!(is_explicit_site_rejection(Some(
        "http status 403 Forbidden"
    )));
    assert!(is_explicit_site_rejection(Some(
        "HTTP status 503 Service Unavailable"
    )));
    assert!(!is_explicit_site_rejection(Some(
        "搜索超时（单源 10s 预算）"
    )));
    assert!(!is_explicit_site_rejection(Some(
        "error sending request for url (https://x.example): connection timed out"
    )));
    assert!(!is_explicit_site_rejection(None));
}

#[tokio::test]
async fn import_transient_timeout_keeps_source_visible() {
    let pool = test_pool().await;
    record_outcomes(
        &pool,
        &[Outcome {
            source_url: "https://slow.example".into(),
            status: STATUS_FAIL,
            latency_ms: 10_000,
            error: Some("搜索超时（单源 10s 预算）".into()),
            hits: 0,
            kept_hits: 0,
            parsed: false,
            usage: false,
            query_class: crate::query_class::QueryClass::Normal,
            rule_complete: 1.0,
        }],
        86400,
        &test_tier_cfg(),
    )
    .await
    .unwrap();
    // 导入期的未知失败：事件已记录，但 tier 拉回 observing，仍可进入搜索池。
    keep_observing_after_transient_probe(&pool, &["https://slow.example".into()])
        .await
        .unwrap();
    let map = load_map(&pool).await.unwrap();
    let row = map.get("https://slow.example").unwrap();
    assert_eq!(row.consecutive_failures, 1);
    assert_eq!(row.tier, source_quality::TIER_OBSERVING);
    assert!(visible_in_search(Some(row)), "导入超时不得把源判死");
    assert!(
        row.cooldown_until.map(|t| t <= now_ms()).unwrap_or(true),
        "单次未知失败不应立刻冷却"
    );
}

#[tokio::test]
async fn import_zero_hit_partial_keeps_source_visible() {
    let pool = test_pool().await;
    record_outcomes(
        &pool,
        &[Outcome {
            source_url: "https://empty.example".into(),
            status: STATUS_PARTIAL,
            latency_ms: 40,
            error: Some("规则未产出结果".into()),
            hits: 0,
            kept_hits: 0,
            parsed: false,
            usage: true,
            query_class: crate::query_class::QueryClass::Normal,
            rule_complete: 1.0,
        }],
        86400,
        &test_tier_cfg(),
    )
    .await
    .unwrap();
    let map = load_map(&pool).await.unwrap();
    let row = map.get("https://empty.example").unwrap();
    assert!(visible_in_search(Some(row)), "0 命中不得把源判死");
}

#[tokio::test]
async fn explicit_site_failures_still_quarantine_at_threshold() {
    let pool = test_pool().await;
    for _ in 0..FAIL_THRESHOLD {
        record_outcomes(
            &pool,
            &[Outcome {
                source_url: "https://dead.example".into(),
                status: STATUS_FAIL,
                latency_ms: 30,
                error: Some("http status 503 Service Unavailable".into()),
                hits: 0,
                kept_hits: 0,
                parsed: false,
                usage: false,
                query_class: crate::query_class::QueryClass::Normal,
                rule_complete: 1.0,
            }],
            86400,
            &test_tier_cfg(),
        )
        .await
        .unwrap();
    }
    let map = load_map(&pool).await.unwrap();
    let row = map.get("https://dead.example").unwrap();
    assert_eq!(row.tier, source_quality::TIER_QUARANTINED);
    assert!(!visible_in_search(Some(row)));
    // 已达到硬阈值：导入期的“保留 observing”不得复活
    keep_observing_after_transient_probe(&pool, &["https://dead.example".into()])
        .await
        .unwrap();
    let map = load_map(&pool).await.unwrap();
    let row = map.get("https://dead.example").unwrap();
    assert_eq!(
        row.tier,
        source_quality::TIER_QUARANTINED,
        "达到阈值仍应隔离"
    );
}

// 回归：search_count / hit_count / parse_ok_count / kept_hit_count / yield_ok_count
// 五列必须原样落库。它们类型相同，UPSERT 的列名与 bind 顺序错位不会报错，
// 只会静默把 kept 写成 yield、把 yield 写成 parse（线上曾因此把产出率变成解析成功率）。
#[tokio::test]
async fn db_roundtrip_keeps_search_yield_columns_aligned() {
    let pool = test_pool().await;
    record_outcomes(
        &pool,
        &[Outcome {
            source_url: "https://yield.example".into(),
            status: STATUS_OK,
            latency_ms: 120,
            error: None,
            hits: 30,
            kept_hits: 7,
            parsed: true,
            usage: true,
            query_class: crate::query_class::QueryClass::Normal,
            rule_complete: 1.0,
        }],
        86400,
        &test_tier_cfg(),
    )
    .await
    .unwrap();

    let map = load_map(&pool).await.unwrap();
    let row = map.get("https://yield.example").unwrap();
    assert_eq!(row.search_count, 1);
    assert_eq!(row.hit_count, 30);
    assert_eq!(row.parse_ok_count, 1, "解析成功次数");
    assert_eq!(row.kept_hit_count, 7, "书名匹配条数之和");
    assert_eq!(row.yield_ok_count, 1, "有产出的搜索次数");

    // 第二次：解析成功但书名全不匹配，只增加 search_count / hit_count / parse_ok_count。
    record_outcomes(
        &pool,
        &[Outcome {
            source_url: "https://yield.example".into(),
            status: STATUS_OK,
            latency_ms: 120,
            error: None,
            hits: 12,
            kept_hits: 0,
            parsed: true,
            usage: true,
            query_class: crate::query_class::QueryClass::Normal,
            rule_complete: 1.0,
        }],
        86400,
        &test_tier_cfg(),
    )
    .await
    .unwrap();

    let map = load_map(&pool).await.unwrap();
    let row = map.get("https://yield.example").unwrap();
    assert_eq!(row.search_count, 2);
    assert_eq!(row.hit_count, 42);
    assert_eq!(row.parse_ok_count, 2);
    assert_eq!(row.kept_hit_count, 7);
    assert_eq!(row.yield_ok_count, 1);
}
