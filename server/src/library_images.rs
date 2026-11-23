//! 馆藏漫画图片：把漫画章节的图片原始字节真正落盘。
//!
//! 只有 `tier=collection` 的漫画由后台 worker 下载并写入本表；临时层仍走进程内
//! LRU，不占磁盘。图片按 `(url, origin)` 去重，和正文缓存键保持一致，避免镜像源
//! 同 URL 串图。
//!
//! 这里只负责图片的读写；正文行的 `images_ready` 标记、书目占用统计仍由
//! [`crate::library`] 与 worker 维护。

use sqlx::SqlitePool;

/// 一张已落盘的图片。
#[derive(Debug, Clone)]
pub struct StoredImage {
    pub data: Vec<u8>,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// 图片的轻量元数据（不读 BLOB）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageMeta {
    pub size_bytes: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

fn to_u32(v: Option<i64>) -> Option<u32> {
    v.and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0)
}

type LoadRow = (String, String, Vec<u8>, Option<i64>, Option<i64>);

/// 读取图片字节；命中时刷新访问时间，并顺带刷新所属书目的 `last_accessed_at`。
pub async fn load(
    pool: &SqlitePool,
    url: &str,
    origin: &str,
) -> anyhow::Result<Option<StoredImage>> {
    let row: Option<LoadRow> = sqlx::query_as(
        "SELECT book_url, mime, data, width, height FROM library_images \
         WHERE url = ?1 AND origin = ?2",
    )
    .bind(url)
    .bind(origin)
    .fetch_optional(pool)
    .await?;
    let Some((book_url, mime, data, width, height)) = row else {
        return Ok(None);
    };
    let _ = sqlx::query(
        "UPDATE library_images SET accessed_at = datetime('now') WHERE url = ?1 AND origin = ?2",
    )
    .bind(url)
    .bind(origin)
    .execute(pool)
    .await;
    if !book_url.trim().is_empty() {
        crate::library::touch_book(pool, &book_url).await;
    }
    Ok(Some(StoredImage {
        data,
        mime,
        width: to_u32(width),
        height: to_u32(height),
    }))
}

/// 只读元数据：用于 worker 断点续跑时判断「这张图是否已经下过」。
pub async fn meta_of(
    pool: &SqlitePool,
    url: &str,
    origin: &str,
) -> anyhow::Result<Option<ImageMeta>> {
    let row: Option<(i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT size_bytes, width, height FROM library_images WHERE url = ?1 AND origin = ?2",
    )
    .bind(url)
    .bind(origin)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(size_bytes, width, height)| ImageMeta {
        size_bytes,
        width: to_u32(width),
        height: to_u32(height),
    }))
}

/// 写入/覆盖一张图片，返回实际字节数。
#[allow(clippy::too_many_arguments)]
pub async fn save(
    pool: &SqlitePool,
    book_url: &str,
    url: &str,
    origin: &str,
    mime: &str,
    data: &[u8],
    width: Option<u32>,
    height: Option<u32>,
) -> anyhow::Result<i64> {
    let size = data.len() as i64;
    sqlx::query(
        "INSERT INTO library_images \
           (url, origin, book_url, mime, data, size_bytes, width, height, updated_at, accessed_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8, datetime('now'), datetime('now')) \
         ON CONFLICT(url, origin) DO UPDATE SET \
           book_url=excluded.book_url, mime=excluded.mime, data=excluded.data, \
           size_bytes=excluded.size_bytes, width=excluded.width, height=excluded.height, \
           updated_at=datetime('now'), accessed_at=datetime('now')",
    )
    .bind(url)
    .bind(origin)
    .bind(book_url)
    .bind(mime)
    .bind(data)
    .bind(size)
    .bind(width.map(i64::from))
    .bind(height.map(i64::from))
    .execute(pool)
    .await?;
    Ok(size)
}

/// 删除一批图片 BLOB。目录已经持有原始字节后调用，避免同一章占两份磁盘。
///
/// 按 `(book_url, url, origin)` 删：表按 `(url, origin)` 去重，两本书可能共享同一张图，
/// 这里只删当前书的行。返回实际删除的行数。
pub async fn delete_urls(
    pool: &SqlitePool,
    book_url: &str,
    urls: &[String],
    origin: &str,
) -> anyhow::Result<u64> {
    let mut tx = pool.begin().await?;
    let mut removed = 0u64;
    for url in urls {
        removed += sqlx::query(
            "DELETE FROM library_images WHERE book_url = ?1 AND url = ?2 AND origin = ?3",
        )
        .bind(book_url)
        .bind(url)
        .bind(origin)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    tx.commit().await?;
    Ok(removed)
}

/// 重置一章的图片完整性标记（重新抓取清单或补图前调用）。
pub async fn mark_chapter_pending(
    pool: &SqlitePool,
    chapter_url: &str,
    origin: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE library_content SET images_ready = 0 WHERE url = ?1 AND origin = ?2")
        .bind(chapter_url)
        .bind(origin)
        .execute(pool)
        .await?;
    Ok(())
}

/// 标记一章的图片已全部落盘（worker 在一章所有图片都成功后调用）。
pub async fn mark_chapter_ready(
    pool: &SqlitePool,
    chapter_url: &str,
    origin: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE library_content SET images_ready = 1 WHERE url = ?1 AND origin = ?2")
        .bind(chapter_url)
        .bind(origin)
        .execute(pool)
        .await?;
    Ok(())
}
