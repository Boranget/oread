-- 书源类型落地：只做小说(0) + 漫画(2)；音频(1) 及其他值落库但默认禁用。
ALTER TABLE book_sources ADD COLUMN book_source_type INTEGER NOT NULL DEFAULT 0;
ALTER TABLE book_sources ADD COLUMN unsupported_type INTEGER NOT NULL DEFAULT 0;

-- 存量回填：兼容内部 snake_case 与 Legado camelCase 两种存法；
-- 非法/缺失 JSON 一律按 0（小说），不因单条脏数据阻塞迁移。
UPDATE book_sources SET book_source_type = COALESCE(CAST(
    COALESCE(
        CASE WHEN json_valid(content) THEN
            COALESCE(json_extract(content, '$.book_source_type'),
                     json_extract(content, '$.bookSourceType'))
        END,
        CASE WHEN json_valid(rules) THEN
            COALESCE(json_extract(rules, '$.book_source_type'),
                     json_extract(rules, '$.bookSourceType'))
        END
    ) AS INTEGER), 0);

UPDATE book_sources SET unsupported_type =
    CASE WHEN book_source_type IN (0, 2) THEN 0 ELSE 1 END;

-- 非小说/漫画源不参与搜索与发现，统一默认禁用（保留数据，可在详情查看）
UPDATE book_sources SET enabled = 0 WHERE unsupported_type = 1;
