use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::auth::{AuthUser, UserDb};
use crate::entities::{Book, BookChapter, BookContent, SearchBook};
use crate::error::{AppError, AppResult};
use crate::routes::AppState;
use crate::routes::replace;
use crate::routes::sources::{load_source, load_sources};
use crate::source_health;
use crate::source_ledger;
use crate::web_book;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/search", get(search))
        .route("/api/search/aggregate", get(search_aggregate))
        .route("/api/search/start", get(search_start))
        .route("/api/search/result", get(search_result))
        .route("/api/explore", get(explore))
        .route("/api/explore/categories", get(explore_categories))
        .route("/api/book/info", get(book_info))
        .route("/api/book/toc", get(toc))
        .route("/api/book/toc/batch", post(toc_batch))
        .route("/api/book/content", get(content))
        .route("/api/book/search", get(book_search))
        .route("/api/book/cache-all", post(cache_all))
        .route("/api/book/cache/clear", post(cache_clear))
        .route("/api/book/switch-source", post(switch_source))
}

#[derive(Deserialize)]
pub struct SearchQ {
    /// 正式参数名；与文档里的 `q` 互为别名，缺两者才 400。
    key: Option<String>,
    #[serde(default)]
    q: Option<String>,
    /// 逗号分隔的书源 id：支持 `book_source_url`（书源主键）或列表顺序的 1-based 序号。
    #[serde(default)]
    source_ids: Option<String>,
    /// 显式越权：为 `1`/`true` 时冷却中的源也参与搜索（排查用）。
    #[serde(default)]
    include_cooled: Option<String>,
    page: Option<i32>,
    page_size: Option<usize>,
}

/// 宽松布尔：`1`/`true`/`yes`/`on` 视为真。
fn truthy(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

impl SearchQ {
    fn keyword(&self) -> String {
        self.key
            .as_deref()
            .or(self.q.as_deref())
            .unwrap_or("")
            .trim()
            .to_string()
    }
}

/// 解析 `use_replace` 参数：返回 `Some(true/false)` 表示显式覆盖，
/// `None` 表示沿用账号设置。目录与正文共用同一套取值规则。
fn parse_use_replace(raw: Option<&str>) -> Option<bool> {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("0") | Some("false") | Some("off") => Some(false),
        Some("1") | Some("true") | Some("on") => Some(true),
        _ => None,
    }
}

/// 合并请求参数与账号设置，得到本次请求最终是否应用替换规则。
async fn resolve_use_replace(raw: Option<&str>, db: &SqlitePool) -> bool {
    match parse_use_replace(raw) {
        Some(v) => v,
        None => replace::replace_rule_enabled(db).await,
    }
}
#[derive(Deserialize)]
pub struct JobResultQ {
    job_id: String,
    page: Option<i32>,
    page_size: Option<usize>,
}
#[derive(Deserialize)]
pub struct ExploreQ {
    url: String,
    page: Option<i32>,
}
#[derive(Deserialize)]
pub struct CategoryQ {
    source: String,
    page: Option<i32>,
}
#[derive(Deserialize)]
pub struct BookQ {
    source: String,
    book_url: String,
    /// 搜索结果带入的字段：详情规则为空/未命中时沿用（Legado 语义），避免被写成 null
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    intro: Option<String>,
    #[serde(default)]
    cover_url: Option<String>,
    /// 目录标题是否应用替换规则：`1`/`0` 显式覆盖用户设置；缺省沿用账号的
    /// `use_replace_rule`（与 `/api/book/content` 同名参数）
    #[serde(default)]
    use_replace: Option<String>,
}
#[derive(Deserialize)]
pub struct ContentQ {
    source: String,
    chapter_url: String,
    /// 显式传入书籍 URL：用于按书名匹配替换规则，并回填到响应
    book_url: Option<String>,
    /// 章节标题兜底（章节记录里没有标题时）
    title: Option<String>,
    /// 是否应用替换规则：`1`/`0` 显式覆盖用户设置；缺省沿用账号设置
    use_replace: Option<String>,
}
#[derive(Deserialize)]
pub struct SwitchQ {
    name: String,
    author: Option<String>,
    exclude_source: Option<String>,
}

/// 每一页最多返回的书籍数（聚合后统一分页）
const DEFAULT_PAGE_SIZE: usize = 30;

/// 聚合搜索结果缓存：同一查询 60s 内复用同一份完整结果，保证翻页不重不漏、
/// `total` 可信；仅保留 4 个查询、每个最多 400 本，内存上界约 1~2 MiB。
const AGG_TTL_SECS: u64 = 60;
const AGG_MAX_QUERIES: usize = 4;
const AGG_MAX_BOOKS: usize = 400;

struct AggEntry {
    key: String,
    at: Instant,
    books: Vec<SearchBook>,
    errors: Vec<web_book::SourceFailure>,
}

fn agg_cache() -> &'static Mutex<Vec<AggEntry>> {
    static CACHE: OnceLock<Mutex<Vec<AggEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

fn prune_cache(g: &mut Vec<AggEntry>) {
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
const SEARCH_JOB_TTL_SECS: u64 = 60;
const SEARCH_JOB_MAX: usize = 32;
const SEARCH_JOB_MAX_ACTIVE: usize = 64;
const SEARCH_PER_SOURCE_MAX: usize = 200;

struct SearchJob {
    id: String,
    #[allow(dead_code)]
    key: String,
    at: Instant,
    /// 创建时刻（Unix 毫秒），仅用于日志定位“为什么 1 秒内 404”
    created_unix_ms: u64,
    /// 启用中的书源快照：用于去重/按权重排序，保证与 aggregate 的排序规则一致
    sources: Vec<crate::entities::BookSource>,
    source_total: usize,
    /// 因冷却被跳过的源数（本次请求）
    skipped_cooldown: usize,
    /// 候选集合中当前处于冷却的源总数（前端提示“另有 N 个源处于冷却”）
    cooldown_total: usize,
    state: Mutex<SearchJobState>,
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
struct SearchJobState {
    books: Vec<SearchBook>,
    errors: Vec<web_book::SourceFailure>,
    finished: usize,
    pending: usize,
}

fn job_registry() -> &'static Mutex<Vec<Arc<SearchJob>>> {
    static JOBS: OnceLock<Mutex<Vec<Arc<SearchJob>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(Vec::new()))
}

fn job_pending(j: &Arc<SearchJob>) -> Option<usize> {
    j.state.try_lock().ok().map(|st| st.pending)
}

fn prune_jobs(list: &mut Vec<Arc<SearchJob>>) {
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

fn next_job_id() -> String {
    static SEQ: AtomicI64 = AtomicI64::new(0);
    format!("j{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed))
}

/// 读取视图：合并已完成源的结果 → 去重排序 → 截断（与 aggregate 同一套规则）
fn job_view(
    job: &SearchJob,
) -> (
    Vec<SearchBook>,
    Vec<web_book::SourceFailure>,
    bool,
    usize,
    usize,
) {
    let (books, errors, finished, pending) = {
        let st = job.state.lock().unwrap();
        (st.books.clone(), st.errors.clone(), st.finished, st.pending)
    };
    let mut full = web_book::dedupe_and_sort(books, &job.sources);
    full.truncate(AGG_MAX_BOOKS);
    (full, errors, pending == 0, finished, pending)
}

async fn run_search_job(
    job: Arc<SearchJob>,
    sources: Vec<crate::entities::BookSource>,
    key: String,
    pool: SqlitePool,
    cooldown_secs: u64,
    tier_cfg: crate::source_quality::TierConfig,
) {
    let budget = crate::req::search_budget();
    // 全局截止时间：排队等槽不计入单源预算，但整次搜索仍受此上限约束（与旧版总耗时同量级）。
    let deadline = tokio::time::Instant::now() + crate::req::search_total_budget();

    enum TaskOutcome {
        Done(anyhow::Result<Vec<crate::entities::SearchBook>>),
        /// 拿到槽后真实请求超时（或全局剩余时间不足被截断）
        Timeout(String),
        /// 排队到全局 deadline 仍未拿到并发槽
        Skipped(String),
    }

    let mut pending = futures::stream::FuturesUnordered::new();
    for src in sources {
        let s = src.clone();
        let k = key.clone();
        pending.push(async move {
            let comic = crate::req::is_comic_source_type(s.book_source_type);
            let queued_at = Instant::now();
            let slot = match crate::req::acquire_search_slot(comic, deadline).await {
                Ok(p) => p,
                Err(e) => {
                    let waited = queued_at.elapsed().as_millis() as i64;
                    return (s, TaskOutcome::Skipped(format!("{e}")), waited);
                }
            };
            let started = Instant::now();
            let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remain.is_zero() {
                drop(slot);
                return (
                    s,
                    TaskOutcome::Skipped("全局搜索预算耗尽（排队未及执行）".to_string()),
                    started.elapsed().as_millis() as i64,
                );
            }
            let r = tokio::time::timeout(remain.min(budget), web_book::search(&s, &k, 1)).await;
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
            (s, outcome, latency_ms)
        });
    }
    // 健康状态只在任务收尾时批量写一次，不随每本书/每条结果写库。
    let mut outcomes: Vec<source_health::Outcome> = Vec::new();
    while let Some((src, outcome, latency_ms)) = pending.next().await {
        let rule_complete = crate::source_quality::rule_completeness(&src);
        let mut st = job.state.lock().unwrap();
        match outcome {
            TaskOutcome::Done(Ok(mut books)) => {
                let hits = books.len() as i64;
                books.truncate(SEARCH_PER_SOURCE_MAX);
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
    if let Err(e) =
        source_health::record_outcomes(&pool, &outcomes, cooldown_secs, &tier_cfg).await
    {
        tracing::warn!(error = %e, "record source health after search failed");
    }
}

/// 解析逗号分隔的 source_ids，去空、去重并保留用户输入顺序。
fn parse_source_ids(raw: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(raw) = raw {
        for tok in raw.split(',') {
            let t = tok.trim();
            if !t.is_empty() && !out.iter().any(|x| x == t) {
                out.push(t.to_string());
            }
        }
    }
    out
}

/// 在「已排序的启用书源」中按 id 选择：
/// - 优先精确匹配 `book_source_url`（书源主键）；
/// - 纯数字按该列表的 1-based 序号解析（兼容文档里的 `source_ids=1,2`）；
/// - 解析不到/不存在/禁用的 id 一律忽略，由调用方在响应里提示。
fn select_sources(
    enabled: &[crate::entities::BookSource],
    requested: &[String],
) -> (Vec<crate::entities::BookSource>, Vec<String>) {
    if requested.is_empty() {
        return (enabled.to_vec(), Vec::new());
    }
    let mut selected: Vec<crate::entities::BookSource> = Vec::new();
    let mut ignored = Vec::new();
    for tok in requested {
        let found = enabled
            .iter()
            .find(|s| s.book_source_url == *tok)
            .or_else(|| {
                tok.parse::<usize>()
                    .ok()
                    .filter(|n| *n >= 1)
                    .and_then(|n| enabled.get(n - 1))
            });
        match found {
            Some(s) => {
                if !selected.iter().any(|x| x.book_source_url == s.book_source_url) {
                    selected.push(s.clone());
                }
            }
            None => ignored.push(tok.clone()),
        }
    }
    (selected, ignored)
}

async fn search_start(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<SearchQ>,
) -> AppResult<Json<serde_json::Value>> {
    let key = q.keyword();
    if key.is_empty() {
        return Err(AppError::BadRequest("key（或别名 q）不能为空".into()));
    }
    let mut sources = load_sources(&s.shared).await?;
    crate::routes::sources::sort_sources(&mut sources);
    let quarantined = source_ledger::quarantined_set(&s.shared)
        .await
        .unwrap_or_default();
    let enabled_all: Vec<crate::entities::BookSource> = sources
        .into_iter()
        .filter(|x| {
            x.enabled
                && crate::entities::is_supported_source_type(x.book_source_type)
                && x.rule_search.is_some()
                && x.search_url.is_some()
                && !quarantined.contains(&x.book_source_url)
        })
        .collect();
    let requested = parse_source_ids(q.source_ids.as_deref());
    let (selected, ignored) = select_sources(&enabled_all, &requested);
    let include_cooled = truthy(q.include_cooled.as_deref());
    let health = source_health::load_map(&s.shared).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "load source health failed; search without cooldown filter");
        std::collections::HashMap::new()
    });
    let now = source_health::now_ms();
    let cooldown_total = source_health::cooled_count(&enabled_all, &health, now);
    let partition = source_health::partition_cooldown(selected, &health, include_cooled, now);
    let skipped_cooldown = partition.skipped;
    let enabled = partition.active;
    let source_total = enabled.len();
    let id = next_job_id();
    let job = Arc::new(SearchJob {
        id: id.clone(),
        key: key.clone(),
        at: Instant::now(),
        created_unix_ms: unix_ms(),
        sources: enabled.clone(),
        source_total,
        skipped_cooldown,
        cooldown_total,
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
            known_jobs = g.len(),
            "create search job"
        );
    }
    tokio::spawn(run_search_job(
        job,
        enabled,
        key,
        s.shared.clone(),
        s.cfg.source_cooldown_secs,
        crate::source_quality::TierConfig::from_config(&s.cfg),
    ));
    Ok(Json(json!({
        "job_id": id,
        "source_total": source_total,
        "deadline_ms": crate::req::search_budget().as_millis() as u64,
        // 契约变更：显式回显过滤结果，便于前端提示“哪些 id 被忽略”
        "source_ids_requested": requested,
        "ignored_source_ids": ignored,
        "warning": (!ignored.is_empty())
            .then(|| format!("已忽略无效书源 id：{}", ignored.join(", "))),
        // 冷却：默认跳过冷却中的源；include_cooled=1 时全部参与
        "include_cooled": include_cooled,
        "skipped_cooldown": skipped_cooldown,
        "cooldown_total": cooldown_total,
    })))
}

async fn search_result(
    State(_s): State<AppState>,
    _u: AuthUser,
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
    let (full, errors, done, finished, pending) = job_view(&job);

    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, 200);
    let total = full.len();
    let skip = ((page as usize) - 1) * page_size;
    let books: Vec<SearchBook> = full.into_iter().skip(skip).take(page_size).collect();
    let source_failed = errors.len();
    Ok(Json(json!({
        "books": books,
        "errors": errors,
        "page": page,
        "page_size": page_size,
        "total": total,
        "has_more": skip + page_size < total,
        "done": done,
        "finished": finished,
        "pending": pending,
        "source_total": job.source_total,
        "source_failed": source_failed,
        "deadline_ms": crate::req::search_budget().as_millis() as u64,
        // 冷却统计随 job 固化，轮询期间不会变化
        "skipped_cooldown": job.skipped_cooldown,
        "cooldown_total": job.cooldown_total,
    })))
}

async fn search(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<SearchQ>,
) -> AppResult<Json<Vec<SearchBook>>> {
    let sources = load_sources(&s.shared).await?;
    let page = q.page.unwrap_or(1).max(1);
    let include_cooled = truthy(q.include_cooled.as_deref());
    let (active, _, _) = searchable_without_cooled(&s.shared, &sources, include_cooled).await;
    let (full, _) = aggregated(&active, &q.keyword(), include_cooled).await;
    Ok(Json(paginate(full, page, q.page_size)))
}

/// 取一次完整聚合结果（带 60s 缓存），供 `/api/search` 与 `/api/search/aggregate` 共用，
/// 保证两个入口的分页都基于同一份结果，不重不漏。
async fn aggregated(
    sources: &[crate::entities::BookSource],
    key: &str,
    include_cooled: bool,
) -> (Vec<SearchBook>, Vec<web_book::SourceFailure>) {
    // 冷却过滤会改变参与源集合，缓存键必须区分两种模式，避免 60s 内串用。
    let cache_key = if include_cooled {
        format!("{key}\u{1f}cooled")
    } else {
        key.to_string()
    };
    let hit = {
        let mut g = agg_cache().lock().unwrap();
        prune_cache(&mut g);
        g.iter()
            .find(|e| e.key == cache_key)
            .map(|e| (e.books.clone(), e.errors.clone()))
    };
    if let Some(v) = hit {
        return v;
    }
    let outcome = web_book::search_all_with_errors(sources, key, 1).await;
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

/// 过滤出可参与搜索的源，并按健康状态剔除冷却中的源。
async fn searchable_without_cooled(
    pool: &SqlitePool,
    sources: &[crate::entities::BookSource],
    include_cooled: bool,
) -> (Vec<crate::entities::BookSource>, usize, usize) {
    let quarantined = source_ledger::quarantined_set(pool).await.unwrap_or_default();
    let eligible: Vec<crate::entities::BookSource> = sources
        .iter()
        .filter(|x| {
            x.enabled
                && crate::entities::is_supported_source_type(x.book_source_type)
                && x.rule_search.is_some()
                && x.search_url.is_some()
                && !quarantined.contains(&x.book_source_url)
        })
        .cloned()
        .collect();
    let health = source_health::load_map(pool).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "load source health failed; search without cooldown filter");
        std::collections::HashMap::new()
    });
    let now = source_health::now_ms();
    let cooldown_total = source_health::cooled_count(&eligible, &health, now);
    let partition = source_health::partition_cooldown(eligible, &health, include_cooled, now);
    (partition.active, partition.skipped, cooldown_total)
}

/// 聚合搜索明细：书籍 + 成功/失败书源统计（单源失败不影响整体返回）。
/// 分页基于「一次完整聚合结果」的缓存，保证跨页不重复、不遗漏，且 `total` 可信。
async fn search_aggregate(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<SearchQ>,
) -> AppResult<Json<serde_json::Value>> {
    let sources = load_sources(&s.shared).await?;
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, 200);
    let include_cooled = truthy(q.include_cooled.as_deref());
    let (active, skipped_cooldown, cooldown_total) =
        searchable_without_cooled(&s.shared, &sources, include_cooled).await;
    let (full, errors) = aggregated(&active, &q.keyword(), include_cooled).await;

    let total = full.len();
    let skip = ((page as usize) - 1) * page_size;
    let books: Vec<SearchBook> = full.into_iter().skip(skip).take(page_size).collect();
    let source_failed = errors.len();
    Ok(Json(json!({
        "books": books,
        "errors": errors,
        "page": page,
        "page_size": page_size,
        "total": total,
        "has_more": skip + page_size < total,
        "source_failed": source_failed,
        "source_total": active.len(),
        "include_cooled": include_cooled,
        "skipped_cooldown": skipped_cooldown,
        "cooldown_total": cooldown_total,
        "cache_ttl_secs": AGG_TTL_SECS,
    })))
}

fn paginate(all: Vec<SearchBook>, page: i32, page_size: Option<usize>) -> Vec<SearchBook> {
    let size = page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, 200);
    let skip = (page as usize - 1) * size;
    all.into_iter().skip(skip).take(size).collect()
}

/// 发现：前端传的是书源 ID（`Discover` 页的 select），展开该源 `exploreUrl` 的第一个分类；
/// 若传入的是 http(s) URL，则按同一分类 URL 并发查询所有发现类书源。
async fn explore(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<ExploreQ>,
) -> AppResult<Json<Vec<SearchBook>>> {
    let sources = load_sources(&s.shared).await?;
    let page = q.page.unwrap_or(1).max(1);
    if q.url.starts_with("http://") || q.url.starts_with("https://") {
        return Ok(Json(web_book::explore_all(&sources, &q.url).await));
    }
    let source = sources
        .iter()
        .find(|x| x.book_source_url == q.url)
        .ok_or_else(|| AppError::NotFound("source not found".into()))?;
    let cats = web_book::explore_categories(source, page)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    let url = cats
        .first()
        .map(|c| c.url.clone())
        .ok_or_else(|| AppError::BadRequest("source has no expandable exploreUrl".into()))?;
    Ok(Json(web_book::explore(source, &url).await?))
}

/// 发现分类列表：把书源 `exploreUrl` 展开为 `{title, url}`，供前端渲染分类导航。
async fn explore_categories(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<CategoryQ>,
) -> AppResult<Json<Vec<web_book::ExploreCategory>>> {
    let source = load_source(&s.shared, &q.source).await?;
    let cats = web_book::explore_categories(&source, q.page.unwrap_or(1).max(1))
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Json(cats))
}

async fn book_info(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<BookQ>,
) -> AppResult<Json<Book>> {
    let sources = load_sources(&s.shared).await?;
    let source = sources
        .into_iter()
        .find(|x| x.book_source_url == q.source)
        .ok_or_else(|| AppError::NotFound("source not found".into()))?;
    let mut book = Book {
        book_url: q.book_url.clone(),
        origin: Some(source.book_source_url.clone()),
        origin_name: Some(source.book_source_name.clone()),
        name: q.name.clone().filter(|s| !s.trim().is_empty()),
        author: q.author.clone().filter(|s| !s.trim().is_empty()),
        intro: q.intro.clone().filter(|s| !s.trim().is_empty()),
        cover_url: q.cover_url.clone().filter(|s| !s.trim().is_empty()),
        ..Default::default()
    };
    // 已有 book_meta（书架/上次详情）里的字段也作为兜底，规则为空时不丢
    if let Some(m) = load_book_meta(&db.0, &q.book_url).await? {
        book.name = book.name.or(m.name);
        book.author = book.author.or(m.author);
        book.intro = book.intro.or(m.intro);
        book.cover_url = book.cover_url.or(m.cover_url);
        book.kind = book.kind.or(m.kind);
        book.toc_url = book.toc_url.or(m.toc_url);
        // 详情接口要回显目录已写入的元信息，否则前端看到的 total_chapter_num 永远是 0
        book.total_chapter_num = m.total_chapter_num;
        book.word_count = book.word_count.or(m.word_count);
        book.latest_chapter_title = book.latest_chapter_title.or(m.latest_chapter_title);
        book.latest_chapter_time = m.latest_chapter_time;
    }
    web_book::book_info(&source, &mut book).await?;
    save_book_meta(&db.0, &book).await?;
    Ok(Json(book))
}

/// 目录：优先命中 `book_chapters` 的 TTL 缓存；过期或缺失时抓取并整本替换缓存。
/// `use_replace` 与 `/api/book/content` 同名：显式开关覆盖账号设置，缺省沿用
/// `use_replace_rule`；净化只作用于返回标题，缓存保留原文。
async fn toc(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<BookQ>,
) -> AppResult<Json<Vec<BookChapter>>> {
    let use_replace = resolve_use_replace(q.use_replace.as_deref(), &db.0).await;
    let mut chapters =
        if let Some(mut chaps) = load_cached_chapters(&db.0, &q.book_url, s.cfg.toc_ttl_secs).await?
        {
            // 历史书目录已在缓存里但 total_chapter_num 仍为 0：一次目录访问就地补齐，
            // 不必等缓存过期重抓。
            let _ = sync_total_chapter_num(&db.0, &q.book_url, chaps.len() as i64).await;
            chaps
        } else {
            let source = load_source(&s.shared, &q.source).await?;
            let mut book = Book {
                book_url: q.book_url.clone(),
                ..Default::default()
            };
            if let Some(m) = load_book_meta(&db.0, &q.book_url).await? {
                book.toc_url = m.toc_url;
            }
            let chapters = web_book::chapter_list(&source, &book).await?;
            save_chapters(&db.0, &q.book_url, &chapters).await?;
            // 目录落库时同步总章节数，供进度百分比与书架完成度使用
            sync_total_chapter_num(&db.0, &q.book_url, chapters.len() as i64).await?;
            // 记录目录总数变化（首次建基线不出角标）
            crate::routes::bookshelf::record_toc_delta(&db.0, &q.book_url, chapters.len() as i64)
                .await;
            // 已缓存过正文的章节顺手回填字数，避免 toc 阶段只能给出 0
            let _ = backfill_word_counts(&db.0, &q.book_url).await;
            chapters
        };
    if use_replace && !chapters.is_empty() {
        let book_name = book_name_of(&db.0, &q.book_url).await.unwrap_or_default();
        let mut cleaned: Vec<String> = chapters
            .iter()
            .map(|c| c.title.clone().unwrap_or_default())
            .collect();
        replace::purify_titles(&db.0, &book_name, &q.source, &mut cleaned).await;
        for (c, t) in chapters.iter_mut().zip(cleaned) {
            // 错误/空标题保持原样，避免把 None 变成空串
            if c.title.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false) {
                c.title = Some(t);
            }
        }
    }
    enrich_volumes(&mut chapters);
    Ok(Json(chapters))
}

/// 分卷分组：分卷行用自己的标题，普通章节继承最近的前置分卷标题。
pub(crate) fn enrich_volumes(chapters: &mut [BookChapter]) {
    let mut current: Option<String> = None;
    for c in chapters.iter_mut() {
        if c.is_volume {
            current = c.title.clone();
            c.volume_title = c.title.clone();
        } else {
            c.volume_title = current.clone();
        }
    }
}

/// 回写总章节数。只在 total > 0 时写，避免空目录把已有值清零；
/// upsert 用 `book_meta` 自身值兜底，凡是详情/书架等路径都不可能再把它降成 0。
async fn sync_total_chapter_num(
    pool: &SqlitePool,
    book_url: &str,
    total: i64,
) -> anyhow::Result<()> {
    if total <= 0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO book_meta (book_url, total_chapter_num) VALUES (?1, ?2) \
         ON CONFLICT(book_url) DO UPDATE SET total_chapter_num = excluded.total_chapter_num",
    )
    .bind(book_url)
    .bind(total)
    .execute(pool)
    .await?;
    Ok(())
}

/// 目录写入后，用已缓存正文的长度回填章节字数（SQLite `length()` 对 TEXT 按字符计数）。
async fn backfill_word_counts(pool: &SqlitePool, book_url: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE book_chapters SET word_count = COALESCE((\
           SELECT length(trim(content)) FROM book_content \
           WHERE book_content.url = book_chapters.url AND content IS NOT NULL\
         ), 0) WHERE book_url = ?1",
    )
    .bind(book_url)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------- 批量刷新目录 ----------------

#[derive(Deserialize)]
pub struct TocBatchQ {
    /// 书籍 URL 列表；也接受 Legado 风格的 book_ids
    #[serde(default, alias = "book_ids")]
    pub book_urls: Vec<String>,
    /// 可选统一书源；缺省按每本书的 book_meta.origin
    pub source: Option<String>,
}

pub(crate) const TOC_BATCH_MAX: usize = 100;
pub(crate) const TOC_BATCH_CONCURRENCY: usize = 3;

#[derive(Default)]
struct TocBatchOutcome {
    book_url: String,
    status: &'static str,
    chapters: usize,
    error: Option<String>,
    /// 刷新前章节总数
    before_total: i64,
    /// 刷新后章节总数
    after_total: i64,
    /// 新增章节数（按章节 URL 比对）
    added: i64,
    /// 删除章节数（按章节 URL 比对）
    removed: i64,
}

impl TocBatchOutcome {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "book_url": self.book_url,
            "status": self.status,
            "chapters": self.chapters,
            "before_total": self.before_total,
            "after_total": self.after_total,
            "added": self.added,
            "removed": self.removed,
            "total_delta": self.after_total - self.before_total,
            "error": self.error,
        })
    }
}

/// 批量重新抓目录并写库：逐本独立，单本失败不影响其他；并发 3。
/// 抓取沿用全局 `req` 限制（单请求 5 MiB / 15 s）。
async fn toc_batch(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<TocBatchQ>,
) -> AppResult<Json<serde_json::Value>> {
    let started = Instant::now();
    let source_override = q
        .source
        .clone()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let mut outcomes: Vec<TocBatchOutcome> = Vec::new();
    let mut urls: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for raw in q.book_urls {
        let u = raw.trim().to_string();
        if u.is_empty() {
            continue;
        }
        if !seen.insert(u.clone()) {
            outcomes.push(TocBatchOutcome {
                book_url: u,
                status: "skipped",
                chapters: 0,
                error: Some("重复的 book_url".into()),
                ..Default::default()
            });
            continue;
        }
        urls.push(u);
    }
    if urls.is_empty() {
        return Err(AppError::BadRequest("book_urls 不能为空".into()));
    }
    if urls.len() > TOC_BATCH_MAX {
        return Err(AppError::BadRequest(format!(
            "单次最多 {TOC_BATCH_MAX} 本，请分批调用"
        )));
    }

    let pool = db.0.clone();
    let shared = s.shared.clone();
    let done: Vec<TocBatchOutcome> = stream::iter(urls.into_iter().map(|book_url| {
        let pool = pool.clone();
        let shared = shared.clone();
        let source_override = source_override.clone();
        async move {
            let meta = match load_book_meta(&pool, &book_url).await {
                Ok(m) => m,
                Err(e) => {
                    return TocBatchOutcome {
                        book_url,
                        status: "failed",
                        chapters: 0,
                        error: Some(format!("读取书籍元信息失败：{e}")),
                        ..Default::default()
                    }
                }
            };
            // 书架上标记为"停用更新"的书不参与批量刷新（手动点单本目录仍可刷新）
            let shelf_flag: Option<i64> =
                sqlx::query_scalar("SELECT enable_update FROM bookshelf WHERE book_id = ?1")
                    .bind(&book_url)
                    .fetch_optional(&pool)
                    .await
                    .unwrap_or(None);
            if matches!(shelf_flag, Some(0)) {
                return TocBatchOutcome {
                    book_url,
                    status: "skipped",
                    chapters: 0,
                    error: Some("已停用更新".into()),
                    ..Default::default()
                };
            }
            let source_id = match source_override
                .clone()
                .or_else(|| meta.as_ref().and_then(|m| m.origin.clone()))
                .filter(|s| !s.trim().is_empty())
            {
                Some(s) => s,
                None => {
                    return TocBatchOutcome {
                        book_url,
                        status: "skipped",
                        chapters: 0,
                        error: Some("缺少书源，且 book_meta 未记录 origin".into()),
                        ..Default::default()
                    }
                }
            };
            let source = match load_source(&shared, &source_id).await {
                Ok(s) => s,
                Err(_) => {
                    return TocBatchOutcome {
                        book_url,
                        status: "skipped",
                        chapters: 0,
                        error: Some(format!("书源不存在：{source_id}")),
                        ..Default::default()
                    }
                }
            };
            let mut book = Book {
                book_url: book_url.clone(),
                ..Default::default()
            };
            if let Some(m) = meta {
                book.toc_url = m.toc_url;
            }
            let chapters = match web_book::chapter_list(&source, &book).await {
                Ok(c) => c,
                Err(e) => {
                    return TocBatchOutcome {
                        book_url,
                        status: "failed",
                        chapters: 0,
                        error: Some(format!("抓取目录失败：{e}")),
                        ..Default::default()
                    }
                }
            };
            if chapters.is_empty() {
                return TocBatchOutcome {
                    book_url,
                    status: "failed",
                    chapters: 0,
                    error: Some("目录为空".into()),
                    ..Default::default()
                };
            }
            // 保存前记录旧目录，用于计算新增/删除章节（总数变化之外的可核对增量）
            let before_total: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM book_chapters WHERE book_url = ?1")
                    .bind(&book_url)
                    .fetch_optional(&pool)
                    .await
                    .unwrap_or(None)
                    .unwrap_or(0);
            let old_urls: HashSet<String> =
                sqlx::query_scalar("SELECT url FROM book_chapters WHERE book_url = ?1")
                    .bind(&book_url)
                    .fetch_all(&pool)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
            if let Err(e) = save_chapters(&pool, &book_url, &chapters).await {
                return TocBatchOutcome {
                    book_url,
                    status: "failed",
                    chapters: 0,
                    error: Some(format!("写入目录失败：{e}")),
                    before_total,
                    ..Default::default()
                };
            }
            let _ = sync_total_chapter_num(&pool, &book_url, chapters.len() as i64).await;
            let _ = backfill_word_counts(&pool, &book_url).await;
            let after_total = chapters.len() as i64;
            let new_urls: HashSet<&str> = chapters.iter().map(|c| c.url.as_str()).collect();
            let added = new_urls
                .iter()
                .filter(|u| !old_urls.contains(**u))
                .count() as i64;
            let removed = old_urls
                .iter()
                .filter(|u| !new_urls.contains(u.as_str()))
                .count() as i64;
            crate::routes::bookshelf::record_toc_delta(&pool, &book_url, after_total).await;
            TocBatchOutcome {
                book_url,
                status: "updated",
                chapters: chapters.len(),
                error: None,
                before_total,
                after_total,
                added,
                removed,
            }
        }
    }))
    .buffer_unordered(TOC_BATCH_CONCURRENCY)
    .collect()
    .await;
    outcomes.extend(done);
    let updated = outcomes.iter().filter(|o| o.status == "updated").count();
    let failed = outcomes.iter().filter(|o| o.status == "failed").count();
    let skipped = outcomes.iter().filter(|o| o.status == "skipped").count();
    Ok(Json(json!({
        "ok": true,
        "total": outcomes.len(),
        "updated": updated,
        "failed": failed,
        "skipped": skipped,
        "results": outcomes.iter().map(|o| o.to_json()).collect::<Vec<_>>(),
        "elapsed_ms": started.elapsed().as_millis() as u64,
    })))
}

/// 正文：命中缓存（TTL 内）直接返回；否则抓取、落库、按总量上限淘汰最久未读的章节，
/// 最后统一应用用户替换规则（标题/正文分别受 scope_title / scope_content 控制）。
///
/// 契约变更（向后兼容）：新增 `content_type`（text|image）、`images`（图片源正文）、
/// `truncated`（单章图片超过上限被截断）与 `placeholder_suspected`（付费/会员章节疑似占位图）。
/// 文本源仍返回原 `content` 且 `images: []`；图片源 `content` 为空串 + `images` 列表。
async fn content(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<ContentQ>,
) -> AppResult<Json<serde_json::Value>> {
    // 分卷标题是抓取层合成的伪 URL（`volume:<book_url>#<index>`），没有正文可抓；
    // 提前返回业务化错误，避免 reqwest 报出看不懂的 `builder error`。
    if q.chapter_url.starts_with("volume:") {
        return Err(AppError::BadRequest(
            "该条为分卷标题，请从目录选择章节".into(),
        ));
    }
    let mut c = match load_cached_content(&db.0, &q.chapter_url, s.cfg.content_ttl_secs).await? {
        Some(c) => c,
        None => {
            let source = load_source(&s.shared, &q.source).await?;
            let chapter = BookChapter {
                url: q.chapter_url.clone(),
                ..Default::default()
            };
            let c = if source.book_source_type == 2 {
                // 漫画源：只抽图片列表，content 置空以保持字段类型不变
                let imgs = web_book::chapter_images(&source, &chapter).await?;
                BookContent {
                    book_url: chapter.book_url.clone().unwrap_or_default(),
                    url: chapter.url.clone(),
                    title: chapter.title.clone(),
                    content: Some(String::new()),
                    chapter_index: chapter.index,
                    content_type: "image".into(),
                    images_json: Some(serde_json::to_string(&imgs)?),
                    ..Default::default()
                }
            } else {
                web_book::chapter_content(&source, &chapter).await?
            };
            // 用户设置的缓存上限优先（且不超过全局上限），未设置时用全局配置
            let limit = match replace::user_cache_limit_mb(&db.0).await {
                Some(mb) => (mb * 1024 * 1024).min(s.cfg.cache_limit_bytes()),
                None => s.cfg.cache_limit_bytes(),
            };
            save_content_cached(&db.0, &c, limit).await?;
            c
        }
    };
    if c.content_type.trim().is_empty() {
        c.content_type = "text".into();
    }
    let is_image = c.content_type == "image";
    // 验收 P2：content 只带 chapter_url 时 book_url/title 为空，导致按书名圈定的替换规则失效、
    // 响应字段缺失。这里用请求参数兜底，并在净化前补齐 book_url。
    if c.book_url.trim().is_empty() {
        if let Some(bu) = q.book_url.as_deref().filter(|s| !s.trim().is_empty()) {
            c.book_url = bu.to_string();
        }
    }
    // 缓存书可能没存 book_url：用章节 URL 反查 book_chapters（其主键就是书籍 URL）。
    if c.book_url.trim().is_empty() {
        let found: Option<String> = sqlx::query_scalar(
            "SELECT book_url FROM book_chapters WHERE url = ?1 AND book_url <> '' LIMIT 1",
        )
        .bind(&q.chapter_url)
        .fetch_optional(&db.0)
        .await
        .ok()
        .flatten();
        if let Some(bu) = found {
            c.book_url = bu;
        }
    }
    if c.title.is_none() {
        c.title = q.title.clone().filter(|s| !s.trim().is_empty());
    }
    if c.title.is_none() {
        c.title = sqlx::query_scalar("SELECT title FROM book_chapters WHERE url = ?1 LIMIT 1")
            .bind(&q.chapter_url)
            .fetch_optional(&db.0)
            .await
            .ok()
            .flatten()
            .filter(|s: &String| !s.trim().is_empty());
    }
    // 仍拿不到书籍地址时给明确错误码，而不是让前端猜。
    if c.book_url.trim().is_empty() {
        return Err(AppError::MissingBookUrl);
    }
    // 替换规则开关：请求参数可显式覆盖，缺省沿用账号设置
    let use_replace = resolve_use_replace(q.use_replace.as_deref(), &db.0).await;
    if is_image {
        // 图片章节不做正文净化/图片样式注入，避免把 <img> 列表当富文本改写
        let (images, truncated) = decode_cached_images(c.images_json.as_deref());
        let (is_pay, is_vip): (bool, bool) =
            sqlx::query_as("SELECT is_pay, is_vip FROM book_chapters WHERE url = ?1 LIMIT 1")
                .bind(&q.chapter_url)
                .fetch_optional(&db.0)
                .await
                .ok()
                .flatten()
                .unwrap_or((false, false));
        let placeholder_suspected = !images.is_empty() && (is_pay || is_vip);
        let prefetch = images.len().min(3);
        return Ok(Json(json!({
            "book_url": c.book_url,
            "url": c.url,
            "title": c.title,
            "content": c.content.unwrap_or_default(),
            "chapter_index": c.chapter_index,
            "content_type": "image",
            "images": images,
            "truncated": truncated,
            "placeholder_suspected": placeholder_suspected,
            "replace_applied": false,
            // 前端可按 priority 分批：前 3 张为 high，其余 normal
            "prefetch_hint": {"count": prefetch, "priority": "high"},
        })));
    }
    let book_name = book_name_of(&db.0, &c.book_url)
        .await
        .unwrap_or_default();
    if use_replace {
        let (title, body) = replace::purify(
            &db.0,
            &book_name,
            &q.source,
            c.title.clone(),
            c.content.clone(),
        )
        .await;
        c.title = title;
        c.content = body;
    }
    let style = replace::user_image_style(&db.0).await;
    if let Some(b) = c.content.take() {
        c.content = Some(replace::apply_image_style(&b, style));
    }
    Ok(Json(json!({
        "book_url": c.book_url,
        "url": c.url,
        "title": c.title,
        "content": c.content,
        "chapter_index": c.chapter_index,
        "content_type": "text",
        "images": [],
        "truncated": false,
        "placeholder_suspected": false,
        "replace_applied": use_replace,
    })))
}

/// 解析图片缓存 JSON；兼容旧的裸数组形态，缺失/损坏时返回空列表。
fn decode_cached_images(raw: Option<&str>) -> (Vec<crate::entities::BookImage>, bool) {
    let Some(raw) = raw else {
        return (Vec::new(), false);
    };
    if let Ok(v) = serde_json::from_str::<web_book::ChapterImages>(raw) {
        return (v.images, v.truncated);
    }
    if let Ok(list) = serde_json::from_str::<Vec<crate::entities::BookImage>>(raw) {
        return (list, false);
    }
    (Vec::new(), false)
}

// ---------------- 书内全文搜索（仅已缓存章节） ----------------

#[derive(Deserialize)]
pub struct BookSearchQ {
    pub book_id: String,
    pub q: String,
    pub limit: Option<usize>,
}

const SEARCH_TIMEOUT_SECS: u64 = 5;
const SEARCH_MAX_QUERY_CHARS: usize = 200;

/// 转义 LIKE 的 `%`/`_`/`\`，让用户输入按字面量匹配。
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// 找到首个命中并生成上下文片段。返回 `(snippet, 字符偏移, 本章命中次数)`。
/// SQL 的 LIKE 对 ASCII 大小写不敏感，这里做同样的回退查找。
fn find_snippet(content: &str, q: &str) -> Option<(String, usize, usize)> {
    let lower = content.to_lowercase();
    let ql = q.to_lowercase();
    // 命中次数按 LIKE 语义做 ASCII 大小写不敏感统计
    let count = lower.matches(&ql).count();
    let byte = if let Some(idx) = content.find(q) {
        idx
    } else {
        let idx = lower.find(&ql)?;
        let char_idx = lower[..idx].chars().count();
        content
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(0)
    };
    let char_off = content[..byte].chars().count();
    let total = content.chars().count();
    let qlen = q.chars().count();
    let start = char_off.saturating_sub(40);
    let end = (char_off + qlen + 40).min(total);
    let mut snippet: String = content.chars().skip(start).take(end - start).collect();
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < total {
        snippet.push('…');
    }
    Some((snippet, char_off, count))
}

#[derive(sqlx::FromRow)]
struct ContentSearchRow {
    url: String,
    title: Option<String>,
    chapter_index: i64,
    content: Option<String>,
}

async fn book_search(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<BookSearchQ>,
) -> AppResult<Json<serde_json::Value>> {
    let book_id = q.book_id.trim();
    if book_id.is_empty() {
        return Err(AppError::BadRequest("book_id 不能为空".into()));
    }
    let query = q.q.trim();
    if query.is_empty() {
        return Err(AppError::BadRequest("q 不能为空".into()));
    }
    if query.chars().count() > SEARCH_MAX_QUERY_CHARS {
        return Err(AppError::BadRequest(format!(
            "q 最多 {SEARCH_MAX_QUERY_CHARS} 字"
        )));
    }
    let limit = q.limit.unwrap_or(20).clamp(1, 50);
    let started = Instant::now();
    let pattern = escape_like(query);
    let rows_fut = sqlx::query_as::<_, ContentSearchRow>(
        "SELECT url, title, chapter_index, content FROM book_content \
         WHERE book_url = ?1 AND content IS NOT NULL \
           AND content LIKE '%' || ?2 || '%' ESCAPE '\\' \
         ORDER BY chapter_index LIMIT ?3",
    )
    .bind(book_id)
    .bind(&pattern)
    .bind((limit + 1) as i64)
    .fetch_all(&db.0);
    let rows = tokio::time::timeout(Duration::from_secs(SEARCH_TIMEOUT_SECS), rows_fut)
        .await
        .map_err(|_| {
            AppError::Timeout(format!(
                "书内搜索超过 {SEARCH_TIMEOUT_SECS}s 预算，请缩短关键词"
            ))
        })??;
    let truncated = rows.len() > limit;
    let hits: Vec<serde_json::Value> = rows
        .into_iter()
        .take(limit)
        .filter_map(|row| {
            let content = row.content?;
            let (snippet, offset, match_count) = find_snippet(&content, query)?;
            Some(json!({
                "chapter_index": row.chapter_index,
                "chapter_url": row.url,
                "chapter_title": row.title,
                "match_count": match_count,
                "offset": offset,
                "snippet": snippet,
            }))
        })
        .collect();

    // 明确告诉调用方“只搜了已缓存部分”，避免把部分结果当成全书结果。
    let (total, cached): (i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM book_chapters WHERE book_url = ?1), \
           (SELECT COUNT(*) FROM book_content WHERE book_url = ?1 AND content IS NOT NULL)",
    )
    .bind(book_id)
    .fetch_one(&db.0)
    .await?;
    let uncached = (total - cached).max(0);
    // 目录未缓存时无法判断“全书是否搜全”，此时不能报告 complete=true。
    let complete = uncached == 0 && total > 0;
    let note = if total == 0 {
        "目录未缓存，无法判断是否覆盖全书；仅返回已缓存正文中的命中"
    } else if uncached == 0 {
        "已覆盖全部缓存章节"
    } else {
        "仅搜索已缓存章节，未缓存章节未参与检索"
    };
    Ok(Json(json!({
        "ok": true,
        "book_id": book_id,
        "query": query,
        "limit": limit,
        "truncated": truncated,
        "hits": hits,
        "total_chapters": total,
        "cached_chapters": cached,
        "uncached_chapters": uncached,
        "complete": complete,
        "note": note,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    })))
}

// ---------------- 整本缓存 / 缓存清理 ----------------

#[derive(Deserialize)]
pub struct CacheAllQ {
    #[serde(alias = "book_id")]
    pub book_url: String,
    pub source: Option<String>,
    /// 本次最多抓取的章节数（默认 200，上限 500），防止一次请求跑太久
    pub limit: Option<usize>,
}

#[derive(Deserialize, Default)]
pub struct CacheClearQ {
    /// 缺省表示清空该用户的全部正文缓存
    #[serde(default, alias = "book_url", alias = "book_id")]
    pub book_id: Option<String>,
}

/// 原子预占预算：成功则扣减并返回 true；不足返回 false。
fn reserve_budget(remaining: &AtomicI64, size: i64) -> bool {
    let mut cur = remaining.load(Ordering::SeqCst);
    loop {
        if size > cur {
            return false;
        }
        match remaining.compare_exchange(cur, cur - size, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(v) => cur = v,
        }
    }
}

enum CacheOutcome {
    Cached { index: i64, bytes: i64 },
    Failed {
        index: i64,
        url: String,
        title: Option<String>,
        error: String,
    },
    Budget { index: i64 },
}

async fn cache_all(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<CacheAllQ>,
) -> AppResult<Json<serde_json::Value>> {
    let started = Instant::now();
    let book_url = q.book_url.trim().to_string();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    let chapters: Vec<BookChapter> = sqlx::query_as(
        "SELECT url, title, is_volume, base_url, book_url, \"index\", is_vip, is_pay, \
         resource_url, tag, start, \"end\", update_time, word_count FROM book_chapters \
         WHERE book_url = ?1 ORDER BY \"index\"",
    )
    .bind(&book_url)
    .fetch_all(&db.0)
    .await?;
    if chapters.is_empty() {
        return Err(AppError::BadRequest(
            "目录未缓存，请先调用 GET /api/book/toc".into(),
        ));
    }
    let source_id = match q.source.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => {
            let origin: Option<String> =
                sqlx::query_scalar("SELECT origin FROM book_meta WHERE book_url = ?1")
                    .bind(&book_url)
                    .fetch_optional(&db.0)
                    .await?;
            origin
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| {
                    AppError::BadRequest("缺少 source，且 book_meta 未记录书源".into())
                })?
        }
    };
    let source = load_source(&s.shared, &source_id).await?;
    let cached_urls: Vec<String> = sqlx::query_scalar(
        "SELECT url FROM book_content WHERE book_url = ?1 AND content IS NOT NULL",
    )
    .bind(&book_url)
    .fetch_all(&db.0)
    .await?;
    let cached_set: HashSet<String> = cached_urls.into_iter().collect();
    let total_chapters = chapters.len();
    let mut pending: Vec<BookChapter> = chapters
        .into_iter()
        .filter(|c| !c.is_volume && !cached_set.contains(&c.url))
        .collect();
    let already_cached = total_chapters - pending.len();
    let max_chapters = q.limit.unwrap_or(200).clamp(1, 500);
    let limited = pending.len() > max_chapters;
    pending.truncate(max_chapters);

    let limit_bytes = match replace::user_cache_limit_mb(&db.0).await {
        Some(mb) => (mb * 1024 * 1024).min(s.cfg.cache_limit_bytes()),
        None => s.cfg.cache_limit_bytes(),
    };
    let used_before: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(size_bytes),0) FROM book_content",
    )
    .fetch_one(&db.0)
    .await?;
    let attempted = pending.len();
    let remaining = Arc::new(AtomicI64::new((limit_bytes - used_before).max(0)));
    let exhausted = Arc::new(AtomicBool::new(false));
    let pool = db.0.clone();
    let src = source.clone();
    let bu = book_url.clone();

    let outcomes: Vec<CacheOutcome> = stream::iter(pending.into_iter().map(|ch| {
        let pool = pool.clone();
        let src = src.clone();
        let bu = bu.clone();
        let remaining = remaining.clone();
        let exhausted = exhausted.clone();
        async move {
            if exhausted.load(Ordering::SeqCst) {
                return CacheOutcome::Budget { index: ch.index };
            }
            let mut chapter = ch.clone();
            if chapter.book_url.as_deref().unwrap_or("").trim().is_empty() {
                chapter.book_url = Some(bu.clone());
            }
            let idx = chapter.index;
            let url = chapter.url.clone();
            let title = chapter.title.clone();
            match web_book::chapter_content(&src, &chapter).await {
                Ok(mut c) => {
                    if c.book_url.trim().is_empty() {
                        c.book_url = bu.clone();
                    }
                    if c.url.trim().is_empty() {
                        c.url = url.clone();
                    }
                    c.chapter_index = idx;
                    let size = c.content.as_deref().map(|s| s.len()).unwrap_or(0) as i64;
                    if !reserve_budget(&remaining, size) {
                        exhausted.store(true, Ordering::SeqCst);
                        return CacheOutcome::Budget { index: idx };
                    }
                    match upsert_content(&pool, &c).await {
                        Ok(()) => CacheOutcome::Cached {
                            index: idx,
                            bytes: size,
                        },
                        Err(e) => CacheOutcome::Failed {
                            index: idx,
                            url,
                            title,
                            error: format!("{e}"),
                        },
                    }
                }
                Err(e) => CacheOutcome::Failed {
                    index: idx,
                    url,
                    title,
                    error: format!("{e}"),
                },
            }
        }
    }))
    .buffer_unordered(3)
    .collect()
    .await;

    let mut cached = 0usize;
    let mut budget_skipped = 0usize;
    let mut cached_bytes = 0i64;
    let mut failures: Vec<serde_json::Value> = Vec::new();
    for o in outcomes {
        match o {
            CacheOutcome::Cached { bytes, .. } => {
                cached += 1;
                cached_bytes += bytes;
            }
            CacheOutcome::Budget { .. } => budget_skipped += 1,
            CacheOutcome::Failed {
                index,
                url,
                title,
                error,
            } => failures.push(json!({
                "chapter_index": index,
                "chapter_url": url,
                "chapter_title": title,
                "error": error,
            })),
        }
    }
    failures.sort_by_key(|v| v["chapter_index"].as_i64().unwrap_or(0));
    Ok(Json(json!({
        "ok": true,
        "book_url": book_url,
        "source": source_id,
        "total_chapters": total_chapters,
        "already_cached": already_cached,
        "attempted": attempted,
        "cached": cached,
        "failed": failures.len(),
        "skipped_budget": budget_skipped,
        "limited": limited,
        "limit": max_chapters,
        "budget_bytes": limit_bytes,
        "used_bytes_before": used_before,
        "used_bytes_after": used_before + cached_bytes,
        "failures": failures,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    })))
}

/// 清空正文缓存：`book_id` 有值按书清，缺省清该用户全部。返回 (删除行数, 释放字节数)。
pub(crate) async fn clear_content_cache(
    pool: &SqlitePool,
    book_id: Option<&str>,
) -> anyhow::Result<(u64, i64)> {
    let scope = book_id.map(str::trim).filter(|s| !s.is_empty());
    let (deleted, freed) = match scope {
        Some(id) => {
            let freed: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(size_bytes),0) FROM book_content WHERE book_url = ?1",
            )
            .bind(id)
            .fetch_one(pool)
            .await?;
            let deleted = sqlx::query("DELETE FROM book_content WHERE book_url = ?1")
                .bind(id)
                .execute(pool)
                .await?
                .rows_affected();
            (deleted, freed)
        }
        None => {
            let freed: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(size_bytes),0) FROM book_content",
            )
            .fetch_one(pool)
            .await?;
            let deleted = sqlx::query("DELETE FROM book_content")
                .execute(pool)
                .await?
                .rows_affected();
            (deleted, freed)
        }
    };
    // 有界归还空闲页，避免文件只涨不落。
    let _ = crate::db::reclaim_pages(pool, 1024).await;
    Ok((deleted, freed))
}

async fn cache_clear(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<CacheClearQ>,
) -> AppResult<Json<serde_json::Value>> {
    let scope = q
        .book_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (deleted, freed) = clear_content_cache(&db.0, scope).await?;
    Ok(Json(json!({
        "ok": true,
        "scope": if scope.is_some() { "book" } else { "user" },
        "book_id": scope,
        "deleted": deleted,
        "bytes_freed": freed,
    })))
}

/// 换源：按书名+作者在其他书源里搜索，返回匹配项（不自动切换，由前端选择）。
async fn switch_source(
    State(s): State<AppState>,
    _u: AuthUser,
    Json(q): Json<SwitchQ>,
) -> AppResult<Json<Vec<SearchBook>>> {
    let sources = load_sources(&s.shared).await?;
    let all = web_book::search_all(&sources, &q.name, 1).await;
    let name = q.name.trim().to_lowercase();
    let author = q.author.as_deref().unwrap_or("").trim().to_lowercase();
    let candidates = all.into_iter().filter(|b| {
        let bn = b.name.as_deref().unwrap_or("").trim().to_lowercase();
        let ba = b.author.as_deref().unwrap_or("").trim().to_lowercase();
        let name_ok = bn == name || bn.contains(&name);
        let author_ok = author.is_empty() || ba == author;
        name_ok && author_ok
    });
    let out: Vec<SearchBook> = match q.exclude_source.as_deref() {
        Some(ex) => candidates.filter(|b| b.origin.as_deref() != Some(ex)).collect(),
        None => candidates.collect(),
    };
    Ok(Json(out))
}

// ---------------- 缓存落库辅助 ----------------

pub(crate) async fn save_book_meta(pool: &SqlitePool, b: &Book) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO book_meta \
         (book_url, toc_url, origin, origin_name, name, author, kind, cover_url, intro, charset, \
          book_source_type, \"group\", latest_chapter_title, latest_chapter_time, total_chapter_num, \
          dur_chapter_title, dur_chapter_index, dur_chapter_pos, dur_chapter_time, word_count, \
          can_update, \"order\", origin_order, variable, custom_tag, custom_cover_url, custom_intro) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27) \
         ON CONFLICT(book_url) DO UPDATE SET \
          toc_url=excluded.toc_url, origin=excluded.origin, origin_name=excluded.origin_name, \
          name=excluded.name, author=excluded.author, kind=excluded.kind, cover_url=excluded.cover_url, \
          intro=excluded.intro, charset=excluded.charset, book_source_type=excluded.book_source_type, \
          \"group\"=excluded.\"group\", latest_chapter_title=excluded.latest_chapter_title, \
          latest_chapter_time=excluded.latest_chapter_time, \
          total_chapter_num=CASE WHEN excluded.total_chapter_num > 0 THEN excluded.total_chapter_num \
                                 ELSE book_meta.total_chapter_num END, \
          dur_chapter_title=excluded.dur_chapter_title, dur_chapter_index=excluded.dur_chapter_index, \
          dur_chapter_pos=excluded.dur_chapter_pos, dur_chapter_time=excluded.dur_chapter_time, \
          word_count=excluded.word_count, can_update=excluded.can_update, \"order\"=excluded.\"order\", \
          origin_order=excluded.origin_order, variable=excluded.variable, custom_tag=excluded.custom_tag, \
          custom_cover_url=excluded.custom_cover_url, custom_intro=excluded.custom_intro",
    )
    .bind(&b.book_url)
    .bind(&b.toc_url)
    .bind(&b.origin)
    .bind(&b.origin_name)
    .bind(&b.name)
    .bind(&b.author)
    .bind(&b.kind)
    .bind(&b.cover_url)
    .bind(&b.intro)
    .bind(&b.charset)
    .bind(b.book_source_type)
    .bind(&b.group)
    .bind(&b.latest_chapter_title)
    .bind(b.latest_chapter_time)
    .bind(b.total_chapter_num)
    .bind(&b.dur_chapter_title)
    .bind(b.dur_chapter_index)
    .bind(b.dur_chapter_pos)
    .bind(b.dur_chapter_time)
    .bind(&b.word_count)
    .bind(b.can_update)
    .bind(b.order)
    .bind(b.origin_order)
    .bind(&b.variable)
    .bind(&b.custom_tag)
    .bind(&b.custom_cover_url)
    .bind(&b.custom_intro)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn load_book_meta(pool: &SqlitePool, book_url: &str) -> anyhow::Result<Option<Book>> {
    let b: Option<Book> = sqlx::query_as::<_, Book>("SELECT * FROM book_meta WHERE book_url = ?")
        .bind(book_url)
        .fetch_optional(pool)
        .await?;
    Ok(b)
}

/// 目录缓存读取：TTL 内存在任意章节则整体命中（一次 SELECT 避免 N 次查询）。
pub(crate) async fn load_cached_chapters(
    pool: &SqlitePool,
    book_url: &str,
    ttl_secs: i64,
) -> anyhow::Result<Option<Vec<BookChapter>>> {
    let fresh: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM book_chapters \
         WHERE book_url = ?1 AND updated_at >= datetime('now', ?2)",
    )
    .bind(book_url)
    .bind(format!("-{ttl_secs} seconds"))
    .fetch_one(pool)
    .await?;
    if fresh == 0 {
        return Ok(None);
    }
    let rows: Vec<BookChapter> = sqlx::query_as(
        "SELECT url, title, is_volume, base_url, book_url, \"index\", is_vip, is_pay, \
         resource_url, tag, start, \"end\", update_time, word_count FROM book_chapters \
         WHERE book_url = ?1 ORDER BY \"index\"",
    )
    .bind(book_url)
    .fetch_all(pool)
    .await?;
    Ok(Some(rows))
}

/// 目录整本刷新：先清旧章节再写入，避免目录缩短后残留脏数据（写入放大优先让位于内存峰值）。
pub(crate) async fn save_chapters(
    pool: &SqlitePool,
    book_url: &str,
    chapters: &[BookChapter],
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM book_chapters WHERE book_url = ?")
        .bind(book_url)
        .execute(pool)
        .await?;
    for c in chapters {
        sqlx::query(
            "INSERT INTO book_chapters \
             (url, book_url, title, is_volume, base_url, \"index\", is_vip, is_pay, resource_url, tag, start, \"end\", update_time, word_count, updated_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14, datetime('now')) \
             ON CONFLICT(url) DO UPDATE SET title=excluded.title, is_volume=excluded.is_volume, \
              base_url=excluded.base_url, \"index\"=excluded.\"index\", is_vip=excluded.is_vip, \
              is_pay=excluded.is_pay, resource_url=excluded.resource_url, tag=excluded.tag, \
              start=excluded.start, \"end\"=excluded.\"end\", update_time=excluded.update_time, \
              word_count=excluded.word_count, updated_at=datetime('now')",
        )
        .bind(&c.url)
        .bind(&c.book_url)
        .bind(&c.title)
        .bind(c.is_volume)
        .bind(&c.base_url)
        .bind(c.index)
        .bind(c.is_vip)
        .bind(c.is_pay)
        .bind(&c.resource_url)
        .bind(&c.tag)
        .bind(c.start)
        .bind(c.end)
        .bind(&c.update_time)
        .bind(c.word_count)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// 正文缓存读取：TTL 内命中则更新 accessed_at（LRU 依据）。
pub(crate) async fn load_cached_content(
    pool: &SqlitePool,
    url: &str,
    ttl_secs: i64,
) -> anyhow::Result<Option<BookContent>> {
    let row: Option<BookContent> = sqlx::query_as(
        "SELECT url, book_url, title, content, chapter_index, resource_url, is_local, \
         content_type, images \
         FROM book_content WHERE url = ?1 AND updated_at >= datetime('now', ?2)",
    )
    .bind(url)
    .bind(format!("-{ttl_secs} seconds"))
    .fetch_optional(pool)
    .await?;
    if row.is_some() {
        let _ = sqlx::query("UPDATE book_content SET accessed_at = datetime('now') WHERE url = ?")
            .bind(url)
            .execute(pool)
            .await;
    }
    Ok(row)
}

/// 单条正文 upsert（不做淘汰），供 `/api/book/content` 与整本缓存共用。
pub(crate) async fn upsert_content(pool: &SqlitePool, c: &BookContent) -> anyhow::Result<()> {
    let content_type = if c.content_type.trim().is_empty() {
        "text"
    } else {
        c.content_type.as_str()
    };
    let size = (c.content.as_deref().map(|s| s.len()).unwrap_or(0)
        + c.images_json.as_deref().map(|s| s.len()).unwrap_or(0)) as i64;
    sqlx::query(
        "INSERT INTO book_content \
         (url, book_url, title, content, chapter_index, resource_url, is_local, \
          content_type, images, size_bytes, updated_at, accessed_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10, datetime('now'), datetime('now')) \
         ON CONFLICT(url) DO UPDATE SET title=excluded.title, content=excluded.content, \
          chapter_index=excluded.chapter_index, resource_url=excluded.resource_url, \
          is_local=excluded.is_local, content_type=excluded.content_type, \
          images=excluded.images, size_bytes=excluded.size_bytes, \
          updated_at=datetime('now'), accessed_at=datetime('now')",
    )
    .bind(&c.url)
    .bind(&c.book_url)
    .bind(&c.title)
    .bind(&c.content)
    .bind(c.chapter_index)
    .bind(&c.resource_url)
    .bind(c.is_local)
    .bind(content_type)
    .bind(&c.images_json)
    .bind(size)
    .execute(pool)
    .await?;
    // 正文字数回填到目录行（未知时保持 0）；SQLite TEXT 的 length() 按字符计数。
    let chars = c
        .content
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .count() as i64;
    let _ = sqlx::query("UPDATE book_chapters SET word_count = ?2 WHERE url = ?1")
        .bind(&c.url)
        .bind(chars)
        .execute(pool)
        .await;
    Ok(())
}

/// 正文落库 + 总量上限淘汰：超过 `limit_bytes` 时按 accessed_at 升序删除最久未读章节。
pub(crate) async fn save_content_cached(
    pool: &SqlitePool,
    c: &BookContent,
    limit_bytes: i64,
) -> anyhow::Result<()> {
    upsert_content(pool, c).await?;

    // 淘汰：每轮删除最久未读的一批，最多 8 轮，避免极端情况下长时间占用连接
    for _ in 0..8 {
        let total: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(size_bytes),0) FROM book_content",
        )
        .fetch_one(pool)
        .await?;
        if total <= limit_bytes {
            break;
        }
        let deleted = sqlx::query(
            "DELETE FROM book_content WHERE url IN \
             (SELECT url FROM book_content ORDER BY accessed_at ASC LIMIT 16)",
        )
        .execute(pool)
        .await?
        .rows_affected();
        if deleted == 0 {
            break;
        }
        tracing::info!(total, limit = limit_bytes, deleted, "content cache evicted");
        // 增量归还有界空间（256 页 ≈ 1 MiB），避免文件只涨不落；非增量库为空操作。
        let _ = crate::db::reclaim_pages(pool, 256).await;
    }
    Ok(())
}

/// 取书籍名称（用于替换规则的 scope 匹配）
async fn book_name_of(pool: &SqlitePool, book_url: &str) -> anyhow::Result<String> {
    let name: Option<String> =
        sqlx::query_scalar("SELECT name FROM book_meta WHERE book_url = ?")
            .bind(book_url)
            .fetch_optional(pool)
            .await?;
    Ok(name.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    fn mk_content(url: &str, book: &str, idx: i64, body: &str) -> BookContent {
        BookContent {
            book_url: book.into(),
            url: url.into(),
            title: Some(format!("第{idx}章")),
            content: Some(body.into()),
            chapter_index: idx,
            resource_url: None,
            is_local: false,
            ..Default::default()
        }
    }

    async fn mem_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE book_content (url TEXT PRIMARY KEY, book_url TEXT NOT NULL, \
             title TEXT, content TEXT, chapter_index INTEGER NOT NULL DEFAULT 0, \
             resource_url TEXT, is_local INTEGER NOT NULL DEFAULT 0, \
             content_type TEXT NOT NULL DEFAULT 'text', images TEXT, \
             size_bytes INTEGER NOT NULL DEFAULT 0, updated_at TEXT, accessed_at TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    // 预算预占：并发下必须原子扣减，且耗尽后不再放行
    #[test]
    fn budget_reservation_is_atomic_and_bounded() {
        let remaining = AtomicI64::new(100);
        assert!(reserve_budget(&remaining, 0));
        assert!(reserve_budget(&remaining, 40));
        assert!(reserve_budget(&remaining, 60));
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
        assert!(!reserve_budget(&remaining, 1));
        // 单章超过总预算也直接拒绝，不出现负数
        let small = AtomicI64::new(10);
        assert!(!reserve_budget(&small, 11));
        assert_eq!(small.load(Ordering::SeqCst), 10);
    }

    // LIKE 通配符必须按字面量匹配，避免用户输入 `%`/`_` 把全库搜出来
    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("c:\\x"), "c:\\\\x");
        assert_eq!(escape_like("普通文本"), "普通文本");
    }

    // 片段：命中位置按字符计、前后 40 字上下文、命中次数正确（含 ASCII 大小写回退）
    #[test]
    fn snippet_has_context_offset_and_count() {
        let body = format!("{}关键词{}关键词{}", "前".repeat(100), "中".repeat(50), "后".repeat(100));
        let (snippet, offset, count) = find_snippet(&body, "关键词").unwrap();
        assert_eq!(count, 2);
        assert_eq!(offset, 100, "首个命中应位于第 100 个字符");
        assert!(snippet.starts_with('…'), "左侧被截断应有省略号");
        assert!(snippet.ends_with('…'), "右侧被截断应有省略号");
        assert!(snippet.contains("关键词"));

        let ascii = "Hello World, hello rust";
        let (_, offset2, count2) = find_snippet(ascii, "hello").unwrap();
        assert_eq!(offset2, 13, "小写回退也应返回真实字符偏移");
        assert_eq!(count2, 2, "大小写不敏感统计应命中 2 次");
        assert!(find_snippet("无关内容", "关键词").is_none());
    }

    // 目录/正文共用：use_replace 参数只接受显式布尔，未知值回落到账号设置（None）
    #[test]
    fn parse_use_replace_accepts_explicit_switch_only() {
        assert_eq!(parse_use_replace(Some("1")), Some(true));
        assert_eq!(parse_use_replace(Some(" TRUE ")), Some(true));
        assert_eq!(parse_use_replace(Some("on")), Some(true));
        assert_eq!(parse_use_replace(Some("0")), Some(false));
        assert_eq!(parse_use_replace(Some("false")), Some(false));
        assert_eq!(parse_use_replace(Some("off")), Some(false));
        assert_eq!(parse_use_replace(None), None);
        assert_eq!(parse_use_replace(Some("")), None);
        assert_eq!(parse_use_replace(Some("yes")), None);
    }

    // 整本缓存写入路径：同一 URL 二次写入只保留一行，size_bytes 随正文更新
    #[tokio::test]
    async fn upsert_content_keeps_single_row_and_updates_size() {
        let pool = mem_pool().await;
        let c = mk_content("https://x/1", "https://x/book", 1, "短正文");
        upsert_content(&pool, &c).await.unwrap();
        let c2 = mk_content("https://x/1", "https://x/book", 1, "更长的正文内容");
        upsert_content(&pool, &c2).await.unwrap();
        let (rows, size): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes),0) FROM book_content",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(size, "更长的正文内容".len() as i64);
        let title: String =
            sqlx::query_scalar("SELECT title FROM book_content WHERE url = 'https://x/1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(title, "第1章");
    }

    async fn meta_pool() -> SqlitePool {
        let pool = mem_pool().await;
        sqlx::query(
            "CREATE TABLE book_meta (book_url TEXT PRIMARY KEY, toc_url TEXT, origin TEXT, \
             origin_name TEXT, name TEXT, author TEXT, kind TEXT, cover_url TEXT, intro TEXT, \
             charset TEXT, book_source_type INTEGER, \"group\" INTEGER, latest_chapter_title TEXT, \
             latest_chapter_time INTEGER, total_chapter_num INTEGER NOT NULL DEFAULT 0, \
             dur_chapter_title TEXT, dur_chapter_index INTEGER, dur_chapter_pos INTEGER, \
             dur_chapter_time INTEGER, word_count INTEGER, can_update INTEGER, \
             \"order\" INTEGER NOT NULL DEFAULT 0, origin_order INTEGER NOT NULL DEFAULT 0, \
             variable TEXT, custom_tag TEXT, custom_cover_url TEXT, custom_intro TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    // total_chapter_num：目录回写后，详情保存（Book 默认 0）不得把它清零；空目录也不得清零
    #[tokio::test]
    async fn total_chapter_num_survives_detail_save_and_empty_refresh() {
        let pool = meta_pool().await;
        let url = "https://x/book";
        sync_total_chapter_num(&pool, url, 500).await.unwrap();

        let detail = Book {
            book_url: url.into(),
            name: Some("斗破苍穹".into()),
            ..Default::default()
        };
        save_book_meta(&pool, &detail).await.unwrap();
        let total: i64 = sqlx::query_scalar(
            "SELECT total_chapter_num FROM book_meta WHERE book_url = ?1",
        )
        .bind(url)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(total, 500, "详情保存不应把总章节数写回 0");

        sync_total_chapter_num(&pool, url, 0).await.unwrap();
        let total: i64 =
            sqlx::query_scalar("SELECT total_chapter_num FROM book_meta WHERE book_url = ?1")
                .bind(url)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(total, 500, "空目录不应清掉已有总章节数");

        sync_total_chapter_num(&pool, url, 520).await.unwrap();
        let total: i64 =
            sqlx::query_scalar("SELECT total_chapter_num FROM book_meta WHERE book_url = ?1")
                .bind(url)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(total, 520, "目录刷新应更新为最新章节数");
    }

    async fn chapters_pool() -> SqlitePool {
        let pool = mem_pool().await;
        sqlx::query(
            "CREATE TABLE book_chapters (url TEXT PRIMARY KEY, book_url TEXT NOT NULL, \
             title TEXT, is_volume INTEGER NOT NULL DEFAULT 0, base_url TEXT, \
             \"index\" INTEGER NOT NULL DEFAULT 0, is_vip INTEGER NOT NULL DEFAULT 0, \
             is_pay INTEGER NOT NULL DEFAULT 0, resource_url TEXT, tag TEXT, \
             start INTEGER NOT NULL DEFAULT 0, \"end\" INTEGER NOT NULL DEFAULT 0, \
             update_time TEXT, word_count INTEGER NOT NULL DEFAULT 0, updated_at TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    // 目录元信息往返：is_volume / is_vip / update_time / word_count 落库并读回；分卷分组正确
    #[tokio::test]
    async fn chapter_meta_roundtrip_and_volume_grouping() {
        let pool = chapters_pool().await;
        let book = "https://x/book";
        let chapters = vec![
            BookChapter {
                url: "volume:https://x/book#0".into(),
                title: Some("第一卷 风起".into()),
                book_url: Some(book.into()),
                is_volume: true,
                index: 0,
                update_time: Some("2026-01-01 10:00".into()),
                ..Default::default()
            },
            BookChapter {
                url: "https://x/c1".into(),
                title: Some("第一章".into()),
                book_url: Some(book.into()),
                is_vip: true,
                is_pay: true,
                index: 1,
                update_time: Some("2026-01-02".into()),
                ..Default::default()
            },
            BookChapter {
                url: "https://x/c2".into(),
                title: Some("第二章".into()),
                book_url: Some(book.into()),
                index: 2,
                ..Default::default()
            },
        ];
        save_chapters(&pool, book, &chapters).await.unwrap();
        let mut loaded = load_cached_chapters(&pool, book, 0).await.unwrap().unwrap();
        assert_eq!(loaded.len(), 3);
        assert!(loaded[0].is_volume);
        assert_eq!(loaded[0].update_time.as_deref(), Some("2026-01-01 10:00"));
        assert!(loaded[1].is_vip);
        assert!(loaded[1].is_pay, "is_pay 应随目录落库并读回");
        assert!(!loaded[2].is_pay);
        assert_eq!(loaded[2].word_count, 0);

        enrich_volumes(&mut loaded);
        assert_eq!(loaded[0].volume_title.as_deref(), Some("第一卷 风起"));
        assert_eq!(loaded[1].volume_title.as_deref(), Some("第一卷 风起"));
        assert_eq!(loaded[2].volume_title.as_deref(), Some("第一卷 风起"));

        // 正文落库回填字数；第 2 章在下一个分卷下
        upsert_content(&pool, &mk_content("https://x/c1", book, 1, "第一章正文内容"))
            .await
            .unwrap();
        let reloaded = load_cached_chapters(&pool, book, 0).await.unwrap().unwrap();
        assert_eq!(reloaded[1].word_count, "第一章正文内容".chars().count() as i64);
    }

    // 清理：按书只删该书；不传 book_id 清空全部；bytes_freed 为被删行的 size_bytes 之和
    #[tokio::test]
    async fn clear_cache_by_book_and_by_user() {
        let pool = mem_pool().await;
        let a = mk_content("https://x/a1", "https://x/a", 1, "aaa");
        let b = mk_content("https://x/a2", "https://x/a", 2, "bbbb");
        let other = mk_content("https://x/b1", "https://x/b", 1, "ccccc");
        for c in [&a, &b, &other] {
            upsert_content(&pool, c).await.unwrap();
        }
        let (deleted, freed) = clear_content_cache(&pool, Some("https://x/a"))
            .await
            .unwrap();
        assert_eq!(deleted, 2);
        assert_eq!(freed, 7);
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_content")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 1, "另一本书的缓存不应被清掉");

        let (deleted_all, freed_all) = clear_content_cache(&pool, None).await.unwrap();
        assert_eq!(deleted_all, 1);
        assert_eq!(freed_all, 5);
        let left2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_content")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left2, 0);
    }

    // 两段式搜索的读视图：已完成源先可读、去重按权重、done 由 pending 决定
    #[test]
    fn job_view_dedupes_by_weight_and_reports_progress() {
        let mut hi = crate::entities::BookSource::default();
        hi.book_source_url = "http://hi".into();
        hi.weight = 10;
        let mut lo = crate::entities::BookSource::default();
        lo.book_source_url = "http://lo".into();
        lo.weight = 0;

        let mut a = SearchBook::default();
        a.book_url = "http://hi/1".into();
        a.origin = Some("http://hi".into());
        a.name = Some("剑来".into());
        a.author = Some("烽火戏诸侯".into());
        let mut b = SearchBook::default();
        b.book_url = "http://lo/1".into();
        b.origin = Some("http://lo".into());
        b.name = Some("剑来".into());
        b.author = Some("烽火戏诸侯".into());

        let job = SearchJob {
            id: "j-test".into(),
            key: "剑来".into(),
            at: Instant::now(),
            created_unix_ms: 0,
            sources: vec![hi, lo],
            source_total: 2,
            skipped_cooldown: 0,
            cooldown_total: 0,
            state: Mutex::new(SearchJobState {
                books: vec![a, b],
                errors: vec![],
                finished: 1,
                pending: 1,
            }),
        };
        let (books, errors, done, finished, pending) = job_view(&job);
        assert_eq!(books.len(), 1, "同名同作者应去重为一条");
        assert_eq!(
            books[0].origin.as_deref(),
            Some("http://hi"),
            "权重高的源应排在前面"
        );
        assert!(errors.is_empty());
        assert_eq!((finished, pending), (1, 1));
        assert!(!done, "还有源未完成时 done 必须为 false");

        job.state.lock().unwrap().pending = 0;
        let (_, _, done, _, pending) = job_view(&job);
        assert!(done);
        assert_eq!(pending, 0);
    }

    fn test_job(id: &str, age_secs: u64, pending: usize) -> Arc<SearchJob> {
        Arc::new(SearchJob {
            id: id.into(),
            key: "k".into(),
            at: Instant::now()
                .checked_sub(Duration::from_secs(age_secs))
                .unwrap(),
            created_unix_ms: 0,
            sources: Vec::new(),
            source_total: pending,
            skipped_cooldown: 0,
            cooldown_total: 0,
            state: Mutex::new(SearchJobState {
                pending,
                ..Default::default()
            }),
        })
    }

    /// 已完成的旧任务超过上限时，只淘汰已完成的最旧任务；进行中的新 job 必须保留。
    #[test]
    fn prune_jobs_evicts_completed_but_keeps_active() {
        let mut list: Vec<Arc<SearchJob>> = (0..SEARCH_JOB_MAX)
            .map(|i| test_job(&format!("done{i}"), (SEARCH_JOB_MAX - i) as u64, 0))
            .collect();
        // 新 job 仍在进行中（pending=5）
        let active = test_job("active", 0, 5);
        list.push(active.clone());

        prune_jobs(&mut list);

        assert!(list.len() <= SEARCH_JOB_MAX, "应回到已完成任务上限");
        assert_eq!(list.iter().filter(|j| j.id.starts_with("done")).count(), SEARCH_JOB_MAX - 1);
        assert!(
            list.iter().any(|j| j.id == active.id),
            "进行中的 job 不能被完成后淘汰误杀"
        );
        assert!(
            !list.iter().any(|j| j.id == "done0"),
            "应淘汰最旧的已完成任务"
        );
    }

    /// 并发 4~64 个进行中的 job 时，一个都不应被淘汰（旧实现按总数 8 直接误杀）。
    #[test]
    fn prune_jobs_keeps_all_active_under_hard_cap() {
        let mut list: Vec<Arc<SearchJob>> = (0..SEARCH_JOB_MAX_ACTIVE - 1)
            .map(|i| test_job(&format!("run{i}"), 10, 3))
            .collect();
        let fresh = test_job("fresh", 0, 3);
        list.push(fresh.clone());

        prune_jobs(&mut list);

        assert_eq!(list.len(), SEARCH_JOB_MAX_ACTIVE, "硬上限内全部保留");
        assert!(list.iter().any(|j| j.id == fresh.id));
    }

    /// 超过硬上限才淘汰最旧的活跃任务（兜底防内存膨胀）。
    #[test]
    fn prune_jobs_evicts_active_only_beyond_hard_cap() {
        let mut list: Vec<Arc<SearchJob>> = (0..SEARCH_JOB_MAX_ACTIVE + 1)
            .map(|i| test_job(&format!("run{i}"), if i == 0 { 11 } else { 10 }, 2))
            .collect();
        prune_jobs(&mut list);
        assert_eq!(list.len(), SEARCH_JOB_MAX_ACTIVE);
        assert!(!list.iter().any(|j| j.id == "run0"), "最旧的活跃任务被硬上限淘汰");
    }

    fn src(name: &str, url: &str, weight: i64, enabled: bool) -> crate::entities::BookSource {
        crate::entities::BookSource {
            book_source_url: url.into(),
            book_source_name: name.into(),
            weight,
            enabled,
            search_url: Some("https://x/s?q=".into()),
            rule_search: Some(Default::default()),
            ..Default::default()
        }
    }

    /// `q` 是 `key` 的别名；两者都在时以 `key` 为准，缺两者为空（由 handler 返回 400）。
    #[test]
    fn search_keyword_accepts_q_alias() {
        let only_q = SearchQ {
            key: None,
            q: Some(" 剑来 ".into()),
            source_ids: None,
            include_cooled: None,
            page: None,
            page_size: None,
        };
        assert_eq!(only_q.keyword(), "剑来");

        let both = SearchQ {
            key: Some("斗破".into()),
            q: Some("剑来".into()),
            source_ids: None,
            include_cooled: None,
            page: None,
            page_size: None,
        };
        assert_eq!(both.keyword(), "斗破");

        let none = SearchQ {
            key: None,
            q: None,
            source_ids: None,
            include_cooled: None,
            page: None,
            page_size: None,
        };
        assert_eq!(none.keyword(), "");
    }

    #[test]
    fn search_source_ids_filter_and_report_ignored() {
        // 排序：权重降序 → custom_order 升序 → 名称
        let mut all = vec![
            src("乙", "http://b", 1, true),
            src("甲", "http://a", 2, true),
            src("丙", "http://c", 0, false),
        ];
        crate::routes::sources::sort_sources(&mut all);
        let enabled: Vec<_> = all
            .into_iter()
            .filter(|x| {
            x.enabled
                && crate::entities::is_supported_source_type(x.book_source_type)
                && x.rule_search.is_some()
                && x.search_url.is_some()
        })
            .collect();
        assert_eq!(enabled[0].book_source_url, "http://a");

        // 数字按排序后的 1-based 序号：1,2 → 前两个启用源
        let (sel, ign) = select_sources(&enabled, &parse_source_ids(Some("1,2")));
        assert_eq!(sel.len(), 2);
        assert_eq!(sel[0].book_source_url, "http://a");
        assert_eq!(sel[1].book_source_url, "http://b");
        assert!(ign.is_empty());

        // 精确 URL id 优先；重复项去重；禁用源与不存在的 id 进入 ignored
        let (sel, ign) = select_sources(
            &enabled,
            &parse_source_ids(Some("http://b,http://b,http://c,999,abc")),
        );
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].book_source_url, "http://b");
        assert_eq!(
            ign,
            vec!["http://c".to_string(), "999".to_string(), "abc".to_string()]
        );

        // 未传 source_ids 时保持原有“全部启用源”语义
        let (sel, ign) = select_sources(&enabled, &parse_source_ids(None));
        assert_eq!(sel.len(), 2);
        assert!(ign.is_empty());
    }
}
