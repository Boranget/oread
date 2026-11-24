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

use crate::entities::{Book, BookChapter};
use crate::library::{self, JobRow};
use crate::routes::AppState;

/// 单章内图片下载并发。与章节并发相乘后仍远低于交互阅读的优先级。
const IMAGE_PARALLEL: usize = 3;
/// 落盘图片的单张上限：超过只保留清单，不写进书目录（与进程内 LRU 一致）。
const IMAGE_KEEP_MAX: usize = 2 * 1024 * 1024;
/// 单章漫画下载到内存的上限；超过则整章失败，避免一次任务把内存吃光。
const IMAGE_CHAPTER_MAX_BYTES: usize = 256 * 1024 * 1024;

/// 下载完成、尚未写进书目录的一张漫画图。
struct FetchedImage {
    data: Vec<u8>,
    mime: String,
    width: Option<u32>,
    height: Option<u32>,
}

/// 空闲时的轮询间隔。
const IDLE_POLL: Duration = Duration::from_secs(5);
/// 磁盘不足时的重试间隔。
const DISK_WAIT: Duration = Duration::from_secs(60);
/// 临时缓存 TTL 清理周期。
const CLEANUP_EVERY: Duration = Duration::from_secs(3600);
/// 入馆 rename 失败后的重试周期。
const PROMOTE_RETRY: Duration = Duration::from_secs(60);

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
    let st = state.clone();
    tokio::spawn(async move {
        promote_loop(st).await;
    });
}

async fn worker_loop(state: AppState, idx: usize) {
    loop {
        match library::next_queued_job(&state.library).await {
            Ok(Some(job)) => {
                // 书源冷却时不占着通道硬跑：把任务推到冷却结束，先处理别的书。
                if let Some(until) = cooling_until(&state, &job.source).await {
                    match library::defer_job(&state.library, &job.id, until).await {
                        Ok(true) => {
                            tracing::info!(
                                worker = idx,
                                job = %job.id,
                                source = %job.source,
                                until,
                                "library job deferred: source cooling"
                            );
                        }
                        Ok(false) => tokio::time::sleep(IDLE_POLL).await,
                        Err(e) => {
                            tracing::warn!(job = %job.id, error = %e, "defer library job failed");
                            tokio::time::sleep(IDLE_POLL).await;
                        }
                    }
                    continue;
                }
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
        match library::cleanup_temp_books(&state.library, state.cfg.library_temp_ttl_secs).await {
            Ok(r) if r.report.books > 0 => {
                let mut trashed = 0u64;
                for book_url in &r.books {
                    match state.store().move_to_trash(book_url).await {
                        Ok(true) => trashed += 1,
                        Ok(false) => {}
                        Err(e) => {
                            tracing::warn!(book = %book_url, error = %e, "trash expired book dir failed")
                        }
                    }
                }
                tracing::info!(
                    books = r.report.books,
                    trashed,
                    bytes = r.report.bytes,
                    "library temp cache cleaned"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "library temp cleanup failed"),
        }
    }
}

/// 入馆时目录 rename 可能被占用（Windows 阅读器/杀软）；标记文件在这里被重试。
async fn promote_loop(state: AppState) {
    loop {
        tokio::time::sleep(PROMOTE_RETRY).await;
        match state.store().retry_pending_promotions().await {
            Ok(0) => {}
            Ok(n) => tracing::info!(books = n, "pending library promotions completed"),
            Err(e) => tracing::warn!(error = %e, "retry pending promotions failed"),
        }
    }
}

/// 书源冷却结束时间（epoch ms）；不在冷却中返回 `None`。
async fn cooling_until(state: &AppState, source: &str) -> Option<i64> {
    if source.trim().is_empty() {
        return None;
    }
    let until: Option<i64> =
        sqlx::query_scalar("SELECT cooldown_until FROM source_health WHERE source_url = ?1")
            .bind(source)
            .fetch_optional(&state.shared)
            .await
            .ok()
            .flatten()
            .flatten();
    until.filter(|t| *t > library::now_ms())
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
    // 入馆时可能只带了 source：把来源字段补进馆藏书目，列表与后续读取才有源可用。
    library::adopt_source(&state.library, &job.book_url, &source).await?;

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

    // 目录即书：只有真正落进书目录的章节才算已缓存。
    let cached = match crate::library_store::usage::cached_chapter_urls(
        &state.store(),
        &job.book_url,
    )
    .await
    {
        Ok(Some(v)) => v,
        Ok(None) => std::collections::HashSet::new(),
        Err(e) => {
            tracing::warn!(book = %job.book_url, error = %e, "scan cached chapters from dir failed");
            std::collections::HashSet::new()
        }
    };

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
    // 目录是权威：按书目录重算占用。
    if let Err(e) =
        crate::library_store::usage::refresh_rollup(&state.library, &state.store(), &job.book_url)
            .await
    {
        tracing::warn!(book = %job.book_url, error = %e, "refresh usage failed");
    }
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
    if let Err(e) = crate::library_store::write::sync_toc(
        &state.library,
        &state.store(),
        &job.book_url,
        &chapters,
    )
    .await
    {
        tracing::warn!(book = %job.book_url, error = %e, "sync toc to book dir failed");
    }
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
        return fetch_comic(state, source, store, book_url, chapter).await;
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
    // 目录优先：正文写进书目录成功后不再往 `library_content` 写同一份。
    crate::routes::books::save_text_prefer_dir(&state.store(), &state.library, &c).await?;
    Ok(())
}

/// 漫画章节：抓图片清单 + 下载整章图片，直接写进书目录（`oread.json` 页表）。
///
/// 书目录是唯一副本：图片下载完整后交给 `save_comic_pages`，不写任何旧缓存表。
/// 单张超过 `IMAGE_KEEP_MAX` 的图只保留清单、不落盘（在线仍可经代理读到）。
/// 任何一张图抓取失败/缺页时不写页表，下次任务会再试。
async fn fetch_comic(
    state: &AppState,
    source: &crate::entities::BookSource,
    store: &crate::web_book::BookStore,
    book_url: &str,
    chapter: &BookChapter,
) -> anyhow::Result<()> {
    let mut imgs: crate::web_book::ChapterImages =
        crate::web_book::chapter_images_book(state.fetcher.as_ref(), source, chapter, Some(store))
            .await?;
    if imgs.images.is_empty() {
        anyhow::bail!("图片列表为空");
    }

    let max_body = state.cfg.image_max_body_bytes.max(1024);
    let referer = chapter.url.clone();
    let outcomes: Vec<(usize, anyhow::Result<Option<FetchedImage>>)> =
        stream::iter(imgs.images.iter().cloned().enumerate().map(|(i, img)| {
            let referer = referer.clone();
            async move {
                if img.url.trim().is_empty() {
                    return (i, Ok(None));
                }
                match crate::image_fetch::fetch(source, &img.url, Some(&referer), max_body).await {
                    Ok(f) if f.data.len() <= IMAGE_KEEP_MAX => (
                        i,
                        Ok(Some(FetchedImage {
                            data: f.data,
                            mime: f.mime,
                            width: f.width,
                            height: f.height,
                        })),
                    ),
                    // 超大图：跳过落盘，但清单里的 URL 仍保留（在线仍可经代理读到）。
                    Ok(_) => (i, Ok(None)),
                    Err(e) => (i, Err(e)),
                }
            }
        }))
        .buffer_unordered(IMAGE_PARALLEL)
        .collect()
        .await;

    // 按清单顺序回填，`buffer_unordered` 的完成顺序不可靠。
    let mut fetched: Vec<Option<FetchedImage>> = (0..imgs.images.len()).map(|_| None).collect();
    let mut failed = 0usize;
    let mut missing = 0usize;
    let mut total_bytes = 0usize;
    for (i, outcome) in outcomes {
        match outcome {
            Ok(Some(img)) => {
                total_bytes = total_bytes.saturating_add(img.data.len());
                let im = &mut imgs.images[i];
                im.bytes = Some(img.data.len() as u64);
                if im.width.is_none() {
                    im.width = img.width;
                }
                if im.height.is_none() {
                    im.height = img.height;
                }
                fetched[i] = Some(img);
            }
            Ok(None) => {
                // 超大图只留清单；空 URL 是源页本身没有这一页。
                if !imgs.images[i].url.trim().is_empty() {
                    missing += 1;
                }
            }
            Err(e) => {
                failed += 1;
                tracing::warn!(
                    book = %book_url,
                    chapter = %chapter.url,
                    image = %imgs.images[i].url,
                    error = %e,
                    "library comic image failed"
                );
            }
        }
    }

    if failed > 0 {
        anyhow::bail!("{failed} 张图片下载失败");
    }
    if total_bytes > IMAGE_CHAPTER_MAX_BYTES {
        anyhow::bail!(
            "本章图片总大小超过 {} MiB，未写入书目录",
            IMAGE_CHAPTER_MAX_BYTES / 1024 / 1024
        );
    }
    // 缺页（超大图/空页）时不写不完整的页表，避免离线阅读缺页；下次任务会再试
    // （超大图由在线代理兜底）。
    if missing > 0 {
        return Ok(());
    }

    let mut pages = Vec::with_capacity(imgs.images.len());
    for (img, fetched) in imgs.images.iter().zip(fetched) {
        if img.url.trim().is_empty() {
            continue;
        }
        let Some(fetched) = fetched else {
            // `missing == 0` 时不该发生；防御性跳过，保持整章原子。
            anyhow::bail!("图片尚未下载: {}", img.url);
        };
        pages.push(crate::library_store::write::ComicPageInput {
            data: fetched.data,
            mime: fetched.mime,
            width: fetched.width.or(img.width),
            height: fetched.height.or(img.height),
            source_url: Some(img.url.clone()),
        });
    }
    match crate::library_store::write::save_comic_pages(
        &state.store(),
        book_url,
        &chapter.url,
        &pages,
    )
    .await
    {
        Ok(true) => {
            // 页表已完整落盘，这一章离线可读；统计按目录重算。
            if let Err(e) = crate::library_store::usage::refresh_rollup(
                &state.library,
                &state.store(),
                book_url,
            )
            .await
            {
                tracing::warn!(book = %book_url, error = %e, "refresh comic usage failed");
            }
        }
        // 书目录还没建（TOC 未同步）：图片只在内存里，下次任务重下。
        Ok(false) => {
            tracing::warn!(book = %book_url, chapter = %chapter.url, "book dir not ready; comic pages discarded")
        }
        Err(e) => {
            tracing::warn!(book = %book_url, chapter = %chapter.url, error = %e, "write comic pages to dir failed")
        }
    }
    Ok(())
}
