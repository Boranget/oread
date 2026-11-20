-- 漫画图片真正落盘：馆藏漫画不再只存图片 URL 清单。
--
-- 分工：
--   library_content.images      章节图片清单（有序 URL + 宽高/字节数）
--   library_images              图片原始字节（按 url + origin 去重，BLOB 存库）
--
-- 只有 tier=collection 的漫画由 worker 下载图片；临时层仍走进程内 LRU。
-- 图片与正文一起参与整本删除 / TTL 回收，不单独淘汰。

-- 书目冗余统计：图片占用（字节）。content_bytes 仍表示正文 + 图片清单 JSON。
ALTER TABLE library_books ADD COLUMN image_bytes INTEGER NOT NULL DEFAULT 0;

-- 图片章节的完整性标记（只对 content_type='image' 有意义）：
--   1 = 清单里的图片已全部落盘
--   0 = 只有清单，图片尚未下载完整
-- 老图片章节保持 0，等待 worker 重新补齐；文本章节不看这一列。
ALTER TABLE library_content ADD COLUMN images_ready INTEGER NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS library_images (
    url         TEXT NOT NULL,
    origin      TEXT NOT NULL DEFAULT '',
    book_url    TEXT NOT NULL,
    mime        TEXT NOT NULL DEFAULT '',
    data        BLOB NOT NULL,
    size_bytes  INTEGER NOT NULL DEFAULT 0,
    width       INTEGER,
    height      INTEGER,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    accessed_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (url, origin)
);

CREATE INDEX IF NOT EXISTS idx_library_images_book
    ON library_images(book_url);
CREATE INDEX IF NOT EXISTS idx_library_images_access
    ON library_images(accessed_at);
