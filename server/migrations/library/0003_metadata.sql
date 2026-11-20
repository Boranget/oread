-- 馆藏书目补「可交换元数据」：系列、语言、出版信息、标识与标签。
--
-- 分层约定：
--   全局（本库）：OPF 语义字段，谁抓到都一样，只存一份；
--   个人（users/<uid>.db）：星级、分组、私人标签，不进这里。
--
-- 文本字段一律可空；写入时空串归一为 NULL（管理员用它清空字段）。

ALTER TABLE library_books ADD COLUMN language TEXT;
ALTER TABLE library_books ADD COLUMN publisher TEXT;
ALTER TABLE library_books ADD COLUMN pubdate TEXT;
ALTER TABLE library_books ADD COLUMN series_name TEXT;
ALTER TABLE library_books ADD COLUMN series_index REAL;

-- 标识：对应 OPF <dc:identifier>，一个 scheme 可有多值（如多版本 ISBN）。
CREATE TABLE IF NOT EXISTS library_identifiers (
    book_url TEXT NOT NULL,
    scheme   TEXT NOT NULL,
    value    TEXT NOT NULL,
    PRIMARY KEY (book_url, scheme, value)
);

CREATE INDEX IF NOT EXISTS idx_library_identifiers_value
    ON library_identifiers(scheme, value);

-- 标签：对应 OPF <dc:subject>。
CREATE TABLE IF NOT EXISTS library_tags (
    book_url TEXT NOT NULL,
    tag      TEXT NOT NULL,
    PRIMARY KEY (book_url, tag)
);

CREATE INDEX IF NOT EXISTS idx_library_tags_tag
    ON library_tags(tag);
