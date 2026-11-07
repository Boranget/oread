//! books 路由的 search_job 部分（move-only 从 books.rs 拆出）。

use super::*;

pub(super) struct AggEntry {
    pub(super) key: String,
    pub(super) at: Instant,
    pub(super) books: Vec<SearchBook>,
    pub(super) errors: Vec<web_book::SourceFailure>,
}

pub(super) fn agg_cache() -> &'static Mutex<Vec<AggEntry>> {
    static CACHE: OnceLock<Mutex<Vec<AggEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

pub(super) fn prune_cache(g: &mut Vec<AggEntry>) {
    g.retain(|e| e.at.elapsed().as_secs() < AGG_TTL_SECS);
    while g.len() > AGG_MAX_QUERIES {
        // 淘汰最旧的查询
        if let Some((idx, _)) = g
            .iter()
            .enumerate()
            .min_by_key(|(_, e)| e.at.elapsed().as_millis())
        {
            g.remove(idx);
        } else {
            break;
        }
    }
}

// ---------- 两段式聚合搜索：先返回已完成源，轮询补齐 ----------
/// job 存活 60s。
/// 已完成任务保留上限 32；进行中任务另有 64 的硬上限，避免把仍在轮询的 job 误杀成 404。
pub(super) const SEARCH_JOB_TTL_SECS: u64 = 60;
pub(super) const SEARCH_JOB_MAX: usize = 32;
pub(super) const SEARCH_JOB_MAX_ACTIVE: usize = 64;
pub(super) const SEARCH_PER_SOURCE_MAX: usize = 200;

pub(super) struct SearchJob {
    pub(super) id: String,
    /// 创建者用户 id：结果接口只允许本人读取，跨用户查询一律 404。
    pub(super) uid: i64,
    #[allow(dead_code)]
    pub(super) key: String,
    pub(super) at: Instant,
    /// 创建时刻（Unix 毫秒），仅用于日志定位“为什么 1 秒内 404”
    pub(super) created_unix_ms: u64,
    /// 启用中的书源快照：用于去重/按权重排序，保证与 aggregate 的排序规则一致
    pub(super) sources: Vec<crate::entities::BookSource>,
    pub(super) source_total: usize,
    /// 本次搜索范围（curated / all），用于结果接口回显与预算/并发选择。
    pub(super) scope: crate::req::SearchScope,
    /// 因冷却被跳过的源数（本次请求）
    pub(super) skipped_cooldown: usize,
    /// 候选集合中当前处于冷却的源总数（前端提示“另有 N 个源处于冷却”）
    pub(super) cooldown_total: usize,
    /// 实际覆盖情况：`curated`（只用了精选源）/ `expanded`（退回或显式覆盖到全部可搜索源）
    pub(super) coverage: String,
    /// 源集合来源：`user` / `curated` / `curated_fallback` / `all`（结果接口回显）
    pub(super) source_mode: String,
    /// 精选收缩排除掉的可搜索源数量（显式源为 0）
    pub(super) scope_filtered: usize,
    /// 当前精选集合大小（curated_total）
    pub(super) curated_total: usize,
    pub(super) state: Mutex<SearchJobState>,
}

pub(super) fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
pub(super) struct SearchJobState {
    pub(super) books: Vec<SearchBook>,
    pub(super) errors: Vec<web_book::SourceFailure>,
    /// 已处理完的源数（含成功/失败/排队未及执行）
    pub(super) finished: usize,
    /// 尚未处理完的源数（legacy 语义；与 `scheduled` 不同）
    pub(super) pending: usize,
    /// 已进入调度的源数（两段式下逐批累加）
    pub(super) scheduled: usize,
    /// 真正拿到并发槽并发出过至少一次请求的源数
    pub(super) attempted: usize,
    /// job 内的稳定排序/快照：首次翻页后冻结，保证同一会话不重不漏。
    pub(super) rank: web_book::RankFreeze,
}

pub(super) fn job_registry() -> &'static Mutex<Vec<Arc<SearchJob>>> {
    static JOBS: OnceLock<Mutex<Vec<Arc<SearchJob>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(Vec::new()))
}

pub(super) fn job_pending(j: &Arc<SearchJob>) -> Option<usize> {
    j.state.try_lock().ok().map(|st| st.pending)
}

pub(super) fn prune_jobs(list: &mut Vec<Arc<SearchJob>>) {
    list.retain(|j| j.at.elapsed().as_secs() < SEARCH_JOB_TTL_SECS);
    // 第一优先：淘汰“已完成且最旧”的任务。进行中的任务不参与淘汰，
    // 否则并发 start/轮询下，刚创建的 job 会被后来的 start 顶掉并 404。
    while list.len() > SEARCH_JOB_MAX {
        let victim = list
            .iter()
            .enumerate()
            .filter(|(_, j)| job_pending(j) == Some(0))
            .max_by_key(|(_, j)| j.at.elapsed().as_millis())
            .map(|(i, _)| i);
        let Some(i) = victim else { break };
        let j = &list[i];
        tracing::warn!(
            target: "search_job",
            job_id = %j.id,
            created_unix_ms = j.created_unix_ms,
            elapsed_ms = j.at.elapsed().as_millis() as u64,
            pending = 0u64,
            reason = "completed_overflow",
            "evict search job"
        );
        list.remove(i);
    }
    // 仅当进行中的任务超过硬上限时才淘汰活跃任务（兜底防内存膨胀）。
    while list.len() > SEARCH_JOB_MAX_ACTIVE {
        let Some((i, j)) = list
            .iter()
            .enumerate()
            .max_by_key(|(_, j)| j.at.elapsed().as_millis())
        else {
            break;
        };
        let pending = job_pending(j).unwrap_or(0);
        tracing::warn!(
            target: "search_job",
            job_id = %j.id,
            created_unix_ms = j.created_unix_ms,
            elapsed_ms = j.at.elapsed().as_millis() as u64,
            pending = pending as u64,
            reason = "active_hard_cap",
            "evict search job"
        );
        list.remove(i);
    }
}

pub(super) fn next_job_id() -> String {
    static SEQ: AtomicI64 = AtomicI64::new(0);
    format!(
        "j{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

pub(super) struct JobView {
    pub(super) books: Vec<SearchBook>,
    pub(super) errors: Vec<web_book::SourceFailure>,
    pub(super) done: bool,
    pub(super) finished: usize,
    pub(super) pending: usize,
    pub(super) scheduled: usize,
    pub(super) attempted: usize,
}

/// 读取视图：合并已完成源的结果 → 去重排序 → 截断（与 aggregate 同一套规则）。
///
/// 排序使用 job 内的稳定 `(bucket, seq)`：同一条结果首次入池后位置固定，
/// 后到的源只会追加到桶内尾部，不会挤动已经翻过的页。
pub(super) fn job_view(job: &SearchJob) -> JobView {
    let mut st = job.state.lock().unwrap();
    let books = st.books.clone();
    let mut full =
        web_book::dedupe_and_sort_with(books, &job.sources, &job.key, Some(&mut st.rank));
    let errors = st.errors.clone();
    let finished = st.finished;
    let pending = st.pending;
    let scheduled = st.scheduled;
    let attempted = st.attempted;
    drop(st);
    full.truncate(AGG_MAX_BOOKS);
    JobView {
        books: full,
        errors,
        done: pending == 0,
        finished,
        pending,
        scheduled,
        attempted,
    }
}

/// 冻结快照读取：首次翻页（cursor / page>1）时把当前有序结果固化，
/// 后续页全部从同一份快照取，保证同一搜索会话翻页不重不漏。
pub(super) fn frozen_job_view(job: &SearchJob) -> JobView {
    {
        let st = job.state.lock().unwrap();
        if let Some(snap) = st.rank.snapshot.clone() {
            let view = JobView {
                books: snap,
                errors: st.errors.clone(),
                done: st.pending == 0,
                finished: st.finished,
                pending: st.pending,
                scheduled: st.scheduled,
                attempted: st.attempted,
            };
            return view;
        }
    }
    let view = job_view(job);
    let mut st = job.state.lock().unwrap();
    if st.rank.snapshot.is_none() {
        st.rank.snapshot = Some(view.books.clone());
    }
    view
}

/// 同一批次内的入队顺序键（越小越先占并发槽）：
/// - `0`：有延迟样本且最近一次不是站点失败 → 按 p90/均值延迟升序；
/// - `1`：无观测数据（新导入/从未搜到过）→ 排在已知快源之后；
/// - `2`：最近一次是站点失败 → 垫底，让预算优先给可用源。
///
/// 只调整顺序，不改变参与集合、预算与并发上限，因此不会减少「本来能在预算内完成」的源。
pub(super) fn health_order_key(h: Option<&source_health::HealthRow>) -> (u8, i64) {
    let Some(h) = h else {
        return (1, 0);
    };
    let latency = h.ttfb_ms_p90.or(h.avg_latency_ms);
    match (h.last_status == source_health::STATUS_FAIL, latency) {
        (true, l) => (2, l.unwrap_or(i64::MAX)),
        (false, Some(ms)) => (0, ms),
        (false, None) => (1, 0),
    }
}

#[allow(clippy::too_many_arguments)] // 搜索任务上下文（job/源/关键词/池/预算/取消），拆分仅把参数列表搬到另一层。
pub(super) async fn run_search_job(
    job: Arc<SearchJob>,
    sources: Vec<crate::entities::BookSource>,
    key: String,
    pool: SqlitePool,
    book_pool: SqlitePool,
    store: Arc<web_book::BookStore>,
    cooldown_secs: u64,
    tier_cfg: crate::source_quality::TierConfig,
    health: HashMap<String, source_health::HealthRow>,
    scope: crate::req::SearchScope,
) {
    // 单源与整批预算独立：curated 5s/6s，all 6s/10s（见 req::search_budget_for）。
    let budget = crate::req::search_budget_for(scope);
    let total_budget = crate::req::search_total_budget(scope);
    // 排队等槽不计入单源预算，但整次搜索仍受此上限约束。
    let deadline = tokio::time::Instant::now() + total_budget;

    // 两批同时进调度：preferred 先入队（优先占源级并发槽），但不再等它整批跑完；
    // observing 立刻跟上，共用同一总预算与同一套源级并发信号量。
    // 各自完成即写回结果，首批慢站不再拖住首屏。
    let (mut preferred, mut observing): (Vec<_>, Vec<_>) = sources
        .into_iter()
        .partition(|s| source_health::is_preferred(health.get(&s.book_source_url)));

    // 慢源后置：并发槽按入队顺序分配，先入队的是「已知快且近期正常」的源；
    // 慢源/近期站点失败源排到队尾，临近总预算时被自然截断（只记 skipped，不当站点故障冷却）。
    preferred.sort_by_key(|s| health_order_key(health.get(&s.book_source_url)));
    observing.sort_by_key(|s| health_order_key(health.get(&s.book_source_url)));

    let (mut pref_outcomes, obs_outcomes) = tokio::join!(
        run_search_batch(
            &job, preferred, &key, deadline, budget, &health, scope, &book_pool, &store,
        ),
        run_search_batch(
            &job, observing, &key, deadline, budget, &health, scope, &book_pool, &store,
        )
    );
    pref_outcomes.extend(obs_outcomes);

    if let Err(e) =
        source_health::record_outcomes(&pool, &pref_outcomes, cooldown_secs, &tier_cfg).await
    {
        tracing::warn!(error = %e, "record source health after search failed");
    }
}

#[allow(clippy::too_many_arguments)] // 与 `run_search_job` 同一批调度参数，保持签名对称便于对照。
pub(super) async fn run_search_batch(
    job: &Arc<SearchJob>,
    sources: Vec<crate::entities::BookSource>,
    key: &str,
    phase_deadline: tokio::time::Instant,
    budget: Duration,
    health: &HashMap<String, source_health::HealthRow>,
    scope: crate::req::SearchScope,
    book_pool: &SqlitePool,
    store: &Arc<web_book::BookStore>,
) -> Vec<source_health::Outcome> {
    enum TaskOutcome {
        Done(anyhow::Result<Vec<crate::entities::SearchBook>>),
        /// 拿到槽后真实请求超时（或全局剩余时间不足被截断）
        Timeout(String),
        /// 排队到全局 deadline 仍未拿到并发槽
        Skipped(String),
    }

    let mut pending = futures::stream::FuturesUnordered::new();
    let mut outcomes: Vec<source_health::Outcome> = Vec::new();
    {
        let mut st = job.state.lock().unwrap();
        st.scheduled += sources.len();
    }
    for src in sources {
        let s = src.clone();
        let k = key.to_string();
        let st = store.clone();
        let bp = book_pool.clone();
        pending.push(async move {
            let comic = crate::req::is_comic_source_type(s.book_source_type);
            let queued_at = Instant::now();
            let slot = match crate::req::acquire_search_slot(comic, phase_deadline, scope).await {
                Ok(p) => p,
                Err(e) => {
                    let waited = queued_at.elapsed().as_millis() as i64;
                    return (s, TaskOutcome::Skipped(format!("{e}")), waited, false);
                }
            };
            let started = Instant::now();
            let remain = phase_deadline.saturating_duration_since(tokio::time::Instant::now());
            if remain.is_zero() {
                drop(slot);
                return (
                    s,
                    TaskOutcome::Skipped("全局搜索预算耗尽（排队未及执行）".to_string()),
                    started.elapsed().as_millis() as i64,
                    false,
                );
            }
            let r = tokio::time::timeout(
                remain.min(budget),
                web_book::search_book(&s, &k, 1, Some(st.as_ref())),
            )
            .await;
            // 搜索阶段的 `@put`（爱看写 bid）按 book_key 落库，供后续 info/toc 读回。
            let changes = st.take_changes();
            if !changes.is_empty() {
                persist_book_changes(&bp, changes).await;
            }
            let latency_ms = started.elapsed().as_millis() as i64;
            drop(slot);
            let outcome = match r {
                Ok(x) => TaskOutcome::Done(x),
                Err(_) => {
                    let err = if remain < budget {
                        format!(
                            "全局搜索预算耗尽（排队后仅剩 {}ms，未完成请求）",
                            remain.as_millis()
                        )
                    } else {
                        format!("搜索超时（单源 {}s 预算）", budget.as_secs())
                    };
                    TaskOutcome::Timeout(err)
                }
            };
            (s, outcome, latency_ms, true)
        });
    }
    // 健康状态只在任务收尾时批量写一次，不随每本书/每条结果写库。
    while let Some((src, outcome, latency_ms, attempted)) = pending.next().await {
        // 逐源耗时埋点（默认 debug 不输出；扫参时开 `RUST_LOG=search_source=debug`）。
        // 排队时间不计入 latency_ms，`timed_out` 只标真实请求超时，与 Skipped 区分。
        let (kind, hits, timed_out) = match &outcome {
            TaskOutcome::Done(Ok(b)) => ("ok", b.len() as i64, false),
            TaskOutcome::Done(Err(_)) => ("error", 0, false),
            TaskOutcome::Timeout(_) => ("timeout", 0, true),
            TaskOutcome::Skipped(_) => ("skipped", 0, false),
        };
        tracing::debug!(
            target: "search_source",
            source = %src.book_source_url,
            scope = scope.as_str(),
            kind,
            timed_out,
            latency_ms,
            hits,
            "search source done"
        );
        let rule_complete = crate::source_quality::rule_completeness(&src);
        let mut st = job.state.lock().unwrap();
        if attempted {
            st.attempted += 1;
        }
        match outcome {
            TaskOutcome::Done(Ok(mut books)) => {
                let hits = books.len() as i64;
                books.truncate(SEARCH_PER_SOURCE_MAX);
                // 回显来源档位，便于排查“这条结果来自什么档的源”。
                let src_tier = health
                    .get(&src.book_source_url)
                    .map(|h| h.tier.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| crate::source_quality::TIER_OBSERVING.to_string());
                let src_score = health
                    .get(&src.book_source_url)
                    .map(|h| h.score)
                    .unwrap_or(50);
                for b in books.iter_mut() {
                    b.tier = Some(src_tier.clone());
                    b.score = Some(src_score);
                }
                st.books.append(&mut books);
                outcomes.push(source_health::Outcome {
                    source_url: src.book_source_url.clone(),
                    status: source_health::STATUS_OK,
                    latency_ms,
                    error: None,
                    hits,
                    parsed: true,
                    usage: true,
                    rule_complete,
                });
            }
            TaskOutcome::Done(Err(e)) => {
                // 站点故障（连接/超时/HTTP 非成功）才计入失败；引擎故障记 partial，不冷却。
                let status = match source_health::classify(&e) {
                    source_health::FailureKind::Site => source_health::STATUS_FAIL,
                    source_health::FailureKind::Engine => source_health::STATUS_PARTIAL,
                };
                st.errors.push(web_book::SourceFailure {
                    source: src.book_source_url.clone(),
                    source_name: src.book_source_name.clone(),
                    error: format!("{e}"),
                });
                outcomes.push(source_health::Outcome {
                    source_url: src.book_source_url.clone(),
                    status,
                    latency_ms,
                    error: Some(format!("{e}")),
                    hits: 0,
                    parsed: false,
                    usage: true,
                    rule_complete,
                });
            }
            TaskOutcome::Timeout(err) => {
                st.errors.push(web_book::SourceFailure {
                    source: src.book_source_url.clone(),
                    source_name: src.book_source_name.clone(),
                    error: err.clone(),
                });
                outcomes.push(source_health::Outcome {
                    source_url: src.book_source_url.clone(),
                    status: source_health::STATUS_FAIL,
                    latency_ms,
                    error: Some(err),
                    hits: 0,
                    parsed: false,
                    usage: true,
                    rule_complete,
                });
            }
            TaskOutcome::Skipped(err) => {
                // 源根本没被查询（排队耗尽），不能按源站故障冷却，只记录到本次 errors。
                st.errors.push(web_book::SourceFailure {
                    source: src.book_source_url.clone(),
                    source_name: src.book_source_name.clone(),
                    error: err,
                });
            }
        }
        st.finished += 1;
        st.pending = st.pending.saturating_sub(1);
        drop(st);
    }
    outcomes
}
