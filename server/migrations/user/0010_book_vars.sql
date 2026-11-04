-- 跨请求的 book 层状态（对齐 Legado AnalyzeRule.book.putVariable / CacheManager）。
-- 放在用户桶而非共享桶：变量/缓存都按用户隔离，不能跨账号串。
--
-- book_key = sha256(规范化 source_url + '\n' + book_url) 的十六进制，
-- 不把原始长 URL 当主键，避免索引膨胀与超长键。
CREATE TABLE IF NOT EXISTS book_vars (
    book_key   TEXT PRIMARY KEY,
    vars_json  TEXT NOT NULL DEFAULT '{}',
    updated_at INTEGER NOT NULL DEFAULT 0
);

-- 每本书的 cache（键 = book_key + cache_key，值带过期时间，默认 10 分钟）。
CREATE TABLE IF NOT EXISTS book_cache (
    book_key   TEXT NOT NULL,
    cache_key  TEXT NOT NULL,
    value      TEXT NOT NULL,
    expires_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (book_key, cache_key)
);

CREATE INDEX IF NOT EXISTS idx_book_cache_expires ON book_cache (expires_at);
