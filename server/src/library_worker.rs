//! 馆藏采购单的后台执行器：入馆后由服务端整本下载正文，不依赖前端保持页面。
//!
//! 设计要点：
//! - **低优先级**：独立通道，默认 1 个任务并发（`LIBRARY_JOB_PARALLEL`），
//!   每任务 3 章并发（`LIBRARY_JOB_CHAPTER_PARALLEL`），交互阅读不受影响。
//! - **可续跑**：每章状态落在 `library_job_items`，进程重启后 `running` 任务回到队列。
//! - **磁盘安全阀**：剩余空间低于 `LIBRARY_DISK_FLOOR_MB`（默认 1 GiB）时暂停，
//!   等空间恢复后继续；交互阅读照常可用。
//! - **可取消**：每章开始前查一次任务状态，`cancelled` 立即停止。

use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use sqlx::SqlitePool;

use crate::entities::{Book, BookChapter, BookContent};
use crate::library::{self, JobRow};
use crate::routes::AppState;

/// 空闲时的轮询间隔。
const IDLE_POLL: Duration = Duration::from_secs(5);
/// 磁盘不足时的重试间隔。
const DISK_WAIT: Duration = Duration::from_secs(60);
/// 临时缓存 TTL 清理周期。
const CLEANUP_EVERY: Duration = Duration::from_secs(3600);

/// 启动后台执行器与定期清理任务。进程生命周期内常驻。
pub fn spawn(state: AppState) {
    for idx in 0..state.cfg.library_job_parallel {
        let st = state.clone();
        tokio::spawn(async move {
            worker_loop(st, idx).await;
        });
    }
    let st = state.clone();
    tokio::spawn(async move {
        cleanup_loop(st).await;
    });
}

async fn worker_loop(state: AppState, idx: usize) {
    loop {
        match library::next_queued_job(&state.library).await {
            Ok(Some(job)) => {
                if let Ok(true) = library::claim_job(&state.library, &job.id).await {
                    tracing::info!(
                        worker = idx,
                        job = %job.id,
                        book = %job.book_url,
                        "library job started"
                    );
                    let outcome = run_job(&state, &job).await;
                    match outcome {
                        Ok(status) => {
                            if status != library::JOB_CANCELLED {
                                if let Err(e) =
                                    library::finish_job(&state.library, &job.id, status, None).await
                                {
                                    tracing::warn!(job = %job.id, error = %e, "finish job failed");
                                }
                            }
                            tracing::info!(worker = idx, job = %job.id, status, "library job finished");
                        }
                        Err(e) => {
                            let msg = format!("{e}");
                            tracing::warn!(worker = idx, job = %job.id, error = %msg, "library job failed");
                            let _ = library::finish_job(
                                &state.library,
                                &job.id,
                                library::JOB_FAILED,
                                Some(&msg),
                            )
                            .await;
                        }
                    }
                    continue;
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "poll library jobs failed"),
        }
        tokio::time::sleep(IDLE_POLL).await;
    }
}

async fn cleanup_loop(state: AppState) {
    loop {
        tokio::time::sleep(CLEANUP_EVERY).await;
        match library::cleanup_temp(&state.library, state.cfg.library_temp_ttl_secs).await {
            Ok(r) if r.books > 0 => {
                tracing::info!(
                    books = r.books,
                    bytes = r.bytes,
                    "library temp cache cleaned"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "library temp cleanup failed"),
        }
    }
}

async fn job_cancelled(state: &AppState, job_id: &str) -> bool {
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM library_jobs WHERE id = ?1")
            .bind(job_id)
            .fetch_optional(&state.library)
            .await
            .ok()
            .flatten();
    matches!(status.as_deref(), Some(library::JOB_CANCELLED) | None)
}

/// 剩余空间不足时等待；返回 false 表示任务已被取消。
async fn wait_for_disk(state: &AppState, job_id: &str) -> bool {
    loop {
        let free = library::free_bytes(std::path::Path::new(&state.cfg.data_dir));
        let ok = free
            .map(|f| f >= state.cfg.library_disk_floor_bytes)
            .unwrap_or(true);
        if ok {
            return true;
        }
        tracing::warn!(
            job = %job_id,
            free_bytes = free,
            floor = state.cfg.library_disk_floor_bytes,
            "library job paused: disk floor reached"
        );
        tokio::time::sleep(DISK_WAIT).await;
        if job_cancelled(state, job_id).await {
            return false;
        }
    }
}

/// 执行一张采购单，返回最终状态（`done` / `partial` / `cancelled`）。
async fn run_job(state: &AppState, job: &JobRow) -> anyhow::Result<&'static str> {
    let source = crate::routes::sources::load_source(&state.shared, &job.source)
        .await
        .map_err(|e| anyhow::anyhow!("书源不可用：{e}"))?;

    // 借书证里的书源登录态（登录态书源也能后台抓）。
    let user_pool: SqlitePool = crate::auth::get_user_pool(state, job.uid)
        .await
        .map_err(|e| anyhow::anyhow!("用户库不可用：{e}"))?;
    let key = crate::web_book::book_key(&source, &job.book_url);
    let store = crate::routes::books::book_store_for(&user_pool, &key).await;

    let chapters = ensure_chapters(state, job, &user_pool, &source, &store).await?;
    if chapters.is_empty() {
        anyhow::bail!("目录为空，无法整本下载");
    }

    let cached: std::collections::HashSet<String> = sqlx::query_scalar(
        "SELECT url FROM library_content WHERE book_url = ?1 AND content IS NOT NULL \
         AND cache_version = ?2",
    )
    .bind(&job.book_url)
    .bind(crate::routes::books::content_cache_version())
    .fetch_all(&state.library)
    .await?
    .into_iter()
    .collect();

    let pending: Vec<BookChapter> = chapters
        .iter()
        .filter(|c| !c.is_volume && !cached.contains(&c.url))
        .cloned()
        .collect();
    let total = chapters.iter().filter(|c| !c.is_volume).count() as i64;
    let skipped = total - pending.len() as i64;
    // `done` 只计本次新下载的章节，`skipped` 是开工前已缓存的章节；
    // 前端进度按 `(done + skipped) / total` 计算，两者不能重叠。
    library::update_job_progress(&state.library, &job.id, total, 0, 0, skipped).await?;

    let done = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let failed = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let concurrency = state.cfg.library_job_chapter_parallel.max(1);
    let src_ref = &source;
    let store_ref = &store;
    let outcomes: Vec<ChapterOutcome> = stream::iter(pending.into_iter().map(|ch| {
        let done = done.clone();
        let failed = failed.clone();
        let cancelled = cancelled.clone();
        async move {
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                return ChapterOutcome::Cancelled;
            }
            if !wait_for_disk(state, &job.id).await {
                cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                return ChapterOutcome::Cancelled;
            }
            if job_cancelled(state, &job.id).await {
                cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                return ChapterOutcome::Cancelled;
            }
            match fetch_and_store(state, src_ref, store_ref, &job.book_url, &ch).await {
                Ok(()) => {
                    let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    let _ = library::set_job_item(
                        &state.library,
                        &job.id,
                        &ch.url,
                        ch.index,
                        "ok",
                        None,
                    )
                    .await;
                    let _ = library::update_job_progress(
                        &state.library,
                        &job.id,
                        total,
                        n,
                        failed.load(std::sync::atomic::Ordering::Relaxed),
                        skipped,
                    )
                    .await;
                    ChapterOutcome::Ok
                }
                Err(e) => {
                    let msg = format!("{e}");
                    let n = failed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    let _ = library::set_job_item(
                        &state.library,
                        &job.id,
                        &ch.url,
                        ch.index,
                        "failed",
                        Some(&msg),
                    )
                    .await;
                    let _ = library::update_job_progress(
                        &state.library,
                        &job.id,
                        total,
                        done.load(std::sync::atomic::Ordering::Relaxed),
                        n,
                        skipped,
                    )
                    .await;
                    ChapterOutcome::Failed
                }
            }
        }
    }))
    .buffer_unordered(concurrency)
    .collect()
    .await;

    let _ = crate::routes::books::persist_book_changes(&user_pool, store.take_changes()).await;

    if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(library::JOB_CANCELLED);
    }
    let failed_n = outcomes
        .iter()
        .filter(|o| matches!(o, ChapterOutcome::Failed))
        .count();
    library::refresh_rollup(&state.library, &job.book_url)
        .await
        .ok();
    Ok(if failed_n == 0 {
        library::JOB_DONE
    } else {
        library::JOB_PARTIAL
    })
}

enum ChapterOutcome {
    Ok,
    Failed,
    Cancelled,
}

/// 目录：优先读馆藏；没有就抓一次并落库（入馆后不一定打开过目录）。
async fn ensure_chapters(
    state: &AppState,
    job: &JobRow,
    user_pool: &SqlitePool,
    source: &crate::entities::BookSource,
    store: &crate::web_book::BookStore,
) -> anyhow::Result<Vec<BookChapter>> {
    let rows: Vec<BookChapter> = sqlx::query_as(
        "SELECT url, title, is_volume, base_url, book_url, \"index\", is_vip, is_pay, \
         resource_url, tag, start, \"end\", update_time, word_count FROM library_chapters \
         WHERE book_url = ?1 ORDER BY \"index\"",
    )
    .bind(&job.book_url)
    .fetch_all(&state.library)
    .await?;
    if !rows.is_empty() {
        return Ok(rows);
    }
    let meta = library::load_book(&state.library, &job.book_url).await?;
    let book = Book {
        book_url: job.book_url.clone(),
        toc_url: meta.as_ref().and_then(|m| m.toc_url.clone()),
        origin: Some(source.book_source_url.clone()),
        origin_name: Some(source.book_source_name.clone()),
        book_source_type: source.book_source_type,
        name: meta.as_ref().and_then(|m| m.name.clone()),
        author: meta.as_ref().and_then(|m| m.author.clone()),
        ..Default::default()
    };
    let chapters =
        crate::web_book::chapter_list_book(state.fetcher.as_ref(), source, &book, Some(store))
            .await?;
    if chapters.is_empty() {
        return Ok(chapters);
    }
    crate::routes::books::save_chapters(&state.library, &job.book_url, &chapters).await?;
    let _ = crate::routes::books::sync_total_chapter_num(
        &state.library,
        user_pool,
        &job.book_url,
        chapters.len() as i64,
    )
    .await;
    Ok(chapters)
}

/// 抓一章并落馆藏。空正文/零图按失败处理（不把空白固化进馆藏）。
async fn fetch_and_store(
    state: &AppState,
    source: &crate::entities::BookSource,
    store: &crate::web_book::BookStore,
    book_url: &str,
    chapter: &BookChapter,
) -> anyhow::Result<()> {
    if source.book_source_type == 2 {
        let imgs = crate::web_book::chapter_images_book(
            state.fetcher.as_ref(),
            source,
            chapter,
            Some(store),
        )
        .await?;
        if imgs.images.is_empty() {
            anyhow::bail!("图片列表为空");
        }
        let c = BookContent {
            url: chapter.url.clone(),
            origin: source.book_source_url.clone(),
            book_url: book_url.to_string(),
            title: chapter.title.clone(),
            content: Some(String::new()),
            chapter_index: chapter.index,
            content_type: "image".into(),
            images_json: Some(serde_json::to_string(&imgs)?),
            ..Default::default()
        };
        crate::routes::books::upsert_content(&state.library, &c).await?;
        return Ok(());
    }
    let mut c =
        crate::web_book::chapter_content_book(state.fetcher.as_ref(), source, chapter, Some(store))
            .await?;
    if c.book_url.trim().is_empty() {
        c.book_url = book_url.to_string();
    }
    if c.url.trim().is_empty() {
        c.url = chapter.url.clone();
    }
    c.origin = source.book_source_url.clone();
    c.chapter_index = chapter.index;
    if crate::routes::books::content_is_empty(&c) {
        anyhow::bail!("正文为空");
    }
    crate::routes::books::upsert_content(&state.library, &c).await?;
    Ok(())
}
