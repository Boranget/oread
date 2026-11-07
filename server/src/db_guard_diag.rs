//! db_guard 的迁移错误翻译（move-only 从 db_guard.rs 拆出）。

use super::*;

// ---------------------------------------------------------------------------
// 错误翻译
// ---------------------------------------------------------------------------

pub(crate) fn common_info(
    cfg: &Config,
    scope: Scope,
    db_version: i64,
    supported_max: i64,
) -> String {
    format!(
        "  数据库版本：{db_version}（迁移记录中的最高版本）\n  本程序支持的最高版本：{supported_max}\n  程序版本：{ver}\n  数据库文件：{path}\n  备份目录：{backups}",
        ver = env!("CARGO_PKG_VERSION"),
        path = db_path_of(cfg, scope).display(),
        backups = backup_root(cfg).display()
    )
}

pub(crate) fn newer_message(
    cfg: &Config,
    scope: Scope,
    applied_max: i64,
    supported_max: i64,
    backup: Option<&Path>,
) -> String {
    let info = common_info(cfg, scope, applied_max, supported_max);
    let backup_line = match backup {
        Some(p) if p.is_dir() => format!("\n  本次已备份到：{}", p.display()),
        _ => String::new(),
    };
    format!(
        "数据库版本高于当前程序，无法安全启动。\n\n{info}{backup_line}\n\n\
         说明：数据库已被更新版本的程序写入，本版本无法安全操作它（继续启动可能损坏数据）。\n\
         下一步：\n  \
         1. 升级到支持 version {applied_max} 或更高版本的程序（当前 {ver} 最高支持 {supported_max}）；\n  \
         2. 或从备份回滚：停止服务后，用 backups/<时间戳>/{file} 覆盖回 {path}（升级前的快照都在 {backups}）。",
        ver = env!("CARGO_PKG_VERSION"),
        file = db_file_name(scope),
        path = db_path_of(cfg, scope).display(),
        backups = backup_root(cfg).display()
    )
}

/// sqlite 的 `DatabaseError::code()` 给的是扩展错误码，取低 8 位得到主错误码。
pub(crate) fn sqlite_parts(err: &sqlx::Error) -> (Option<i64>, Option<String>) {
    match err {
        sqlx::Error::Database(db) => (
            db.code()
                .and_then(|c| c.parse::<i64>().ok())
                .map(|c| c & 0xff),
            Some(db.message().to_string()),
        ),
        _ => (None, None),
    }
}

/// sqlite 主错误码 →（人话原因，可执行下一步）。
pub(crate) fn code_hint(code: Option<i64>) -> (&'static str, &'static str) {
    match code {
        Some(13) => (
            "磁盘空间不足（SQLite 13：database or disk is full）",
            "清理 data/backups 或其它占用空间后重启；备份保留份数由 BORANLIB_DB_BACKUP_KEEP 控制。",
        ),
        Some(8) | Some(3) => (
            "数据库文件没有写权限（SQLite READONLY/PERM）",
            "确认 DATA_DIR 与数据库文件对运行用户可读写（chown/chmod），并检查 systemd 的 ProtectSystem/ReadWritePaths。",
        ),
        Some(10) => (
            "磁盘/文件系统 I/O 出错（SQLite IOERR）",
            "检查磁盘健康（eMMC 寿命/坏块、网络盘掉线），再从 data/backups 恢复。",
        ),
        Some(5) | Some(6) => (
            "数据库被其它连接占用（SQLite BUSY/LOCKED）",
            "确认旧服务进程已退出、没有第二个进程在用同一 DATA_DIR，然后重启。",
        ),
        Some(11) | Some(26) => (
            "数据库文件损坏或不是 SQLite 文件（SQLITE_CORRUPT/NOTADB）",
            "停止服务后从 data/backups 恢复快照（覆盖前先保留现场文件）。",
        ),
        Some(1) => (
            "迁移脚本执行失败（SQLite 语法/结构错误）",
            "先不要反复重启：从 data/backups 恢复，或检查迁移 SQL 与现有表结构是否冲突。",
        ),
        Some(19) => (
            "迁移脚本违反约束（SQLite CONSTRAINT）",
            "从 data/backups 恢复；若确认是数据问题，手工修正后再启动。",
        ),
        _ => (
            "迁移执行失败",
            "查看下方技术细节；必要时从 data/backups 恢复。",
        ),
    }
}

pub(crate) fn execute_message(inner: &sqlx::Error, info: &str) -> String {
    let (code, message) = sqlite_parts(inner);
    let (cause, next) = code_hint(code);
    let detail = message.unwrap_or_else(|| inner.to_string());
    format!(
        "数据库迁移失败（{cause}），启动已中止。\n\n{info}\n  技术细节：{detail}\n下一步：{next}"
    )
}

pub(crate) fn explain(
    err: &MigrateError,
    cfg: &Config,
    scope: Scope,
    db_version: i64,
    supported_max: i64,
    repair: Option<&RepairReport>,
) -> String {
    let info = common_info(cfg, scope, db_version, supported_max);
    match err {
        MigrateError::VersionMismatch(version) => {
            let repair_note = match repair {
                Some(r) if r.attempted && r.remaining.contains(version) => format!(
                    "\n本次启动已尝试“换行归一化”（按 CRLF 变体哈希比对），但第 {version} 条的内容仍与程序内嵌文件不一致——\
                     这不是换行问题（迁移文件可能被真正改过，或库被其它程序写过），已停止启动。"
                ),
                _ => String::new(),
            };
            format!(
                "数据库迁移记录与当前程序不一致，无法安全启动。\n\n{info}\n  \
                 冲突迁移：第 {version} 条（校验和不同）\n\n\
                 说明：数据库里记录的迁移内容与当前程序内嵌的迁移文件不同——\
                 通常是数据库曾被“临时构建/改过迁移文件”的程序写入过。{repair_note}\n\
                 下一步：\n  \
                 1. 换回与数据库匹配的程序版本（或升级到能安全接管该库的新版本）；\n  \
                 2. 无法确定时，从备份目录恢复升级前的快照：停止服务后，把 backups/<时间戳>/{file} 覆盖回 {path}。",
                file = db_file_name(scope),
                path = db_path_of(cfg, scope).display()
            )
        }
        MigrateError::VersionMissing(version) | MigrateError::VersionNotPresent(version) => {
            format!(
                "数据库包含当前程序没有的迁移记录（第 {version} 条），无法安全启动。\n\n{info}\n\n\
                 说明：数据库已被更新版本（或其它分支）的程序写入，本版本无法安全操作它。\n\
                 下一步：\n  \
                 1. 升级到能识别第 {version} 条迁移的程序版本；\n  \
                 2. 或从 backups 恢复升级前的快照：停止服务后，把 backups/<时间戳>/{file} 覆盖回 {path}。",
                file = db_file_name(scope),
                path = db_path_of(cfg, scope).display()
            )
        }
        MigrateError::VersionTooNew(version, latest) => format!(
            "数据库里第 {version} 条迁移比本程序已有的最新迁移（第 {latest} 条）还新，无法安全启动。\n\n{info}\n\n\
             说明：数据库已被更新版本的程序写入，本版本无法安全操作它。\n\
             下一步：升级程序，或从 backups 恢复升级前的快照。"
        ),
        MigrateError::VersionTooOld(version, latest) => format!(
            "迁移顺序冲突：第 {version} 条迁移比数据库中已应用的最高版本（第 {latest} 条）还旧，无法安全启动。\n\n{info}\n\n\
             说明：程序内嵌的迁移文件与数据库里的迁移顺序不一致，继续执行可能重复建表。\n\
             下一步：换回生成该数据库的程序版本，或从 backups 恢复升级前的快照。"
        ),
        MigrateError::Dirty(version) => format!(
            "数据库迁移未完整完成（第 {version} 条处于 dirty 状态），无法安全启动。\n\n{info}\n\n\
             说明：上次迁移在中途失败，库里既可能已有新表、也可能只改了一半。\n\
             下一步：从 backups 恢复升级前的快照；或人工检查 `_sqlx_migrations` 中 version={version} 的 \
             success=0 记录并修正后重启。"
        ),
        MigrateError::Execute(inner) => execute_message(inner, &info),
        _ => format!(
            "数据库迁移失败，启动已中止。\n\n{info}\n  技术细节：{err}\n\
             下一步：查看技术细节；必要时从 backups 恢复升级前的快照。"
        ),
    }
}

// ---------------------------------------------------------------------------
// 对外主流程
// ---------------------------------------------------------------------------

pub(crate) fn backup_failure_message(cfg: &Config, scope: Scope, reason: &str) -> String {
    format!(
        "迁移前备份失败，已停止启动，未对数据库做任何修改。\n\n  \
         备份对象：{label}\n  备份目录：{root}\n  原因：{reason}\n\n\
         下一步：检查磁盘空间与 data 目录写权限，处理后重启；\
         备份保留份数可用 BORANLIB_DB_BACKUP_KEEP 调整（默认 5）。",
        label = scope.label(),
        root = backup_root(cfg).display()
    )
}

pub(crate) async fn backup_or_fail(cfg: &Config, scope: Scope) -> Result<PathBuf, FatalStartup> {
    match backup_now(cfg, scope).await {
        Ok(dir) => {
            tracing::info!(
                dir = %dir.display(),
                scope = %scope.label(),
                "迁移前一致性备份完成（VACUUM INTO + integrity_check）"
            );
            Ok(dir)
        }
        Err(reason) => {
            tracing::error!(scope = %scope.label(), reason = %reason, "迁移前备份失败");
            if scope.is_shared() {
                record_startup_failure(cfg, &reason);
            }
            Err(FatalStartup(backup_failure_message(cfg, scope, &reason)))
        }
    }
}

/// 迁移过程中的暂时性 SQLite 错误：`10=IOERR`（网络盘抖动/坏块重试）、
/// `5/6=BUSY/LOCKED`（旧连接尚未释放）。磁盘满（13）、只读（8）、语法（1）
/// 重试也不会好转，直接失败并给出可执行建议。
pub(crate) fn is_transient_migration_error(err: &MigrateError) -> bool {
    let MigrateError::Execute(inner) = err else {
        return false;
    };
    matches!(sqlite_parts(inner).0, Some(5) | Some(6) | Some(10))
}
