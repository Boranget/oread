//! books 路由的 store 部分（move-only 从 books.rs 拆出）。

use super::*;

// ---------------- 缓存落库辅助 ----------------

/// 写入解析/抓取得到的书籍元信息。
///
/// 这是“解析路径”：只更新来源可重新提供的字段，`group` / `can_update` / `order` /
/// `origin_order` / `variable` / `custom_tag` / `custom_cover_url` / `custom_intro` /
/// `dur_chapter_*` 属于用户或本地阅读状态，冲突时一律保留旧值（首次插入仍按传入值落库）。
/// 可空字段用 COALESCE 兜底，避免规则缺字段时把已有书名/作者等清空。
pub(crate) async fn save_book_meta(pool: &SqlitePool, b: &Book) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO book_meta \
         (book_url, toc_url, origin, origin_name, name, author, kind, cover_url, intro, charset, \
          book_source_type, \"group\", latest_chapter_title, latest_chapter_time, total_chapter_num, \
          dur_chapter_title, dur_chapter_index, dur_chapter_pos, dur_chapter_time, word_count, \
          can_update, \"order\", origin_order, variable, custom_tag, custom_cover_url, custom_intro) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27) \
         ON CONFLICT(book_url) DO UPDATE SET \
          toc_url=COALESCE(excluded.toc_url, book_meta.toc_url), \
          origin=COALESCE(excluded.origin, book_meta.origin), \
          origin_name=COALESCE(excluded.origin_name, book_meta.origin_name), \
          name=COALESCE(NULLIF(excluded.name, ''), book_meta.name), \
          author=COALESCE(NULLIF(excluded.author, ''), book_meta.author), \
          kind=COALESCE(NULLIF(excluded.kind, ''), book_meta.kind), \
          cover_url=COALESCE(NULLIF(excluded.cover_url, ''), book_meta.cover_url), \
          intro=COALESCE(NULLIF(excluded.intro, ''), book_meta.intro), \
          charset=COALESCE(NULLIF(excluded.charset, ''), book_meta.charset), \
          book_source_type=CASE WHEN excluded.book_source_type > 0 \
                                THEN excluded.book_source_type ELSE book_meta.book_source_type END, \
          latest_chapter_title=COALESCE(NULLIF(excluded.latest_chapter_title, ''), \
                                        book_meta.latest_chapter_title), \
          latest_chapter_time=CASE WHEN excluded.latest_chapter_time > 0 \
                                   THEN excluded.latest_chapter_time \
                                   ELSE book_meta.latest_chapter_time END, \
          total_chapter_num=CASE WHEN excluded.total_chapter_num > 0 THEN excluded.total_chapter_num \
                                 ELSE book_meta.total_chapter_num END, \
          word_count=COALESCE(NULLIF(excluded.word_count, ''), book_meta.word_count)",
    )
    .bind(&b.book_url)
    .bind(&b.toc_url)
    .bind(&b.origin)
    .bind(&b.origin_name)
    .bind(&b.name)
    .bind(&b.author)
    .bind(&b.kind)
    .bind(&b.cover_url)
    .bind(&b.intro)
    .bind(&b.charset)
    .bind(b.book_source_type)
    .bind(&b.group)
    .bind(&b.latest_chapter_title)
    .bind(b.latest_chapter_time)
    .bind(b.total_chapter_num)
    .bind(&b.dur_chapter_title)
    .bind(b.dur_chapter_index)
    .bind(b.dur_chapter_pos)
    .bind(b.dur_chapter_time)
    .bind(&b.word_count)
    .bind(b.can_update)
    .bind(b.order)
    .bind(b.origin_order)
    .bind(&b.variable)
    .bind(&b.custom_tag)
    .bind(&b.custom_cover_url)
    .bind(&b.custom_intro)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn load_book_meta(
    pool: &SqlitePool,
    book_url: &str,
) -> anyhow::Result<Option<Book>> {
    let b: Option<Book> = sqlx::query_as::<_, Book>("SELECT * FROM book_meta WHERE book_url = ?")
        .bind(book_url)
        .fetch_optional(pool)
        .await?;
    Ok(b)
}

// ---------------- 跨请求 book 层状态（变量 + cache） ----------------
/// cache 的默认有效期：10 分钟（上游 CacheManager 的内存缓存同量级）。
pub(super) const BOOK_CACHE_TTL_MS: i64 = 10 * 60 * 1000;
/// 单本书变量 JSON 上限 64 KiB，超限截断并告警。
pub(super) const BOOK_VARS_MAX_BYTES: usize = 64 * 1024;

/// 预载一本书的跨请求状态：持久化变量 + 未过期的 cache。
pub(super) async fn load_book_scope(
    pool: &SqlitePool,
    key: &str,
) -> (HashMap<String, String>, HashMap<String, String>) {
    let vars_json: Option<String> =
        sqlx::query_scalar("SELECT vars_json FROM book_vars WHERE book_key = ?1")
            .bind(key)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    let vars = vars_json
        .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
        .unwrap_or_default();
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT cache_key, value FROM book_cache WHERE book_key = ?1 AND expires_at > ?2",
    )
    .bind(key)
    .bind(source_health::now_ms())
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    (vars, rows.into_iter().collect())
}

/// 把编排层收集到的增量落库（UPSERT 语义；失败只记日志，不影响主结果）。
pub(crate) async fn persist_book_changes(pool: &SqlitePool, changes: Vec<web_book::BookChange>) {
    let now = source_health::now_ms();
    for ch in changes {
        if let Some(vars) = ch.vars {
            let mut json = serde_json::to_string(&vars).unwrap_or_else(|_| "{}".into());
            if json.len() > BOOK_VARS_MAX_BYTES {
                // 超限：按键名排序后保留尽可能多的项（不产生非法 JSON）
                let mut keys: Vec<&String> = vars.keys().collect();
                keys.sort();
                let mut kept = serde_json::Map::new();
                for k in keys {
                    kept.insert(k.clone(), serde_json::Value::String(vars[k].clone()));
                    if serde_json::to_string(&kept)
                        .map(|s| s.len())
                        .unwrap_or(usize::MAX)
                        > BOOK_VARS_MAX_BYTES
                    {
                        kept.remove(k);
                        break;
                    }
                }
                json = serde_json::to_string(&kept).unwrap_or_else(|_| "{}".into());
                tracing::warn!(
                    book_key = %ch.key,
                    bytes = json.len(),
                    limit = BOOK_VARS_MAX_BYTES,
                    "book vars truncated"
                );
            }
            if let Err(e) = sqlx::query(
                "INSERT INTO book_vars (book_key, vars_json, updated_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(book_key) DO UPDATE SET vars_json = excluded.vars_json, \
                 updated_at = excluded.updated_at",
            )
            .bind(&ch.key)
            .bind(&json)
            .bind(now)
            .execute(pool)
            .await
            {
                tracing::warn!(error = %e, "save book vars failed");
            }
        }
        for (ck, cv) in ch.cache_upserts {
            if let Err(e) = sqlx::query(
                "INSERT INTO book_cache (book_key, cache_key, value, expires_at) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(book_key, cache_key) DO UPDATE SET value = excluded.value, \
                 expires_at = excluded.expires_at",
            )
            .bind(&ch.key)
            .bind(&ck)
            .bind(&cv)
            .bind(now + BOOK_CACHE_TTL_MS)
            .execute(pool)
            .await
            {
                tracing::warn!(error = %e, "save book cache failed");
            }
        }
        for ck in ch.cache_deletes {
            if let Err(e) =
                sqlx::query("DELETE FROM book_cache WHERE book_key = ?1 AND cache_key = ?2")
                    .bind(&ch.key)
                    .bind(&ck)
                    .execute(pool)
                    .await
            {
                tracing::warn!(error = %e, "delete book cache failed");
            }
        }
    }
}

/// 构造并预载一本书的作用域容器。
pub(crate) async fn book_store_for(pool: &SqlitePool, key: &str) -> web_book::BookStore {
    let (vars, cache) = load_book_scope(pool, key).await;
    let store = web_book::BookStore::new();
    store.seed(key.to_string(), vars, cache);
    store
}

/// 目录缓存读取：TTL 内存在任意章节则整体命中（一次 SELECT 避免 N 次查询）。
pub(crate) async fn load_cached_chapters(
    pool: &SqlitePool,
    book_url: &str,
    ttl_secs: i64,
) -> anyhow::Result<Option<Vec<BookChapter>>> {
    let fresh: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM book_chapters \
         WHERE book_url = ?1 AND updated_at >= datetime('now', ?2)",
    )
    .bind(book_url)
    .bind(format!("-{ttl_secs} seconds"))
    .fetch_one(pool)
    .await?;
    if fresh == 0 {
        return Ok(None);
    }
    let rows: Vec<BookChapter> = sqlx::query_as(
        "SELECT url, title, is_volume, base_url, book_url, \"index\", is_vip, is_pay, \
         resource_url, tag, start, \"end\", update_time, word_count FROM book_chapters \
         WHERE book_url = ?1 ORDER BY \"index\"",
    )
    .bind(book_url)
    .fetch_all(pool)
    .await?;
    Ok(Some(rows))
}

/// 目录整本刷新：先清旧章节再写入，避免目录缩短后残留脏数据（写入放大优先让位于内存峰值）。
pub(crate) async fn save_chapters(
    pool: &SqlitePool,
    book_url: &str,
    chapters: &[BookChapter],
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM book_chapters WHERE book_url = ?")
        .bind(book_url)
        .execute(pool)
        .await?;
    for c in chapters {
        sqlx::query(
            "INSERT INTO book_chapters \
             (url, book_url, title, is_volume, base_url, \"index\", is_vip, is_pay, resource_url, tag, start, \"end\", update_time, word_count, updated_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14, datetime('now')) \
             ON CONFLICT(url) DO UPDATE SET title=excluded.title, is_volume=excluded.is_volume, \
              base_url=excluded.base_url, \"index\"=excluded.\"index\", is_vip=excluded.is_vip, \
              is_pay=excluded.is_pay, resource_url=excluded.resource_url, tag=excluded.tag, \
              start=excluded.start, \"end\"=excluded.\"end\", update_time=excluded.update_time, \
              word_count=excluded.word_count, updated_at=datetime('now')",
        )
        .bind(&c.url)
        .bind(&c.book_url)
        .bind(&c.title)
        .bind(c.is_volume)
        .bind(&c.base_url)
        .bind(c.index)
        .bind(c.is_vip)
        .bind(c.is_pay)
        .bind(&c.resource_url)
        .bind(&c.tag)
        .bind(c.start)
        .bind(c.end)
        .bind(&c.update_time)
        .bind(c.word_count)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// 正文解析管线各步骤的版本号。抽取策略、清洗步骤、替换规则等任一变更时，
/// 递增对应数字即可让全量旧缓存失效，无需人工清库，也不按时间粗暴失效。
pub(super) const CONTENT_PIPELINE_STEPS: &[(&str, u32)] = &[
    ("extract", 1), // 规则抽取：@textNodes 空结果回退 @html
    ("clean", 2),   // 空白/换行清洗与字符计数
    ("replace", 1), // 书源替换规则
];

/// 当前解析管线指纹（`p1-<fnv64>`）。存进 `book_content.cache_version`；
/// 读取时指纹不符或缺失（旧记录）都按未命中处理并重建。
pub(crate) fn content_cache_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        let joined = CONTENT_PIPELINE_STEPS
            .iter()
            .map(|(name, v)| format!("{name}:{v}"))
            .collect::<Vec<_>>()
            .join(",");
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in joined.as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("p1-{hash:016x}")
    })
}

/// 正文缓存读取：TTL 内命中原管线指纹则更新 accessed_at（LRU 依据）。
/// `origin` 是书源 URL，与章节 `url` 共同构成缓存键，避免镜像源同 URL 串正文。
pub(crate) async fn load_cached_content(
    pool: &SqlitePool,
    url: &str,
    origin: &str,
    ttl_secs: i64,
) -> anyhow::Result<Option<BookContent>> {
    let row: Option<BookContent> = sqlx::query_as(
        "SELECT url, book_url, title, content, chapter_index, resource_url, is_local, \
         content_type, images, cache_version \
         FROM book_content WHERE url = ?1 AND origin = ?2 \
           AND updated_at >= datetime('now', ?3)",
    )
    .bind(url)
    .bind(origin)
    .bind(format!("-{ttl_secs} seconds"))
    .fetch_optional(pool)
    .await?;
    if let Some(row) = row {
        // 空正文（文本为空 / 图片章节零图）视为缓存未命中：旧版本的空白缓存不再命中，
        // 每次读取都会重新抓取，避免整个 TTL 周期内一直白屏。
        if content_is_empty(&row) {
            return Ok(None);
        }
        // 解析管线变了（或旧记录没有指纹）：旧结果可能残缺，按未命中重建。
        if row.cache_version.as_deref() != Some(content_cache_version()) {
            tracing::debug!(
                url,
                cached = row.cache_version.as_deref().unwrap_or("<none>"),
                current = content_cache_version(),
                "content cache version mismatch, refetch"
            );
            return Ok(None);
        }
        let _ = sqlx::query(
            "UPDATE book_content SET accessed_at = datetime('now') WHERE url = ?1 AND origin = ?2",
        )
        .bind(url)
        .bind(origin)
        .execute(pool)
        .await;
        return Ok(Some(row));
    }
    Ok(None)
}

/// 判定缓存正文是否为空：文本章节按渲染结果判断（去掉标签/`&nbsp;` 后无可见字符）；
/// 图片章节 images 列表为空。
pub(super) fn content_is_empty(c: &BookContent) -> bool {
    if c.content_type.trim() == "image" {
        decode_cached_images(c.images_json.as_deref()).0.is_empty()
    } else {
        text_is_blank(c.content.as_deref().unwrap_or(""))
    }
}

/// 文本正文是否没有可见字符：`<p>正文</p>` 不算空，`<div class="text">   </div>`
/// （`@textNodes` 为空时回退 `@html` 的产物）算空，避免把渲染后空白的正文写进缓存。
pub(super) fn text_is_blank(s: &str) -> bool {
    let mut visible = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => visible.push(ch),
            _ => {}
        }
    }
    visible.replace("&nbsp;", " ").trim().is_empty()
}

/// 单条正文 upsert（不做淘汰），供 `/api/book/content` 与整本缓存共用。
/// 缓存键为 `(url, origin)`；调用方必须先把 `c.origin` 填成书源 URL。
pub(crate) async fn upsert_content(pool: &SqlitePool, c: &BookContent) -> anyhow::Result<()> {
    let content_type = if c.content_type.trim().is_empty() {
        "text"
    } else {
        c.content_type.as_str()
    };
    let size = (c.content.as_deref().map(|s| s.len()).unwrap_or(0)
        + c.images_json.as_deref().map(|s| s.len()).unwrap_or(0)) as i64;
    sqlx::query(
        "INSERT INTO book_content \
         (url, origin, book_url, title, content, chapter_index, resource_url, is_local, \
          content_type, images, size_bytes, cache_version, updated_at, accessed_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12, datetime('now'), datetime('now')) \
         ON CONFLICT(url, origin) DO UPDATE SET \
          book_url=CASE WHEN excluded.book_url <> '' THEN excluded.book_url \
                        ELSE book_content.book_url END, \
          title=excluded.title, content=excluded.content, \
          chapter_index=excluded.chapter_index, resource_url=excluded.resource_url, \
          is_local=excluded.is_local, content_type=excluded.content_type, \
          images=excluded.images, size_bytes=excluded.size_bytes, \
          cache_version=excluded.cache_version, \
          updated_at=datetime('now'), accessed_at=datetime('now')",
    )
    .bind(&c.url)
    .bind(&c.origin)
    .bind(&c.book_url)
    .bind(&c.title)
    .bind(&c.content)
    .bind(c.chapter_index)
    .bind(&c.resource_url)
    .bind(c.is_local)
    .bind(content_type)
    .bind(&c.images_json)
    .bind(size)
    .bind(content_cache_version())
    .execute(pool)
    .await?;
    // 正文字数回填到目录行（未知时保持 0）；SQLite TEXT 的 length() 按字符计数。
    let chars = c.content.as_deref().unwrap_or("").trim().chars().count() as i64;
    let _ = sqlx::query(
        "UPDATE book_chapters SET word_count = ?2 \
         WHERE url = ?1 AND (?3 = '' OR book_url = ?3)",
    )
    .bind(&c.url)
    .bind(chars)
    .bind(&c.book_url)
    .execute(pool)
    .await;
    Ok(())
}

/// 正文落库 + 总量上限淘汰：超过 `limit_bytes` 时按 accessed_at 升序删除最久未读章节。
pub(crate) async fn save_content_cached(
    pool: &SqlitePool,
    c: &BookContent,
    limit_bytes: i64,
) -> anyhow::Result<()> {
    // 空正文不落库：源站临时占位/规则失效时抓到的空结果只影响本次请求，
    // 后续重试与换源不会被这份空缓存挡住。
    if content_is_empty(c) {
        tracing::debug!(url = c.url.as_str(), "skip caching empty chapter content");
        return Ok(());
    }
    upsert_content(pool, c).await?;

    // 淘汰：每轮删除最久未读的一批，最多 8 轮，避免极端情况下长时间占用连接
    for _ in 0..8 {
        let total: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(size_bytes),0) FROM book_content")
            .fetch_one(pool)
            .await?;
        if total <= limit_bytes {
            break;
        }
        let deleted = sqlx::query(
            "DELETE FROM book_content WHERE rowid IN \
             (SELECT rowid FROM book_content ORDER BY accessed_at ASC LIMIT 16)",
        )
        .execute(pool)
        .await?
        .rows_affected();
        if deleted == 0 {
            break;
        }
        tracing::info!(total, limit = limit_bytes, deleted, "content cache evicted");
        // 增量归还有界空间（256 页 ≈ 1 MiB），避免文件只涨不落；非增量库为空操作。
        let _ = crate::db::reclaim_pages(pool, 256).await;
    }
    Ok(())
}

/// 取书籍名称（用于替换规则的 scope 匹配）
pub(super) async fn book_name_of(pool: &SqlitePool, book_url: &str) -> anyhow::Result<String> {
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM book_meta WHERE book_url = ?")
        .bind(book_url)
        .fetch_optional(pool)
        .await?;
    Ok(name.unwrap_or_default())
}
