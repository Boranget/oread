//! books 路由的 content 部分（move-only 从 books.rs 拆出）。

use super::*;

/// 正文：命中缓存（TTL 内）直接返回；否则抓取、落库、按总量上限淘汰最久未读的章节，
/// 最后统一应用用户替换规则（标题/正文分别受 scope_title / scope_content 控制）。
///
/// 契约变更（向后兼容）：新增 `content_type`（text|image）、`images`（图片源正文）、
/// `truncated`（单章图片超过上限被截断）与 `placeholder_suspected`（付费/会员章节疑似占位图）。
/// 文本源仍返回原 `content` 且 `images: []`；图片源 `content` 为空串 + `images` 列表。
pub(super) async fn content(
    State(s): State<AppState>,
    u: AuthUser,
    db: UserDb,
    Query(q): Query<ContentQ>,
) -> AppResult<Json<serde_json::Value>> {
    // 分卷标题是抓取层合成的伪 URL（`volume:<book_url>#<index>`），没有正文可抓；
    // 提前返回业务化错误，避免 reqwest 报出看不懂的 `builder error`。
    if q.chapter_url.starts_with("volume:") {
        return Err(AppError::BadRequest(
            "该条为分卷标题，请从目录选择章节".into(),
        ));
    }
    let mut c = match super::books_store::peek_cached_content(
        &db.0,
        &q.chapter_url,
        &q.source,
        s.cfg.content_ttl_secs,
    )
    .await?
    {
        Some(c) => c,
        None => {
            // 同用户同章节并发未命中只抓取一次：等待者拿到闸门后先重查缓存。
            let _flight = crate::routes::inflight::acquire(format!(
                "{}|{}|{}",
                u.id, q.source, q.chapter_url
            ))
            .await;
            if let Some(c) = super::books_store::peek_cached_content(
                &db.0,
                &q.chapter_url,
                &q.source,
                s.cfg.content_ttl_secs,
            )
            .await?
            {
                c
            } else {
                // 真正触发抓取时才记一次 miss；并发等待者只记 hit，命中率不被稀释。
                crate::cache_metrics::record_miss();
                let source = load_source(&s.shared, &q.source).await?;
                let chapter = BookChapter {
                    url: q.chapter_url.clone(),
                    // 书籍地址用于按 book_key 预载跨请求变量/cache（老客户端可能不传）
                    book_url: q.book_url.clone().filter(|s| !s.trim().is_empty()),
                    ..Default::default()
                };
                let store = match chapter.book_url.as_deref() {
                    Some(bu) if !bu.trim().is_empty() => {
                        let key = web_book::book_key(&source, bu);
                        Some(book_store_for(&db.0, &key).await)
                    }
                    _ => None,
                };
                let mut c = if source.book_source_type == 2 {
                    // 漫画源：只抽图片列表，content 置空以保持字段类型不变
                    let imgs = web_book::chapter_images_book(
                        s.fetcher.as_ref(),
                        &source,
                        &chapter,
                        store.as_ref(),
                    )
                    .await?;
                    BookContent {
                        book_url: chapter.book_url.clone().unwrap_or_default(),
                        url: chapter.url.clone(),
                        title: chapter.title.clone(),
                        content: Some(String::new()),
                        chapter_index: chapter.index,
                        content_type: "image".into(),
                        images_json: Some(serde_json::to_string(&imgs)?),
                        ..Default::default()
                    }
                } else {
                    web_book::chapter_content_book(
                        s.fetcher.as_ref(),
                        &source,
                        &chapter,
                        store.as_ref(),
                    )
                    .await?
                };
                c.origin = source.book_source_url.clone();
                if let Some(st) = &store {
                    persist_book_changes(&db.0, st.take_changes()).await;
                }
                // 用户设置的缓存上限优先（且不超过全局上限），未设置时用全局配置
                let limit = match replace::user_cache_limit_mb(&db.0).await {
                    Some(mb) => (mb * 1024 * 1024).min(s.cfg.cache_limit_bytes()),
                    None => s.cfg.cache_limit_bytes(),
                };
                save_content_cached(&db.0, &c, limit).await?;
                c
            }
        }
    };
    if c.content_type.trim().is_empty() {
        c.content_type = "text".into();
    }
    let is_image = c.content_type == "image";
    // 验收 P2：content 只带 chapter_url 时 book_url/title 为空，导致按书名圈定的替换规则失效、
    // 响应字段缺失。这里用请求参数兜底，并在净化前补齐 book_url。
    if c.book_url.trim().is_empty() {
        if let Some(bu) = q.book_url.as_deref().filter(|s| !s.trim().is_empty()) {
            c.book_url = bu.to_string();
        }
    }
    // 缓存书可能没存 book_url：用章节 URL 反查 book_chapters（其主键就是书籍 URL）。
    if c.book_url.trim().is_empty() {
        let found: Option<String> = sqlx::query_scalar(
            "SELECT book_url FROM book_chapters WHERE url = ?1 AND book_url <> '' LIMIT 1",
        )
        .bind(&q.chapter_url)
        .fetch_optional(&db.0)
        .await
        .ok()
        .flatten();
        if let Some(bu) = found {
            c.book_url = bu;
        }
    }
    if c.title.is_none() {
        c.title = q.title.clone().filter(|s| !s.trim().is_empty());
    }
    if c.title.is_none() {
        c.title = sqlx::query_scalar("SELECT title FROM book_chapters WHERE url = ?1 LIMIT 1")
            .bind(&q.chapter_url)
            .fetch_optional(&db.0)
            .await
            .ok()
            .flatten()
            .filter(|s: &String| !s.trim().is_empty());
    }
    // 仍拿不到书籍地址时给明确错误码，而不是让前端猜。
    if c.book_url.trim().is_empty() {
        return Err(AppError::MissingBookUrl);
    }
    // 替换规则开关：请求参数可显式覆盖，缺省沿用账号设置
    let use_replace = resolve_use_replace(q.use_replace.as_deref(), &db.0).await;
    if is_image {
        // 图片章节不做正文净化/图片样式注入，避免把 <img> 列表当富文本改写
        let (images, truncated) = decode_cached_images(c.images_json.as_deref());
        let (is_pay, is_vip): (bool, bool) =
            sqlx::query_as("SELECT is_pay, is_vip FROM book_chapters WHERE url = ?1 LIMIT 1")
                .bind(&q.chapter_url)
                .fetch_optional(&db.0)
                .await
                .ok()
                .flatten()
                .unwrap_or((false, false));
        let placeholder_suspected = !images.is_empty() && (is_pay || is_vip);
        let prefetch = images.len().min(3);
        return Ok(Json(json!({
            "book_url": c.book_url,
            "url": c.url,
            "title": c.title,
            "content": c.content.unwrap_or_default(),
            "chapter_index": c.chapter_index,
            "content_type": "image",
            "images": images,
            "truncated": truncated,
            "placeholder_suspected": placeholder_suspected,
            "replace_applied": false,
            // 前端可按 priority 分批：前 3 张为 high，其余 normal
            "prefetch_hint": {"count": prefetch, "priority": "high"},
        })));
    }
    let book_name = book_name_of(&db.0, &c.book_url).await.unwrap_or_default();
    if use_replace {
        let (title, body) = replace::purify(
            &db.0,
            &book_name,
            &q.source,
            c.title.clone(),
            c.content.clone(),
        )
        .await;
        c.title = title;
        c.content = body;
    }
    let style = replace::user_image_style(&db.0).await;
    if let Some(b) = c.content.take() {
        c.content = Some(replace::apply_image_style(&b, style));
    }
    Ok(Json(json!({
        "book_url": c.book_url,
        "url": c.url,
        "title": c.title,
        "content": c.content,
        "chapter_index": c.chapter_index,
        "content_type": "text",
        "images": [],
        "truncated": false,
        "placeholder_suspected": false,
        "replace_applied": use_replace,
    })))
}

/// 解析图片缓存 JSON；兼容旧的裸数组形态，缺失/损坏时返回空列表。
pub(super) fn decode_cached_images(raw: Option<&str>) -> (Vec<crate::entities::BookImage>, bool) {
    let Some(raw) = raw else {
        return (Vec::new(), false);
    };
    if let Ok(v) = serde_json::from_str::<web_book::ChapterImages>(raw) {
        return (v.images, v.truncated);
    }
    if let Ok(list) = serde_json::from_str::<Vec<crate::entities::BookImage>>(raw) {
        return (list, false);
    }
    (Vec::new(), false)
}

// ---------------- 书内全文搜索（仅已缓存章节） ----------------

#[derive(Deserialize)]
pub struct BookSearchQ {
    pub book_id: String,
    pub q: String,
    pub limit: Option<usize>,
}

pub(super) const SEARCH_TIMEOUT_SECS: u64 = 5;
pub(super) const SEARCH_MAX_QUERY_CHARS: usize = 200;

/// 转义 LIKE 的 `%`/`_`/`\`，让用户输入按字面量匹配。
pub(super) fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// 找到首个命中并生成上下文片段。返回 `(snippet, 字符偏移, 本章命中次数)`。
/// SQL 的 LIKE 对 ASCII 大小写不敏感，这里做同样的回退查找。
pub(super) fn find_snippet(content: &str, q: &str) -> Option<(String, usize, usize)> {
    let lower = content.to_lowercase();
    let ql = q.to_lowercase();
    // 命中次数按 LIKE 语义做 ASCII 大小写不敏感统计
    let count = lower.matches(&ql).count();
    let byte = if let Some(idx) = content.find(q) {
        idx
    } else {
        let idx = lower.find(&ql)?;
        let char_idx = lower[..idx].chars().count();
        content
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(0)
    };
    let char_off = content[..byte].chars().count();
    let total = content.chars().count();
    let qlen = q.chars().count();
    let start = char_off.saturating_sub(40);
    let end = (char_off + qlen + 40).min(total);
    let mut snippet: String = content.chars().skip(start).take(end - start).collect();
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < total {
        snippet.push('…');
    }
    Some((snippet, char_off, count))
}

#[derive(sqlx::FromRow)]
pub(super) struct ContentSearchRow {
    pub(super) url: String,
    pub(super) title: Option<String>,
    pub(super) chapter_index: i64,
    pub(super) content: Option<String>,
}

pub(super) async fn book_search(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<BookSearchQ>,
) -> AppResult<Json<serde_json::Value>> {
    let book_id = q.book_id.trim();
    if book_id.is_empty() {
        return Err(AppError::BadRequest("book_id 不能为空".into()));
    }
    let query = q.q.trim();
    if query.is_empty() {
        return Err(AppError::BadRequest("q 不能为空".into()));
    }
    if query.chars().count() > SEARCH_MAX_QUERY_CHARS {
        return Err(AppError::BadRequest(format!(
            "q 最多 {SEARCH_MAX_QUERY_CHARS} 字"
        )));
    }
    let limit = q.limit.unwrap_or(20).clamp(1, 50);
    let started = Instant::now();
    let pattern = escape_like(query);
    let rows_fut = sqlx::query_as::<_, ContentSearchRow>(
        "SELECT url, title, chapter_index, content FROM book_content \
         WHERE book_url = ?1 AND content IS NOT NULL \
           AND content LIKE '%' || ?2 || '%' ESCAPE '\\' \
         ORDER BY chapter_index LIMIT ?3",
    )
    .bind(book_id)
    .bind(&pattern)
    .bind((limit + 1) as i64)
    .fetch_all(&db.0);
    let rows = tokio::time::timeout(Duration::from_secs(SEARCH_TIMEOUT_SECS), rows_fut)
        .await
        .map_err(|_| {
            AppError::Timeout(format!(
                "书内搜索超过 {SEARCH_TIMEOUT_SECS}s 预算，请缩短关键词"
            ))
        })??;
    let truncated = rows.len() > limit;
    let hits: Vec<serde_json::Value> = rows
        .into_iter()
        .take(limit)
        .filter_map(|row| {
            let content = row.content?;
            let (snippet, offset, match_count) = find_snippet(&content, query)?;
            Some(json!({
                "chapter_index": row.chapter_index,
                "chapter_url": row.url,
                "chapter_title": row.title,
                "match_count": match_count,
                "offset": offset,
                "snippet": snippet,
            }))
        })
        .collect();

    // 明确告诉调用方“只搜了已缓存部分”，避免把部分结果当成全书结果。
    let (total, cached): (i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM book_chapters WHERE book_url = ?1), \
           (SELECT COUNT(*) FROM book_content WHERE book_url = ?1 AND content IS NOT NULL)",
    )
    .bind(book_id)
    .fetch_one(&db.0)
    .await?;
    let uncached = (total - cached).max(0);
    // 目录未缓存时无法判断“全书是否搜全”，此时不能报告 complete=true。
    let complete = uncached == 0 && total > 0;
    let note = if total == 0 {
        "目录未缓存，无法判断是否覆盖全书；仅返回已缓存正文中的命中"
    } else if uncached == 0 {
        "已覆盖全部缓存章节"
    } else {
        "仅搜索已缓存章节，未缓存章节未参与检索"
    };
    Ok(Json(json!({
        "ok": true,
        "book_id": book_id,
        "query": query,
        "limit": limit,
        "truncated": truncated,
        "hits": hits,
        "total_chapters": total,
        "cached_chapters": cached,
        "uncached_chapters": uncached,
        "complete": complete,
        "note": note,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    })))
}

// ---------------- 整本缓存 / 缓存清理 ----------------

#[derive(Deserialize)]
pub struct CacheAllQ {
    #[serde(alias = "book_id")]
    pub book_url: String,
    pub source: Option<String>,
    /// 本次最多抓取的章节数（默认 200，上限 500），防止一次请求跑太久
    pub limit: Option<usize>,
}

#[derive(Deserialize, Default)]
pub struct CacheClearQ {
    /// 缺省表示清空该用户的全部正文缓存
    #[serde(default, alias = "book_url", alias = "book_id")]
    pub book_id: Option<String>,
}

/// 原子预占预算：成功则扣减并返回 true；不足返回 false。
pub(super) fn reserve_budget(remaining: &AtomicI64, size: i64) -> bool {
    let mut cur = remaining.load(Ordering::SeqCst);
    loop {
        if size > cur {
            return false;
        }
        match remaining.compare_exchange(cur, cur - size, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(v) => cur = v,
        }
    }
}

// `index`/`bytes` 供构造侧记录进度与诊断，部分分支只读取状态码而不读字段；保留完整载荷便于排查。
#[allow(dead_code)]
pub(super) enum CacheOutcome {
    Cached {
        index: i64,
        bytes: i64,
    },
    Failed {
        index: i64,
        url: String,
        title: Option<String>,
        error: String,
    },
    Budget {
        index: i64,
    },
}

pub(super) async fn cache_all(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<CacheAllQ>,
) -> AppResult<Json<serde_json::Value>> {
    let started = Instant::now();
    let book_url = q.book_url.trim().to_string();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    let chapters: Vec<BookChapter> = sqlx::query_as(
        "SELECT url, title, is_volume, base_url, book_url, \"index\", is_vip, is_pay, \
         resource_url, tag, start, \"end\", update_time, word_count FROM book_chapters \
         WHERE book_url = ?1 ORDER BY \"index\"",
    )
    .bind(&book_url)
    .fetch_all(&db.0)
    .await?;
    if chapters.is_empty() {
        return Err(AppError::BadRequest(
            "目录未缓存，请先调用 GET /api/book/toc".into(),
        ));
    }
    let source_id = match q.source.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => {
            let origin: Option<String> =
                sqlx::query_scalar("SELECT origin FROM book_meta WHERE book_url = ?1")
                    .bind(&book_url)
                    .fetch_optional(&db.0)
                    .await?;
            origin.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
                AppError::BadRequest("缺少 source，且 book_meta 未记录书源".into())
            })?
        }
    };
    let source = load_source(&s.shared, &source_id).await?;
    // 只有当前解析管线指纹的记录才算已缓存；旧指纹/无指纹的行交给预取重建。
    let cached_urls: Vec<String> = sqlx::query_scalar(
        "SELECT url FROM book_content WHERE book_url = ?1 AND content IS NOT NULL \
         AND cache_version = ?2",
    )
    .bind(&book_url)
    .bind(content_cache_version())
    .fetch_all(&db.0)
    .await?;
    let cached_set: HashSet<String> = cached_urls.into_iter().collect();
    let total_chapters = chapters.len();
    let mut pending: Vec<BookChapter> = chapters
        .into_iter()
        .filter(|c| !c.is_volume && !cached_set.contains(&c.url))
        .collect();
    let already_cached = total_chapters - pending.len();
    let max_chapters = q.limit.unwrap_or(200).clamp(1, 500);
    let limited = pending.len() > max_chapters;
    pending.truncate(max_chapters);

    let limit_bytes = match replace::user_cache_limit_mb(&db.0).await {
        Some(mb) => (mb * 1024 * 1024).min(s.cfg.cache_limit_bytes()),
        None => s.cfg.cache_limit_bytes(),
    };
    let used_before: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(size_bytes),0) FROM book_content")
            .fetch_one(&db.0)
            .await?;
    let attempted = pending.len();
    let remaining = Arc::new(AtomicI64::new((limit_bytes - used_before).max(0)));
    let exhausted = Arc::new(AtomicBool::new(false));
    let pool = db.0.clone();
    let src = source.clone();
    let bu = book_url.clone();

    let outcomes: Vec<CacheOutcome> = stream::iter(pending.into_iter().map(|ch| {
        let pool = pool.clone();
        let src = src.clone();
        let bu = bu.clone();
        let remaining = remaining.clone();
        let exhausted = exhausted.clone();
        async move {
            if exhausted.load(Ordering::SeqCst) {
                return CacheOutcome::Budget { index: ch.index };
            }
            let mut chapter = ch.clone();
            if chapter.book_url.as_deref().unwrap_or("").trim().is_empty() {
                chapter.book_url = Some(bu.clone());
            }
            let idx = chapter.index;
            let url = chapter.url.clone();
            let title = chapter.title.clone();
            match web_book::chapter_content(&src, &chapter).await {
                Ok(mut c) => {
                    if c.book_url.trim().is_empty() {
                        c.book_url = bu.clone();
                    }
                    if c.url.trim().is_empty() {
                        c.url = url.clone();
                    }
                    c.origin = src.book_source_url.clone();
                    c.chapter_index = idx;
                    let size = c.content.as_deref().map(|s| s.len()).unwrap_or(0) as i64;
                    // 与单章读取一致：空正文/零图不写缓存，避免整批预取把空白固化下来。
                    if content_is_empty(&c) {
                        return CacheOutcome::Cached {
                            index: idx,
                            bytes: 0,
                        };
                    }
                    if !reserve_budget(&remaining, size) {
                        exhausted.store(true, Ordering::SeqCst);
                        return CacheOutcome::Budget { index: idx };
                    }
                    match upsert_content(&pool, &c).await {
                        Ok(()) => CacheOutcome::Cached {
                            index: idx,
                            bytes: size,
                        },
                        Err(e) => CacheOutcome::Failed {
                            index: idx,
                            url,
                            title,
                            error: format!("{e}"),
                        },
                    }
                }
                Err(e) => CacheOutcome::Failed {
                    index: idx,
                    url,
                    title,
                    error: format!("{e}"),
                },
            }
        }
    }))
    .buffer_unordered(3)
    .collect()
    .await;

    let mut cached = 0usize;
    let mut budget_skipped = 0usize;
    let mut cached_bytes = 0i64;
    let mut failures: Vec<serde_json::Value> = Vec::new();
    for o in outcomes {
        match o {
            CacheOutcome::Cached { bytes, .. } => {
                cached += 1;
                cached_bytes += bytes;
            }
            CacheOutcome::Budget { .. } => budget_skipped += 1,
            CacheOutcome::Failed {
                index,
                url,
                title,
                error,
            } => failures.push(json!({
                "chapter_index": index,
                "chapter_url": url,
                "chapter_title": title,
                "error": error,
            })),
        }
    }
    failures.sort_by_key(|v| v["chapter_index"].as_i64().unwrap_or(0));
    Ok(Json(json!({
        "ok": true,
        "book_url": book_url,
        "source": source_id,
        "total_chapters": total_chapters,
        "already_cached": already_cached,
        "attempted": attempted,
        "cached": cached,
        "failed": failures.len(),
        "skipped_budget": budget_skipped,
        "limited": limited,
        "limit": max_chapters,
        "budget_bytes": limit_bytes,
        "used_bytes_before": used_before,
        "used_bytes_after": used_before + cached_bytes,
        "failures": failures,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    })))
}

/// 清空正文缓存：`book_id` 有值按书清，缺省清该用户全部。返回 (删除行数, 释放字节数)。
pub(crate) async fn clear_content_cache(
    pool: &SqlitePool,
    book_id: Option<&str>,
) -> anyhow::Result<(u64, i64)> {
    let scope = book_id.map(str::trim).filter(|s| !s.is_empty());
    let (deleted, freed) = match scope {
        Some(id) => {
            let freed: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(size_bytes),0) FROM book_content WHERE book_url = ?1",
            )
            .bind(id)
            .fetch_one(pool)
            .await?;
            let deleted = sqlx::query("DELETE FROM book_content WHERE book_url = ?1")
                .bind(id)
                .execute(pool)
                .await?
                .rows_affected();
            (deleted, freed)
        }
        None => {
            let freed: i64 =
                sqlx::query_scalar("SELECT COALESCE(SUM(size_bytes),0) FROM book_content")
                    .fetch_one(pool)
                    .await?;
            let deleted = sqlx::query("DELETE FROM book_content")
                .execute(pool)
                .await?
                .rows_affected();
            (deleted, freed)
        }
    };
    // 有界归还空闲页，避免文件只涨不落。
    let _ = crate::db::reclaim_pages(pool, 1024).await;
    Ok((deleted, freed))
}

pub(super) async fn cache_clear(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<CacheClearQ>,
) -> AppResult<Json<serde_json::Value>> {
    let scope = q
        .book_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (deleted, freed) = clear_content_cache(&db.0, scope).await?;
    Ok(Json(json!({
        "ok": true,
        "scope": if scope.is_some() { "book" } else { "user" },
        "book_id": scope,
        "deleted": deleted,
        "bytes_freed": freed,
    })))
}

/// 换源：按书名+作者在其他书源里搜索，返回匹配项（不自动切换，由前端选择）。
pub(super) async fn switch_source(
    State(s): State<AppState>,
    _u: AuthUser,
    Json(q): Json<SwitchQ>,
) -> AppResult<Json<Vec<SearchBook>>> {
    let sources = load_sources(&s.shared).await?;
    let all = web_book::search_all(&sources, &q.name, 1).await;
    let name = q.name.trim().to_lowercase();
    let author = q.author.as_deref().unwrap_or("").trim().to_lowercase();
    let candidates = all.into_iter().filter(|b| {
        let bn = b.name.as_deref().unwrap_or("").trim().to_lowercase();
        let ba = b.author.as_deref().unwrap_or("").trim().to_lowercase();
        let name_ok = bn == name || bn.contains(&name);
        let author_ok = author.is_empty() || ba == author;
        name_ok && author_ok
    });
    let out: Vec<SearchBook> = match q.exclude_source.as_deref() {
        Some(ex) => candidates
            .filter(|b| b.origin.as_deref() != Some(ex))
            .collect(),
        None => candidates.collect(),
    };
    Ok(Json(out))
}
