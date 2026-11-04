-- 体检语料与精选书源（curated）：
-- 1) 内置语料写在 Rust 常量（builtin=1 的行只承载"用户停用覆盖"，标题/作者/分类始终以常量回填）；
-- 2) 用户追加语料存本表（builtin=0），可编辑、可删除；
-- 3) 体检结果按批次落 source_bench_runs（保留历史，便于按 category 聚合与"可疑语料"标记）；
-- 4) source_curated 只记录当前精选状态，curated 是 preferred 的子集标记，不新增 tier。
CREATE TABLE IF NOT EXISTS source_bench_corpus (
    id              TEXT PRIMARY KEY,
    title           TEXT NOT NULL,
    expected_author TEXT,
    category        TEXT NOT NULL DEFAULT '',
    source_type     INTEGER NOT NULL DEFAULT 0, -- 0 小说 / 2 漫画
    enabled         INTEGER NOT NULL DEFAULT 1,
    builtin         INTEGER NOT NULL DEFAULT 0,
    norm_title      TEXT NOT NULL,
    created_at      INTEGER,
    updated_at      INTEGER
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_source_bench_corpus_key
    ON source_bench_corpus(norm_title, source_type);
CREATE INDEX IF NOT EXISTS idx_source_bench_corpus_type
    ON source_bench_corpus(source_type, enabled);

CREATE TABLE IF NOT EXISTS source_bench_runs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_id        TEXT NOT NULL,
    source_url      TEXT NOT NULL,
    corpus_key      TEXT NOT NULL,
    title           TEXT NOT NULL,
    category        TEXT NOT NULL DEFAULT '',
    source_type     INTEGER NOT NULL DEFAULT 0,
    builtin_sample  INTEGER NOT NULL DEFAULT 0,
    found           INTEGER NOT NULL DEFAULT 0,
    title_match     INTEGER NOT NULL DEFAULT 0,
    author_match    INTEGER NOT NULL DEFAULT 0,
    latency_ms      INTEGER NOT NULL DEFAULT 0,
    error_kind      TEXT,
    suspicious      INTEGER NOT NULL DEFAULT 0,
    run_at          INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_source_bench_runs_source
    ON source_bench_runs(source_url, run_at);
CREATE INDEX IF NOT EXISTS idx_source_bench_runs_batch
    ON source_bench_runs(batch_id);

CREATE TABLE IF NOT EXISTS source_curated (
    source_url      TEXT PRIMARY KEY,
    source_type     INTEGER NOT NULL DEFAULT 0,
    bench_score     INTEGER NOT NULL DEFAULT 0,
    curated         INTEGER NOT NULL DEFAULT 0,
    samples         INTEGER NOT NULL DEFAULT 0,
    fail_streak     INTEGER NOT NULL DEFAULT 0,
    last_bench_at   INTEGER,
    meta_json       TEXT
);

CREATE INDEX IF NOT EXISTS idx_source_curated_flag
    ON source_curated(curated, bench_score);
