-- 三层模型：本地源库（book_sources）+ 导入账本（只追溯/diff）+ 隔离决策。
-- 隔离保留原始定义（仍在 book_sources 中），只是 decision=quarantined，不再进入搜索与巡检。

-- 每次导入的批次记录（来源追溯 + raw_sha 判定重复导入）。
CREATE TABLE IF NOT EXISTS import_batches (
    id             TEXT PRIMARY KEY,
    source         TEXT NOT NULL,          -- 合集 URL / 文件名 / inline
    source_kind    TEXT NOT NULL,          -- collection | file | api | single | inline
    imported_at    INTEGER NOT NULL,       -- unix epoch 毫秒
    item_count     INTEGER NOT NULL,
    raw_sha        TEXT,                   -- 原始请求体规范化后的 SHA-256
    probe_summary  TEXT                    -- 体检汇总 JSON（可为空）
);
CREATE INDEX IF NOT EXISTS idx_import_batches_time ON import_batches(imported_at);

-- 每次导入的逐条明细（保留原始定义快照，便于上游更新 diff 与复活）。
CREATE TABLE IF NOT EXISTS import_items (
    batch_id      TEXT NOT NULL,
    source_url    TEXT NOT NULL,
    definition    TEXT NOT NULL,           -- 规范化后的书源 JSON 快照（保留规则主体）
    content_sha   TEXT NOT NULL,
    probe_status  TEXT NOT NULL,           -- ok | fail | never
    decision      TEXT NOT NULL,           -- active | quarantined
    reason        TEXT,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (batch_id, source_url)
);
CREATE INDEX IF NOT EXISTS idx_import_items_url ON import_items(source_url);

-- 当前每条源在本地库中的决策（活跃 / 隔离）。缺失行按 active 兼容历史数据。
CREATE TABLE IF NOT EXISTS source_library_state (
    source_url        TEXT PRIMARY KEY,
    decision          TEXT NOT NULL DEFAULT 'active', -- active | quarantined
    reason            TEXT,
    first_imported_at INTEGER,
    last_imported_at  INTEGER,
    last_batch_id     TEXT,
    content_sha       TEXT,                 -- 上次被接受/记录的规则哈希
    review_state      TEXT,                 -- NULL | update_rejected | conflict
    review_reason     TEXT,
    review_at         INTEGER
);
CREATE INDEX IF NOT EXISTS idx_source_library_decision ON source_library_state(decision);
