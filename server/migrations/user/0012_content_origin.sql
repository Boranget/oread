-- 正文缓存键加入来源：同一章节 URL 在不同书源（镜像/同源转发）下必须互不串正文。
-- SQLite 不能直接修改主键，这里重建表；旧数据统一记为 origin=''，首次按来源读取时按未命中重建。
CREATE TABLE book_content_new (
    url            TEXT NOT NULL,
    origin         TEXT NOT NULL DEFAULT '',
    book_url       TEXT NOT NULL,
    title          TEXT,
    content        TEXT,
    chapter_index  INTEGER NOT NULL DEFAULT 0,
    resource_url   TEXT,
    is_local       INTEGER NOT NULL DEFAULT 0,
    updated_at     TEXT,
    accessed_at    TEXT,
    size_bytes     INTEGER NOT NULL DEFAULT 0,
    content_type   TEXT NOT NULL DEFAULT 'text',
    images         TEXT,
    cache_version  TEXT,
    PRIMARY KEY (url, origin)
);

INSERT INTO book_content_new
    (url, origin, book_url, title, content, chapter_index, resource_url, is_local,
     updated_at, accessed_at, size_bytes, content_type, images, cache_version)
SELECT url, '', book_url, title, content, chapter_index, resource_url, is_local,
       updated_at, accessed_at, size_bytes, content_type, images, cache_version
FROM book_content;

DROP TABLE book_content;
ALTER TABLE book_content_new RENAME TO book_content;

CREATE INDEX IF NOT EXISTS idx_book_content_book ON book_content (book_url);
CREATE INDEX IF NOT EXISTS idx_book_content_accessed ON book_content (accessed_at);
