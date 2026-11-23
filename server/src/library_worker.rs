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
use crate::library_images;
use crate::routes::AppState;

/// 单章内图片下载并发。与章节并发相乘后仍远低于交互阅读的优先级。
const IMAGE_PARALLEL: usize = 3;
/// 落盘图片的单张上限：超过只保留清单，不进 `library_images`（与进程内 LRU 一致）。
const IMAGE_KEEP_MAX: usize = 2 * 1024 * 1024;
/// 单张图片的抓取结果：`(清单下标, Ok((字节数, 宽, 高)) | Err)`。
type ImageOutcome = (
    usize,
    anyhow::Result<Option<(i64, Option<u32>, Option<u32>)>>,
);

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

    // 图片章节的 `content` 是空串，不能只看 content IS NOT NULL；
    // `cached_chapter_urls` 对漫画额外要求 images_ready=1（图片真的全部落盘）。
    // 目录优先：已落进书目录的章节也算已缓存；目录不存在时回退旧表。
    let mut cached = match crate::library_store::usage::cached_chapter_urls(
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
    cached.extend(
        library::cached_chapter_urls(
            &state.library,
            &job.book_url,
            crate::routes::books::content_cache_version(),
        )
        .await?,
    );

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
    // 目录是权威：有目录时按目录重算占用；还没有目录的书回退旧表聚合。
    match crate::library_store::usage::refresh_rollup(&state.library, &state.store(), &job.book_url)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            library::refresh_rollup(&state.library, &job.book_url)
                .await
                .ok();
        }
        Err(e) => tracing::warn!(book = %job.book_url, error = %e, "refresh usage failed"),
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
    crate::routes::books::upsert_content(&state.library, &c).await?;
    // 目录优先：TOC 已经建过目录时，把归一化后的正文同步落盘。
    let paragraphs =
        crate::library_store::write::markup_to_paragraphs(c.content.as_deref().unwrap_or(""));
    if let Err(e) = crate::library_store::write::save_text_chapter(
        &state.store(),
        book_url,
        &chapter.url,
        &paragraphs,
    )
    .await
    {
        tracing::warn!(book = %book_url, url = %chapter.url, error = %e, "write chapter to dir failed");
    }
    Ok(())
}

/// 漫画章节：抓图片清单 + 把图片原始字节落进 `library_images`。
///
/// 续跑时优先复用库里已有的清单，不重新请求目录页；已落盘的图片按
/// `(url, origin)` 跳过。单张超过 `IMAGE_KEEP_MAX` 的图只保留清单、不落盘。
/// 任何一张图抓取失败都会让本章保持 `images_ready=0`，任务结束后可重试补齐。
async fn fetch_comic(
    state: &AppState,
    source: &crate::entities::BookSource,
    store: &crate::web_book::BookStore,
    book_url: &str,
    chapter: &BookChapter,
) -> anyhow::Result<()> {
    let origin = source.book_source_url.clone();
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT images FROM library_content \
         WHERE url = ?1 AND origin = ?2 AND content_type = 'image'",
    )
    .bind(&chapter.url)
    .bind(&origin)
    .fetch_optional(&state.library)
    .await?;
    let mut imgs: crate::web_book::ChapterImages = match existing
        .as_deref()
        .and_then(|s| serde_json::from_str::<crate::web_book::ChapterImages>(s).ok())
    {
        Some(v) if !v.images.is_empty() => v,
        _ => {
            crate::web_book::chapter_images_book(
                state.fetcher.as_ref(),
                source,
                chapter,
                Some(store),
            )
            .await?
        }
    };
    if imgs.images.is_empty() {
        anyhow::bail!("图片列表为空");
    }

    let mut c = BookContent {
        url: chapter.url.clone(),
        origin: origin.clone(),
        book_url: book_url.to_string(),
        title: chapter.title.clone(),
        content: Some(String::new()),
        chapter_index: chapter.index,
        content_type: "image".into(),
        images_json: Some(serde_json::to_string(&imgs)?),
        ..Default::default()
    };
    // 先落清单（并把完整性标记清零）：中途取消/重启后仍能从清单继续补图。
    crate::routes::books::upsert_content(&state.library, &c).await?;
    library_images::mark_chapter_pending(&state.library, &chapter.url, &origin).await?;

    let max_body = state.cfg.image_max_body_bytes.max(1024);
    let referer = chapter.url.clone();
    let outcomes: Vec<ImageOutcome> =
        stream::iter(imgs.images.iter().cloned().enumerate().map(|(i, img)| {
            let referer = referer.clone();
            let origin = origin.clone();
            async move {
                if img.url.trim().is_empty() {
                    return (i, Ok(None));
                }
                if let Ok(Some(m)) =
                    library_images::meta_of(&state.library, &img.url, &origin).await
                {
                    return (i, Ok(Some((m.size_bytes, m.width, m.height))));
                }
                match crate::image_fetch::fetch(source, &img.url, Some(&referer), max_body).await {
                    Ok(f) if f.data.len() <= IMAGE_KEEP_MAX => {
                        let saved = library_images::save(
                            &state.library,
                            book_url,
                            &img.url,
                            &origin,
                            &f.mime,
                            &f.data,
                            f.width,
                            f.height,
                        )
                        .await;
                        match saved {
                            Ok(n) => (i, Ok(Some((n, f.width, f.height)))),
                            Err(e) => (i, Err(e)),
                        }
                    }
                    // 超大图：跳过落盘，但清单里的 URL 仍保留（在线仍可经代理读到）。
                    Ok(_) => (i, Ok(None)),
                    Err(e) => (i, Err(e)),
                }
            }
        }))
        .buffer_unordered(IMAGE_PARALLEL)
        .collect()
        .await;

    let mut failed = 0usize;
    let mut missing = 0usize;
    for (i, outcome) in outcomes {
        match outcome {
            Ok(Some((size, w, h))) => {
                let im = &mut imgs.images[i];
                im.bytes = Some(size.max(0) as u64);
                if im.width.is_none() {
                    im.width = w;
                }
                if im.height.is_none() {
                    im.height = h;
                }
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

    c.images_json = Some(serde_json::to_string(&imgs)?);
    crate::routes::books::upsert_content(&state.library, &c).await?;
    if failed > 0 {
        anyhow::bail!("{failed} 张图片下载失败");
    }
    library_images::mark_chapter_ready(&state.library, &chapter.url, &origin).await?;
    // 整章图片都落库后才写书目录：部分成功不写页表，读路径继续回退 DB。
    if missing == 0 {
        match write_comic_chapter_dir(state, book_url, chapter, &imgs, &origin).await {
            Ok(true) => {
                // 目录已持有原始图片：删掉这一章的 BLOB，避免同一章占两份磁盘。
                let urls: Vec<String> = imgs.images.iter().map(|i| i.url.clone()).collect();
                if let Err(e) =
                    library_images::delete_urls(&state.library, book_url, &urls, &origin).await
                {
                    tracing::warn!(book = %book_url, chapter = %chapter.url, error = %e, "drop comic blobs failed");
                }
                // 占用改由目录统计（图片字节在文件里，不再有 BLOB 行）。
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
            // 书目录还没建（TOC 未同步）：保留 BLOB 给读路径回退。
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(book = %book_url, chapter = %chapter.url, error = %e, "write comic pages to dir failed")
            }
        }
    }
    Ok(())
}

/// 把刚下载完的一章漫画从 `library_images` 读回，写进书目录（`oread.json` 页表）。
///
/// 返回 `false` 表示书目录还没建（章节不在 spine 里），图片 BLOB 需要保留给读路径回退。
async fn write_comic_chapter_dir(
    state: &AppState,
    book_url: &str,
    chapter: &BookChapter,
    imgs: &crate::web_book::ChapterImages,
    origin: &str,
) -> anyhow::Result<bool> {
    let mut pages = Vec::with_capacity(imgs.images.len());
    for img in &imgs.images {
        if img.url.trim().is_empty() {
            continue;
        }
        let Some(stored) = library_images::load(&state.library, &img.url, origin).await? else {
            anyhow::bail!("图片尚未落盘: {}", img.url);
        };
        pages.push(crate::library_store::write::ComicPageInput {
            data: stored.data,
            mime: stored.mime,
            width: stored.width.or(img.width),
            height: stored.height.or(img.height),
            source_url: Some(img.url.clone()),
        });
    }
    crate::library_store::write::save_comic_pages(&state.store(), book_url, &chapter.url, &pages)
        .await
        .map_err(Into::into)
}
