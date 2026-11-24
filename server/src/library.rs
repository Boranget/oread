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
    /// OPF 语义字段（全局内容层，管理员可编辑；标签/标识在同库的扩展表里）
    pub language: Option<String>,
    pub publisher: Option<String>,
    /// 出版日期原样保存（`YYYY-MM-DD` 或书写源给的字符串）
    pub pubdate: Option<String>,
    pub series_name: Option<String>,
    pub series_index: Option<f64>,
    pub tier: String,
    pub collected_at: Option<i64>,
    pub last_accessed_at: i64,
    pub checked_at: i64,
    pub cached_chapters: i64,
    /// 正文 + 图片清单 JSON 的占用字节
    pub content_bytes: i64,
    /// 漫画图片原始字节占用
    pub image_bytes: i64,
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

/// 用书源补齐馆藏书目的来源字段（缺什么补什么，不覆盖已有值）。
///
/// 入馆时可能只传了 `source`，书目里还没有 `origin` / `origin_name`；
/// 后台 worker 拿到真实书源后回填，列表页与后续缓存读取才有源可用。
pub async fn adopt_source(
    pool: &SqlitePool,
    book_url: &str,
    source: &crate::entities::BookSource,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE library_books SET \
           origin = COALESCE(origin, ?2), \
           origin_name = COALESCE(origin_name, ?3), \
           book_source_type = CASE WHEN book_source_type = 0 THEN ?4 ELSE book_source_type END \
         WHERE book_url = ?1",
    )
    .bind(book_url)
    .bind(&source.book_source_url)
    .bind(&source.book_source_name)
    .bind(source.book_source_type)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 元数据（OPF 语义字段）
// ---------------------------------------------------------------------------

/// 全局内容层的元数据补丁。
///
/// 每个字段都是 `None = 保持不变`；文本字段传空串表示清空（落库为 NULL）。
/// `identifiers` / `tags` 传 `Some(vec![])` 表示清空，`Some(非空)` 表示整体替换。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MetadataPatch {
    pub name: Option<String>,
    pub author: Option<String>,
    pub intro: Option<String>,
    pub language: Option<String>,
    pub publisher: Option<String>,
    pub pubdate: Option<String>,
    pub series_name: Option<String>,
    pub series_index: Option<f64>,
    pub identifiers: Option<Vec<(String, String)>>,
    pub tags: Option<Vec<String>>,
}

impl MetadataPatch {
    /// 一个字段都没带：路由层据此返回 400，避免空补丁静默成功。
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.author.is_none()
            && self.intro.is_none()
            && self.language.is_none()
            && self.publisher.is_none()
            && self.pubdate.is_none()
            && self.series_name.is_none()
            && self.series_index.is_none()
            && self.identifiers.is_none()
            && self.tags.is_none()
    }
}

/// 一本书的扩展元数据：标签与标识单独成表，按需读取。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MetadataExtras {
    pub identifiers: Vec<(String, String)>,
    pub tags: Vec<String>,
}

fn non_empty(v: &str) -> Option<&str> {
    let t = v.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// 写入元数据补丁：先确保书目存在，再按需更新列、整体替换标识与标签。
///
/// 只动全局内容层；个人字段（星级、分组、私人标签）留在用户库。
pub async fn update_metadata(
    pool: &SqlitePool,
    book_url: &str,
    patch: &MetadataPatch,
) -> anyhow::Result<()> {
    let book_url = book_url.trim();
    ensure_book(pool, book_url).await?;
    let mut tx = pool.begin().await?;

    let mut sets: Vec<&str> = Vec::new();
    if patch.name.is_some() {
        sets.push("name = ?");
    }
    if patch.author.is_some() {
        sets.push("author = ?");
    }
    if patch.intro.is_some() {
        sets.push("intro = ?");
    }
    if patch.language.is_some() {
        sets.push("language = ?");
    }
    if patch.publisher.is_some() {
        sets.push("publisher = ?");
    }
    if patch.pubdate.is_some() {
        sets.push("pubdate = ?");
    }
    if patch.series_name.is_some() {
        sets.push("series_name = ?");
    }
    if patch.series_index.is_some() {
        sets.push("series_index = ?");
    }
    if !sets.is_empty() {
        let sql = format!(
            "UPDATE library_books SET {} WHERE book_url = ?",
            sets.join(", ")
        );
        let mut q = sqlx::query(&sql);
        if let Some(v) = &patch.name {
            q = q.bind(non_empty(v));
        }
        if let Some(v) = &patch.author {
            q = q.bind(non_empty(v));
        }
        if let Some(v) = &patch.intro {
            q = q.bind(non_empty(v));
        }
        if let Some(v) = &patch.language {
            q = q.bind(non_empty(v));
        }
        if let Some(v) = &patch.publisher {
            q = q.bind(non_empty(v));
        }
        if let Some(v) = &patch.pubdate {
            q = q.bind(non_empty(v));
        }
        if let Some(v) = &patch.series_name {
            q = q.bind(non_empty(v));
        }
        if let Some(v) = patch.series_index {
            q = q.bind(v);
        }
        q.bind(book_url).execute(&mut *tx).await?;
    }

    if let Some(list) = &patch.identifiers {
        sqlx::query("DELETE FROM library_identifiers WHERE book_url = ?1")
            .bind(book_url)
            .execute(&mut *tx)
            .await?;
        for (scheme, value) in list {
            let (Some(scheme), Some(value)) = (non_empty(scheme), non_empty(value)) else {
                continue;
            };
            sqlx::query(
                "INSERT OR IGNORE INTO library_identifiers (book_url, scheme, value) \
                 VALUES (?1, ?2, ?3)",
            )
            .bind(book_url)
            .bind(scheme)
            .bind(value)
            .execute(&mut *tx)
            .await?;
        }
    }

    if let Some(tags) = &patch.tags {
        sqlx::query("DELETE FROM library_tags WHERE book_url = ?1")
            .bind(book_url)
            .execute(&mut *tx)
            .await?;
        for tag in tags {
            let Some(tag) = non_empty(tag) else {
                continue;
            };
            sqlx::query("INSERT OR IGNORE INTO library_tags (book_url, tag) VALUES (?1, ?2)")
                .bind(book_url)
                .bind(tag)
                .execute(&mut *tx)
                .await?;
        }
    }

    tx.commit().await?;
    Ok(())
}

/// 详情页自动映射：把系列/标签写进馆藏元数据，**只填空缺**。
///
/// 管理员编辑过的值（系列、序号、标签）是权威，书源不能覆盖；
/// 返回实际写入的补丁，调用方据此同步书目录里的 OPF/ComicInfo。
pub async fn adopt_metadata(
    pool: &SqlitePool,
    book_url: &str,
    series_name: Option<&str>,
    series_index: Option<f64>,
    tags: &[String],
) -> anyhow::Result<MetadataPatch> {
    let book_url = book_url.trim();
    if book_url.is_empty() {
        return Ok(MetadataPatch::default());
    }
    ensure_book(pool, book_url).await?;
    let (current_series, current_index): (Option<String>, Option<f64>) =
        sqlx::query_as("SELECT series_name, series_index FROM library_books WHERE book_url = ?1")
            .bind(book_url)
            .fetch_one(pool)
            .await?;
    let mut patch = MetadataPatch::default();
    if non_empty(current_series.as_deref().unwrap_or("")).is_none() {
        if let Some(name) = series_name.and_then(non_empty) {
            patch.series_name = Some(name.to_string());
        }
    }
    if current_index.is_none() {
        if let Some(index) = series_index.filter(|v| v.is_finite() && *v >= 0.0) {
            patch.series_index = Some(index);
        }
    }
    let has_tags: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM library_tags WHERE book_url = ?1")
            .bind(book_url)
            .fetch_one(pool)
            .await?;
    if has_tags == 0 {
        let tags: Vec<String> = tags
            .iter()
            .filter_map(|tag| non_empty(tag).map(str::to_string))
            .collect();
        if !tags.is_empty() {
            patch.tags = Some(tags);
        }
    }
    if !patch.is_empty() {
        update_metadata(pool, book_url, &patch).await?;
    }
    Ok(patch)
}

/// 读取标签与标识（按 scheme/tag 排序，保证输出稳定）。
pub async fn load_metadata_extras(
    pool: &SqlitePool,
    book_url: &str,
) -> anyhow::Result<MetadataExtras> {
    let identifiers = sqlx::query_as::<_, (String, String)>(
        "SELECT scheme, value FROM library_identifiers \
         WHERE book_url = ?1 ORDER BY scheme, value",
    )
    .bind(book_url)
    .fetch_all(pool)
    .await?;
    let tags = sqlx::query_scalar::<_, String>(
        "SELECT tag FROM library_tags WHERE book_url = ?1 ORDER BY tag",
    )
    .bind(book_url)
    .fetch_all(pool)
    .await?;
    Ok(MetadataExtras { identifiers, tags })
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
    pub images: u64,
    pub bytes: i64,
}

/// 整本删除的结果（正文 + 图片 + 目录 + 书目）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteReport {
    pub books: u64,
    pub chapters: u64,
    pub contents: u64,
    pub images: u64,
    /// 正文与图片字节合计
    pub bytes: i64,
}

/// 回收过期的临时缓存：`tier=temp` 且 `last_accessed_at` 早于 `ttl_secs` 的书整本删除。
///
/// 馆藏（`tier=collection`）永不参与，只能由管理员移出后再回收。
pub async fn cleanup_temp(pool: &SqlitePool, ttl_secs: i64) -> anyhow::Result<CleanupReport> {
    Ok(cleanup_temp_books(pool, ttl_secs).await?.report)
}

/// 一次 TTL 清理的结果：统计 + 被删书的 `book_url`。
///
/// 目录即书之后，调用方还要按 `book_url` 把书目录移进 `trash/`；
/// 只返回统计会让磁盘上的目录变成孤儿。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupOutcome {
    pub report: CleanupReport,
    pub books: Vec<String>,
}

/// 同 [`cleanup_temp`]，额外返回被删除的 `book_url` 列表。
pub async fn cleanup_temp_books(
    pool: &SqlitePool,
    ttl_secs: i64,
) -> anyhow::Result<CleanupOutcome> {
    let cutoff = now_ms() - ttl_secs.max(0) * 1000;
    let expired: Vec<(String,)> = sqlx::query_as(
        "SELECT book_url FROM library_books WHERE tier = 'temp' AND last_accessed_at < ?1",
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await?;
    let mut out = CleanupOutcome::default();
    for (book_url,) in expired {
        let r = delete_book(pool, &book_url).await?;
        out.report.books += 1;
        out.report.chapters += r.chapters;
        out.report.contents += r.contents;
        out.report.images += r.images;
        out.report.bytes += r.bytes;
        out.books.push(book_url);
    }
    if out.report.books > 0 {
        tracing::info!(
            books = out.report.books,
            contents = out.report.contents,
            bytes = out.report.bytes,
            ttl_secs,
            "library temp cache expired"
        );
        let _ = crate::db::reclaim_pages(pool, 1024).await;
    }
    Ok(out)
}

/// 整本删除（正文 + 图片 + 目录 + 书目行）。
///
/// 删除与「取消该书的未完成任务」在同一事务里完成，避免后台 worker 在删除后
/// 继续写入、把刚删掉的书重新变出来。已在抓取中的单章仍可能落库一次，
/// 但任务已取消，后续章节会立即停止。
pub async fn delete_book(pool: &SqlitePool, book_url: &str) -> anyhow::Result<DeleteReport> {
    let mut tx = pool.begin().await?;
    cancel_jobs_for_book_tx(&mut tx, book_url).await?;
    // 旧缓存表已删除；字节与章节数直接读书目上的冗余统计。
    let stats: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT cached_chapters, content_bytes, image_bytes FROM library_books WHERE book_url = ?1",
    )
    .bind(book_url)
    .fetch_optional(&mut *tx)
    .await?;
    let (cached, content_bytes, image_bytes) = stats.unwrap_or_default();
    let chapters = sqlx::query("DELETE FROM library_chapters WHERE book_url = ?1")
        .bind(book_url)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    // 元数据扩展表没有外键，整本删除时显式清理，避免孤儿行长期堆积。
    sqlx::query("DELETE FROM library_identifiers WHERE book_url = ?1")
        .bind(book_url)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM library_tags WHERE book_url = ?1")
        .bind(book_url)
        .execute(&mut *tx)
        .await?;
    let books = sqlx::query("DELETE FROM library_books WHERE book_url = ?1")
        .bind(book_url)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(DeleteReport {
        books,
        chapters,
        contents: cached.max(0) as u64,
        images: 0,
        bytes: content_bytes + image_bytes,
    })
}

/// 取消某本书所有未完成任务（queued / running）。
pub async fn cancel_jobs_for_book(pool: &SqlitePool, book_url: &str) -> anyhow::Result<u64> {
    let mut tx = pool.begin().await?;
    let changed = cancel_jobs_for_book_tx(&mut tx, book_url).await?;
    tx.commit().await?;
    Ok(changed)
}

async fn cancel_jobs_for_book_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    book_url: &str,
) -> anyhow::Result<u64> {
    let now = now_ms();
    let changed = sqlx::query(
        "UPDATE library_jobs SET status = 'cancelled', updated_at = ?2, finished_at = ?2 \
         WHERE book_url = ?1 AND status IN ('queued','running')",
    )
    .bind(book_url)
    .bind(now)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(changed)
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
///
/// 同时取消该书的后台整本下载：已经移出馆藏，就不该继续占用下载通道。
pub async fn release(pool: &SqlitePool, book_url: &str) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await?;
    let changed = sqlx::query(
        "UPDATE library_books SET tier = 'temp', collected_at = NULL, last_accessed_at = ?2 \
         WHERE book_url = ?1 AND tier = 'collection'",
    )
    .bind(book_url)
    .bind(now_ms())
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed > 0 {
        cancel_jobs_for_book_tx(&mut tx, book_url).await?;
    }
    tx.commit().await?;
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
    /// 馆藏正文 + 图片总计
    pub collection_bytes: i64,
    pub collection_text_bytes: i64,
    pub collection_image_bytes: i64,
    pub temp_books: i64,
    pub temp_chapters: i64,
    /// 临时层正文 + 图片总计
    pub temp_bytes: i64,
    pub temp_text_bytes: i64,
    pub temp_image_bytes: i64,
    pub total_chapters: i64,
    pub content_rows: i64,
    pub content_bytes: i64,
    pub image_rows: i64,
    pub image_bytes: i64,
    /// 两层正文 + 图片总计
    pub total_bytes: i64,
    pub temp_expired_books: i64,
}

/// `stats()` 的聚合行：顺序与 SQL 的 SELECT 列表一一对应。
type StatsRow = (i64, i64, i64, i64, i64, i64, i64, i64, i64);

pub async fn stats(pool: &SqlitePool, ttl_secs: i64) -> LibraryStats {
    let row: StatsRow = sqlx::query_as(
        "SELECT \
               COALESCE(SUM(CASE WHEN tier='collection' THEN 1 ELSE 0 END),0), \
               COALESCE(SUM(CASE WHEN tier='collection' THEN cached_chapters ELSE 0 END),0), \
               COALESCE(SUM(CASE WHEN tier='collection' THEN content_bytes ELSE 0 END),0), \
               COALESCE(SUM(CASE WHEN tier='collection' THEN image_bytes ELSE 0 END),0), \
               COALESCE(SUM(CASE WHEN tier='temp' THEN 1 ELSE 0 END),0), \
               COALESCE(SUM(CASE WHEN tier='temp' THEN cached_chapters ELSE 0 END),0), \
               COALESCE(SUM(CASE WHEN tier='temp' THEN content_bytes ELSE 0 END),0), \
               COALESCE(SUM(CASE WHEN tier='temp' THEN image_bytes ELSE 0 END),0), \
               COALESCE(SUM(CASE WHEN tier='temp' AND last_accessed_at < ?1 THEN 1 ELSE 0 END),0) \
             FROM library_books",
    )
    .bind(now_ms() - ttl_secs.max(0) * 1000)
    .fetch_one(pool)
    .await
    .unwrap_or((0, 0, 0, 0, 0, 0, 0, 0, 0));
    let text_bytes = row.2 + row.6;
    let image_bytes = row.3 + row.7;
    LibraryStats {
        collection_books: row.0,
        collection_chapters: row.1,
        collection_text_bytes: row.2,
        collection_image_bytes: row.3,
        collection_bytes: row.2 + row.3,
        temp_books: row.4,
        temp_chapters: row.5,
        temp_text_bytes: row.6,
        temp_image_bytes: row.7,
        temp_bytes: row.6 + row.7,
        total_chapters: row.1 + row.5,
        content_rows: row.1 + row.5,
        content_bytes: text_bytes,
        image_rows: 0,
        image_bytes,
        total_bytes: text_bytes + image_bytes,
        temp_expired_books: row.8,
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
/// 用单条 `INSERT ... WHERE NOT EXISTS` 保证并发入馆不会产生两张采购单。
pub async fn enqueue_job(
    pool: &SqlitePool,
    book_url: &str,
    source: &str,
    uid: i64,
) -> anyhow::Result<String> {
    for _ in 0..3 {
        let id = new_job_id();
        let now = now_ms();
        let inserted = sqlx::query(
            "INSERT INTO library_jobs (id, book_url, source, uid, status, created_at, updated_at) \
             SELECT ?1, ?2, ?3, ?4, 'queued', ?5, ?5 \
             WHERE NOT EXISTS (SELECT 1 FROM library_jobs \
                               WHERE book_url = ?2 AND status IN ('queued','running'))",
        )
        .bind(&id)
        .bind(book_url)
        .bind(source)
        .bind(uid)
        .bind(now)
        .execute(pool)
        .await?
        .rows_affected();
        if inserted > 0 {
            return Ok(id);
        }
        // 没插进去说明已存在活动任务；极小概率它在两条语句之间刚好结束，重试一次。
        if let Some(existing) = active_job_id(pool, book_url).await? {
            return Ok(existing);
        }
    }
    anyhow::bail!("排队采购单失败：并发竞争未能取得任务 ID")
}

async fn active_job_id(pool: &SqlitePool, book_url: &str) -> anyhow::Result<Option<String>> {
    let id = sqlx::query_scalar(
        "SELECT id FROM library_jobs WHERE book_url = ?1 AND status IN ('queued','running') \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(book_url)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// 推迟任务：`queued` 任务的 `updated_at` 兼作「下次可执行时间」，
/// 书源冷却时用它把任务让给其它书，冷却结束自动重新参与调度。
pub async fn defer_job(pool: &SqlitePool, id: &str, not_before_ms: i64) -> anyhow::Result<bool> {
    let changed = sqlx::query(
        "UPDATE library_jobs SET updated_at = ?2, error = ?3 \
         WHERE id = ?1 AND status = 'queued'",
    )
    .bind(id)
    .bind(not_before_ms)
    .bind("书源冷却中，等待自动重试")
    .execute(pool)
    .await?
    .rows_affected();
    Ok(changed > 0)
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

/// 取最早入队且已到可执行时间的一条任务（不改变状态；调用方负责 CAS 到 running）。
///
/// `queued` 任务的 `updated_at` 兼作「下次可执行时间」：书源冷却时由
/// [`defer_job`] 推后，避免一个冷却源占住唯一的后台下载通道。
pub async fn next_queued_job(pool: &SqlitePool) -> anyhow::Result<Option<JobRow>> {
    let row = sqlx::query_as::<_, JobRow>(
        "SELECT id, book_url, source, uid FROM library_jobs \
         WHERE status = 'queued' AND updated_at <= ?1 \
         ORDER BY created_at ASC LIMIT 1",
    )
    .bind(now_ms())
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 抢占任务：只有仍处于 `queued` 时才成功，避免多 worker 重复执行。
pub async fn claim_job(pool: &SqlitePool, id: &str) -> anyhow::Result<bool> {
    let changed = sqlx::query(
        "UPDATE library_jobs SET status = 'running', updated_at = ?2, error = NULL \
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

#[cfg(test)]
#[path = "library_tests.rs"]
mod tests;
