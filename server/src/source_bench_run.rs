//! source_bench 的体检执行与评分（move-only 从 source_bench.rs 拆出）。

use super::*;

pub(super) struct RawSample {
    pub(super) corpus_key: String,
    pub(super) title: String,
    pub(super) category: String,
    pub(super) builtin_sample: bool,
    pub(super) found: bool,
    pub(super) title_match: bool,
    pub(super) author_match: bool,
    pub(super) latency_ms: i64,
    pub(super) error_kind: Option<String>,
}

/// 每条语料最多看搜索结果的前 N 条。
/// 2026-11 复测：10 个 0 命中源里，看书网《庆余年》/顶点小说《全职高手》的**第一条**
/// 就是精确同名同作者，而体检当时只取第一条却记成未命中（排名/接口抖动）。
/// 放宽到前 3–5 条可消除这类漏判；`is_hit` 口径保持不变（仍要求近似同名，或包含+作者匹配）。
pub(super) const CHECK_TOP_N: usize = 5;

pub(super) fn judge_books(books: &[SearchBook], item: &CorpusEntry) -> (bool, bool, bool) {
    let mut found = false;
    let mut title_match = false;
    let mut author_match = false;
    for b in books.iter().take(CHECK_TOP_N) {
        let Some(name) = b.name.as_deref() else {
            continue;
        };
        let tm = title_matches(name, &item.title);
        title_match |= tm;
        let am = author_matches(b.author.as_deref(), item.expected_author.as_deref());
        author_match |= tm && am;
        found |= is_hit(
            name,
            &item.title,
            b.author.as_deref(),
            item.expected_author.as_deref(),
        );
    }
    (found, title_match, author_match)
}

pub(super) async fn run_one_sample(
    src: &BookSource,
    item: &CorpusEntry,
    deadline: Instant,
) -> RawSample {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining < Duration::from_millis(100) {
        return RawSample {
            corpus_key: item.id.clone(),
            title: item.title.clone(),
            category: item.category.clone(),
            builtin_sample: item.builtin,
            found: false,
            title_match: false,
            author_match: false,
            latency_ms: 0,
            error_kind: Some("timeout".into()),
        };
    }
    let budget = Duration::from_secs(PER_SAMPLE_BUDGET_SECS).min(remaining);
    let started = Instant::now();
    let result = tokio::time::timeout(budget, web_book::search(src, &item.title, 1)).await;
    let latency_ms = started.elapsed().as_millis() as i64;
    match result {
        Ok(Ok(books)) => {
            let (found, title_match, author_match) = judge_books(&books, item);
            RawSample {
                corpus_key: item.id.clone(),
                title: item.title.clone(),
                category: item.category.clone(),
                builtin_sample: item.builtin,
                found,
                title_match,
                author_match,
                latency_ms,
                error_kind: None,
            }
        }
        Ok(Err(e)) => {
            let kind = match classify(&e) {
                FailureKind::Site => "network",
                FailureKind::Engine => "parse",
            };
            tracing::debug!(source = %src.book_source_url, title = %item.title, error = %e, "bench sample failed");
            RawSample {
                corpus_key: item.id.clone(),
                title: item.title.clone(),
                category: item.category.clone(),
                builtin_sample: item.builtin,
                found: false,
                title_match: false,
                author_match: false,
                latency_ms,
                error_kind: Some(kind.to_string()),
            }
        }
        Err(_) => RawSample {
            corpus_key: item.id.clone(),
            title: item.title.clone(),
            category: item.category.clone(),
            builtin_sample: item.builtin,
            found: false,
            title_match: false,
            author_match: false,
            latency_ms,
            error_kind: Some("timeout".into()),
        },
    }
}

pub(super) fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = v.len() / 2;
    if v.len().is_multiple_of(2) {
        (v[mid - 1] + v[mid]) / 2.0
    } else {
        v[mid]
    }
}

/// 各 category 的全局命中率（仅内置、计分、观测数达到阈值的条目）。
///
/// 用于判断“这类语料是否整体与当前源池匹配”：全局 0 命中的 category
/// 不能用来区分源质量，否则会无差别拉低所有源的 bench_score。
pub(super) fn category_hit_rates(
    runs: &[BenchRun],
    corpus: &[CorpusEntry],
) -> HashMap<String, f64> {
    let mut per_key: HashMap<&str, (i64, i64)> = HashMap::new();
    for r in runs {
        let e = per_key.entry(r.corpus_key.as_str()).or_insert((0, 0));
        e.1 += 1;
        if r.found {
            e.0 += 1;
        }
    }
    let mut cat_rates: HashMap<String, Vec<f64>> = HashMap::new();
    for e in corpus.iter().filter(|e| e.builtin && e.scored) {
        if let Some((hits, obs)) = per_key.get(e.id.as_str()) {
            if *obs >= MIN_CURATED_SAMPLES {
                cat_rates
                    .entry(e.category.clone())
                    .or_default()
                    .push(*hits as f64 / *obs as f64);
            }
        }
    }
    cat_rates
        .into_iter()
        .map(|(k, v)| (k, median(&v)))
        .collect()
}

/// 标记“语料可能失效”的条目：命中率显著低于同 category 中位数（< 中位数 × 0.5）。
/// 只影响评分口径，run 明细全部保留。
pub(super) fn mark_suspicious(runs: &mut [BenchRun], corpus: &[CorpusEntry]) {
    let mut per_key: HashMap<String, (i64, i64, String)> = HashMap::new();
    for r in runs.iter() {
        let e = per_key
            .entry(r.corpus_key.clone())
            .or_insert((0, 0, r.category.clone()));
        e.1 += 1;
        if r.found {
            e.0 += 1;
        }
    }
    // 同 category 中位数只用内置且计分的样本计算
    // （用户冷门书名、出版/实体书这类 scored=false 的条目都不应拉低基线）
    let medians = category_hit_rates(runs, corpus);
    let mut flagged: HashSet<String> = HashSet::new();
    for (key, (hits, obs, category)) in per_key.iter() {
        if *obs < MIN_CURATED_SAMPLES {
            continue;
        }
        let Some(med) = medians.get(category).copied() else {
            continue;
        };
        if med <= 0.0 {
            continue;
        }
        let rate = *hits as f64 / *obs as f64;
        if rate < med * 0.5 {
            flagged.insert(key.clone());
        }
    }
    for r in runs.iter_mut() {
        if flagged.contains(&r.corpus_key) {
            r.suspicious = true;
        }
    }
}

pub(super) fn compute_source_rows(
    runs: &[BenchRun],
    sources: &HashMap<String, BookSource>,
    corpus: &[CorpusEntry],
    cfg: &Config,
    health: &HashMap<String, source_health::HealthRow>,
    prev_fail_streak: &HashMap<String, i64>,
) -> Vec<SourceBenchRow> {
    let mut by_source: HashMap<String, Vec<&BenchRun>> = HashMap::new();
    for r in runs {
        by_source.entry(r.source_url.clone()).or_default().push(r);
    }
    // 参与计分的 category：每个 type 下至少含 1 条内置、且 scored=true 的样本
    let mut scoring_cats: HashMap<i64, HashSet<String>> = HashMap::new();
    for e in corpus.iter().filter(|e| e.builtin && e.scored) {
        scoring_cats
            .entry(e.source_type)
            .or_default()
            .insert(e.category.clone());
    }
    // scored=false 的内置条目：照常跑、照常落库，但不进入 samples/hits/latency/分数。
    let unscored_keys: HashSet<&str> = corpus
        .iter()
        .filter(|e| !e.scored)
        .map(|e| e.id.as_str())
        .collect();

    // 全局 category 命中率：用于剔除“所有源都搜不到”的语料类别。
    let global_cat = category_hit_rates(runs, corpus);
    let mut out: Vec<SourceBenchRow> = Vec::new();
    for (url, list) in by_source.iter() {
        let src = sources.get(url);
        let name = src
            .map(|s| s.book_source_name.clone())
            .unwrap_or_else(|| url.clone());
        let source_type = src.map(|s| s.book_source_type).unwrap_or(TYPE_NOVEL);
        let total = list.len() as i64;
        let excluded = list.iter().filter(|r| r.suspicious).count() as i64;
        let usable: Vec<&BenchRun> = list
            .iter()
            .filter(|r| !r.suspicious && !unscored_keys.contains(r.corpus_key.as_str()))
            .copied()
            .collect();
        let obs = usable.len() as i64;
        // 只有真正拿到响应的样本才算“有效样本”：
        // 体检整批 deadline 到了还没发出的请求不能算样本，更不能按 latency=0 拿满分。
        let ok: Vec<&BenchRun> = usable
            .iter()
            .filter(|r| r.error_kind.is_none())
            .copied()
            .collect();
        let samples = ok.len() as i64;
        let hits = ok.iter().filter(|r| r.found).count() as i64;
        let errors = obs - samples;
        let avg_latency = if samples == 0 {
            (PER_SAMPLE_BUDGET_SECS * 1000) as i64
        } else {
            ok.iter().map(|r| r.latency_ms.max(0)).sum::<i64>() / samples
        };
        let mut kinds: HashMap<String, i64> = HashMap::new();
        for r in usable.iter() {
            if let Some(k) = r.error_kind.as_deref() {
                *kinds.entry(k.to_string()).or_insert(0) += 1;
            }
        }
        // 按 category 聚合命中率（单条冷门书名不会单独把源判死）；错误样本不计入分母。
        let cats = scoring_cats.get(&source_type).cloned().unwrap_or_default();
        let mut per_cat: Vec<CategoryRate> = Vec::new();
        for cat in cats.iter() {
            // 整个 category 在所有源上都 0 命中（或观测不足）→ 语料与源池不匹配，
            // 不进入评分；否则所有源的 hit_rate 会被同一类冷门语料无差别拉低。
            if global_cat.get(cat).copied().unwrap_or(0.0) <= 0.0 {
                continue;
            }
            let cs = ok.iter().filter(|r| r.category == *cat).count() as i64;
            if cs == 0 {
                continue;
            }
            let ch = ok.iter().filter(|r| r.category == *cat && r.found).count() as i64;
            per_cat.push(CategoryRate {
                category: cat.clone(),
                samples: cs,
                hits: ch,
                hit_rate: ch as f64 / cs as f64,
            });
        }
        let hit_rate = if per_cat.is_empty() {
            0.0
        } else {
            per_cat.iter().map(|c| c.hit_rate).sum::<f64>() / per_cat.len() as f64
        };
        let latency_score = if samples == 0 {
            0.0
        } else {
            1.0 - (avg_latency as f64 / (PER_SAMPLE_BUDGET_SECS * 1000) as f64).clamp(0.0, 1.0)
        };
        let success = if obs == 0 {
            0.0
        } else {
            1.0 - errors as f64 / obs as f64
        };
        let bench_score = (cfg.source_bench_w_hit as f64 * hit_rate
            + cfg.source_bench_w_latency as f64 * latency_score
            + cfg.source_bench_w_fail as f64 * success)
            .round()
            .clamp(0.0, 100.0) as i32;
        let _ = (total, prev_fail_streak, health); // 精选资格在 recompute_curated 阶段统一判定
        out.push(SourceBenchRow {
            source_url: url.clone(),
            name,
            source_type,
            samples,
            hits,
            hit_rate: (hit_rate * 1000.0).round() / 1000.0,
            avg_latency_ms: avg_latency,
            errors,
            error_kinds: serde_json::to_value(&kinds).unwrap_or(serde_json::Value::Null),
            bench_score,
            curated: false,
            excluded_samples: excluded,
            per_category_hit_rate: per_cat,
        });
    }
    out.sort_by(|a, b| {
        b.bench_score
            .cmp(&a.bench_score)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// 断层截断：分数降序，遇到相邻差 >= gap 就在断点截断，数量夹在 min..max。
pub fn pick_curated_count(sorted_scores: &[i32], min: usize, max: usize, gap: i32) -> usize {
    let n = sorted_scores.len();
    if n == 0 {
        return 0;
    }
    let lo = min.max(1);
    let hi = max.max(lo);
    if n <= lo {
        return n;
    }
    let cap = hi.min(n);
    let mut i = lo - 1;
    while i < cap {
        let next = i + 1;
        if next >= n {
            return cap;
        }
        if sorted_scores[i] - sorted_scores[next] >= gap {
            return next;
        }
        i += 1;
    }
    cap
}

/// 精选清单的中间结构；生产路径直接写库，本结构与 `select_curated` 仅单测使用。
#[allow(dead_code)]
pub(super) struct CuratedSelection {
    pub(super) novel: Vec<CuratedItem>,
    pub(super) comic: Vec<CuratedItem>,
    pub(super) flags: HashMap<String, bool>,
}

/// 按配置挑出精选源；生产路径在入库阶段完成同样筛选，本函数仅单测使用。
#[allow(dead_code)]
pub(super) fn select_curated(
    rows: &[SourceBenchRow],
    cfg: &Config,
    eligible: &HashSet<String>,
) -> CuratedSelection {
    let mut result = CuratedSelection {
        novel: Vec::new(),
        comic: Vec::new(),
        flags: HashMap::new(),
    };
    for source_type in [TYPE_NOVEL, TYPE_COMIC] {
        let mut pool: Vec<&SourceBenchRow> = rows
            .iter()
            .filter(|r| r.source_type == source_type && r.samples >= MIN_CURATED_SAMPLES)
            .filter(|r| curated_hit_ok(r.hit_rate))
            .filter(|r| eligible.contains(&r.source_url))
            .collect();
        pool.sort_by(|a, b| {
            b.bench_score
                .cmp(&a.bench_score)
                .then_with(|| a.source_url.cmp(&b.source_url))
        });
        let scores: Vec<i32> = pool.iter().map(|r| r.bench_score).collect();
        let (min, max) = curated_bounds(cfg, source_type);
        let count = pick_curated_count(&scores, min, max, cfg.source_bench_gap);
        let cut_gap = if count > 0 && count < pool.len() {
            Some(scores[count - 1] - scores[count])
        } else {
            None
        };
        let mut items: Vec<CuratedItem> = Vec::new();
        for (idx, r) in pool.iter().take(count).enumerate() {
            result.flags.insert(r.source_url.clone(), true);
            items.push(CuratedItem {
                rank: idx + 1,
                source_url: r.source_url.clone(),
                name: r.name.clone(),
                source_type,
                bench_score: r.bench_score,
                samples: r.samples,
                cut_gap: if idx + 1 == count { cut_gap } else { None },
            });
        }
        for r in pool.iter().skip(count) {
            result.flags.insert(r.source_url.clone(), false);
        }
        if source_type == TYPE_NOVEL {
            result.novel = items;
        } else {
            result.comic = items;
        }
    }
    result
}

/// 执行一次体检。`source_ids` 为 None 表示全部启用源（排除隔离区）。
/// `concurrency` 由调用方区分手动（[`BENCH_CONCURRENCY`]）与后台（[`BENCH_CONCURRENCY_BACKGROUND`]）。
pub async fn run_bench(
    pool: &SqlitePool,
    cfg: &Config,
    source_ids: Option<Vec<String>>,
    limit: Option<usize>,
    progress: Option<Arc<BenchProgress>>,
    concurrency: usize,
) -> AppResult<BenchReport> {
    let started_at = now_ms();
    let started = Instant::now();
    let batch_seq = next_seq();
    let batch_id = format!("b{}-{}", started_at, batch_seq);

    let all = crate::routes::sources::load_sources(pool).await?;
    let by_url: HashMap<String, BookSource> = all
        .iter()
        .cloned()
        .map(|s| (s.book_source_url.clone(), s))
        .collect();
    let states = crate::source_ledger::load_states(pool)
        .await
        .unwrap_or_default();
    let quarantined: HashSet<String> = states
        .iter()
        .filter(|(_, st)| st.is_quarantined())
        .map(|(url, _)| url.clone())
        .collect();
    let auto_quarantined: HashSet<String> = states
        .iter()
        .filter(|(_, st)| {
            st.is_quarantined()
                && crate::source_ledger::is_auto_quarantine_reason(st.reason.as_deref())
        })
        .map(|(url, _)| url.clone())
        .collect();
    let corpus = load_corpus(pool).await?;
    let samples_per_source = limit.unwrap_or(cfg.source_bench_samples).clamp(1, 20);
    let health = source_health::load_map(pool).await.unwrap_or_default();

    let mut sources: Vec<BookSource> = Vec::new();
    match source_ids {
        None => {
            sources = bench_candidates(&all, &quarantined, &auto_quarantined);
        }
        Some(ids) => {
            for id in ids {
                if let Some(s) = by_url.get(&id) {
                    // 显式点名也不扫描需登录源：登录墙下探测只会得到噪声失败。
                    if !crate::entities::requires_login(s) {
                        sources.push(s.clone());
                    }
                }
            }
        }
    }
    // 恢复优先：自动隔离源最先跑，其次是被质量档隔离/近期失败的源。
    // 整批预算紧张时（源多、并发低）队尾样本会被截断，若不排序，
    // 最需要体检复活的源反而永远排在最后跑不到。
    sources.sort_by_key(|s| {
        let auto = auto_quarantined.contains(&s.book_source_url);
        (
            if auto { 0u8 } else { 1u8 },
            bench_priority(&health, &s.book_source_url),
        )
    });

    let corpus_for_type = |t: i64| -> Vec<CorpusEntry> {
        corpus
            .iter()
            .filter(|e| e.source_type == t)
            .cloned()
            .collect()
    };
    let novel_corpus = corpus_for_type(TYPE_NOVEL);
    let comic_corpus = corpus_for_type(TYPE_COMIC);

    let concurrency = concurrency.max(1);
    let deadline = started
        + Duration::from_secs(batch_budget_secs(
            sources.len(),
            samples_per_source,
            concurrency,
        ));
    let prev_streak: HashMap<String, i64> =
        sqlx::query_as::<_, (String, i64)>("SELECT source_url, fail_streak FROM source_curated")
            .fetch_all(pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();

    // 每个源内部顺序执行 N 条；源之间用 buffer_unordered 限制在 BENCH_CONCURRENCY。
    // 真正的 HTTP 并发仍由 req 的全局信号量（HTTP_CONCURRENCY）兜底。
    if let Some(p) = &progress {
        p.total.store(sources.len(), Ordering::SeqCst);
    }
    let mut tasks = futures::stream::iter(sources.into_iter().enumerate().map(|(idx, src)| {
        let novel = novel_corpus.clone();
        let comic = comic_corpus.clone();
        let task_progress = progress.clone();
        async move {
            let pool_corpus = if src.book_source_type == TYPE_COMIC {
                comic
            } else {
                novel
            };
            let picked = pick_samples(
                &pool_corpus,
                src.book_source_type,
                samples_per_source,
                batch_seq as usize + idx,
            );
            let mut out = Vec::with_capacity(picked.len());
            for item in picked {
                if Instant::now() >= deadline {
                    break;
                }
                let sample = run_one_sample(&src, &item, deadline).await;
                out.push((src.book_source_url.clone(), src.book_source_type, sample));
            }
            if let Some(p) = &task_progress {
                p.done.fetch_add(1, Ordering::SeqCst);
            }
            out
        }
    }))
    .buffer_unordered(concurrency)
    .collect::<Vec<_>>()
    .await;

    let run_at = now_ms();
    let mut runs: Vec<BenchRun> = Vec::new();
    for list in tasks.iter_mut() {
        for (url, source_type, s) in list.drain(..) {
            runs.push(BenchRun {
                batch_id: batch_id.clone(),
                source_url: url,
                corpus_key: s.corpus_key,
                title: s.title,
                category: s.category,
                source_type,
                builtin_sample: s.builtin_sample,
                found: s.found,
                title_match: s.title_match,
                author_match: s.author_match,
                latency_ms: s.latency_ms,
                error_kind: s.error_kind,
                suspicious: false,
                run_at,
            });
        }
    }

    mark_suspicious(&mut runs, &corpus);
    let mut suspicious: Vec<SuspiciousCorpus> = Vec::new();
    {
        let mut per_key: HashMap<String, (i64, i64, String, String)> = HashMap::new();
        for r in runs.iter().filter(|r| r.suspicious) {
            let e = per_key.entry(r.corpus_key.clone()).or_insert((
                0,
                0,
                r.category.clone(),
                r.title.clone(),
            ));
            e.1 += 1;
            if r.found {
                e.0 += 1;
            }
        }
        let medians = category_medians(&runs, &corpus);
        for (key, (hits, obs, category, title)) in per_key.into_iter() {
            let med = medians.get(&category).copied().unwrap_or(0.0);
            suspicious.push(SuspiciousCorpus {
                corpus_key: key,
                title,
                category,
                hit_rate: if obs > 0 {
                    (hits as f64 / obs as f64 * 1000.0).round() / 1000.0
                } else {
                    0.0
                },
                category_median: (med * 1000.0).round() / 1000.0,
                observations: obs,
                reason: "命中率低于同 category 中位数的 50%，判定为“语料可能失效”，已排除出源评分"
                    .into(),
            });
        }
        suspicious.sort_by(|a, b| {
            a.category
                .cmp(&b.category)
                .then_with(|| a.title.cmp(&b.title))
        });
    }

    let mut rows = compute_source_rows(&runs, &by_url, &corpus, cfg, &health, &prev_streak);

    // 落库批次明细
    let mut tx = pool.begin().await?;
    for r in runs.iter() {
        sqlx::query(
            "INSERT INTO source_bench_runs \
             (batch_id, source_url, corpus_key, title, category, source_type, builtin_sample, \
              found, title_match, author_match, latency_ms, error_kind, suspicious, run_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&r.batch_id)
        .bind(&r.source_url)
        .bind(&r.corpus_key)
        .bind(&r.title)
        .bind(&r.category)
        .bind(r.source_type)
        .bind(r.builtin_sample)
        .bind(r.found)
        .bind(r.title_match)
        .bind(r.author_match)
        .bind(r.latency_ms)
        .bind(&r.error_kind)
        .bind(r.suspicious)
        .bind(r.run_at)
        .execute(&mut *tx)
        .await?;
    }
    // 当前状态写入 source_curated（保留 fail_streak 供“连续 2 批失败退出精选”）
    for row in rows.iter() {
        let all_failed = row.samples > 0 && row.errors == row.samples;
        let prev = prev_streak.get(&row.source_url).copied().unwrap_or(0);
        let fail_streak = if all_failed {
            prev.saturating_add(1)
        } else {
            0
        };
        sqlx::query(
            "INSERT INTO source_curated \
             (source_url, source_type, bench_score, curated, samples, fail_streak, last_bench_at, meta_json) \
             VALUES (?, ?, ?, 0, ?, ?, ?, ?) \
             ON CONFLICT(source_url) DO UPDATE SET \
              source_type=excluded.source_type, bench_score=excluded.bench_score, \
              samples=excluded.samples, fail_streak=excluded.fail_streak, \
              last_bench_at=excluded.last_bench_at, meta_json=excluded.meta_json",
        )
        .bind(&row.source_url)
        .bind(row.source_type)
        .bind(row.bench_score)
        .bind(row.samples)
        .bind(fail_streak)
        .bind(run_at)
        .bind(serde_json::json!({ "batch_id": batch_id, "hit_rate": row.hit_rate, "avg_latency_ms": row.avg_latency_ms }).to_string())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    // 体检也是真实搜索：把本批结果回写健康观测，并给冷启动一条“体检直通 preferred”的通路。
    // 失败只告警，不让健康写回拖垮体检结果本身。
    if let Err(e) = record_bench_health(pool, &rows, &by_url, cfg).await {
        tracing::warn!(error = %e, "record bench health failed");
    }

    // 重算精选：用当前库内最新分数（含本次批次），资格要求 preferred + 样本足够 + 未连续失败 2 批。
    let (novel_sel, comic_sel) = recompute_curated(pool, cfg).await?;
    let mut flags: HashMap<String, bool> = HashMap::new();
    for item in novel_sel.iter().chain(comic_sel.iter()) {
        flags.insert(item.source_url.clone(), true);
    }
    for row in rows.iter_mut() {
        row.curated = flags.get(&row.source_url).copied().unwrap_or(false);
    }
    // 用库内重算结果覆盖返回里的精选清单
    let novel_items = novel_sel;
    let comic_items = comic_sel;

    let finished_at = now_ms();
    Ok(BenchReport {
        batch_id,
        started_at,
        finished_at,
        elapsed_ms: finished_at - started_at,
        interrupted: Instant::now() >= deadline,
        corpus_total: corpus.len(),
        samples_per_source,
        sources: rows,
        novel_curated: novel_items,
        comic_curated: comic_items,
        excluded_from_score: runs.iter().filter(|r| r.suspicious).count() as i64,
        suspicious,
    })
}

/// 体检排队优先级：数字越小越先跑。
///
/// - `0`：质量档已被隔离（`quarantined`）——最需要一次有效体检来复活；
/// - `1`：观察档但有连续失败 / 最近一次失败——次优先，防止体检没跑到就继续恶化；
/// - `2`：其余（preferred / 正常 observing / 无健康行）。
pub(super) fn bench_priority(health: &HashMap<String, source_health::HealthRow>, url: &str) -> u8 {
    match health.get(url) {
        Some(h) if h.tier == crate::source_quality::TIER_QUARANTINED => 0,
        Some(h)
            if h.tier == crate::source_quality::TIER_OBSERVING
                && (h.consecutive_failures > 0 || h.last_status == source_health::STATUS_FAIL) =>
        {
            1
        }
        _ => 2,
    }
}

/// 体检候选源：正常可搜索源 + 自动隔离源。
///
/// 自动隔离源在导入时被强制 `enabled=0`，如果只按“可搜索源”筛选，
/// 它们永远进不了体检，也就永远无法自愈。人工隔离 / 清理隔离仍排除在外，
/// 避免后台体检推翻用户或清理策略的显式决定。
pub(super) fn bench_candidates(
    all: &[BookSource],
    quarantined: &HashSet<String>,
    auto_quarantined: &HashSet<String>,
) -> Vec<BookSource> {
    all.iter()
        .filter(|s| {
            if quarantined.contains(&s.book_source_url) {
                auto_quarantined.contains(&s.book_source_url) && bench_static_eligible(s)
            } else {
                crate::entities::is_searchable_source(s)
            }
        })
        .cloned()
        .collect()
}

/// 忽略 `enabled` 的规则层可体检判定（自动隔离源需要绕过导入时写下的 enabled=0）。
fn bench_static_eligible(s: &BookSource) -> bool {
    crate::entities::is_supported_source_type(s.book_source_type)
        && s.rule_search.is_some()
        && s.search_url.is_some()
        && !crate::entities::requires_login(s)
}

/// 冷启动直通的纯规则，供单测与写库共用。
pub(super) fn bench_fast_track_ok(row: &SourceBenchRow) -> bool {
    row.samples >= MIN_CURATED_SAMPLES
        && row.hit_rate > 0.0
        && row.bench_score >= BENCH_FAST_TRACK_MIN_SCORE
}

/// 体检回写健康：每源每批折合成 1 条观测，再按 [`BENCH_FAST_TRACK_MIN_SCORE`] 直通 preferred。
///
/// - **只写“有有效响应”的源**（`samples > 0`）：整批超时/没拿到响应的源不写健康，
///   避免体检把暂时连不上的源打成 `partial` 并累计引擎失败。
/// - 有有效响应 → `ok`，但 `usage=false`：体检命中不计入用户搜索的 hit_rate，
///   否则冷门语料会无差别拉低所有源的健康分。
/// - 直通 preferred 现在也允许复活隔离源（显式清理失败计数与冷却），
///   否则一旦被误隔离就永远进不了搜索，无法靠体检自愈。
pub(super) async fn record_bench_health(
    pool: &SqlitePool,
    rows: &[SourceBenchRow],
    by_url: &HashMap<String, BookSource>,
    cfg: &Config,
) -> AppResult<()> {
    let tier_cfg = crate::source_quality::TierConfig::from_config(cfg);
    let outcomes: Vec<Outcome> = rows
        .iter()
        .filter(|r| r.samples > 0)
        .map(|r| {
            let rule_complete = by_url
                .get(&r.source_url)
                .map(crate::source_quality::rule_completeness)
                .unwrap_or(0.0);
            Outcome {
                source_url: r.source_url.clone(),
                status: source_health::STATUS_OK,
                latency_ms: r.avg_latency_ms,
                error: None,
                hits: r.hits,
                // 体检的 hits 已经是语料标题匹配结果，与 kept 同口径。
                kept_hits: r.hits,
                parsed: true,
                // 体检不是用户搜索：只更新可用性/延迟，不污染 hit_rate / yield。
                usage: false,
                rule_complete,
            }
        })
        .collect();
    source_health::record_outcomes(pool, &outcomes, cfg.source_cooldown_secs, &tier_cfg).await?;

    let now = now_ms();
    let promote = cfg.source_tier_promote_streak.max(1);
    for r in rows.iter().filter(|r| bench_fast_track_ok(r)) {
        sqlx::query(
            "UPDATE source_health SET tier = ?1, promote_streak = MAX(promote_streak, ?2), \
             demote_streak = 0, consecutive_failures = 0, fail_site_count = 0, \
             cooldown_until = NULL, last_error = NULL, last_eval_at = ?3 \
             WHERE source_url = ?4",
        )
        .bind(crate::source_quality::TIER_PREFERRED)
        .bind(promote)
        .bind(now)
        .bind(&r.source_url)
        .execute(pool)
        .await?;
    }
    // 有有效响应但未达到直通门槛：被质量档隔离的源先降回 observing，重新参与用户搜索。
    // 隔离通常源于历史误判（如预算耗尽被记成站点失败）；体检连续拿到响应即视为可用。
    // 同时清零连续失败与站点失败证据，给源一个干净的重评起点；后续真实搜索仍无命中会被再次评分/隔离。
    for r in rows
        .iter()
        .filter(|r| r.samples > 0 && !bench_fast_track_ok(r))
    {
        sqlx::query(
            "UPDATE source_health SET tier = ?1, demote_streak = 0, \
             consecutive_failures = 0, fail_site_count = 0, \
             cooldown_until = NULL, last_error = NULL, last_eval_at = ?2 \
             WHERE source_url = ?3 AND tier = ?4",
        )
        .bind(crate::source_quality::TIER_OBSERVING)
        .bind(now)
        .bind(&r.source_url)
        .bind(crate::source_quality::TIER_QUARANTINED)
        .execute(pool)
        .await?;
    }

    // 体检拿到有效响应（samples>0）即视为源可用：同步复活账本自动隔离，
    // 并把导入隔离时强制写下的 enabled=0 恢复。人工/清理隔离由 revive_auto 过滤。
    let revive_urls: Vec<String> = rows
        .iter()
        .filter(|r| r.samples > 0)
        .map(|r| r.source_url.clone())
        .collect();
    match crate::source_ledger::revive_auto(pool, &revive_urls).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(revived = n, "bench revived auto-quarantined sources"),
        Err(e) => tracing::warn!(error = %e, "bench ledger revival failed"),
    }
    Ok(())
}

pub(super) fn category_medians(runs: &[BenchRun], corpus: &[CorpusEntry]) -> HashMap<String, f64> {
    let mut per_key: HashMap<String, (i64, i64)> = HashMap::new();
    for r in runs.iter() {
        let e = per_key.entry(r.corpus_key.clone()).or_insert((0, 0));
        e.1 += 1;
        if r.found {
            e.0 += 1;
        }
    }
    let mut cat_rates: HashMap<String, Vec<f64>> = HashMap::new();
    for e in corpus.iter().filter(|e| e.builtin) {
        if let Some((hits, obs)) = per_key.get(&e.id) {
            if *obs >= MIN_CURATED_SAMPLES {
                cat_rates
                    .entry(e.category.clone())
                    .or_default()
                    .push(*hits as f64 / *obs as f64);
            }
        }
    }
    cat_rates
        .into_iter()
        .map(|(k, v)| (k, median(&v)))
        .collect()
}
