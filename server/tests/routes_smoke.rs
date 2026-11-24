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
use boranlib::entities::BookChapter;
use boranlib::library_store::{text_blocks, write as store_write, Store};
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
    let library = db::init_library(&cfg).await.expect("初始化馆藏库");
    (
        routes::build_router(shared.clone(), library, cfg),
        shared,
        dir,
    )
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

fn authed_post(uri: &str, body: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn authed_delete(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

/// 注册后直接提权并重新登录，返回管理员 token。
async fn register_admin(app: &axum::Router, pool: &SqlitePool, username: &str) -> String {
    register(app, username).await;
    sqlx::query("UPDATE users SET role = 'admin' WHERE username = ?1")
        .bind(username)
        .execute(pool)
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/auth/login",
            &format!(r#"{{"username":"{username}","password":"secret123"}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员重新登录");
    body_json(resp).await["token"]
        .as_str()
        .expect("admin token")
        .to_string()
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
    let body = body_json(resp).await;
    assert_eq!(body["ok"], true, "healthz: {body}");
    assert!(body["content_cache"].is_object(), "healthz: {body}");
    for key in [
        "hits",
        "misses",
        "hit_rate",
        "saves",
        "evictions",
        "evicted_rows",
    ] {
        assert!(
            body["content_cache"][key].is_number(),
            "缺少 content_cache.{key}: {body}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

// 馆藏监控：/api/healthz 暴露分层占用（不设上限）；/api/book/cache/trim 回收临时缓存。
#[tokio::test]
async fn cache_usage_and_trim_are_exposed() {
    let (app, _pool, dir) = test_app("cache-trim").await;
    let token = register(&app, "trimmer").await;

    let resp = app
        .clone()
        .oneshot(authed_get("/api/healthz", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["cache"]["collection"]["books"], 0, "{body}");
    assert_eq!(body["cache"]["temp"]["books"], 0, "{body}");
    assert!(body["cache"]["books"].is_array(), "缺少按书占用: {body}");

    let resp = app
        .clone()
        .oneshot(authed_post("/api/book/cache/trim", "{}", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["ok"], true, "trim: {body}");
    assert_eq!(body["deleted"], 0, "空缓存无需清理: {body}");
    assert_eq!(
        body["used_bytes_before"], body["used_bytes_after"],
        "未删除时前后占用应一致: {body}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

// 响应编码层只挂在 `/k`：/k/login 的 HTML 走 gzip，API 热路径（/healthz）保持 identity。
#[tokio::test]
async fn kindle_pages_are_gzipped_but_api_stays_identity() {
    let (app, _pool, dir) = test_app("gzip").await;
    let plain = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/k/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        plain.headers().get(header::CONTENT_ENCODING).is_none(),
        "未声明 accept-encoding 时不应压缩"
    );
    let gz = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/k/login")
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(gz.status(), StatusCode::OK);
    assert_eq!(
        gz.headers()
            .get(header::CONTENT_ENCODING)
            .map(|v| v.to_str().unwrap()),
        Some("gzip")
    );
    assert!(
        gz.headers().contains_key(header::VARY),
        "压缩响应应带 Vary，避免缓存串味"
    );
    // API 热路径不压缩：压缩层只挂在 /k，避免 /healthz 与搜索结果多付 gzip CPU。
    let api = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        api.headers().get(header::CONTENT_ENCODING).is_none(),
        "API 热路径不应带 content-encoding"
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
async fn admin_sources_crud_and_export() {
    let (app, pool, dir) = test_app("sources-crud").await;
    let token = register_admin(&app, &pool, "dave").await;
    let url = "https://smoke.example";
    let body = format!(
        r#"{{"book_source_url":"{url}","book_source_name":"Smoke Source","search_url":"{url}/search?q={{{{key}}}}"}}"#
    );

    let resp = app
        .clone()
        .oneshot(authed_post("/api/sources", &body, &token))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "保存书源: {}",
        response_text(resp).await
    );

    let resp = app
        .clone()
        .oneshot(authed_get("/api/sources", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_json(resp).await;
    assert!(
        list.as_array()
            .is_some_and(|a| a.iter().any(|s| s["book_source_url"] == url)),
        "列表应包含新建书源: {list}"
    );

    let resp = app
        .clone()
        .oneshot(authed_get("/api/sources/export", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let exported = body_json(resp).await;
    assert!(
        exported
            .as_array()
            .is_some_and(|a| a.iter().any(|s| s["bookSourceUrl"] == url)),
        "导出应包含新建书源: {exported}"
    );

    let resp = app
        .clone()
        .oneshot(authed_delete(
            "/api/sources/delete?url=https%3A%2F%2Fsmoke.example",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(authed_get("/api/sources", &token))
        .await
        .unwrap();
    let list = body_json(resp).await;
    assert_eq!(list, Value::Array(Vec::new()), "删除后列表应为空");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn settings_validation_and_roundtrip() {
    let (app, _pool, dir) = test_app("settings").await;
    let token = register(&app, "erin").await;

    let resp = app
        .clone()
        .oneshot(authed_get("/api/settings", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let defaults = body_json(resp).await;
    assert_eq!(defaults["font_size"], 18, "默认字号: {defaults}");
    assert_eq!(defaults["theme"], "light", "默认主题: {defaults}");

    let resp = app
        .clone()
        .oneshot(authed_post("/api/settings", r#"{"font_size":99}"#, &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "越界字号应被拒绝");

    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/settings",
            r#"{"font_size":20,"line_height":1.8,"theme":"sepia"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(authed_get("/api/settings", &token))
        .await
        .unwrap();
    let saved = body_json(resp).await;
    assert_eq!(saved["font_size"], 20);
    assert_eq!(saved["theme"], "sepia");
    assert_eq!(saved["line_height"], 1.8);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn bookshelf_progress_roundtrip() {
    let (app, _pool, dir) = test_app("progress").await;
    let token = register(&app, "frank").await;
    let book = "https://book.example/smoke-1";

    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/bookshelf",
            &format!(r#"{{"book_id":"{book}","name":"Smoke Book","author":"Tester"}}"#),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/bookshelf/progress",
            &format!(
                r#"{{"book_id":"{book}","chapter_index":3,"position":0.5,"total_chapters":10}}"#
            ),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "写入进度");

    let resp = app
        .clone()
        .oneshot(authed_get(
            "/api/bookshelf/progress?book_id=https%3A%2F%2Fbook.example%2Fsmoke-1",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let progress = body_json(resp).await;
    assert_eq!(progress["chapter_index"], 3, "进度: {progress}");
    assert_eq!(progress["position"], 0.5, "进度: {progress}");

    let resp = app
        .oneshot(authed_get("/api/bookshelf", &token))
        .await
        .unwrap();
    let shelf = body_json(resp).await;
    assert!(
        shelf
            .as_array()
            .is_some_and(|a| a.iter().any(|b| b["book_id"] == book)),
        "书架应包含新书: {shelf}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn search_job_lifecycle_without_sources() {
    let (app, _pool, dir) = test_app("search-job").await;
    let token = register(&app, "gina").await;

    let resp = app
        .clone()
        .oneshot(authed_get("/api/search/start?key=smoke", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let started = body_json(resp).await;
    let job_id = started["job_id"].as_str().expect("job_id").to_string();
    assert_eq!(started["source_total"], 0, "没有书源: {started}");

    let resp = app
        .clone()
        .oneshot(authed_get(
            &format!("/api/search/result?job_id={job_id}&page=1"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let result = body_json(resp).await;
    assert_eq!(
        result["books"].as_array().map(Vec::len),
        Some(0),
        "空源搜索应返回空结果: {result}"
    );
    assert_eq!(result["status"], "done", "空源任务应立即结束: {result}");

    // 任务列表：内存 + 数据库合并后应能看到刚结束的任务。
    let resp = app
        .clone()
        .oneshot(authed_get("/api/search/jobs", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let jobs = body_json(resp).await;
    assert!(
        jobs["jobs"].as_array().is_some_and(|a| a
            .iter()
            .any(|j| j["job_id"] == job_id.as_str() && j["status"] == "done")),
        "任务列表应包含刚结束的任务: {jobs}"
    );

    // 已完成任务取消是幂等的（不报错、不改变状态）。
    let resp = app
        .clone()
        .oneshot(authed_post(
            &format!("/api/search/jobs/cancel?job_id={job_id}"),
            "",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["already_finished"], true);

    // 移除后：列表与结果接口都不再返回（内存与数据库同时删除）。
    let resp = app
        .clone()
        .oneshot(authed_delete(
            &format!("/api/search/jobs?job_id={job_id}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["removed"], true);

    let resp = app
        .clone()
        .oneshot(authed_get(
            &format!("/api/search/result?job_id={job_id}&page=1"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let resp = app
        .oneshot(authed_get("/api/search/jobs", &token))
        .await
        .unwrap();
    let jobs = body_json(resp).await;
    assert!(
        jobs["jobs"]
            .as_array()
            .is_none_or(|a| a.iter().all(|j| j["job_id"] != job_id.as_str())),
        "移除后列表不应再包含任务: {jobs}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn kindle_redirects_to_login_without_session() {
    let (app, _pool, dir) = test_app("kindle-auth").await;

    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/k").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/k/login")
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/k/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("<form"), "登录页应包含表单: {html}");

    // 手机视口：登录页必须跟着 vw/vh 走（旧实现固定 1072 宽，右侧被裁）
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/k/login?vw=390&vh=844")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("content=\"width=390,"), "{html}");
    assert!(html.contains("height:844px"), "文档高跟随 vh：{html}");
    assert!(html.contains("width=\"318\""), "表单宽度取内容宽：{html}");
    assert!(
        html.contains("font-size:16px;line-height:26px"),
        "正文字号按统一比例缩到 50%：{html}"
    );
    assert!(
        !html.contains("width=\"928\""),
        "手机不得渲染 928 宽表单：{html}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// `/k` 搜索必须走后台任务：首请求 303 到 `job=`，任务页从快照渲染，
/// 完成后不再自动刷新，且翻页/字号链接都保留 job，避免每次翻页重新搜索。
#[tokio::test]
async fn kindle_search_creates_job_and_renders_snapshot() {
    let (app, _pool, dir) = test_app("kindle-search-job").await;
    let username = "ksearch";
    register(&app, username).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/k/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "username={username}&password=secret123"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("登录应写入 k_session")
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("k_session="), "cookie: {cookie}");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/k/search?key=abc")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("搜索应 303 到任务页")
        .to_string();
    assert!(
        location.starts_with("/k/search?job="),
        "首请求应创建任务: {location}"
    );

    // 空库没有可搜索的源：任务立即完成，任务页应直接渲染快照结论。
    let mut last = String::new();
    let mut rendered = false;
    for _ in 0..20 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&location)
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        last = String::from_utf8_lossy(&bytes).to_string();
        if last.contains("当前没有可搜索的源") {
            rendered = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(rendered, "任务页未渲染结果快照: {last}");
    assert!(
        !last.contains("http-equiv=\"refresh\""),
        "任务完成后不应继续自动刷新"
    );
    assert!(last.contains("job="), "任务页链接必须保留 job 参数: {last}");

    // 不存在的任务必须走共享库查询并给出明确文案（而不是数据库错误页）。
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/k/search?job=missing-job")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&bytes);
    assert!(
        html.contains("搜索任务不存在或已过期"),
        "不存在的任务应给出过期文案: {html}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 目录/正文冒烟用的固定抓取实现：不联网，按 URL 返回 fixture HTML。
struct MockFetcher {
    /// 正文 URL（/c1）被真实抓取的次数，用于验证 single-flight 合并。
    content_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl boranlib::web_book::Fetcher for MockFetcher {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        _method: &'a str,
        _headers: &'a std::collections::HashMap<String, String>,
        _body: Option<&'a str>,
        _source_key: &'a str,
        _timeout: Option<std::time::Duration>,
        _limit: boranlib::req::SourceLimit,
    ) -> boranlib::web_book::FetchFuture<'a> {
        Box::pin(async move {
            if url == "https://mock.example/c1" {
                self.content_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // 留出并发窗口，确保并发请求真的重叠，single-flight 才可被验证。
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            }
            let html = match url {
                "https://mock.example/book/1" => {
                    "<html><body><ul>\
                     <li class=\"c\"><a href=\"/c1\">第一章</a></li>\
                     <li class=\"c\"><a href=\"/c2\">第二章</a></li>\
                     </ul></body></html>"
                }
                "https://mock.example/c1" => {
                    "<html><body><div id=\"content\"><p>正文一</p><p>正文二</p></div></body></html>"
                }
                other => anyhow::bail!("unexpected url: {other}"),
            };
            Ok(html.to_string())
        })
    }
}

#[tokio::test]
async fn toc_and_content_routes_with_injected_fetcher() {
    let dir = temp_dir("toc-content");
    let cfg = test_config(&dir);
    let shared = db::init_shared(&cfg).await.expect("初始化共享库");
    let library = db::init_library(&cfg).await.expect("初始化馆藏库");
    let app = routes::build_router_with_fetcher(
        shared.clone(),
        library,
        cfg,
        std::sync::Arc::new(MockFetcher {
            content_calls: Default::default(),
        }),
    );
    let token = register_admin(&app, &shared, "erin").await;

    let source = r#"{
        "book_source_url":"https://mock.example",
        "book_source_name":"Mock",
        "rule_toc":{
            "chapter_list":"class.c",
            "chapter_name":"tag.a@text",
            "chapter_url":"tag.a@href"
        },
        "rule_content":{"content":"id.content@html"}
    }"#;
    let resp = app
        .clone()
        .oneshot(authed_post("/api/sources", source, &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "导入书源");

    let toc_uri = "/api/book/toc?source=https%3A%2F%2Fmock.example\
        &book_url=https%3A%2F%2Fmock.example%2Fbook%2F1&use_replace=0";
    let resp = app
        .clone()
        .oneshot(authed_get(toc_uri, &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "目录路由");
    let toc = body_json(resp).await;
    let chapters = toc.as_array().expect("目录为数组");
    assert_eq!(chapters.len(), 2, "fixture 两章: {toc}");
    assert_eq!(chapters[0]["title"], "第一章");
    assert_eq!(chapters[0]["url"], "https://mock.example/c1");
    assert_eq!(chapters[1]["url"], "https://mock.example/c2");

    let content_uri = "/api/book/content?source=https%3A%2F%2Fmock.example\
        &chapter_url=https%3A%2F%2Fmock.example%2Fc1\
        &book_url=https%3A%2F%2Fmock.example%2Fbook%2F1&use_replace=0";
    let resp = app
        .clone()
        .oneshot(authed_get(content_uri, &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "正文路由");
    let content = body_json(resp).await;
    let text = content["content"].as_str().unwrap_or_default();
    assert!(
        text.contains("正文一"),
        "正文应包含 fixture 内容: {content}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 8 个并发同章请求：只有第一个真正抓取，其余等待后命中缓存。
#[tokio::test]
async fn concurrent_content_requests_share_one_fetch() {
    let dir = temp_dir("content-single-flight");
    let cfg = test_config(&dir);
    let shared = db::init_shared(&cfg).await.expect("初始化共享库");
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let library = db::init_library(&cfg).await.expect("初始化馆藏库");
    let app = routes::build_router_with_fetcher(
        shared.clone(),
        library.clone(),
        cfg,
        std::sync::Arc::new(MockFetcher {
            content_calls: calls.clone(),
        }),
    );
    let token = register_admin(&app, &shared, "frank").await;

    let source = r#"{
        "book_source_url":"https://mock.example",
        "book_source_name":"Mock",
        "rule_content":{"content":"id.content@html"}
    }"#;
    let resp = app
        .clone()
        .oneshot(authed_post("/api/sources", source, &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "导入书源");

    // 目录是正文唯一副本：先有书目录，首次抓取的正文才落得下来，
    // 后续并发请求才能在闸门后重查目录命中。
    sqlx::query(
        "INSERT INTO library_books (book_url, name, origin, tier, last_accessed_at) \
         VALUES ('https://mock.example/book/1', '并发书', 'https://mock.example', 'temp', 1)",
    )
    .execute(&library)
    .await
    .unwrap();
    let chapters = vec![BookChapter {
        url: "https://mock.example/c1".into(),
        title: Some("第一章".into()),
        book_url: Some("https://mock.example/book/1".into()),
        index: 0,
        ..Default::default()
    }];
    let store = Store::new(&dir);
    store_write::sync_toc(&library, &store, "https://mock.example/book/1", &chapters)
        .await
        .unwrap();

    let uri = "/api/book/content?source=https%3A%2F%2Fmock.example\
        &chapter_url=https%3A%2F%2Fmock.example%2Fc1\
        &book_url=https%3A%2F%2Fmock.example%2Fbook%2F1&use_replace=0"
        .to_string();
    let mut handles = Vec::new();
    for _ in 0..8 {
        let app = app.clone();
        let token = token.clone();
        let uri = uri.clone();
        handles.push(tokio::spawn(async move {
            let resp = app.oneshot(authed_get(&uri, &token)).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "并发正文请求");
            let body = body_json(resp).await;
            let text = body["content"].as_str().unwrap_or_default();
            assert!(text.contains("正文一"), "正文内容: {body}");
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "并发同章只应抓取一次"
    );

    let _ = std::fs::remove_dir_all(dir);
}

async fn response_text(resp: Response) -> String {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap_or_default();
    format!("{status}: {}", String::from_utf8_lossy(&bytes))
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

/// 馆藏浏览/管理：普通账号只读、管理员可入馆/移出/删除；列表按书名 keyset 分页。
#[tokio::test]
async fn library_browse_and_admin_actions() {
    let dir = temp_dir("library");
    let cfg = test_config(&dir);
    let shared = db::init_shared(&cfg).await.expect("初始化共享库");
    let library = db::init_library(&cfg).await.expect("初始化馆藏库");
    let app = routes::build_router(shared.clone(), library.clone(), cfg);
    let user = register(&app, "libuser").await;
    let admin = register_admin(&app, &shared, "libadmin").await;

    // 三本书：两本馆藏（入馆时间不同）+ 一本临时（没有入馆时间）。
    for (url, tier, collected) in [
        ("https://lib.test/a", "collection", Some(300i64)),
        ("https://lib.test/b", "collection", Some(200)),
        ("https://lib.test/c", "temp", None),
    ] {
        sqlx::query(
            "INSERT INTO library_books (book_url, name, tier, collected_at, last_accessed_at) \
             VALUES (?1, ?1, ?2, ?3, 0)",
        )
        .bind(url)
        .bind(tier)
        .bind(collected)
        .execute(&library)
        .await
        .unwrap();
    }

    // 普通账号可看占用，但不能管理。
    let resp = app
        .clone()
        .oneshot(authed_get("/api/library/stats", &user))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "普通账号读占用");
    let stats = body_json(resp).await;
    assert_eq!(stats["collection"]["books"], 2, "{stats}");
    assert_eq!(stats["temp"]["books"], 1, "{stats}");
    assert!(stats["disk"]["floor_bytes"].as_i64().unwrap_or(0) > 0);

    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/library/collect",
            r#"{"book_url":"https://lib.test/a"}"#,
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "普通账号不能入馆");

    let resp = app
        .clone()
        .oneshot(authed_get("/api/library/jobs", &user))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "普通账号不能看采购单");

    // 书名分页：第一页取 2 本，第二页继续；本用例没有目录，走 library.db 回退。
    let resp = app
        .clone()
        .oneshot(authed_get(
            "/api/library/books?tier=all&sort=title&limit=2",
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let page1 = body_json(resp).await;
    assert_eq!(page1["sort"], "title", "{page1}");
    let urls1: Vec<&str> = page1["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["book_url"].as_str())
        .collect();
    assert_eq!(urls1, vec!["https://lib.test/a", "https://lib.test/b"]);
    let cursor = page1["next_cursor"]
        .as_str()
        .expect("还有下一页")
        .to_string();

    let resp = app
        .clone()
        .oneshot(authed_get(
            &format!("/api/library/books?tier=all&sort=title&limit=2&cursor={cursor}"),
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let page2 = body_json(resp).await;
    let urls2: Vec<&str> = page2["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["book_url"].as_str())
        .collect();
    assert_eq!(urls2, vec!["https://lib.test/c"], "临时书不能漏行");
    assert!(page2["next_cursor"].is_null(), "{page2}");

    // 管理员入馆：无书源时只标记馆藏，不排队下载。
    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/library/collect",
            r#"{"book_url":"https://lib.test/new"}"#,
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let collected = body_json(resp).await;
    assert_eq!(collected["newly_collected"], true, "{collected}");
    assert_eq!(collected["queued"], false, "没有书源不排队: {collected}");

    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/library/release",
            r#"{"book_url":"https://lib.test/new"}"#,
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["released"], true);

    // 仍在馆藏的书不能直接删除。
    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/library/delete",
            r#"{"book_url":"https://lib.test/a"}"#,
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "馆藏需先移出");

    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/library/delete",
            r#"{"book_url":"https://lib.test/new"}"#,
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["deleted_books"], 1);

    let resp = app
        .clone()
        .oneshot(authed_get("/api/library/jobs", &admin))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let _ = std::fs::remove_dir_all(dir);
}

/// 漫画导出：info 报页数，cbz 返回 ZIP；纯文本书用 409 明确拒绝。
#[tokio::test]
async fn comic_cbz_export_info_and_download() {
    let dir = temp_dir("export-cbz");
    let cfg = test_config(&dir);
    let shared = db::init_shared(&cfg).await.expect("初始化共享库");
    let library = db::init_library(&cfg).await.expect("初始化馆藏库");
    let app = routes::build_router(shared.clone(), library.clone(), cfg);
    let user = register(&app, "cbzuser").await;

    sqlx::query(
        "INSERT INTO library_books (book_url, name, author, tier, book_source_type, \
         collected_at, last_accessed_at) \
         VALUES ('https://c.test/book', '漫画书', '画师', 'collection', 2, 1, 1)",
    )
    .execute(&library)
    .await
    .unwrap();
    let chapters = vec![
        BookChapter {
            url: "https://c.test/ch1".into(),
            title: Some("第一话".into()),
            book_url: Some("https://c.test/book".into()),
            index: 0,
            ..Default::default()
        },
        BookChapter {
            url: "https://c.test/ch2".into(),
            title: Some("第二话".into()),
            book_url: Some("https://c.test/book".into()),
            index: 1,
            ..Default::default()
        },
    ];
    let store = Store::new(&dir);
    store_write::sync_toc(&library, &store, "https://c.test/book", &chapters)
        .await
        .unwrap();
    for (url, pages) in [
        ("https://c.test/ch1", vec![b"one".to_vec(), b"two".to_vec()]),
        ("https://c.test/ch2", vec![b"three".to_vec()]),
    ] {
        let pages: Vec<store_write::ComicPageInput> = pages
            .into_iter()
            .map(|data| store_write::ComicPageInput {
                data,
                mime: "image/jpeg".into(),
                width: None,
                height: None,
                source_url: None,
            })
            .collect();
        store_write::save_comic_pages(&store, "https://c.test/book", url, &pages)
            .await
            .unwrap();
    }

    let resp = app
        .clone()
        .oneshot(authed_get(
            "/api/book/export/cbz/info?book_url=https%3A%2F%2Fc.test%2Fbook",
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let info = body_json(resp).await;
    assert_eq!(info["total_pages"], 3, "{info}");
    assert_eq!(info["available_pages"], 3);
    assert_eq!(info["image_chapters"], 2);
    assert_eq!(info["complete"], true);

    let resp = app
        .clone()
        .oneshot(authed_get(
            "/api/book/export/cbz?book_url=https%3A%2F%2Fc.test%2Fbook",
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()["content-type"],
        "application/vnd.comicbook+zip"
    );
    assert_eq!(resp.headers()["x-written-pages"], "3");
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.starts_with(b"PK\x03\x04"), "CBZ 必须是 ZIP");

    // 纯文本书（没有图片章节）明确 409。
    sqlx::query(
        "INSERT INTO library_books (book_url, name, tier, last_accessed_at) \
         VALUES ('https://c.test/text', '文字书', 'collection', 1)",
    )
    .execute(&library)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_chapters (book_url, url, title, is_volume, \"index\") \
         VALUES ('https://c.test/text', 'https://c.test/text/1', '第一章', 0, 0)",
    )
    .execute(&library)
    .await
    .unwrap();
    let resp = app
        .clone()
        .oneshot(authed_get(
            "/api/book/export/cbz?book_url=https%3A%2F%2Fc.test%2Ftext",
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let _ = std::fs::remove_dir_all(dir);
}

/// 目录即书：漫画目录命中后，source/toc/chapter/图片代理/meta 全走本地，不需要书源。
#[tokio::test]
async fn comic_reads_from_local_dir_without_source() {
    use boranlib::library_store::{comic, Store, Tier};

    let dir = temp_dir("comic-dir-read");
    let cfg = test_config(&dir);
    let shared = db::init_shared(&cfg).await.expect("初始化共享库");
    let library = db::init_library(&cfg).await.expect("初始化馆藏库");
    let app = routes::build_router(shared.clone(), library.clone(), cfg);
    let user = register(&app, "comicdir").await;

    let book_url = "https://local.test/comic";
    let store = Store::new(&dir);
    store.create_layout().unwrap();
    let book_dir = store.dir_for(Tier::Temp, book_url);
    std::fs::create_dir_all(&book_dir).unwrap();
    std::fs::write(book_dir.join("0001-001.jpg"), b"one").unwrap();
    let mut meta = comic::ComicMeta::new(book_url, "本地漫画");
    meta.author = Some("作者".into());
    meta.chapters.push(comic::ComicChapter {
        id: "ch-0001".into(),
        title: "第一话".into(),
        url: None,
        is_volume: false,
        pages: vec![comic::ComicPage {
            href: "0001-001.jpg".into(),
            mime: "image/jpeg".into(),
            width: Some(100),
            height: Some(200),
            bytes: Some(3),
            source_url: None,
        }],
    });
    comic::write_meta(&book_dir, &meta).await.unwrap();
    let encoded = "https%3A%2F%2Flocal.test%2Fcomic";

    let resp = app
        .clone()
        .oneshot(authed_get(
            &format!("/api/comic/source?book_url={encoded}"),
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["comic"], true);
    assert_eq!(v["local"], true);

    let resp = app
        .clone()
        .oneshot(authed_get(
            &format!("/api/book/toc?source=&book_url={encoded}"),
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let toc = body_json(resp).await;
    assert_eq!(toc.as_array().unwrap().len(), 1);
    assert_eq!(toc[0]["title"], "第一话");

    let resp = app
        .clone()
        .oneshot(authed_get(
            &format!("/api/comic/chapter?book_url={encoded}&index=1"),
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let chapter = body_json(resp).await;
    assert_eq!(chapter["local"], true);
    assert_eq!(chapter["chapter_title"], "第一话");
    assert_eq!(chapter["count"], 1);
    let local_url = chapter["images"][0]["url"].as_str().unwrap().to_string();
    assert!(local_url.starts_with("oread-img://"), "{local_url}");

    // 图片代理直读目录文件：不需要 source，也不需要回源。
    let resp = app
        .clone()
        .oneshot(authed_get(
            &format!("/api/book/image?url={local_url}"),
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/jpeg");
    let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    assert_eq!(&bytes[..], b"one");

    // 批量 meta 同样支持本地 URL。
    let body = format!(r#"{{"urls":["{local_url}"]}}"#);
    let resp = app
        .clone()
        .oneshot(authed_post("/api/book/images/meta", &body, &user))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let meta = body_json(resp).await;
    assert_eq!(meta["count"], 1);
    assert_eq!(meta["items"][0]["ok"], true);
    assert_eq!(meta["items"][0]["bytes"], 3);

    let _ = std::fs::remove_dir_all(dir);
}

/// `/k` 正文目录优先：书目录存在时不需要书源、也不查缓存，直接渲染章节文件。
#[tokio::test]
async fn kindle_reads_text_from_local_dir_without_source() {
    use boranlib::library_store::{ChapterItem, OpfMeta, Store, Tier};

    let dir = temp_dir("kindle-dir-read");
    let cfg = test_config(&dir);
    let shared = db::init_shared(&cfg).await.expect("初始化共享库");
    let library = db::init_library(&cfg).await.expect("初始化馆藏库");
    let app = routes::build_router(shared.clone(), library.clone(), cfg);
    let username = "kindledir";
    register(&app, username).await;

    let book_url = "https://local.test/text";
    let store = Store::new(&dir);
    store.create_layout().unwrap();
    let mut meta = OpfMeta::new(book_url, "本地文字书");
    let item = ChapterItem {
        id: "ch-0001".into(),
        href: "chapters/ch-0001-a.xhtml".into(),
        title: "本地章节标题".into(),
        url: Some("https://local.test/text/1".into()),
        is_volume: false,
    };
    meta.chapters.push(item.clone());
    let book_dir = store.ensure_skeleton(Tier::Temp, &meta).await.unwrap();
    store
        .write_chapter(
            &book_dir,
            "zh",
            &item,
            &text_blocks(&["目录里的正文段落".into(), "第二段".into()]),
        )
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/k/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "username={username}&password=secret123"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("登录应写入 k_session")
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let encoded = "https%3A%2F%2Flocal.test%2Ftext";
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/k/toc?book={encoded}&source="))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("本地章节标题"), "目录应来自书目录: {html}");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/k/read?book={encoded}&source=&ch=0"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(
        html.contains("目录里的正文段落"),
        "正文应来自书目录: {html}"
    );
    assert!(!html.contains("正文抓取失败"), "不应回源抓取: {html}");

    let _ = std::fs::remove_dir_all(dir);
}

/// calibre 式编辑：文件树、源码保存、替换与 Check book 走通，修订号递增。
#[tokio::test]
async fn admin_book_file_editor_routes() {
    use boranlib::library_store::{ChapterItem, OpfMeta, Store, Tier};

    let dir = temp_dir("book-files");
    let cfg = test_config(&dir);
    let shared = db::init_shared(&cfg).await.expect("初始化共享库");
    let library = db::init_library(&cfg).await.expect("初始化馆藏库");
    let app = routes::build_router(shared.clone(), library.clone(), cfg);
    let admin = register_admin(&app, &shared, "fileadmin").await;

    let book_url = "https://local.test/files";
    let encoded = "https%3A%2F%2Flocal.test%2Ffiles";
    let store = Store::new(&dir);
    store.create_layout().unwrap();
    let mut meta = OpfMeta::new(book_url, "文件编辑书");
    let item = ChapterItem {
        id: "ch-0001".into(),
        href: "chapters/ch-0001-a.xhtml".into(),
        title: "第一章".into(),
        url: Some("https://local.test/files/1".into()),
        is_volume: false,
    };
    meta.chapters.push(item.clone());
    let book_dir = store.ensure_skeleton(Tier::Temp, &meta).await.unwrap();
    store
        .write_chapter(
            &book_dir,
            "zh",
            &item,
            &text_blocks(&["原始正文段落".into()]),
        )
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(authed_get(
            &format!("/api/library/book/edit/files?book_url={encoded}"),
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tree = body_json(resp).await;
    let revision = tree["revision"].as_u64().expect("revision");
    assert!(tree["files"]
        .as_array()
        .is_some_and(|a| a.iter().any(|f| f["path"] == "OEBPS/content.opf")));

    let chapter = "OEBPS/chapters/ch-0001-a.xhtml";
    let resp = app
        .clone()
        .oneshot(authed_get(
            &format!("/api/library/book/edit/file?book_url={encoded}&path={chapter}"),
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let read = body_json(resp).await;
    assert!(read["text"].as_str().unwrap_or("").contains("原始正文段落"));

    let edited = read["text"]
        .as_str()
        .unwrap()
        .replace("原始正文段落", "改过的正文");
    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/library/book/edit/file",
            &serde_json::json!({
                "book_url": book_url,
                "path": chapter,
                "revision": revision,
                "content": edited,
            })
            .to_string(),
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let saved = body_json(resp).await;
    let revision = saved["revision"].as_u64().expect("revision");
    assert_eq!(revision, tree["revision"].as_u64().unwrap() + 1);

    let resp = app
        .clone()
        .oneshot(authed_get(
            &format!("/api/library/book/edit/check?book_url={encoded}"),
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let report = body_json(resp).await;
    assert_eq!(report["errors"], 0, "{report}");

    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/library/book/edit/replace",
            &serde_json::json!({
                "book_url": book_url,
                "revision": revision,
                "query": "改过的正文",
                "replacement": "替换后的正文",
                "dry_run": true,
            })
            .to_string(),
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let dry = body_json(resp).await;
    assert_eq!(dry["applied"], false);
    assert!(dry["total"].as_u64().unwrap_or(0) >= 1, "{dry}");

    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/library/book/edit/replace",
            &serde_json::json!({
                "book_url": book_url,
                "revision": revision,
                "query": "改过的正文",
                "replacement": "替换后的正文",
            })
            .to_string(),
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let applied = body_json(resp).await;
    assert_eq!(applied["applied"], true, "{applied}");

    // 越界路径必须按参数错误拒绝，不能落到 500。
    let resp = app
        .clone()
        .oneshot(authed_get(
            &format!("/api/library/book/edit/file?book_url={encoded}&path=..%2Fsecret"),
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let _ = std::fs::remove_dir_all(dir);
}

/// EPUB 导出：info 报正文章节，epub 返回 ZIP；漫画书用 409 明确拒绝。
#[tokio::test]
async fn epub_export_info_and_download() {
    let dir = temp_dir("export-epub");
    let cfg = test_config(&dir);
    let shared = db::init_shared(&cfg).await.expect("初始化共享库");
    let library = db::init_library(&cfg).await.expect("初始化馆藏库");
    let app = routes::build_router(shared.clone(), library.clone(), cfg);
    let user = register(&app, "epubuser").await;

    sqlx::query(
        "INSERT INTO library_books (book_url, name, author, tier, collected_at, last_accessed_at) \
         VALUES ('https://e.test/book', '文字书', '作者：作者甲', 'collection', 1, 1)",
    )
    .execute(&library)
    .await
    .unwrap();
    let chapters = vec![
        BookChapter {
            url: "https://e.test/c1".into(),
            title: Some("第一章".into()),
            book_url: Some("https://e.test/book".into()),
            index: 0,
            ..Default::default()
        },
        BookChapter {
            url: "https://e.test/c2".into(),
            title: Some("第二章".into()),
            book_url: Some("https://e.test/book".into()),
            index: 1,
            ..Default::default()
        },
    ];
    let store = Store::new(&dir);
    store_write::sync_toc(&library, &store, "https://e.test/book", &chapters)
        .await
        .unwrap();
    store_write::save_text_chapter(
        &store,
        "https://e.test/book",
        "https://e.test/c1",
        &text_blocks(&["第一章正文".into()]),
    )
    .await
    .unwrap();

    let resp = app
        .clone()
        .oneshot(authed_get(
            "/api/book/export/epub/info?book_url=https%3A%2F%2Fe.test%2Fbook",
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let info = body_json(resp).await;
    assert_eq!(info["text_chapters"], 2, "{info}");
    assert_eq!(info["cached_chapters"], 1);
    assert_eq!(info["missing_chapters"], 1);
    assert_eq!(info["complete"], false);

    let resp = app
        .clone()
        .oneshot(authed_get(
            "/api/book/export/epub?book_url=https%3A%2F%2Fe.test%2Fbook",
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "application/epub+zip");
    assert_eq!(resp.headers()["x-written-chapters"], "1");
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.starts_with(b"PK\x03\x04"), "EPUB 必须是 ZIP");

    // 只有漫画章节的书没有正文目录，明确 409。
    sqlx::query(
        "INSERT INTO library_books (book_url, name, tier, book_source_type, last_accessed_at) \
         VALUES ('https://e.test/comic', '漫画书', 'collection', 2, 1)",
    )
    .execute(&library)
    .await
    .unwrap();
    let resp = app
        .clone()
        .oneshot(authed_get(
            "/api/book/export/epub?book_url=https%3A%2F%2Fe.test%2Fcomic",
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let _ = std::fs::remove_dir_all(dir);
}

/// 元数据读写：普通账号可读、只有管理员能改；空补丁与非法值返回 400。
#[tokio::test]
async fn library_metadata_read_and_admin_write() {
    let dir = temp_dir("library-meta");
    let cfg = test_config(&dir);
    let shared = db::init_shared(&cfg).await.expect("初始化共享库");
    let library = db::init_library(&cfg).await.expect("初始化馆藏库");
    let app = routes::build_router(shared.clone(), library.clone(), cfg);
    let user = register(&app, "metauser").await;
    let admin = register_admin(&app, &shared, "metaadmin").await;

    sqlx::query(
        "INSERT INTO library_books (book_url, name, tier, last_accessed_at) \
         VALUES ('https://m.test/a', '银河系漫游指南', 'collection', 0)",
    )
    .execute(&library)
    .await
    .unwrap();

    // 普通账号可读；未设置的可空字段返回 null、数组返回空。
    let resp = app
        .clone()
        .oneshot(authed_get(
            "/api/library/book/meta?book_url=https%3A%2F%2Fm.test%2Fa",
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let meta = body_json(resp).await;
    assert_eq!(meta["meta"]["name"], "银河系漫游指南");
    assert!(meta["meta"]["language"].is_null());
    assert!(meta["meta"]["tags"].as_array().unwrap().is_empty());

    // 普通账号不能写馆藏元数据。
    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/library/book/meta",
            r#"{"book_url":"https://m.test/a","language":"zh"}"#,
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 管理员写入：标识 scheme 归一为小写，标签去空白。
    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/library/book/meta",
            r#"{"book_url":"https://m.test/a","language":"zh","series_name":"银河系漫游指南","series_index":1.0,"identifiers":[{"scheme":"ISBN","value":"9787532754687"}],"tags":[" 科幻 ","经典"]}"#,
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let meta = body_json(resp).await;
    assert_eq!(meta["meta"]["language"], "zh");
    assert_eq!(meta["meta"]["series_index"], 1.0);
    assert_eq!(meta["meta"]["identifiers"][0]["scheme"], "isbn");
    assert_eq!(
        meta["meta"]["tags"],
        serde_json::json!(["科幻", "经典"]),
        "{meta}"
    );

    // 空补丁：400，避免静默成功。
    let resp = app
        .clone()
        .oneshot(authed_post(
            "/api/library/book/meta",
            r#"{"book_url":"https://m.test/a"}"#,
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 不在馆藏里的书：404。
    let resp = app
        .clone()
        .oneshot(authed_get(
            "/api/library/book/meta?book_url=https%3A%2F%2Fm.test%2Fnone",
            &user,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(dir);
}

/// OPDS 只接受 HTTP Basic；导航/书单/下载都走同一账号，且只列 `tier=collection`。
#[tokio::test]
async fn opds_requires_basic_and_exposes_collection() {
    use base64::Engine as _;

    fn basic_get(uri: &str, user: &str, pass: &str) -> Request<Body> {
        let raw = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        Request::builder()
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Basic {raw}"))
            .body(Body::empty())
            .unwrap()
    }

    let dir = temp_dir("opds");
    let cfg = test_config(&dir);
    let shared = db::init_shared(&cfg).await.expect("初始化共享库");
    let library = db::init_library(&cfg).await.expect("初始化馆藏库");
    let app = routes::build_router(shared.clone(), library.clone(), cfg);
    let _ = register(&app, "opdsuser").await;

    // 未带凭据：401 且必须给 WWW-Authenticate，否则阅读器不会弹账号框。
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/opds").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers()["www-authenticate"]
        .to_str()
        .unwrap()
        .contains("Basic"));

    // 错口令：同样 401，不区分用户不存在/口令错误。
    let resp = app
        .clone()
        .oneshot(basic_get("/opds", "opdsuser", "wrong-pass"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 正确口令：导航 feed。
    let resp = app
        .clone()
        .oneshot(basic_get("/opds", "opdsuser", "secret123"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap()
        .contains("kind=navigation"));
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let nav = String::from_utf8(body.to_vec()).unwrap();
    assert!(nav.contains("/opds/books?kind=all"), "{nav}");
    assert!(nav.contains("全部馆藏"), "{nav}");

    // 两本馆藏 + 一本临时缓存；临时书不得出现在 OPDS 目录。
    sqlx::query(
        "INSERT INTO library_books (book_url, name, author, origin, tier, collected_at, \
         last_accessed_at, language, publisher, pubdate, series_name, series_index, \
         cached_chapters) \
         VALUES ('https://o.test/text', '文字 & 书', '作者：张三', 'https://src.test', \
         'collection', 10, 100, 'zh', '出版社', '2024-01-02', '系列', 2.0, 1)",
    )
    .execute(&library)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_books (book_url, name, tier, collected_at, last_accessed_at, \
         book_source_type, cached_chapters) \
         VALUES ('https://o.test/comic', '漫画书', 'collection', 20, 200, 2, 1)",
    )
    .execute(&library)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_books (book_url, name, tier, last_accessed_at) \
         VALUES ('https://o.test/temp', '临时书', 'temp', 300)",
    )
    .execute(&library)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_chapters (book_url, url, title, is_volume, \"index\") \
         VALUES ('https://o.test/text', 'https://o.test/text/1', '第一章', 0, 0)",
    )
    .execute(&library)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_chapters (book_url, url, title, is_volume, \"index\") \
         VALUES ('https://o.test/comic', 'https://o.test/comic/1', '第一话', 0, 0)",
    )
    .execute(&library)
    .await
    .unwrap();
    sqlx::query("INSERT INTO library_tags (book_url, tag) VALUES ('https://o.test/text', '科幻')")
        .execute(&library)
        .await
        .unwrap();

    // 下载复用导出管线：文字书必须有完整书目录才能导出 EPUB。
    let store = Store::new(&dir);
    let chapters = vec![BookChapter {
        url: "https://o.test/text/1".into(),
        title: Some("第一章".into()),
        book_url: Some("https://o.test/text".into()),
        index: 0,
        ..Default::default()
    }];
    store_write::sync_toc(&library, &store, "https://o.test/text", &chapters)
        .await
        .unwrap();
    store_write::save_text_chapter(
        &store,
        "https://o.test/text",
        "https://o.test/text/1",
        &text_blocks(&["正文".into()]),
    )
    .await
    .unwrap();

    // 第一页 limit=1：最近阅读的漫画在前，且给出 next 游标。
    let resp = app
        .clone()
        .oneshot(basic_get(
            "/opds/books?kind=all&limit=1",
            "opdsuser",
            "secret123",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap()
        .contains("kind=acquisition"));
    let first = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(first.contains("漫画书"), "{first}");
    assert!(!first.contains("临时书"), "临时缓存不得进 OPDS: {first}");
    assert!(first.contains("rel=\"next\""), "{first}");
    assert!(first.contains("/opds/download/cbz?book_url="), "{first}");
    assert!(!first.contains("/opds/download/epub"), "{first}");

    // 文字书：IP 元数据、标签、EPUB acquisition 都要出现。
    let resp = app
        .clone()
        .oneshot(basic_get("/opds/books?kind=text", "opdsuser", "secret123"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let text = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(text.contains("<title>文字 &amp; 书</title>"), "{text}");
    assert!(
        text.contains("<author><name>张三</name></author>"),
        "{text}"
    );
    assert!(text.contains("<dc:language>zh</dc:language>"), "{text}");
    assert!(
        text.contains("<dc:publisher>出版社</dc:publisher>"),
        "{text}"
    );
    assert!(text.contains("<dc:issued>2024-01-02</dc:issued>"), "{text}");
    assert!(
        text.contains("<category term=\"series:系列\" label=\"系列 #2\"/>"),
        "{text}"
    );
    assert!(text.contains("<category term=\"科幻\""), "{text}");
    assert!(
        text.contains("/opds/download/epub?book_url=https%3A%2F%2Fo.test%2Ftext"),
        "{text}"
    );

    // 下载复用导出管线：Basic 通过后返回真正的 EPUB。
    let resp = app
        .clone()
        .oneshot(basic_get(
            "/opds/download/epub?book_url=https%3A%2F%2Fo.test%2Ftext",
            "opdsuser",
            "secret123",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "application/epub+zip");
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.starts_with(b"PK\x03\x04"), "EPUB 必须是 ZIP");

    let _ = std::fs::remove_dir_all(dir);
}
