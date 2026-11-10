-- 按 (源, 查询类) 记录搜索产出证据，带时间衰减。
--
-- 与 source_health 的分工：
--   source_health        站点级慢变量（存活 / 延迟 / 全局命中率）
--   source_query_stats   源 × 查询类快变量（这类查询能不能出结果）
--
-- 计数用 REAL 存衰减后的值：写入时先按 updated_at 打折（半衰期 14 天）再累加，
-- 读路径不再算时间差。只由真实搜索（usage=true）写入；体检/巡检不写，
-- 避免热门语料把冷门查询的产出率先验抬高。
CREATE TABLE IF NOT EXISTS source_query_stats (
    source_url     TEXT NOT NULL,
    query_class    TEXT NOT NULL, -- hot | normal | obscure | author | serial
    search_count   REAL NOT NULL DEFAULT 0,
    yield_ok_count REAL NOT NULL DEFAULT 0,
    kept_hit_count REAL NOT NULL DEFAULT 0,
    updated_at     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (source_url, query_class)
);

CREATE INDEX IF NOT EXISTS idx_source_query_stats_class
    ON source_query_stats(query_class, updated_at);
