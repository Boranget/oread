//! Boranlib 服务端库入口。
//!
//! 以「库 + 二进制」两层暴露：`main.rs` 只负责进程启动，所有模块都放在这里，
//! 这样 `server/tests/` 下的集成测试（路由冒烟、规则 fixture）可以直接调用公开 API，
//! 不必再通过 `include!` 或复制源码。

pub mod auth;
pub mod cache_metrics;
pub mod config;
pub mod crypto;
pub mod db;
pub mod db_guard;
pub mod entities;
pub mod error;
pub mod image_cache;
pub mod k_metrics;
pub mod models;
pub mod query_class;
pub mod req;
pub mod routes;
pub mod rule_engine;
// search_rank 的排序/配额/停止策略目前由单测与基准覆盖，生产路由尚未接入；
// 模块级放行 dead_code，避免 20+ 条逐项 allow 淹没真实告警。
pub mod search_priority;
#[allow(dead_code)]
pub mod search_rank;
pub mod serde_util;
pub mod source_bench;
pub mod source_health;
pub mod source_ledger;
pub mod source_quality;
pub mod source_query_stats;
pub mod title_filter;
pub mod web_book;
