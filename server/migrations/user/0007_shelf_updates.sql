-- 书架更新开关：每本书是否参与自动/批量更新（默认开启）
ALTER TABLE bookshelf ADD COLUMN enable_update INTEGER NOT NULL DEFAULT 1;

-- 新章节角标：记录目录总数与用户已看到的总数，角标 = MAX(0, total - seen_total)。
-- 只在目录刷新/打开章节时写一行，避免每次全量比对目录。
CREATE TABLE IF NOT EXISTS book_updates (
    book_id     TEXT PRIMARY KEY,
    total       INTEGER NOT NULL DEFAULT 0,
    seen_total  INTEGER NOT NULL DEFAULT 0,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
