//! db_guard 的一致性快照与备份轮转（move-only 从 db_guard.rs 拆出）。

use super::*;

/// 用 `VACUUM INTO` 做一致性快照，并对产物跑 `PRAGMA integrity_check`。
///
/// sqlx 没有暴露 sqlite3 的在线 backup API，`VACUUM INTO` 是等价的
/// “在单个读事务里把整库写出一份新文件”的方式：源库处于 WAL 时，
/// WAL 中未 checkpoint 的数据也会被写进快照，不会出现“只拷主文件丢数据”。
pub(crate) async fn snapshot_db(src: &Path, dst: &Path) -> Result<(), String> {
    let src_opts = SqliteConnectOptions::new()
        .filename(src)
        .busy_timeout(Duration::from_secs(10));
    let mut conn = SqliteConnection::connect_with(&src_opts)
        .await
        .map_err(|e| format!("打开源库 {} 失败：{e}", src.display()))?;
    let sql = format!("VACUUM INTO {}", sql_literal(dst));
    let result = sqlx::query(&sql).execute(&mut conn).await;
    let _ = conn.close().await;
    result.map_err(|e| format!("一致性快照（VACUUM INTO）失败：{e}"))?;

    let dst_opts = SqliteConnectOptions::new().filename(dst).read_only(true);
    let mut check = SqliteConnection::connect_with(&dst_opts)
        .await
        .map_err(|e| format!("打开备份文件 {} 失败：{e}", dst.display()))?;
    let verdict: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&mut check)
        .await
        .map_err(|e| format!("备份文件完整性检查失败：{e}"))?;
    let _ = check.close().await;
    if verdict != "ok" {
        return Err(format!("备份文件未通过 integrity_check：{verdict}"));
    }
    Ok(())
}

/// 立即创建一份备份；返回实际备份目录。
pub(crate) async fn backup_now(cfg: &Config, scope: Scope) -> Result<PathBuf, String> {
    let root = backup_root(cfg);
    let dir = create_backup_dir(&root)?;

    let mut sources: Vec<PathBuf> = Vec::new();
    match scope {
        Scope::Shared => {
            let shared = db_path_of(cfg, Scope::Shared);
            if shared.is_file() {
                sources.push(shared);
            }
            if let Ok(entries) = std::fs::read_dir(data_dir(cfg).join("users")) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().is_some_and(|ext| ext == "db") {
                        sources.push(path);
                    }
                }
            }
        }
        Scope::User(_) => {
            let path = db_path_of(cfg, scope);
            if path.is_file() {
                sources.push(path);
            }
        }
    }

    if sources.is_empty() {
        // 没有可备份的文件（例如全新库）：不留空目录，避免“多余备份”。
        let _ = std::fs::remove_dir_all(&dir);
        return Ok(dir);
    }

    for src in &sources {
        let rel = src
            .strip_prefix(data_dir(cfg))
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| PathBuf::from(src.file_name().unwrap_or_default()));
        let dst = dir.join(rel);
        if let Some(parent) = dst.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(format!("创建备份子目录 {} 失败：{e}", parent.display()));
            }
        }
        if let Err(e) = snapshot_db(src, &dst).await {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(format!("快照 {} 失败：{e}", src.display()));
        }
    }

    prune_backups(cfg);
    Ok(dir)
}

/// 只保留最近 `cfg.db_backup_keep` 份备份（按目录名时间戳倒序）。
/// 仅删除形如 `<UTC 时间戳>` 的目录，人工放进去的东西不动。
pub(crate) fn prune_backups(cfg: &Config) {
    let root = backup_root(cfg);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| is_backup_dir_name(name))
        .collect();
    names.sort();
    names.reverse(); // 最新在前
    for name in names.into_iter().skip(cfg.db_backup_keep.max(1)) {
        let path = root.join(&name);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => tracing::info!(removed = %path.display(), "清理旧备份"),
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "清理旧备份失败"),
        }
    }
}
