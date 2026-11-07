//! db_guard 的迁移状态探测与换行自愈（move-only 从 db_guard.rs 拆出）。

use super::*;

// ---------------------------------------------------------------------------
// 迁移状态探测
// ---------------------------------------------------------------------------

pub(crate) async fn inspect(
    pool: &SqlitePool,
    migrator: &Migrator,
) -> Result<DbState, sqlx::Error> {
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

pub(crate) async fn max_applied(pool: &SqlitePool) -> Option<i64> {
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
pub(crate) struct RepairReport {
    /// 实际被改写的迁移版本
    pub(crate) normalized: Vec<i64>,
    /// 本次是否执行了扫描（`MIGRATION_REPAIR=off` 时为 false）
    pub(crate) attempted: bool,
    /// 连“换行变体”也解释不了的真实不一致
    pub(crate) remaining: Vec<i64>,
}

pub(crate) fn sha384(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = Sha384::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

/// 把换行统一成 CRLF；已经是 `\r\n` 的不会变成 `\r\r\n`。
pub(crate) fn crlf_bytes(bytes: &[u8]) -> Vec<u8> {
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

pub(crate) async fn integrity_ok(pool: &SqlitePool) -> Result<bool, sqlx::Error> {
    let verdict: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(pool)
        .await?;
    Ok(verdict == "ok")
}

pub(crate) fn repair_fatal(
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
pub(crate) async fn repair_newline_checksums(
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
