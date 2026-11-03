pub mod sources;
pub mod books;
pub mod bookshelf;
pub mod export;
pub mod bookmarks;
pub mod replace;
pub mod stats;
pub mod health;
pub mod kindle;
pub mod image;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{routing::{get, post}, Router};
use sqlx::SqlitePool;

use crate::auth;
use crate::config::Config;

/// 用户库连接池缓存（LRU）：限制同时打开的 SQLite 文件数，
/// 超出 `db_max_open_users` 时关闭最久未使用的池，避免多用户把内存摊爆。
#[derive(Default)]
pub struct UserPoolCache {
    pub entries: HashMap<i64, (SqlitePool, u64)>,
    pub tick: u64,
}

impl UserPoolCache {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub shared: SqlitePool,
    pub cfg: Config,
    /// 每用户独立 SQLite 连接池缓存（物理分桶 + LRU 上限）
    pub user_pools: Arc<Mutex<UserPoolCache>>,
}

pub fn build_router(shared: SqlitePool, cfg: Config) -> Router {
    let state = AppState {
        shared,
        cfg: cfg.clone(),
        user_pools: Arc::new(Mutex::new(UserPoolCache::default())),
    };
    // 所有路由统一携带 AppState；鉴权中间件内部放行公开路径
    let app = Router::new()
        .route("/api/health", get(health::simple))
        .route("/healthz", get(health::healthz))
        .route("/api/healthz", get(health::healthz_user))
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/password", post(auth::change_password))
        .route("/api/admin/users", get(auth::admin_list_users))
        .route("/api/admin/users/status", post(auth::admin_set_user_status))
        .merge(sources::routes())
        .merge(books::routes())
        .merge(export::routes())
        .merge(bookshelf::routes())
        .merge(bookmarks::routes())
        .merge(replace::routes())
        .merge(stats::routes())
        .merge(kindle::routes())
        .merge(image::routes());
    app.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth::auth_middleware,
    ))
    .with_state(state)
}
