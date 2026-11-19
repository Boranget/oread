-- 馆藏库（全局一份）：书目 / 目录 / 正文，所有账号共享。
--
-- 与用户库的分工：
--   馆藏（本库）  = 内容层，谁抓到都一样，只存一份
--   借书证（users/<uid>.db）= 个人状态：书架、进度、书签、设置、替换规则、书源登录态
--
-- 分层（tier）：
--   collection = 馆藏，永久保存，只能由管理员移出/删除
--   temp       = 临时缓存，按 last_accessed_at 做 TTL 回收
--
-- 不设容量上限；占用只做监控（见 GET /api/library/stats）。

-- 书目：只放书源能重新提供的字段。
-- 分组、自定义封面/简介/标签、排序、dur_chapter_* 等属于「我书架里的这本书」，
-- 仍留在用户库 book_meta，读时按账号覆盖。
CREATE TABLE IF NOT EXISTS library_books (
    book_url             TEXT PRIMARY KEY,
    toc_url              TEXT,
    origin               TEXT,
    origin_name          TEXT,
    name                 TEXT,
    author               TEXT,
    kind                 TEXT,
    cover_url            TEXT,
    intro                TEXT,
    charset              TEXT,
    book_source_type     INTEGER NOT NULL DEFAULT 0,
    latest_chapter_title TEXT,
    latest_chapter_time  INTEGER NOT NULL DEFAULT 0,
    total_chapter_num    INTEGER NOT NULL DEFAULT 0,
    word_count           TEXT,
    -- 馆藏状态
    tier                 TEXT NOT NULL DEFAULT 'temp',   -- collection | temp
    collected_at         INTEGER,                         -- 入馆时间（epoch ms）
    last_accessed_at     INTEGER NOT NULL DEFAULT 0,      -- 最近读取/写入（epoch ms），TTL 依据
    checked_at           INTEGER NOT NULL DEFAULT 0,      -- 目录最近刷新（epoch ms）
    -- 冗余统计：列表页/监控直接读，避免每次聚合
    cached_chapters      INTEGER NOT NULL DEFAULT 0,
    content_bytes        INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_library_books_tier_access
    ON library_books(tier, last_accessed_at);
CREATE INDEX IF NOT EXISTS idx_library_books_collected
    ON library_books(collected_at DESC);

-- 目录：按 (book_url, url) 唯一。同一章节 URL 出现在不同书里时互不影响。
CREATE TABLE IF NOT EXISTS library_chapters (
    book_url    TEXT NOT NULL,
    url         TEXT NOT NULL,
    title       TEXT,
    is_volume   INTEGER NOT NULL DEFAULT 0,
    base_url    TEXT,
    "index"     INTEGER NOT NULL DEFAULT 0,
    is_vip      INTEGER NOT NULL DEFAULT 0,
    is_pay      INTEGER NOT NULL DEFAULT 0,
    resource_url TEXT,
    tag         TEXT,
    start       INTEGER NOT NULL DEFAULT 0,
    "end"       INTEGER NOT NULL DEFAULT 0,
    update_time TEXT,
    word_count  INTEGER NOT NULL DEFAULT 0,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (book_url, url)
);

CREATE INDEX IF NOT EXISTS idx_library_chapters_book_index
    ON library_chapters(book_url, "index");

-- 正文/图片：键 (url, origin)，避免镜像源同 URL 串正文。
-- scope_uid 预留给「登录态书源按账号私有」，0 = 公开（当前全部为 0）。
CREATE TABLE IF NOT EXISTS library_content (
    url           TEXT NOT NULL,
    origin        TEXT NOT NULL DEFAULT '',
    book_url      TEXT NOT NULL,
    title         TEXT,
    content       TEXT,
    chapter_index INTEGER NOT NULL DEFAULT 0,
    resource_url  TEXT,
    is_local      INTEGER NOT NULL DEFAULT 0,
    content_type  TEXT NOT NULL DEFAULT 'text',   -- text | image
    images        TEXT,                            -- 图片章节的 JSON
    size_bytes    INTEGER NOT NULL DEFAULT 0,
    cache_version TEXT,
    scope_uid     INTEGER NOT NULL DEFAULT 0,
    updated_at    TEXT NOT NULL DEFAULT (datetime('now')),
    accessed_at   TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (url, origin)
);

CREATE INDEX IF NOT EXISTS idx_library_content_book
    ON library_content(book_url);
CREATE INDEX IF NOT EXISTS idx_library_content_access
    ON library_content(accessed_at);

-- 采购单：入馆后的后台整本下载任务。
CREATE TABLE IF NOT EXISTS library_jobs (
    id         TEXT PRIMARY KEY,
    book_url   TEXT NOT NULL,
    source     TEXT NOT NULL DEFAULT '',
    uid        INTEGER NOT NULL DEFAULT 0,
    status     TEXT NOT NULL,          -- queued | running | done | partial | failed | cancelled
    total      INTEGER NOT NULL DEFAULT 0,
    done       INTEGER NOT NULL DEFAULT 0,
    failed     INTEGER NOT NULL DEFAULT 0,
    skipped    INTEGER NOT NULL DEFAULT 0,
    error      TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    finished_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_library_jobs_status
    ON library_jobs(status, created_at);
CREATE INDEX IF NOT EXISTS idx_library_jobs_book
    ON library_jobs(book_url, created_at DESC);

-- 每章状态：支持断点续跑与失败重试。
CREATE TABLE IF NOT EXISTS library_job_items (
    job_id        TEXT NOT NULL,
    chapter_url   TEXT NOT NULL,
    chapter_index INTEGER NOT NULL DEFAULT 0,
    status        TEXT NOT NULL,       -- pending | ok | failed | skipped
    error         TEXT,
    updated_at    INTEGER NOT NULL,
    PRIMARY KEY (job_id, chapter_url)
);
