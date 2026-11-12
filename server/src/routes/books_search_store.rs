//! 搜索任务的数据库持久化（元数据 + 最终快照）。
//!
//! 内存注册表负责运行期的实时进度；这里负责两件内存做不到的事：
//! 1. 前端关闭/重新登录后仍能列出任务（同进程时进度仍以内存为准）；
//! 2. 进程重启后仍能看到历史任务与已完成的搜索结果（运行中的行标为 interrupted）。
//!
//! 写路径只有两处：`search_start` 写 running 行，任务收尾写最终快照；
//! 运行期不逐秒刷库，避免把低配设备的 WAL 打满。

use super::*;

/// 数据库中的任务行（列表与结果回退共用）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct SearchJobRow {
    pub(super) id: String,
    pub(super) uid: i64,
    pub(super) keyword: String,
    pub(super) scope: String,
    pub(super) source_mode: String,
    pub(super) coverage: String,
    pub(super) status: String,
    pub(super) source_total: i64,
    pub(super) finished: i64,
    pub(super) pending: i64,
    pub(super) skipped: i64,
    pub(super) cancelled: i64,
    pub(super) failed: i64,
    pub(super) curated_total: i64,
    pub(super) scope_filtered: i64,
    pub(super) skipped_cooldown: i64,
    pub(super) cooldown_total: i64,
    pub(super) created_at: i64,
    pub(super) finished_at: Option<i64>,
    pub(super) snapshot_json: Option<String>,
}

const JOB_COLUMNS: &str = "id, uid, keyword, scope, source_mode, coverage, status, \
     source_total, finished, pending, skipped, cancelled, failed, curated_total, \
     scope_filtered, skipped_cooldown, cooldown_total, created_at, updated_at, \
     finished_at, snapshot_json";

/// 单用户数据库保留的任务数（含运行中；更旧的自动删除）。
pub(super) const SEARCH_JOB_DB_KEEP: i64 = 50;

/// 任务创建时写入 running 行；失败只记日志，不影响搜索本身。
pub(super) async fn persist_started(pool: &SqlitePool, job: &SearchJob) -> sqlx::Result<()> {
    let now = unix_ms() as i64;
    sqlx::query(
        "INSERT INTO search_jobs \
         (id, uid, keyword, scope, source_mode, coverage, status, source_total, pending, \
          curated_total, scope_filtered, skipped_cooldown, cooldown_total, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7, ?7, ?8, ?9, ?10, ?11, ?12, ?12) \
         ON CONFLICT(id) DO NOTHING",
    )
    .bind(&job.id)
    .bind(job.uid)
    .bind(&job.key)
    .bind(job.scope.as_str())
    .bind(&job.source_mode)
    .bind(&job.coverage)
    .bind(job.source_total as i64)
    .bind(job.curated_total as i64)
    .bind(job.scope_filtered as i64)
    .bind(job.skipped_cooldown as i64)
    .bind(job.cooldown_total as i64)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// 任务收尾：写最终状态、进度计数与结果快照，并按单用户上限裁剪历史行。
pub(super) async fn persist_finished(
    pool: &SqlitePool,
    meta: &JobMeta,
    view: &JobView,
) -> sqlx::Result<()> {
    let now = unix_ms() as i64;
    let failed = view
        .errors
        .iter()
        .filter(|e| !is_budget_skip(&e.kind))
        .count() as i64;
    let snapshot = serde_json::json!({
        "books": view.books,
        "errors": view.errors,
    });
    sqlx::query(
        "UPDATE search_jobs SET status = ?2, finished = ?3, pending = ?4, skipped = ?5, \
         cancelled = ?6, failed = ?7, source_total = ?8, updated_at = ?9, finished_at = ?10, \
         snapshot_json = ?11 \
         WHERE id = ?1",
    )
    .bind(&meta.id)
    .bind(&meta.status)
    .bind(view.finished as i64)
    .bind(view.pending as i64)
    .bind(view.skipped as i64)
    .bind(view.cancelled as i64)
    .bind(failed)
    .bind(meta.source_total as i64)
    .bind(now)
    .bind(now)
    .bind(snapshot.to_string())
    .execute(pool)
    .await?;
    trim_user(pool, meta.uid, SEARCH_JOB_DB_KEEP).await?;
    Ok(())
}

/// 当前用户最近的任务行（按创建时间倒序）。
pub(super) async fn db_list(
    pool: &SqlitePool,
    uid: i64,
    limit: i64,
) -> sqlx::Result<Vec<SearchJobRow>> {
    let sql = format!(
        "SELECT {JOB_COLUMNS} FROM search_jobs WHERE uid = ?1 ORDER BY created_at DESC LIMIT ?2"
    );
    sqlx::query_as::<_, SearchJobRow>(&sql)
        .bind(uid)
        .bind(limit.clamp(1, SEARCH_JOB_DB_KEEP))
        .fetch_all(pool)
        .await
}

/// 按 id 读取当前用户的任务行（跨用户视为不存在）。
pub(super) async fn db_get(
    pool: &SqlitePool,
    uid: i64,
    id: &str,
) -> sqlx::Result<Option<SearchJobRow>> {
    let sql = format!("SELECT {JOB_COLUMNS} FROM search_jobs WHERE uid = ?1 AND id = ?2");
    sqlx::query_as::<_, SearchJobRow>(&sql)
        .bind(uid)
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// 删除一条任务行；返回是否真的删除。
pub(super) async fn db_delete(pool: &SqlitePool, uid: i64, id: &str) -> sqlx::Result<bool> {
    let r = sqlx::query("DELETE FROM search_jobs WHERE uid = ?1 AND id = ?2")
        .bind(uid)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(r.rows_affected() > 0)
}

/// 启动时把上次进程遗留的 running 行标为 interrupted（任务已随进程结束）。
pub(super) async fn mark_interrupted(pool: &SqlitePool) -> sqlx::Result<u64> {
    let now = unix_ms() as i64;
    let r = sqlx::query(
        "UPDATE search_jobs SET status = 'interrupted', pending = 0, updated_at = ?1, \
         finished_at = COALESCE(finished_at, ?1) WHERE status = 'running'",
    )
    .bind(now)
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// 单用户历史裁剪：只保留最近 `keep` 条。
pub(super) async fn trim_user(pool: &SqlitePool, uid: i64, keep: i64) -> sqlx::Result<()> {
    sqlx::query(
        "DELETE FROM search_jobs WHERE uid = ?1 AND id NOT IN \
         (SELECT id FROM search_jobs WHERE uid = ?1 ORDER BY created_at DESC LIMIT ?2)",
    )
    .bind(uid)
    .bind(keep.max(1))
    .execute(pool)
    .await?;
    Ok(())
}
