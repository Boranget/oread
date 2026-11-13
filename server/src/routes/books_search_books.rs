//! 搜索结果行存储：边搜边落库，分页直接读库。
//!
//! 三条不变量（翻页不重不漏的前提）：
//! - `seq` 首次入库时分配，单调递增，之后永不改变；
//! - `score` 首次入库时按相关性 + 源权重算一次，之后永不改变；
//! - 同书（`book_key` 相同）只追加来源，不重算分数、不改序号。
//!
//! 这样「结果持续增长」只是往表里加新行，老行的排序键是常量，
//! 已翻过的页不会变——除非有人主动推进 `frozen_seq`。

use super::*;

/// 一个源批次写入后的增量统计。
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct BatchWrite {
    /// 通过相关性门槛的候选条数（书名过滤前）
    pub(super) candidates: usize,
    /// 新增去重行数
    pub(super) added: usize,
    /// 结果体积增量（字节）
    pub(super) bytes_added: i64,
}

/// 写入一个源的结果批次：相关性门槛 → 书名过滤 → 去重 → 分配稳定序号。
///
/// 每源一个事务：批次要么整体可见，要么整体不可见，崩溃不会留下半批数据。
pub(super) async fn insert_batch(
    pool: &SqlitePool,
    job_id: &str,
    key: &str,
    source_weight: i64,
    books: Vec<SearchBook>,
) -> sqlx::Result<BatchWrite> {
    let q = crate::search_rank::normalize_title(key);
    if q.is_empty() {
        return Ok(BatchWrite::default());
    }

    // 批内先完成门槛、书名过滤与去重，避免把注定不展示的条目写进库。
    let mut batch: Vec<(String, u8, SearchBook)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut candidates = 0usize;
    for mut b in books {
        let rel = crate::search_rank::search_relevance(
            &q,
            b.name.as_deref().unwrap_or(""),
            b.author.as_deref().unwrap_or(""),
        );
        if rel == 0 {
            continue;
        }
        candidates += 1;
        if !crate::title_filter::title_contains(key, b.name.as_deref().unwrap_or("")) {
            continue;
        }
        b.relevance = Some(crate::search_rank::relevance_class(rel).to_string());
        // 旧字段不再写入：新任务统一用 (rank_score, rank_seq) 做游标。
        b.rank_bucket = None;
        let k = crate::web_book::dedupe_key(&b);
        match index.get(&k) {
            Some(&i) => merge_origin(&mut batch[i].2, &b),
            None => {
                index.insert(k.clone(), batch.len());
                normalize_origins(&mut b);
                batch.push((k, rel, b));
            }
        }
    }
    if batch.is_empty() {
        return Ok(BatchWrite {
            candidates,
            ..Default::default()
        });
    }

    let mut tx = pool.begin().await?;
    let mut next_seq: i64 = sqlx::query_scalar("SELECT next_seq FROM search_jobs WHERE id = ?1")
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(0);

    // 一次查出本批次可能命中的已有行，避免逐条 SELECT。
    let mut existing: HashMap<String, (String, i64)> = HashMap::new();
    {
        let placeholders: Vec<String> = (2..batch.len() + 2).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT book_key, book_json, bytes FROM search_job_books \
             WHERE job_id = ?1 AND book_key IN ({})",
            placeholders.join(",")
        );
        let mut query = sqlx::query_as::<_, (String, String, i64)>(&sql).bind(job_id);
        for (k, _, _) in &batch {
            query = query.bind(k);
        }
        for (k, json, bytes) in query.fetch_all(&mut *tx).await? {
            existing.insert(k, (json, bytes));
        }
    }

    let mut added = 0usize;
    let mut bytes_added = 0i64;
    for (k, rel, mut book) in batch {
        if let Some((old_json, old_bytes)) = existing.get(&k) {
            // 同书多源：只把新来源补进 origins，分数与序号保持首次入库时的值。
            let mut row: SearchBook = match serde_json::from_str(old_json) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let before = row.origins.len();
            merge_origin(&mut row, &book);
            if row.origins.len() == before {
                continue;
            }
            let json = serde_json::to_string(&row).unwrap_or_else(|_| old_json.clone());
            let bytes = json.len() as i64;
            sqlx::query(
                "UPDATE search_job_books SET book_json = ?3, bytes = ?4 \
                 WHERE job_id = ?1 AND book_key = ?2",
            )
            .bind(job_id)
            .bind(&k)
            .bind(&json)
            .bind(bytes)
            .execute(&mut *tx)
            .await?;
            bytes_added += bytes - old_bytes;
            continue;
        }

        let score = crate::search_rank::combined_score(rel, source_weight, 1);
        book.rank_score = Some(score);
        book.rank_seq = Some(next_seq as u64);
        let json = serde_json::to_string(&book).unwrap_or_default();
        if json.is_empty() {
            continue;
        }
        let bytes = json.len() as i64;
        sqlx::query(
            "INSERT INTO search_job_books (job_id, seq, score, book_key, book_json, bytes) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(job_id, book_key) DO NOTHING",
        )
        .bind(job_id)
        .bind(next_seq)
        .bind(score as i64)
        .bind(&k)
        .bind(&json)
        .bind(bytes)
        .execute(&mut *tx)
        .await?;
        next_seq += 1;
        added += 1;
        bytes_added += bytes;
    }

    sqlx::query(
        "UPDATE search_jobs SET next_seq = ?2, raw_count = raw_count + ?3, \
         result_count = result_count + ?4, result_bytes = result_bytes + ?5 WHERE id = ?1",
    )
    .bind(job_id)
    .bind(next_seq)
    .bind(candidates as i64)
    .bind(added as i64)
    .bind(bytes_added)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(BatchWrite {
        candidates,
        added,
        bytes_added,
    })
}

/// `origins` 只保留展示名：空值丢弃、去重、保持首次出现顺序。
fn normalize_origins(b: &mut SearchBook) {
    b.origins.retain(|n| !n.trim().is_empty());
    let mut seen = std::collections::HashSet::new();
    b.origins.retain(|n| seen.insert(n.clone()));
    if b.origins.is_empty() {
        if let Some(name) = b.origin_name.as_deref().filter(|n| !n.is_empty()) {
            b.origins.push(name.to_string());
        }
    }
}

fn merge_origin(into: &mut SearchBook, from: &SearchBook) {
    let name = from
        .origin_name
        .as_deref()
        .filter(|n| !n.is_empty())
        .map(str::to_string);
    let Some(name) = name else { return };
    if !into.origins.iter().any(|n| n == &name) {
        into.origins.push(name);
    }
}

/// 游标翻页：`score DESC, seq ASC`，取 `limit + 1` 判断是否还有下一页。
///
/// `frozen` 是首屏边界（`seq < frozen`，即「边界前的已入库条数」）：
/// 运行中第一次翻页后冻结，后台新结果不会挤动已经翻过的页。
pub(super) async fn load_page(
    pool: &SqlitePool,
    job_id: &str,
    frozen: Option<i64>,
    after: Option<(u8, u64)>,
    limit: usize,
) -> sqlx::Result<(Vec<SearchBook>, bool)> {
    let limit = limit.max(1);
    let mut sql = String::from("SELECT book_json FROM search_job_books WHERE job_id = ?1");
    let mut n = 2;
    if frozen.is_some() {
        sql.push_str(&format!(" AND seq < ?{n}"));
        n += 1;
    }
    if after.is_some() {
        sql.push_str(&format!(
            " AND (score < ?{n} OR (score = ?{n} AND seq > ?{}))",
            n + 1
        ));
        n += 2;
    }
    sql.push_str(&format!(" ORDER BY score DESC, seq ASC LIMIT ?{n}"));

    let mut query = sqlx::query_scalar::<_, String>(&sql).bind(job_id);
    if let Some(f) = frozen {
        query = query.bind(f);
    }
    if let Some((score, seq)) = after {
        query = query.bind(score as i64).bind(seq as i64);
    }
    let rows = query.bind((limit + 1) as i64).fetch_all(pool).await?;
    let has_more = rows.len() > limit;
    Ok((
        rows.into_iter()
            .take(limit)
            .filter_map(parse_book)
            .collect(),
        has_more,
    ))
}

/// 页码翻页（`/k` 用）：冻结集合内用 OFFSET 是安全的，因为集合不再增长。
pub(crate) async fn load_page_at(
    pool: &SqlitePool,
    job_id: &str,
    frozen: Option<i64>,
    offset: usize,
    limit: usize,
) -> sqlx::Result<Vec<SearchBook>> {
    let limit = limit.max(1);
    let sql = match frozen {
        Some(_) => {
            "SELECT book_json FROM search_job_books WHERE job_id = ?1 AND seq < ?2 \
                    ORDER BY score DESC, seq ASC LIMIT ?3 OFFSET ?4"
        }
        None => {
            "SELECT book_json FROM search_job_books WHERE job_id = ?1 \
                 ORDER BY score DESC, seq ASC LIMIT ?2 OFFSET ?3"
        }
    };
    let mut query = sqlx::query_scalar::<_, String>(sql).bind(job_id);
    if let Some(f) = frozen {
        query = query.bind(f);
    }
    let rows = query
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().filter_map(parse_book).collect())
}

fn parse_book(json: String) -> Option<SearchBook> {
    serde_json::from_str(&json).ok()
}

/// 冻结集合内的结果条数；`frozen` 为 `None` 时统计全部。
pub(crate) async fn count(
    pool: &SqlitePool,
    job_id: &str,
    frozen: Option<i64>,
) -> sqlx::Result<i64> {
    match frozen {
        Some(f) => {
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM search_job_books WHERE job_id = ?1 AND seq < ?2",
            )
            .bind(job_id)
            .bind(f)
            .fetch_one(pool)
            .await
        }
        None => {
            sqlx::query_scalar("SELECT COUNT(*) FROM search_job_books WHERE job_id = ?1")
                .bind(job_id)
                .fetch_one(pool)
                .await
        }
    }
}

/// 首次翻页时冻结首屏边界；已冻结则原样返回。
pub(crate) async fn freeze(pool: &SqlitePool, job_id: &str) -> sqlx::Result<i64> {
    sqlx::query(
        "UPDATE search_jobs SET frozen_seq = next_seq WHERE id = ?1 AND frozen_seq IS NULL",
    )
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(
        sqlx::query_scalar("SELECT COALESCE(frozen_seq, next_seq) FROM search_jobs WHERE id = ?1")
            .bind(job_id)
            .fetch_optional(pool)
            .await?
            .unwrap_or(0),
    )
}

/// 主动推进首屏边界（`/k` 的「显示更多结果」）：把后台已入库的结果纳入当前集合。
/// 返回推进后的边界。
pub(crate) async fn advance_freeze(pool: &SqlitePool, job_id: &str) -> sqlx::Result<i64> {
    sqlx::query("UPDATE search_jobs SET frozen_seq = next_seq WHERE id = ?1")
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(
        sqlx::query_scalar("SELECT COALESCE(frozen_seq, next_seq) FROM search_jobs WHERE id = ?1")
            .bind(job_id)
            .fetch_optional(pool)
            .await?
            .unwrap_or(0),
    )
}

/// 任务的结果行：删除任务、历史裁剪时级联清理。
pub(super) async fn delete_books(pool: &SqlitePool, job_id: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM search_job_books WHERE job_id = ?1")
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 清理已经没有对应任务的行（历史任务被裁剪后留下的孤儿）。
pub(super) async fn delete_orphans(pool: &SqlitePool) -> sqlx::Result<u64> {
    let r = sqlx::query(
        "DELETE FROM search_job_books WHERE job_id NOT IN (SELECT id FROM search_jobs)",
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}
