//! HTTP 请求层（AnalyzeUrl 等价封装）
//! 低资源约束：全进程复用同一个 reqwest Client，全局并发信号量限制抓取数量，
//! 响应体按流式读取并硬性截断，避免大响应把内存吃满。
//!
//! 请求按书源隔离：
//! - `header` 字段（JSON / 多行 / `@js:`）由 `source_headers` 解析后注入每一次请求；
//! - Cookie 用进程内小 LRU 按 `bookSourceUrl` 分桶，桶数 ≤64、单桶 ≤8KB，支持 Set-Cookie 回写，不落盘；
//! - 响应体按 Content-Type / meta charset / 字节嗅探解码（UTF-8、GBK、GB18030、Big5），
//!   解码失败给出具名错误，不做有损替换；
//! - 重试分级：连接失败与超时不重试，5xx 重试 1 次。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use encoding_rs::{Encoding, BIG5, GB18030};
use futures::StreamExt;
use once_cell::sync::{Lazy, OnceCell};
use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode, Url};
use tokio::sync::Semaphore;

use crate::config::{
    Config, SOURCE_CONCURRENCY_DEFAULT, SOURCE_CONCURRENCY_MAX, SOURCE_CONCURRENCY_MIN,
};
use crate::entities::BookSource;

#[path = "req_config.rs"]
mod req_config;
pub use req_config::*;
#[path = "req_http.rs"]
mod req_http;
pub use req_http::*;
#[path = "req_cookie.rs"]
mod req_cookie;
use req_cookie::*;
pub(crate) use req_cookie::{js_cookie_clear, js_cookie_get, js_cookie_remove, js_cookie_set};
#[path = "req_text.rs"]
mod req_text;
use req_text::*;
#[path = "req_fetch.rs"]
mod req_fetch;
pub use req_fetch::*;
#[path = "req_url.rs"]
mod req_url;
pub use req_url::*;
#[path = "req_stream.rs"]
mod req_stream;
pub use req_stream::*;

#[cfg(test)]
#[path = "req_tests.rs"]
mod tests;
