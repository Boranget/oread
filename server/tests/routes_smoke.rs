//! 路由级冒烟测试（phase 2.2 骨架）：
//! 用 `axum::Router` + `tower::ServiceExt::oneshot` 直接打路由，不监听端口、不碰真实 data。
//!
//! 目前只覆盖最小闭环（健康检查 + 鉴权拦截）；后续按计划补登录、sources CRUD、
//! 搜索 job、toc/content、progress、settings、export、bookmarks 与 `/k`。

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boranlib::config::Config;
use boranlib::{db, routes};
use tower::ServiceExt;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "boranlib-smoke-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_config(dir: &Path) -> Config {
    let mut cfg = Config::from_env();
    cfg.data_dir = dir.display().to_string();
    cfg.web_dir = dir.join("web").display().to_string();
    cfg.db_backup_keep = 1;
    cfg.migration_repair = true;
    cfg
}

async fn test_app(tag: &str) -> (axum::Router, PathBuf) {
    let dir = temp_dir(tag);
    let cfg = test_config(&dir);
    let shared = db::init_shared(&cfg).await.expect("初始化共享库");
    (routes::build_router(shared, cfg), dir)
}

#[tokio::test]
async fn healthz_returns_ok_without_auth() {
    let (app, dir) = test_app("healthz").await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("ok") || text.contains("status"),
        "healthz: {text}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn protected_api_rejects_missing_token() {
    let (app, dir) = test_app("auth").await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/sources")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let _ = std::fs::remove_dir_all(dir);
}
