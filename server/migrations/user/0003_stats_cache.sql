-- 0003：阅读统计 + 缓存时间戳/淘汰字段 + 替换规则超时字段

-- 书架：显式分组（与 book_meta."group" 同步）与置顶排序
ALTER TABLE bookshelf ADD COLUMN "group" TEXT;

-- 书架分组：补齐 Legado BookGroup 的排序/展示字段
ALTER TABLE book_groups ADD COLUMN cover TEXT;
ALTER TABLE book_groups ADD COLUMN "order" INTEGER NOT NULL DEFAULT 0;
ALTER TABLE book_groups ADD COLUMN enable_refresh INTEGER NOT NULL DEFAULT 1;
ALTER TABLE book_groups ADD COLUMN show INTEGER NOT NULL DEFAULT 1;
ALTER TABLE book_groups ADD COLUMN book_sort INTEGER NOT NULL DEFAULT 0;

-- 替换规则：Legado 的正则超时（毫秒）。0 表示使用服务端默认预算。
ALTER TABLE replace_rules ADD COLUMN timeout_millisecond INTEGER NOT NULL DEFAULT 0;

-- 目录缓存：记录刷新时间，供 TTL 判定
ALTER TABLE book_chapters ADD COLUMN updated_at TEXT;
UPDATE book_chapters SET updated_at = datetime('now') WHERE updated_at IS NULL;

-- 正文缓存：刷新时间 + 最近访问时间 + 字节数，供 TTL 与 LRU 淘汰统计
ALTER TABLE book_content ADD COLUMN updated_at TEXT;
ALTER TABLE book_content ADD COLUMN accessed_at TEXT;
ALTER TABLE book_content ADD COLUMN size_bytes INTEGER NOT NULL DEFAULT 0;
UPDATE book_content SET updated_at = datetime('now') WHERE updated_at IS NULL;
UPDATE book_content SET accessed_at = updated_at WHERE accessed_at IS NULL;
UPDATE book_content SET size_bytes = length(content) WHERE size_bytes = 0;

CREATE INDEX IF NOT EXISTS idx_book_content_book ON book_content (book_url);
CREATE INDEX IF NOT EXISTS idx_book_content_accessed ON book_content (accessed_at);

-- 阅读统计：按天聚合的阅读时长/字数
CREATE TABLE IF NOT EXISTS reading_stats (
    day         TEXT PRIMARY KEY,              -- YYYY-MM-DD（本地时区）
    seconds     INTEGER NOT NULL DEFAULT 0,    -- 阅读时长
    chars       INTEGER NOT NULL DEFAULT 0,    -- 阅读字数
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 单本完成度：记录已读章节与总章节
CREATE TABLE IF NOT EXISTS book_stats (
    book_id         TEXT PRIMARY KEY,
    name            TEXT,
    total_chapters  INTEGER NOT NULL DEFAULT 0,
    read_chapters   INTEGER NOT NULL DEFAULT 0,
    last_read_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 连续阅读天数（由 reading_stats 推导；单独表存跨天游标，避免每次全表扫描）
CREATE TABLE IF NOT EXISTS reading_streak (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    current_days    INTEGER NOT NULL DEFAULT 0,
    last_day        TEXT,
    longest_days    INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO reading_streak (id, current_days, longest_days) VALUES (1, 0, 0);
