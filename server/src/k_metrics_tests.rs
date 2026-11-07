//! k_metrics 单测（move-only 从 k_metrics.rs 拆出）。

use super::*;

fn sample_line() -> KLine {
    KLine {
        ts: "2026-11-04T10:00:00+08:00".into(),
        path: "/k/search".into(),
        status: 200,
        render_ms: 321,
        bytes: 4096,
        key: Some("百年孤独".into()),
        book: None,
        ch: None,
        p: Some("1".into()),
        fs: Some("2".into()),
        vw: None,
        vh: None,
        scope: Some("curated".into()),
        tag: None,
        ua_class: "kindle".into(),
        ua: "Mozilla/5.0 (Kindle)".into(),
        sid: Some(sid_hash("super-secret-token")),
        since_prev_ms: Some(2500),
    }
}

#[test]
fn line_has_all_fields_and_sid_is_hash() {
    let line = sample_line();
    let v = serde_json::to_value(&line).unwrap();
    for key in [
        "ts",
        "path",
        "status",
        "render_ms",
        "bytes",
        "key",
        "book",
        "ch",
        "p",
        "fs",
        "vw",
        "vh",
        "scope",
        "tag",
        "ua_class",
        "ua",
        "sid",
        "since_prev_ms",
    ] {
        assert!(v.get(key).is_some(), "缺字段 {key}");
    }
    let sid = v["sid"].as_str().unwrap();
    assert_eq!(sid.len(), 8, "sid 只保留 8 位");
    assert!(!sid.contains("super-secret"), "sid 不能是原文");
    assert_eq!(sid, sid_hash("super-secret-token"), "同一 token 哈希稳定");
    assert!(v["key"].as_str().unwrap().chars().count() <= 64);
}

#[test]
fn key_is_truncated() {
    let long = "长".repeat(100);
    assert_eq!(truncate(&long, 64).chars().count(), 64);
    let mut params = HashMap::new();
    params.insert("key".to_string(), long);
    assert_eq!(take_param(&params, "key", 64).unwrap().chars().count(), 64);
}

#[test]
fn prune_removes_old_files_and_oversize() {
    let dir = std::env::temp_dir().join(format!("k-metrics-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 1..=9 {
        std::fs::write(
            dir.join(format!("k-metrics-2026010{i}.jsonl")),
            "x".repeat(600),
        )
        .unwrap();
    }
    // 保留 7 天 → 删最旧 2 个
    prune_with(&dir, "k-metrics", 7, MAX_BYTES);
    assert!(!dir.join("k-metrics-20260101.jsonl").exists());
    assert!(!dir.join("k-metrics-20260102.jsonl").exists());
    assert!(dir.join("k-metrics-20260109.jsonl").exists());
    // 总量超限 → 继续从最旧删，直到 <= 1000
    prune_with(&dir, "k-metrics", 7, 1000);
    let files = log_files(&dir, "k-metrics");
    let total: u64 = files.iter().map(|p| file_len(p)).sum();
    assert!(total <= 1000, "超限后总量应 <= 1000，实际 {total}");
    assert!(files.len() < 7);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_admin_gets_forbidden() {
    let user = AuthUser {
        id: 1,
        username: "u".into(),
        role: "user".into(),
    };
    let err = ensure_admin(&user).unwrap_err();
    assert!(matches!(err, AppError::Forbidden));
    assert_eq!(
        AppError::Forbidden.into_response().status(),
        StatusCode::FORBIDDEN
    );
}

#[test]
fn probe_summary_rejects_non_admin() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // 探针聚合只读文件系统，连接池仅用于构造 AppState
        let pool = sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap();
        let state = AppState {
            shared: pool,
            cfg: crate::config::Config::from_env(),
            user_pools: std::sync::Arc::new(std::sync::Mutex::new(
                crate::routes::UserPoolCache::default(),
            )),
        };
        let user = AuthUser {
            id: 2,
            username: "u2".into(),
            role: "user".into(),
        };
        let res = probe_summary(State(state), user, Query(MetricsQ { date: None })).await;
        match res {
            Err(AppError::Forbidden) => {}
            Err(e) => panic!("非管理员必须是 403，实得 {e:?}"),
            Ok(_) => panic!("非管理员不得拿到探针聚合"),
        }
    });
}

#[test]
fn probe_line_keeps_all_fields() {
    let line = ProbeLine {
        ts: "2026-11-05T17:09:36+08:00".into(),
        path: "/k/read".into(),
        vw_param: Some("1072".into()),
        vh_param: Some("1268".into()),
        inner_w: Some("1072".into()),
        inner_h: Some("1268".into()),
        client_w: Some("1055".into()),
        client_h: Some("1200".into()),
        scroll_w: Some("1072".into()),
        scroll_h: Some("1244".into()),
        dpr: Some("1".into()),
        screen_w: Some("1072".into()),
        screen_h: Some("1268".into()),
        avail_h: Some("1268".into()),
        outer_w: Some("1072".into()),
        outer_h: Some("1268".into()),
        body_h: Some("1244".into()),
        stage_h: Some("1056".into()),
        visible_h: Some("1180".into()),
        bar_top: Some("1092".into()),
        calib: Some("ok".into()),
        meas_y: Some("2088".into()),
        bar_bottom: Some("1168".into()),
        page_y: Some("0".into()),
        body_scroll_h: Some("1168".into()),
        kfix: Some("0".into()),
        body_rect_bottom: Some("1168".into()),
        lowest_el_bottom: Some("1168".into()),
        scroll_evt: Some("0".into()),
        stage: Some("move".into()),
        err: None,
        ln: Some("0".into()),
        doc_top: Some("0".into()),
        scroll_top_body: Some("0".into()),
        scroll_top_doc: Some("4".into()),
        max_seen: Some("8".into()),
        vv_off: Some("-".into()),
        bar_rect_top: Some("1092".into()),
        move_src: Some("touchmove".into()),
        bar_top_delta: Some("24".into()),
        scroll_y: Some("0".into()),
        tev: Some("move".into()),
        cancelable: Some("1".into()),
        touches_len: Some("1".into()),
        client_y: Some("640".into()),
        page_y_touch: Some("640".into()),
        doc_h: Some("1136".into()),
        doc_minus_outer: Some("-96".into()),
        doc_minus_client: Some("-132".into()),
        outer_minus_doc: Some("96".into()),
        meta_h: Some("1200".into()),
        vw_meta_height: Some("1200".into()),
        fs: Some("2".into()),
        ua_class: "kindle".into(),
        ua: Some("Mozilla/5.0 Kindle Silk".into()),
        sid: Some("deadbeef".into()),
    };
    let v: serde_json::Value = serde_json::to_value(&line).unwrap();
    for k in [
        "ts",
        "path",
        "vw_param",
        "vh_param",
        "inner_w",
        "inner_h",
        "client_w",
        "client_h",
        "scroll_w",
        "scroll_h",
        "dpr",
        "screen_w",
        "screen_h",
        "avail_h",
        "outer_w",
        "outer_h",
        "body_h",
        "stage_h",
        "visible_h",
        "bar_top",
        "calib",
        "meas_y",
        "bar_bottom",
        "page_y",
        "body_scroll_h",
        "kfix",
        "body_rect_bottom",
        "lowest_el_bottom",
        "scroll_evt",
        "stage",
        "err",
        "ln",
        "doc_top",
        "scroll_top_body",
        "scroll_top_doc",
        "max_seen",
        "vv_off",
        "bar_rect_top",
        "move_src",
        "bar_top_delta",
        "scroll_y",
        "tev",
        "cancelable",
        "touches_len",
        "client_y",
        "page_y",
        "doc_h",
        "doc_minus_outer",
        "doc_minus_client",
        "outer_minus_doc",
        "meta_h",
        "vw_meta_height",
        "fs",
        "ua_class",
        "ua",
        "sid",
    ] {
        assert!(v.get(k).is_some(), "探针字段缺失：{k}");
    }
    // 反序列化也能还原（聚合读取依赖它）
    let back: ProbeLine = serde_json::from_value(v).unwrap();
    assert_eq!(back.client_h.as_deref(), Some("1200"));
    assert_eq!(back.screen_h.as_deref(), Some("1268"));
    assert_eq!(back.stage_h.as_deref(), Some("1056"));
    assert_eq!(back.visible_h.as_deref(), Some("1180"));
    assert_eq!(back.bar_top.as_deref(), Some("1092"));
}

/// v1 的历史行没有 v2 字段，聚合读旧文件不能整行丢掉。
#[test]
fn probe_old_line_without_v2_fields_still_parses() {
    let old = r#"{"ts":"2026-11-05T19:26:59+08:00","path":"/k/read","vw_param":"1072","vh_param":"1268","inner_w":"1072","inner_h":"1268","client_w":"1072","client_h":"1268","scroll_w":"1072","scroll_h":"1268","dpr":"1.7964072227478027","fs":"2","ua_class":"kindle","ua":"Mozilla/5.0 Kindle","sid":"ad620038"}"#;
    let back: ProbeLine = serde_json::from_str(old).unwrap();
    assert_eq!(back.client_h.as_deref(), Some("1268"));
    assert_eq!(back.screen_h, None);
}

#[test]
fn probe_values_are_truncated() {
    let long = "1".repeat(200);
    let p = ProbeQ {
        vw_param: Some(long.clone()),
        vh_param: Some(" 1268 ".into()),
        inner_w: None,
        inner_h: None,
        client_w: None,
        client_h: None,
        scroll_w: None,
        scroll_h: None,
        dpr: None,
        screen_w: Some(long.clone()),
        screen_h: Some(" 1268 ".into()),
        avail_h: Some(long.clone()),
        outer_w: Some(long.clone()),
        outer_h: None,
        body_h: Some(long.clone()),
        stage_h: Some("-".into()),
        visible_h: Some("1180".into()),
        bar_top: Some("1092".into()),
        calib: Some("fail".into()),
        meas_y: Some("0".into()),
        bar_bottom: Some(long.clone()),
        page_y: Some(" 12 ".into()),
        body_scroll_h: Some(long.clone()),
        kfix: Some("1".into()),
        body_rect_bottom: Some(long.clone()),
        lowest_el_bottom: Some(long.clone()),
        scroll_evt: Some("1".into()),
        stage: Some("gerr".into()),
        err: Some(long.clone()),
        ln: Some("42".into()),
        doc_top: Some(" 4 ".into()),
        scroll_top_body: Some(long.clone()),
        scroll_top_doc: Some("0".into()),
        max_seen: Some("8".into()),
        vv_off: Some("-".into()),
        bar_rect_top: Some("1092".into()),
        move_src: Some("end".into()),
        bar_top_delta: Some("24".into()),
        scroll_y: Some("0".into()),
        tev: Some("end".into()),
        cancelable: Some("0".into()),
        touches_len: Some("0".into()),
        client_y: Some("700".into()),
        page_y_touch: Some("700".into()),
        doc_h: Some("1136".into()),
        doc_minus_outer: Some("-96".into()),
        doc_minus_client: Some("-132".into()),
        outer_minus_doc: Some("96".into()),
        meta_h: Some("-".into()),
        vw_meta_height: Some("-".into()),
        fs: None,
        path: Some("/k/read".repeat(30)),
    };
    assert_eq!(norm(p.vw_param).unwrap().chars().count(), PROBE_LIMIT);
    assert_eq!(norm(p.vh_param).as_deref(), Some("1268"));
    assert_eq!(norm(p.path).unwrap().chars().count(), PROBE_LIMIT);
    for v in [
        norm(p.screen_w),
        norm(p.avail_h),
        norm(p.outer_w),
        norm(p.body_h),
    ] {
        assert_eq!(
            v.unwrap().chars().count(),
            PROBE_LIMIT,
            "v2 字段同样 32 字符截断"
        );
    }
    assert_eq!(norm(p.screen_h).as_deref(), Some("1268"));
    assert_eq!(norm(p.stage_h).as_deref(), Some("-"));
    assert_eq!(norm(p.visible_h).as_deref(), Some("1180"));
    assert_eq!(norm(p.calib).as_deref(), Some("fail"));
    assert_eq!(norm(p.meas_y).as_deref(), Some("0"));
    assert_eq!(norm(p.page_y).as_deref(), Some("12"));
    assert_eq!(norm(p.kfix).as_deref(), Some("1"));
    assert_eq!(norm(p.scroll_evt).as_deref(), Some("1"));
    for v in [
        norm(p.bar_bottom),
        norm(p.body_scroll_h),
        norm(p.body_rect_bottom),
        norm(p.lowest_el_bottom),
    ] {
        assert_eq!(
            v.unwrap().chars().count(),
            PROBE_LIMIT,
            "v4 字段同样 32 字符截断"
        );
    }
    assert_eq!(norm(p.stage).as_deref(), Some("gerr"));
    assert_eq!(norm(p.ln).as_deref(), Some("42"));
    assert_eq!(norm(p.doc_top).as_deref(), Some("4"));
    assert_eq!(norm(p.scroll_top_doc).as_deref(), Some("0"));
    assert_eq!(norm(p.max_seen).as_deref(), Some("8"));
    assert_eq!(norm(p.vv_off).as_deref(), Some("-"));
    assert_eq!(norm(p.move_src).as_deref(), Some("end"));
    assert_eq!(norm(p.bar_top_delta).as_deref(), Some("24"));
    assert_eq!(norm(p.tev).as_deref(), Some("end"));
    assert_eq!(norm(p.cancelable).as_deref(), Some("0"));
    assert_eq!(norm(p.touches_len).as_deref(), Some("0"));
    assert_eq!(norm(p.client_y).as_deref(), Some("700"));
    assert_eq!(norm(p.page_y_touch).as_deref(), Some("700"));
    assert_eq!(norm(p.doc_h).as_deref(), Some("1136"));
    assert_eq!(norm(p.meta_h).as_deref(), Some("-"));
    assert_eq!(
        norm(p.scroll_top_body).unwrap().chars().count(),
        PROBE_LIMIT
    );
    assert_eq!(
        norm60(p.err).unwrap().chars().count(),
        60,
        "异常信息放宽到 60 字"
    );
    assert_eq!(norm(Some("   ".into())), None);
}
