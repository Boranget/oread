//! 馆藏漫画图片：旧库 BLOB 的只读回退。
//!
//! 新写入的漫画图片直接进书目录（`oread.json` 页表 + 扁平图片文件），不再落
//! `library_images`。本模块只保留：
//! - [`load`]：图片代理在目录缺失时读旧 BLOB；
//! - [`delete_urls`]：目录接手原始字节后清掉旧行；
//! - `mark_chapter_*`：维护 `library_content.images_ready` 标记。
//!
//! 等 `doctor` 确认旧表覆盖完成、删表后本模块即可整体移除。

use sqlx::SqlitePool;

/// 一张已落盘的图片。
#[derive(Debug, Clone)]
pub struct StoredImage {
    pub data: Vec<u8>,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

fn to_u32(v: Option<i64>) -> Option<u32> {
    v.and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0)
}

type LoadRow = (String, String, Vec<u8>, Option<i64>, Option<i64>);

/// 读取旧库图片字节；命中时刷新访问时间，并顺带刷新所属书目的 `last_accessed_at`。
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

/// 删除一批旧 BLOB。目录已经持有原始字节后调用，避免同一章占两份磁盘。
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

/// 标记一章的图片清单已抓取（worker 在清单落库后调用；页表是否完整由目录判断）。
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
