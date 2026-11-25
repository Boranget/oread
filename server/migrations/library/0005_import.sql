-- 本地 EPUB 导入：记录来源文件指纹，供三层查重使用。
--
-- file_sha256          上传包的 SHA-256；同一文件重复导入时直接命中。
-- content_fingerprint  清洗后正文的 SHA-256；不同文件、同内容的书也会命中。
--                      纯图册或正文过短时不写（NULL），避免误判。
-- import_filename      原始文件名，只用于展示与排查。
ALTER TABLE library_books ADD COLUMN file_sha256 TEXT;
ALTER TABLE library_books ADD COLUMN content_fingerprint TEXT;
ALTER TABLE library_books ADD COLUMN import_filename TEXT;

CREATE INDEX IF NOT EXISTS idx_library_books_file_sha256
    ON library_books(file_sha256);
CREATE INDEX IF NOT EXISTS idx_library_books_content_fingerprint
    ON library_books(content_fingerprint);
