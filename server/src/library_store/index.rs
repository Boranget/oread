//! `index.db`：书目录的派生索引，**可以随时删除重建**。
//!
//! 权威永远是书目录（`content.opf` / `oread.json`）：这里只缓存检索字段、修订号
//! 与章/页计数，让「列出馆藏、判断某本书是否已落盘、检查数据库与目录是否一致」
//! 不必每次解析所有 OPF。删除 `index.db` 不会丢任何内容：
//!
//! - 进程启动时后台重建一次；
//! - `boranlib doctor --rebuild-index` 手动重建。
//!
//! v1 不在每次写盘时同步索引：索引只服务诊断与后续的列表加速，允许短暂滞后，
//! 需要精确结果时用 `doctor` 重建。这样写路径不必为了缓存再引入一个事务点。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

use super::{comic, Store, Tier};

/// 索引文件名，放在 `data_dir/library/` 下。
pub const INDEX_FILE: &str = "index.db";

/// 一本书在索引里的投影。
#[derive(Debug, Clone, PartialEq)]
pub struct IndexRow {
    pub book_id: String,
    pub book_url: String,
    pub tier: String,
    /// `text` / `comic`
    pub kind: String,
    pub title: String,
    pub author: Option<String>,
    pub language: Option<String>,
    pub publisher: Option<String>,
    pub pubdate: Option<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub subjects: Vec<String>,
    pub description: Option<String>,
    pub revision: u64,
    /// 非卷标题的章节数。
    pub chapter_count: i64,
    /// 漫画总页数；文字书为 0。
    pub page_count: i64,
    pub cover_href: Option<String>,
    pub updated_at: i64,
}

/// 重建结果：给 CLI 与启动日志用。
#[derive(Debug, Default, Clone)]
pub struct RebuildReport {
    pub scanned: usize,
    pub written: usize,
    pub removed: usize,
    /// 目录存在但无法解析的条目，形如 `collection/<book_id>: 原因`。
    pub failed: Vec<String>,
    /// 空骨架目录（入馆 rename 失败留下的），跳过不算错误。
    pub skipped: Vec<String>,
}

/// 一致性检查结果。字段都是「需要人看一眼」的集合。
#[derive(Debug, Default, Clone)]
pub struct VerifyReport {
    /// 扫描到的非空书目录数。
    pub dirs: usize,
    /// `index.db` 行数。
    pub indexed: i64,
    /// 目录存在但无法解析。
    pub parse_failed: Vec<String>,
    /// 目录有、索引没有（索引滞后）。
    pub missing_in_index: Vec<String>,
    /// 索引有、目录已不存在（索引滞后）。
    pub stale_in_index: Vec<String>,
    /// 索引修订号与目录不一致（启动后编辑过，重建即可）。
    pub revision_drift: Vec<String>,
    /// `library.db` 有书目、但目录不存在（迁移期仍靠 DB 回退）。
    pub db_without_dir: Vec<String>,
    /// 目录存在、但 `library.db` 没有书目（用户在馆藏列表里看不到）。
    pub dir_without_db: Vec<String>,
}

impl VerifyReport {
    /// 有需要处理的问题。索引滞后本身不算：重建即可。
    pub fn has_problems(&self) -> bool {
        !self.parse_failed.is_empty() || !self.dir_without_db.is_empty()
    }
}

/// 馆藏列表排序：只用索引里稳定、与访问行为无关的字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Title,
    Author,
}

impl Sort {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "title" => Some(Self::Title),
            "author" => Some(Self::Author),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Author => "author",
        }
    }

    fn column(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Author => "COALESCE(author, '')",
        }
    }

    /// 行里参与游标的排序值，必须与 SQL 的 `column()` 一致。
    pub fn key(self, row: &IndexRow) -> &str {
        match self {
            Self::Title => &row.title,
            Self::Author => row.author.as_deref().unwrap_or(""),
        }
    }
}

const ROW_COLUMNS: &str =
    "book_id, book_url, tier, kind, title, author, language, publisher, pubdate, \
     series, series_index, subjects, description, revision, chapter_count, page_count, \
     cover_href, updated_at";

/// `index.db` 的句柄。克隆便宜（SQLite 连接池）。
#[derive(Debug, Clone)]
pub struct Index {
    pool: SqlitePool,
}

impl Index {
    /// 打开（必要时创建）`<library root>/index.db`。
    pub async fn open(root: &Path) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(root).await?;
        let opts = SqliteConnectOptions::new()
            .filename(root.join(INDEX_FILE))
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(opts)
            .await?;
        for sql in [
            "CREATE TABLE IF NOT EXISTS book_index (
                book_id       TEXT PRIMARY KEY,
                book_url      TEXT NOT NULL,
                tier          TEXT NOT NULL,
                kind          TEXT NOT NULL,
                title         TEXT NOT NULL,
                author        TEXT,
                language      TEXT,
                publisher     TEXT,
                pubdate       TEXT,
                series        TEXT,
                series_index  REAL,
                subjects      TEXT NOT NULL DEFAULT '[]',
                description   TEXT,
                revision      INTEGER NOT NULL,
                chapter_count INTEGER NOT NULL,
                page_count    INTEGER NOT NULL,
                cover_href    TEXT,
                updated_at    INTEGER NOT NULL
            )",
            "CREATE UNIQUE INDEX IF NOT EXISTS book_index_url ON book_index(book_url)",
            "CREATE INDEX IF NOT EXISTS book_index_updated ON book_index(updated_at)",
            "CREATE INDEX IF NOT EXISTS book_index_title ON book_index(title, book_url)",
        ] {
            sqlx::query(sql).execute(&pool).await?;
        }
        Ok(Self { pool })
    }

    /// 按 `book_url` 读一行；没有返回 `None`。
    pub async fn get(&self, book_url: &str) -> anyhow::Result<Option<IndexRow>> {
        let row = sqlx::query(
            "SELECT book_id, book_url, tier, kind, title, author, language, publisher, pubdate, \
             series, series_index, subjects, description, revision, chapter_count, page_count, \
             cover_href, updated_at FROM book_index WHERE book_url = ?1",
        )
        .bind(book_url.trim())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_from_sql))
    }

    /// 全部行，按书名排序。给后续的列表/搜索用。
    pub async fn rows(&self) -> anyhow::Result<Vec<IndexRow>> {
        let rows = sqlx::query(
            "SELECT book_id, book_url, tier, kind, title, author, language, publisher, pubdate, \
             series, series_index, subjects, description, revision, chapter_count, page_count, \
             cover_href, updated_at FROM book_index ORDER BY title, book_url",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_from_sql).collect())
    }

    /// 跨书名/作者/系列/主题/简介搜索，返回匹配的 `book_url`（按书名排序）。
    ///
    /// 只做 `LIKE` 子串匹配：个人馆藏规模下足够快，也不引入 FTS 的写放大。
    pub async fn search(&self, term: &str, limit: i64) -> anyhow::Result<Vec<String>> {
        let term = term.trim();
        if term.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!(
            "%{}%",
            term.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let rows = sqlx::query_scalar(
            "SELECT book_url FROM book_index \
             WHERE book_url <> '' AND (title LIKE ?1 ESCAPE '\\' OR author LIKE ?1 ESCAPE '\\' \
               OR series LIKE ?1 ESCAPE '\\' OR subjects LIKE ?1 ESCAPE '\\' \
               OR description LIKE ?1 ESCAPE '\\') \
             ORDER BY title, book_url LIMIT ?2",
        )
        .bind(pattern)
        .bind(limit.max(1))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 馆藏列表：按书名或作者 keyset 分页，可选关键词过滤（与搜索同口径）。
    ///
    /// 游标是 `(排序值, book_url)`；调用方负责把 `next_cursor` 原样传回。
    /// `tier` 为 `all` 时不限层级。
    pub async fn page(
        &self,
        tier: &str,
        term: Option<&str>,
        sort: Sort,
        after: Option<(&str, &str)>,
        limit: i64,
    ) -> anyhow::Result<Vec<IndexRow>> {
        let pattern = term.map(|term| {
            format!(
                "%{}%",
                term.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        });
        let sql = format!(
            "SELECT {ROW_COLUMNS} FROM book_index \
             WHERE (?1 = 'all' OR tier = ?1) \
               AND (?2 = '' OR title LIKE ?2 ESCAPE '\\' OR author LIKE ?2 ESCAPE '\\' \
                 OR series LIKE ?2 ESCAPE '\\' OR subjects LIKE ?2 ESCAPE '\\' \
                 OR description LIKE ?2 ESCAPE '\\') \
               AND (({col} > ?3) OR ({col} = ?3 AND book_url > ?4)) \
             ORDER BY {col} ASC, book_url ASC LIMIT ?5",
            col = sort.column()
        );
        let (key, url) = after.unwrap_or(("", ""));
        let rows = sqlx::query(&sql)
            .bind(tier)
            .bind(pattern.unwrap_or_default())
            .bind(key)
            .bind(url)
            .bind(limit.max(1))
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(row_from_sql).collect())
    }

    /// 刷新一本书；目录不存在时返回 `None` 并删除旧行。
    pub async fn upsert(&self, store: &Store, book_url: &str) -> anyhow::Result<Option<IndexRow>> {
        let Some(row) = row_for_url(store, book_url).await? else {
            self.remove(book_url).await?;
            return Ok(None);
        };
        self.write_row(&row).await?;
        Ok(Some(row))
    }

    pub async fn remove(&self, book_url: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM book_index WHERE book_url = ?1")
            .bind(book_url.trim())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 全量重建：扫描 `collection/` 与 `temp/`，解析每本书，删掉已不存在的行。
    pub async fn rebuild(&self, store: &Store) -> anyhow::Result<RebuildReport> {
        let mut report = RebuildReport::default();
        let mut seen = HashSet::new();

        for (tier, dir) in scan_dirs(store).await? {
            report.scanned += 1;
            let tag = format!("{}/{}", tier.as_str(), dir_name(&dir));
            match row_for_dir(store, tier, &dir).await {
                Ok(Some(row)) => {
                    if !row.book_url.is_empty() {
                        seen.insert(row.book_id.clone());
                        self.write_row(&row).await?;
                        report.written += 1;
                    } else {
                        report
                            .failed
                            .push(format!("{tag}: 目录里没有 book_url，无法索引"));
                    }
                }
                // 空骨架：入馆 rename 失败留下的，等后台重试，不算坏数据。
                Ok(None) => report.skipped.push(tag),
                Err(e) => report.failed.push(format!("{tag}: {e}")),
            }
        }

        let existing: Vec<String> = sqlx::query_scalar("SELECT book_id FROM book_index")
            .fetch_all(&self.pool)
            .await?;
        for id in existing {
            if seen.contains(&id) {
                continue;
            }
            // 目录还在（例如扫描之后才写入，或暂时解析失败）就保留旧行，
            // 宁可索引滞后，也不要删掉一本真实存在的书。
            if store.find_by_id(&id).is_some() {
                continue;
            }
            sqlx::query("DELETE FROM book_index WHERE book_id = ?1")
                .bind(&id)
                .execute(&self.pool)
                .await?;
            report.removed += 1;
        }
        Ok(report)
    }

    /// 把目录里有、`library.db` 里没有的书补登成书目。
    ///
    /// 手工放进 `collection/` / `temp/` 的书目录、或迁移时漏登的书，执行后就会出现在馆藏列表。
    /// 只插入缺失行，不覆盖已有书目；返回实际补登数量。
    pub async fn adopt_orphans(&self, library: &SqlitePool) -> anyhow::Result<usize> {
        let rows = self.rows().await?;
        if rows.is_empty() {
            return Ok(0);
        }
        let existing: HashSet<String> = sqlx::query_scalar("SELECT book_url FROM library_books")
            .fetch_all(library)
            .await?
            .into_iter()
            .collect();
        let now = now_ms();
        let mut adopted = 0;
        for row in rows {
            let book_url = row.book_url.trim();
            if book_url.is_empty() || existing.contains(book_url) {
                continue;
            }
            // 封面 URL 与管理员换封面后的格式一致：文本书在 OEBPS 下，带修订号失效缓存。
            let cover_url = row.cover_href.as_deref().map(|href| {
                comic::local_image_url_v(&row.book_id, &format!("OEBPS/{href}"), row.revision)
            });
            let comic = row.kind == "comic";
            let mut tx = library.begin().await?;
            let inserted = sqlx::query(
                "INSERT OR IGNORE INTO library_books (
                    book_url, name, author, intro, language, publisher, pubdate,
                    series_name, series_index, cover_url, book_source_type, total_chapter_num,
                    tier, collected_at, last_accessed_at, origin_name
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )
            .bind(book_url)
            .bind(&row.title)
            .bind(&row.author)
            .bind(&row.description)
            .bind(&row.language)
            .bind(&row.publisher)
            .bind(&row.pubdate)
            .bind(&row.series)
            .bind(row.series_index)
            .bind(&cover_url)
            .bind(if comic { 2i64 } else { 1i64 })
            .bind(row.chapter_count)
            .bind(&row.tier)
            .bind((row.tier == "collection").then_some(now))
            .bind(now)
            .bind("馆藏目录")
            .execute(&mut *tx)
            .await?
            .rows_affected();
            for tag in &row.subjects {
                sqlx::query("INSERT OR IGNORE INTO library_tags (book_url, tag) VALUES (?1, ?2)")
                    .bind(book_url)
                    .bind(tag)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
            if inserted > 0 {
                adopted += 1;
                tracing::info!(book = %book_url, tier = %row.tier, "adopted orphan dir into library.db");
            }
        }
        Ok(adopted)
    }

    /// 只读检查：目录 ↔ 索引 ↔ `library.db` 三方的差异。
    pub async fn verify(
        &self,
        store: &Store,
        library: &SqlitePool,
    ) -> anyhow::Result<VerifyReport> {
        let mut report = VerifyReport::default();
        let indexed = self.rows().await?;
        report.indexed = indexed.len() as i64;
        let by_url: std::collections::HashMap<&str, &IndexRow> =
            indexed.iter().map(|r| (r.book_url.as_str(), r)).collect();
        let mut dir_urls = HashSet::new();

        for (tier, dir) in scan_dirs(store).await? {
            let tag = format!("{}/{}", tier.as_str(), dir_name(&dir));
            match row_for_dir(store, tier, &dir).await {
                Ok(Some(row)) if !row.book_url.is_empty() => {
                    report.dirs += 1;
                    dir_urls.insert(row.book_url.clone());
                    match by_url.get(row.book_url.as_str()) {
                        None => report.missing_in_index.push(row.book_url),
                        Some(old) if old.revision != row.revision => {
                            report.revision_drift.push(row.book_url)
                        }
                        Some(_) => {}
                    }
                }
                Ok(Some(_)) => report
                    .parse_failed
                    .push(format!("{tag}: 目录里没有 book_url")),
                Ok(None) => {}
                Err(e) => report.parse_failed.push(format!("{tag}: {e}")),
            }
        }

        for row in &indexed {
            if !dir_urls.contains(&row.book_url) {
                report.stale_in_index.push(row.book_url.clone());
            }
        }

        // 数据库里登记了、但目录还没建：迁移期正常，读路径会回退 DB。
        let db_urls: Vec<String> = sqlx::query_scalar("SELECT book_url FROM library_books")
            .fetch_all(library)
            .await?;
        let db_set: HashSet<&str> = db_urls.iter().map(String::as_str).collect();
        for url in &db_urls {
            if store.find(url).is_none() {
                report.db_without_dir.push(url.clone());
            }
        }
        for url in &dir_urls {
            if !db_set.contains(url.as_str()) {
                report.dir_without_db.push(url.clone());
            }
        }
        Ok(report)
    }

    async fn write_row(&self, row: &IndexRow) -> anyhow::Result<()> {
        let subjects = serde_json::to_string(&row.subjects)?;
        sqlx::query(
            "INSERT INTO book_index (
                book_id, book_url, tier, kind, title, author, language, publisher, pubdate,
                series, series_index, subjects, description, revision, chapter_count,
                page_count, cover_href, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(book_id) DO UPDATE SET
                book_url = excluded.book_url,
                tier = excluded.tier,
                kind = excluded.kind,
                title = excluded.title,
                author = excluded.author,
                language = excluded.language,
                publisher = excluded.publisher,
                pubdate = excluded.pubdate,
                series = excluded.series,
                series_index = excluded.series_index,
                subjects = excluded.subjects,
                description = excluded.description,
                revision = excluded.revision,
                chapter_count = excluded.chapter_count,
                page_count = excluded.page_count,
                cover_href = excluded.cover_href,
                updated_at = excluded.updated_at",
        )
        .bind(&row.book_id)
        .bind(&row.book_url)
        .bind(&row.tier)
        .bind(&row.kind)
        .bind(&row.title)
        .bind(&row.author)
        .bind(&row.language)
        .bind(&row.publisher)
        .bind(&row.pubdate)
        .bind(&row.series)
        .bind(row.series_index)
        .bind(subjects)
        .bind(&row.description)
        .bind(row.revision as i64)
        .bind(row.chapter_count)
        .bind(row.page_count)
        .bind(&row.cover_href)
        .bind(row.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// 进程内按库根缓存索引句柄：写路径每章都要刷新，不能每次都新建连接池。
static SHARED: Lazy<tokio::sync::Mutex<HashMap<PathBuf, Index>>> =
    Lazy::new(|| tokio::sync::Mutex::new(HashMap::new()));

/// 取共享索引句柄（首次打开时建表）。测试/多数据目录按 root 区分。
pub async fn shared(root: &Path) -> anyhow::Result<Index> {
    let mut map = SHARED.lock().await;
    if let Some(index) = map.get(root) {
        return Ok(index.clone());
    }
    let index = Index::open(root).await?;
    map.insert(root.to_path_buf(), index.clone());
    Ok(index)
}

/// 写路径的增量刷新：失败只记日志，不影响已经落盘的保存结果。
pub async fn sync_book(store: &Store, book_url: &str) {
    let index = match shared(store.root()).await {
        Ok(index) => index,
        Err(e) => {
            tracing::warn!(error = %e, "index.db open failed; skip sync");
            return;
        }
    };
    if let Err(e) = index.upsert(store, book_url).await {
        tracing::warn!(book = %book_url, error = %e, "index.db sync failed");
    }
}

/// 删除/移出后清掉索引行；失败同样只记日志。
pub async fn forget_book(store: &Store, book_url: &str) {
    let index = match shared(store.root()).await {
        Ok(index) => index,
        Err(e) => {
            tracing::warn!(error = %e, "index.db open failed; skip forget");
            return;
        }
    };
    if let Err(e) = index.remove(book_url).await {
        tracing::warn!(book = %book_url, error = %e, "index.db forget failed");
    }
}

/// 启动时后台重建索引：失败只记日志，不影响服务可用性。
pub fn spawn_rebuild(store: Store) {
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        match Index::open(store.root()).await {
            Ok(index) => match index.rebuild(&store).await {
                Ok(r) => tracing::info!(
                    scanned = r.scanned,
                    written = r.written,
                    removed = r.removed,
                    failed = r.failed.len(),
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "index.db rebuilt"
                ),
                Err(e) => tracing::warn!(error = %e, "index.db rebuild failed"),
            },
            Err(e) => tracing::warn!(error = %e, "index.db open failed"),
        }
    });
}

// ---------------------------------------------------------------------------
// 目录 → 索引行
// ---------------------------------------------------------------------------

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 馆藏时间戳（毫秒），与 `library_books.collected_at/last_accessed_at` 同口径。
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 枚举 `collection/` 与 `temp/` 下的书目录（一层，不递归）。
async fn scan_dirs(store: &Store) -> std::io::Result<Vec<(Tier, PathBuf)>> {
    let mut out = Vec::new();
    for tier in [Tier::Collection, Tier::Temp] {
        let base = store.root().join(tier.as_str());
        let mut rd = match tokio::fs::read_dir(&base).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        while let Some(ent) = rd.next_entry().await? {
            if ent.file_type().await?.is_dir() {
                out.push((tier, ent.path()));
            }
        }
    }
    Ok(out)
}

async fn row_for_url(store: &Store, book_url: &str) -> anyhow::Result<Option<IndexRow>> {
    let Some((tier, dir)) = store.find(book_url) else {
        return Ok(None);
    };
    row_for_dir(store, tier, &dir).await
}

/// 读一个目录的权威元数据。空骨架返回 `None`，解析失败返回 `Err`。
async fn row_for_dir(store: &Store, tier: Tier, dir: &Path) -> anyhow::Result<Option<IndexRow>> {
    if dir.join("OEBPS").join("content.opf").is_file() {
        let meta = store.read_opf(dir).await?;
        return Ok(Some(IndexRow {
            book_id: super::Store::book_id(&meta.book_url),
            book_url: meta.book_url.clone(),
            tier: tier.as_str().to_string(),
            kind: "text".into(),
            title: meta.title,
            author: meta.author,
            language: Some(meta.language),
            publisher: meta.publisher,
            pubdate: meta.pubdate,
            series: meta.series,
            series_index: meta.series_index,
            subjects: meta.subjects,
            description: meta.description,
            revision: meta.revision,
            chapter_count: meta.chapters.iter().filter(|c| !c.is_volume).count() as i64,
            page_count: 0,
            cover_href: meta.cover.map(|c| c.href),
            updated_at: now_ts(),
        }));
    }
    if comic::is_comic_dir(dir) {
        // `read_meta` 的 hint 为空：目录里没有 `oread.json` 时反推不出 book_url。
        // 这种手工放入的 CBZ 仍按目录名记一行，`book_url` 为空，由调用方报告。
        let meta = comic::read_meta(dir, "").await?;
        let book_url = meta.book_url.trim().to_string();
        return Ok(Some(IndexRow {
            book_id: if book_url.is_empty() {
                dir_name(dir)
            } else {
                super::Store::book_id(&book_url)
            },
            book_url,
            tier: tier.as_str().to_string(),
            kind: "comic".into(),
            title: meta.title,
            author: meta.author,
            language: meta.language,
            publisher: meta.publisher,
            pubdate: meta.pubdate,
            series: meta.series,
            series_index: meta.series_index,
            subjects: meta.subjects,
            description: meta.description,
            revision: meta.revision,
            chapter_count: meta.chapters.iter().filter(|c| !c.is_volume).count() as i64,
            page_count: meta.chapters.iter().map(|c| c.pages.len() as i64).sum(),
            cover_href: None,
            updated_at: now_ts(),
        }));
    }
    Ok(None)
}

fn row_from_sql(row: sqlx::sqlite::SqliteRow) -> IndexRow {
    let subjects: String = row.get("subjects");
    IndexRow {
        book_id: row.get("book_id"),
        book_url: row.get("book_url"),
        tier: row.get("tier"),
        kind: row.get("kind"),
        title: row.get("title"),
        author: row.get("author"),
        language: row.get("language"),
        publisher: row.get("publisher"),
        pubdate: row.get("pubdate"),
        series: row.get("series"),
        series_index: row.get("series_index"),
        subjects: serde_json::from_str(&subjects).unwrap_or_default(),
        description: row.get("description"),
        revision: row.get::<i64, _>("revision").max(0) as u64,
        chapter_count: row.get("chapter_count"),
        page_count: row.get("page_count"),
        cover_href: row.get("cover_href"),
        updated_at: row.get("updated_at"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library_store::opf::OpfMeta;
    use crate::library_store::ChapterItem;
    use sqlx::sqlite::SqlitePoolOptions;

    fn temp_root(tag: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("oread-index-{tag}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn text_book(store: &Store, book_url: &str, title: &str) -> PathBuf {
        let mut meta = OpfMeta::new(book_url, title);
        meta.author = Some("作者".into());
        meta.subjects = vec!["科幻".into()];
        meta.chapters.push(ChapterItem {
            id: "ch-0001".into(),
            href: "chapters/ch-0001-a.xhtml".into(),
            title: "第一章".into(),
            url: Some(format!("{book_url}/1")),
            is_volume: false,
        });
        let dir = store
            .ensure_skeleton(Tier::Collection, &meta)
            .await
            .unwrap();
        meta.bump();
        store.write_opf(&dir, &meta).await.unwrap();
        dir
    }

    async fn comic_book(store: &Store, book_url: &str, title: &str) {
        let dir = store.dir_for(Tier::Temp, book_url);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let mut meta = comic::ComicMeta::new(book_url, title);
        meta.revision = 3;
        let mut chapter = comic::ComicChapter {
            id: "ch-1".into(),
            title: "第 1 话".into(),
            url: Some(format!("{book_url}/1")),
            is_volume: false,
            pages: Vec::new(),
        };
        for i in 1..=2 {
            chapter.pages.push(comic::ComicPage {
                href: format!("0001-{i:03}.jpg"),
                mime: "image/jpeg".into(),
                width: Some(800),
                height: Some(1200),
                bytes: Some(1024),
                source_url: None,
            });
        }
        meta.chapters.push(chapter);
        comic::write_meta(&dir, &meta).await.unwrap();
        comic::write_comic_info(&dir, &meta).await.unwrap();
    }

    #[tokio::test]
    async fn rebuild_indexes_text_and_comic() {
        let root = temp_root("rebuild");
        let store = Store::at(root.clone());
        store.create_layout().unwrap();
        text_book(&store, "https://example.com/t/1", "文字书").await;
        comic_book(&store, "https://example.com/c/1", "漫画").await;

        let index = Index::open(&root).await.unwrap();
        let report = index.rebuild(&store).await.unwrap();
        assert_eq!(report.scanned, 2);
        assert_eq!(report.written, 2);
        assert_eq!(report.removed, 0);
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        let text = index
            .get("https://example.com/t/1")
            .await
            .unwrap()
            .expect("文字书应已索引");
        assert_eq!(text.kind, "text");
        assert_eq!(text.title, "文字书");
        assert_eq!(text.author.as_deref(), Some("作者"));
        assert_eq!(text.subjects, vec!["科幻"]);
        assert_eq!(text.chapter_count, 1);
        assert_eq!(text.page_count, 0);
        assert_eq!(text.revision, 1);

        let comic = index
            .get("https://example.com/c/1")
            .await
            .unwrap()
            .expect("漫画应已索引");
        assert_eq!(comic.kind, "comic");
        assert_eq!(comic.tier, "temp");
        assert_eq!(comic.revision, 3);
        assert_eq!(comic.chapter_count, 1);
        assert_eq!(comic.page_count, 2);
    }

    #[tokio::test]
    async fn page_sorts_by_title_and_cursor_resumes() {
        let root = temp_root("page");
        let store = Store::at(root.clone());
        store.create_layout().unwrap();
        text_book(&store, "https://example.com/p/c", "C 书").await;
        text_book(&store, "https://example.com/p/a", "A 书").await;
        text_book(&store, "https://example.com/p/b", "B 书").await;

        let index = Index::open(&root).await.unwrap();
        index.rebuild(&store).await.unwrap();

        let first = index
            .page("collection", None, Sort::Title, None, 2)
            .await
            .unwrap();
        let titles: Vec<&str> = first.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, vec!["A 书", "B 书"]);

        let last = first.last().unwrap();
        let key = Sort::Title.key(last);
        let rest = index
            .page(
                "collection",
                None,
                Sort::Title,
                Some((key, last.book_url.as_str())),
                2,
            )
            .await
            .unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].title, "C 书");

        let hit = index
            .page("collection", Some("B"), Sort::Title, None, 10)
            .await
            .unwrap();
        assert_eq!(hit.len(), 1, "关键词过滤走索引字段");
        assert_eq!(hit[0].title, "B 书");

        // 作者相同：按 book_url 升序稳定续页。
        let by_author = index
            .page("collection", None, Sort::Author, None, 10)
            .await
            .unwrap();
        let urls: Vec<&str> = by_author.iter().map(|r| r.book_url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://example.com/p/a",
                "https://example.com/p/b",
                "https://example.com/p/c"
            ]
        );
    }

    #[tokio::test]
    async fn rebuild_drops_rows_for_removed_dirs() {
        let root = temp_root("stale");
        let store = Store::at(root.clone());
        store.create_layout().unwrap();
        text_book(&store, "https://example.com/t/2", "留下的").await;
        comic_book(&store, "https://example.com/c/2", "删掉的").await;

        let index = Index::open(&root).await.unwrap();
        index.rebuild(&store).await.unwrap();
        assert!(index
            .get("https://example.com/c/2")
            .await
            .unwrap()
            .is_some());

        // 模拟目录被外部删掉（不经过 Store 的 forget），rebuild 应清掉残留行。
        tokio::fs::remove_dir_all(store.dir_for(Tier::Temp, "https://example.com/c/2"))
            .await
            .unwrap();
        let report = index.rebuild(&store).await.unwrap();
        assert_eq!(report.removed, 1);
        assert!(index
            .get("https://example.com/c/2")
            .await
            .unwrap()
            .is_none());
        assert!(index
            .get("https://example.com/t/2")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn search_matches_subjects_and_escapes_like_wildcards() {
        let root = temp_root("search");
        let store = Store::at(root.clone());
        store.create_layout().unwrap();
        let mut meta = OpfMeta::new("https://example.com/s/1", "100% 指南");
        meta.description = Some("讲三体问题的科普".into());
        meta.subjects = vec!["科幻".into()];
        store
            .ensure_skeleton(Tier::Collection, &meta)
            .await
            .unwrap();

        let index = Index::open(&root).await.unwrap();
        index.rebuild(&store).await.unwrap();

        let by_subject = index.search("科幻", 10).await.unwrap();
        assert_eq!(by_subject, vec!["https://example.com/s/1"]);
        let by_desc = index.search("三体", 10).await.unwrap();
        assert_eq!(by_desc, vec!["https://example.com/s/1"]);
        assert!(index.search("不存在", 10).await.unwrap().is_empty());
        // `%` 按字面匹配，而不是当成 LIKE 通配符。
        let by_percent = index.search("%", 10).await.unwrap();
        assert_eq!(by_percent, vec!["https://example.com/s/1"]);
    }

    #[tokio::test]
    async fn sync_book_follows_edits_and_deletes() {
        let root = temp_root("sync");
        let store = Store::at(root.clone());
        store.create_layout().unwrap();
        let book_url = "https://example.com/sync/1";
        let dir = text_book(&store, book_url, "原标题").await;

        sync_book(&store, book_url).await;
        let index = Index::open(&root).await.unwrap();
        assert_eq!(index.get(book_url).await.unwrap().unwrap().title, "原标题");

        // 直接改 OPF（模拟管理员/外部编辑），再同步应看到新标题。
        let mut meta = store.read_opf(&dir).await.unwrap();
        meta.title = "新标题".into();
        meta.bump();
        store.write_opf(&dir, &meta).await.unwrap();
        sync_book(&store, book_url).await;
        let row = index.get(book_url).await.unwrap().unwrap();
        assert_eq!(row.title, "新标题");
        assert_eq!(row.revision, meta.revision);

        // 删除（移进 trash）后索引行应被清掉。
        store.move_to_trash(book_url).await.unwrap();
        assert!(index.get(book_url).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn adopt_orphans_inserts_missing_books_and_tags() {
        let root = temp_root("adopt");
        let store = Store::at(root.clone());
        store.create_layout().unwrap();
        text_book(&store, "https://example.com/adopt/1", "补登书").await;

        let index = Index::open(&root).await.unwrap();
        index.rebuild(&store).await.unwrap();

        let library = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(root.join("library.db"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        for sql in [
            "CREATE TABLE library_books (book_url TEXT PRIMARY KEY, name TEXT, author TEXT, \
             intro TEXT, language TEXT, publisher TEXT, pubdate TEXT, series_name TEXT, \
             series_index REAL, cover_url TEXT, book_source_type INTEGER NOT NULL DEFAULT 0, \
             total_chapter_num INTEGER NOT NULL DEFAULT 0, tier TEXT NOT NULL DEFAULT 'temp', \
             collected_at INTEGER, last_accessed_at INTEGER NOT NULL DEFAULT 0, origin_name TEXT)",
            "CREATE TABLE library_tags (book_url TEXT NOT NULL, tag TEXT NOT NULL, \
             PRIMARY KEY (book_url, tag))",
        ] {
            sqlx::query(sql).execute(&library).await.unwrap();
        }

        assert_eq!(index.adopt_orphans(&library).await.unwrap(), 1);
        let (name, tier, source_type, chapters): (String, String, i64, i64) = sqlx::query_as(
            "SELECT name, tier, book_source_type, total_chapter_num FROM library_books",
        )
        .fetch_one(&library)
        .await
        .unwrap();
        assert_eq!(name, "补登书");
        assert_eq!(tier, "collection");
        assert_eq!(source_type, 1);
        assert_eq!(chapters, 1);
        let tag: String = sqlx::query_scalar("SELECT tag FROM library_tags")
            .fetch_one(&library)
            .await
            .unwrap();
        assert_eq!(tag, "科幻");

        // 再跑一次不会重复插入。
        assert_eq!(index.adopt_orphans(&library).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn verify_reports_dirs_missing_from_library_db() {
        let root = temp_root("verify");
        let store = Store::at(root.clone());
        store.create_layout().unwrap();
        text_book(&store, "https://example.com/t/3", "有目录").await;

        let index = Index::open(&root).await.unwrap();
        index.rebuild(&store).await.unwrap();

        let library = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE library_books (book_url TEXT PRIMARY KEY, tier TEXT)")
            .execute(&library)
            .await
            .unwrap();

        let v = index.verify(&store, &library).await.unwrap();
        assert_eq!(v.dirs, 1);
        assert_eq!(v.dir_without_db, vec!["https://example.com/t/3"]);
        assert!(v.has_problems());

        sqlx::query("INSERT INTO library_books (book_url, tier) VALUES (?1, 'collection')")
            .bind("https://example.com/t/3")
            .execute(&library)
            .await
            .unwrap();
        let v = index.verify(&store, &library).await.unwrap();
        assert!(v.dir_without_db.is_empty());
        assert!(!v.has_problems());

        sqlx::query("INSERT INTO library_books (book_url, tier) VALUES (?1, 'collection')")
            .bind("https://example.com/only-db")
            .execute(&library)
            .await
            .unwrap();
        let v = index.verify(&store, &library).await.unwrap();
        assert_eq!(v.db_without_dir, vec!["https://example.com/only-db"]);
    }
}
