//! 数据库迁移护栏：迁移前一致性备份、错误分类（人话）、重启风暴抑制。
//!
//! 事故背景：设备上曾运行过“本机临时构建”的二进制（把 `shared.db` 迁到 version 7），
//! 随后装回只支持到 version 5 的正式版，启动直接退出且只打印
//! `Sqlx(Migrate(VersionMismatch(1)))`；healthz 连续 000、systemd 反复重启，
//! 用户侧完全无法判断发生了什么。
//!
//! 本模块负责：
//! 1. 在应用任何待执行迁移之前，用 `VACUUM INTO` 把 `shared.db` 与 `users/*.db`
//!    做一致性快照（不是复制文件，WAL 中的内容也会包含在快照里）；
//! 2. 把版本不匹配 / 权限 / 磁盘满 / 语法错误翻译成“原因 + 下一步”；
//! 3. 写启动失败标记，让下一次启动至少等待 20s，避免 systemd 重启风暴；
//! 4. 自愈“换行血缘不一致”：同一份库被 CRLF/LF 两种构建写过时，校验和只差换行，
//!    归一化记录后继续启动（真差异仍按原样拒绝）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha384};
use sqlx::migrate::{MigrateError, Migrator};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, SqliteConnection, SqlitePool};

use crate::config::Config;

/// 备份根目录名（位于 `DATA_DIR` 下）。
pub const BACKUP_DIR: &str = "backups";
/// 启动失败标记（位于 `DATA_DIR` 下），仅用于抑制重启风暴。
const FAILURE_MARKER: &str = ".startup_failure";
/// 两次启动尝试之间的最小间隔（秒）。systemd 默认 `RestartSec=100ms`，
/// 没有这个退避时 25 秒内可以重启上百次。
const STARTUP_BACKOFF_SECS: u64 = 20;

/// 迁移作用域：决定备份哪些文件、错误里显示哪个库。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// 共享库 `shared.db`：备份时连带 `users/*.db` 一起快照（用户库也可能随后迁移）。
    Shared,
    /// 单个用户库 `users/<uid>.db`：按需迁移时只备份该文件。
    User(i64),
}

impl Scope {
    fn is_shared(self) -> bool {
        matches!(self, Scope::Shared)
    }

    fn label(self) -> String {
        match self {
            Scope::Shared => "共享库".to_string(),
            Scope::User(uid) => format!("用户库 users/{uid}.db"),
        }
    }
}

/// 启动阶段的致命错误。`Display` 直接输出给人看的多行说明，
/// `main` 返回 `anyhow::Result` 时不会再把内部枚举原样打印出来。
#[derive(Clone)]
pub struct FatalStartup(pub String);

impl std::fmt::Display for FatalStartup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::fmt::Debug for FatalStartup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for FatalStartup {}

/// 迁移前的库状态（只读探测，不写库）。
#[derive(Debug, PartialEq, Eq)]
enum DbState {
    /// 还没有 `_sqlx_migrations`：全新库，没有可丢的数据，不备份。
    Fresh,
    /// 已应用迁移与程序内嵌的一致，无需迁移（也不备份）。
    UpToDate,
    /// 有本程序支持、但库里还没应用的迁移：迁移前必须备份。
    Pending { applied_max: i64, pending: Vec<i64> },
    /// 库里有本程序不认识（更高）的迁移：库比程序新，必须备份后拒绝启动。
    Newer {
        applied_max: i64,
        supported_max: i64,
    },
    /// 迁移记录 `success=0`：上次迁移中途失败，需要人工处理。
    Dirty { version: i64 },
}

fn data_dir(cfg: &Config) -> &Path {
    Path::new(&cfg.data_dir)
}

fn db_path_of(cfg: &Config, scope: Scope) -> PathBuf {
    match scope {
        Scope::Shared => data_dir(cfg).join("shared.db"),
        Scope::User(uid) => data_dir(cfg).join("users").join(format!("{uid}.db")),
    }
}

fn db_file_name(scope: Scope) -> String {
    match scope {
        Scope::Shared => "shared.db".to_string(),
        Scope::User(uid) => format!("users/{uid}.db"),
    }
}

pub fn backup_root(cfg: &Config) -> PathBuf {
    data_dir(cfg).join(BACKUP_DIR)
}

fn marker_path(cfg: &Config) -> PathBuf {
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
fn record_startup_failure(cfg: &Config, reason: &str) {
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

fn utc_stamp() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// 备份目录名必须是 `<UTC 时间戳>`，可选 `-N` 后缀（同一秒内多次备份）。
fn is_backup_dir_name(name: &str) -> bool {
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

fn create_backup_dir(root: &Path) -> Result<PathBuf, String> {
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
fn sql_literal(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    format!("'{}'", raw.replace('\'', "''"))
}

/// 用 `VACUUM INTO` 做一致性快照，并对产物跑 `PRAGMA integrity_check`。
///
/// sqlx 没有暴露 sqlite3 的在线 backup API，`VACUUM INTO` 是等价的
/// “在单个读事务里把整库写出一份新文件”的方式：源库处于 WAL 时，
/// WAL 中未 checkpoint 的数据也会被写进快照，不会出现“只拷主文件丢数据”。
async fn snapshot_db(src: &Path, dst: &Path) -> Result<(), String> {
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
async fn backup_now(cfg: &Config, scope: Scope) -> Result<PathBuf, String> {
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
fn prune_backups(cfg: &Config) {
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

// ---------------------------------------------------------------------------
// 迁移状态探测
// ---------------------------------------------------------------------------

async fn inspect(pool: &SqlitePool, migrator: &Migrator) -> Result<DbState, sqlx::Error> {
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await?;
    if exists == 0 {
        return Ok(DbState::Fresh);
    }

    let rows: Vec<(i64, i64)> =
        sqlx::query_as("SELECT version, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await?;
    if let Some((version, _)) = rows.iter().find(|(_, success)| *success == 0) {
        return Ok(DbState::Dirty { version: *version });
    }

    let applied: HashSet<i64> = rows.iter().map(|(v, _)| *v).collect();
    let supported_max = migrator.iter().map(|m| m.version).max().unwrap_or(0);
    let applied_max = applied.iter().copied().max().unwrap_or(0);

    if applied.iter().any(|v| !migrator.version_exists(*v)) {
        return Ok(DbState::Newer {
            applied_max,
            supported_max,
        });
    }

    let pending: Vec<i64> = migrator
        .iter()
        .map(|m| m.version)
        .filter(|v| !applied.contains(v))
        .collect();
    if pending.is_empty() {
        Ok(DbState::UpToDate)
    } else {
        Ok(DbState::Pending {
            applied_max,
            pending,
        })
    }
}

async fn max_applied(pool: &SqlitePool) -> Option<i64> {
    sqlx::query_scalar::<_, i64>("SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations")
        .fetch_one(pool)
        .await
        .ok()
}

// ---------------------------------------------------------------------------
// 换行血缘自愈（CRLF/LF 校验和）
// ---------------------------------------------------------------------------

/// 一次自愈扫描的结果。
#[derive(Default, Debug)]
struct RepairReport {
    /// 实际被改写的迁移版本
    normalized: Vec<i64>,
    /// 本次是否执行了扫描（`MIGRATION_REPAIR=off` 时为 false）
    attempted: bool,
    /// 连“换行变体”也解释不了的真实不一致
    remaining: Vec<i64>,
}

fn sha384(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = Sha384::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

/// 把换行统一成 CRLF；已经是 `\r\n` 的不会变成 `\r\r\n`。
fn crlf_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 16);
    let mut prev = 0u8;
    for &b in bytes {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        prev = b;
    }
    out
}

async fn integrity_ok(pool: &SqlitePool) -> Result<bool, sqlx::Error> {
    let verdict: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(pool)
        .await?;
    Ok(verdict == "ok")
}

fn repair_fatal(
    cfg: &Config,
    scope: Scope,
    stage: &str,
    detail: &str,
    backup: Option<&Path>,
) -> FatalStartup {
    if scope.is_shared() {
        record_startup_failure(cfg, &format!("{stage}: {detail}"));
    }
    let backup_line = match backup {
        Some(p) => format!("\n  本次备份：{}", p.display()),
        None => String::new(),
    };
    FatalStartup(format!(
        "迁移记录自愈（换行归一化）失败：{stage}，启动已中止。\n\n  \
         数据库文件：{path}{backup_line}\n  技术细节：{detail}\n\n\
         下一步：确认磁盘空间/权限后重试，或从 data/backups 恢复升级前的快照。",
        path = db_path_of(cfg, scope).display()
    ))
}

/// 对已应用（`success=1`）的迁移做“仅换行差异”归一化。
///
/// 判定非常保守：只有当库中校验和恰好等于“当前内嵌字节转 CRLF 后”的 SHA-384 时才改写；
/// 任何其它不一致都原样保留，交给 sqlx 报错（绝不掩盖真实内容差异）。
async fn repair_newline_checksums(
    cfg: &Config,
    pool: &SqlitePool,
    migrator: &Migrator,
    scope: Scope,
    mut backup: Option<PathBuf>,
) -> Result<(RepairReport, Option<PathBuf>), FatalStartup> {
    let mut report = RepairReport {
        attempted: true,
        ..Default::default()
    };
    if !cfg.migration_repair {
        report.attempted = false;
        return Ok((report, backup));
    }

    let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
        "SELECT version, checksum FROM _sqlx_migrations WHERE success = 1 ORDER BY version",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| {
        repair_fatal(
            cfg,
            scope,
            "读取迁移校验和失败",
            &e.to_string(),
            backup.as_deref(),
        )
    })?;

    let mut fixes: Vec<(i64, Vec<u8>)> = Vec::new();
    for (version, stored) in rows {
        let Some(migration) = migrator.iter().find(|m| m.version == version) else {
            continue; // 本程序不认识的迁移由 Newer 状态处理
        };
        let embedded = sha384(migration.sql.as_bytes());
        if stored == embedded {
            continue;
        }
        let crlf = sha384(&crlf_bytes(migration.sql.as_bytes()));
        if crlf != embedded && stored == crlf {
            fixes.push((version, embedded));
        } else {
            report.remaining.push(version);
        }
    }

    if fixes.is_empty() {
        return Ok((report, backup));
    }

    match integrity_ok(pool).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(repair_fatal(
                cfg,
                scope,
                "归一化前 integrity_check 未通过",
                "数据库可能已损坏，拒绝写入迁移记录",
                backup.as_deref(),
            ))
        }
        Err(e) => {
            return Err(repair_fatal(
                cfg,
                scope,
                "归一化前 integrity_check 执行失败",
                &e.to_string(),
                backup.as_deref(),
            ))
        }
    }

    if backup.is_none() {
        backup = Some(backup_or_fail(cfg, scope).await?);
    }
    let backup_path = backup.clone();

    let mut tx = pool.begin().await.map_err(|e| {
        repair_fatal(
            cfg,
            scope,
            "开启归一化事务失败",
            &e.to_string(),
            backup_path.as_deref(),
        )
    })?;
    for (version, checksum) in &fixes {
        let affected = sqlx::query(
            "UPDATE _sqlx_migrations SET checksum = ?1 WHERE version = ?2 AND success = 1",
        )
        .bind(checksum.as_slice())
        .bind(*version)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            repair_fatal(
                cfg,
                scope,
                "更新迁移校验和失败",
                &e.to_string(),
                backup_path.as_deref(),
            )
        })?
        .rows_affected();
        if affected != 1 {
            return Err(repair_fatal(
                cfg,
                scope,
                "更新迁移校验和失败",
                &format!("version {version} 影响行数 {affected}（期望 1）"),
                backup_path.as_deref(),
            ));
        }
        tracing::info!(
            version = *version,
            "migration v{}: 仅换行差异，已归一化校验和（CRLF→LF）",
            version
        );
    }
    tx.commit().await.map_err(|e| {
        repair_fatal(
            cfg,
            scope,
            "提交归一化事务失败",
            &e.to_string(),
            backup_path.as_deref(),
        )
    })?;
    report.normalized = fixes.iter().map(|(version, _)| *version).collect();

    match integrity_ok(pool).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(repair_fatal(
                cfg,
                scope,
                "归一化后 integrity_check 未通过",
                "请从备份恢复该数据库",
                backup_path.as_deref(),
            ))
        }
        Err(e) => {
            return Err(repair_fatal(
                cfg,
                scope,
                "归一化后 integrity_check 执行失败",
                &e.to_string(),
                backup_path.as_deref(),
            ))
        }
    }

    Ok((report, backup))
}

// ---------------------------------------------------------------------------
// 错误翻译
// ---------------------------------------------------------------------------

fn common_info(cfg: &Config, scope: Scope, db_version: i64, supported_max: i64) -> String {
    format!(
        "  数据库版本：{db_version}（迁移记录中的最高版本）\n  本程序支持的最高版本：{supported_max}\n  程序版本：{ver}\n  数据库文件：{path}\n  备份目录：{backups}",
        ver = env!("CARGO_PKG_VERSION"),
        path = db_path_of(cfg, scope).display(),
        backups = backup_root(cfg).display()
    )
}

fn newer_message(
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
fn sqlite_parts(err: &sqlx::Error) -> (Option<i64>, Option<String>) {
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
fn code_hint(code: Option<i64>) -> (&'static str, &'static str) {
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

fn execute_message(inner: &sqlx::Error, info: &str) -> String {
    let (code, message) = sqlite_parts(inner);
    let (cause, next) = code_hint(code);
    let detail = message.unwrap_or_else(|| inner.to_string());
    format!(
        "数据库迁移失败（{cause}），启动已中止。\n\n{info}\n  技术细节：{detail}\n下一步：{next}"
    )
}

fn explain(
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

fn backup_failure_message(cfg: &Config, scope: Scope, reason: &str) -> String {
    format!(
        "迁移前备份失败，已停止启动，未对数据库做任何修改。\n\n  \
         备份对象：{label}\n  备份目录：{root}\n  原因：{reason}\n\n\
         下一步：检查磁盘空间与 data 目录写权限，处理后重启；\
         备份保留份数可用 BORANLIB_DB_BACKUP_KEEP 调整（默认 5）。",
        label = scope.label(),
        root = backup_root(cfg).display()
    )
}

async fn backup_or_fail(cfg: &Config, scope: Scope) -> Result<PathBuf, FatalStartup> {
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
fn is_transient_migration_error(err: &MigrateError) -> bool {
    let MigrateError::Execute(inner) = err else {
        return false;
    };
    matches!(sqlite_parts(inner).0, Some(5) | Some(6) | Some(10))
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::migrate::{Migration, MigrationType};
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn tmp_dir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("oread-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_cfg(dir: &Path, keep: usize) -> Config {
        let mut cfg = Config::from_env();
        cfg.data_dir = dir.display().to_string();
        cfg.db_backup_keep = keep;
        cfg.migration_repair = true;
        cfg
    }

    /// 用指定 SQL 构造迁移集（校验和按字节计算，和 sqlx 一致）。
    fn migrator_with_sql(rows: &[(i64, &'static str)]) -> Migrator {
        let migrations: Vec<Migration> = rows
            .iter()
            .map(|(version, sql)| {
                Migration::new(
                    *version,
                    Cow::Borrowed("test migration"),
                    MigrationType::Simple,
                    Cow::Borrowed(sql),
                )
            })
            .collect();
        Migrator {
            migrations: Cow::Owned(migrations),
            ignore_missing: false,
            locking: true,
        }
    }

    async fn stored_checksum(pool: &SqlitePool, version: i64) -> Vec<u8> {
        sqlx::query_scalar::<_, Vec<u8>>("SELECT checksum FROM _sqlx_migrations WHERE version = ?1")
            .bind(version)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn backup_dir_count(cfg: &Config) -> usize {
        std::fs::read_dir(backup_root(cfg))
            .map(|entries| entries.flatten().filter(|e| e.path().is_dir()).count())
            .unwrap_or(0)
    }

    async fn open_pool(path: &Path) -> SqlitePool {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        SqlitePool::connect_with(opts).await.unwrap()
    }

    async fn create_migrations_table(pool: &SqlitePool, rows: &[(i64, bool, Vec<u8>)]) {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _sqlx_migrations (\n  version BIGINT PRIMARY KEY,\n  \
             description TEXT NOT NULL,\n  installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,\n  \
             success BOOLEAN NOT NULL,\n  checksum BLOB NOT NULL,\n  execution_time BIGINT NOT NULL\n)",
        )
        .execute(pool)
        .await
        .unwrap();
        for (version, success, checksum) in rows {
            sqlx::query(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
                 VALUES (?1, 'test', ?2, ?3, 0)",
            )
            .bind(*version)
            .bind(if *success { 1i64 } else { 0i64 })
            .bind(checksum.as_slice())
            .execute(pool)
            .await
            .unwrap();
        }
    }

    fn migrator(versions: &[i64]) -> Migrator {
        let migrations: Vec<Migration> = versions
            .iter()
            .map(|v| {
                Migration::new(
                    *v,
                    Cow::Borrowed("test migration"),
                    MigrationType::Simple,
                    Cow::Borrowed("SELECT 1;"),
                )
            })
            .collect();
        Migrator {
            migrations: Cow::Owned(migrations),
            ignore_missing: false,
            locking: true,
        }
    }

    /// 备份必须包含 WAL 中未 checkpoint 的数据，且产物能过 integrity_check。
    #[tokio::test]
    async fn backup_is_consistent_with_wal() {
        let dir = tmp_dir("backup-wal");
        let cfg = test_cfg(&dir, 5);
        let src = dir.join("shared.db");
        let pool = open_pool(&src).await;
        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (x TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        let blob = "x".repeat(2048);
        for _ in 0..60 {
            sqlx::query("INSERT INTO t (x) VALUES (?1)")
                .bind(&blob)
                .execute(&pool)
                .await
                .unwrap();
        }
        // 前提：数据确实还留在 WAL 里（只拷主文件会丢行）。
        let wal = src.with_extension("db-wal");
        let wal_len = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
        assert!(wal_len > 0, "测试前提：WAL 中应有未合并的数据");

        let backup_dir = backup_now(&cfg, Scope::Shared).await.unwrap();
        let copy = backup_dir.join("shared.db");
        assert!(copy.is_file(), "备份文件应存在");

        let opts = SqliteConnectOptions::new().filename(&copy).read_only(true);
        let mut conn = SqliteConnection::connect_with(&opts).await.unwrap();
        let verdict: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(verdict, "ok");
        assert_eq!(rows, 60, "WAL 中的行也必须进入快照");
        let _ = conn.close().await;
        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 保留策略：造 7 份后只剩最近 5 份，且不误删非备份目录。
    #[test]
    fn retention_keeps_newest_five() {
        let dir = tmp_dir("retention");
        let cfg = test_cfg(&dir, 5);
        let root = backup_root(&cfg);
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..7u32 {
            std::fs::create_dir(root.join(format!("20261104T00000{i}Z"))).unwrap();
        }
        std::fs::create_dir(root.join("manual-keep")).unwrap();

        prune_backups(&cfg);

        let mut kept: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        kept.sort();
        assert_eq!(kept.len(), 6, "5 份备份 + 1 个人工目录");
        for i in 0..2u32 {
            assert!(
                !root.join(format!("20261104T00000{i}Z")).exists(),
                "最旧的第 {i} 份应被清理"
            );
        }
        for i in 2..7u32 {
            assert!(
                root.join(format!("20261104T00000{i}Z")).exists(),
                "最近的第 {i} 份应保留"
            );
        }
        assert!(root.join("manual-keep").exists(), "非备份目录不得删除");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 状态探测：库比程序新 / 有待应用迁移 / 全新库。
    #[tokio::test]
    async fn inspect_detects_newer_pending_and_fresh() {
        let dir = tmp_dir("inspect");
        let db = dir.join("shared.db");
        let pool = open_pool(&db).await;

        assert_eq!(
            inspect(&pool, &migrator(&[1, 2])).await.unwrap(),
            DbState::Fresh
        );

        create_migrations_table(
            &pool,
            &[
                (1, true, vec![0u8; 48]),
                (2, true, vec![0u8; 48]),
                (3, true, vec![0u8; 48]),
            ],
        )
        .await;
        assert_eq!(
            inspect(&pool, &migrator(&[1, 2])).await.unwrap(),
            DbState::Newer {
                applied_max: 3,
                supported_max: 2
            }
        );

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);

        let dir = tmp_dir("inspect-pending");
        let db = dir.join("shared.db");
        let pool = open_pool(&db).await;
        create_migrations_table(&pool, &[(1, true, vec![0u8; 48])]).await;
        assert_eq!(
            inspect(&pool, &migrator(&[1, 2])).await.unwrap(),
            DbState::Pending {
                applied_max: 1,
                pending: vec![2]
            }
        );
        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真实迁移入口：库比程序新时给出人话错误，且备份目录里确实有一份快照。
    #[tokio::test]
    async fn newer_database_aborts_with_backup() {
        let dir = tmp_dir("newer-abort");
        let cfg = test_cfg(&dir, 5);
        let db = dir.join("shared.db");
        let pool = open_pool(&db).await;
        create_migrations_table(
            &pool,
            &[
                (1, true, vec![0u8; 48]),
                (2, true, vec![0u8; 48]),
                (3, true, vec![0u8; 48]),
            ],
        )
        .await;

        let err = migrate_bucket(&cfg, &pool, &migrator(&[1, 2]), Scope::Shared)
            .await
            .unwrap_err();
        assert!(err.0.contains("数据库版本高于当前程序"), "{}", err.0);
        assert!(err.0.contains("数据库版本：3"), "{}", err.0);
        assert!(err.0.contains("本程序支持的最高版本：2"), "{}", err.0);
        assert!(
            !err.0.contains("VersionMismatch"),
            "不得暴露裸错误：{}",
            err.0
        );

        let backups: Vec<_> = std::fs::read_dir(backup_root(&cfg))
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(backups.len(), 1, "应恰好产生一份备份");
        let copy = backups[0].path().join("shared.db");
        assert!(copy.is_file());
        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 正常启动（无待应用迁移）不产生多余备份。
    #[tokio::test]
    async fn up_to_date_does_not_backup() {
        let dir = tmp_dir("uptodate");
        let cfg = test_cfg(&dir, 5);
        let db = dir.join("shared.db");
        let pool = open_pool(&db).await;

        let m = migrator(&[1]);
        let checksum = m.iter().next().unwrap().checksum.to_vec();
        create_migrations_table(&pool, &[(1, true, checksum)]).await;
        migrate_bucket(&cfg, &pool, &m, Scope::Shared)
            .await
            .unwrap();

        assert!(
            !backup_root(&cfg).exists(),
            "无待应用迁移时不应创建备份目录"
        );
        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 版本不匹配 / 权限 / 磁盘满都要翻译成人话，不打印裸枚举。
    #[test]
    fn errors_are_human_readable() {
        let dir = tmp_dir("messages");
        let cfg = test_cfg(&dir, 5);

        let mismatch = explain(
            &MigrateError::VersionMismatch(1),
            &cfg,
            Scope::Shared,
            7,
            7,
            None,
        );
        assert!(mismatch.contains("数据库迁移记录与当前程序不一致"));
        assert!(mismatch.contains("校验和"));
        assert!(mismatch.contains("backups"));
        assert!(!mismatch.contains("VersionMismatch"));

        let newer = newer_message(&cfg, Scope::Shared, 7, 5, None);
        assert!(newer.contains("数据库已被更新版本的程序写入"));
        assert!(newer.contains(env!("CARGO_PKG_VERSION")));

        let info = common_info(&cfg, Scope::Shared, 7, 5);
        assert!(info.contains("本程序支持的最高版本：5"));
        assert!(info.contains("数据库版本：7"));

        // 磁盘满 / 只读 / 语法错误：每条都要有原因与下一步。
        let (cause, next) = code_hint(Some(13));
        assert!(cause.contains("磁盘空间不足"));
        assert!(next.contains("BORANLIB_DB_BACKUP_KEEP"));
        let (cause, _) = code_hint(Some(8));
        assert!(cause.contains("没有写权限"));
        let (cause, _) = code_hint(Some(1));
        assert!(cause.contains("语法"));

        let msg = execute_message(&sqlx::Error::Protocol("synthetic".into()), &info);
        assert!(msg.contains("数据库迁移失败"));
        assert!(msg.contains("技术细节"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 1) 库里是 CRLF 哈希、内嵌文件是 LF：启动成功、记录被改写、第二次启动无写入。
    #[tokio::test]
    async fn crlf_only_mismatch_is_normalized_once() {
        let dir = tmp_dir("repair-crlf");
        let cfg = test_cfg(&dir, 5);
        let db = dir.join("shared.db");
        let pool = open_pool(&db).await;

        let sql = "CREATE TABLE t (x TEXT);\nINSERT INTO t (x) VALUES ('a');\n";
        let m = migrator_with_sql(&[(1, sql)]);
        let lf = sha384(sql.as_bytes());
        let crlf = sha384(&crlf_bytes(sql.as_bytes()));
        assert_ne!(lf, crlf, "测试前提：LF/CRLF 哈希应不同");
        create_migrations_table(&pool, &[(1, true, crlf)]).await;

        migrate_bucket(&cfg, &pool, &m, Scope::Shared)
            .await
            .unwrap();
        assert_eq!(stored_checksum(&pool, 1).await, lf, "应改写成 LF 哈希");
        assert_eq!(backup_dir_count(&cfg), 1, "归一化前应有一份快照");

        // 第二次启动：不再改库、不再产生新备份。
        let before = stored_checksum(&pool, 1).await;
        migrate_bucket(&cfg, &pool, &m, Scope::Shared)
            .await
            .unwrap();
        assert_eq!(stored_checksum(&pool, 1).await, before);
        assert_eq!(backup_dir_count(&cfg), 1, "第二次启动不应新建备份");
        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2) 内容真被改（非换行）：仍然拒绝启动、错误可读、不写库。
    #[tokio::test]
    async fn real_content_change_is_still_rejected() {
        let dir = tmp_dir("repair-tampered");
        let cfg = test_cfg(&dir, 5);
        let db = dir.join("shared.db");
        let pool = open_pool(&db).await;

        let sql = "CREATE TABLE t (x TEXT);\n";
        let m = migrator_with_sql(&[(1, sql)]);
        let tampered = sha384(b"CREATE TABLE t (x TEXT);\n-- tampered\n");
        assert_ne!(tampered, sha384(&crlf_bytes(sql.as_bytes())));
        create_migrations_table(&pool, &[(1, true, tampered.clone())]).await;

        let err = migrate_bucket(&cfg, &pool, &m, Scope::Shared)
            .await
            .unwrap_err();
        assert!(
            err.0.contains("数据库迁移记录与当前程序不一致"),
            "{}",
            err.0
        );
        assert!(err.0.contains("已尝试“换行归一化”"), "{}", err.0);
        assert_eq!(stored_checksum(&pool, 1).await, tampered, "真差异不得写库");
        assert_eq!(backup_dir_count(&cfg), 0, "真差异不应产生备份");
        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3) WAL 模式下归一化 + 迁移前备份的一致性。
    #[tokio::test]
    async fn repair_in_wal_keeps_backup_consistent() {
        let dir = tmp_dir("repair-wal");
        let cfg = test_cfg(&dir, 5);
        let db = dir.join("shared.db");
        let pool = open_pool(&db).await;
        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE data (x TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        let blob = "x".repeat(1024);
        for _ in 0..30 {
            sqlx::query("INSERT INTO data (x) VALUES (?1)")
                .bind(&blob)
                .execute(&pool)
                .await
                .unwrap();
        }
        let wal_len = std::fs::metadata(db.with_extension("db-wal"))
            .map(|m| m.len())
            .unwrap_or(0);
        assert!(wal_len > 0, "测试前提：WAL 中应有未合并数据");

        let sql = "CREATE TABLE t (x TEXT);\n";
        let m = migrator_with_sql(&[(1, sql)]);
        let crlf = sha384(&crlf_bytes(sql.as_bytes()));
        create_migrations_table(&pool, &[(1, true, crlf)]).await;

        migrate_bucket(&cfg, &pool, &m, Scope::Shared)
            .await
            .unwrap();
        assert_eq!(stored_checksum(&pool, 1).await, sha384(sql.as_bytes()));
        assert_eq!(backup_dir_count(&cfg), 1);

        let backups: Vec<PathBuf> = std::fs::read_dir(backup_root(&cfg))
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.path())
            .collect();
        let copy = backups[0].join("shared.db");
        let opts = SqliteConnectOptions::new().filename(&copy).read_only(true);
        let mut conn = SqliteConnection::connect_with(&opts).await.unwrap();
        let verdict: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM data")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(verdict, "ok");
        assert_eq!(rows, 30, "WAL 中的数据必须进入一致性快照");
        let _ = conn.close().await;
        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 4) 开关 off：退回“只报错不修”。
    #[tokio::test]
    async fn repair_switch_off_only_errors() {
        let dir = tmp_dir("repair-off");
        let mut cfg = test_cfg(&dir, 5);
        cfg.migration_repair = false;
        let db = dir.join("shared.db");
        let pool = open_pool(&db).await;

        let sql = "CREATE TABLE t (x TEXT);\n";
        let m = migrator_with_sql(&[(1, sql)]);
        let crlf = sha384(&crlf_bytes(sql.as_bytes()));
        create_migrations_table(&pool, &[(1, true, crlf.clone())]).await;

        let err = migrate_bucket(&cfg, &pool, &m, Scope::Shared)
            .await
            .unwrap_err();
        assert!(
            err.0.contains("数据库迁移记录与当前程序不一致"),
            "{}",
            err.0
        );
        assert!(!err.0.contains("已尝试“换行归一化”"), "{}", err.0);
        assert_eq!(stored_checksum(&pool, 1).await, crlf, "off 时不得写库");
        assert_eq!(backup_dir_count(&cfg), 0);
        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
