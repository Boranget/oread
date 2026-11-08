//! source_bench 的抽样、报告结构与任务注册（move-only 从 source_bench.rs 拆出）。

use super::*;

// ---------------- 抽样 ----------------

/// 按 type 分组、内置优先、category 轮转抽 `limit` 条。
/// `salt` 用于每次体检轮换起点，保证长期覆盖到所有 category。
pub fn pick_samples(
    entries: &[CorpusEntry],
    source_type: i64,
    limit: usize,
    salt: usize,
) -> Vec<CorpusEntry> {
    let usable: Vec<&CorpusEntry> = entries
        .iter()
        .filter(|e| e.enabled && e.source_type == source_type)
        .collect();
    let builtin: Vec<&CorpusEntry> = usable.iter().copied().filter(|e| e.builtin).collect();
    let user: Vec<&CorpusEntry> = usable.iter().copied().filter(|e| !e.builtin).collect();
    if limit == 0 {
        return Vec::new();
    }
    let mut out: Vec<CorpusEntry> = Vec::with_capacity(limit);
    // 用户条目至少留 1 个抽样位：否则内置一多，用户新增的书名永远进不了下一次体检
    // （验收要求“新增书名必须出现在下批抽样里”）。内置仍占绝对多数，覆盖不被挤占。
    let user_reserved = usize::from(!user.is_empty() && limit >= 2);
    let builtin_slots = limit.saturating_sub(user_reserved);
    round_robin(&builtin, builtin_slots, salt, &mut out);
    if !user.is_empty() {
        let mut rotated = user.clone();
        let offset = salt % rotated.len();
        rotated.rotate_left(offset);
        for e in rotated {
            if out.len() >= limit {
                break;
            }
            out.push(e.clone());
        }
    }
    if out.len() < limit {
        round_robin(&builtin, limit - out.len(), salt, &mut out);
    }
    out
}

pub(super) fn round_robin(
    items: &[&CorpusEntry],
    limit: usize,
    salt: usize,
    out: &mut Vec<CorpusEntry>,
) {
    if items.is_empty() || limit == 0 {
        return;
    }
    let mut cats: Vec<String> = Vec::new();
    for e in items {
        if !cats.contains(&e.category) {
            cats.push(e.category.clone());
        }
    }
    if cats.is_empty() {
        return;
    }
    let mut buckets: HashMap<&str, Vec<&CorpusEntry>> = HashMap::new();
    for e in items {
        buckets.entry(e.category.as_str()).or_default().push(e);
    }
    // salt 同时参与“类间顺序”和“类内起点”：
    // 旧实现只旋转 category 顺序、类内永远从第 0 条开始，limit=5 且每个 category
    // 有 3 条时，24 条语料实际永远只跑到 10 条（小说 5 条 + 漫画 5 条）。
    // 2026-11 实测：147 源体检后 DISTINCT corpus_key 只有 10。
    let mut idx: HashMap<&str, usize> = HashMap::new();
    let mut placed = 0usize;
    let mut guard = 0usize;
    while placed < limit && guard < items.len() * 2 + 8 {
        guard += 1;
        let mut any = false;
        let offset = salt % cats.len();
        for step in 0..cats.len() {
            if placed >= limit {
                break;
            }
            let cat_pos = (offset + step) % cats.len();
            let cat = &cats[cat_pos];
            let list = buckets.get(cat.as_str()).cloned().unwrap_or_default();
            if list.is_empty() {
                continue;
            }
            let pos = idx.entry(cat.as_str()).or_insert(0);
            if *pos < list.len() {
                let start = (salt + cat_pos) % list.len();
                out.push(list[(start + *pos) % list.len()].clone());
                *pos += 1;
                placed += 1;
                any = true;
            }
        }
        if !any {
            break;
        }
    }
}

// ---------------- 体检执行 ----------------

#[derive(Debug, Clone)]
pub struct BenchRun {
    pub batch_id: String,
    pub source_url: String,
    pub corpus_key: String,
    pub title: String,
    pub category: String,
    pub source_type: i64,
    pub builtin_sample: bool,
    pub found: bool,
    pub title_match: bool,
    pub author_match: bool,
    pub latency_ms: i64,
    pub error_kind: Option<String>,
    pub suspicious: bool,
    pub run_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryRate {
    pub category: String,
    pub samples: i64,
    pub hits: i64,
    pub hit_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceBenchRow {
    pub source_url: String,
    pub name: String,
    pub source_type: i64,
    pub samples: i64,
    pub hits: i64,
    pub hit_rate: f64,
    pub avg_latency_ms: i64,
    pub errors: i64,
    pub error_kinds: serde_json::Value,
    pub bench_score: i32,
    pub curated: bool,
    pub excluded_samples: i64,
    pub per_category_hit_rate: Vec<CategoryRate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuspiciousCorpus {
    pub corpus_key: String,
    pub title: String,
    pub category: String,
    pub hit_rate: f64,
    pub category_median: f64,
    pub observations: i64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CuratedItem {
    pub rank: usize,
    pub source_url: String,
    pub name: String,
    pub source_type: i64,
    pub bench_score: i32,
    pub samples: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut_gap: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchReport {
    pub batch_id: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub elapsed_ms: i64,
    pub interrupted: bool,
    pub corpus_total: usize,
    pub samples_per_source: usize,
    pub sources: Vec<SourceBenchRow>,
    pub novel_curated: Vec<CuratedItem>,
    pub comic_curated: Vec<CuratedItem>,
    pub suspicious: Vec<SuspiciousCorpus>,
    pub excluded_from_score: i64,
}

/// 异步体检进度：源总数 / 已完成源数（按源完成即 +1，不等整批结束）。
#[derive(Debug, Default)]
pub struct BenchProgress {
    pub(super) total: AtomicUsize,
    pub(super) done: AtomicUsize,
}

/// `GET /bench/run/{id}` 返回的任务视图。
#[derive(Debug, Clone, Serialize)]
pub struct BenchJobView {
    pub run_id: String,
    pub status: String,
    pub batch_id: Option<String>,
    pub sources_total: usize,
    pub sources_done: usize,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub elapsed_ms: Option<i64>,
    pub curated_total: Option<usize>,
    pub novel_total: Option<usize>,
    pub comic_total: Option<usize>,
    pub error: Option<String>,
}

pub(super) struct BenchJobEntry {
    pub(super) status: String,
    pub(super) batch_id: Option<String>,
    pub(super) progress: Arc<BenchProgress>,
    pub(super) started_at: i64,
    pub(super) finished_at: Option<i64>,
    pub(super) curated_total: Option<usize>,
    pub(super) novel_total: Option<usize>,
    pub(super) comic_total: Option<usize>,
    pub(super) error: Option<String>,
}

pub(super) static BENCH_JOBS: OnceLock<Mutex<HashMap<String, BenchJobEntry>>> = OnceLock::new();

pub(super) fn bench_jobs() -> &'static Mutex<HashMap<String, BenchJobEntry>> {
    BENCH_JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 启动一次后台体检，立即返回 run_id；进度用 `bench_job_view` 查询。
pub fn start_bench_job(
    pool: SqlitePool,
    cfg: Config,
    source_ids: Option<Vec<String>>,
    limit: Option<usize>,
) -> AppResult<String> {
    if !try_begin_bench() {
        return Err(AppError::BadRequest(
            "已有体检任务在运行，请稍后查询结果".into(),
        ));
    }
    let started_at = now_ms();
    let run_id = format!("br-{}-{}", started_at, next_seq());
    let progress = Arc::new(BenchProgress::default());
    bench_jobs()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(
            run_id.clone(),
            BenchJobEntry {
                status: "running".into(),
                batch_id: None,
                progress: progress.clone(),
                started_at,
                finished_at: None,
                curated_total: None,
                novel_total: None,
                comic_total: None,
                error: None,
            },
        );

    let job_id = run_id.clone();
    tokio::spawn(async move {
        let result = run_bench(
            &pool,
            &cfg,
            source_ids,
            limit,
            Some(progress),
            BENCH_CONCURRENCY,
        )
        .await;
        let finished_at = now_ms();
        let mut jobs = bench_jobs().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = jobs.get_mut(&job_id) {
            entry.finished_at = Some(finished_at);
            match result {
                Ok(report) => {
                    entry.status = "completed".into();
                    entry.batch_id = Some(report.batch_id);
                    entry.novel_total = Some(report.novel_curated.len());
                    entry.comic_total = Some(report.comic_curated.len());
                    entry.curated_total =
                        Some(report.novel_curated.len() + report.comic_curated.len());
                }
                Err(e) => {
                    entry.status = "failed".into();
                    entry.error = Some(e.to_string());
                }
            }
        }
        end_bench();
    });
    Ok(run_id)
}

/// 查询异步体检任务；未知 run_id 返回 None。
pub fn bench_job_view(run_id: &str) -> Option<BenchJobView> {
    let jobs = bench_jobs().lock().unwrap_or_else(|p| p.into_inner());
    let entry = jobs.get(run_id)?;
    let finished_at = entry.finished_at;
    Some(BenchJobView {
        run_id: run_id.to_string(),
        status: entry.status.clone(),
        batch_id: entry.batch_id.clone(),
        sources_total: entry.progress.total.load(Ordering::SeqCst),
        sources_done: entry.progress.done.load(Ordering::SeqCst),
        started_at: entry.started_at,
        finished_at,
        elapsed_ms: finished_at.map(|t| t - entry.started_at),
        curated_total: entry.curated_total,
        novel_total: entry.novel_total,
        comic_total: entry.comic_total,
        error: entry.error.clone(),
    })
}
