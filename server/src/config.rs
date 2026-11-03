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
    /// 单源搜索超时（秒）：聚合搜索按“每个书源”独立计时，超时只丢该源、不影响整体
    pub search_timeout_secs: u64,
    /// 单响应体大小上限（字节）
    pub http_max_body_bytes: usize,
    /// 图片代理进程内 LRU 缓存上限（MiB）
    pub image_cache_mb: u64,
    /// 单张图片大小上限（字节，超限返回 413 或流式截断）
    pub image_max_body_bytes: usize,
    /// 书源 JS 规则总开关
    pub js_enabled: bool,
    /// 单段 JS 规则的时间预算（毫秒，用于超时提示/统计）
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
    /// 规则试跑预览是否允许访问回环/私网/链路本地地址（默认拒绝，防 SSRF）。
    /// 本地调试书源时可用 `PREVIEW_ALLOW_PRIVATE=true` 放行。
    pub preview_allow_private: bool,
    /// 前端静态资源目录。默认 `web/dist` 保持原有行为；
    /// 从其他 cwd 启动时用 `WEB_DIR` 指向绝对路径。
    pub web_dir: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            host: env_prefixed("HOST").unwrap_or_else(|| "0.0.0.0".into()),
            port: env_prefixed("PORT")
                .and_then(|v| v.parse().ok())
                .unwrap_or(8080),
            data_dir: env_prefixed("DATA_DIR").unwrap_or_else(|| "data".into()),
            jwt_secret: env_prefixed("JWT_SECRET").unwrap_or_else(|| "change-me-in-prod".into()),

            cache_limit_mb: env_u64("CACHE_LIMIT_MB", 50).clamp(4, 512),
            // 并发 8（原 3）：25 个源的实测里，3 并发会让后半批源在队列里空转到超时，
            // 8 并发把“排队时间”压到预算以内，单源超时才真正只惩罚慢站。
            http_concurrency: env_u64("HTTP_CONCURRENCY", 8).clamp(1, 8) as usize,
            http_timeout_secs: env_u64("HTTP_TIMEOUT_SECS", 15).clamp(3, 60),
            // 单源 10s：比 15s 缩短 1/3，但与 ↑并发配合后“排队等待”不会再吃掉整段预算
            search_timeout_secs: env_u64("SEARCH_TIMEOUT_SECS", 10).clamp(5, 30),
            http_max_body_bytes: env_u64("HTTP_MAX_BODY_MB", 5).clamp(1, 32) as usize * 1024 * 1024,
            image_cache_mb: env_u64("IMAGE_CACHE_MB", 64).clamp(4, 256),
            image_max_body_bytes: env_u64("IMAGE_MAX_BODY_MB", 8).clamp(1, 32) as usize * 1024 * 1024,
            js_enabled: env_bool("JS_ENABLED", true),
            js_timeout_ms: env_u64("JS_TIMEOUT_MS", 200).clamp(20, 5000),
            toc_ttl_secs: env_u64("TOC_TTL_SECS", 6 * 3600).clamp(60, 7 * 86400) as i64,
            content_ttl_secs: env_u64("CONTENT_TTL_SECS", 7 * 86400).clamp(60, 30 * 86400) as i64,
            db_max_open_users: env_u64("DB_MAX_OPEN_USERS", 8).clamp(1, 32) as usize,
            db_vacuum_convert: env_bool("DB_VACUUM_CONVERT", false),
            db_backup_keep: env_u64("DB_BACKUP_KEEP", 5).clamp(1, 50) as usize,
            migration_repair: env_bool("MIGRATION_REPAIR", true),
            source_cooldown_secs: env_u64("SOURCE_COOLDOWN_SECS", 24 * 3600)
                .clamp(60, 30 * 86400),
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
        let map = HashMap::from([
            ("BORANLIB_HOST", "127.0.0.1"),
            ("LEGADO_HOST", "0.0.0.0"),
        ]);
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
        let map = HashMap::from([
            ("BORANLIB_PORT", "   "),
            ("LEGADO_PORT", "3000"),
        ]);
        assert_eq!(
            lookup_prefixed("PORT", lookup_from(&map)).as_deref(),
            Some("3000")
        );
    }
}
