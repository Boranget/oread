-- 目录章节元信息：来源站点的更新时间与正文字数（字数在正文缓存落库时回填）
ALTER TABLE book_chapters ADD COLUMN update_time TEXT;
ALTER TABLE book_chapters ADD COLUMN word_count INTEGER NOT NULL DEFAULT 0;
