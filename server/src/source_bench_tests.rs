//! source_bench 单测（move-only 从 source_bench.rs 拆出）。

use super::*;

fn entry(title: &str, cat: &str, ty: i64, builtin: bool) -> CorpusEntry {
    CorpusEntry {
        id: title.into(),
        title: title.into(),
        expected_author: None,
        category: cat.into(),
        source_type: ty,
        enabled: true,
        builtin,
        norm_title: normalize_title(title),
        created_at: None,
        updated_at: None,
        scored: true,
    }
}

/// 把内置常量转成运行时 CorpusEntry（与 load_corpus 的字段映射保持一致）。
fn builtin_entries() -> Vec<CorpusEntry> {
    BUILTIN_CORPUS
        .iter()
        .map(|b| CorpusEntry {
            id: builtin_id(b),
            title: b.title.to_string(),
            expected_author: b.expected_author.map(str::to_string),
            category: b.category.to_string(),
            source_type: b.source_type,
            enabled: true,
            builtin: true,
            norm_title: normalize_title(b.title),
            created_at: None,
            updated_at: None,
            scored: b.scored,
        })
        .collect()
}

#[test]
fn builtin_corpus_has_expected_shape() {
    assert_eq!(BUILTIN_CORPUS.len(), 24);
    let novel = BUILTIN_CORPUS
        .iter()
        .filter(|b| b.source_type == TYPE_NOVEL)
        .count();
    let comic = BUILTIN_CORPUS
        .iter()
        .filter(|b| b.source_type == TYPE_COMIC)
        .count();
    assert_eq!(novel, 12);
    assert_eq!(comic, 12, "漫画语料扩到 12 条（10–12 区间上限）");
    for title in [
        "海贼王",
        "火影忍者",
        "名侦探柯南",
        "一人之下",
        "鬼灭之刃",
        "斗罗大陆",
        "秦时明月",
        "狐妖小红娘",
    ] {
        assert!(
            BUILTIN_CORPUS
                .iter()
                .any(|b| b.title == title && b.source_type == TYPE_COMIC),
            "漫画内置语料缺少 {title}"
        );
    }
    let mut seen = HashSet::new();
    for b in BUILTIN_CORPUS {
        assert!(
            seen.insert((normalize_title(b.title), b.source_type)),
            "内置语料重复: {}",
            b.title
        );
    }
}

#[test]
fn normalization_is_stable() {
    assert_eq!(normalize_title("《斗破苍穹》 最新"), "斗破苍穹最新");
    assert_eq!(normalize_title("ＳＰＹ×ＦＡＭＩＬＹ"), "spyfamily");
    // 繁简不做转换：繁体与简体不同
    assert_ne!(normalize_title("鬥破蒼穹"), normalize_title("斗破苍穹"));
}

#[test]
fn title_match_rules() {
    assert!(title_matches("斗破苍穹", "斗破苍穹"));
    assert!(title_matches("斗破苍穹 最新章节", "斗破苍穹"));
    assert!(title_matches("《斗破苍穹》", "斗破苍穹"));
    // 包含率 < 0.6 不算近似同名
    assert!(!title_near_equal("斗破苍穹之少年归来", "斗破苍穹"));
    assert!(!title_near_equal("遮天外传", "遮天"));
    // 包含率 >= 0.6 但多出来的部分是“外传/之/从…”等衍生标记，也不算同名
    assert!(!title_near_equal("斗破苍穹外传", "斗破苍穹"));
    assert!(!title_near_equal("从斗破苍穹开始打卡", "斗破苍穹"));
    // 但诊断字段仍能看到“标题包含”
    assert!(title_contains("斗破苍穹外传", "斗破苍穹"));
    assert!(!title_matches("别的书", "斗破苍穹"));
}

#[test]
fn author_match_rules() {
    assert!(author_matches(Some("天蚕土豆"), Some("天蚕土豆")));
    assert!(author_matches(Some("天蚕土豆 著"), Some("天蚕土豆")));
    assert!(
        author_matches(Some("作者：天蚕土豆"), Some("天蚕土豆")),
        "去掉“作者：”前缀"
    );
    assert!(author_matches(Some("刘慈欣/王晋康"), Some("刘慈欣")));
    assert!(!author_matches(Some("别人"), Some("天蚕土豆")));
    assert!(!author_matches(None, Some("天蚕土豆")));
    assert!(author_matches(None, None), "无期望作者时不校验");
}

/// 旧规则（本仓库上一版）：标题完全相等或包含率 ≥ 0.8，且作者匹配。
fn old_hit(
    got_title: &str,
    expected_title: &str,
    got_author: Option<&str>,
    expected_author: Option<&str>,
) -> bool {
    let a = strip_decorations(normalize_title(got_title));
    let b = strip_decorations(normalize_title(expected_title));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let title_ok = a == b
        || ((a.contains(&b) || b.contains(&a))
            && norm_len(&a).min(norm_len(&b)) as f64 / norm_len(&a).max(norm_len(&b)) as f64
                >= 0.8);
    title_ok && author_matches(got_author, expected_author)
}

#[test]
fn hit_rule_five_samples_before_after() {
    // 2 组真命中 + 3 组衍生作误判
    let cases: [(&str, &str, Option<&str>, Option<&str>, bool); 5] = [
        (
            "斗破苍穹",
            "斗破苍穹",
            Some("天蚕土豆"),
            Some("天蚕土豆"),
            true,
        ),
        (
            "《三体》 最新章节",
            "三体",
            Some("刘慈欣"),
            Some("刘慈欣"),
            true,
        ),
        (
            "从斗破苍穹开始打卡",
            "斗破苍穹",
            Some("晨昏彼岸"),
            Some("天蚕土豆"),
            false,
        ),
        (
            "斗破苍穹之少年归来",
            "斗破苍穹",
            Some("某同人"),
            Some("天蚕土豆"),
            false,
        ),
        (
            "斗破苍穹外传",
            "斗破苍穹",
            Some("某同人"),
            Some("天蚕土豆"),
            false,
        ),
    ];
    for (got_t, exp_t, got_a, exp_a, want) in cases {
        let before = old_hit(got_t, exp_t, got_a, exp_a);
        let after = is_hit(got_t, exp_t, got_a, exp_a);
        eprintln!("{got_t} vs {exp_t} | before={before} after={after}");
        assert_eq!(after, want, "新规则判定错误: {got_t}");
    }
}

#[test]
fn exact_title_hits_without_author() {
    // 同名书作者缺失（站点未填）也应算命中：近似同名不依赖作者
    assert!(is_hit("斗破苍穹", "斗破苍穹", None, Some("天蚕土豆")));
    assert!(is_hit(
        "《斗破苍穹》 最新章节",
        "斗破苍穹",
        None,
        Some("天蚕土豆")
    ));
    // 但只是“标题包含”时仍必须作者一致
    assert!(!is_hit("斗破苍穹外传", "斗破苍穹", None, Some("天蚕土豆")));
    assert!(
        is_hit("斗破苍穹番外篇", "斗破苍穹", None, None),
        "无期望作者时包含即命中"
    );
}

#[test]
fn fast_track_requires_samples_hit_and_score() {
    let base = SourceBenchRow {
        source_url: "u".into(),
        name: "n".into(),
        source_type: TYPE_NOVEL,
        samples: 2,
        hits: 1,
        hit_rate: 0.4,
        avg_latency_ms: 1200,
        errors: 0,
        error_kinds: serde_json::Value::Null,
        bench_score: BENCH_FAST_TRACK_MIN_SCORE,
        curated: false,
        excluded_samples: 0,
        per_category_hit_rate: Vec::new(),
    };
    assert!(bench_fast_track_ok(&base));
    assert!(!bench_fast_track_ok(&SourceBenchRow {
        samples: 1,
        ..base.clone()
    }));
    assert!(!bench_fast_track_ok(&SourceBenchRow {
        hit_rate: 0.0,
        hits: 0,
        ..base.clone()
    }));
    assert!(!bench_fast_track_ok(&SourceBenchRow {
        bench_score: BENCH_FAST_TRACK_MIN_SCORE - 1,
        ..base
    }));
}

#[test]
fn salt_covers_all_24_builtin_entries() {
    let corpus = builtin_entries();
    let mut seen = HashSet::new();
    // 12 次不同 salt：小说源覆盖 12 条小说语料，漫画源覆盖 12 条漫画语料。
    for salt in 0..12 {
        for ty in [TYPE_NOVEL, TYPE_COMIC] {
            for e in pick_samples(&corpus, ty, 5, salt) {
                seen.insert(e.id);
            }
        }
    }
    assert_eq!(
        seen.len(),
        BUILTIN_CORPUS.len(),
        "12 次抽样必须覆盖全部内置语料（旧实现固定只跑 10 条）"
    );
    // 单次盐值不应重复抽同一条
    for salt in 0..12 {
        let picked = pick_samples(&corpus, TYPE_COMIC, 5, salt);
        let ids: HashSet<_> = picked.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids.len(), picked.len(), "同一批内不得重复抽样");
    }
}

#[test]
fn scored_flags_and_comic_categories() {
    let unscored: Vec<_> = BUILTIN_CORPUS.iter().filter(|b| !b.scored).collect();
    assert_eq!(
        unscored.len(),
        3,
        "出版/实体书 3 条（活着/平凡的世界/三体）标记为不计分"
    );
    assert!(unscored.iter().all(|b| b.source_type == TYPE_NOVEL));
    let comic_cats: HashSet<_> = BUILTIN_CORPUS
        .iter()
        .filter(|b| b.source_type == TYPE_COMIC)
        .map(|b| b.category)
        .collect();
    assert_eq!(
        comic_cats,
        HashSet::from(["日漫", "国漫"]),
        "漫画语料按日漫/国漫拆分 category"
    );
}

#[test]
fn judge_books_scans_top_results_without_loosening_is_hit() {
    let mut item = entry("庆余年", "历史", TYPE_NOVEL, true);
    item.expected_author = Some("猫腻".into());
    let mut derivative = SearchBook::default();
    derivative.name = Some("庆余年之无上之境".into());
    derivative.author = Some("路人甲".into());
    let mut exact = SearchBook::default();
    exact.name = Some("庆余年".into());
    exact.author = Some("猫腻".into());

    // 衍生作排第一、原书排第二：旧实现（只看第一条）会误判为未命中
    let (found, tm, am) = judge_books(&[derivative.clone(), exact.clone()], &item);
    assert!(found && tm && am);

    // 只有衍生作：仍然判未命中，is_hit 口径没有被放宽成“包含即可”
    let (found, tm, _) = judge_books(&[derivative], &item);
    assert!(!found);
    assert!(tm, "标题包含仍应记录 title_match，但不能算命中");

    // 只扫前 CHECK_TOP_N 条
    let filler = vec![SearchBook::default(); CHECK_TOP_N];
    let mut books = filler;
    books.push(exact);
    let (found, _, _) = judge_books(&books, &item);
    assert!(!found, "第 {} 条之后不再匹配", CHECK_TOP_N + 1);
}

#[test]
fn sample_pick_prefers_builtin_and_rotates_categories() {
    let corpus = vec![
        entry("a1", "玄幻", TYPE_NOVEL, true),
        entry("a2", "玄幻", TYPE_NOVEL, true),
        entry("b1", "都市", TYPE_NOVEL, true),
        entry("c1", "历史", TYPE_NOVEL, true),
        entry("u1", "都市", TYPE_NOVEL, false),
    ];
    let picked = pick_samples(&corpus, TYPE_NOVEL, 5, 0);
    assert_eq!(picked.len(), 5);
    assert!(picked[..4].iter().all(|e| e.builtin), "先覆盖内置（4 条）");
    assert_eq!(picked[4].title, "u1", "用户条目只补足，不挤占内置");
    let cats: HashSet<&str> = picked[..4].iter().map(|e| e.category.as_str()).collect();
    assert_eq!(cats.len(), 3, "内置按 category 轮转覆盖 3 个 category");
    // 漫画源只取 type=2
    let comic = pick_samples(&corpus, TYPE_COMIC, 5, 1);
    assert!(comic.is_empty());
}

#[test]
fn user_corpus_gets_a_slot_even_with_many_builtins() {
    let mut corpus: Vec<CorpusEntry> = (1..=10)
        .map(|i| entry(&format!("内置{i}"), "玄幻", TYPE_NOVEL, true))
        .collect();
    corpus.push(entry("用户新书", "自定义", TYPE_NOVEL, false));
    let picked = pick_samples(&corpus, TYPE_NOVEL, 5, 7);
    assert_eq!(picked.len(), 5);
    assert!(
        picked.iter().any(|e| !e.builtin && e.title == "用户新书"),
        "用户新增书名必须能进入下一批抽样"
    );
    assert_eq!(
        picked.iter().filter(|e| e.builtin).count(),
        4,
        "内置仍占绝对多数"
    );
}

#[test]
fn curated_gap_truncates_between_min_and_max() {
    let scores = vec![90, 88, 86, 84, 82, 80, 78, 76, 60, 58, 56, 54, 52, 50, 48];
    // 8 分断层在 76→60（第 9 名之前）
    assert_eq!(pick_curated_count(&scores, 8, 14, 8), 8);
    let flat = vec![80; 20];
    assert_eq!(pick_curated_count(&flat, 8, 14, 8), 14, "无断层取上限");
    let small = vec![90, 89, 88];
    assert_eq!(
        pick_curated_count(&small, 8, 14, 8),
        3,
        "不足下限就全取（但仍要求样本>=2）"
    );
}

#[test]
fn bench_staleness_window() {
    let hour = 3_600_000i64;
    let now = 1_000_000_000i64;
    assert!(!bench_due(None, now, 72), "无体检历史不触发");
    assert!(!bench_due(Some(0), now, 72));
    assert!(!bench_due(Some(now - 71 * hour), now, 72));
    assert!(bench_due(Some(now - 72 * hour), now, 72), "满 72h 触发");
    assert!(bench_due(Some(now - 80 * hour), now, 72));
}

#[test]
fn comic_curated_uses_its_own_min() {
    let cfg = Config::from_env();
    assert_eq!(
        curated_bounds(&cfg, TYPE_COMIC),
        (3, 14),
        "漫画下限单独放宽到 3"
    );
    assert_eq!(curated_bounds(&cfg, TYPE_NOVEL), (8, 14), "小说仍用 min=8");
    // 5 个漫画源：若共用 min=8 会全取 5 个（不足下限全取），但漫画上限/断层语义独立；
    // 这里验证“断层优先”仍在：90/88/86 | 60/58 → 在 86→60 处截断为 3。
    let scores = vec![90, 88, 86, 60, 58];
    assert_eq!(
        pick_curated_count(&scores, 3, 14, 8),
        3,
        "漫画：断层在 3 名后截断"
    );
}

fn run(url: &str, cat: &str, key: &str, found: bool, suspicious: bool) -> BenchRun {
    BenchRun {
        batch_id: "b".into(),
        source_url: url.into(),
        corpus_key: key.into(),
        title: key.into(),
        category: cat.into(),
        source_type: TYPE_NOVEL,
        builtin_sample: true,
        found,
        title_match: found,
        author_match: true,
        latency_ms: 100,
        error_kind: None,
        suspicious,
        run_at: 1,
    }
}

#[test]
fn suspicious_corpus_is_flagged_and_excluded_from_score() {
    // 3 条内置语料：正常两条 100% 命中，冷门一条全失败
    let corpus = vec![
        entry("正常1", "玄幻", TYPE_NOVEL, true),
        entry("正常2", "玄幻", TYPE_NOVEL, true),
        entry("冷门", "玄幻", TYPE_NOVEL, true),
    ];
    let mut runs = vec![
        run("https://s1", "玄幻", "正常1", true, false),
        run("https://s1", "玄幻", "正常2", true, false),
        run("https://s1", "玄幻", "冷门", false, false),
        run("https://s2", "玄幻", "正常1", true, false),
        run("https://s2", "玄幻", "正常2", true, false),
        run("https://s2", "玄幻", "冷门", false, false),
    ];
    mark_suspicious(&mut runs, &corpus);
    assert!(
        runs.iter()
            .find(|r| r.corpus_key == "冷门")
            .unwrap()
            .suspicious
    );
    assert!(
        !runs
            .iter()
            .find(|r| r.corpus_key == "正常1")
            .unwrap()
            .suspicious
    );

    let cfg = Config::from_env();
    let mut src = BookSource::default();
    src.book_source_url = "https://s1".into();
    src.book_source_name = "s1".into();
    let sources = HashMap::from([("https://s1".to_string(), src)]);
    let rows = compute_source_rows(
        &runs,
        &sources,
        &corpus,
        &cfg,
        &HashMap::new(),
        &HashMap::new(),
    );
    let row = &rows[0];
    assert_eq!(row.samples, 2, "可疑语料被排除出计分样本");
    assert_eq!(row.excluded_samples, 1);
    assert_eq!(row.hits, 2);
    assert!(
        (row.hit_rate - 1.0).abs() < 1e-9,
        "剔除冷门语料后命中率不受拖累"
    );
}

#[test]
fn globally_zero_category_is_excluded_from_score() {
    // 两个 category：玄幻两条都命中，出版在所有源上 0 命中。
    // 出版类与源池不匹配，不应把每个源的 hit_rate 从 1.0 拉到 0.5。
    let corpus = vec![
        entry("热门", "玄幻", TYPE_NOVEL, true),
        entry("冷门", "出版", TYPE_NOVEL, true),
    ];
    let runs = vec![
        run("https://s1", "玄幻", "热门", true, false),
        run("https://s1", "出版", "冷门", false, false),
        run("https://s2", "玄幻", "热门", true, false),
        run("https://s2", "出版", "冷门", false, false),
    ];
    let mut src = BookSource::default();
    src.book_source_url = "https://s1".into();
    src.book_source_name = "s1".into();
    let sources = HashMap::from([("https://s1".to_string(), src)]);
    let rows = compute_source_rows(
        &runs,
        &sources,
        &corpus,
        &Config::from_env(),
        &HashMap::new(),
        &HashMap::new(),
    );
    let row = &rows[0];
    assert_eq!(row.samples, 2, "冷门 category 仍可测，样本照常记录");
    assert_eq!(
        row.per_category_hit_rate.len(),
        1,
        "全局 0 命中类不参与评分"
    );
    assert!(
        (row.hit_rate - 1.0).abs() < 1e-9,
        "剔除全局 0 命中 category 后命中率不应被拉低"
    );
}

#[tokio::test]
async fn bench_health_skips_no_response_and_does_not_pollute_search_stats() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    create_health_tables(&pool).await;

    // 已有真实搜索统计：体检不能改动 search_count / hit_count。
    sqlx::query(
        "INSERT INTO source_health \
         (source_url, last_status, search_count, hit_count, parse_ok_count, tier, score) \
         VALUES ('https://ok', 'ok', 10, 1, 8, 'observing', 60)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut src = BookSource::default();
    src.book_source_url = "https://ok".into();
    src.book_source_name = "ok".into();
    let by_url = HashMap::from([("https://ok".to_string(), src)]);

    let rows = vec![
        bench_row("https://ok", 2, 0, 0.0, 80),
        // 整批无有效响应：不应写入健康表。
        bench_row("https://dead", 0, 0, 0.0, 0),
    ];
    record_bench_health(&pool, &rows, &by_url, &Config::from_env())
        .await
        .unwrap();

    let (status, search_count, hit_count, last_ok): (String, i64, i64, Option<i64>) =
        sqlx::query_as(
            "SELECT last_status, search_count, hit_count, last_ok_at \
             FROM source_health WHERE source_url = 'https://ok'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, source_health::STATUS_OK);
    assert_eq!(search_count, 10, "体检 usage=false，不增加 search_count");
    assert_eq!(hit_count, 1, "体检命中不进入用户搜索 hit_count");
    assert!(last_ok.is_some(), "有有效响应应刷新 last_ok_at");

    let dead: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM source_health WHERE source_url = 'https://dead' LIMIT 1")
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(dead.is_none(), "无有效响应的源不写健康记录");
}

#[tokio::test]
async fn bench_fast_track_revives_quarantined_source() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    create_health_tables(&pool).await;

    let future = crate::source_health::now_ms() + 24 * 3600 * 1000;
    sqlx::query(
        "INSERT INTO source_health \
         (source_url, last_status, last_error, consecutive_failures, cooldown_until, \
          tier, score, fail_site_count) \
         VALUES ('https://revive', 'fail', 'bench: no usable response', 3, ?, \
                 'quarantined', 20, 3)",
    )
    .bind(future)
    .execute(&pool)
    .await
    .unwrap();

    let mut src = BookSource::default();
    src.book_source_url = "https://revive".into();
    src.book_source_name = "revive".into();
    let by_url = HashMap::from([("https://revive".to_string(), src)]);
    let rows = vec![bench_row("https://revive", 2, 2, 1.0, 90)];
    record_bench_health(&pool, &rows, &by_url, &Config::from_env())
        .await
        .unwrap();

    let (tier, cooldown, cf, fs, err): (String, Option<i64>, i64, i64, Option<String>) =
        sqlx::query_as(
            "SELECT tier, cooldown_until, consecutive_failures, fail_site_count, last_error \
             FROM source_health WHERE source_url = 'https://revive'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        tier,
        crate::source_quality::TIER_PREFERRED,
        "体检直通复活隔离源"
    );
    assert!(cooldown.is_none(), "复活时清除冷却");
    assert_eq!(cf, 0, "复活时清零连续失败");
    assert_eq!(fs, 0, "复活时清零站点失败率证据");
    assert!(err.is_none(), "复活时清除旧错误");
}

#[test]
fn bench_priority_puts_recovery_candidates_first() {
    let mut quarantined = source_health::HealthRow::default();
    quarantined.source_url = "https://q".into();
    quarantined.tier = crate::source_quality::TIER_QUARANTINED.into();

    let mut failing = source_health::HealthRow::default();
    failing.source_url = "https://f".into();
    failing.consecutive_failures = 2;

    let mut healthy = source_health::HealthRow::default();
    healthy.source_url = "https://ok".into();

    let health = HashMap::from([
        ("https://q".to_string(), quarantined),
        ("https://f".to_string(), failing),
        ("https://ok".to_string(), healthy),
    ]);
    assert_eq!(bench_priority(&health, "https://q"), 0, "隔离源最先跑");
    assert_eq!(bench_priority(&health, "https://f"), 1, "近期失败源次优先");
    assert_eq!(bench_priority(&health, "https://ok"), 2);
    assert_eq!(bench_priority(&health, "https://missing"), 2);

    let mut order = vec!["https://ok", "https://q", "https://f", "https://missing"];
    order.sort_by_key(|u| bench_priority(&health, u));
    assert_eq!(
        order,
        vec!["https://q", "https://f", "https://ok", "https://missing"],
        "排序后恢复候选在前，同优先级保持原顺序"
    );
}

#[tokio::test]
async fn bench_valid_response_demotes_quarantined_to_observing() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    create_health_tables(&pool).await;

    let future = crate::source_health::now_ms() + 24 * 3600 * 1000;
    sqlx::query(
        "INSERT INTO source_health \
         (source_url, last_status, last_error, consecutive_failures, cooldown_until, \
          tier, score, fail_site_count) \
         VALUES ('https://cold', 'fail', '全局搜索预算耗尽', 3, ?, \
                 'quarantined', 20, 3)",
    )
    .bind(future)
    .execute(&pool)
    .await
    .unwrap();

    let mut src = BookSource::default();
    src.book_source_url = "https://cold".into();
    src.book_source_name = "cold".into();
    let by_url = HashMap::from([("https://cold".to_string(), src)]);
    // 有有效响应（samples>0）但命中率 0、分数不够直通：不应直接 preferred。
    let rows = vec![bench_row("https://cold", 2, 0, 0.0, 40)];
    record_bench_health(&pool, &rows, &by_url, &Config::from_env())
        .await
        .unwrap();

    let (tier, cooldown, cf, fs): (String, Option<i64>, i64, i64) = sqlx::query_as(
        "SELECT tier, cooldown_until, consecutive_failures, fail_site_count \
         FROM source_health WHERE source_url = 'https://cold'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        tier,
        crate::source_quality::TIER_OBSERVING,
        "有响应但未达直通门槛 → 先降回 observing 重新参与搜索"
    );
    assert!(cooldown.is_none(), "降回 observing 时清除冷却");
    assert_eq!(cf, 0, "清零连续失败");
    assert_eq!(fs, 0, "清零历史站点失败证据");
}

fn bench_row(
    url: &str,
    samples: i64,
    hits: i64,
    hit_rate: f64,
    bench_score: i32,
) -> SourceBenchRow {
    SourceBenchRow {
        source_url: url.into(),
        name: url.into(),
        source_type: TYPE_NOVEL,
        samples,
        hits,
        hit_rate,
        avg_latency_ms: 100,
        errors: 0,
        error_kinds: serde_json::json!({}),
        bench_score,
        curated: false,
        excluded_samples: 0,
        per_category_hit_rate: vec![],
    }
}

async fn create_health_tables(pool: &SqlitePool) {
    sqlx::query(
        "CREATE TABLE source_health (\
         source_url TEXT PRIMARY KEY, last_checked_at INTEGER, \
         last_status TEXT NOT NULL DEFAULT 'never', last_error TEXT, \
         consecutive_failures INTEGER NOT NULL DEFAULT 0, last_ok_at INTEGER, \
         cooldown_until INTEGER, avg_latency_ms INTEGER, \
         check_total INTEGER NOT NULL DEFAULT 0, tier TEXT NOT NULL DEFAULT 'observing', \
         score INTEGER NOT NULL DEFAULT 50, evidence_json TEXT, last_eval_at INTEGER, \
         search_count INTEGER NOT NULL DEFAULT 0, hit_count INTEGER NOT NULL DEFAULT 0, \
         parse_ok_count INTEGER NOT NULL DEFAULT 0, \
         fail_site_count INTEGER NOT NULL DEFAULT 0, \
         fail_engine_count INTEGER NOT NULL DEFAULT 0, ttfb_ms_p90 INTEGER, \
         ttfb_samples_json TEXT, promote_streak INTEGER NOT NULL DEFAULT 0, \
         demote_streak INTEGER NOT NULL DEFAULT 0)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE source_curated (\
         source_url TEXT PRIMARY KEY, source_type INTEGER, bench_score INTEGER, \
         curated INTEGER NOT NULL DEFAULT 0, samples INTEGER NOT NULL DEFAULT 0, \
         fail_streak INTEGER NOT NULL DEFAULT 0, last_bench_at INTEGER, \
         meta_json TEXT, last_run_at INTEGER)",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[test]
fn batch_budget_scales_with_source_count() {
    assert_eq!(
        batch_budget_secs(1, 5, BENCH_CONCURRENCY),
        BATCH_BUDGET_SECS,
        "单源仍用 60s"
    );
    assert_eq!(
        batch_budget_secs(139, 5, BENCH_CONCURRENCY),
        434,
        "139 源×5 样本：手动并发 8 按最坏情况估算"
    );
    assert_eq!(
        batch_budget_secs(200, 20, BENCH_CONCURRENCY),
        MAX_BATCH_BUDGET_SECS,
        "封顶 600s"
    );
    assert_eq!(
        batch_budget_secs(139, 5, BENCH_CONCURRENCY_BACKGROUND),
        MAX_BATCH_BUDGET_SECS,
        "后台并发 4：估算超过 600s，按封顶值"
    );
}

#[test]
fn timeout_samples_are_not_scored_as_fast() {
    let corpus = vec![entry("正常1", "玄幻", TYPE_NOVEL, true)];
    let mut src = BookSource::default();
    src.book_source_url = "https://dead".into();
    src.book_source_name = "dead".into();
    let sources = HashMap::from([("https://dead".to_string(), src)]);
    let cfg = Config::from_env();
    let mut runs = Vec::new();
    for _ in 0..5 {
        let mut r = run("https://dead", "玄幻", "正常1", false, false);
        r.latency_ms = 0;
        r.error_kind = Some("timeout".into());
        runs.push(r);
    }
    let rows = compute_source_rows(
        &runs,
        &sources,
        &corpus,
        &cfg,
        &HashMap::new(),
        &HashMap::new(),
    );
    let row = &rows[0];
    assert_eq!(row.samples, 0, "超时样本不算有效样本");
    assert_eq!(row.errors, 5);
    assert_eq!(row.bench_score, 0, "全超时不能靠 latency=0 拿到 30 分基线");
}

#[test]
fn curated_never_includes_non_preferred_or_low_samples() {
    let scores = vec![95, 94, 90];
    let cfg = Config::from_env();
    let eligible: HashSet<String> = ["https://a".into(), "https://c".into()]
        .into_iter()
        .collect();
    let rows = vec![
        SourceBenchRow {
            source_url: "https://a".into(),
            name: "a".into(),
            source_type: TYPE_NOVEL,
            samples: 5,
            hits: 5,
            hit_rate: 1.0,
            avg_latency_ms: 100,
            errors: 0,
            error_kinds: serde_json::json!({}),
            bench_score: 95,
            curated: false,
            excluded_samples: 0,
            per_category_hit_rate: vec![],
        },
        SourceBenchRow {
            source_url: "https://b".into(),
            name: "b".into(),
            source_type: TYPE_NOVEL,
            samples: 5,
            hits: 5,
            hit_rate: 1.0,
            avg_latency_ms: 100,
            errors: 0,
            error_kinds: serde_json::json!({}),
            bench_score: 94,
            curated: false,
            excluded_samples: 0,
            per_category_hit_rate: vec![],
        },
        SourceBenchRow {
            source_url: "https://c".into(),
            name: "c".into(),
            source_type: TYPE_NOVEL,
            samples: 1,
            hits: 1,
            hit_rate: 1.0,
            avg_latency_ms: 100,
            errors: 0,
            error_kinds: serde_json::json!({}),
            bench_score: 90,
            curated: false,
            excluded_samples: 0,
            per_category_hit_rate: vec![],
        },
    ];
    let sel = select_curated(&rows, &cfg, &eligible);
    let urls: Vec<&str> = sel.novel.iter().map(|i| i.source_url.as_str()).collect();
    assert_eq!(
        urls,
        vec!["https://a"],
        "b 非 preferred、c 样本不足都不参与"
    );
    let _ = scores;
}

fn srow(url: &str, ty: i64, score: i32, hit_rate: f64) -> SourceBenchRow {
    SourceBenchRow {
        source_url: url.into(),
        name: url.into(),
        source_type: ty,
        samples: 5,
        hits: (hit_rate * 5.0).round() as i64,
        hit_rate,
        avg_latency_ms: 300,
        errors: 0,
        error_kinds: serde_json::json!({}),
        bench_score: score,
        curated: false,
        excluded_samples: 0,
        per_category_hit_rate: vec![],
    }
}

#[test]
fn curated_requires_nonzero_hit_rate() {
    let cfg = Config::from_env();
    let eligible: HashSet<String> = ["https://h1", "https://h2", "https://z"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let rows = vec![
        srow("https://h1", TYPE_COMIC, 86, 0.8),
        srow("https://h2", TYPE_COMIC, 85, 0.8),
        srow("https://z", TYPE_COMIC, 29, 0.0),
    ];
    let sel = select_curated(&rows, &cfg, &eligible);
    let urls: Vec<&str> = sel.comic.iter().map(|i| i.source_url.as_str()).collect();
    assert_eq!(
        urls,
        vec!["https://h1", "https://h2"],
        "0 命中源不入选，comic min=3 也不拿它凑数"
    );
    assert!(!sel.flags.get("https://z").copied().unwrap_or(false));
}

#[test]
fn low_score_but_hitting_source_still_selected() {
    let cfg = Config::from_env();
    let eligible: HashSet<String> = ["https://a", "https://b", "https://c"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let rows = vec![
        srow("https://a", TYPE_NOVEL, 31, 0.2),
        srow("https://b", TYPE_NOVEL, 30, 0.2),
        srow("https://c", TYPE_NOVEL, 29, 0.2),
    ];
    let sel = select_curated(&rows, &cfg, &eligible);
    let urls: Vec<&str> = sel.novel.iter().map(|i| i.source_url.as_str()).collect();
    assert_eq!(
        urls,
        vec!["https://a", "https://b", "https://c"],
        "命中率>0 的低分源仍可入选，断层规则不变"
    );
}

#[test]
fn meta_hit_rate_reads_last_bench_rate() {
    assert_eq!(
        meta_hit_rate(Some(r#"{"batch_id":"b1","hit_rate":0.8}"#)),
        0.8
    );
    assert_eq!(
        meta_hit_rate(Some(r#"{"batch_id":"b1","hit_rate":0}"#)),
        0.0
    );
    assert_eq!(meta_hit_rate(None), 0.0, "无 meta 视为未体检，不具备资格");
    assert_eq!(meta_hit_rate(Some("not json")), 0.0);
    assert!(!curated_hit_ok(0.0));
    assert!(curated_hit_ok(0.2), "1/5 命中仍算有命中，只是分数低");
}
