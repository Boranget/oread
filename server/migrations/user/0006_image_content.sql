-- 图片正文缓存：content_type=text|image；images 为 {images:[{index,url,width?,height?}],truncated} 的 JSON。
-- 老行默认按文本，content 字段语义不变（图片章节 content 为空串）。
ALTER TABLE book_content ADD COLUMN content_type TEXT NOT NULL DEFAULT 'text';
ALTER TABLE book_content ADD COLUMN images TEXT;
