//! books 路由的 info 部分（move-only 从 books.rs 拆出）。

use super::*;

/// 发现：前端传的是书源 ID（`Discover` 页的 select），展开该源 `exploreUrl` 的第一个分类；
/// 若传入的是 http(s) URL，则按同一分类 URL 并发查询所有发现类书源。
pub(super) async fn explore(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<ExploreQ>,
) -> AppResult<Json<Vec<SearchBook>>> {
    let sources = load_sources(&s.shared).await?;
    let page = q.page.unwrap_or(1).max(1);
    if q.url.starts_with("http://") || q.url.starts_with("https://") {
        return Ok(Json(web_book::explore_all(&sources, &q.url).await));
    }
    let source = sources
        .iter()
        .find(|x| x.book_source_url == q.url)
        .ok_or_else(|| AppError::NotFound("source not found".into()))?;
    let cats = web_book::explore_categories(source, page)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    let url = cats
        .first()
        .map(|c| c.url.clone())
        .ok_or_else(|| AppError::BadRequest("source has no expandable exploreUrl".into()))?;
    Ok(Json(web_book::explore(source, &url).await?))
}

/// 发现分类列表：把书源 `exploreUrl` 展开为 `{title, url}`，供前端渲染分类导航。
pub(super) async fn explore_categories(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<CategoryQ>,
) -> AppResult<Json<Vec<web_book::ExploreCategory>>> {
    let source = load_source(&s.shared, &q.source).await?;
    let cats = web_book::explore_categories(&source, q.page.unwrap_or(1).max(1))
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Json(cats))
}

pub(super) async fn book_info(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<BookQ>,
) -> AppResult<Json<Book>> {
    let sources = load_sources(&s.shared).await?;
    let source = sources
        .into_iter()
        .find(|x| x.book_source_url == q.source)
        .ok_or_else(|| AppError::NotFound("source not found".into()))?;
    let mut book = Book {
        book_url: q.book_url.clone(),
        origin: Some(source.book_source_url.clone()),
        origin_name: Some(source.book_source_name.clone()),
        book_source_type: source.book_source_type,
        name: q.name.clone().filter(|s| !s.trim().is_empty()),
        author: q.author.clone().filter(|s| !s.trim().is_empty()),
        intro: q.intro.clone().filter(|s| !s.trim().is_empty()),
        cover_url: q.cover_url.clone().filter(|s| !s.trim().is_empty()),
        ..Default::default()
    };
    // 已有 book_meta（书架/上次详情）里的字段也作为兜底，规则为空时不丢
    if let Some(m) = load_book_meta(&db.0, &q.book_url).await? {
        book.name = book.name.or(m.name);
        book.author = book.author.or(m.author);
        book.intro = book.intro.or(m.intro);
        book.cover_url = book.cover_url.or(m.cover_url);
        book.kind = book.kind.or(m.kind);
        book.toc_url = book.toc_url.or(m.toc_url);
        // 详情接口要回显目录已写入的元信息，否则前端看到的 total_chapter_num 永远是 0
        book.total_chapter_num = m.total_chapter_num;
        book.word_count = book.word_count.or(m.word_count);
        book.latest_chapter_title = book.latest_chapter_title.or(m.latest_chapter_title);
        book.latest_chapter_time = m.latest_chapter_time;
    }
    let key = web_book::book_key(&source, &book.book_url);
    let store = book_store_for(&db.0, &key).await;
    web_book::book_info_book(&source, &mut book, Some(&store)).await?;
    persist_book_changes(&db.0, store.take_changes()).await;
    save_book_meta(&db.0, &book).await?;
    Ok(Json(book))
}

/// 目录：优先命中 `book_chapters` 的 TTL 缓存；过期或缺失时抓取并整本替换缓存。
/// `use_replace` 与 `/api/book/content` 同名：显式开关覆盖账号设置，缺省沿用
/// `use_replace_rule`；净化只作用于返回标题，缓存保留原文。
pub(super) async fn toc(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<BookQ>,
) -> AppResult<Json<Vec<BookChapter>>> {
    let use_replace = resolve_use_replace(q.use_replace.as_deref(), &db.0).await;
    let mut chapters = if let Some(chaps) =
        load_cached_chapters(&db.0, &q.book_url, s.cfg.toc_ttl_secs).await?
    {
        // 历史书目录已在缓存里但 total_chapter_num 仍为 0：一次目录访问就地补齐，
        // 不必等缓存过期重抓。
        let _ = sync_total_chapter_num(&db.0, &q.book_url, chaps.len() as i64).await;
        chaps
    } else {
        let source = load_source(&s.shared, &q.source).await?;
        let mut book = Book {
            book_url: q.book_url.clone(),
            ..Default::default()
        };
        if let Some(m) = load_book_meta(&db.0, &q.book_url).await? {
            book.toc_url = m.toc_url;
        }
        let key = web_book::book_key(&source, &book.book_url);
        let store = book_store_for(&db.0, &key).await;
        let chapters = web_book::chapter_list_book(&source, &book, Some(&store)).await?;
        persist_book_changes(&db.0, store.take_changes()).await;
        save_chapters(&db.0, &q.book_url, &chapters).await?;
        // 目录落库时同步总章节数，供进度百分比与书架完成度使用
        sync_total_chapter_num(&db.0, &q.book_url, chapters.len() as i64).await?;
        // 记录目录总数变化（首次建基线不出角标）
        crate::routes::bookshelf::record_toc_delta(&db.0, &q.book_url, chapters.len() as i64).await;
        // 已缓存过正文的章节顺手回填字数，避免 toc 阶段只能给出 0
        let _ = backfill_word_counts(&db.0, &q.book_url).await;
        chapters
    };
    if use_replace && !chapters.is_empty() {
        let book_name = book_name_of(&db.0, &q.book_url).await.unwrap_or_default();
        let mut cleaned: Vec<String> = chapters
            .iter()
            .map(|c| c.title.clone().unwrap_or_default())
            .collect();
        replace::purify_titles(&db.0, &book_name, &q.source, &mut cleaned).await;
        for (c, t) in chapters.iter_mut().zip(cleaned) {
            // 错误/空标题保持原样，避免把 None 变成空串
            if c.title
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
            {
                c.title = Some(t);
            }
        }
    }
    enrich_volumes(&mut chapters);
    Ok(Json(chapters))
}

/// 分卷分组：分卷行用自己的标题，普通章节继承最近的前置分卷标题。
pub(crate) fn enrich_volumes(chapters: &mut [BookChapter]) {
    let mut current: Option<String> = None;
    for c in chapters.iter_mut() {
        if c.is_volume {
            current = c.title.clone();
            c.volume_title = c.title.clone();
        } else {
            c.volume_title = current.clone();
        }
    }
}

/// 回写总章节数。只在 total > 0 时写，避免空目录把已有值清零；
/// upsert 用 `book_meta` 自身值兜底，凡是详情/书架等路径都不可能再把它降成 0。
pub(super) async fn sync_total_chapter_num(
    pool: &SqlitePool,
    book_url: &str,
    total: i64,
) -> anyhow::Result<()> {
    if total <= 0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO book_meta (book_url, total_chapter_num) VALUES (?1, ?2) \
         ON CONFLICT(book_url) DO UPDATE SET total_chapter_num = excluded.total_chapter_num",
    )
    .bind(book_url)
    .bind(total)
    .execute(pool)
    .await?;
    Ok(())
}

/// 目录写入后，用已缓存正文的长度回填章节字数（SQLite `length()` 对 TEXT 按字符计数）。
pub(super) async fn backfill_word_counts(pool: &SqlitePool, book_url: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE book_chapters SET word_count = COALESCE((\
           SELECT length(trim(content)) FROM book_content \
           WHERE book_content.url = book_chapters.url \
             AND book_content.book_url = book_chapters.book_url \
             AND content IS NOT NULL\
         ), 0) WHERE book_url = ?1",
    )
    .bind(book_url)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------- 批量刷新目录 ----------------

#[derive(Deserialize)]
pub struct TocBatchQ {
    /// 书籍 URL 列表；也接受 Legado 风格的 book_ids
    #[serde(default, alias = "book_ids")]
    pub book_urls: Vec<String>,
    /// 可选统一书源；缺省按每本书的 book_meta.origin
    pub source: Option<String>,
}

pub(crate) const TOC_BATCH_MAX: usize = 100;
pub(crate) const TOC_BATCH_CONCURRENCY: usize = 3;

#[derive(Default)]
pub(super) struct TocBatchOutcome {
    pub(super) book_url: String,
    pub(super) status: &'static str,
    pub(super) chapters: usize,
    pub(super) error: Option<String>,
    /// 刷新前章节总数
    pub(super) before_total: i64,
    /// 刷新后章节总数
    pub(super) after_total: i64,
    /// 新增章节数（按章节 URL 比对）
    pub(super) added: i64,
    /// 删除章节数（按章节 URL 比对）
    pub(super) removed: i64,
}

impl TocBatchOutcome {
    pub(super) fn to_json(&self) -> serde_json::Value {
        json!({
            "book_url": self.book_url,
            "status": self.status,
            "chapters": self.chapters,
            "before_total": self.before_total,
            "after_total": self.after_total,
            "added": self.added,
            "removed": self.removed,
            "total_delta": self.after_total - self.before_total,
            "error": self.error,
        })
    }
}

/// 批量重新抓目录并写库：逐本独立，单本失败不影响其他；并发 3。
/// 抓取沿用全局 `req` 限制（单请求 5 MiB / 15 s）。
pub(super) async fn toc_batch(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<TocBatchQ>,
) -> AppResult<Json<serde_json::Value>> {
    let started = Instant::now();
    let source_override = q
        .source
        .clone()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let mut outcomes: Vec<TocBatchOutcome> = Vec::new();
    let mut urls: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for raw in q.book_urls {
        let u = raw.trim().to_string();
        if u.is_empty() {
            continue;
        }
        if !seen.insert(u.clone()) {
            outcomes.push(TocBatchOutcome {
                book_url: u,
                status: "skipped",
                chapters: 0,
                error: Some("重复的 book_url".into()),
                ..Default::default()
            });
            continue;
        }
        urls.push(u);
    }
    if urls.is_empty() {
        return Err(AppError::BadRequest("book_urls 不能为空".into()));
    }
    if urls.len() > TOC_BATCH_MAX {
        return Err(AppError::BadRequest(format!(
            "单次最多 {TOC_BATCH_MAX} 本，请分批调用"
        )));
    }

    let pool = db.0.clone();
    let shared = s.shared.clone();
    let done: Vec<TocBatchOutcome> = stream::iter(urls.into_iter().map(|book_url| {
        let pool = pool.clone();
        let shared = shared.clone();
        let source_override = source_override.clone();
        async move {
            let meta = match load_book_meta(&pool, &book_url).await {
                Ok(m) => m,
                Err(e) => {
                    return TocBatchOutcome {
                        book_url,
                        status: "failed",
                        chapters: 0,
                        error: Some(format!("读取书籍元信息失败：{e}")),
                        ..Default::default()
                    }
                }
            };
            // 书架上标记为"停用更新"的书不参与批量刷新（手动点单本目录仍可刷新）
            let shelf_flag: Option<i64> =
                sqlx::query_scalar("SELECT enable_update FROM bookshelf WHERE book_id = ?1")
                    .bind(&book_url)
                    .fetch_optional(&pool)
                    .await
                    .unwrap_or(None);
            if matches!(shelf_flag, Some(0)) {
                return TocBatchOutcome {
                    book_url,
                    status: "skipped",
                    chapters: 0,
                    error: Some("已停用更新".into()),
                    ..Default::default()
                };
            }
            let source_id = match source_override
                .clone()
                .or_else(|| meta.as_ref().and_then(|m| m.origin.clone()))
                .filter(|s| !s.trim().is_empty())
            {
                Some(s) => s,
                None => {
                    return TocBatchOutcome {
                        book_url,
                        status: "skipped",
                        chapters: 0,
                        error: Some("缺少书源，且 book_meta 未记录 origin".into()),
                        ..Default::default()
                    }
                }
            };
            let source = match load_source(&shared, &source_id).await {
                Ok(s) => s,
                Err(_) => {
                    return TocBatchOutcome {
                        book_url,
                        status: "skipped",
                        chapters: 0,
                        error: Some(format!("书源不存在：{source_id}")),
                        ..Default::default()
                    }
                }
            };
            let mut book = Book {
                book_url: book_url.clone(),
                ..Default::default()
            };
            if let Some(m) = meta {
                book.toc_url = m.toc_url;
            }
            let chapters = match web_book::chapter_list(&source, &book).await {
                Ok(c) => c,
                Err(e) => {
                    return TocBatchOutcome {
                        book_url,
                        status: "failed",
                        chapters: 0,
                        error: Some(format!("抓取目录失败：{e}")),
                        ..Default::default()
                    }
                }
            };
            if chapters.is_empty() {
                return TocBatchOutcome {
                    book_url,
                    status: "failed",
                    chapters: 0,
                    error: Some("目录为空".into()),
                    ..Default::default()
                };
            }
            // 保存前记录旧目录，用于计算新增/删除章节（总数变化之外的可核对增量）
            let before_total: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM book_chapters WHERE book_url = ?1")
                    .bind(&book_url)
                    .fetch_optional(&pool)
                    .await
                    .unwrap_or(None)
                    .unwrap_or(0);
            let old_urls: HashSet<String> =
                sqlx::query_scalar("SELECT url FROM book_chapters WHERE book_url = ?1")
                    .bind(&book_url)
                    .fetch_all(&pool)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
            if let Err(e) = save_chapters(&pool, &book_url, &chapters).await {
                return TocBatchOutcome {
                    book_url,
                    status: "failed",
                    chapters: 0,
                    error: Some(format!("写入目录失败：{e}")),
                    before_total,
                    ..Default::default()
                };
            }
            let _ = sync_total_chapter_num(&pool, &book_url, chapters.len() as i64).await;
            let _ = backfill_word_counts(&pool, &book_url).await;
            let after_total = chapters.len() as i64;
            let new_urls: HashSet<&str> = chapters.iter().map(|c| c.url.as_str()).collect();
            let added = new_urls.iter().filter(|u| !old_urls.contains(**u)).count() as i64;
            let removed = old_urls
                .iter()
                .filter(|u| !new_urls.contains(u.as_str()))
                .count() as i64;
            crate::routes::bookshelf::record_toc_delta(&pool, &book_url, after_total).await;
            TocBatchOutcome {
                book_url,
                status: "updated",
                chapters: chapters.len(),
                error: None,
                before_total,
                after_total,
                added,
                removed,
            }
        }
    }))
    .buffer_unordered(TOC_BATCH_CONCURRENCY)
    .collect()
    .await;
    outcomes.extend(done);
    let updated = outcomes.iter().filter(|o| o.status == "updated").count();
    let failed = outcomes.iter().filter(|o| o.status == "failed").count();
    let skipped = outcomes.iter().filter(|o| o.status == "skipped").count();
    Ok(Json(json!({
        "ok": true,
        "total": outcomes.len(),
        "updated": updated,
        "failed": failed,
        "skipped": skipped,
        "results": outcomes.iter().map(|o| o.to_json()).collect::<Vec<_>>(),
        "elapsed_ms": started.elapsed().as_millis() as u64,
    })))
}
