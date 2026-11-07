//! 正文缓存的可观测性计数（进程级、生命周期内累计）。
//!
//! 只做计数，不改缓存策略：`/healthz` 读取快照即可观察命中率、落库次数与淘汰量，
//! 为 TTL / 缓存上限的参数校准提供前后对比数据。

use std::sync::atomic::{AtomicU64, Ordering};

static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static SAVES: AtomicU64 = AtomicU64::new(0);
static EVICTIONS: AtomicU64 = AtomicU64::new(0);
static EVICTED_ROWS: AtomicU64 = AtomicU64::new(0);

pub fn record_hit() {
    HITS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_miss() {
    MISSES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_save() {
    SAVES.fetch_add(1, Ordering::Relaxed);
}

/// 一次淘汰轮次删掉了 `rows` 行（空轮次由调用方跳过，不计入）。
pub fn record_eviction(rows: u64) {
    EVICTIONS.fetch_add(1, Ordering::Relaxed);
    EVICTED_ROWS.fetch_add(rows, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub saves: u64,
    pub evictions: u64,
    pub evicted_rows: u64,
}

impl CacheStats {
    /// 命中率（0.0–1.0，保留三位小数）；无流量时为 0。
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            return 0.0;
        }
        (self.hits as f64 / total as f64 * 1000.0).round() / 1000.0
    }
}

pub fn stats() -> CacheStats {
    CacheStats {
        hits: HITS.load(Ordering::Relaxed),
        misses: MISSES.load(Ordering::Relaxed),
        saves: SAVES.load(Ordering::Relaxed),
        evictions: EVICTIONS.load(Ordering::Relaxed),
        evicted_rows: EVICTED_ROWS.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
#[path = "cache_metrics_tests.rs"]
mod tests;
