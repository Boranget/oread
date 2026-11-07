//! k_metrics 的参数解析与日志写入（move-only 从 k_metrics.rs 拆出）。

use super::*;

pub(crate) fn parse_params(query: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

pub(crate) fn take_param(
    params: &HashMap<String, String>,
    key: &str,
    limit: usize,
) -> Option<String> {
    params.get(key).map(|v| truncate(v, limit))
}

pub(crate) fn truncate(s: &str, limit: usize) -> String {
    s.chars().take(limit).collect()
}

pub(crate) fn session_token(cookie_header: &str) -> Option<String> {
    cookie_header
        .split(';')
        .map(str::trim)
        .find_map(|kv| kv.strip_prefix("k_session="))
        .map(|v| v.to_string())
}

/// cookie 短哈希：sha256 前 8 位十六进制，不落原文。
pub(crate) fn sid_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    format!("{:x}", digest)[..8].to_string()
}

pub(crate) fn classify_ua(ua: &str) -> &'static str {
    let l = ua.to_ascii_lowercase();
    if l.is_empty() {
        return "unknown";
    }
    if l.contains("kindle") || l.contains("silk") {
        "kindle"
    } else if l.contains("mobile")
        || l.contains("android")
        || l.contains("iphone")
        || l.contains("ipad")
        || l.contains("mobi")
    {
        "mobile"
    } else {
        "desktop"
    }
}

pub(crate) fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn spawn_write<T: Serialize + Send + 'static>(
    dir: PathBuf,
    prefix: &'static str,
    line: T,
) {
    tokio::spawn(async move {
        let Ok(json) = serde_json::to_string(&line) else {
            return;
        };
        let logs = dir.join("logs");
        if let Err(e) = tokio::fs::create_dir_all(&logs).await {
            warn!(error = %e, "k-metrics: 创建日志目录失败");
            return;
        }
        let day = Local::now().format("%Y%m%d").to_string();
        let file = logs.join(format!("{prefix}-{day}.jsonl"));
        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .await
        {
            Ok(mut f) => {
                use tokio::io::AsyncWriteExt;
                if let Err(e) = f.write_all(format!("{json}\n").as_bytes()).await {
                    warn!(error = %e, "k-metrics: 写入失败");
                }
            }
            Err(e) => warn!(error = %e, "k-metrics: 打开文件失败"),
        }
        let n = WRITES.fetch_add(1, Ordering::Relaxed);
        if n.is_multiple_of(PRUNE_EVERY) {
            let logs = logs.clone();
            let prefix = prefix;
            tokio::task::spawn_blocking(move || prune_with(&logs, prefix, KEEP_DAYS, MAX_BYTES));
        }
    });
}

pub(crate) fn log_files(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix) && n.ends_with(".jsonl"))
                .unwrap_or(false)
        })
        .collect();
    // 文件名按日期升序
    files.sort();
    files
}

pub(crate) fn file_len(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}
