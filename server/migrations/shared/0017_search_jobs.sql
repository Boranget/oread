-- 搜索任务持久化：让「关掉前端后任务继续跑、重新登录还能看到进度并管理」成立。
-- 运行期进度以内存注册表为准；这里保存任务元数据与最终快照（books/errors），
-- 内存过期或进程重启后仍能列出任务、查看已完成的搜索结果。
CREATE TABLE IF NOT EXISTS search_jobs (
    id TEXT PRIMARY KEY,
    uid INTEGER NOT NULL,
    keyword TEXT NOT NULL,
    scope TEXT NOT NULL,
    source_mode TEXT NOT NULL DEFAULT '',
    coverage TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL,                 -- running | done | cancelled | interrupted
    source_total INTEGER NOT NULL DEFAULT 0,
    finished INTEGER NOT NULL DEFAULT 0,
    pending INTEGER NOT NULL DEFAULT 0,
    skipped INTEGER NOT NULL DEFAULT 0,
    cancelled INTEGER NOT NULL DEFAULT 0,
    failed INTEGER NOT NULL DEFAULT 0,
    curated_total INTEGER NOT NULL DEFAULT 0,
    scope_filtered INTEGER NOT NULL DEFAULT 0,
    skipped_cooldown INTEGER NOT NULL DEFAULT 0,
    cooldown_total INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    finished_at INTEGER,
    snapshot_json TEXT                    -- {"books":[...],"errors":[...]}
);

CREATE INDEX IF NOT EXISTS idx_search_jobs_uid_created
    ON search_jobs(uid, created_at DESC);
