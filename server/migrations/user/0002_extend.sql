-- 扩展用户级替换规则（支持作用域/正则/标题正文开关）
ALTER TABLE replace_rules ADD COLUMN name TEXT;
ALTER TABLE replace_rules ADD COLUMN "group" TEXT;
ALTER TABLE replace_rules ADD COLUMN scope TEXT;
ALTER TABLE replace_rules ADD COLUMN is_regex INTEGER NOT NULL DEFAULT 0;
ALTER TABLE replace_rules ADD COLUMN is_enabled INTEGER NOT NULL DEFAULT 1;
ALTER TABLE replace_rules ADD COLUMN scope_title INTEGER NOT NULL DEFAULT 1;
ALTER TABLE replace_rules ADD COLUMN scope_content INTEGER NOT NULL DEFAULT 1;
ALTER TABLE replace_rules ADD COLUMN exclude_scope TEXT;
ALTER TABLE replace_rules ADD COLUMN "order" INTEGER NOT NULL DEFAULT 0;

-- 书籍元数据缓存（详情页/书架展示用，按 book_url 物理唯一）
CREATE TABLE IF NOT EXISTS book_meta (
    book_url            TEXT PRIMARY KEY,
    toc_url             TEXT,
    origin              TEXT,
    origin_name         TEXT,
    name                TEXT,
    author              TEXT,
    kind                TEXT,
    cover_url           TEXT,
    intro               TEXT,
    charset             TEXT,
    book_source_type    INTEGER NOT NULL DEFAULT 0,
    "group"             TEXT,
    latest_chapter_title TEXT,
    latest_chapter_time  INTEGER NOT NULL DEFAULT 0,
    total_chapter_num    INTEGER NOT NULL DEFAULT 0,
    dur_chapter_title    TEXT,
    dur_chapter_index    INTEGER NOT NULL DEFAULT 0,
    dur_chapter_pos      INTEGER NOT NULL DEFAULT 0,
    dur_chapter_time     INTEGER NOT NULL DEFAULT 0,
    word_count           TEXT,
    can_update           INTEGER NOT NULL DEFAULT 1,
    "order"              INTEGER NOT NULL DEFAULT 0,
    origin_order         INTEGER NOT NULL DEFAULT 0,
    variable             TEXT,
    custom_tag           TEXT,
    custom_cover_url     TEXT,
    custom_intro         TEXT
);

-- 目录缓存（按章节 url 唯一）
CREATE TABLE IF NOT EXISTS book_chapters (
    url          TEXT PRIMARY KEY,
    book_url     TEXT NOT NULL,
    title        TEXT,
    is_volume    INTEGER NOT NULL DEFAULT 0,
    base_url     TEXT,
    "index"      INTEGER NOT NULL DEFAULT 0,
    is_vip       INTEGER NOT NULL DEFAULT 0,
    is_pay       INTEGER NOT NULL DEFAULT 0,
    resource_url TEXT,
    tag          TEXT,
    start        INTEGER NOT NULL DEFAULT 0,
    "end"        INTEGER NOT NULL DEFAULT 0
);

-- 正文缓存（按章节 url 唯一）
CREATE TABLE IF NOT EXISTS book_content (
    url            TEXT PRIMARY KEY,
    book_url       TEXT NOT NULL,
    title          TEXT,
    content        TEXT,
    chapter_index  INTEGER NOT NULL DEFAULT 0,
    resource_url   TEXT,
    is_local       INTEGER NOT NULL DEFAULT 0
);
