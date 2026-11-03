-- 修复历史数据：早期 upsert 只写 rules，而详情接口读 content，导致详情永远为空。
-- 双向回填，保证任一列缺失时都有可用 JSON；新写入会同时维护两列。
UPDATE book_sources SET content = rules WHERE (content IS NULL OR content = '') AND rules IS NOT NULL;
UPDATE book_sources SET rules = content WHERE (rules IS NULL OR rules = '') AND content IS NOT NULL;
