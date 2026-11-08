-- 一次性修复历史误判：全局搜索预算耗尽曾被记为站点失败（STATUS_FAIL），
-- 连续 3 次后把健康源打成 quarantined 并进入冷却。修复后的代码不再为
-- truncated/skipped 写健康，因此带有该错误前缀的隔离记录必然来自旧版本。
-- 只把“明确由预算耗尽导致”的隔离源降回 observing，保留真实站点失败的隔离。
UPDATE source_health
SET tier = 'observing',
    consecutive_failures = 0,
    fail_site_count = 0,
    cooldown_until = NULL,
    last_error = NULL,
    demote_streak = 0
WHERE tier = 'quarantined'
  AND last_error LIKE '全局搜索预算耗尽%';
