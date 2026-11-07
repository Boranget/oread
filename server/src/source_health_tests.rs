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
        parsed: status == STATUS_OK,
        usage: false,
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
                parsed: false,
                usage: false,
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
            parsed: true,
            usage: false,
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
                parsed: false,
                usage: false,
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
                parsed: false,
                usage: false,
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
            parsed: false,
            usage: false,
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
            parsed: false,
            usage: true,
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
                parsed: false,
                usage: false,
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
