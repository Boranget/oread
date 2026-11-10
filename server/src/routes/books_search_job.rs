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
    /// 因全局预算耗尽而未执行/未完成的源数（不写健康状态）
    pub(super) skipped: usize,
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
    /// 因全局预算耗尽而“未执行/未完成”的源数（不写入健康状态）。
    pub(super) skipped: usize,
}

/// 正在运行的搜索任务数：后台体检用它判断是否该让路（见 `source_bench`）。
static ACTIVE_SEARCH_JOBS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct SearchActiveGuard;

impl SearchActiveGuard {
    fn new() -> Self {
        ACTIVE_SEARCH_JOBS.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for SearchActiveGuard {
    fn drop(&mut self) {
        ACTIVE_SEARCH_JOBS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 是否有用户搜索正在跑；后台体检会先等搜索空闲，避免抢并发/预算。
pub(crate) fn searches_active() -> bool {
    ACTIVE_SEARCH_JOBS.load(Ordering::Relaxed) > 0
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
    let skipped = st.skipped;
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
        skipped,
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
                skipped: st.skipped,
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

/// 该失败文案是否属于“超时”（单源预算用尽或底层请求超时）。
///
/// 超时在聚合搜索里是**不确定**证据：同一个源在体检（低并发）下可能很快。
/// 因此它只记 `partial`，不参与连续失败/冷却判定。
pub(super) fn is_timeout_message(msg: &str) -> bool {
    msg.contains("超时")
}

/// 搜索失败的三元归类：`(健康状态, 错误 kind, 是否计入搜索统计)`。
///
/// - **超时**：`partial` + 不计统计。16 路并发 + 单源预算下超时是「未知」，
///   既不能累计连续失败（会 24h 冷却/隔离），也不能算一次 0 命中
///   （否则冷门关键词搜两轮就把正常源打成隔离）。
/// - **站点故障**（连接失败/HTTP 非成功）：`fail` + 计入，这是明确的站点问题。
/// - **引擎故障**（选择器/JSONPath/JS）：`partial` + 计入，留证据但不判死站点。
pub(super) fn classify_search_failure(
    msg: &str,
    engine: bool,
) -> (&'static str, &'static str, bool) {
    if is_timeout_message(msg) {
        (source_health::STATUS_PARTIAL, "timeout", false)
    } else if engine {
        (source_health::STATUS_PARTIAL, "engine", true)
    } else {
        (source_health::STATUS_FAIL, "site", true)
    }
}

/// 全局预算是否“明显不足”单源预算。
///
/// 留 1s 宽限：剩余 7.2s / 单源 8s 仍按真实单源超时处理，
/// 避免把“几乎跑满单源预算”的站点误判成预算截断而漏掉站点故障。
pub(super) fn timeout_is_truncation(remain: Duration, budget: Duration) -> bool {
    remain.saturating_add(Duration::from_secs(1)) < budget
}

/// 早停门槛：已经拿到两页以上的书名匹配结果，且至少跑过一批源，才提前收工。
///
/// 宁可多跑几个源，也不要在冷门查询上因为「前几个源恰好命中」而错过正主：
/// 阈值取一页（30）的两倍，并要求至少 24 个源已完成。
const EARLY_STOP_KEPT: i64 = 60;
const EARLY_STOP_MIN_FINISHED: usize = 24;

/// 单个源的一次执行结果（尚未写入 job 状态）。
pub(super) struct SourceRun {
    pub(super) src: crate::entities::BookSource,
    pub(super) outcome: TaskOutcome,
    pub(super) latency_ms: i64,
    pub(super) attempted: bool,
}

pub(super) enum TaskOutcome {
    Done(anyhow::Result<Vec<crate::entities::SearchBook>>),
    /// 用满单源预算仍超时：记为「未知」（partial + 不计产出），不判死站点
    Timeout(String),
    /// 全局 deadline 提前截断（剩余预算明显不足单源预算）：不算站点失败
    Truncated(String),
    /// 排队到全局 deadline 仍未拿到并发槽，或剩余预算已不足以发请求
    Skipped(String),
}

/// 动态单源预算：剩余时间按「还剩几轮并发」摊，最多给 2 份，夹在 [2s, cap]。
///
/// 固定 8s 上限在尾部会让慢源吃掉整批预算；这里让越接近 deadline 的请求
/// 拿到越短的预算，保证「还能再试一个源」而不是「卡在最后一个源上超时」。
pub(super) fn dynamic_budget(
    deadline: tokio::time::Instant,
    remaining_sources: usize,
    slots: usize,
    cap: Duration,
) -> Duration {
    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
    let slots = slots.max(1);
    let waves_left = remaining_sources.max(1).div_ceil(slots).max(1) as u32;
    let share = left / waves_left;
    let floor = Duration::from_secs(2).min(cap);
    (share * 2).clamp(floor, cap)
}

pub(super) fn should_stop_early(kept_total: i64, finished: usize, total: usize) -> bool {
    finished >= EARLY_STOP_MIN_FINISHED.min(total) && kept_total >= EARLY_STOP_KEPT
}

fn sort_queue(
    queue: &mut [crate::entities::BookSource],
    health: &HashMap<String, source_health::HealthRow>,
    prior: &crate::search_priority::SearchPrior,
) {
    queue.sort_by_key(|s| prior.priority(&s.book_source_url, health.get(&s.book_source_url)));
}

fn resort_queue(
    queue: &Mutex<Vec<crate::entities::BookSource>>,
    health: &HashMap<String, source_health::HealthRow>,
    prior: &crate::search_priority::SearchPrior,
) {
    let mut g = queue.lock().unwrap();
    sort_queue(&mut g, health, prior);
}

/// 一次搜索的 anytime 调度：单一优先队列，每完成一个源就按最新证据重排。
///
/// 与旧的两批 `join!` 相比：
/// - 不再让 `preferred` 整批占住槽位，慢的优选源不会挡住高产出普通源；
/// - 每完成一个源就把结果折进证据，后续源的顺序反映本次搜索的实际产出；
/// - 槽位空出即取当前最优的源，不需要等整轮跑完；
/// - 达到早停门槛或 deadline 时停止**取新源**，已在飞的请求照常完成。
#[allow(clippy::too_many_arguments)] // 搜索任务上下文（job/源/关键词/池/预算/取消）。
pub(super) async fn run_search_job(
    job: Arc<SearchJob>,
    sources: Vec<crate::entities::BookSource>,
    key: String,
    pool: SqlitePool,
    book_pool: SqlitePool,
    store: Arc<web_book::BookStore>,
    cooldown_secs: u64,
    tier_cfg: crate::source_quality::TierConfig,
    mut health: HashMap<String, source_health::HealthRow>,
    mut prior: crate::search_priority::SearchPrior,
    scope: crate::req::SearchScope,
) {
    // 后台体检通过 `searches_active()` 感知用户搜索，先让路再跑。
    let _active = SearchActiveGuard::new();
    let per_source_cap = crate::req::search_budget_for(scope);
    let total_budget = crate::req::search_total_budget(scope);
    // 排队等槽不计入单源预算，但整次搜索仍受此上限约束。
    let deadline = tokio::time::Instant::now() + total_budget;
    let slots = crate::req::search_slots(scope).max(1);

    let mut queue: Vec<crate::entities::BookSource> = sources;
    sort_queue(&mut queue, &health, &prior);
    let source_total = queue.len();
    {
        let mut st = job.state.lock().unwrap();
        // anytime 调度下所有源都在同一个优先队列里，`scheduled` 在真正取源时累加，
        // 这样进度接口仍能显示「已排上 / 尚未排上」，而不是一开始就全量排上。
        st.pending = source_total;
    }
    tracing::debug!(
        target: "search_schedule",
        job_id = %job.id,
        scope = scope.as_str(),
        query_class = prior.class.as_str(),
        sources = source_total,
        slots,
        "anytime search start"
    );

    let queue = Arc::new(Mutex::new(queue));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut outcomes: Vec<source_health::Outcome> = Vec::new();
    let mut kept_total: i64 = 0;

    // 队列里只放「还没开始跑」的源；已开始的由 buffer_unordered 管理。
    // unfold 每次只在有槽位时被拉取，因此拿到的 `left` 就是当下的剩余源数。
    let src_stream = {
        let q = queue.clone();
        let stop = stop.clone();
        let job = job.clone();
        futures::stream::unfold(q, move |q| {
            let stop = stop.clone();
            let job = job.clone();
            async move {
                if stop.load(Ordering::Relaxed) {
                    return None;
                }
                let next = {
                    let mut g = q.lock().unwrap();
                    if g.is_empty() {
                        None
                    } else {
                        Some((g.remove(0), g.len()))
                    }
                };
                if next.is_some() {
                    // 只有真正从队列取出（交给 buffer_unordered）才算「已排上」。
                    job.state.lock().unwrap().scheduled += 1;
                }
                next.map(|(src, left)| ((src, left), q))
            }
        })
    };

    // 漫画源走独立小池（1/4 槽）。给 stream 多留出这部分并发，
    // 否则等漫画槽的小说任务会占住 stream 槽位，拖慢小说源。
    let stream_slots = slots + crate::req::comic_slot_count(slots);
    let inflight = src_stream
        .map(|(src, left)| {
            let key = key.clone();
            let bp = book_pool.clone();
            let st = store.clone();
            let budget = dynamic_budget(deadline, left + 1, slots, per_source_cap);
            async move { search_one_source(src, key, deadline, budget, scope, bp, st).await }
        })
        .buffer_unordered(stream_slots);
    // `unfold` 的闭包返回非 Unpin 的 async block，`next()` 需要显式钉住整个流。
    futures::pin_mut!(inflight);

    while let Some(run) = inflight.next().await {
        let SourceRun {
            src,
            outcome,
            latency_ms,
            attempted,
        } = run;
        // 逐源耗时埋点（默认 debug 不输出；扫参时开 `RUST_LOG=search_source=debug`）。
        // 排队时间不计入 latency_ms，`timed_out` 只标真实请求超时，与 Skipped 区分。
        // `hits` = 解析条数；`kept_hits` = 其中书名真匹配的条数（搜索产出率只用后者）。
        let (kind, hits, kept_hits, timed_out) = match &outcome {
            TaskOutcome::Done(Ok(b)) => (
                "ok",
                b.len() as i64,
                crate::title_filter::count_title_matches(b, &key),
                false,
            ),
            TaskOutcome::Done(Err(_)) => ("error", 0, 0, false),
            TaskOutcome::Timeout(_) => ("timeout", 0, 0, true),
            TaskOutcome::Truncated(_) => ("truncated", 0, 0, false),
            TaskOutcome::Skipped(_) => ("skipped", 0, 0, false),
        };
        tracing::debug!(
            target: "search_source",
            source = %src.book_source_url,
            scope = scope.as_str(),
            kind,
            timed_out,
            latency_ms,
            hits,
            kept_hits,
            "search source done"
        );
        let rule_complete = crate::source_quality::rule_completeness(&src);
        let mut recorded: Option<source_health::Outcome> = None;
        let mut st = job.state.lock().unwrap();
        if attempted {
            st.attempted += 1;
        }
        match outcome {
            TaskOutcome::Done(Ok(mut books)) => {
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
                recorded = Some(source_health::Outcome {
                    source_url: src.book_source_url.clone(),
                    status: source_health::STATUS_OK,
                    latency_ms,
                    error: None,
                    hits,
                    kept_hits,
                    parsed: true,
                    usage: true,
                    query_class: prior.class,
                    rule_complete,
                });
            }
            TaskOutcome::Done(Err(e)) => {
                let msg = format!("{e}");
                // 超时是「未知」而非「站点已死」：16 路并发 + 单源预算下，超时更多是
                // 本机排队放大。与 truncated/skipped 同口径：不累计失败、不冷却、不写命中统计。
                let engine = matches!(
                    source_health::classify(&e),
                    source_health::FailureKind::Engine
                );
                let (status, kind, usage) = classify_search_failure(&msg, engine);
                st.errors.push(web_book::SourceFailure {
                    source: src.book_source_url.clone(),
                    source_name: src.book_source_name.clone(),
                    error: msg.clone(),
                    kind: kind.to_string(),
                });
                recorded = Some(source_health::Outcome {
                    source_url: src.book_source_url.clone(),
                    status,
                    latency_ms,
                    error: Some(msg),
                    hits: 0,
                    kept_hits: 0,
                    parsed: false,
                    usage,
                    query_class: prior.class,
                    rule_complete,
                });
            }
            TaskOutcome::Timeout(err) => {
                // 超时只说明「这次没跑完」，不能据此把源判死：
                // - 记 partial（不累计连续失败、不触发 24h 冷却、不拉低连通性到 0）；
                // - usage=false（不把未完成的请求算成一次 0 命中，否则冷门关键词
                //   搜两轮就会把大量正常源打成隔离）。
                // 站点是否真的坏由体检/巡检（更低并发、独立预算）判定。
                st.errors.push(web_book::SourceFailure {
                    source: src.book_source_url.clone(),
                    source_name: src.book_source_name.clone(),
                    error: err.clone(),
                    kind: "timeout".to_string(),
                });
                recorded = Some(source_health::Outcome {
                    source_url: src.book_source_url.clone(),
                    status: source_health::STATUS_PARTIAL,
                    latency_ms,
                    error: Some(err),
                    hits: 0,
                    kept_hits: 0,
                    parsed: false,
                    usage: false,
                    query_class: prior.class,
                    rule_complete,
                });
            }
            TaskOutcome::Truncated(err) => {
                // 全局预算截断：请求可能已发出但没跑完，不是站点故障。
                // 不写健康记录，避免连续 3 次预算截断把健康源打成隔离/冷却。
                st.skipped += 1;
                st.errors.push(web_book::SourceFailure {
                    source: src.book_source_url.clone(),
                    source_name: src.book_source_name.clone(),
                    error: err,
                    kind: "truncated".to_string(),
                });
            }
            TaskOutcome::Skipped(err) => {
                // 源根本没被查询（排队耗尽），不能按源站故障冷却，只记录到本次 errors。
                st.skipped += 1;
                st.errors.push(web_book::SourceFailure {
                    source: src.book_source_url.clone(),
                    source_name: src.book_source_name.clone(),
                    error: err,
                    kind: "skipped".to_string(),
                });
            }
        }
        st.finished += 1;
        st.pending = st.pending.saturating_sub(1);
        let finished = st.finished;
        drop(st);

        // 在线更新证据：本次结果立即影响后续源的顺序。落库仍在收尾统一做。
        if let Some(o) = recorded {
            if o.usage {
                prior.observe(&o.source_url, o.kept_hits);
                let row = source_health::apply_outcome(
                    health.get(&o.source_url),
                    &o,
                    source_health::now_ms(),
                    cooldown_secs,
                );
                health.insert(o.source_url.clone(), row);
            }
            outcomes.push(o);
        }
        kept_total += kept_hits;
        resort_queue(&queue, &health, &prior);

        if should_stop_early(kept_total, finished, source_total) {
            tracing::debug!(
                target: "search_schedule",
                job_id = %job.id,
                finished,
                kept_total,
                remaining = queue.lock().unwrap().len(),
                reason = "enough_results",
                "early stop"
            );
            stop.store(true, Ordering::Relaxed);
        } else if tokio::time::Instant::now() >= deadline {
            stop.store(true, Ordering::Relaxed);
        }
    }

    // 提前停止/deadline 后仍未取出的源：记为 skipped（不写健康、不算站点失败）。
    {
        let mut g = queue.lock().unwrap();
        let mut st = job.state.lock().unwrap();
        for s in g.drain(..) {
            st.skipped += 1;
            st.errors.push(web_book::SourceFailure {
                source: s.book_source_url.clone(),
                source_name: s.book_source_name.clone(),
                error: "全局搜索预算耗尽（排队未及执行）".to_string(),
                kind: "skipped".to_string(),
            });
            st.finished += 1;
            st.pending = st.pending.saturating_sub(1);
        }
    }

    if let Err(e) = source_health::record_outcomes(&pool, &outcomes, cooldown_secs, &tier_cfg).await
    {
        tracing::warn!(error = %e, "record source health after search failed");
    }
}

/// 跑一个源：拿源级并发槽 → 单源预算内请求 → 分类超时/截断。
///
/// 排队时间不计入 `latency_ms`；`timed_out` 只标真实请求超时，与 Skipped 区分。
pub(super) async fn search_one_source(
    s: crate::entities::BookSource,
    key: String,
    deadline: tokio::time::Instant,
    budget: Duration,
    scope: crate::req::SearchScope,
    book_pool: SqlitePool,
    store: Arc<web_book::BookStore>,
) -> SourceRun {
    let comic = crate::req::is_comic_source_type(s.book_source_type);
    let queued_at = Instant::now();
    let slot = match crate::req::acquire_search_slot(comic, deadline, scope).await {
        Ok(p) => p,
        Err(e) => {
            let waited = queued_at.elapsed().as_millis() as i64;
            return SourceRun {
                src: s,
                outcome: TaskOutcome::Skipped(format!("{e}")),
                latency_ms: waited,
                attempted: false,
            };
        }
    };
    let started = Instant::now();
    let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
    // 剩余不足 500ms 时发请求只会制造必然超时的假失败（超时虽按「未知」处理，
    // 但会占用槽位与错误列表）；直接按预算耗尽归为 skipped。
    if remain < Duration::from_millis(500) {
        drop(slot);
        return SourceRun {
            src: s,
            outcome: TaskOutcome::Skipped("全局搜索预算耗尽（排队未及执行）".to_string()),
            latency_ms: started.elapsed().as_millis() as i64,
            attempted: false,
        };
    }
    let r = tokio::time::timeout(
        remain.min(budget),
        web_book::search_book(&s, &key, 1, Some(store.as_ref())),
    )
    .await;
    // 搜索阶段的 `@put`（爱看写 bid）按 book_key 落库，供后续 info/toc 读回。
    let changes = store.take_changes();
    if !changes.is_empty() {
        persist_book_changes(&book_pool, changes).await;
    }
    let latency_ms = started.elapsed().as_millis() as i64;
    drop(slot);
    let outcome = match r {
        Ok(x) => TaskOutcome::Done(x),
        Err(_) => {
            if timeout_is_truncation(remain, budget) {
                TaskOutcome::Truncated(format!(
                    "全局搜索预算耗尽（排队后仅剩 {}ms，未完成请求）",
                    remain.as_millis()
                ))
            } else {
                TaskOutcome::Timeout(format!("搜索超时（单源 {}s 预算）", budget.as_secs()))
            }
        }
    };
    SourceRun {
        src: s,
        outcome,
        latency_ms,
        attempted: true,
    }
}
