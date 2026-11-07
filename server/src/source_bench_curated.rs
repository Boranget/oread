//! source_bench 的精选重算与查询（move-only 从 source_bench.rs 拆出）。

use super::*;

pub(super) struct StoredScoreRow {
    pub(super) source_url: String,
    pub(super) name: String,
    pub(super) source_type: i64,
    pub(super) bench_score: i32,
    pub(super) samples: i64,
    pub(super) hit_rate: f64,
    pub(super) fail_streak: i64,
    pub(super) tier: Option<String>,
}

/// 从库内状态重算精选并回写 curated 标记。
pub(super) async fn recompute_curated(
    pool: &SqlitePool,
    cfg: &Config,
) -> AppResult<(Vec<CuratedItem>, Vec<CuratedItem>)> {
    #[allow(clippy::type_complexity)]
    // 行类型与 SELECT 列一一对应；抽成别名反而在列变更时不易察觉。
    let rows: Vec<(
        String,
        i64,
        i64,
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT c.source_url, c.source_type, c.bench_score, c.samples, c.fail_streak, h.tier, \
                (SELECT b.name FROM book_sources b WHERE b.id = c.source_url), c.meta_json \
         FROM source_curated c LEFT JOIN source_health h ON h.source_url = c.source_url",
    )
    .fetch_all(pool)
    .await?;
    let stored: Vec<StoredScoreRow> = rows
        .into_iter()
        .map(
            |(
                source_url,
                source_type,
                bench_score,
                samples,
                fail_streak,
                tier,
                name,
                meta_json,
            )| StoredScoreRow {
                name: name.unwrap_or_else(|| source_url.clone()),
                source_url,
                source_type,
                bench_score: bench_score as i32,
                samples,
                hit_rate: meta_hit_rate(meta_json.as_deref()),
                fail_streak,
                tier,
            },
        )
        .collect();

    let mut novel_items: Vec<CuratedItem> = Vec::new();
    let mut comic_items: Vec<CuratedItem> = Vec::new();
    // 先全部清 0，再只把本次入选的源置 1：这样非 preferred、样本不足、连续失败、
    // 掉出断层的源都会自动退出精选（不依赖“它是否在本次体检名单里”）。
    let recomputed_at = now_ms();
    sqlx::query("UPDATE source_curated SET curated = 0, last_run_at = ?")
        .bind(recomputed_at)
        .execute(pool)
        .await?;
    let mut selected: Vec<String> = Vec::new();
    for source_type in [TYPE_NOVEL, TYPE_COMIC] {
        let mut pool_rows: Vec<&StoredScoreRow> = stored
            .iter()
            .filter(|r| r.source_type == source_type)
            .filter(|r| r.samples >= MIN_CURATED_SAMPLES && r.fail_streak < 2)
            .filter(|r| curated_hit_ok(r.hit_rate))
            .filter(|r| r.tier.as_deref() == Some(crate::source_quality::TIER_PREFERRED))
            .collect();
        pool_rows.sort_by(|a, b| {
            b.bench_score
                .cmp(&a.bench_score)
                .then_with(|| a.source_url.cmp(&b.source_url))
        });
        let scores: Vec<i32> = pool_rows.iter().map(|r| r.bench_score).collect();
        let (min, max) = curated_bounds(cfg, source_type);
        let count = pick_curated_count(&scores, min, max, cfg.source_bench_gap);
        let cut_gap = if count > 0 && count < pool_rows.len() {
            Some(scores[count - 1] - scores[count])
        } else {
            None
        };
        let mut items: Vec<CuratedItem> = Vec::new();
        for (idx, r) in pool_rows.iter().take(count).enumerate() {
            selected.push(r.source_url.clone());
            items.push(CuratedItem {
                rank: idx + 1,
                source_url: r.source_url.clone(),
                name: r.name.clone(),
                source_type,
                bench_score: r.bench_score,
                samples: r.samples,
                cut_gap: if idx + 1 == count { cut_gap } else { None },
            });
        }
        if source_type == TYPE_NOVEL {
            novel_items = items;
        } else {
            comic_items = items;
        }
    }
    if !selected.is_empty() {
        let mut tx = pool.begin().await?;
        for url in selected {
            sqlx::query(
                "UPDATE source_curated SET curated = 1, last_run_at = ? WHERE source_url = ?",
            )
            .bind(recomputed_at)
            .bind(url)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }
    Ok((novel_items, comic_items))
}

/// 轻量重算：只针对一个源，把它是否仍该留在精选写回 `source_curated`。
/// 只负责“降级/样本不足/连续失败的源及时退出”，不会强行把源加进来（避免绕过断层与上限）。
pub async fn revalidate_curated_source(pool: &SqlitePool, source_url: &str) -> AppResult<()> {
    let row: Option<(i64, i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT c.samples, c.fail_streak, h.tier, c.meta_json \
         FROM source_curated c LEFT JOIN source_health h ON h.source_url = c.source_url \
         WHERE c.source_url = ?",
    )
    .bind(source_url)
    .fetch_optional(pool)
    .await?;
    let now = now_ms();
    if let Some((samples, fail_streak, tier, meta_json)) = row {
        let eligible = samples >= MIN_CURATED_SAMPLES
            && fail_streak < 2
            && curated_hit_ok(meta_hit_rate(meta_json.as_deref()))
            && tier.as_deref() == Some(crate::source_quality::TIER_PREFERRED);
        if eligible {
            sqlx::query("UPDATE source_curated SET last_run_at = ? WHERE source_url = ?")
                .bind(now)
                .bind(source_url)
                .execute(pool)
                .await?;
        } else {
            sqlx::query(
                "UPDATE source_curated SET curated = 0, last_run_at = ? WHERE source_url = ?",
            )
            .bind(now)
            .bind(source_url)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// 搜索前调用：最近一次体检超过 interval_hours 时，在后台跑一次整批体检。
/// 不阻塞当前搜索；同一时间只允许一个整批体检。
pub fn maybe_schedule_stale_bench(pool: SqlitePool, cfg: &Config) {
    if !try_begin_bench() {
        return;
    }
    let interval = cfg.source_bench_interval_hours;
    tokio::spawn(async move {
        let last: Option<i64> =
            sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(last_bench_at) FROM source_curated")
                .fetch_one(&pool)
                .await
                .ok()
                .flatten();
        if !bench_due(last, now_ms(), interval) {
            end_bench();
            return;
        }
        tracing::info!(target: "search_job", interval_hours = interval, "source bench stale; running in background");
        let result = run_bench(&pool, &Config::from_env(), None, None, None).await;
        end_bench();
        if let Err(e) = result {
            tracing::warn!(error = %e, "scheduled source bench failed");
        }
    });
}

/// 只读：当前 curated 集合（供搜索 scope=curated 使用）。
/// 读时兜底再校验 tier：即使降级后轻量重算还没跑完，也不会继续拿已降级的源去搜。
pub async fn curated_set(pool: &SqlitePool) -> AppResult<HashSet<String>> {
    let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT c.source_url, h.tier, c.meta_json FROM source_curated c \
         LEFT JOIN source_health h ON h.source_url = c.source_url \
         WHERE c.curated = 1",
    )
    .fetch_all(pool)
    .await?;
    let mut set = HashSet::new();
    let mut stale = Vec::new();
    for (url, tier, meta_json) in rows {
        if tier.as_deref() == Some(crate::source_quality::TIER_PREFERRED)
            && curated_hit_ok(meta_hit_rate(meta_json.as_deref()))
        {
            set.insert(url);
        } else {
            stale.push(url);
        }
    }
    if !stale.is_empty() {
        let pool = pool.clone();
        tokio::spawn(async move {
            for url in stale {
                if let Err(e) = revalidate_curated_source(&pool, &url).await {
                    tracing::warn!(error = %e, source_url = %url, "revalidate curated source failed");
                }
            }
        });
    }
    Ok(set)
}

/// 只读：精选清单（前端展示用）。
pub async fn curated_lists(pool: &SqlitePool) -> AppResult<(Vec<CuratedItem>, Vec<CuratedItem>)> {
    #[allow(clippy::type_complexity)]
    // 行类型与 SELECT 列一一对应；抽成别名反而在列变更时不易察觉。
    let rows: Vec<(String, i64, i64, i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT c.source_url, c.source_type, c.bench_score, c.samples, \
                (SELECT b.name FROM book_sources b WHERE b.id = c.source_url), c.meta_json \
         FROM source_curated c LEFT JOIN source_health h ON h.source_url = c.source_url \
         WHERE c.curated = 1 AND h.tier = 'preferred' \
         ORDER BY c.source_type, c.bench_score DESC, c.source_url",
    )
    .fetch_all(pool)
    .await?;
    let mut novel = Vec::new();
    let mut comic = Vec::new();
    for (source_url, source_type, bench_score, samples, name, meta_json) in rows {
        if !curated_hit_ok(meta_hit_rate(meta_json.as_deref())) {
            continue;
        }
        let item = CuratedItem {
            rank: 0,
            source_url: source_url.clone(),
            name: name.unwrap_or(source_url),
            source_type,
            bench_score: bench_score as i32,
            samples,
            cut_gap: None,
        };
        if source_type == TYPE_COMIC {
            comic.push(item);
        } else {
            novel.push(item);
        }
    }
    for (i, item) in novel.iter_mut().enumerate() {
        item.rank = i + 1;
    }
    for (i, item) in comic.iter_mut().enumerate() {
        item.rank = i + 1;
    }
    Ok((novel, comic))
}
