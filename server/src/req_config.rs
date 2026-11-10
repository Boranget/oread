//! req 的配置、预算与并发闸门（move-only 从 req.rs 拆出）。

use super::*;

/// 预览抓取最多跟随的跳数（每跳都重新做一次私网校验）
pub(super) const MAX_REDIRECTS: usize = 5;
/// 域名解析的独立预算，避免 DNS 卡住整个预览请求
pub(super) const DNS_TIMEOUT_SECS: u64 = 5;
/// 书源级超时（`timeout_millisecond`）的边界：不再做 1–60s 的人为夹取。
/// 只保留极小值保护（防止 1ms 把正常站点全部判死）与全局上限兜底（防止单源卡死整批），
/// 取值由 `parse_source_timeout_ms` 说明；站点声明的任意中间值原样生效。
pub const SOURCE_TIMEOUT_MIN_MS: i64 = 100;
pub const SOURCE_TIMEOUT_MAX_MS: i64 = 120_000;
/// `concurrentRate` 的间隔上限：防止异常来源写入超大间隔把源永久卡住。
pub(super) const SOURCE_RATE_MAX_INTERVAL_MS: u64 = 600_000;
/// 普通抓取跟随的最大跳数（与浏览器常见的 10 跳一致）。
/// reqwest 默认就是 limited(10)，这里显式写出来，避免以后改默认值造成 301/302 断链。
pub(super) const FOLLOW_REDIRECTS: usize = 10;
/// 默认 UA。品牌改为 Boranlib 后仍保留浏览器前缀：`Boranlib/0.1` 这类自报 UA
/// 会被部分站点直接判为爬虫并重置连接（快眼看书 botaodz / booksky 实测：
/// 浏览器 UA 200，自报 UA 被 RST），因此只在末尾追加产品标识。
pub(super) const DEFAULT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Boranlib/0.1";

/// Cookie 桶数量上限（按 bookSourceUrl 分桶）
pub(super) const COOKIE_MAX_BUCKETS: usize = 64;
/// 单桶 Cookie 字节上限（名字 + 值 + 分隔符）
pub(super) const COOKIE_BUCKET_MAX_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy)]
pub(super) struct HttpConf {
    pub(super) concurrency: usize,
    /// `scope=all` 的源级槽：min(用户配置,16)，未设置时 16。
    pub(super) all_concurrency: usize,
    pub(super) timeout_secs: u64,
    pub(super) search_timeout_secs: u64,
    /// `scope=all` 的单源预算（与 curated 解耦：all 覆盖上百个源，单源预算要短）。
    pub(super) search_all_timeout_secs: u64,
    /// 整批搜索总预算：curated / all 两档。
    pub(super) search_curated_budget_secs: u64,
    pub(super) search_all_budget_secs: u64,
    /// 书源未声明 `concurrentRate` 时的每源在飞上限。
    pub(super) source_concurrency_default: usize,
    pub(super) max_body_bytes: usize,
}

pub(super) const DEFAULT_CONF: HttpConf = HttpConf {
    concurrency: 16,
    all_concurrency: 16,
    timeout_secs: 15,
    search_timeout_secs: 8,
    search_all_timeout_secs: 8,
    search_curated_budget_secs: 6,
    search_all_budget_secs: 40,
    source_concurrency_default: SOURCE_CONCURRENCY_DEFAULT,
    max_body_bytes: 5 * 1024 * 1024,
};

/// 搜索范围：决定参与源集合与总预算。
///
/// - `All`（默认）：全量可搜索源。流水线调度后首条结果通常 0.5s 左右，
///   相关性过滤会挡住垃圾结果；源级并发默认 16 槽，总预算 40s、单源 8s。
/// - `Curated`（用户显式选择「精选」）：只跑体检精选源；curated 集合不存在时退化为全量，
///   总预算随 `All`；交集为空时退化为 preferred。显式 `source_ids` 会跳过该收缩。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchScope {
    Curated,
    All,
}

impl SearchScope {
    /// 宽松解析：缺省/未知值按 `all`（新默认）；只有显式 `curated` 才收紧。
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("curated" | "preferred" | "curated_only") => Self::Curated,
            _ => Self::All,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Curated => "curated",
            Self::All => "all",
        }
    }

    /// 整批总预算默认值（秒）。实际生效值见 `search_total_budget()`；本方法仅单测使用。
    #[allow(dead_code)]
    pub fn default_total_budget_secs(self) -> u64 {
        match self {
            Self::Curated => 6,
            Self::All => 40,
        }
    }
}

pub(super) static CONF: OnceCell<HttpConf> = OnceCell::new();
pub(super) static CLIENT: OnceCell<Client> = OnceCell::new();
pub(super) static SEM: OnceCell<Arc<Semaphore>> = OnceCell::new();
/// 聚合搜索的“源级并发槽”：拿到槽之后才开始计单源预算，
/// 排队时间不再吃掉单源预算，避免几百个源里排后面的源全部报超时。
pub(super) static SEARCH_NOVEL: OnceCell<Arc<Semaphore>> = OnceCell::new();
/// `scope=all` 的源级槽：设备实测 16 并发可行（4 核、VmHWM < 71 MB）。
pub(super) static SEARCH_NOVEL_ALL: OnceCell<Arc<Semaphore>> = OnceCell::new();
/// 漫画源专用小池（数量少、单源耗时长），保证不被小说源排队饿死。
pub(super) static SEARCH_COMIC: OnceCell<Arc<Semaphore>> = OnceCell::new();
pub(super) static SEARCH_COMIC_ALL: OnceCell<Arc<Semaphore>> = OnceCell::new();

/// 由 main 在启动时调用一次；未初始化时使用默认预算（单测/工具函数场景）。
pub fn init(cfg: &Config) {
    let _ = CONF.set(HttpConf {
        concurrency: cfg.http_concurrency,
        all_concurrency: cfg.search_all_slots,
        timeout_secs: cfg.http_timeout_secs,
        search_timeout_secs: cfg.search_timeout_secs,
        search_all_timeout_secs: cfg.search_all_timeout_secs,
        search_curated_budget_secs: cfg.search_curated_budget_secs,
        search_all_budget_secs: cfg.search_all_budget_secs,
        source_concurrency_default: cfg.source_concurrency_default,
        max_body_bytes: cfg.http_max_body_bytes,
    });
    let per_source = cfg.search_timeout_secs;
    let all_per_source = cfg.search_all_timeout_secs;
    let curated = cfg.search_curated_budget_secs;
    let all = cfg.search_all_budget_secs;
    // 启动日志：两个生效值必须一眼可见，避免又回到“两者同值”的隐式 bug。
    tracing::info!(
        target: "search_budget",
        per_source_secs = per_source,
        all_per_source_secs = all_per_source,
        total_curated_secs = curated,
        total_all_secs = all,
        source_slots = cfg.http_concurrency,
        all_source_slots = cfg.search_all_slots,
        source_concurrency_default = cfg.source_concurrency_default,
        "search budgets resolved"
    );
}

pub(super) fn conf() -> HttpConf {
    *CONF.get().unwrap_or(&DEFAULT_CONF)
}

/// 单源搜索预算：聚合搜索按“一个书源”整体计时，超时只丢该源。
pub fn search_budget() -> Duration {
    Duration::from_secs(conf().search_timeout_secs)
}

/// 按 scope 的单源预算：curated 用 `SEARCH_TIMEOUT_SECS`（默认 8s），
/// all 用 `SEARCH_ALL_TIMEOUT_SECS`（默认 5s）。
///
/// `all` 要覆盖上百个源：单源预算越长，16 个并发槽越容易被慢站占满，
/// 同预算下跑到的源越少（实测 8s→5s 覆盖 20 → 90 个源）。
pub fn search_budget_for(scope: SearchScope) -> Duration {
    let secs = match scope {
        SearchScope::Curated => conf().search_timeout_secs,
        SearchScope::All => conf().search_all_timeout_secs,
    };
    Duration::from_secs(secs)
}

/// 一次聚合搜索的整批时长上限（含排队）。与单源预算独立：
/// 按 scope 取已解析值（curated 默认 6s / all 默认 10s）。
pub fn search_total_budget(scope: SearchScope) -> Duration {
    let secs = match scope {
        SearchScope::Curated => conf().search_curated_budget_secs,
        SearchScope::All => conf().search_all_budget_secs,
    };
    Duration::from_secs(secs)
}

/// 解析书源级超时：空/0/负数 → `None`（调用方回退全局 `HTTP_TIMEOUT_SECS`）；
/// 否则夹取到 [`SOURCE_TIMEOUT_MIN_MS`(100ms), `SOURCE_TIMEOUT_MAX_MS`(120s)]。
/// 与 Legado 一致：源声明的 1–60s 之外的值（如 80s）也原样采用。
pub fn parse_source_timeout_ms(raw: i64) -> Option<Duration> {
    if raw <= 0 {
        return None;
    }
    let ms = raw.clamp(SOURCE_TIMEOUT_MIN_MS, SOURCE_TIMEOUT_MAX_MS);
    Some(Duration::from_millis(ms as u64))
}

/// 书源实体的单源超时；未设置时返回 `None`，由请求层回退全局默认。
pub fn source_timeout(source: &BookSource) -> Option<Duration> {
    parse_source_timeout_ms(source.timeout_millisecond)
}

/// 解析 Legado 兼容的 `concurrentRate`：只取整数部分（`"3/1"` → 3、`"2.5"` → 2），
/// 缺省/非法/0 → `default`；有效值夹取到 1–8。
///
/// Legado 的完整语义是「次数/间隔」，我们只借用其中的「同时在飞次数」上限：
/// 间隔用不上（搜索/阅读的请求节奏由调度与预算控制）。
pub fn parse_source_concurrency(raw: Option<&str>, default: usize) -> usize {
    let digits: String = match raw {
        Some(v) => v
            .trim()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect(),
        None => String::new(),
    };
    match digits.parse::<usize>() {
        Ok(0) | Err(_) => default,
        Ok(n) => n.clamp(SOURCE_CONCURRENCY_MIN, SOURCE_CONCURRENCY_MAX),
    }
}

/// 书源的每源在飞上限（旧接口，等价于 [`source_limit`] 的并发部分）。
#[cfg(test)]
pub fn source_concurrency(source: &BookSource) -> usize {
    parse_source_concurrency(
        source.concurrent_rate.as_deref(),
        conf().source_concurrency_default,
    )
}

/// 书源请求限制：`concurrentRate` 的完整 Legado 语义「次数/间隔」。
/// - `concurrency`：同时在飞上限（`N`）
/// - `per` / `interval`：每 `per` 次请求后等待 `interval`（`N/interval`，单位毫秒）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceLimit {
    pub concurrency: usize,
    pub per: usize,
    pub interval: Duration,
}

impl SourceLimit {
    pub fn concurrency_only(n: usize) -> Self {
        Self {
            concurrency: n,
            per: 0,
            interval: Duration::ZERO,
        }
    }
}

/// 解析完整的 `concurrentRate`：`"3/1000"` → 3 次/1000ms；只写 `"3"` 时退化为纯并发上限。
/// 非法/0/缺省时 `per=0`（不做速率等待），并发上限按 [`parse_source_concurrency`]。
pub fn parse_source_limit(raw: Option<&str>, default: usize) -> SourceLimit {
    let concurrency = parse_source_concurrency(raw, default);
    let interval_ms = raw
        .and_then(|v| v.trim().split_once('/'))
        .and_then(|(_, b)| b.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(|ms| ms.clamp(1, SOURCE_RATE_MAX_INTERVAL_MS));
    match interval_ms {
        Some(interval_ms) => SourceLimit {
            concurrency,
            per: concurrency,
            interval: Duration::from_millis(interval_ms),
        },
        None => SourceLimit::concurrency_only(concurrency),
    }
}

/// 书源的完整请求限制（并发 + 速率）。
pub fn source_limit(source: &BookSource) -> SourceLimit {
    parse_source_limit(
        source.concurrent_rate.as_deref(),
        conf().source_concurrency_default,
    )
}

/// 每源在飞信号量：键是 `bookSourceUrl`，值带上限（源规则改动后自动重建）。
pub(super) type SourceSlots = HashMap<String, (usize, Arc<Semaphore>)>;
pub(super) static SOURCE_SLOTS: Lazy<Mutex<SourceSlots>> = Lazy::new(|| Mutex::new(HashMap::new()));
/// 按源限流导致的等待次数 / 累计等待毫秒（`/healthz` 暴露）。
pub(super) static SOURCE_LIMIT_WAITS: AtomicU64 = AtomicU64::new(0);
pub(super) static SOURCE_LIMIT_WAIT_MS: AtomicU64 = AtomicU64::new(0);

pub(super) fn source_semaphore(source_key: &str, limit: usize) -> Arc<Semaphore> {
    let mut map = SOURCE_SLOTS.lock().unwrap_or_else(|e| e.into_inner());
    match map.get(source_key) {
        Some((l, sem)) if *l == limit => sem.clone(),
        _ => {
            let sem = Arc::new(Semaphore::new(limit));
            map.insert(source_key.to_string(), (limit, sem.clone()));
            sem
        }
    }
}

/// 拿一个「同一书源」的在飞许可；超限时排队并计数。
/// 先拿按源许可再拿全局许可，保证某个源的排队不会占住全局 16 槽（其他源照常推进）。
pub(super) async fn acquire_source_slot(
    source_key: &str,
    limit: usize,
) -> anyhow::Result<tokio::sync::OwnedSemaphorePermit> {
    let limit = limit.clamp(SOURCE_CONCURRENCY_MIN, SOURCE_CONCURRENCY_MAX);
    let sem = source_semaphore(source_key, limit);
    if let Ok(p) = sem.clone().try_acquire_owned() {
        return Ok(p);
    }
    SOURCE_LIMIT_WAITS.fetch_add(1, Ordering::Relaxed);
    let started = std::time::Instant::now();
    let p = sem
        .acquire_owned()
        .await
        .map_err(|_| anyhow!("source semaphore closed"))?;
    SOURCE_LIMIT_WAIT_MS.fetch_add(started.elapsed().as_millis() as u64, Ordering::Relaxed);
    Ok(p)
}

/// `(等待次数, 累计等待毫秒)`：按源限流指标，供 `/healthz` 与排障使用。
pub fn source_limit_metrics() -> (u64, u64) {
    (
        SOURCE_LIMIT_WAITS.load(Ordering::Relaxed),
        SOURCE_LIMIT_WAIT_MS.load(Ordering::Relaxed),
    )
}

// ---- `concurrentRate` 的「次数/间隔」速率门 ----

#[derive(Clone, Copy, Default)]
pub(super) struct RateGate {
    pub(super) used: usize,
    pub(super) until: Option<std::time::Instant>,
}

pub(super) static SOURCE_RATE_GATES: Lazy<Mutex<HashMap<String, RateGate>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// 每 `limit.per` 次请求后等待 `limit.interval`（Legado `concurrentRate` 语义）。
/// 与并发信号量互补：信号量限制在飞数量，本函数限制请求节奏。
pub(super) async fn acquire_source_rate(
    source_key: &str,
    limit: SourceLimit,
) -> anyhow::Result<()> {
    if limit.per == 0 || limit.interval.is_zero() {
        return Ok(());
    }
    loop {
        let wait = {
            let mut map = SOURCE_RATE_GATES.lock().unwrap_or_else(|e| e.into_inner());
            let g = map.entry(source_key.to_string()).or_default();
            let now = std::time::Instant::now();
            match g.until {
                Some(until) if now < until => Some(until - now),
                _ => {
                    g.until = None;
                    g.used += 1;
                    if g.used >= limit.per {
                        g.used = 0;
                        g.until = Some(now + limit.interval);
                    }
                    None
                }
            }
        };
        if let Some(d) = wait {
            SOURCE_LIMIT_WAITS.fetch_add(1, Ordering::Relaxed);
            let started = std::time::Instant::now();
            tokio::time::sleep(d).await;
            SOURCE_LIMIT_WAIT_MS.fetch_add(started.elapsed().as_millis() as u64, Ordering::Relaxed);
            continue;
        }
        return Ok(());
    }
}

/// 全局默认的每源上限（供未持有 `BookSource` 的调用点使用）。
pub fn source_concurrency_default() -> usize {
    conf().source_concurrency_default
}

/// 漫画源（type=2）判定；聚合搜索按此分配独立并发池。
pub fn is_comic_source_type(t: i64) -> bool {
    t == 2
}

pub(super) fn novel_search_slots() -> &'static Arc<Semaphore> {
    SEARCH_NOVEL.get_or_init(|| Arc::new(Semaphore::new(conf().concurrency)))
}

pub(super) fn novel_search_slots_all() -> &'static Arc<Semaphore> {
    SEARCH_NOVEL_ALL.get_or_init(|| Arc::new(Semaphore::new(conf().all_concurrency)))
}

/// `scope=all` 最终生效的源级并发槽（用户显式调低时小于 16）。仅单测断言使用。
#[allow(dead_code)]
pub fn all_search_slots() -> usize {
    conf().all_concurrency
}

/// 该 scope 的源级并发槽总数（小说 + 漫画）。调度器用它决定同时在飞的源数。
pub fn search_slots(scope: SearchScope) -> usize {
    match scope {
        SearchScope::All => conf().all_concurrency,
        _ => conf().concurrency,
    }
}

/// 漫画槽数量：并发的 1/4（至少 1 个）。默认 16 → 4 个。
pub(crate) fn comic_slot_count(slots: usize) -> usize {
    (slots / 4).max(1)
}

pub(super) fn comic_search_slots() -> &'static Arc<Semaphore> {
    SEARCH_COMIC.get_or_init(|| Arc::new(Semaphore::new(comic_slot_count(conf().concurrency))))
}

pub(super) fn comic_search_slots_all() -> &'static Arc<Semaphore> {
    SEARCH_COMIC_ALL
        .get_or_init(|| Arc::new(Semaphore::new(comic_slot_count(conf().all_concurrency))))
}

/// 在全局 deadline 之前拿一个“源级”搜索槽：
/// - 漫画源走独立小池，不排在小说源后面；
/// - 排队时间不计入单源请求预算；
/// - 到 deadline 仍未拿到 → 明确报“全局搜索预算耗尽”，不伪装成源超时。
/// - `scope=all`（加强搜索）使用独立池：未显式设置并发时 16 槽，显式设置时尊重用户配置。
pub async fn acquire_search_slot(
    comic: bool,
    deadline: tokio::time::Instant,
    scope: SearchScope,
) -> anyhow::Result<tokio::sync::SemaphorePermit<'static>> {
    let sem = match (scope, comic) {
        (SearchScope::Curated, true) => comic_search_slots(),
        (SearchScope::Curated, false) => novel_search_slots(),
        (SearchScope::All, true) => comic_search_slots_all(),
        (SearchScope::All, false) => novel_search_slots_all(),
    };
    match tokio::time::timeout_at(deadline, sem.acquire()).await {
        Ok(Ok(p)) => Ok(p),
        Ok(Err(_)) => Err(anyhow!("search semaphore closed")),
        Err(_) => Err(anyhow!("全局搜索预算耗尽（排队未及执行）")),
    }
}
