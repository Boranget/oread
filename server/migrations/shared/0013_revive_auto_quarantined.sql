-- 一次性修复历史“自动隔离无法自愈”：
-- 1) 体检/巡检已经拿到有效响应（last_status='ok'）的源，健康档不应继续停在 quarantined；
-- 2) 自动隔离（站点不可达 / 质量 / 引擎限制 / 体检无响应 / 历史空原因）且最近一次响应为 ok 的源，
--    账本恢复为 active 并重新启用；人工隔离（manual_quarantine）与清理隔离（pruned_site_fail）保持不动。

UPDATE source_health
SET tier = 'observing',
    consecutive_failures = 0,
    fail_site_count = 0,
    demote_streak = 0,
    cooldown_until = NULL,
    last_error = NULL
WHERE tier = 'quarantined'
  AND last_status = 'ok';

UPDATE book_sources
SET enabled = 1
WHERE enabled = 0
  AND id IN (
    SELECT l.source_url
    FROM source_library_state l
    JOIN source_health h ON h.source_url = l.source_url
    WHERE l.decision = 'quarantined'
      AND h.last_status = 'ok'
      AND (
        l.reason IS NULL
        OR TRIM(l.reason) = ''
        OR l.reason LIKE 'site_unreachable%'
        OR l.reason LIKE 'quality_quarantine%'
        OR l.reason LIKE 'engine_limited%'
        OR l.reason LIKE 'bench:%'
        OR l.reason LIKE '全局搜索预算耗尽%'
      )
  );

UPDATE source_library_state
SET decision = 'active', reason = NULL
WHERE decision = 'quarantined'
  AND (
    reason IS NULL
    OR TRIM(reason) = ''
    OR reason LIKE 'site_unreachable%'
    OR reason LIKE 'quality_quarantine%'
    OR reason LIKE 'engine_limited%'
    OR reason LIKE 'bench:%'
    OR reason LIKE '全局搜索预算耗尽%'
  )
  AND source_url IN (
    SELECT source_url FROM source_health WHERE last_status = 'ok'
  );
