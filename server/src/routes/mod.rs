pub mod sources;
pub mod books;
pub mod bookshelf;
pub mod replace;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{routing::{get, post}, Router};
use sqlx::SqlitePool;

use crate::auth;
use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub shared: SqlitePool,
    pub cfg: Config,
    /// 每用户独立 SQLite 连接池缓存（物理分桶）
    pub user_pools: Arc<Mutex<HashMap<i64, SqlitePool>>>,
}

pub fn build_router(shared: SqlitePool, cfg: Config) -> Router {
    let state = AppState {
        shared,
        cfg: cfg.clone(),
        user_pools: Arc::new(Mutex::new(HashMap::new())),
    };
    // 所有路由统一携带 AppState；鉴权中间件内部放行公开路径
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .merge(sources::routes())
        .merge(books::routes())
        .merge(bookshelf::routes())
        .merge(replace::routes());
    app.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth::auth_middleware,
    ))
    .with_state(state)
}

async fn health(_: axum::extract::State<AppState>) -> &'static str {
    "ok"
}
