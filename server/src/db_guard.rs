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
pub(crate) enum DbState {
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

#[path = "db_guard_paths.rs"]
mod paths;
pub use paths::*;
#[path = "db_guard_backup.rs"]
mod backup;
pub(crate) use backup::*;
#[path = "db_guard_repair.rs"]
mod repair;
pub(crate) use repair::*;
#[path = "db_guard_diag.rs"]
mod diag;
pub(crate) use diag::*;
#[path = "db_guard_migrate.rs"]
mod migrate;
pub use migrate::*;

#[cfg(test)]
#[path = "db_guard_tests.rs"]
mod tests;
