-- 搜索结果行表：边搜边落库，内存只保留计数。
--
-- 排序不变量（翻页不重不漏的前提）：
-- - `seq` 首次入库时分配，单调递增，之后永不改变；
-- - `score` 首次入库时按相关性 + 源权重算一次，之后永不改变；
-- - `book_key` 去重键（归一化书名 + 作者），同书多源只追加来源、不动前两者。
--
-- `frozen_seq` 是「首屏边界」：运行中第一次翻页时冻结，
-- 翻页只看 `seq < frozen_seq`（即边界前已入库的条数），后台新结果不会挤动已看过的页。
CREATE TABLE IF NOT EXISTS search_job_books (
    job_id    TEXT    NOT NULL,
    seq       INTEGER NOT NULL,
    score     INTEGER NOT NULL,
    book_key  TEXT    NOT NULL,
    book_json TEXT    NOT NULL,
    bytes     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (job_id, seq),
    UNIQUE (job_id, book_key)
);

CREATE INDEX IF NOT EXISTS idx_search_job_books_page
    ON search_job_books(job_id, score DESC, seq ASC);

-- 任务级计数：轮询只读这些列，不再扫结果表。
ALTER TABLE search_jobs ADD COLUMN next_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE search_jobs ADD COLUMN raw_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE search_jobs ADD COLUMN result_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE search_jobs ADD COLUMN result_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE search_jobs ADD COLUMN frozen_seq INTEGER;
ALTER TABLE search_jobs ADD COLUMN stop_reason TEXT NOT NULL DEFAULT '';
ALTER TABLE search_jobs ADD COLUMN errors_json TEXT;

CREATE INDEX IF NOT EXISTS idx_search_job_books_job ON search_job_books(job_id);
