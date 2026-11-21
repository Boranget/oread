//! 书源引擎内核（开源阅读 AnalyzeRule / AnalyzeUrl 的等价实现）。
//!
//! 这里只有「给定书源规则与 URL → 结构化数据」所需的东西：
//! HTTP 抓取预算、字符集解码、Cookie、链接补全、规则解释器、书目结构体。
//!
//! 纪律：
//! - 默认构建**不依赖 sqlx / axum**，不认识数据库、路由和 `data_dir`；
//! - 抓取行为只由 [`config::Config`] 里的限额驱动，测试可自行构造；
//! - 引擎不落盘（规则缓存目录是唯一例外，由调用方显式初始化）。

pub mod config;
pub mod crypto;
pub mod entities;
pub mod req;
pub mod rule_engine;
#[allow(dead_code)]
pub mod search_rank;
pub mod serde_util;
pub mod title_filter;
