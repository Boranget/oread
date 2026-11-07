//! k_metrics 的日志保留与清理（move-only 从 k_metrics.rs 拆出）。

use super::*;

/// 有界清理：保留最近 `keep_days` 个同类日文件；总占用超过 `max_bytes` 时删最旧。
pub(crate) fn prune_with(dir: &Path, prefix: &str, keep_days: usize, max_bytes: u64) {
    let mut files = log_files(dir, prefix);
    while files.len() > keep_days {
        let old = files.remove(0);
        if let Err(e) = std::fs::remove_file(&old) {
            warn!(error = %e, path = %old.display(), "k-metrics: 删除过期日志失败");
        }
    }
    let mut total: u64 = files.iter().map(|p| file_len(p)).sum();
    while total > max_bytes && files.len() > 1 {
        let old = files.remove(0);
        total = total.saturating_sub(file_len(&old));
        if let Err(e) = std::fs::remove_file(&old) {
            warn!(error = %e, path = %old.display(), "k-metrics: 删除超限日志失败");
        }
    }
}

pub(crate) fn default_date() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

pub(crate) fn valid_date(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

pub(crate) fn log_file(data_dir: &Path, prefix: &str, date: &str) -> PathBuf {
    data_dir
        .join("logs")
        .join(format!("{prefix}-{}.jsonl", date.replace('-', "")))
}
