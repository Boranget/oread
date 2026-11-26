-- 个人阅读状态：想读 / 在读 / 已读。
--
-- 与书架（bookshelf）解耦：状态是「我对这本书的进度语义」，收藏是「我要把它放在手边」。
-- 一本书可以不在书架里但有阅读状态（例如从馆藏直接标记想读）。
-- 记录阅读进度时会把 want 自动推进到 reading，不会把 finished 回退。
CREATE TABLE IF NOT EXISTS reading_status (
    book_id    TEXT PRIMARY KEY,
    status     TEXT NOT NULL,          -- want | reading | finished
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_reading_status_status
    ON reading_status(status, updated_at DESC);
