use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::config::Config;
use crate::error::AppResult;

/// 内存优先的 SQLite 调优：WAL + NORMAL 同步 + 8MB 页缓存 + 禁用 mmap，
/// 并设置 5s busy_timeout（多连接池并发下避免立即 SQLITE_BUSY）。
/// 8MB 是每个「打开着的数据库文件」的上限，配合用户池 LRU（≤8 个）仍受控。
async fn apply_pragma(pool: &SqlitePool) -> AppResult<()> {
    sqlx::query(
        // 新库在首次建表前就切到增量回收；已有库需显式 `DB_VACUUM_CONVERT=1` 才做一次性转换。
        "PRAGMA auto_vacuum=INCREMENTAL; \
         PRAGMA journal_mode=WAL; \
         PRAGMA synchronous=NORMAL; \
         PRAGMA cache_size=-8000; \
         PRAGMA mmap_size=0; \
         PRAGMA busy_timeout=5000;",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// 把已有库转成 `auto_vacuum=INCREMENTAL`。**会触发一次整库 `VACUUM` 重写**，
/// 只允许在 `DB_VACUUM_CONVERT=1`（默认关闭）时、由启动/首次开库路径调用。
async fn ensure_incremental(pool: &SqlitePool) -> AppResult<()> {
    let mode: i64 = sqlx::query_scalar("PRAGMA auto_vacuum")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    if mode == 2 {
        return Ok(());
    }
    let started = std::time::Instant::now();
    sqlx::query("PRAGMA auto_vacuum=INCREMENTAL")
        .execute(pool)
        .await?;
    sqlx::query("VACUUM").execute(pool).await?;
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        "sqlite converted to incremental auto_vacuum"
    );
    Ok(())
}

/// 回收至多 `pages` 个空闲页（4 KiB/页）。有界、可重复调用；非增量模式时为空操作。
/// 用于正文缓存淘汰后立刻归还空间，避免文件只涨不落。
pub async fn reclaim_pages(pool: &SqlitePool, pages: u32) -> AppResult<()> {
    let sql = format!("PRAGMA incremental_vacuum({})", pages.min(4096));
    sqlx::query(&sql).execute(pool).await?;
    Ok(())
}

/// 初始化共享桶：建目录 + 建库 + 执行 shared 迁移。
pub async fn init_shared(cfg: &Config) -> AppResult<SqlitePool> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    std::fs::create_dir_all(format!("{}/users", cfg.data_dir))?;

    let opts = SqliteConnectOptions::from_str(&cfg.shared_db_url())?.create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await?;
    apply_pragma(&pool).await?;
    sqlx::migrate!("./migrations/shared").run(&pool).await?;
    if cfg.db_vacuum_convert {
        ensure_incremental(&pool).await?;
    }
    Ok(pool)
}

/// 取得（按需创建）某用户的独立桶连接池——物理分桶的核心。
/// 每个用户一个 SQLite 文件，天然隔离；首次访问该用户时自动建库并迁移。
pub async fn user_pool(cfg: &Config, uid: i64) -> AppResult<SqlitePool> {
    let url = cfg.user_db_url(&uid.to_string());
    let opts = SqliteConnectOptions::from_str(&url)?.create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(opts)
        .await?;
    apply_pragma(&pool).await?;
    sqlx::migrate!("./migrations/user").run(&pool).await?;
    if cfg.db_vacuum_convert {
        ensure_incremental(&pool).await?;
    }
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::{apply_pragma, reclaim_pages};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 新库在增量模式下，删行后用 `incremental_vacuum` 能把空闲页归还。
    #[tokio::test]
    async fn incremental_vacuum_reclaims_freelist() {
        let path = std::env::temp_dir().join(format!("oread-vac-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        apply_pragma(&pool).await.unwrap();
        let mode: i64 = sqlx::query_scalar("PRAGMA auto_vacuum")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mode, 2, "新库应直接是 INCREMENTAL");
        sqlx::query("CREATE TABLE t (x TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        let blob = "x".repeat(8192);
        for _ in 0..200 {
            sqlx::query("INSERT INTO t (x) VALUES (?1)")
                .bind(&blob)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("DELETE FROM t").execute(&pool).await.unwrap();
        let before: i64 = sqlx::query_scalar("PRAGMA freelist_count")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(before > 0, "删行后应有空闲页");
        reclaim_pages(&pool, 4096).await.unwrap();
        let after: i64 = sqlx::query_scalar("PRAGMA freelist_count")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(after < before, "incremental_vacuum 后空闲页应减少: {before} -> {after}");
        pool.close().await;
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), ext));
        }
    }
}
