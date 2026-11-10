//! 按 (源, 查询类) 记录搜索产出证据，带时间衰减。
//!
//! 与 `source_health` 的分工：
//! - `source_health` 是**站点级**慢变量（存活、延迟、全局命中率）；
//! - 本表是**源 × 查询类**快变量（这类查询能不能出结果）。
//!
//! 计数用指数衰减而不是永久累加：书源会改版、站点会换书库，三个月前的证据
//! 不应该和昨天的一样重。衰减在写入时完成（先按 `updated_at` 打折，再累加），
//! 读路径不需要再算时间差。

use std::collections::HashMap;

use sqlx::{FromRow, SqlitePool};

use crate::error::AppResult;
use crate::query_class::QueryClass;

/// 证据半衰期：14 天。高频源会自然保留近期状态，低频源的旧证据慢慢退场。
pub const HALF_LIFE_MS: f64 = 14.0 * 24.0 * 3600.0 * 1000.0;

#[derive(Debug, Clone, FromRow, Default)]
pub struct QueryStatsRow {
    pub source_url: String,
    pub query_class: String,
    pub search_count: f64,
    pub yield_ok_count: f64,
    pub kept_hit_count: f64,
    pub updated_at: i64,
}

/// 距离上次观测 `dt` 毫秒后的保留比例（半衰期 [`HALF_LIFE_MS`]）。
pub fn decay_factor(prev_ms: i64, now_ms: i64) -> f64 {
    let dt = (now_ms.saturating_sub(prev_ms)).max(0) as f64;
    0.5f64.powf(dt / HALF_LIFE_MS)
}

/// 载入某一查询类的全部源证据（源数最多几百，一次全取比逐源查更省）。
pub async fn load_class(
    pool: &SqlitePool,
    class: QueryClass,
) -> AppResult<HashMap<String, QueryStatsRow>> {
    let rows: Vec<QueryStatsRow> = sqlx::query_as(
        "SELECT source_url, query_class, search_count, yield_ok_count, kept_hit_count, updated_at \
         FROM source_query_stats WHERE query_class = ?",
    )
    .bind(class.as_str())
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.source_url.clone(), r))
        .collect())
}

/// 在健康写回事务里追加一条查询类证据（衰减后累加）。
///
/// 只由真实搜索（`usage=true`）调用：体检/巡检的语料命中属于「站点覆盖」，
/// 不能当作「这类查询有产出」，否则热门语料会把所有源的产出率先验抬高。
pub async fn record_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    source_url: &str,
    class: QueryClass,
    kept_hits: i64,
    now_ms: i64,
) -> Result<(), sqlx::Error> {
    let prev: Option<(f64, f64, f64, i64)> = sqlx::query_as(
        "SELECT search_count, yield_ok_count, kept_hit_count, updated_at \
         FROM source_query_stats WHERE source_url = ?1 AND query_class = ?2",
    )
    .bind(source_url)
    .bind(class.as_str())
    .fetch_optional(&mut **tx)
    .await?;

    let (search, yield_ok, kept, prev_at) = prev.unwrap_or((0.0, 0.0, 0.0, now_ms));
    let f = decay_factor(prev_at, now_ms);
    let search = search * f + 1.0;
    let yield_ok = yield_ok * f + if kept_hits > 0 { 1.0 } else { 0.0 };
    let kept = kept * f + kept_hits.max(0) as f64;

    sqlx::query(
        "INSERT INTO source_query_stats \
         (source_url, query_class, search_count, yield_ok_count, kept_hit_count, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(source_url, query_class) DO UPDATE SET \
          search_count=excluded.search_count, \
          yield_ok_count=excluded.yield_ok_count, \
          kept_hit_count=excluded.kept_hit_count, \
          updated_at=excluded.updated_at",
    )
    .bind(source_url)
    .bind(class.as_str())
    .bind(search)
    .bind(yield_ok)
    .bind(kept)
    .bind(now_ms)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        let p = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE source_query_stats (\
             source_url TEXT NOT NULL, query_class TEXT NOT NULL, \
             search_count REAL NOT NULL DEFAULT 0, yield_ok_count REAL NOT NULL DEFAULT 0, \
             kept_hit_count REAL NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL DEFAULT 0, \
             PRIMARY KEY (source_url, query_class))",
        )
        .execute(&p)
        .await
        .unwrap();
        p
    }

    #[test]
    fn decay_halves_after_one_half_life() {
        let now = 10 * HALF_LIFE_MS as i64;
        assert!((decay_factor(now, now) - 1.0).abs() < 1e-9);
        assert!((decay_factor(now - HALF_LIFE_MS as i64, now) - 0.5).abs() < 1e-9);
        // 时钟回拨不应放大证据
        assert!((decay_factor(now + 1000, now) - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn records_and_decays_per_class() {
        let p = pool().await;
        let mut tx = p.begin().await.unwrap();
        record_in_tx(&mut tx, "https://a", QueryClass::Hot, 3, 1_000)
            .await
            .unwrap();
        record_in_tx(&mut tx, "https://a", QueryClass::Hot, 0, 2_000)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let hot = load_class(&p, QueryClass::Hot).await.unwrap();
        let row = hot.get("https://a").unwrap();
        // 两次写入间隔 1s，衰减因子≈1（但不是精确 1），用近似比较。
        assert!((row.search_count - 2.0).abs() < 1e-4);
        assert!((row.yield_ok_count - 1.0).abs() < 1e-4);
        assert!((row.kept_hit_count - 3.0).abs() < 1e-4);

        // 不同查询类互不影响
        assert!(load_class(&p, QueryClass::Obscure)
            .await
            .unwrap()
            .is_empty());

        // 隔一个半衰期再写：旧计数减半后累加
        let mut tx = p.begin().await.unwrap();
        let later = 2_000 + HALF_LIFE_MS as i64;
        record_in_tx(&mut tx, "https://a", QueryClass::Hot, 1, later)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let hot = load_class(&p, QueryClass::Hot).await.unwrap();
        let row = hot.get("https://a").unwrap();
        assert!((row.search_count - 2.0).abs() < 1e-6, "2*0.5+1");
        assert!((row.kept_hit_count - 2.5).abs() < 1e-6, "3*0.5+1");
    }
}
