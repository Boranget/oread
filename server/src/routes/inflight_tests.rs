//! single-flight 行为测试：同键串行、异键并行、条目清理。

use super::{acquire, has_key, waiters};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::time::{timeout, Duration};

static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static MAX_ACTIVE: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn same_key_serializes_and_entries_are_cleaned() {
    // 4 个同键请求：任一时刻只允许 1 个处于临界区。
    let tasks: Vec<_> = (0..4)
        .map(|_| {
            tokio::spawn(async move {
                let _flight = acquire("same-key").await;
                let now = ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
                MAX_ACTIVE.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                ACTIVE.fetch_sub(1, Ordering::SeqCst);
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(MAX_ACTIVE.load(Ordering::SeqCst), 1, "同键不得并发进入");
    assert_eq!(ACTIVE.load(Ordering::SeqCst), 0);
    assert!(!has_key("same-key"), "等待者归零后应清理");
}

#[tokio::test]
async fn different_keys_do_not_block() {
    let held = acquire("key-a").await;
    let other = timeout(Duration::from_millis(300), async {
        let _flight = acquire("key-b").await;
    })
    .await;
    assert!(other.is_ok(), "异键不应被同键闸门阻塞");
    drop(held);
    assert!(!has_key("key-a") && !has_key("key-b"));
}

#[tokio::test]
async fn waiters_keep_entry_until_last_leaves() {
    let first = acquire("key-c").await;
    let second = tokio::spawn(async {
        let _flight = acquire("key-c").await;
    });
    // 等第二个任务注册进 map（等待者=2）再释放第一个，避免竞态。
    for _ in 0..100 {
        if super::waiters("key-c") == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(waiters("key-c"), 2, "第二个请求应已排队");
    assert!(has_key("key-c"), "仍有等待者时不得提前清理");
    drop(first);
    second.await.unwrap();
    assert!(!has_key("key-c"));
}
