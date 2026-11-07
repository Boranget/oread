//! 体检语料与精选书源（curated）。
//!
//! 设计要点：
//! - **内置语料写在 Rust 常量**（随版本发布、不可删除/编辑，只能临时停用），
//!   用户追加的语料存 `source_bench_corpus`（builtin=0，可改可删）。
//! - 体检按“书名 + 匹配度 + 速度”给源打分，**单条书名的失败不直接扣源的分**：
//!   先在 category 内聚合命中率，再把明显低于同 category 中位数的语料标记为
//!   `suspicious`（语料可能失效）并排除出评分。
//! - `curated` 只是 `preferred` 的子集标记，不新增 tier；断层截断数量夹在 8..14。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

use crate::config::Config;
use crate::entities::{BookSource, SearchBook};
use crate::error::{AppError, AppResult};
use crate::source_health::{self, classify, FailureKind, Outcome};
use crate::web_book;

#[path = "source_bench_corpus.rs"]
mod corpus;
use corpus::*;
#[path = "source_bench_match.rs"]
mod matcher;
use matcher::*;
#[path = "source_bench_store.rs"]
mod store;
use store::*;
#[path = "source_bench_job.rs"]
mod job;
use job::*;
#[path = "source_bench_run.rs"]
mod run;
use run::*;
#[path = "source_bench_curated.rs"]
mod curated;
use curated::*;

// 路由层与 source_health 原先从 `source_bench::*` 取用的公开项。
pub use corpus::{TYPE_COMIC, TYPE_NOVEL};
pub use curated::{
    curated_lists, curated_set, maybe_schedule_stale_bench, revalidate_curated_source,
};
pub use job::{bench_job_view, start_bench_job};
pub use store::{
    add_user_corpus, delete_corpus, load_corpus, toggle_corpus, update_user_corpus, CorpusInput,
};

#[allow(clippy::field_reassign_with_default, clippy::type_complexity)] // 测试里逐字段构造对象、就地写行元组，均便于对照真实结构。
#[cfg(test)]
#[path = "source_bench_tests.rs"]
mod tests;
