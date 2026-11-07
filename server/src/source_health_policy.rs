//! 书源健康的搜索准入与状态查询（move-only 从 source_health.rs 拆出）。

use super::*;

/// 搜索准入：只允许 `preferred` / `observing`；缺失健康行按 observing 兼容历史数据。
/// 用白名单而不是“非 quarantined”，避免未知/历史脏值（如空串）混进聚合。
pub fn visible_in_search(row: Option<&HealthRow>) -> bool {
    row.map(|h| {
        matches!(
            h.tier.as_str(),
            source_quality::TIER_PREFERRED | source_quality::TIER_OBSERVING
        )
    })
    .unwrap_or(true)
}

/// 探测失败是否属于“明确站点级拒绝”（HTTP 4xx/5xx）。
/// 导入期只有这类失败参与阻断性判定；超时/连接失败属于“未知”，不能据此把源判死。
pub fn is_explicit_site_rejection(error: Option<&str>) -> bool {
    error
        .map(|e| {
            e.trim_start()
                .to_ascii_lowercase()
                .starts_with("http status ")
        })
        .unwrap_or(false)
}

/// 导入期探测的“未知失败”修正：保留 `record_outcomes` 已写入的健康事件与失败计数，
/// 但在未达到既有硬阈值（连续/站点失败 3 次）前把 tier 拉回 observing，
/// 让新导入的源保持可搜索，下一次真实搜索/体检再按既有评分、冷却、隔离规则判定。
pub async fn keep_observing_after_transient_probe(
    pool: &SqlitePool,
    source_urls: &[String],
) -> AppResult<()> {
    if source_urls.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for url in source_urls {
        sqlx::query(
            "UPDATE source_health SET tier = ?1, demote_streak = 0 \
             WHERE source_url = ?2 \
               AND consecutive_failures < ?3 AND fail_site_count < ?3",
        )
        .bind(source_quality::TIER_OBSERVING)
        .bind(url)
        .bind(FAIL_THRESHOLD)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 该源当前是否 preferred（用于两批调度的第一批）。
pub fn is_preferred(row: Option<&HealthRow>) -> bool {
    row.map(|h| h.tier.as_str() == source_quality::TIER_PREFERRED)
        .unwrap_or(false)
}

/// 清空某源或全部健康记录，返回清理行数。
pub async fn reset(pool: &SqlitePool, source_ids: &[String]) -> AppResult<u64> {
    if source_ids.is_empty() {
        let r = sqlx::query("DELETE FROM source_health")
            .execute(pool)
            .await?;
        return Ok(r.rows_affected());
    }
    let mut cleared = 0u64;
    for id in source_ids {
        let r = sqlx::query("DELETE FROM source_health WHERE source_url = ?")
            .bind(id)
            .execute(pool)
            .await?;
        cleared += r.rows_affected();
    }
    Ok(cleared)
}
