//! db_guard 的路径、启动退避与备份目录（move-only 从 db_guard.rs 拆出）。

use super::*;

pub(crate) fn data_dir(cfg: &Config) -> &Path {
    Path::new(&cfg.data_dir)
}

pub(crate) fn db_path_of(cfg: &Config, scope: Scope) -> PathBuf {
    match scope {
        Scope::Shared => data_dir(cfg).join("shared.db"),
        Scope::User(uid) => data_dir(cfg).join("users").join(format!("{uid}.db")),
    }
}

pub(crate) fn db_file_name(scope: Scope) -> String {
    match scope {
        Scope::Shared => "shared.db".to_string(),
        Scope::User(uid) => format!("users/{uid}.db"),
    }
}

pub fn backup_root(cfg: &Config) -> PathBuf {
    data_dir(cfg).join(BACKUP_DIR)
}

pub(crate) fn marker_path(cfg: &Config) -> PathBuf {
    data_dir(cfg).join(FAILURE_MARKER)
}

// ---------------------------------------------------------------------------
// 启动退避
// ---------------------------------------------------------------------------

/// 上次启动失败后的退避：存在失败标记且时间很近时，先等够最小间隔。
/// 目的是让 systemd 的 `Restart=always` 不要打成风暴，而不是阻止人工立即重试
/// （人工重试最多也就多等 20s 以内）。
pub async fn throttle_startup(cfg: &Config) {
    let path = marker_path(cfg);
    let Ok(meta) = std::fs::metadata(&path) else {
        return;
    };
    let Ok(modified) = meta.modified() else {
        return;
    };
    let Ok(elapsed) = std::time::SystemTime::now().duration_since(modified) else {
        return;
    };
    let backoff = Duration::from_secs(STARTUP_BACKOFF_SECS);
    if elapsed < backoff {
        let wait = backoff - elapsed;
        tracing::warn!(
            wait_secs = wait.as_secs(),
            marker = %path.display(),
            "检测到上次启动失败，延迟重试以避免高频重启"
        );
        tokio::time::sleep(wait).await;
    }
}

/// 记录启动失败（覆盖写，mtime 即“上次失败时间”）。
pub(crate) fn record_startup_failure(cfg: &Config, reason: &str) {
    let path = marker_path(cfg);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = format!(
        "time={}\nreason={reason}\n",
        chrono::Utc::now().to_rfc3339()
    );
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!(error = %e, "无法写入启动失败标记（不影响本次错误输出）");
    }
}

/// 启动成功后清除失败标记，恢复正常启动速度。
pub fn clear_startup_failure(cfg: &Config) {
    let path = marker_path(cfg);
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
}

// ---------------------------------------------------------------------------
// 迁移前备份
// ---------------------------------------------------------------------------

pub(crate) fn utc_stamp() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// 备份目录名必须是 `<UTC 时间戳>`，可选 `-N` 后缀（同一秒内多次备份）。
pub(crate) fn is_backup_dir_name(name: &str) -> bool {
    let b = name.as_bytes();
    if b.len() < 16 || b[8] != b'T' {
        return false;
    }
    if !b[..8]
        .iter()
        .chain(b[9..15].iter())
        .all(|c| c.is_ascii_digit())
    {
        return false;
    }
    if b[15] != b'Z' {
        return false;
    }
    match b.len() {
        16 => true,
        _ => b[16] == b'-' && b[17..].iter().all(|c| c.is_ascii_digit()) && b.len() > 17,
    }
}

pub(crate) fn create_backup_dir(root: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root)
        .map_err(|e| format!("创建备份目录 {} 失败：{e}", root.display()))?;
    let base = utc_stamp();
    for i in 0..100 {
        let name = if i == 0 {
            base.clone()
        } else {
            format!("{base}-{i}")
        };
        let path = root.join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("创建备份目录 {} 失败：{e}", path.display())),
        }
    }
    Err(format!(
        "同一秒内创建备份目录 {} 失败（重名过多）",
        root.display()
    ))
}

/// SQL 字符串字面量：Windows 反斜杠在 SQLite 里是普通字符，但统一成正斜杠更稳。
pub(crate) fn sql_literal(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    format!("'{}'", raw.replace('\'', "''"))
}
