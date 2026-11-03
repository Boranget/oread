-- 0004：书签 + 阅读页宽/字体族默认值

CREATE TABLE IF NOT EXISTS bookmarks (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    book_id       TEXT NOT NULL,
    chapter_index INTEGER NOT NULL DEFAULT 0,
    position      REAL NOT NULL DEFAULT 0,   -- 章内比例 0..1
    chapter_title TEXT,
    note          TEXT,
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_bookmarks_book_time
    ON bookmarks (book_id, created_at DESC, id DESC);

-- 旧客户端不传这两个字段时，GET /api/settings 仍需返回可用默认值。
INSERT OR IGNORE INTO user_settings (key, value) VALUES ('page_width', '800');
INSERT OR IGNORE INTO user_settings (key, value) VALUES ('font_family', 'serif');
