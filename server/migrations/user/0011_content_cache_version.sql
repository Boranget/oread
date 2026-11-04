-- 正文缓存解析管线指纹：旧记录没有该字段，读取时按未命中处理并在首次抓取后按当前指纹回写。
-- 仅用于判定「这份正文是否由当前解析管线产出」，不做时间失效（TTL 仍由 updated_at 负责）。
ALTER TABLE book_content ADD COLUMN cache_version TEXT;
