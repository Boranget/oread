//! 只测纯计算：计数器是进程级静态量，测试并行执行时不宜断言绝对值。

use super::CacheStats;

#[test]
fn hit_rate_is_zero_without_traffic() {
    assert_eq!(CacheStats::default().hit_rate(), 0.0);
}

#[test]
fn hit_rate_rounds_to_three_decimals() {
    let stats = CacheStats {
        hits: 1,
        misses: 2,
        ..Default::default()
    };
    assert_eq!(stats.hit_rate(), 0.333);
    let stats = CacheStats {
        hits: 2,
        misses: 1,
        ..Default::default()
    };
    assert_eq!(stats.hit_rate(), 0.667);
}

#[test]
fn hit_rate_all_hits_and_all_misses() {
    let stats = CacheStats {
        hits: 7,
        ..Default::default()
    };
    assert_eq!(stats.hit_rate(), 1.0);
    let stats = CacheStats {
        misses: 7,
        ..Default::default()
    };
    assert_eq!(stats.hit_rate(), 0.0);
}
