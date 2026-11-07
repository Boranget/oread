//! db_guard 的迁移入口（move-only 从 db_guard.rs 拆出）。

use super::*;

/// 迁移入口：探测状态 → 必要时备份 → 执行迁移 → 失败时给出人话错误。
///
/// `scope=Shared` 时的失败会额外写启动失败标记，供下次启动退避。
pub async fn migrate_bucket(
    cfg: &Config,
    pool: &SqlitePool,
    migrator: &Migrator,
    scope: Scope,
) -> Result<(), FatalStartup> {
    let state = inspect(pool, migrator).await.map_err(|e| {
        let msg = format!(
            "读取数据库迁移状态失败（{label}），已停止启动，未做任何修改。\n\n  \
             数据库文件：{path}\n  原因：{e}\n\n\
             下一步：检查文件权限与磁盘空间后重试。",
            label = scope.label(),
            path = db_path_of(cfg, scope).display()
        );
        if scope.is_shared() {
            record_startup_failure(cfg, &e.to_string());
        }
        FatalStartup(msg)
    })?;

    let supported_max = migrator.iter().map(|m| m.version).max().unwrap_or(0);
    let mut backup_dir: Option<PathBuf> = None;

    match &state {
        DbState::Fresh | DbState::UpToDate => {
            // 无待应用迁移：不创建备份（避免每次正常启动都堆一份）。
        }
        DbState::Pending {
            applied_max,
            pending,
        } => {
            tracing::info!(
                db = %db_path_of(cfg, scope).display(),
                applied_max,
                pending = ?pending,
                "检测到待应用迁移，先做一致性备份"
            );
            backup_dir = Some(backup_or_fail(cfg, scope).await?);
        }
        DbState::Newer {
            applied_max,
            supported_max,
        } => {
            // 库比程序新：绝不尝试迁移，但先留一份一致快照再退出。
            let backup = backup_or_fail(cfg, scope).await?;
            let message = newer_message(cfg, scope, *applied_max, *supported_max, Some(&backup));
            tracing::error!(db = %db_path_of(cfg, scope).display(), %message, "数据库版本高于当前程序，拒绝启动");
            if scope.is_shared() {
                record_startup_failure(
                    cfg,
                    &format!("database version {applied_max} > supported {supported_max}"),
                );
            }
            return Err(FatalStartup(message));
        }
        DbState::Dirty { version } => {
            let message = explain(
                &MigrateError::Dirty(*version),
                cfg,
                scope,
                *version,
                supported_max,
                None,
            );
            tracing::error!(db = %db_path_of(cfg, scope).display(), %message, "数据库迁移处于 dirty 状态，拒绝启动");
            if scope.is_shared() {
                record_startup_failure(cfg, &format!("dirty migration {version}"));
            }
            return Err(FatalStartup(message));
        }
    }

    // 执行迁移前先处理“仅换行差异”的校验和（CRLF/LF 血缘）。
    // 需要写入时会先走同一套一致性备份；备份失败即中止。
    let repair = if matches!(state, DbState::UpToDate | DbState::Pending { .. }) {
        let (report, _) =
            repair_newline_checksums(cfg, pool, migrator, scope, backup_dir.take()).await?;
        report
    } else {
        RepairReport::default()
    };

    // 暂时性 I/O/BUSY 错误最多重试 3 次（约 0.5s、1s 退避）；磁盘满/只读等直接失败。
    let mut retries = 0u32;
    let run_result = loop {
        match migrator.run(pool).await {
            Ok(()) => break Ok(()),
            Err(err) if retries < 2 && is_transient_migration_error(&err) => {
                retries += 1;
                let delay = Duration::from_millis(250u64 << retries);
                tracing::warn!(
                    attempt = retries,
                    delay_ms = delay.as_millis() as u64,
                    error = %err,
                    "数据库迁移遇到暂时性错误，退避后重试"
                );
                tokio::time::sleep(delay).await;
            }
            Err(err) => break Err(err),
        }
    };

    match run_result {
        Ok(()) => {
            if !repair.normalized.is_empty() {
                tracing::info!(
                    count = repair.normalized.len(),
                    versions = ?repair.normalized,
                    "本次归一化 {} 条迁移记录",
                    repair.normalized.len()
                );
            }
            Ok(())
        }
        Err(err) => {
            let db_version = max_applied(pool).await.unwrap_or(0);
            let message = explain(&err, cfg, scope, db_version, supported_max, Some(&repair));
            tracing::error!(
                db = %db_path_of(cfg, scope).display(),
                %message,
                "数据库迁移失败，启动中止"
            );
            if scope.is_shared() {
                record_startup_failure(cfg, &err.to_string());
            }
            Err(FatalStartup(message))
        }
    }
}
