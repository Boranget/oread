-- 用户独立桶：书架/分组/阅读进度/搜索历史/替换规则/个人设置（隐私数据，按用户隔离）

CREATE TABLE IF NOT EXISTS bookshelf (
    book_id  TEXT PRIMARY KEY,
    title    TEXT NOT NULL,
    author   TEXT,
    cover    TEXT,
    source_id TEXT,
    added_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS book_groups (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS reading_progress (
    book_id      TEXT PRIMARY KEY,
    chapter_index INTEGER NOT NULL DEFAULT 0,
    position     REAL NOT NULL DEFAULT 0,
    updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS search_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    query       TEXT NOT NULL,
    searched_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS replace_rules (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    pattern     TEXT NOT NULL,
    replacement TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS user_settings (
    key   TEXT PRIMARY KEY,
    value TEXT
);
