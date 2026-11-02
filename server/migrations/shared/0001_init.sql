-- 共享桶：账户 + 书源 + RSS 源 + 公共配置（全站一份，仅管理员可写）

CREATE TABLE IF NOT EXISTS users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT UNIQUE NOT NULL,
    password_hash TEXT NOT NULL,
    role          TEXT NOT NULL DEFAULT 'user',   -- 'user' | 'admin'
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS book_sources (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    url        TEXT,
    rules      TEXT NOT NULL,                      -- 书源规则(JSON)，沿用 Legado analyzeRule DSL
    enabled    INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by TEXT
);

CREATE TABLE IF NOT EXISTS rss_sources (
    id      TEXT PRIMARY KEY,
    name    TEXT NOT NULL,
    url     TEXT,
    rules   TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS public_settings (
    key   TEXT PRIMARY KEY,
    value TEXT
);
