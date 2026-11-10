-- 一次性修复「搜索超时被当成站点故障」造成的误隔离。
--
-- 现象：`scope=all` 有上百个源、16 个并发槽、单源预算 8s。慢站把槽占满后大量请求超时，
-- 旧逻辑把超时记成 `fail`，连续 3 次就硬隔离（`tier='quarantined'`）；同时把未完成的
-- 请求也算成一次 0 命中，命中率/解析率被拉低，评分再触发一次质量隔离。结果是可搜索源
-- 从 100+ 掉到个位数，之后任何关键词都返回 0 条。
--
-- 超时是「未知」而不是「站点已死」：本迁移把这类误隔离的源放回 observing，并清掉被超时
-- 污染的命中/解析计数与延迟样本，给它们一个干净的重评起点。人工隔离（manual_quarantine）
-- 与清理隔离（pruned_site_fail）保持不动；真正连不上的源由体检/巡检重新判定。

-- 1) 账本：自动隔离的源恢复启用（人工/清理隔离不动）。
UPDATE book_sources
SET enabled = 1
WHERE enabled = 0
  AND id IN (
    SELECT l.source_url
    FROM source_library_state l
    JOIN source_health h ON h.source_url = l.source_url
    WHERE l.decision = 'quarantined'
      AND h.tier = 'quarantined'
      AND COALESCE(h.last_error, '') LIKE '%超时%'
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
    SELECT source_url
    FROM source_health
    WHERE tier = 'quarantined'
      AND COALESCE(last_error, '') LIKE '%超时%'
  );

-- 2) 健康档：误隔离源回到 observing，并清掉被超时污染的证据。
UPDATE source_health
SET tier = 'observing',
    consecutive_failures = 0,
    fail_site_count = 0,
    fail_engine_count = 0,
    promote_streak = 0,
    demote_streak = 0,
    cooldown_until = NULL,
    search_count = 0,
    hit_count = 0,
    parse_ok_count = 0,
    avg_latency_ms = NULL,
    ttfb_ms_p90 = NULL,
    ttfb_samples_json = NULL,
    last_error = NULL
WHERE tier = 'quarantined'
  AND COALESCE(last_error, '') LIKE '%超时%';
