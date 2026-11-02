use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::config::Config;
use crate::error::AppResult;

/// 内存优先的 SQLite 调优：WAL + NORMAL 同步 + 2MB 页缓存 + 禁用 mmap，
/// 确保单实例常驻内存远低于 100MB 红线。
async fn apply_pragma(pool: &SqlitePool) -> AppResult<()> {
    sqlx::query(
        "PRAGMA journal_mode=WAL; \
         PRAGMA synchronous=NORMAL; \
         PRAGMA cache_size=-2000; \
         PRAGMA mmap_size=0;",
    )
    .execute(pool)
    .await?;
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
    Ok(pool)
}
