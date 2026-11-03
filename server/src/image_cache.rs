//! 图片代理的进程内 LRU 缓存。
//!
//! 只缓存成功抓取的小图（默认单项上限 2 MiB），总容量默认 64 MiB；
//! 超出后按最近最少使用淘汰。缓存命中时不重新回源，直接内存输出。

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone)]
struct Entry {
    data: Vec<u8>,
    content_type: String,
    size: usize,
    seq: u64,
}

/// 可独立测试的 LRU 实现。
pub struct ImageLru {
    limit: usize,
    used: usize,
    seq: u64,
    entries: HashMap<String, Entry>,
    /// 淘汰顺序：记录 (key, seq)。seq 不匹配的旧记录直接跳过（惰性清理）。
    order: VecDeque<(String, u64)>,
}

impl ImageLru {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            used: 0,
            seq: 0,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn get(&mut self, key: &str) -> Option<(Vec<u8>, String)> {
        self.seq += 1;
        let seq = self.seq;
        let entry = self.entries.get_mut(key)?;
        entry.seq = seq;
        let out = (entry.data.clone(), entry.content_type.clone());
        let k = key.to_string();
        self.order.push_back((k, seq));
        Some(out)
    }

    pub fn put(&mut self, key: &str, data: Vec<u8>, content_type: String) {
        let size = data.len();
        if size == 0 || size > self.limit {
            return;
        }
        self.seq += 1;
        let seq = self.seq;
        if let Some(old) = self.entries.remove(key) {
            self.used = self.used.saturating_sub(old.size);
        }
        self.used += size;
        self.entries.insert(
            key.to_string(),
            Entry {
                data,
                content_type,
                size,
                seq,
            },
        );
        self.order.push_back((key.to_string(), seq));
        self.evict();
    }

    fn evict(&mut self) {
        while self.used > self.limit {
            let Some((key, seq)) = self.order.pop_front() else {
                break;
            };
            let stale = match self.entries.get(&key) {
                Some(e) => e.seq != seq,
                None => true,
            };
            if stale {
                continue;
            }
            if let Some(e) = self.entries.remove(&key) {
                self.used = self.used.saturating_sub(e.size);
            }
        }
        // 惰性清理防止 order 无限增长
        if self.order.len() > self.entries.len() * 4 + 64 {
            let keep: VecDeque<(String, u64)> = self
                .order
                .iter()
                .filter(|(k, s)| self.entries.get(k).map(|e| e.seq == *s).unwrap_or(false))
                .cloned()
                .collect();
            self.order = keep;
        }
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        (self.used, self.entries.len(), self.limit)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.used = 0;
    }
}

fn global() -> &'static Mutex<ImageLru> {
    static CACHE: OnceLock<Mutex<ImageLru>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ImageLru::new(64 * 1024 * 1024)))
}

/// 启动时按配置初始化容量（仅在尚未写入数据时生效）。
pub fn init(limit_bytes: usize) {
    let mut c = global().lock().unwrap();
    if c.entries.is_empty() {
        c.limit = limit_bytes.max(1024 * 1024);
    }
}

pub fn get(key: &str) -> Option<(Vec<u8>, String)> {
    global().lock().ok()?.get(key)
}

pub fn put(key: &str, data: Vec<u8>, content_type: String) {
    if let Ok(mut c) = global().lock() {
        c.put(key, data, content_type);
    }
}

/// `(已用字节, 条目数, 容量字节)`
pub fn stats() -> (usize, usize, usize) {
    global()
        .lock()
        .map(|c| c.stats())
        .unwrap_or((0, 0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_evicts_least_recently_used() {
        let mut c = ImageLru::new(10);
        c.put("a", vec![0; 4], "image/png".into());
        c.put("b", vec![0; 4], "image/png".into());
        // 访问 a，使其成为最近使用；再次插入 c 时应淘汰 b
        assert!(c.get("a").is_some());
        c.put("c", vec![0; 4], "image/png".into());
        assert!(c.get("a").is_some());
        assert!(c.get("b").is_none());
        assert!(c.get("c").is_some());
        assert_eq!(c.stats().0, 8);
    }

    #[test]
    fn oversized_item_is_not_cached() {
        let mut c = ImageLru::new(10);
        c.put("big", vec![0; 11], "image/png".into());
        assert!(c.get("big").is_none());
        assert_eq!(c.stats(), (0, 0, 10));
    }

    #[test]
    fn rewriting_key_does_not_double_count() {
        let mut c = ImageLru::new(100);
        c.put("k", vec![0; 10], "image/png".into());
        c.put("k", vec![0; 5], "image/gif".into());
        assert_eq!(c.stats().0, 5);
        assert_eq!(c.get("k").unwrap().1, "image/gif");
    }
}
