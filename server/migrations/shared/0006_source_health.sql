-- 书源健康状态（客观观测）：与 book_sources.enabled（用户意志）互不覆盖。
-- 时间均为 Unix epoch 毫秒（INTEGER），便于直接与运行时 now 比较。
CREATE TABLE IF NOT EXISTS source_health (
    source_url           TEXT PRIMARY KEY,
    last_checked_at      INTEGER,
    last_status          TEXT NOT NULL DEFAULT 'never', -- ok | partial | fail | never
    last_error           TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    last_ok_at           INTEGER,
    cooldown_until       INTEGER,
    avg_latency_ms       INTEGER,
    check_total          INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_source_health_cooldown
    ON source_health(cooldown_until);
CREATE INDEX IF NOT EXISTS idx_source_health_status
    ON source_health(last_status, last_checked_at);
