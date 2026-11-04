use std::env;

/// 环境变量前缀：新品牌优先，旧前缀仅作兼容（两者都缺才用默认值）。
const ENV_PREFIXES: [&str; 2] = ["BORANLIB_", "LEGADO_"];

/// 按前缀顺序查找；空串/纯空白视为未设置，继续找下一个前缀。
fn lookup_prefixed(key: &str, get: impl Fn(&str) -> Option<String>) -> Option<String> {
    ENV_PREFIXES
        .iter()
        .filter_map(|p| get(&format!("{p}{key}")))
        .find(|v| !v.trim().is_empty())
}

/// 读 `BORANLIB_<key>`，缺省时读 `LEGADO_<key>`，都没有返回 None。
pub fn env_prefixed(key: &str) -> Option<String> {
    lookup_prefixed(key, |k| env::var(k).ok())
}

fn env_u64(key: &str, default: u64) -> u64 {
    env_prefixed(key)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

/// 显式设置才返回 `Some`（缺省/空串/非法值均视为“未设置”），用于区分默认值与用户覆盖。
fn env_u64_opt(key: &str) -> Option<u64> {
    env_prefixed(key).and_then(|v| v.trim().parse::<u64>().ok())
}

/// 源级并发槽：默认 16（2026-11-04 扫参照：8→16 在同预算下命中源 +3.3pt，
/// 首屏不变），允许 1..16，超出夹回。
fn clamp_http_concurrency(n: u64) -> usize {
    n.clamp(1, MAX_SEARCH_CONCURRENCY) as usize
}

/// `scope=all` 的源级并发槽：显式设置 `HTTP_CONCURRENCY` 时尊重用户（min(配置,16)），
/// 未设置时用设备建议值 16。
fn resolve_all_slots(raw: Option<u64>) -> usize {
    match raw {
        Some(n) => n.clamp(1, MAX_SEARCH_CONCURRENCY) as usize,
        None => MAX_SEARCH_CONCURRENCY as usize,
    }
}

/// 搜索并发硬上限：设备实测 16 槽不构成资源瓶颈（VmHWM 46–71 MB）。
const MAX_SEARCH_CONCURRENCY: u64 = 16;

/// 单段 JS 规则的默认时间预算（毫秒）。
///
/// 200ms 是早期按开发机估算的值：慢设备上腾讯漫画图片规则单次约 1.4s，
/// 默认配置会把整章图片判超时（0 张）。放宽到 3s 覆盖实测最慢规则并留约 2× 余量；
/// 预算只影响「确实跑得慢」的规则，常见规则仍是毫秒级，不会因此变慢。
pub const DEFAULT_JS_TIMEOUT_MS: u64 = 3000;

/// 夹取用户设置：默认 [`DEFAULT_JS_TIMEOUT_MS`]，范围 20–5000ms。
/// 上限 5s 用于兜住写坏的规则，同时保证单个请求的最坏等待有界。
fn resolve_js_timeout_ms(raw: Option<u64>) -> u64 {
    raw.unwrap_or(DEFAULT_JS_TIMEOUT_MS).clamp(20, 5000)
}

/// 解析搜索的三个预算旋钮：返回 `(单源预算, curated 整批, all 整批)`。
///
/// - `SEARCH_TIMEOUT_SECS`：单源预算，默认 8s，两个 scope 共用；
/// - `SEARCH_CURATED_BUDGET_SECS` / `SEARCH_ALL_BUDGET_SECS`：按 scope 的整批预算；
/// - `SEARCH_TOTAL_BUDGET_SECS`：两 scope 的整批预算统一覆盖值（优先级低于
///   上面两个按 scope 的变量）。
///
/// 未设置任何整批变量时取 scope 默认（curated 6s / all 10s）。
/// 兼容旧部署：只显式设置了 `SEARCH_TIMEOUT_SECS` 时，整批预算回落到同值，
/// 保持“两者同值”的历史语义。
fn resolve_search_budgets(
    timeout_raw: Option<u64>,
    total_raw: Option<u64>,
    curated_raw: Option<u64>,
    all_raw: Option<u64>,
) -> (u64, u64, u64) {
    let per_source = timeout_raw.unwrap_or(8).clamp(5, 30);
    // 旧语义只在“完全没有设置任何整批变量”时生效：只设 SEARCH_TIMEOUT_SECS 视为
    // “两者同值”。一旦显式给了任一整批变量，另一个 scope 就回落到自己的默认值。
    let legacy = if total_raw.is_none() && curated_raw.is_none() && all_raw.is_none() {
        timeout_raw.map(|_| per_source)
    } else {
        None
    };
    let total = total_raw.map(|v| v.clamp(2, 60));
    let curated = curated_raw
        .map(|v| v.clamp(2, 60))
        .or(total)
        .or(legacy)
        .unwrap_or(6);
    let all = all_raw
        .map(|v| v.clamp(2, 60))
        .or(total)
        .or(legacy)
        .unwrap_or(10);
    (per_source, curated, all)
}

fn env_bool(key: &str, default: bool) -> bool {
    match env_prefixed(key).map(|v| v.to_ascii_lowercase()) {
        Some(v) => matches!(v.as_str(), "1" | "true" | "yes" | "on"),
        None => default,
    }
}

/// 服务配置，全部来自环境变量，便于在 100MB 内存的小机上调参。
#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// 数据根目录，下面会生成 shared.db 与 users/{uid}.db
    pub data_dir: String,
    pub jwt_secret: String,

    // ---- 低资源预算（474 MiB 可用内存的 OpenStick）----
    /// 正文缓存落库总量上限（MiB）
    pub cache_limit_mb: u64,
    /// 全局 HTTP 抓取并发上限（信号量）
    pub http_concurrency: usize,
    /// 单次抓取超时（秒）
    pub http_timeout_secs: u64,
    /// 单源搜索预算（秒）：聚合搜索按“每个书源”独立计时，超时只丢该源、不影响整体。
    /// 与整批总预算解耦（见 `search_curated_budget_secs` / `search_all_budget_secs`）。
    pub search_timeout_secs: u64,
    /// `scope=curated` 的整批搜索总预算（秒，默认 6）。
    pub search_curated_budget_secs: u64,
    /// `scope=all` 的整批搜索总预算（秒，默认 10）。
    pub search_all_budget_secs: u64,
    /// `scope=all`（加强搜索）的源级并发槽：用户显式设置 `HTTP_CONCURRENCY` 时取
    /// `min(配置, 16)`，未显式设置时用 16（设备实测值）。
    pub search_all_slots: usize,
    /// 单响应体大小上限（字节）
    pub http_max_body_bytes: usize,
    /// 图片代理进程内 LRU 缓存上限（MiB）
    pub image_cache_mb: u64,
    /// 单张图片大小上限（字节，超限返回 413 或流式截断）
    pub image_max_body_bytes: usize,
    /// 长条漫画页服务端切片总开关（默认关）。关闭时 `/k` 与 `/api/book/image?slice=1`
    /// 都退回整图一页；开启后只对显示高度超过 1.3 屏的图切片。
    pub comic_slice: bool,
    /// 切片版心宽（CSS px，默认 928）
    pub comic_slice_dw: u32,
    /// 切片可用高（CSS px，默认 1196 = 1268 舞台 − 72 底栏留白）
    pub comic_slice_vh: u32,
    /// 片间 overlap（CSS px，默认 64，上限为可用高的 1/4）
    pub comic_slice_ov: u32,
    /// 近白行亮度均值下限（默认 200）
    pub comic_slice_mu: u8,
    /// 近白行带内标准差上限（默认 30）
    pub comic_slice_sd: u8,
    /// 切点搜索半径（CSS px，默认 96）
    pub comic_slice_r: u32,
    /// `/k` 默认视口宽（CSS px，缺 `vw` 参数时兜底，默认 1072）
    pub kindle_vw: u32,
    /// `/k` 默认视口高（CSS px，缺 `vh` 参数时兜底，默认 1268）
    pub kindle_vh: u32,
    /// 书源 JS 规则总开关
    pub js_enabled: bool,
    /// 单段 JS 规则的时间预算（毫秒，用于超时提示/统计）。
    /// 默认 [`DEFAULT_JS_TIMEOUT_MS`]，可用 `BORANLIB_JS_TIMEOUT_MS` 覆盖。
    pub js_timeout_ms: u64,
    /// 目录缓存 TTL（秒）
    pub toc_ttl_secs: i64,
    /// 正文缓存 TTL（秒）
    pub content_ttl_secs: i64,
    /// 同时打开的用户库连接池上限（LRU 关闭）
    pub db_max_open_users: usize,
    /// 是否允许把已有库一次性 `VACUUM` 转成增量回收模式（默认关闭，避免启动期长事务）
    pub db_vacuum_convert: bool,
    /// 迁移前自动备份的保留份数（`data/backups` 下最多保留几份，默认 5）
    pub db_backup_keep: usize,
    /// 是否允许自愈“仅换行差异”的迁移校验和（默认开启）
    pub migration_repair: bool,
    /// 书源连续失败进入冷却的时长（秒，默认 24h）
    pub source_cooldown_secs: u64,
    /// 源质量：升入 preferred 的最低分（默认 70）
    pub source_tier_preferred_min_score: i32,
    /// 源质量：跌破该分且已有真实使用证据后进入 quarantined（默认 30）
    pub source_tier_quarantine_below_score: i32,
    /// 源质量：连续 N 次达标才升 preferred（默认 3）
    pub source_tier_promote_streak: i64,
    /// 源质量：连续 N 次不达标才降级（默认 2）
    pub source_tier_demote_streak: i64,
    /// 源质量：覆盖旧版本时新评分允许的最低差值（默认 5）
    pub source_tier_update_margin: i32,
    /// 评分权重（合计 100）
    pub source_score_w_connect: i32,
    pub source_score_w_rule: i32,
    pub source_score_w_hit: i32,
    pub source_score_w_parse: i32,
    pub source_score_w_ttfb: i32,
    /// 体检语料抽样条数（每个源每次抽几条，默认 5）
    pub source_bench_samples: usize,
    /// 体检评分权重：命中率（默认 60）
    pub source_bench_w_hit: i32,
    /// 体检评分权重：延迟分（默认 30）
    pub source_bench_w_latency: i32,
    /// 体检评分权重：成功率（默认 10）
    pub source_bench_w_fail: i32,
    /// 精选断层阈值：相邻分数差 >= 该值即截断（默认 8）
    pub source_bench_gap: i32,
    /// 精选数量下限 / 上限（默认 8 / 14）
    pub source_bench_min: usize,
    pub source_bench_max: usize,
    /// 漫画精选单独下限 / 上限（默认 3 / 14）：按语料类型单独配额，避免与小说共用 min 时该类长期选不满
    pub source_bench_comic_min: usize,
    pub source_bench_comic_max: usize,
    /// 精选自动重算间隔（小时，默认 72）：过期后下一次搜索会后台触发一次体检
    pub source_bench_interval_hours: u64,
    /// 规则试跑预览是否允许访问回环/私网/链路本地地址（默认拒绝，防 SSRF）。
    /// 本地调试书源时可用 `PREVIEW_ALLOW_PRIVATE=true` 放行。
    pub preview_allow_private: bool,
    /// 前端静态资源目录。默认 `web/dist` 保持原有行为；
    /// 从其他 cwd 启动时用 `WEB_DIR` 指向绝对路径。
    pub web_dir: String,
}

impl Config {
    pub fn from_env() -> Self {
        let (search_timeout_secs, search_curated_budget_secs, search_all_budget_secs) =
            resolve_search_budgets(
                env_u64_opt("SEARCH_TIMEOUT_SECS"),
                env_u64_opt("SEARCH_TOTAL_BUDGET_SECS"),
                env_u64_opt("SEARCH_CURATED_BUDGET_SECS"),
                env_u64_opt("SEARCH_ALL_BUDGET_SECS"),
            );
        let http_concurrency_raw = env_u64_opt("HTTP_CONCURRENCY");
        // 切片参数有跨字段约束：overlap 不得超过可用高的 1/4（与 SliceParams 的夹取一致）。
        let comic_slice_vh = env_u64("COMIC_SLICE_VH", 1196).clamp(320, 2000) as u32;
        let comic_slice_ov =
            env_u64("COMIC_SLICE_OV", 64).clamp(0, (comic_slice_vh / 4) as u64) as u32;
        Self {
            host: env_prefixed("HOST").unwrap_or_else(|| "0.0.0.0".into()),
            port: env_prefixed("PORT")
                .and_then(|v| v.parse().ok())
                .unwrap_or(8080),
            data_dir: env_prefixed("DATA_DIR").unwrap_or_else(|| "data".into()),
            jwt_secret: env_prefixed("JWT_SECRET").unwrap_or_else(|| "change-me-in-prod".into()),

            cache_limit_mb: env_u64("CACHE_LIMIT_MB", 50).clamp(4, 512),
            // 并发默认 16：扫参照显示同预算下 8→16 命中源 +3.3pt、首屏不变；上限 16。
            http_concurrency: clamp_http_concurrency(http_concurrency_raw.unwrap_or(16)),
            // all 范围：显式设置时尊重用户（min(配置,16)），未设置时用设备建议的 16。
            search_all_slots: resolve_all_slots(http_concurrency_raw),
            http_timeout_secs: env_u64("HTTP_TIMEOUT_SECS", 15).clamp(3, 60),
            // 单源预算与整批预算分开：单源默认 8s，整批按 scope 默认（curated 6s / all 10s）。
            search_timeout_secs,
            search_curated_budget_secs,
            search_all_budget_secs,
            http_max_body_bytes: env_u64("HTTP_MAX_BODY_MB", 5).clamp(1, 32) as usize * 1024 * 1024,
            image_cache_mb: env_u64("IMAGE_CACHE_MB", 64).clamp(4, 256),
            image_max_body_bytes: env_u64("IMAGE_MAX_BODY_MB", 8).clamp(1, 32) as usize
                * 1024
                * 1024,
            comic_slice: env_bool("COMIC_SLICE", false),
            comic_slice_dw: env_u64("COMIC_SLICE_DW", 928).clamp(320, 2000) as u32,
            comic_slice_vh,
            comic_slice_ov,
            comic_slice_mu: env_u64("COMIC_SLICE_MU", 200).clamp(0, 255) as u8,
            comic_slice_sd: env_u64("COMIC_SLICE_SD", 30).clamp(0, 255) as u8,
            comic_slice_r: env_u64("COMIC_SLICE_R", 96).clamp(8, 480) as u32,
            // `/k` 视口兜底：请求带 `vw`/`vh` 时以请求为准，缺参才用这两个环境默认值。
            kindle_vw: env_u64("KINDLE_VW", 1072).clamp(480, 2400) as u32,
            kindle_vh: env_u64("KINDLE_VH", 1268).clamp(480, 2400) as u32,
            js_enabled: env_bool("JS_ENABLED", true),
            js_timeout_ms: resolve_js_timeout_ms(env_u64_opt("JS_TIMEOUT_MS")),
            toc_ttl_secs: env_u64("TOC_TTL_SECS", 6 * 3600).clamp(60, 7 * 86400) as i64,
            content_ttl_secs: env_u64("CONTENT_TTL_SECS", 7 * 86400).clamp(60, 30 * 86400) as i64,
            db_max_open_users: env_u64("DB_MAX_OPEN_USERS", 8).clamp(1, 32) as usize,
            db_vacuum_convert: env_bool("DB_VACUUM_CONVERT", false),
            db_backup_keep: env_u64("DB_BACKUP_KEEP", 5).clamp(1, 50) as usize,
            migration_repair: env_bool("MIGRATION_REPAIR", true),
            source_cooldown_secs: env_u64("SOURCE_COOLDOWN_SECS", 24 * 3600).clamp(60, 30 * 86400),
            source_tier_preferred_min_score: env_u64("SOURCE_TIER_PREFERRED_MIN_SCORE", 70)
                .clamp(0, 100) as i32,
            source_tier_quarantine_below_score: env_u64("SOURCE_TIER_QUARANTINE_BELOW_SCORE", 30)
                .clamp(0, 100) as i32,
            source_tier_promote_streak: env_u64("SOURCE_TIER_PROMOTE_STREAK", 3).clamp(1, 20)
                as i64,
            source_tier_demote_streak: env_u64("SOURCE_TIER_DEMOTE_STREAK", 2).clamp(1, 20) as i64,
            source_tier_update_margin: env_u64("SOURCE_TIER_UPDATE_MARGIN", 5).clamp(0, 50) as i32,
            source_score_w_connect: env_u64("SOURCE_SCORE_W_CONNECT", 25).clamp(0, 100) as i32,
            source_score_w_rule: env_u64("SOURCE_SCORE_W_RULE", 15).clamp(0, 100) as i32,
            source_score_w_hit: env_u64("SOURCE_SCORE_W_HIT", 30).clamp(0, 100) as i32,
            source_score_w_parse: env_u64("SOURCE_SCORE_W_PARSE", 15).clamp(0, 100) as i32,
            source_score_w_ttfb: env_u64("SOURCE_SCORE_W_TTFB", 15).clamp(0, 100) as i32,
            source_bench_samples: env_u64("SOURCE_BENCH_SAMPLES", 5).clamp(1, 20) as usize,
            source_bench_w_hit: env_u64("SOURCE_BENCH_W_HIT", 60).clamp(0, 100) as i32,
            source_bench_w_latency: env_u64("SOURCE_BENCH_W_LATENCY", 30).clamp(0, 100) as i32,
            source_bench_w_fail: env_u64("SOURCE_BENCH_W_FAIL", 10).clamp(0, 100) as i32,
            source_bench_gap: env_u64("SOURCE_BENCH_GAP", 8).clamp(1, 100) as i32,
            source_bench_min: env_u64("SOURCE_BENCH_MIN", 8).clamp(1, 50) as usize,
            source_bench_max: env_u64("SOURCE_BENCH_MAX", 14).clamp(1, 50) as usize,
            source_bench_comic_min: env_u64("SOURCE_BENCH_COMIC_MIN", 3).clamp(1, 50) as usize,
            source_bench_comic_max: env_u64("SOURCE_BENCH_COMIC_MAX", 14).clamp(1, 50) as usize,
            source_bench_interval_hours: env_u64("SOURCE_BENCH_INTERVAL_HOURS", 72)
                .clamp(1, 24 * 30),
            preview_allow_private: env_bool("PREVIEW_ALLOW_PRIVATE", false),
            web_dir: env_prefixed("WEB_DIR")
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "web/dist".into()),
        }
    }

    pub fn cache_limit_bytes(&self) -> i64 {
        (self.cache_limit_mb * 1024 * 1024) as i64
    }

    pub fn image_cache_limit_bytes(&self) -> usize {
        (self.image_cache_mb * 1024 * 1024) as usize
    }

    /// 共享桶连接串：sqlite://<data_dir>/shared.db
    pub fn shared_db_url(&self) -> String {
        format!("sqlite://{}/shared.db", self.data_dir)
    }

    /// 用户独立桶连接串：sqlite://<data_dir>/users/<uid>.db（物理分桶）
    pub fn user_db_url(&self, uid: &str) -> String {
        format!("sqlite://{}/users/{}.db", self.data_dir, uid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_from<'a>(map: &'a HashMap<&'a str, &'a str>) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| map.get(k).map(|v| v.to_string())
    }

    // 新前缀优先：BORANLIB_HOST 与 LEGADO_HOST 同时存在时取前者
    #[test]
    fn boranlib_prefix_wins_over_legacy() {
        let map = HashMap::from([("BORANLIB_HOST", "127.0.0.1"), ("LEGADO_HOST", "0.0.0.0")]);
        assert_eq!(
            lookup_prefixed("HOST", lookup_from(&map)).as_deref(),
            Some("127.0.0.1")
        );
    }

    // 新前缀缺失时回落到旧前缀 LEGADO_
    #[test]
    fn legacy_prefix_is_used_when_boranlib_missing() {
        let map = HashMap::from([("LEGADO_PORT", "3000")]);
        assert_eq!(
            lookup_prefixed("PORT", lookup_from(&map)).as_deref(),
            Some("3000")
        );
    }

    // 两套都缺才返回 None（调用方随后用默认值）
    #[test]
    fn missing_both_prefixes_returns_none() {
        let map: HashMap<&str, &str> = HashMap::new();
        assert_eq!(lookup_prefixed("HOST", lookup_from(&map)), None);
    }

    // 新前缀只设了空串时视为“未设置”，继续回落到 LEGADO_
    #[test]
    fn blank_boranlib_value_falls_back_to_legacy() {
        let map = HashMap::from([("BORANLIB_PORT", "   "), ("LEGADO_PORT", "3000")]);
        assert_eq!(
            lookup_prefixed("PORT", lookup_from(&map)).as_deref(),
            Some("3000")
        );
    }

    // 预算拆分：都不设置 → 单源 8s、整批 curated 6s / all 10s
    #[test]
    fn default_search_budgets_are_split() {
        assert_eq!(resolve_search_budgets(None, None, None, None), (8, 6, 10));
    }

    // 独立生效：总预算设置后不再等于单源预算（两 scope 同值）
    #[test]
    fn search_budgets_are_independent() {
        assert_eq!(
            resolve_search_budgets(Some(6), Some(25), None, None),
            (6, 25, 25)
        );
        assert_eq!(
            resolve_search_budgets(Some(6), Some(8), None, None),
            (6, 8, 8)
        );
    }

    // 按 scope 的整批预算：SEARCH_ALL_BUDGET_SECS 只改 all，curated 仍取默认
    #[test]
    fn scope_specific_total_budget_overrides() {
        assert_eq!(
            resolve_search_budgets(None, None, None, Some(15)),
            (8, 6, 15)
        );
        assert_eq!(
            resolve_search_budgets(None, None, Some(8), Some(15)),
            (8, 8, 15)
        );
        // 显式按 scope 的值优先于统一覆盖值
        assert_eq!(
            resolve_search_budgets(Some(8), Some(25), Some(8), Some(15)),
            (8, 8, 15)
        );
    }

    // 兼容：只设旧变量 SEARCH_TIMEOUT_SECS 时两者同值（历史语义）
    #[test]
    fn legacy_timeout_only_keeps_both_equal() {
        assert_eq!(
            resolve_search_budgets(Some(10), None, None, None),
            (10, 10, 10)
        );
        // 越界也按夹取后的单源值回落，避免日志与实际不一致
        assert_eq!(
            resolve_search_budgets(Some(40), None, None, None),
            (30, 30, 30)
        );
    }

    // 越界夹取：单源 5..30，整批 2..60
    #[test]
    fn search_budgets_are_clamped() {
        assert_eq!(
            resolve_search_budgets(Some(3), Some(1), Some(0), Some(99)),
            (5, 2, 60)
        );
        assert_eq!(
            resolve_search_budgets(Some(99), None, None, None),
            (30, 30, 30)
        );
    }

    // 只指定 all 整批时，curated 仍取自己的默认 6s，不被旧的“两者同值”污染
    #[test]
    fn setting_one_scope_does_not_leak_per_source_into_other_scope() {
        assert_eq!(
            resolve_search_budgets(Some(30), None, None, Some(10)),
            (30, 6, 10)
        );
    }

    // 并发夹取：允许 16、17 夹回 16、0 夹到 1
    #[test]
    fn http_concurrency_is_clamped_to_16() {
        assert_eq!(clamp_http_concurrency(8), 8);
        assert_eq!(clamp_http_concurrency(16), 16, "16 应被允许");
        assert_eq!(clamp_http_concurrency(17), 16, "17 应夹到 16");
        assert_eq!(clamp_http_concurrency(0), 1);
    }

    // 默认并发槽是 16（扫参结论），未显式设置时 all 搜索也拿 16 槽
    #[test]
    fn default_concurrency_is_16() {
        assert_eq!(clamp_http_concurrency(16), 16);
        assert_eq!(resolve_all_slots(None), 16);
    }

    // all 范围的并发：用户设 4 → 4；未设置 → 16；设 32 → 16
    #[test]
    fn all_scope_slots_respect_explicit_setting() {
        assert_eq!(resolve_all_slots(Some(4)), 4, "用户显式调低应生效");
        assert_eq!(resolve_all_slots(None), 16, "未设置时用设备建议 16");
        assert_eq!(resolve_all_slots(Some(32)), 16, "超过 16 夹回");
        assert_eq!(resolve_all_slots(Some(0)), 1);
    }

    // JS 预算：默认 3s（覆盖腾讯漫画图片规则实测 1.4s），显式值原样生效
    #[test]
    fn js_timeout_defaults_to_3000() {
        assert_eq!(resolve_js_timeout_ms(None), DEFAULT_JS_TIMEOUT_MS);
        assert_eq!(resolve_js_timeout_ms(None), 3000);
        assert_eq!(resolve_js_timeout_ms(Some(1000)), 1000);
        assert_eq!(resolve_js_timeout_ms(Some(3000)), 3000);
    }

    // JS 预算夹取边界：下限 20ms、上限 5000ms（写坏的规则也不至于让请求无限等）
    #[test]
    fn js_timeout_is_clamped_to_20_5000() {
        assert_eq!(resolve_js_timeout_ms(Some(0)), 20);
        assert_eq!(resolve_js_timeout_ms(Some(19)), 20);
        assert_eq!(resolve_js_timeout_ms(Some(20)), 20);
        assert_eq!(resolve_js_timeout_ms(Some(5000)), 5000);
        assert_eq!(resolve_js_timeout_ms(Some(5001)), 5000);
        assert_eq!(resolve_js_timeout_ms(Some(u64::MAX)), 5000);
    }
}
