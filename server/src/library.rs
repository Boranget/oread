//! 馆藏库（`library.db`）的领域逻辑：全局共享的书目 / 目录 / 正文 + 采购单。
//!
//! 分层模型：
//! - `tier=collection`：馆藏，永久保存，只有管理员能移出或删除；
//! - `tier=temp`：临时缓存，按 `last_accessed_at` 做 TTL 回收；
//! - `library_jobs`：入馆后的后台整本下载任务（服务端跑，不依赖前端保持页面）。
//!
//! 这里只放「馆藏自己」的读写与统计；正文/目录的读写细节仍在 `routes::books_store`，
//! 只是换成传馆藏连接池。用户状态（书架、进度、设置、书源登录态）继续留在用户库。

use axum::http::request::Parts;
use sqlx::SqlitePool;

use crate::error::AppError;

/// 馆藏层：永久保存。
pub const TIER_COLLECTION: &str = "collection";
/// 临时缓存层：按 TTL 回收。
pub const TIER_TEMP: &str = "temp";

/// 当前时间（epoch 毫秒）。与 `source_health` 用同一时钟，便于跨表比较。
pub fn now_ms() -> i64 {
    crate::source_health::now_ms()
}

/// 馆藏库连接池提取器。handler 直接写 `lib: LibraryDb` 即可。
#[derive(Clone, Debug)]
pub struct LibraryDb(pub SqlitePool);

#[axum::async_trait]
impl<S> axum::extract::FromRequestParts<S> for LibraryDb
where
    S: Send + Sync,
{
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<LibraryDb>()
            .cloned()
            .ok_or(AppError::Unauthorized)
    }
}

// ---------------------------------------------------------------------------
// 磁盘水位
// ---------------------------------------------------------------------------

/// 数据目录所在卷的剩余字节。取不到时返回 `None`（调用方按「不阻塞」处理）。
#[cfg(unix)]
pub fn free_bytes(path: &std::path::Path) -> Option<u64> {
    let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut buf) };
    if rc != 0 {
        return None;
    }
    let frsize = if buf.f_frsize > 0 {
        buf.f_frsize as u64
    } else {
        buf.f_bsize as u64
    };
    Some((buf.f_bavail as u64).saturating_mul(frsize))
}

#[cfg(windows)]
pub fn free_bytes(path: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            lp_directory_name: *const u16,
            lp_free_bytes_available_to_caller: *mut u64,
            lp_total_number_of_bytes: *mut u64,
            lp_total_number_of_free_bytes: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok == 0 {
        None
    } else {
        Some(free)
    }
}

#[cfg(not(any(unix, windows)))]
pub fn free_bytes(_path: &std::path::Path) -> Option<u64> {
    None
}

// ---------------------------------------------------------------------------
// 书目行
// ---------------------------------------------------------------------------

/// `library_books` 里属于「书源可重新提供」的字段。
///
/// 与 `entities::Book` 分开：`Book` 还包含分组、自定义封面/简介、排序、`dur_*` 等
/// 用户字段，那些留在用户库，读时覆盖。
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct LibraryBook {
    pub book_url: String,
    pub toc_url: Option<String>,
    pub origin: Option<String>,
    pub origin_name: Option<String>,
    pub name: Option<String>,
    pub author: Option<String>,
    pub kind: Option<String>,
    pub cover_url: Option<String>,
    pub intro: Option<String>,
    pub charset: Option<String>,
    pub book_source_type: i64,
    pub latest_chapter_title: Option<String>,
    pub latest_chapter_time: i64,
    pub total_chapter_num: i64,
    pub word_count: Option<String>,
    pub tier: String,
    pub collected_at: Option<i64>,
    pub last_accessed_at: i64,
    pub checked_at: i64,
    pub cached_chapters: i64,
    pub content_bytes: i64,
}

/// 读馆藏书目（只含源字段 + 馆藏状态）。
pub async fn load_book(pool: &SqlitePool, book_url: &str) -> anyhow::Result<Option<LibraryBook>> {
    let row = sqlx::query_as::<_, LibraryBook>("SELECT * FROM library_books WHERE book_url = ?1")
        .bind(book_url)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

/// 确保书目行存在（新抓到的书默认进临时层）。
pub async fn ensure_book(pool: &SqlitePool, book_url: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO library_books (book_url, last_accessed_at) VALUES (?1, ?2) \
         ON CONFLICT(book_url) DO NOTHING",
    )
    .bind(book_url)
    .bind(now_ms())
    .execute(pool)
    .await?;
    Ok(())
}

/// 刷新「最近访问时间」：TTL 只按这个字段回收临时缓存。
pub async fn touch_book(pool: &SqlitePool, book_url: &str) {
    let _ = sqlx::query("UPDATE library_books SET last_accessed_at = ?2 WHERE book_url = ?1")
        .bind(book_url)
        .bind(now_ms())
        .execute(pool)
        .await;
}

/// 重算某本书的冗余统计（已缓存章节数、正文占用字节）。
///
/// 写入正文后调用；馆藏列表与监控直接读这两列，避免每次列表都全表聚合。
pub async fn refresh_rollup(pool: &SqlitePool, book_url: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE library_books SET \
           cached_chapters = (SELECT COUNT(*) FROM library_content WHERE book_url = ?1), \
           content_bytes = (SELECT COALESCE(SUM(size_bytes),0) FROM library_content \
                            WHERE book_url = ?1), \
           last_accessed_at = ?2 \
         WHERE book_url = ?1",
    )
    .bind(book_url)
    .bind(now_ms())
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 清理 / 删除
// ---------------------------------------------------------------------------

/// 一次 TTL 清理的结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub books: u64,
    pub chapters: u64,
    pub contents: u64,
    pub bytes: i64,
}

/// 回收过期的临时缓存：`tier=temp` 且 `last_accessed_at` 早于 `ttl_secs` 的书整本删除。
///
/// 馆藏（`tier=collection`）永不参与，只能由管理员移出后再回收。
pub async fn cleanup_temp(pool: &SqlitePool, ttl_secs: i64) -> anyhow::Result<CleanupReport> {
    let cutoff = now_ms() - ttl_secs.max(0) * 1000;
    let expired: Vec<(String,)> = sqlx::query_as(
        "SELECT book_url FROM library_books WHERE tier = 'temp' AND last_accessed_at < ?1",
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await?;
    let mut report = CleanupReport::default();
    for (book_url,) in expired {
        let r = delete_book(pool, &book_url).await?;
        report.books += 1;
        report.chapters += r.1;
        report.contents += r.2;
        report.bytes += r.3;
    }
    if report.books > 0 {
        tracing::info!(
            books = report.books,
            contents = report.contents,
            bytes = report.bytes,
            ttl_secs,
            "library temp cache expired"
        );
        let _ = crate::db::reclaim_pages(pool, 1024).await;
    }
    Ok(report)
}

/// 整本删除（正文 + 目录 + 书目行）。返回 `(删除书目行, 目录行, 正文行, 释放字节)`。
pub async fn delete_book(
    pool: &SqlitePool,
    book_url: &str,
) -> anyhow::Result<(u64, u64, u64, i64)> {
    let bytes: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(size_bytes),0) FROM library_content WHERE book_url = ?1",
    )
    .bind(book_url)
    .fetch_one(pool)
    .await?;
    let contents = sqlx::query("DELETE FROM library_content WHERE book_url = ?1")
        .bind(book_url)
        .execute(pool)
        .await?
        .rows_affected();
    let chapters = sqlx::query("DELETE FROM library_chapters WHERE book_url = ?1")
        .bind(book_url)
        .execute(pool)
        .await?
        .rows_affected();
    let books = sqlx::query("DELETE FROM library_books WHERE book_url = ?1")
        .bind(book_url)
        .execute(pool)
        .await?
        .rows_affected();
    Ok((books, chapters, contents, bytes))
}

/// 只删正文，保留书目与目录（「清空这本书的缓存」）。
pub async fn clear_content(pool: &SqlitePool, book_url: &str) -> anyhow::Result<(u64, i64)> {
    let bytes: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(size_bytes),0) FROM library_content WHERE book_url = ?1",
    )
    .bind(book_url)
    .fetch_one(pool)
    .await?;
    let deleted = sqlx::query("DELETE FROM library_content WHERE book_url = ?1")
        .bind(book_url)
        .execute(pool)
        .await?
        .rows_affected();
    refresh_rollup(pool, book_url).await.ok();
    Ok((deleted, bytes))
}

// ---------------------------------------------------------------------------
// 馆藏状态
// ---------------------------------------------------------------------------

/// 入馆：置为 `collection` 并记录入馆时间。返回是否为新入馆（幂等）。
pub async fn collect(pool: &SqlitePool, book_url: &str) -> anyhow::Result<bool> {
    ensure_book(pool, book_url).await?;
    let changed = sqlx::query(
        "UPDATE library_books SET tier = 'collection', \
           collected_at = COALESCE(collected_at, ?2) \
         WHERE book_url = ?1 AND tier <> 'collection'",
    )
    .bind(book_url)
    .bind(now_ms())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(changed > 0)
}

/// 移出馆藏：降级为临时缓存，内容先留着，交给 TTL 或手动删除。
pub async fn release(pool: &SqlitePool, book_url: &str) -> anyhow::Result<bool> {
    let changed = sqlx::query(
        "UPDATE library_books SET tier = 'temp', collected_at = NULL, last_accessed_at = ?2 \
         WHERE book_url = ?1 AND tier = 'collection'",
    )
    .bind(book_url)
    .bind(now_ms())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(changed > 0)
}

// ---------------------------------------------------------------------------
// 统计
// ---------------------------------------------------------------------------

/// 馆藏 / 临时两层的汇总（不设上限，只做监控）。
#[derive(Debug, Clone, Copy, Default)]
pub struct LibraryStats {
    pub collection_books: i64,
    pub collection_chapters: i64,
    pub collection_bytes: i64,
    pub temp_books: i64,
    pub temp_chapters: i64,
    pub temp_bytes: i64,
    pub total_chapters: i64,
    pub content_rows: i64,
    pub content_bytes: i64,
    pub temp_expired_books: i64,
}

pub async fn stats(pool: &SqlitePool, ttl_secs: i64) -> LibraryStats {
    let row: (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
           COALESCE(SUM(CASE WHEN tier='collection' THEN 1 ELSE 0 END),0), \
           COALESCE(SUM(CASE WHEN tier='collection' THEN cached_chapters ELSE 0 END),0), \
           COALESCE(SUM(CASE WHEN tier='collection' THEN content_bytes ELSE 0 END),0), \
           COALESCE(SUM(CASE WHEN tier='temp' THEN 1 ELSE 0 END),0), \
           COALESCE(SUM(CASE WHEN tier='temp' THEN cached_chapters ELSE 0 END),0), \
           COALESCE(SUM(CASE WHEN tier='temp' THEN content_bytes ELSE 0 END),0), \
           COALESCE(SUM(cached_chapters),0), \
           (SELECT COUNT(*) FROM library_content), \
           (SELECT COALESCE(SUM(size_bytes),0) FROM library_content), \
           COALESCE(SUM(CASE WHEN tier='temp' AND last_accessed_at < ?1 THEN 1 ELSE 0 END),0) \
         FROM library_books",
    )
    .bind(now_ms() - ttl_secs.max(0) * 1000)
    .fetch_one(pool)
    .await
    .unwrap_or((0, 0, 0, 0, 0, 0, 0, 0, 0, 0));
    LibraryStats {
        collection_books: row.0,
        collection_chapters: row.1,
        collection_bytes: row.2,
        temp_books: row.3,
        temp_chapters: row.4,
        temp_bytes: row.5,
        total_chapters: row.6,
        content_rows: row.7,
        content_bytes: row.8,
        temp_expired_books: row.9,
    }
}

// ---------------------------------------------------------------------------
// 采购单（后台整本下载任务）
// ---------------------------------------------------------------------------

/// 任务状态。
pub const JOB_QUEUED: &str = "queued";
pub const JOB_RUNNING: &str = "running";
pub const JOB_DONE: &str = "done";
pub const JOB_PARTIAL: &str = "partial";
pub const JOB_FAILED: &str = "failed";
pub const JOB_CANCELLED: &str = "cancelled";

/// 生成任务 ID（进程内单调，含进程号，便于日志定位）。
pub fn new_job_id() -> String {
    static SEQ: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    format!(
        "lj{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// 入队一本书的整本下载任务。
///
/// 同一本书已有 `queued`/`running` 任务时不重复创建，直接返回已有 ID（幂等）。
pub async fn enqueue_job(
    pool: &SqlitePool,
    book_url: &str,
    source: &str,
    uid: i64,
) -> anyhow::Result<String> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM library_jobs WHERE book_url = ?1 AND status IN ('queued','running') \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(book_url)
    .fetch_optional(pool)
    .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = new_job_id();
    let now = now_ms();
    sqlx::query(
        "INSERT INTO library_jobs (id, book_url, source, uid, status, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?5)",
    )
    .bind(&id)
    .bind(book_url)
    .bind(source)
    .bind(uid)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(id)
}

/// 取消任务：只允许取消 `queued`/`running`。
pub async fn cancel_job(pool: &SqlitePool, id: &str) -> anyhow::Result<bool> {
    let changed = sqlx::query(
        "UPDATE library_jobs SET status = 'cancelled', updated_at = ?2, finished_at = ?2 \
         WHERE id = ?1 AND status IN ('queued','running')",
    )
    .bind(id)
    .bind(now_ms())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(changed > 0)
}

/// 进程重启后把残留的 `running` 任务放回队列，由 worker 续跑。
pub async fn requeue_interrupted(pool: &SqlitePool) -> anyhow::Result<u64> {
    let changed = sqlx::query(
        "UPDATE library_jobs SET status = 'queued', updated_at = ?1 WHERE status = 'running'",
    )
    .bind(now_ms())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(changed)
}

/// 一条待执行任务。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct JobRow {
    pub id: String,
    pub book_url: String,
    pub source: String,
    pub uid: i64,
}

/// 取最早入队的一条任务（不改变状态；调用方负责 CAS 到 running）。
pub async fn next_queued_job(pool: &SqlitePool) -> anyhow::Result<Option<JobRow>> {
    let row = sqlx::query_as::<_, JobRow>(
        "SELECT id, book_url, source, uid FROM library_jobs \
         WHERE status = 'queued' ORDER BY created_at ASC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 抢占任务：只有仍处于 `queued` 时才成功，避免多 worker 重复执行。
pub async fn claim_job(pool: &SqlitePool, id: &str) -> anyhow::Result<bool> {
    let changed = sqlx::query(
        "UPDATE library_jobs SET status = 'running', updated_at = ?2 \
         WHERE id = ?1 AND status = 'queued'",
    )
    .bind(id)
    .bind(now_ms())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(changed > 0)
}

/// 写入/更新单章任务状态。
pub async fn set_job_item(
    pool: &SqlitePool,
    job_id: &str,
    chapter_url: &str,
    chapter_index: i64,
    status: &str,
    error: Option<&str>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO library_job_items (job_id, chapter_url, chapter_index, status, error, updated_at) \
         VALUES (?1,?2,?3,?4,?5,?6) \
         ON CONFLICT(job_id, chapter_url) DO UPDATE SET status=excluded.status, \
          error=excluded.error, chapter_index=excluded.chapter_index, updated_at=excluded.updated_at",
    )
    .bind(job_id)
    .bind(chapter_url)
    .bind(chapter_index)
    .bind(status)
    .bind(error)
    .bind(now_ms())
    .execute(pool)
    .await?;
    Ok(())
}

/// 更新任务进度计数与状态。
pub async fn update_job_progress(
    pool: &SqlitePool,
    job_id: &str,
    total: i64,
    done: i64,
    failed: i64,
    skipped: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE library_jobs SET total=?2, done=?3, failed=?4, skipped=?5, updated_at=?6 \
         WHERE id = ?1",
    )
    .bind(job_id)
    .bind(total)
    .bind(done)
    .bind(failed)
    .bind(skipped)
    .bind(now_ms())
    .execute(pool)
    .await?;
    Ok(())
}

/// 结束任务：写最终状态与错误信息。
pub async fn finish_job(
    pool: &SqlitePool,
    job_id: &str,
    status: &str,
    error: Option<&str>,
) -> anyhow::Result<()> {
    let now = now_ms();
    sqlx::query(
        "UPDATE library_jobs SET status=?2, error=?3, updated_at=?4, finished_at=?4 WHERE id = ?1",
    )
    .bind(job_id)
    .bind(status)
    .bind(error)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}
