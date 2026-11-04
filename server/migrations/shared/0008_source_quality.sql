-- 源质量分层：在既有 source_health 上扩列，不新建重复的冷却/健康表。
-- tier: preferred | observing | quarantined；缺失行按 observing 兼容历史数据。
ALTER TABLE source_health ADD COLUMN tier TEXT NOT NULL DEFAULT 'observing';
ALTER TABLE source_health ADD COLUMN score INTEGER NOT NULL DEFAULT 50;
ALTER TABLE source_health ADD COLUMN evidence_json TEXT;
ALTER TABLE source_health ADD COLUMN last_eval_at INTEGER;

-- 成功侧统计：导入体检只是入门考，真实搜索使用才是期末考。
ALTER TABLE source_health ADD COLUMN search_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_health ADD COLUMN hit_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_health ADD COLUMN parse_ok_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_health ADD COLUMN fail_site_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_health ADD COLUMN fail_engine_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_health ADD COLUMN ttfb_ms_p90 INTEGER;
ALTER TABLE source_health ADD COLUMN ttfb_samples_json TEXT;

-- 升降级滞回：连续 N 次达标才升级，连续 N 次不达标才降级，避免评分抖动。
ALTER TABLE source_health ADD COLUMN promote_streak INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_health ADD COLUMN demote_streak INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_source_health_tier_score
    ON source_health(tier, score);
