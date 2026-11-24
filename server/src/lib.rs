//! Boranlib 服务端库入口。
//!
//! 以「库 + 二进制」两层暴露：`main.rs` 只负责进程启动，所有模块都放在这里，
//! 这样 `server/tests/` 下的集成测试（路由冒烟、规则 fixture）可以直接调用公开 API，
//! 不必再通过 `include!` 或复制源码。

pub mod auth;
pub mod cache_metrics;
pub mod db;
pub mod db_guard;
pub mod ebook_source;
pub use boranlib_ebook as ebook;
pub mod error;
pub mod image_cache;
pub mod image_fetch;
pub mod k_metrics;
pub mod library;
pub mod library_store;
pub mod library_worker;
pub mod models;
pub mod query_class;
pub mod routes;
// search_rank 的排序/配额/停止策略目前由单测与基准覆盖，生产路由尚未接入；
// 模块级放行 dead_code，避免 20+ 条逐项 allow 淹没真实告警。
pub mod search_priority;
pub mod source_bench;
pub mod source_health;
pub mod source_ledger;
pub mod source_quality;
pub mod source_query_stats;
pub mod web_book;

// 书源引擎内核已抽成独立 crate（默认不依赖 sqlx/axum）。
// 这里保留模块级再导出，服务端既有 `crate::req::…` 等路径无需改动。
pub use boranlib_rule_core::{
    config, crypto, entities, req, rule_engine, search_rank, serde_util, title_filter,
};
