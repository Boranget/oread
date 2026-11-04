-- 精选自动重算时间戳：轻量重算/整批重算都会更新
ALTER TABLE source_curated ADD COLUMN last_run_at INTEGER;
