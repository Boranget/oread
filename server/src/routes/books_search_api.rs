//! books 路由的 search_api 部分（move-only 从 books.rs 拆出）。

use super::*;

pub(super) fn search_phase(pending: usize) -> &'static str {
    if pending == 0 {
        "final"
    } else {
        "first"
    }
}

/// 是否属于“全局预算耗尽”导致的未完成（不写健康、不算站点失败）。
pub(super) fn is_budget_skip(kind: &str) -> bool {
    matches!(kind, "truncated" | "skipped")
}

/// 解析翻页游标：`bucket:seq`；`start` 表示“从头开始”（配合冻结快照）。
pub(super) fn parse_cursor(raw: &str) -> Option<(u8, u64)> {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("start") {
        return Some((u8::MAX, 0));
    }
    let (b, s) = t.split_once(':')?;
    Some((b.parse().ok()?, s.parse().ok()?))
}

/// 该条目是否严格位于游标之后（与 `search_rank::Cursor::item_after` 同义）。
pub(super) fn book_after_cursor(b: &SearchBook, (cb, cs): (u8, u64)) -> bool {
    match (b.rank_bucket, b.rank_seq) {
        (Some(bb), Some(ss)) => bb < cb || (bb == cb && ss > cs),
        // 缺稳定序号的条目无法定位，宁可多显示也不丢。
        _ => true,
    }
}

pub(super) fn cursor_of(b: &SearchBook) -> Option<String> {
    b.rank_bucket
        .zip(b.rank_seq)
        .map(|(bucket, seq)| format!("{bucket}:{seq}"))
}

pub(super) async fn search_start(
    State(s): State<AppState>,
    u: AuthUser,
    db: UserDb,
    Query(q): Query<SearchQ>,
) -> AppResult<Json<serde_json::Value>> {
    let key = q.keyword();
    if key.is_empty() {
        return Err(AppError::BadRequest("key（或别名 q）不能为空".into()));
    }
    let mut sources = load_sources(&s.shared).await?;
    crate::routes::sources::sort_sources(&mut sources);
    // 精选防腐化：距上次体检超过 72h 时，后台重跑一次（不阻塞本次搜索）。
    crate::source_bench::maybe_schedule_stale_bench(s.shared.clone(), &s.cfg);
    let requested = parse_source_ids(q.source_ids.as_deref());
    let include_cooled = truthy(q.include_cooled.as_deref());
    let plan = resolve_search_sources(
        &s.shared,
        sources,
        &requested,
        q.scope(),
        include_cooled,
        CuratedMissingFallback::All,
    )
    .await;
    let enabled = plan.active.clone();
    let source_total = enabled.len();
    let effective = plan.scope;
    let id = next_job_id();
    let job = Arc::new(SearchJob {
        id: id.clone(),
        uid: u.id,
        key: key.clone(),
        at: Instant::now(),
        created_unix_ms: unix_ms(),
        sources: enabled.clone(),
        source_total,
        scope: effective,
        skipped_cooldown: plan.skipped_cooldown,
        cooldown_total: plan.cooldown_total,
        coverage: plan.coverage.to_string(),
        source_mode: plan.source_mode.to_string(),
        scope_filtered: plan.scope_filtered,
        curated_total: plan.curated_total,
        state: Mutex::new(SearchJobState {
            pending: source_total,
            ..Default::default()
        }),
    });
    {
        let mut g = job_registry().lock().unwrap();
        prune_jobs(&mut g);
        g.push(job.clone());
        tracing::info!(
            target: "search_job",
            job_id = %job.id,
            created_unix_ms = job.created_unix_ms,
            source_total = job.source_total,
            scope = job.scope.as_str(),
            source_mode = job.source_mode.as_str(),
            coverage = job.coverage.as_str(),
            scope_filtered = job.scope_filtered,
            curated_total = job.curated_total,
            known_jobs = g.len(),
            "create search job"
        );
    }
    tokio::spawn(run_search_job(
        job,
        enabled,
        key,
        s.shared.clone(),
        db.0.clone(),
        Arc::new(web_book::BookStore::new()),
        s.cfg.source_cooldown_secs,
        crate::source_quality::TierConfig::from_config(&s.cfg),
        plan.health,
        effective,
    ));
    Ok(Json(json!({
        "job_id": id,
        "scope": effective.as_str(),
        "coverage": plan.coverage,
        "source_mode": plan.source_mode,
        "scope_filtered": plan.scope_filtered,
        "curated_total": plan.curated_total,
        "source_total": source_total,
        // deadline_ms 保持旧语义：本次 scope 的单源预算；整批预算单独给出。
        "deadline_ms": crate::req::search_budget_for(effective).as_millis() as u64,
        "total_budget_ms": crate::req::search_total_budget(effective).as_millis() as u64,
        // 首轮快照：还没调度任何源。
        "phase": search_phase(source_total),
        "partial": source_total > 0,
        "scheduled_sources": 0,
        "attempted_sources": 0,
        "pending_sources": source_total,
        // 契约：显式回显过滤结果，区分「用户指定」与「被 scope 过滤」
        "source_ids_requested": plan.requested,
        "ignored_source_ids": plan.ignored,
        "warning": (!plan.ignored.is_empty())
            .then(|| format!("已忽略无效书源 id：{}", plan.ignored.join(", "))),
        // 冷却：默认跳过冷却中的源；include_cooled=1 时全部参与
        "include_cooled": include_cooled,
        "skipped_cooldown": plan.skipped_cooldown,
        "cooldown_total": plan.cooldown_total,
    })))
}

pub(super) async fn search_result(
    State(_s): State<AppState>,
    u: AuthUser,
    Query(q): Query<JobResultQ>,
) -> AppResult<Json<serde_json::Value>> {
    let job = {
        let mut g = job_registry().lock().unwrap();
        prune_jobs(&mut g);
        let found = g.iter().find(|j| j.id == q.job_id).cloned();
        if found.is_none() {
            let known: Vec<&str> = g.iter().map(|j| j.id.as_str()).collect();
            tracing::warn!(
                target: "search_job",
                job_id = %q.job_id,
                known_jobs = g.len(),
                known_ids = ?known,
                reason = "not_registered_or_pruned",
                "search result 404"
            );
        }
        found
    };
    let job = job.ok_or_else(|| AppError::NotFound("搜索任务不存在或已过期（60s TTL）".into()))?;
    // 归属校验：别人的 job_id 与不存在保持同样的 404，避免泄露任务是否存在。
    if job.uid != u.id {
        tracing::warn!(
            target: "search_job",
            job_id = %q.job_id,
            owner = job.uid,
            requester = u.id,
            reason = "uid_mismatch",
            "search result 404"
        );
        return Err(AppError::NotFound(
            "搜索任务不存在或已过期（60s TTL）".into(),
        ));
    }
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, 200);
    // 游标 / 翻页请求 → 冻结快照；page=1 的普通轮询仍读实时结果（首屏持续更新）。
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    let paginated = q.cursor.is_some() || page > 1;
    let view = if paginated {
        frozen_job_view(&job)
    } else {
        job_view(&job)
    };
    let done = view.done;
    let finished = view.finished;
    let pending = view.pending;
    let scheduled = view.scheduled;
    let attempted = view.attempted;
    let skipped = view.skipped;
    // 未排上的源（两批同时调度后通常为 0）
    let pending_scheduled = job.source_total.saturating_sub(scheduled);
    // 客户端响应边界：只保留书名包含关键词的条目。
    // 过滤发生在去重/排序之后，不改变打分、精选与源健康（健康统计用源原始命中数）。
    let (full, title_filter) = crate::title_filter::retain_title_matches(view.books, &job.key);
    let errors = view.errors;

    let total = full.len();
    let strong_total = full
        .iter()
        .filter(|b| b.relevance.as_deref() == Some("strong"))
        .count();
    let author_total = full
        .iter()
        .filter(|b| b.relevance.as_deref() == Some("author"))
        .count();
    let partial_total = full
        .iter()
        .filter(|b| b.relevance.as_deref() == Some("partial"))
        .count();
    let partial_folded = strong_total >= crate::search_rank::STRONG_MIN_RESULTS;
    let (books, has_more, next_cursor) = match cursor {
        Some(after) => {
            let eligible: Vec<SearchBook> = full
                .iter()
                .filter(|b| book_after_cursor(b, after))
                .cloned()
                .collect();
            let has_more = eligible.len() > page_size;
            let page_books: Vec<SearchBook> = eligible.into_iter().take(page_size).collect();
            let next = has_more
                .then(|| page_books.last().and_then(cursor_of))
                .flatten();
            (page_books, has_more, next)
        }
        None => {
            let skip = ((page as usize) - 1) * page_size;
            let page_books: Vec<SearchBook> =
                full.iter().skip(skip).take(page_size).cloned().collect();
            let has_more = skip + page_books.len() < total;
            let next = has_more
                .then(|| page_books.last().and_then(cursor_of))
                .flatten();
            (page_books, has_more, next)
        }
    };
    // 真实失败与“全局预算未完成”分开计数：后者不写健康、不算站点故障。
    let source_failed = errors.iter().filter(|e| !is_budget_skip(&e.kind)).count();
    let source_skipped = skipped;
    Ok(Json(json!({
        "books": books,
        "errors": errors,
        "page": page,
        "page_size": page_size,
        "total": total,
        // 书名包含过滤：raw=过滤前候选数，kept=过滤后候选总数（即 total），
        // dropped=被过滤掉的候选数。全部被过滤时 books 为空数组，不报错。
        "title_filter": title_filter,
        "has_more": has_more,
        // 下一页游标：`bucket:seq`；传回 `cursor=` 即从该位置继续，翻页不重不漏。
        "next_cursor": next_cursor,
        "done": done,
        // 相关性分层：strong=书名命中，author=只命中作者，partial=只命中片段（可能相关）。
        "strong_total": strong_total,
        "author_total": author_total,
        "partial_total": partial_total,
        // strong ≥ 5 时 partial 被折叠出首屏（不再返回）。
        "partial_folded": partial_folded,
        // 两段式进度：scheduled=已进入调度，attempted=真正发过请求；
        // pending_sources=尚未排上的源（source_total - scheduled），done 仍以“全部处理完”为准。
        "phase": search_phase(pending),
        "partial": !done,
        "scheduled_sources": scheduled,
        "attempted_sources": attempted,
        "pending_sources": pending_scheduled,
        "scope": job.scope.as_str(),
        "source_mode": job.source_mode.as_str(),
        "scope_filtered": job.scope_filtered,
        "finished": finished,
        "pending": pending,
        "source_total": job.source_total,
        "source_failed": source_failed,
        "source_skipped": source_skipped,
        "deadline_ms": crate::req::search_budget_for(job.scope).as_millis() as u64,
        "total_budget_ms": crate::req::search_total_budget(job.scope).as_millis() as u64,
        "coverage": job.coverage.as_str(),
        "curated_total": job.curated_total,
        // 冷却统计随 job 固化，轮询期间不会变化
        "skipped_cooldown": job.skipped_cooldown,
        "cooldown_total": job.cooldown_total,
    })))
}

/// legacy `/api/search` 的源过滤与两段式共用 `resolve_search_sources`（不再有两套口径）。
/// 现在同样尊重 `scope` 与 `source_ids`。
pub(super) async fn search(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<SearchQ>,
) -> AppResult<Json<Vec<SearchBook>>> {
    let mut sources = load_sources(&s.shared).await?;
    crate::routes::sources::sort_sources(&mut sources);
    let page = q.page.unwrap_or(1).max(1);
    let include_cooled = truthy(q.include_cooled.as_deref());
    let requested = parse_source_ids(q.source_ids.as_deref());
    let plan = resolve_search_sources(
        &s.shared,
        sources,
        &requested,
        q.scope(),
        include_cooled,
        CuratedMissingFallback::All,
    )
    .await;
    let (full, _, _) = aggregated(&plan.active, &q.keyword(), include_cooled, plan.scope).await;
    Ok(Json(paginate(full, page, q.page_size)))
}

/// 取一次完整聚合结果（带 60s 缓存），供 `/api/search`、`/api/search/aggregate` 与
/// `/k/search` 共用，保证三个入口的分页都基于同一份结果，不重不漏。
///
/// 缓存里存的是**未做书名过滤**的原始聚合结果；过滤在返回调用方之前统一执行，
/// 因此缓存不会因为关键词归一化差异而串味，客户端拿到的也始终是同一套边界规则。
pub(crate) async fn aggregated(
    sources: &[crate::entities::BookSource],
    key: &str,
    include_cooled: bool,
    scope: crate::req::SearchScope,
) -> (
    Vec<SearchBook>,
    Vec<web_book::SourceFailure>,
    crate::title_filter::TitleFilterStats,
) {
    // 缓存键必须包含实际参与源集合（显式子集 / scope / 冷却都会改变它）与预算档位，
    // 否则 60s 内不同请求会串用结果。
    let mut urls: Vec<&str> = sources.iter().map(|s| s.book_source_url.as_str()).collect();
    urls.sort_unstable();
    let cache_key = format!(
        "{key}\u{1e}{}\u{1e}{}\u{1e}{}",
        scope.as_str(),
        if include_cooled { "cooled" } else { "normal" },
        urls.join("\u{1f}")
    );
    let hit = {
        let mut g = agg_cache().lock().unwrap();
        prune_cache(&mut g);
        g.iter()
            .find(|e| e.key == cache_key)
            .map(|e| (e.books.clone(), e.errors.clone()))
    };
    let (raw_books, errors) = match hit {
        Some(v) => v,
        None => {
            let outcome = web_book::search_all_with_errors_scoped(sources, key, 1, scope).await;
            let mut books = outcome.books;
            books.truncate(AGG_MAX_BOOKS);
            let errors = outcome.errors;
            if !books.is_empty() {
                let mut g = agg_cache().lock().unwrap();
                prune_cache(&mut g);
                g.push(AggEntry {
                    key: cache_key,
                    at: Instant::now(),
                    books: books.clone(),
                    errors: errors.clone(),
                });
                prune_cache(&mut g);
            }
            (books, errors)
        }
    };
    let (books, title_filter) = crate::title_filter::retain_title_matches(raw_books, key);
    tracing::debug!(
        target: "search_filter",
        raw = title_filter.raw,
        kept = title_filter.kept,
        dropped = title_filter.dropped,
        "title filter"
    );
    (books, errors, title_filter)
}

/// 聚合搜索明细：书籍 + 成功/失败书源统计（单源失败不影响整体返回）。
/// 分页基于「一次完整聚合结果」的缓存，保证跨页不重复、不遗漏，且 `total` 可信。
pub(super) async fn search_aggregate(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<SearchQ>,
) -> AppResult<Json<serde_json::Value>> {
    let mut sources = load_sources(&s.shared).await?;
    crate::routes::sources::sort_sources(&mut sources);
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, 200);
    let include_cooled = truthy(q.include_cooled.as_deref());
    let requested = parse_source_ids(q.source_ids.as_deref());
    let plan = resolve_search_sources(
        &s.shared,
        sources,
        &requested,
        q.scope(),
        include_cooled,
        CuratedMissingFallback::All,
    )
    .await;
    let (full, errors, title_filter) =
        aggregated(&plan.active, &q.keyword(), include_cooled, plan.scope).await;

    let total = full.len();
    let skip = ((page as usize) - 1) * page_size;
    let books: Vec<SearchBook> = full.into_iter().skip(skip).take(page_size).collect();
    let source_failed = errors.iter().filter(|e| !is_budget_skip(&e.kind)).count();
    let source_skipped = errors.len() - source_failed;
    Ok(Json(json!({
        "books": books,
        "errors": errors,
        "page": page,
        "page_size": page_size,
        "total": total,
        // 书名包含过滤计数：raw=过滤前候选，kept=过滤后（=total），dropped=被丢弃。
        "title_filter": title_filter,
        "has_more": skip + page_size < total,
        "source_failed": source_failed,
        "source_skipped": source_skipped,
        "scope": plan.scope.as_str(),
        "source_mode": plan.source_mode,
        "scope_filtered": plan.scope_filtered,
        "curated_total": plan.curated_total,
        "source_ids_requested": plan.requested,
        "ignored_source_ids": plan.ignored,
        "source_total": plan.active.len(),
        "include_cooled": include_cooled,
        "skipped_cooldown": plan.skipped_cooldown,
        "cooldown_total": plan.cooldown_total,
        "cache_ttl_secs": AGG_TTL_SECS,
    })))
}

pub(super) fn paginate(
    all: Vec<SearchBook>,
    page: i32,
    page_size: Option<usize>,
) -> Vec<SearchBook> {
    let size = page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, 200);
    let skip = (page as usize - 1) * size;
    all.into_iter().skip(skip).take(size).collect()
}
