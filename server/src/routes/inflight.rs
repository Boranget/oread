//! 同键并发合并（single-flight）：同一用户、同一章节的并发缓存未命中只抓取一次。
//!
//! 键由调用方构造（用户 + 书源 + 章节 URL）。等待者在拿到闸门后必须重新查缓存，
//! 这样第一个请求落库后，其余请求直接命中，不再重复抓取/解码。
//! 最后一个等待者离开时清理条目，避免键集合无限增长。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

struct Entry {
    gate: Arc<AsyncMutex<()>>,
    waiters: usize,
}

static GATES: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();

fn gates() -> &'static Mutex<HashMap<String, Entry>> {
    GATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 持有期间同键请求排队；Drop 时归还等待者计数。
pub struct Flight {
    key: String,
    _guard: OwnedMutexGuard<()>,
}

impl Drop for Flight {
    fn drop(&mut self) {
        let mut map = gates().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = map.get_mut(&self.key) {
            entry.waiters = entry.waiters.saturating_sub(1);
            if entry.waiters == 0 {
                map.remove(&self.key);
            }
        }
    }
}

/// 获取同键闸门；同名并发调用按到达顺序串行通过。
pub async fn acquire(key: impl Into<String>) -> Flight {
    let key = key.into();
    let gate = {
        let mut map = gates().lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(key.clone()).or_insert_with(|| Entry {
            gate: Arc::new(AsyncMutex::new(())),
            waiters: 0,
        });
        entry.waiters += 1;
        entry.gate.clone()
    };
    let guard = gate.lock_owned().await;
    Flight { key, _guard: guard }
}

#[cfg(test)]
pub fn has_key(key: &str) -> bool {
    gates().lock().unwrap().contains_key(key)
}

#[cfg(test)]
pub fn waiters(key: &str) -> usize {
    gates()
        .lock()
        .unwrap()
        .get(key)
        .map(|e| e.waiters)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "inflight_tests.rs"]
mod tests;
