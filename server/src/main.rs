mod auth;
mod config;
mod db;
mod entities;
mod error;
mod models;
mod routes;
mod rule_engine;
mod req;
mod serde_util;
mod web_book;

use axum::Router;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing_subscriber::EnvFilter;

use crate::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = Config::from_env();
    // 初始化共享桶（书源/RSS/账户），缺失则自动建库并执行迁移
    let shared = db::init_shared(&cfg).await?;
    // 首次启动自动创建管理员账号（凭据来自 ADMIN_USER/ADMIN_PASS，缺省 admin/__INITIAL_ADMIN_PASS__）
    auth::bootstrap_admin(&shared).await?;

    let api = routes::build_router(shared, cfg.clone());

    // 前端静态资源（Svelte 构建产物 web/dist）；目录不存在时该路由返回 404，不影响 API
    let app = api
        .fallback_service(ServeDir::new("web/dist"))
        .layer(CorsLayer::permissive()); // TODO: 生产环境收紧为具体前端来源

    let addr = format!("{}:{}", cfg.host, cfg.port);
    tracing::info!("legado-server listening on {addr}");
    let listener = TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
