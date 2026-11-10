-- 搜索产出证据：把「返回了可解析结果」与「书名真的包含关键词」分开记。
--
-- 背景：`hit_count` 统计的是书源解析出的条目数，发生在书名过滤之前，
-- 它只能证明「站点活着、模板没坏」，不能证明「搜到了用户要的书」。
-- 搜索入队需要的是「这个源每秒能产出多少条用户可见结果」，因此新增：
--   - kept_hit_count：书名匹配命中条数之和（只统计真实用户搜索）
--   - yield_ok_count：本次搜索 kept_hits > 0 的次数
--
-- 两者都只在 usage=true（真实搜索）时累计；体检/巡检不写，避免热门语料
-- 把冷门查询的产出率抬高。历史数据从 0 开始，由后续搜索自然填充。
ALTER TABLE source_health ADD COLUMN kept_hit_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_health ADD COLUMN yield_ok_count INTEGER NOT NULL DEFAULT 0;
