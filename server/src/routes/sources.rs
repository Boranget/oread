use axum::{
    extract::{Path, Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use std::time::Instant;

use crate::auth::AuthUser;
use crate::entities::BookSource;
use crate::error::{AppError, AppResult};
use crate::models::BookSourceListItem;
use crate::routes::AppState;
use crate::source_health;
use crate::source_ledger::{self, LibraryState};
use crate::source_quality;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/sources", get(list).post(upsert))
        .route("/api/sources/import", post(import))
        .route("/api/sources/enabled", post(set_enabled))
        .route("/api/sources/export", get(export_all))
        .route("/api/sources/check-updates", post(check_updates))
        .route("/api/sources/detail", get(get_one))
        .route("/api/sources/validate", post(validate))
        .route("/api/sources/delete", delete(delete_one))
        .route("/api/sources/health", get(health_summary))
        .route("/api/sources/health/check", post(health_check))
        .route("/api/sources/health/reset", post(health_reset))
        .route("/api/sources/prune", post(prune))
        .route("/api/sources/quarantine", post(quarantine_sources))
        // 体检语料：GET/POST/DELETE 同一路径（DELETE 用 ?id=），内置条目由后端保护
        .route(
            "/api/sources/bench/corpus",
            get(bench_corpus)
                .post(bench_corpus_add)
                .delete(bench_corpus_delete),
        )
        .route(
            "/api/sources/bench/corpus/update",
            post(bench_corpus_update),
        )
        .route(
            "/api/sources/bench/corpus/toggle",
            post(bench_corpus_toggle),
        )
        .route("/api/sources/bench/run", post(bench_run))
        .route("/api/sources/bench/run/:id", get(bench_run_status))
        .route("/api/sources/bench/curated", get(bench_curated))
}

#[path = "sources_core.rs"]
mod sources_core;
use sources_core::*;
#[path = "sources_probe.rs"]
mod sources_probe;
use sources_probe::*;
#[path = "sources_export.rs"]
mod sources_export;
#[path = "sources_import.rs"]
mod sources_import;
#[path = "sources_sync.rs"]
mod sources_sync;
use sources_sync::*;
#[path = "sources_bench.rs"]
mod sources_bench;
#[path = "sources_health.rs"]
mod sources_health;

// 其它路由与 source_bench 原先从 `routes::sources::*` 取用的公开项。
pub use sources_bench::{
    bench_corpus, bench_corpus_add, bench_corpus_delete, bench_corpus_toggle, bench_corpus_update,
    bench_curated, bench_run, bench_run_status, BenchCorpusIdQ, BenchCorpusInput,
    BenchCorpusUpdateReq, BenchRunReq, BenchToggleReq,
};
pub use sources_core::{list, load_source, load_sources, sort_sources, UrlQ};
pub use sources_export::{
    delete_one, export_all, get_one, set_enabled, validate, EnabledQ, ExportTierQ, ValidateReq,
};
pub use sources_health::{
    health_check, health_reset, health_summary, prune, quarantine_sources, HealthCheckQ,
    HealthSummaryQ, PruneQ, PruneReq, QuarantineReq, ResetReq,
};
pub use sources_import::{import, upsert};
pub use sources_probe::ProbeQ;
pub use sources_sync::{check_updates, SourceUpdateItem};

#[allow(clippy::field_reassign_with_default)] // 测试里按字段逐个构造对象，便于对照默认值。
#[cfg(test)]
#[path = "sources_tests.rs"]
mod tests;
