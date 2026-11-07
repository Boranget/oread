//! 路由级冒烟测试（phase 2.2）：
//! 用 `axum::Router` + `tower::ServiceExt::oneshot` 直接打路由，不监听端口、不碰真实 data。
//!
//! 覆盖范围逐步扩大：健康检查、未登录拦截、注册/登录、普通用户只读、管理员接口。
//! 后续按计划补 sources CRUD、搜索 job、toc/content、progress、settings、export 与 `/k`。

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::response::Response;
use boranlib::config::Config;
use boranlib::{db, routes};
use serde_json::Value;
use sqlx::SqlitePool;
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
    // 冒烟测试固定按开发语义跑，避免本机环境变量把 bootstrap 口令校验切到生产。
    cfg.production = false;
    cfg
}

async fn test_app(tag: &str) -> (axum::Router, SqlitePool, PathBuf) {
    let dir = temp_dir(tag);
    let cfg = test_config(&dir);
    let shared = db::init_shared(&cfg).await.expect("初始化共享库");
    (routes::build_router(shared.clone(), cfg), shared, dir)
}

async fn body_json(resp: Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .expect("读取响应体");
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "响应不是 JSON: {e}; body={}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

fn json_post(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn authed_get(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

async fn register(app: &axum::Router, username: &str) -> String {
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/auth/register",
            &format!(r#"{{"username":"{username}","password":"secret123"}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "注册 {username}");
    body_json(resp).await["token"]
        .as_str()
        .expect("token")
        .to_string()
}

#[tokio::test]
async fn healthz_returns_ok_without_auth() {
    let (app, _pool, dir) = test_app("healthz").await;
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
    let (app, _pool, dir) = test_app("auth").await;
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

#[tokio::test]
async fn register_login_and_read_sources() {
    let (app, _pool, dir) = test_app("login").await;
    let token = register(&app, "alice").await;

    // 注册返回的 token 可直接访问只读接口。
    let resp = app
        .clone()
        .oneshot(authed_get("/api/sources", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await, Value::Array(Vec::new()));

    // 再走一次登录，确认口令哈希与 token 签发链路。
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/auth/login",
            r#"{"username":"alice","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let login_token = body_json(resp).await["token"]
        .as_str()
        .expect("token")
        .to_string();
    let resp = app
        .oneshot(authed_get("/api/sources", &login_token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn regular_user_cannot_write_sources() {
    let (app, _pool, dir) = test_app("forbidden").await;
    let token = register(&app, "bob").await;

    let mut req = json_post("/api/sources", "{}");
    req.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().expect("auth header"),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn admin_can_list_users() {
    let (app, pool, dir) = test_app("admin").await;
    register(&app, "carol").await;
    // 测试里直接提权，避免依赖进程级 ADMIN_USER/ADMIN_PASS 环境变量。
    sqlx::query("UPDATE users SET role = 'admin' WHERE username = 'carol'")
        .execute(&pool)
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/auth/login",
            r#"{"username":"carol","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let token = body_json(resp).await["token"]
        .as_str()
        .expect("token")
        .to_string();

    let resp = app
        .oneshot(authed_get("/api/admin/users", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let users = body_json(resp).await;
    assert!(
        users["users"].as_array().is_some_and(|a| !a.is_empty()),
        "管理员列表应至少包含自己: {users}"
    );

    let _ = std::fs::remove_dir_all(dir);
}
