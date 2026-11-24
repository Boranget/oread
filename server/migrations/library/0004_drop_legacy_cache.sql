-- 目录即书：正文与漫画图片只存书目录，旧缓存表不再使用。
--
-- 旧设备上的正文/图片缓存可以直接丢弃：书重新打开时会按目录重新抓取。
DROP TABLE IF EXISTS library_content;
DROP TABLE IF EXISTS library_images;
