//! books 路由的 search_api 部分（move-only 从 books.rs 拆出）。

use super::*;

pub(super) fn search_phase(pending: usize) -> &'static str {
    if pending == 0 {
        "final"
    } else {
        "first"
    }
}

/// 对外状态：收尾窗口（pending 归零、status 还没写）按真实结束态回显。
pub(super) fn effective_status(meta: &JobMeta, done: bool, cancelled: usize) -> String {
    if meta.status == "running" && done {
        if cancelled > 0 {
            "cancelled".to_string()
        } else {
            "done".to_string()
        }
    } else {
        meta.status.clone()
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

/// 创建任务后的内部句柄：`search_start` 需要回显源过滤结果，`/k` 只需要 job_id。
pub(super) struct StartedSearchJob {
    pub(super) job: Arc<SearchJob>,
    pub(super) plan: SearchSourcePlan,
}

/// 任务创建参数：把「搜什么、用哪些源、怎么退化」打包，避免入口函数参数过长。
pub(super) struct SearchJobRequest {
    pub(super) key: String,
    pub(super) scope: crate::req::SearchScope,
    pub(super) include_cooled: bool,
    pub(super) requested: Vec<String>,
    pub(super) curated_missing: CuratedMissingFallback,
}

/// 统一的任务创建入口：`/api/search/start` 与 `/k/search` 共用同一套源过滤、
/// 并发上限、落库与后台调度。`curated_missing` 保留两个入口的退化差异：
/// `/api` 退全量、`/k` 退 preferred（保住 KPW3 首屏耗时）。
pub(super) async fn create_search_job(
    s: &AppState,
    uid: i64,
    user_db: &SqlitePool,
    req: SearchJobRequest,
) -> AppResult<StartedSearchJob> {
    let SearchJobRequest {
        key,
        scope,
        include_cooled,
        requested,
        curated_missing,
    } = req;
    // 单用户并发上限：每个任务持有整份书源快照 + 结果缓冲，放任并发会打爆低配设备内存。
    // 前端任务面板可取消旧任务，因此这里直接拒绝而不是排队（避免“点了没反应”）。
    if active_jobs_for_user(uid) >= SEARCH_JOB_MAX_ACTIVE_PER_USER {
        return Err(AppError::BadRequest(format!(
            "同时运行的搜索任务已达 {} 个，请先在「搜索任务」里取消不需要的任务",
            SEARCH_JOB_MAX_ACTIVE_PER_USER
        )));
    }
    let mut sources = load_sources(&s.shared).await?;
    crate::routes::sources::sort_sources(&mut sources);
    // 精选防腐化：距上次体检超过 72h 时，后台重跑一次（不阻塞本次搜索）。
    crate::source_bench::maybe_schedule_stale_bench(s.shared.clone(), &s.cfg);
    let plan = resolve_search_sources(
        &s.shared,
        sources,
        &requested,
        &key,
        scope,
        include_cooled,
        curated_missing,
    )
    .await;
    let enabled = plan.active.clone();
    let source_total = enabled.len();
    let effective = plan.scope;
    let id = next_job_id();
    let job = Arc::new(SearchJob {
        id: id.clone(),
        uid,
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
        cancel: Arc::new(tokio::sync::Notify::new()),
        cancelled: AtomicBool::new(false),
        status: Mutex::new("running".to_string()),
        finished_unix_ms: AtomicU64::new(0),
    });
    // 先落库 running 行：即便进程随后崩溃，用户重新登录也能看到「已中断」的任务。
    if let Err(e) = persist_started(&s.shared, &job).await {
        tracing::warn!(target: "search_job", job_id = %job.id, error = %e, "persist search job failed");
    }
    {
        let mut g = job_registry().lock().unwrap();
        prune_jobs(&mut g);
        g.push(job.clone());
        tracing::info!(
            target: "search_job",
            job_id = %job.id,
            created_unix_ms = job.created_unix_ms,
            candidate_total = plan.source_total,
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
        job.clone(),
        enabled,
        key,
        s.shared.clone(),
        user_db.clone(),
        Arc::new(web_book::BookStore::new()),
        s.cfg.source_cooldown_secs,
        crate::source_quality::TierConfig::from_config(&s.cfg),
        plan.health.clone(),
        plan.prior.clone(),
        effective,
    ));
    Ok(StartedSearchJob { job, plan })
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
    let requested = parse_source_ids(q.source_ids.as_deref());
    let include_cooled = truthy(q.include_cooled.as_deref());
    let started = create_search_job(
        &s,
        u.id,
        &db.0,
        SearchJobRequest {
            key,
            scope: q.scope(),
            include_cooled,
            requested,
            curated_missing: CuratedMissingFallback::All,
        },
    )
    .await?;
    let job = &started.job;
    let plan = &started.plan;
    let source_total = job.source_total;
    let effective = job.scope;
    Ok(Json(json!({
        "job_id": &job.id,
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
        "source_ids_requested": &plan.requested,
        "ignored_source_ids": &plan.ignored,
        "warning": (!plan.ignored.is_empty())
            .then(|| format!("已忽略无效书源 id：{}", plan.ignored.join(", "))),
        // 冷却：默认跳过冷却中的源；include_cooled=1 时全部参与
        "include_cooled": include_cooled,
        "skipped_cooldown": plan.skipped_cooldown,
        "cooldown_total": plan.cooldown_total,
    })))
}

/// `/k` 的入口：只返回 job_id，由调用方 303 到进度/结果页。
pub(crate) async fn start_kindle_search_job(
    s: &AppState,
    uid: i64,
    user_db: &SqlitePool,
    key: String,
    scope: crate::req::SearchScope,
    include_cooled: bool,
) -> AppResult<String> {
    let started = create_search_job(
        s,
        uid,
        user_db,
        SearchJobRequest {
            key,
            scope,
            include_cooled,
            requested: Vec::new(),
            curated_missing: CuratedMissingFallback::Preferred,
        },
    )
    .await?;
    Ok(started.job.id.clone())
}

/// 任务不存在（或不属于当前用户）的统一文案：不区分两种情况，避免泄露任务是否存在。
pub(super) const JOB_NOT_FOUND: &str = "搜索任务不存在或已过期";

/// 结果响应组装：内存任务与数据库快照共用同一份字段与分页口径。
///
/// 关键点：数据库快照里的 `books` 仍带 `rank_bucket/rank_seq`，因此恢复后的任务
/// 可以直接复用同一套游标翻页，不重不漏。
pub(super) fn build_result_response(
    meta: &JobMeta,
    view: JobView,
    page: i32,
    page_size: usize,
    cursor: Option<(u8, u64)>,
) -> serde_json::Value {
    let JobView {
        books: raw_books,
        errors,
        done,
        finished,
        pending,
        scheduled,
        attempted,
        skipped,
        cancelled,
        ..
    } = view;
    // 取消/中断后仍会残留“未调度”的源；任务已结束就不再提示还有源要排。
    let pending_scheduled = if done {
        0
    } else {
        meta.source_total.saturating_sub(scheduled)
    };
    // 收尾窗口：pending 已归零但任务还没执行到写 status 的那一步（0 源任务尤其明显）。
    // 对外按“没有剩余工作”的真实状态回显，避免前端看到 done=true 却仍是 running。
    let status = effective_status(meta, done, cancelled);
    // 客户端响应边界：只保留书名包含关键词的条目。
    // 过滤发生在去重/排序之后，不改变打分、精选与源健康（健康统计用源原始命中数）。
    let (full, title_filter) = crate::title_filter::retain_title_matches(raw_books, &meta.key);

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
    json!({
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
        // 任务状态：running / done / cancelled / interrupted（重启后由数据库恢复）。
        "status": status,
        "interrupted": status == "interrupted",
        "cancelled_sources": cancelled,
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
        "scope": meta.scope.as_str(),
        "source_mode": meta.source_mode,
        "scope_filtered": meta.scope_filtered,
        "finished": finished,
        "pending": pending,
        "source_total": meta.source_total,
        "source_failed": source_failed,
        "source_skipped": source_skipped,
        "deadline_ms": crate::req::search_budget_for(meta.scope).as_millis() as u64,
        "total_budget_ms": crate::req::search_total_budget(meta.scope).as_millis() as u64,
        "coverage": meta.coverage,
        "curated_total": meta.curated_total,
        // 冷却统计随 job 固化，轮询期间不会变化
        "skipped_cooldown": meta.skipped_cooldown,
        "cooldown_total": meta.cooldown_total,
        "created_at": meta.created_unix_ms,
        "finished_at": (meta.finished_unix_ms > 0).then_some(meta.finished_unix_ms),
    })
}

pub(super) async fn search_result(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<JobResultQ>,
) -> AppResult<Json<serde_json::Value>> {
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, 200);
    // 游标 / 翻页请求 → 冻结快照；page=1 的普通轮询仍读实时结果（首屏持续更新）。
    let cursor = q.cursor.as_deref().and_then(parse_cursor);
    let paginated = q.cursor.is_some() || page > 1;

    if let Some(job) = find_job(&q.job_id) {
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
            return Err(AppError::NotFound(JOB_NOT_FOUND.into()));
        }
        let view = if paginated {
            frozen_job_view(&job)
        } else {
            job_view(&job)
        };
        let meta = job.meta();
        return Ok(Json(build_result_response(
            &meta, view, page, page_size, cursor,
        )));
    }

    // 内存未命中（超过内存保留期 / 进程重启）：回退数据库快照。
    let row = db_get(&s.shared, u.id, &q.job_id).await?;
    let Some(row) = row else {
        tracing::warn!(
            target: "search_job",
            job_id = %q.job_id,
            requester = u.id,
            reason = "not_in_memory_or_db",
            "search result 404"
        );
        return Err(AppError::NotFound(JOB_NOT_FOUND.into()));
    };
    let mut meta = row_meta(&row);
    let mut view = row_view(&row);
    if meta.status == "running" {
        // 内存里没有、数据库却仍是 running：只可能是进程重启前的遗留行
        // （启动清理失败）或内存硬上限淘汰。按中断处理，避免前端永远轮询。
        meta.status = "interrupted".into();
        view.done = true;
    }
    Ok(Json(build_result_response(
        &meta, view, page, page_size, cursor,
    )))
}

/// 已结束任务的完整快照（装箱，避免与轻量进度变体尺寸悬殊）。
pub(crate) struct FinishedJob {
    pub(crate) meta: JobMeta,
    pub(crate) view: JobView,
}

/// `/k` 的任务读取结果。
pub(crate) enum JobLookup {
    /// 仍在跑：只带轻量进度，不克隆/排序结果。
    Running(JobProgress),
    /// 已结束：完整元数据 + 结果快照。
    Finished(Box<FinishedJob>),
}

/// `/k` 的任务读取：内存优先，过期后回退数据库快照；跨用户与不存在都返回 `None`
/// （调用方统一给出「任务不存在或已过期」）。
///
/// 内存任务始终读实时结果（`job_view`），不使用可能被 SPA 提前冻结的部分快照：
/// `/k` 只在任务结束后渲染结果，实时视图已是最终结果，且不会受其他入口影响。
pub(crate) async fn load_job_lookup(
    pool: &SqlitePool,
    uid: i64,
    job_id: &str,
) -> AppResult<Option<JobLookup>> {
    if let Some(job) = find_job(job_id) {
        if job.uid != uid {
            tracing::warn!(
                target: "search_job",
                job_id = %job_id,
                owner = job.uid,
                requester = uid,
                reason = "uid_mismatch",
                "search job lookup 404"
            );
            return Ok(None);
        }
        let mut meta = job.meta();
        if meta.status == "running" {
            // pending 归零但 status 还没写：按结束处理，避免 /k 永远显示“搜索中”。
            if job_pending(&job) != Some(0) {
                return Ok(Some(JobLookup::Running(job_progress(&job))));
            }
            let view = job_view(&job);
            meta.status = effective_status(&meta, view.done, view.cancelled);
            return Ok(Some(JobLookup::Finished(Box::new(FinishedJob {
                meta,
                view,
            }))));
        }
        let view = job_view(&job);
        return Ok(Some(JobLookup::Finished(Box::new(FinishedJob {
            meta,
            view,
        }))));
    }

    let Some(row) = db_get(pool, uid, job_id).await? else {
        tracing::warn!(
            target: "search_job",
            job_id = %job_id,
            requester = uid,
            reason = "not_in_memory_or_db",
            "search job lookup 404"
        );
        return Ok(None);
    };
    let mut meta = row_meta(&row);
    let mut view = row_view(&row);
    if meta.status == "running" {
        // 内存里没有、数据库却仍是 running：进程重启遗留或内存淘汰，按中断处理。
        meta.status = "interrupted".into();
        view.done = true;
    }
    Ok(Some(JobLookup::Finished(Box::new(FinishedJob {
        meta,
        view,
    }))))
}

/// 结果快照 → 书名过滤后的完整列表（`/k` 本地分页用）。
pub(crate) fn filtered_job_books(meta: &JobMeta, view: &JobView) -> Vec<SearchBook> {
    crate::title_filter::retain_title_matches(view.books.clone(), &meta.key).0
}

/// 数据库行 → 结果元数据（字段口径与内存任务一致）。
pub(super) fn row_meta(row: &SearchJobRow) -> JobMeta {
    JobMeta {
        id: row.id.clone(),
        uid: row.uid,
        key: row.keyword.clone(),
        scope: crate::req::SearchScope::parse(Some(&row.scope)),
        source_mode: row.source_mode.clone(),
        coverage: row.coverage.clone(),
        curated_total: row.curated_total.max(0) as usize,
        source_total: row.source_total.max(0) as usize,
        scope_filtered: row.scope_filtered.max(0) as usize,
        skipped_cooldown: row.skipped_cooldown.max(0) as usize,
        cooldown_total: row.cooldown_total.max(0) as usize,
        status: row.status.clone(),
        created_unix_ms: row.created_at.max(0) as u64,
        finished_unix_ms: row.finished_at.unwrap_or(0).max(0) as u64,
    }
}

/// 数据库行 → 结果视图；快照缺失（运行中/被中断）时返回空结果。
pub(super) fn row_view(row: &SearchJobRow) -> JobView {
    let snap = row
        .snapshot_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<JobSnapshot>(s).ok())
        .unwrap_or_default();
    let finished = row.finished.max(0) as usize;
    let cancelled = row.cancelled.max(0) as usize;
    let source_total = row.source_total.max(0) as usize;
    JobView {
        books: snap.books,
        errors: snap.errors,
        done: row.status != "running",
        finished,
        pending: row.pending.max(0) as usize,
        // 数据库不保存调度细节；结果接口只在任务结束时回退到这里，
        // 因此把已处理源视为已排上，避免出现“还有源没排上”的误导。
        scheduled: (finished + cancelled).min(source_total),
        attempted: finished,
        skipped: row.skipped.max(0) as usize,
        cancelled,
    }
}

/// 结果快照的存储格式（`search_jobs.snapshot_json`）。
#[derive(Debug, Default, Deserialize)]
pub(super) struct JobSnapshot {
    #[serde(default)]
    pub(super) books: Vec<SearchBook>,
    #[serde(default)]
    pub(super) errors: Vec<web_book::SourceFailure>,
}

#[derive(Deserialize)]
pub(super) struct JobIdQ {
    pub(super) job_id: String,
}

/// 任务列表：内存（实时进度）+ 数据库（历史/重启后）合并，按创建时间倒序。
pub(super) async fn search_jobs(
    State(s): State<AppState>,
    u: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for job in user_jobs(u.id) {
        seen.insert(job.id.clone());
        items.push(memory_job_summary(&job));
    }
    let rows = db_list(&s.shared, u.id, SEARCH_JOB_DB_KEEP).await?;
    for row in &rows {
        if !seen.insert(row.id.clone()) {
            continue;
        }
        items.push(db_job_summary(row));
    }
    items.sort_by_key(|v| {
        std::cmp::Reverse(
            v.get("created_at")
                .and_then(|x| x.as_u64())
                .unwrap_or_default(),
        )
    });
    let active = items
        .iter()
        .filter(|v| v.get("status").and_then(|x| x.as_str()) == Some("running"))
        .count();
    Ok(Json(json!({
        "jobs": items,
        "active": active,
        "limit": SEARCH_JOB_DB_KEEP,
    })))
}

/// 内存任务摘要（进度实时，来自 job 状态）。
pub(super) fn memory_job_summary(job: &SearchJob) -> serde_json::Value {
    let (finished, pending, skipped, cancelled, failed) = {
        let st = job.state.lock().unwrap_or_else(|p| p.into_inner());
        (
            st.finished,
            st.pending,
            st.skipped,
            st.cancelled,
            st.errors
                .iter()
                .filter(|e| !is_budget_skip(&e.kind))
                .count(),
        )
    };
    let finished_at = job.finished_unix_ms.load(Ordering::Relaxed);
    // 与结果接口同一口径：pending 归零即视为结束（收尾写 status 前的窗口）。
    let raw_status = job.status_str();
    let status = if job.is_running() && pending == 0 {
        if cancelled > 0 {
            "cancelled"
        } else {
            "done"
        }
    } else {
        raw_status.as_str()
    };
    json!({
        "job_id": job.id,
        "keyword": job.key,
        "status": status,
        "interrupted": status == "interrupted",
        "scope": job.scope.as_str(),
        "coverage": job.coverage,
        "source_mode": job.source_mode,
        "source_total": job.source_total,
        "finished": finished,
        "pending": pending,
        "skipped": skipped,
        "cancelled": cancelled,
        "failed": failed,
        "curated_total": job.curated_total,
        "skipped_cooldown": job.skipped_cooldown,
        "cooldown_total": job.cooldown_total,
        "created_at": job.created_unix_ms,
        "finished_at": (finished_at > 0).then_some(finished_at),
        "origin": "memory",
    })
}

/// 数据库任务摘要（进程重启/内存过期后仍可见）。
///
/// 能走到这里的 running 行一定不在内存注册表里（内存任务会先去重），
/// 说明是重启遗留/被硬上限淘汰的僵尸任务，按 interrupted 展示，避免“永远运行中”。
pub(super) fn db_job_summary(row: &SearchJobRow) -> serde_json::Value {
    let status = if row.status == "running" {
        "interrupted"
    } else {
        row.status.as_str()
    };
    json!({
        "job_id": row.id,
        "keyword": row.keyword,
        "status": status,
        "interrupted": status == "interrupted",
        "scope": row.scope,
        "coverage": row.coverage,
        "source_mode": row.source_mode,
        "source_total": row.source_total.max(0),
        "finished": row.finished.max(0),
        "pending": if row.status == "running" { 0 } else { row.pending.max(0) },
        "skipped": row.skipped.max(0),
        "cancelled": row.cancelled.max(0),
        "failed": row.failed.max(0),
        "curated_total": row.curated_total.max(0),
        "skipped_cooldown": row.skipped_cooldown.max(0),
        "cooldown_total": row.cooldown_total.max(0),
        "created_at": row.created_at.max(0),
        "finished_at": row.finished_at,
        "origin": "db",
    })
}

/// 取消运行中的任务：置标志并唤醒调度循环；已结束的任务幂等返回当前状态。
pub(super) async fn search_job_cancel(
    State(_s): State<AppState>,
    u: AuthUser,
    Query(q): Query<JobIdQ>,
) -> AppResult<Json<serde_json::Value>> {
    let Some(job) = find_job(&q.job_id) else {
        return Err(AppError::NotFound(JOB_NOT_FOUND.into()));
    };
    if job.uid != u.id {
        return Err(AppError::NotFound(JOB_NOT_FOUND.into()));
    }
    if !job.is_running() {
        return Ok(Json(json!({
            "job_id": job.id,
            "status": job.status_str(),
            "already_finished": true,
        })));
    }
    job.cancelled.store(true, Ordering::SeqCst);
    // 用 notify_one 而不是 notify_waiters：调度循环每轮才创建 notified()，
    // notify_one 会留一个 permit，保证「刚设置标志、恰好没在等待」也能立刻生效。
    job.cancel.notify_one();
    tracing::info!(target: "search_job", job_id = %job.id, uid = u.id, "cancel requested");
    Ok(Json(json!({
        "job_id": job.id,
        "status": "cancelling",
        "already_finished": false,
    })))
}

/// 移除任务（内存 + 数据库）。运行中的任务必须先取消，避免“删了还在后台跑”。
pub(super) async fn search_job_delete(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<JobIdQ>,
) -> AppResult<Json<serde_json::Value>> {
    if let Some(job) = find_job(&q.job_id) {
        if job.uid != u.id {
            return Err(AppError::NotFound(JOB_NOT_FOUND.into()));
        }
        if job.is_running() {
            return Err(AppError::BadRequest("任务仍在运行，请先取消再移除".into()));
        }
    }
    let removed_mem = remove_job(&q.job_id, u.id);
    let removed_db = db_delete(&s.shared, u.id, &q.job_id).await?;
    if !removed_mem && !removed_db {
        return Err(AppError::NotFound(JOB_NOT_FOUND.into()));
    }
    tracing::info!(target: "search_job", job_id = %q.job_id, uid = u.id, "remove search job");
    Ok(Json(json!({
        "job_id": q.job_id,
        "removed": true,
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
        &q.keyword(),
        q.scope(),
        include_cooled,
        CuratedMissingFallback::All,
    )
    .await;
    let active = order_by_health(plan.active, &plan.health, &plan.prior);
    let (full, _, _) = aggregated(
        &s.shared,
        s.cfg.source_cooldown_secs,
        &crate::source_quality::TierConfig::from_config(&s.cfg),
        &active,
        &q.keyword(),
        include_cooled,
        plan.scope,
    )
    .await;
    Ok(Json(paginate(full, page, q.page_size)))
}

/// 取一次完整聚合结果（带 60s 缓存），供 `/api/search`、`/api/search/aggregate` 与
/// `/k/search` 共用，保证三个入口的分页都基于同一份结果，不重不漏。
///
/// 缓存里存的是**未做书名过滤**的原始聚合结果；过滤在返回调用方之前统一执行，
/// 因此缓存不会因为关键词归一化差异而串味，客户端拿到的也始终是同一套边界规则。
pub(crate) async fn aggregated(
    pool: &sqlx::SqlitePool,
    cooldown_secs: u64,
    tier_cfg: &crate::source_quality::TierConfig,
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
            let (outcome, evidence) =
                web_book::search_all_scoped_evidenced(sources, key, 1, scope).await;
            // 只有真实搜索（缓存未命中）才写证据：命中 60s 缓存不重复计一次「搜索」。
            // 这样 legacy `/api/search`、`/api/search/aggregate` 与 `/k/search` 与两段式
            // job 共用同一份健康/产出证据，不再「只读不写」。
            if let Err(e) =
                crate::source_health::record_outcomes(pool, &evidence, cooldown_secs, tier_cfg)
                    .await
            {
                tracing::warn!(error = %e, "record source health after aggregate search failed");
            }
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
        &q.keyword(),
        q.scope(),
        include_cooled,
        CuratedMissingFallback::All,
    )
    .await;
    let active = order_by_health(plan.active, &plan.health, &plan.prior);
    let (full, errors, title_filter) = aggregated(
        &s.shared,
        s.cfg.source_cooldown_secs,
        &crate::source_quality::TierConfig::from_config(&s.cfg),
        &active,
        &q.keyword(),
        include_cooled,
        plan.scope,
    )
    .await;

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
        "source_total": active.len(),
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
