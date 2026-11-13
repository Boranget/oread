//! books 路由的 search_job 部分（move-only 从 books.rs 拆出）。
//!
//! 调度模型：**工作量驱动**，不是时间驱动。
//! - 单源预算固定（`search_budget_for`），超时只记「未知」，不判死站点；
//! - 去掉整批时间预算：跑多少源、出多少条都不设上限；
//! - 唯一的任务终止条件是**结果体积**（`search_result_max_bytes`），
//!   到顶后停止取新源，已发出的请求照常收尾；
//! - 结果边搜边落库（`books_search_books`），内存只保留计数。

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
        // 淘汰最旧的查询：Instant 越小越早，不能用 elapsed 取 min（那会淘汰最新的）。
        if let Some((idx, _)) = g.iter().enumerate().min_by_key(|(_, e)| e.at) {
            g.remove(idx);
        } else {
            break;
        }
    }
}

// ---------- 两段式聚合搜索：先返回已完成源，轮询补齐 ----------
/// 运行中任务的存活上限（仅内存淘汰兜底；正常任务由结果体积收尾）。
pub(super) const SEARCH_JOB_TTL_SECS: u64 = 60;
/// 已完成任务在内存保留 10 分钟；更久之后由数据库行表接管（仍可在「搜索任务」里查看）。
pub(super) const SEARCH_JOB_DONE_TTL_SECS: u64 = 600;
pub(super) const SEARCH_JOB_MAX: usize = 64;
pub(super) const SEARCH_JOB_MAX_ACTIVE: usize = 64;
/// 每个用户保留的已完成任务数（更旧的由数据库接管，内存不长期占用）。
pub(super) const SEARCH_JOB_MAX_PER_USER: usize = 20;
/// 单用户同时运行上限：任务会推进整份书源快照的调度，不限制会让“多点几次搜索”
/// 把设备的并发槽和磁盘写带宽打满。
pub(super) const SEARCH_JOB_MAX_ACTIVE_PER_USER: usize = 8;
/// 单个源单次返回的最多结果数：防止一个异常源一次撑爆结果体积预算。
pub(super) const SEARCH_PER_SOURCE_MAX: usize = 200;
/// 内存中保留的错误明细上限：超出只累计计数，避免错误列表随源数增长。
pub(super) const SEARCH_ERRORS_MAX: usize = 200;

pub(super) struct SearchJob {
    pub(super) id: String,
    /// 创建者用户 id：结果接口只允许本人读取，跨用户查询一律 404。
    pub(super) uid: i64,
    pub(super) key: String,
    pub(super) at: Instant,
    /// 创建时刻（Unix 毫秒），仅用于日志定位“为什么 1 秒内 404”
    pub(super) created_unix_ms: u64,
    pub(super) source_total: usize,
    /// 本次搜索范围（curated / all），用于结果接口回显与并发选择。
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
    /// 运行期计数。结果本体在 `search_job_books`，这里不保存书。
    pub(super) state: Mutex<SearchJobState>,
    /// 取消信号：`notify_one()` 唤醒调度循环，立即停止取新源并丢弃在飞请求。
    pub(super) cancel: Arc<tokio::sync::Notify>,
    pub(super) cancelled: AtomicBool,
    /// running / done / cancelled；中断态只存在于数据库（进程重启后由列表接口标注）。
    pub(super) status: Mutex<String>,
    /// 结束时刻（Unix 毫秒，0 = 仍在运行）。
    pub(super) finished_unix_ms: AtomicU64,
}

impl SearchJob {
    pub(super) fn status_str(&self) -> String {
        self.status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub(super) fn is_running(&self) -> bool {
        self.status_str() == "running"
    }

    pub(super) fn meta(&self) -> JobMeta {
        JobMeta {
            id: self.id.clone(),
            uid: self.uid,
            key: self.key.clone(),
            scope: self.scope,
            source_mode: self.source_mode.clone(),
            coverage: self.coverage.clone(),
            curated_total: self.curated_total,
            source_total: self.source_total,
            scope_filtered: self.scope_filtered,
            skipped_cooldown: self.skipped_cooldown,
            cooldown_total: self.cooldown_total,
            status: self.status_str(),
            created_unix_ms: self.created_unix_ms,
            finished_unix_ms: self.finished_unix_ms.load(Ordering::Relaxed),
        }
    }
}

/// 结果接口需要的任务元数据：内存任务与数据库行共用同一份响应组装。
#[derive(Debug, Clone)]
pub(crate) struct JobMeta {
    pub(crate) id: String,
    pub(crate) uid: i64,
    pub(crate) key: String,
    pub(crate) scope: crate::req::SearchScope,
    pub(crate) source_mode: String,
    pub(crate) coverage: String,
    pub(crate) curated_total: usize,
    pub(crate) source_total: usize,
    /// 精选收缩排除掉的可搜索源数量（显式源为 0）。
    pub(crate) scope_filtered: usize,
    pub(crate) skipped_cooldown: usize,
    pub(crate) cooldown_total: usize,
    pub(crate) status: String,
    pub(crate) created_unix_ms: u64,
    pub(crate) finished_unix_ms: u64,
}

/// 任务级计数：轮询、分页、`/k` 进度都只读这些，不再扫结果表或克隆全部书。
#[derive(Debug, Clone, Default)]
pub(crate) struct JobCounters {
    pub(crate) errors: Vec<web_book::SourceFailure>,
    /// 真实失败数（不含 skipped/truncated），错误明细被截断后仍然准确。
    pub(crate) failed: usize,
    pub(crate) finished: usize,
    pub(crate) pending: usize,
    pub(crate) scheduled: usize,
    pub(crate) attempted: usize,
    pub(crate) skipped: usize,
    pub(crate) cancelled: usize,
    /// 已落库的结果条数（去重后、书名匹配后）
    pub(crate) result_count: usize,
    /// 通过相关性门槛的候选条数（书名过滤前）
    pub(crate) raw_count: usize,
    pub(crate) result_bytes: i64,
    /// completed / max_bytes / cancelled
    pub(crate) stop_reason: String,
}

impl JobCounters {
    pub(crate) fn done(&self) -> bool {
        self.pending == 0
    }
}

pub(super) fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
pub(super) struct SearchJobState {
    pub(super) errors: Vec<web_book::SourceFailure>,
    pub(super) failed: usize,
    /// 已处理完的源数（含成功/失败/排队未及执行）
    pub(super) finished: usize,
    /// 尚未处理完的源数（legacy 语义；与 `scheduled` 不同）
    pub(super) pending: usize,
    /// 已进入调度的源数（两段式下逐批累加）
    pub(super) scheduled: usize,
    /// 真正拿到并发槽并发出过至少一次请求的源数
    pub(super) attempted: usize,
    /// 未执行/未完成的源数（取消或结果体积到顶；不写健康状态）
    pub(super) skipped: usize,
    /// 因用户取消而未执行/被中断的源数（不写健康状态）
    pub(super) cancelled: usize,
    pub(super) result_count: usize,
    pub(super) raw_count: usize,
    pub(super) result_bytes: i64,
    pub(super) stop_reason: String,
}

impl SearchJobState {
    /// 记一条错误：明细最多 [`SEARCH_ERRORS_MAX`] 条，计数始终准确。
    fn push_error(&mut self, e: web_book::SourceFailure) {
        if !matches!(e.kind.as_str(), "truncated" | "skipped") {
            self.failed += 1;
        }
        if self.errors.len() < SEARCH_ERRORS_MAX {
            self.errors.push(e);
        }
    }
}

pub(super) fn job_registry() -> &'static Mutex<Vec<Arc<SearchJob>>> {
    static JOBS: OnceLock<Mutex<Vec<Arc<SearchJob>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(Vec::new()))
}

pub(super) fn job_pending(j: &Arc<SearchJob>) -> Option<usize> {
    j.state.try_lock().ok().map(|st| st.pending)
}

pub(super) fn prune_jobs(list: &mut Vec<Arc<SearchJob>>) {
    // 运行中的任务不做时间淘汰：源集合有限、每源有单源预算，任务一定会结束。
    // 这里的硬上限只兜底异常卡死（任务 panic / 取消信号丢失），避免永久占内存。
    list.retain(|j| j.is_running() || j.at.elapsed().as_secs() < SEARCH_JOB_DONE_TTL_SECS);
    list.retain(|j| !j.is_running() || j.at.elapsed().as_secs() < SEARCH_JOB_TTL_SECS * 5);
    // 第一优先：淘汰“已完成且最旧”的任务。进行中的任务不参与淘汰，
    // 否则并发 start/轮询下，刚创建的 job 会被后来的 start 顶掉并 404。
    while list.len() > SEARCH_JOB_MAX {
        let victim = list
            .iter()
            .enumerate()
            .filter(|(_, j)| !j.is_running())
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
    // 单用户保留上限：避免一个用户的历史任务把全局列表挤满。
    prune_per_user(list);
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

/// 每个用户最多保留 [`SEARCH_JOB_MAX_PER_USER`] 个已完成任务（运行中的不淘汰）。
fn prune_per_user(list: &mut Vec<Arc<SearchJob>>) {
    let mut by_user: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, j) in list.iter().enumerate() {
        by_user.entry(j.uid).or_default().push(i);
    }
    let mut drop_idx: Vec<usize> = Vec::new();
    for idxs in by_user.values() {
        let mut done: Vec<usize> = idxs
            .iter()
            .copied()
            .filter(|i| !list[*i].is_running())
            .collect();
        if done.len() <= SEARCH_JOB_MAX_PER_USER {
            continue;
        }
        done.sort_by_key(|i| std::cmp::Reverse(list[*i].created_unix_ms));
        drop_idx.extend(done.into_iter().skip(SEARCH_JOB_MAX_PER_USER));
    }
    drop_idx.sort_unstable_by(|a, b| b.cmp(a));
    for i in drop_idx {
        list.remove(i);
    }
}

/// 当前用户的内存任务（运行中 + 最近完成），按创建时间倒序。
pub(super) fn user_jobs(uid: i64) -> Vec<Arc<SearchJob>> {
    let mut g = job_registry().lock().unwrap_or_else(|p| p.into_inner());
    prune_jobs(&mut g);
    let mut list: Vec<Arc<SearchJob>> = g.iter().filter(|j| j.uid == uid).cloned().collect();
    list.sort_by_key(|j| std::cmp::Reverse(j.created_unix_ms));
    list
}

pub(super) fn find_job(id: &str) -> Option<Arc<SearchJob>> {
    let mut g = job_registry().lock().unwrap_or_else(|p| p.into_inner());
    prune_jobs(&mut g);
    g.iter().find(|j| j.id == id).cloned()
}

/// 从内存注册表移除；返回是否真的移除了。
pub(super) fn remove_job(id: &str, uid: i64) -> bool {
    let mut g = job_registry().lock().unwrap_or_else(|p| p.into_inner());
    let before = g.len();
    g.retain(|j| !(j.id == id && j.uid == uid));
    g.len() != before
}

/// 该用户当前运行中的任务数（start 前做单用户并发限制）。
pub(super) fn active_jobs_for_user(uid: i64) -> usize {
    let mut g = job_registry().lock().unwrap_or_else(|p| p.into_inner());
    prune_jobs(&mut g);
    g.iter().filter(|j| j.uid == uid && j.is_running()).count()
}

pub(super) fn next_job_id() -> String {
    static SEQ: AtomicI64 = AtomicI64::new(0);
    format!(
        "j{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
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

/// 当前任务计数快照（内存任务用）。
pub(super) fn job_counters(job: &SearchJob) -> JobCounters {
    let st = job.state.lock().unwrap_or_else(|p| p.into_inner());
    JobCounters {
        errors: st.errors.clone(),
        failed: st.failed,
        finished: st.finished,
        pending: st.pending,
        scheduled: st.scheduled,
        attempted: st.attempted,
        skipped: st.skipped,
        cancelled: st.cancelled,
        result_count: st.result_count,
        raw_count: st.raw_count,
        result_bytes: st.result_bytes,
        stop_reason: st.stop_reason.clone(),
    }
}

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
    /// 排队/发请求前就被放弃（取消或结果体积到顶）：不算站点失败
    Skipped(String),
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
/// 与时间驱动的区别：
/// - 没有整批 deadline，也不再有“接近 deadline 缩短单源预算”；
/// - 不再因为“已经够多结果”提前收工，源多就一定跑完；
/// - 每完成一个源就把结果写进 `search_job_books`，内存不随结果增长；
/// - 唯一终止预算是结果体积：`result_bytes >= max_bytes` 时停止取新源。
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
    let per_source = crate::req::search_budget_for(scope);
    let max_bytes = crate::req::search_result_max_bytes().max(1) as i64;
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
        max_bytes,
        "anytime search start"
    );

    let queue = Arc::new(Mutex::new(queue));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut outcomes: Vec<source_health::Outcome> = Vec::new();

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
                        Some(g.remove(0))
                    }
                };
                if next.is_some() {
                    // 只有真正从队列取出（交给 buffer_unordered）才算「已排上」。
                    job.state.lock().unwrap().scheduled += 1;
                }
                next.map(|src| (src, q))
            }
        })
    };

    // 漫画源走独立小池（1/4 槽）。给 stream 多留出这部分并发，
    // 否则等漫画槽的小说任务会占住 stream 槽位，拖慢小说源。
    let stream_slots = slots + crate::req::comic_slot_count(slots);
    let inflight = src_stream
        .map(|src| {
            let key = key.clone();
            let bp = book_pool.clone();
            let st = store.clone();
            async move { search_one_source(src, key, per_source, scope, bp, st).await }
        })
        .buffer_unordered(stream_slots);
    // `unfold` 的闭包返回非 Unpin 的 async block，`next()` 需要显式钉住整个流。
    futures::pin_mut!(inflight);

    // 用户取消：`biased` 让取消信号优先于新结果，一旦命中立刻停止取新源，
    // 直接丢弃 buffer_unordered 里的在飞请求（它们不再写健康/结果）。
    let mut cancelled_by_user = false;
    loop {
        if job.cancelled.load(Ordering::Relaxed) {
            cancelled_by_user = true;
            break;
        }
        let next = tokio::select! {
            biased;
            _ = job.cancel.notified() => {
                cancelled_by_user = true;
                break;
            }
            next = inflight.next() => next,
        };
        let Some(run) = next else { break };
        let SourceRun {
            src,
            outcome,
            latency_ms,
            attempted,
        } = run;
        // 逐源耗时埋点（默认 debug 不输出；扫参时开 `RUST_LOG=search_source=debug`）。
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
        let mut wrote: Option<super::books_search_books::BatchWrite> = None;
        let mut new_error: Option<web_book::SourceFailure> = None;
        let mut skipped_now = false;

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
                // 边搜边落库：一个源一个事务。写库失败只记日志，不中断整次搜索。
                match super::books_search_books::insert_batch(
                    &pool, &job.id, &key, src.weight, books,
                )
                .await
                {
                    Ok(w) => wrote = Some(w),
                    Err(e) => tracing::warn!(
                        target: "search_job",
                        job_id = %job.id,
                        source = %src.book_source_url,
                        error = %e,
                        "persist search batch failed"
                    ),
                }
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
                // 超时是「未知」而非「站点已死」：并发 + 单源预算下，超时更多是
                // 本机排队放大。与其他未完成分支同口径：不累计失败、不冷却。
                let engine = matches!(
                    source_health::classify(&e),
                    source_health::FailureKind::Engine
                );
                let (status, kind, usage) = source_health::classify_search_failure(&msg, engine);
                new_error = Some(web_book::SourceFailure {
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
                new_error = Some(web_book::SourceFailure {
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
            TaskOutcome::Skipped(err) => {
                // 源根本没被查询（取消/体积到顶），不能按源站故障冷却，只记录到本次。
                skipped_now = true;
                new_error = Some(web_book::SourceFailure {
                    source: src.book_source_url.clone(),
                    source_name: src.book_source_name.clone(),
                    error: err,
                    kind: "skipped".to_string(),
                });
            }
        }

        let (finished, bytes, stop_now) = {
            let mut st = job.state.lock().unwrap_or_else(|p| p.into_inner());
            if attempted {
                st.attempted += 1;
            }
            if skipped_now {
                st.skipped += 1;
            }
            if let Some(w) = wrote {
                st.result_count += w.added;
                st.raw_count += w.candidates;
                st.result_bytes += w.bytes_added;
            }
            if let Some(e) = new_error {
                st.push_error(e);
            }
            st.finished += 1;
            st.pending = st.pending.saturating_sub(1);
            let bytes = st.result_bytes;
            // 唯一的终止预算：结果体积到顶。已发出的请求照常收尾，不再取新源。
            let stop_now = bytes >= max_bytes;
            if stop_now {
                st.stop_reason = "max_bytes".to_string();
            }
            (st.finished, bytes, stop_now)
        };

        // 在线更新证据：本次结果立即影响后续源的顺序。落库在批次写入时已完成。
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
        resort_queue(&queue, &health, &prior);

        if stop_now {
            tracing::debug!(
                target: "search_schedule",
                job_id = %job.id,
                finished,
                bytes,
                max_bytes,
                remaining = queue.lock().unwrap().len(),
                reason = "result_size",
                "stop taking new sources"
            );
            stop.store(true, Ordering::Relaxed);
        }
    }

    if cancelled_by_user {
        // 取消：清空排队 + 丢弃在飞请求。未完成的源计入 `cancelled`（既不写健康、
        // 也不算站点失败），已完成源的结果保留，用户仍可查看。
        queue.lock().unwrap_or_else(|p| p.into_inner()).clear();
        let mut st = job.state.lock().unwrap_or_else(|p| p.into_inner());
        let left = st.pending;
        st.cancelled += left;
        st.pending = 0;
        st.stop_reason = "cancelled".to_string();
        let finished = st.finished;
        drop(st);
        tracing::info!(
            target: "search_job",
            job_id = %job.id,
            cancelled_sources = left,
            finished,
            "search job cancelled by user"
        );
    } else {
        // 结果体积到顶后仍未取出的源：记为 skipped（不写健康、不算站点失败）。
        let mut g = queue.lock().unwrap();
        let mut st = job.state.lock().unwrap_or_else(|p| p.into_inner());
        for s in g.drain(..) {
            st.skipped += 1;
            st.push_error(web_book::SourceFailure {
                source: s.book_source_url.clone(),
                source_name: s.book_source_name.clone(),
                error: "结果体积达到上限，未执行".to_string(),
                kind: "skipped".to_string(),
            });
            st.finished += 1;
            st.pending = st.pending.saturating_sub(1);
        }
        if st.stop_reason.is_empty() {
            st.stop_reason = "completed".to_string();
        }
    }

    if let Err(e) = source_health::record_outcomes(&pool, &outcomes, cooldown_secs, &tier_cfg).await
    {
        tracing::warn!(error = %e, "record source health after search failed");
    }
    let status = if cancelled_by_user {
        "cancelled"
    } else {
        "done"
    };
    finish_job(&job, &pool, status).await;
}

/// 任务收尾：固化状态/结束时刻/计数，并把停止原因与错误明细写进数据库。
///
/// 结果本体早已按源增量写入 `search_job_books`，这里不再写大快照。
async fn finish_job(job: &Arc<SearchJob>, pool: &SqlitePool, status: &str) {
    *job.status.lock().unwrap_or_else(|p| p.into_inner()) = status.to_string();
    job.finished_unix_ms.store(unix_ms(), Ordering::Relaxed);
    let counters = job_counters(job);
    let meta = job.meta();
    if let Err(e) = super::books_search_store::persist_finished(pool, &meta, &counters).await {
        tracing::warn!(
            target: "search_job",
            job_id = %job.id,
            error = %e,
            "persist search job counters failed"
        );
    }
}

/// 跑一个源：拿源级并发槽 → 单源预算内请求 → 分类超时。
///
/// 排队时间不计入 `latency_ms`；`timed_out` 只标真实请求超时，与 Skipped 区分。
pub(super) async fn search_one_source(
    s: crate::entities::BookSource,
    key: String,
    budget: Duration,
    scope: crate::req::SearchScope,
    book_pool: SqlitePool,
    store: Arc<web_book::BookStore>,
) -> SourceRun {
    let comic = crate::req::is_comic_source_type(s.book_source_type);
    let queued_at = Instant::now();
    // 后台任务没有整批 deadline：等槽不设上限，取消由丢弃 future 生效。
    let slot = match crate::req::acquire_search_slot_now(comic, scope).await {
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
    let r = tokio::time::timeout(
        budget,
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
        Err(_) => TaskOutcome::Timeout(format!("搜索超时（单源 {}s 预算）", budget.as_secs())),
    };
    SourceRun {
        src: s,
        outcome,
        latency_ms,
        attempted: true,
    }
}
