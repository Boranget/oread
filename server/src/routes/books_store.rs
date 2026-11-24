//! books 路由的 store 部分（move-only 从 books.rs 拆出）。
//!
//! 内容层（书目源字段 / 目录 / 正文）落在馆藏库 `library.db`，全局共享；
//! 用户层（分组、自定义封面/简介/标签、排序、`dur_*`）仍在用户库 `book_meta`。
//! 写书时两边各写自己那份，读书时以馆藏为准、用用户库覆盖个人字段。

use super::*;

use crate::library::{self, LibraryBook};

// ---------------- 书目 ----------------

/// 馆藏书目行 → `Book`（用户字段留默认值，由 `overlay_user_fields` 覆盖）。
fn book_from_library(l: &LibraryBook) -> Book {
    Book {
        book_url: l.book_url.clone(),
        toc_url: l.toc_url.clone(),
        origin: l.origin.clone(),
        origin_name: l.origin_name.clone(),
        name: l.name.clone(),
        author: l.author.clone(),
        kind: l.kind.clone(),
        cover_url: l.cover_url.clone(),
        intro: l.intro.clone(),
        charset: l.charset.clone(),
        book_source_type: l.book_source_type,
        latest_chapter_title: l.latest_chapter_title.clone(),
        latest_chapter_time: l.latest_chapter_time,
        total_chapter_num: l.total_chapter_num,
        word_count: l.word_count.clone(),
        ..Default::default()
    }
}

/// 把用户库里的个人字段覆盖到馆藏基线上。
///
/// 只覆盖用户字段：分组、自定义封面/简介/标签、排序、`can_update`、`dur_*`、`variable`。
/// 书源能重新提供的字段（书名/作者/封面/简介/章节数等）一律以馆藏为准。
fn overlay_user_fields(base: &mut Book, user: Book) {
    base.group = user.group;
    base.custom_tag = user.custom_tag;
    base.custom_cover_url = user.custom_cover_url;
    base.custom_intro = user.custom_intro;
    base.dur_chapter_title = user.dur_chapter_title;
    base.dur_chapter_index = user.dur_chapter_index;
    base.dur_chapter_pos = user.dur_chapter_pos;
    base.dur_chapter_time = user.dur_chapter_time;
    base.can_update = user.can_update;
    base.order = user.order;
    base.origin_order = user.origin_order;
    base.variable = user.variable;
    base.last_check_time = user.last_check_time;
    base.last_check_count = user.last_check_count;
}

/// 写入解析/抓取得到的书籍元信息。
///
/// 两处落库：
/// 1. `library.db.library_books`：书源可重新提供的字段（全局共享，冲突时保留更完整的一份）；
/// 2. 用户库 `book_meta`：整行照旧写入，作为书架/统计等本地查询的便捷副本，
///    同时承载分组、自定义封面/简介/标签、排序、`dur_*` 等个人字段。
///
/// 馆藏层的 `tier` / `collected_at` 不在这里改：入馆/移出是独立的管理动作。
pub(crate) async fn save_book_meta(
    lib: &SqlitePool,
    pool: &SqlitePool,
    b: &Book,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO library_books \
         (book_url, toc_url, origin, origin_name, name, author, kind, cover_url, intro, charset, \
          book_source_type, latest_chapter_title, latest_chapter_time, total_chapter_num, \
          word_count, last_accessed_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16) \
         ON CONFLICT(book_url) DO UPDATE SET \
          toc_url=COALESCE(excluded.toc_url, library_books.toc_url), \
          origin=COALESCE(excluded.origin, library_books.origin), \
          origin_name=COALESCE(excluded.origin_name, library_books.origin_name), \
          name=COALESCE(NULLIF(excluded.name, ''), library_books.name), \
          author=COALESCE(NULLIF(excluded.author, ''), library_books.author), \
          kind=COALESCE(NULLIF(excluded.kind, ''), library_books.kind), \
          cover_url=COALESCE(NULLIF(excluded.cover_url, ''), library_books.cover_url), \
          intro=COALESCE(NULLIF(excluded.intro, ''), library_books.intro), \
          charset=COALESCE(NULLIF(excluded.charset, ''), library_books.charset), \
          book_source_type=CASE WHEN excluded.book_source_type > 0 \
                                THEN excluded.book_source_type ELSE library_books.book_source_type END, \
          latest_chapter_title=COALESCE(NULLIF(excluded.latest_chapter_title, ''), \
                                        library_books.latest_chapter_title), \
          latest_chapter_time=CASE WHEN excluded.latest_chapter_time > 0 \
                                   THEN excluded.latest_chapter_time \
                                   ELSE library_books.latest_chapter_time END, \
          total_chapter_num=CASE WHEN excluded.total_chapter_num > 0 \
                                 THEN excluded.total_chapter_num \
                                 ELSE library_books.total_chapter_num END, \
          word_count=COALESCE(NULLIF(excluded.word_count, ''), library_books.word_count), \
          last_accessed_at=excluded.last_accessed_at",
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
    .bind(&b.latest_chapter_title)
    .bind(b.latest_chapter_time)
    .bind(b.total_chapter_num)
    .bind(&b.word_count)
    .bind(library::now_ms())
    .execute(lib)
    .await?;

    // 用户库：整行照旧写入。这是个人视图的便捷副本，不承担跨账号共享。
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

/// 读书目：馆藏提供源字段，用户库覆盖个人字段。
///
/// 只有用户库有行（历史数据）时直接返回该行；只有馆藏有行时（别的账号抓过这本书）
/// 也能读到完整书目，第二个账号不必重新抓详情。
pub(crate) async fn load_book_meta(
    lib: &SqlitePool,
    pool: &SqlitePool,
    book_url: &str,
) -> anyhow::Result<Option<Book>> {
    let src = library::load_book(lib, book_url).await?;
    let user: Option<Book> =
        sqlx::query_as::<_, Book>("SELECT * FROM book_meta WHERE book_url = ?")
            .bind(book_url)
            .fetch_optional(pool)
            .await?;
    Ok(match (src, user) {
        (None, None) => None,
        (None, Some(user)) => Some(user),
        (Some(src), user) => {
            let mut book = book_from_library(&src);
            if let Some(user) = user {
                overlay_user_fields(&mut book, user);
            }
            Some(book)
        }
    })
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
///
/// 注意：这里用的是**用户库**——`book_vars` / `book_cache` 是书源登录态，
/// 共享会串账号，必须留在借书证里。
pub(crate) async fn book_store_for(pool: &SqlitePool, key: &str) -> web_book::BookStore {
    let (vars, cache) = load_book_scope(pool, key).await;
    let store = web_book::BookStore::new();
    store.seed(key.to_string(), vars, cache);
    store
}

// ---------------- 目录（馆藏） ----------------

/// 目录缓存读取：TTL 内存在任意章节则整体命中（一次 SELECT 避免 N 次查询）。
///
/// 馆藏层（`tier=collection`）不按 TTL 过期：书源失效后，已入馆的书仍要能打开目录。
pub(crate) async fn load_cached_chapters(
    lib: &SqlitePool,
    book_url: &str,
    ttl_secs: i64,
) -> anyhow::Result<Option<Vec<BookChapter>>> {
    let fresh: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM library_chapters \
         WHERE book_url = ?1 \
           AND (updated_at >= datetime('now', ?2) \
                OR EXISTS (SELECT 1 FROM library_books b \
                           WHERE b.book_url = ?1 AND b.tier = 'collection'))",
    )
    .bind(book_url)
    .bind(format!("-{ttl_secs} seconds"))
    .fetch_one(lib)
    .await?;
    if fresh == 0 {
        return Ok(None);
    }
    load_chapters(lib, book_url).await
}

/// 忽略 TTL 读取已缓存的目录：回源失败时作为兜底，避免书源失效就完全打不开。
pub(crate) async fn load_any_chapters(
    lib: &SqlitePool,
    book_url: &str,
) -> anyhow::Result<Option<Vec<BookChapter>>> {
    let exists: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM library_chapters WHERE book_url = ?1")
            .bind(book_url)
            .fetch_one(lib)
            .await?;
    if exists == 0 {
        return Ok(None);
    }
    load_chapters(lib, book_url).await
}

async fn load_chapters(
    lib: &SqlitePool,
    book_url: &str,
) -> anyhow::Result<Option<Vec<BookChapter>>> {
    let rows: Vec<BookChapter> = sqlx::query_as(
        "SELECT url, title, is_volume, base_url, book_url, \"index\", is_vip, is_pay, \
         resource_url, tag, start, \"end\", update_time, word_count FROM library_chapters \
         WHERE book_url = ?1 ORDER BY \"index\"",
    )
    .bind(book_url)
    .fetch_all(lib)
    .await?;
    library::touch_book(lib, book_url).await;
    Ok(Some(rows))
}

/// 目录整本刷新：先清旧章节再写入，避免目录缩短后残留脏数据。
///
/// 同时回写馆藏书目的 `total_chapter_num` / `checked_at`，让列表页不必再聚合目录表。
pub(crate) async fn save_chapters(
    lib: &SqlitePool,
    book_url: &str,
    chapters: &[BookChapter],
) -> anyhow::Result<()> {
    // 整本替换放在一个事务里：抓取/解析中途失败时不会留下半份目录。
    let mut tx = lib.begin().await?;
    sqlx::query("DELETE FROM library_chapters WHERE book_url = ?")
        .bind(book_url)
        .execute(&mut *tx)
        .await?;
    for c in chapters {
        sqlx::query(
            "INSERT INTO library_chapters \
             (book_url, url, title, is_volume, base_url, \"index\", is_vip, is_pay, resource_url, tag, start, \"end\", update_time, word_count, updated_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14, datetime('now')) \
             ON CONFLICT(book_url, url) DO UPDATE SET title=excluded.title, \
              is_volume=excluded.is_volume, base_url=excluded.base_url, \
              \"index\"=excluded.\"index\", is_vip=excluded.is_vip, is_pay=excluded.is_pay, \
              resource_url=excluded.resource_url, tag=excluded.tag, start=excluded.start, \
              \"end\"=excluded.\"end\", update_time=excluded.update_time, \
              word_count=excluded.word_count, updated_at=datetime('now')",
        )
        .bind(book_url)
        .bind(&c.url)
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
        .execute(&mut *tx)
        .await?;
    }
    let total = chapters.len() as i64;
    sqlx::query(
        "INSERT INTO library_books (book_url, total_chapter_num, checked_at, last_accessed_at) \
         VALUES (?1, ?2, ?3, ?3) \
         ON CONFLICT(book_url) DO UPDATE SET \
          total_chapter_num = CASE WHEN excluded.total_chapter_num > 0 \
                                   THEN excluded.total_chapter_num \
                                   ELSE library_books.total_chapter_num END, \
          checked_at = excluded.checked_at, last_accessed_at = excluded.last_accessed_at",
    )
    .bind(book_url)
    .bind(total)
    .bind(library::now_ms())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// 回写总章节数（只增不减，避免空目录把已有值清零）。
///
/// 馆藏层是唯一权威，同时把副本写回用户库 `book_meta`，
/// 让书架进度百分比等既有查询不必改口径。
pub(crate) async fn sync_total_chapter_num(
    lib: &SqlitePool,
    pool: &SqlitePool,
    book_url: &str,
    total: i64,
) -> anyhow::Result<()> {
    if total <= 0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO library_books (book_url, total_chapter_num) VALUES (?1, ?2) \
         ON CONFLICT(book_url) DO UPDATE SET total_chapter_num = excluded.total_chapter_num",
    )
    .bind(book_url)
    .bind(total)
    .execute(lib)
    .await?;
    let _ = sqlx::query(
        "INSERT INTO book_meta (book_url, total_chapter_num) VALUES (?1, ?2) \
         ON CONFLICT(book_url) DO UPDATE SET total_chapter_num = excluded.total_chapter_num",
    )
    .bind(book_url)
    .bind(total)
    .execute(pool)
    .await;
    Ok(())
}

/// 取书籍名称（用于替换规则的 scope 匹配）。
pub(crate) async fn book_name_of(lib: &SqlitePool, book_url: &str) -> anyhow::Result<String> {
    let name: Option<String> =
        sqlx::query_scalar("SELECT name FROM library_books WHERE book_url = ?")
            .bind(book_url)
            .fetch_optional(lib)
            .await?;
    Ok(name.unwrap_or_default())
}

// ---------------- 正文（馆藏） ----------------

/// 判定抓取结果是否为空：文本章节按渲染结果判断（去掉标签/`&nbsp;` 后无可见字符）；
/// 图片章节 images 列表为空。空结果不缓存，避免把源站临时占位固化下来。
pub(crate) fn content_is_empty(c: &BookContent) -> bool {
    if c.content_type.trim() == "image" {
        super::books_content::decode_cached_images(c.images_json.as_deref())
            .0
            .is_empty()
    } else {
        let html = c.content.as_deref().unwrap_or("");
        // 只有插图的章节也是有效内容：`text_is_blank` 只数可见字符，会把 <img> 当成空。
        text_is_blank(html)
            && !crate::library_store::write::markup_to_blocks(html)
                .iter()
                .any(boranlib_ebook::html::Block::is_image)
    }
}

/// 文本正文是否没有可见字符：`<p>正文</p>` 不算空，`<div class="text">   </div>`
/// （`@textNodes` 为空时回退 `@html` 的产物）算空。
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

/// 把抓到的文本正文写进书目录，并增量维护缓存统计。
///
/// 书目录是正文的唯一副本：返回 `true` 表示目录已持有这一章；`false` 表示
/// 目录里还没有这本书/这一章（TOC 未同步或正文为空），本次只返回抓取结果、
/// 不落盘也不写数据库。
pub(crate) async fn save_text_prefer_dir(
    store: &crate::library_store::Store,
    lib: &SqlitePool,
    c: &BookContent,
) -> anyhow::Result<bool> {
    if c.content_type.trim() == "image" {
        return Ok(false);
    }
    let book_url = c.book_url.trim();
    if book_url.is_empty() || c.url.trim().is_empty() {
        return Ok(false);
    }
    let blocks = crate::library_store::write::markup_to_blocks(c.content.as_deref().unwrap_or(""));
    match crate::library_store::write::save_text_chapter_tracked(store, book_url, &c.url, &blocks)
        .await
    {
        Ok(Some(write)) => {
            if let Err(e) = crate::library_store::usage::note_text_chapter(
                lib,
                book_url,
                write.was_cached,
                write.size_delta,
            )
            .await
            {
                tracing::warn!(
                    book = %book_url,
                    url = %c.url,
                    error = %e,
                    "update cached chapter stats failed"
                );
            }
            crate::cache_metrics::record_save();
            Ok(true)
        }
        Ok(None) => Ok(false),
        Err(e) => Err(e.into()),
    }
}
