//! sources 路由单测（move-only 从 sources.rs 拆出）。

use super::*;

fn src(url: &str, name: &str, t: i64) -> BookSource {
    let mut s = BookSource::default();
    s.book_source_url = url.into();
    s.book_source_name = name.into();
    s.last_update_time = t;
    s
}

// 只读比对：更新 / 新增 / 本地更新 / 相同 四类都能区分
#[test]
fn diff_reports_update_new_and_same() {
    let local = vec![
        src("https://a.com", "A", 100),
        src("https://b.com", "B", 200),
        src("https://c.com", "C", 300),
    ];
    let incoming = vec![
        src("https://a.com", "A", 101),
        src("https://b.com", "B", 199),
        src("https://c.com", "C", 300),
        src("https://d.com", "D", 50),
    ];
    let v = diff_sources(&local, &incoming);
    assert_eq!(v["updated"].as_array().unwrap().len(), 1);
    assert_eq!(v["updated"][0]["book_source_url"], "https://a.com");
    assert_eq!(v["local_newer"].as_array().unwrap().len(), 1);
    assert_eq!(v["new"].as_array().unwrap().len(), 1);
    assert_eq!(v["new"][0]["local_update_time"], 0);
    assert_eq!(v["same"], 1);
    assert_eq!(v["has_updates"], true);
}

// 没有任何变化时 has_updates=false
#[test]
fn diff_without_updates() {
    let local = vec![src("https://a.com", "A", 100)];
    let incoming = vec![src("https://a.com", "A", 100)];
    let v = diff_sources(&local, &incoming);
    assert_eq!(v["has_updates"], false);
    assert_eq!(v["same"], 1);
}

// 导入策略：0/2 正常启用；1 及其他类型强制禁用并标记不支持
#[test]
fn import_policy_disables_unsupported_types() {
    for (ty, supported) in [(0, true), (2, true), (1, false), (3, false), (99, false)] {
        let mut s = BookSource::default();
        s.book_source_type = ty;
        s.enabled = true;
        let kept = prepare_import(&mut s);
        assert_eq!(kept, supported, "type {ty} 的支持性判断错误");
        assert_eq!(s.unsupported_type, !supported);
        assert_eq!(s.enabled, supported, "type {ty} 的默认启用状态错误");
    }
}

/// 回归：现有 422 源里 9 条“漫画源存成 type=0”必须全部被判定为漫画（仅建议，不改库）。
#[test]
fn suggests_type2_for_mislabelled_comic_sources() {
    // (名称, URL, 真实 rule_content.content 片段)
    let cases: [(&str, &str, &str); 9] = [
        (
            "拷贝漫画（优++）",
            "https://www.mangacopy.com/",
            "$..url\n<js>\nresult.split(\"\\n\").map(x=>'<img src=\"'+x+'\">').join(\"\\n\")",
        ),
        (
            "爱看漫画（优+）",
            "https://m.kanman.com#♤Haxc",
            "$.data.current_chapter.chapter_img_list@js:\nheader={\"Referer\":baseUrl}",
        ),
        (
            "爱看漫画（优）",
            "https://m.kanman.com/",
            "$.data.current_chapter.chapter_img_list@js:\nheader={\"Referer\":baseUrl}",
        ),
        (
            "爱看漫画（优）",
            "https://m.kanman.com#",
            "$.data.current_chapter.chapter_img_list@js:\nheader={\"Referer\":baseUrl}",
        ),
        (
            "酷看漫画（优）",
            "https://www.kukk.net#♤Haxc",
            ".lazy@html##src=.*\"@js:result.replace(/data-original/g,\"src",
        ),
        (
            "奇漫屋子（优）",
            "https://m.qimanwu.app",
            "img@data-src||img@src\n@js:\nresult.split(\"\\n\").map(url => `<img src=\"${url}\">`)",
        ),
        (
            "星际漫画（优）",
            "http://www.xmanhua.com#♤Haxc",
            "@js:\ntry{header={\"Referer\":baseUrl};headers={\"headers\":JS",
        ),
        (
            "爱漫客栈",
            "https://www.mkzhan.com",
            "$..image@js:result.split(\"\\n\").map(x=>'<img src=\"'+x+'\">')",
        ),
        (
            "爱漫客栈",
            "https://www.mkzhan.com#yc1101",
            "$..image@js:result.split(\"\\n\").map(x=>'<img src=\"'+x+'\">')",
        ),
    ];
    for (name, url, content) in cases {
        let s = BookSource {
            book_source_name: name.to_string(),
            book_source_url: url.to_string(),
            book_source_type: 0,
            rule_content: Some(crate::entities::ContentRule {
                content: Some(content.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let sug = suggest_comic_type(&s).unwrap_or_else(|| panic!("{name} {url} 未被判为漫画"));
        assert_eq!(sug["suggested_type"], 2, "{name} 建议类型错误");
        assert_eq!(sug["current_type"], 0, "{name} 当前类型不应被改写");
        assert_eq!(s.book_source_type, 0, "建议不得修改源本身");
    }
}

#[test]
fn no_comic_suggestion_for_novel_or_already_comic() {
    // 小说源：名字不含漫画特征
    let novel = BookSource {
        book_source_name: "企鹅浏览".to_string(),
        book_source_url: "https://qbnovel.qq.com".to_string(),
        book_source_type: 0,
        rule_content: Some(crate::entities::ContentRule {
            content: Some("class.content@html".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(suggest_comic_type(&novel).is_none());

    // 名字像漫画但内容规则没有图片特征 → 不盲判
    let text = BookSource {
        book_source_name: "漫画小说站".to_string(),
        book_source_url: "https://example.com".to_string(),
        book_source_type: 0,
        rule_content: Some(crate::entities::ContentRule {
            content: Some("class.content@text".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(suggest_comic_type(&text).is_none());

    // 已经是 type=2 → 无需建议
    let comic = BookSource {
        book_source_name: "酸奶漫画".to_string(),
        book_source_url: "https://m.1kkk.com/".to_string(),
        book_source_type: 2,
        rule_content: Some(crate::entities::ContentRule {
            content: Some("@js:img".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(suggest_comic_type(&comic).is_none());
}

/// 回归（真库 2026-11 实测漂移）：`book_sources.enabled=0` 但 JSON 里 `enabled` 仍为 true
/// （迁移 0005 与维护脚本只 `UPDATE ... SET enabled=0`，不重写 JSON）。
/// 停用源不得进入 `source_total`（准入集合），也不得出现在搜索结果 `origin`（errors.origin 均无）。
#[tokio::test]
async fn disabled_column_wins_over_drifted_json() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
            "CREATE TABLE book_sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT, \
             rules TEXT NOT NULL, content TEXT, enabled INTEGER NOT NULL DEFAULT 1, \
             book_source_type INTEGER NOT NULL DEFAULT 0, unsupported_type INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

    // 两条规则除 URL 外完全相同（JSON 里都写 enabled=true），只有 enabled 列不同 → 模拟漂移。
    for (id, enabled) in [
        ("https://on.example.com", 1),
        ("https://off.example.com", 0),
    ] {
        let rules = serde_json::json!({
            "book_source_url": id,
            "book_source_name": id,
            "enabled": true,
            "search_url": "http://127.0.0.1:1/search?key=%s",
            "rule_search": {"book_list": ""}
        })
        .to_string();
        sqlx::query(
            "INSERT INTO book_sources (id, name, url, rules, content, enabled) \
                 VALUES (?1, ?2, ?1, ?3, ?3, ?4)",
        )
        .bind(id)
        .bind(id)
        .bind(&rules)
        .bind(enabled)
        .execute(&pool)
        .await
        .unwrap();
    }

    let loaded = load_sources(&pool).await.unwrap();
    assert_eq!(loaded.len(), 2);
    let off = loaded
        .iter()
        .find(|s| s.book_source_url == "https://off.example.com")
        .unwrap();
    assert!(!off.enabled, "列 enabled=0 必须覆盖 JSON 的 enabled=true");
    assert!(
        !load_source(&pool, "https://off.example.com")
            .await
            .unwrap()
            .enabled
    );

    // 与 search_start 相同的准入条件：source_total 只数可搜索源。
    let eligible: Vec<BookSource> = loaded
        .iter()
        .filter(|x| crate::entities::is_searchable_source(x))
        .cloned()
        .collect();
    assert_eq!(eligible.len(), 1, "source_total 不应包含停用源");
    assert_eq!(eligible[0].book_source_url, "https://on.example.com");

    // origin：把「加载后的全量」交给聚合搜索，停用源必须一次请求都不发。
    let out = crate::web_book::search_all_with_errors_budgeted(
        &loaded,
        "test",
        1,
        std::time::Duration::from_millis(800),
    )
    .await;
    assert!(
        out.errors
            .iter()
            .all(|e| e.source != "https://off.example.com"),
        "停用源出现在搜索结果 origin 中"
    );
}

/// 导入路径测试用库：真实 shared 迁移 + 空库，不依赖任何“现有源规模/已知源名”。
async fn import_test_state() -> AppState {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations/shared")
        .run(&pool)
        .await
        .unwrap();
    AppState {
        shared: pool,
        cfg: crate::config::Config::from_env(),
        user_pools: std::sync::Arc::new(std::sync::Mutex::new(Default::default())),
        fetcher: std::sync::Arc::new(crate::web_book::ReqFetcher),
    }
}

// ---------------- 权限收口回归 ----------------

use axum::response::IntoResponse;
use sqlx::sqlite::SqlitePoolOptions;

fn member() -> AuthUser {
    AuthUser {
        id: 2,
        username: "alice".into(),
        role: "user".into(),
    }
}

fn admin() -> AuthUser {
    AuthUser {
        id: 1,
        username: "admin".into(),
        role: "admin".into(),
    }
}

async fn shared_state() -> AppState {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE book_sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT, \
             rules TEXT NOT NULL, content TEXT, enabled INTEGER NOT NULL DEFAULT 1, \
             book_source_type INTEGER NOT NULL DEFAULT 0, \
             unsupported_type INTEGER NOT NULL DEFAULT 0, updated_by TEXT, updated_at TEXT)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
            "CREATE TABLE source_library_state (source_url TEXT PRIMARY KEY, decision TEXT, \
             review_state TEXT, reason TEXT, incoming_hash TEXT, batch_id TEXT, updated_at INTEGER)",
        )
        .execute(&pool)
        .await
        .unwrap();
    let rules = json!({
        "book_source_url": "https://src.example.com",
        "book_source_name": "示例源",
        "header": "Cookie: SID=secret"
    })
    .to_string();
    sqlx::query(
        "INSERT INTO book_sources (id, name, url, rules, content, enabled) \
             VALUES (?1, ?2, ?1, ?3, ?3, 1)",
    )
    .bind("https://src.example.com")
    .bind("示例源")
    .bind(&rules)
    .execute(&pool)
    .await
    .unwrap();
    AppState {
        shared: pool,
        cfg: crate::config::Config::from_env(),
        user_pools: Default::default(),
        fetcher: std::sync::Arc::new(crate::web_book::ReqFetcher),
    }
}

#[tokio::test]
async fn import_marks_login_source_skipped_and_load_derives_searchability() {
    let s = import_test_state().await;
    let admin = AuthUser {
        id: 1,
        username: "admin".into(),
        role: "admin".into(),
    };
    // 空库起点 + 一次导入 1 个需登录源 + 1 个正常源；关闭探测避免外连。
    let body = json!([
        {
            "bookSourceUrl": "https://login.example",
            "bookSourceName": "登录源",
            "loginUrl": "https://login.example/login",
            "searchUrl": "https://login.example/search?q={key}",
            "ruleSearch": {"bookList": ".b"}
        },
        {
            "bookSourceUrl": "https://open.example",
            "bookSourceName": "开放源",
            "searchUrl": "https://open.example/search?q={key}",
            "ruleSearch": {"bookList": ".b"}
        }
    ]);
    let q = ProbeQ {
        probe: Some("0".into()),
        ..Default::default()
    };
    let Json(out) = import(State(s.clone()), admin, Query(q), Json(body))
        .await
        .unwrap();

    assert_eq!(out["skipped_login_required"], 1);
    let results = out["probe_results"].as_array().unwrap();
    let login = results
        .iter()
        .find(|r| r["book_source_url"] == "https://login.example")
        .unwrap();
    assert_eq!(login["category"], "skipped_login_required");
    assert_eq!(login["login_required"], true);
    assert_eq!(login["searchable"], false);
    let open = results
        .iter()
        .find(|r| r["book_source_url"] == "https://open.example")
        .unwrap();
    assert_eq!(open["login_required"], false);
    assert_eq!(open["searchable"], true);

    // 导入台账必须留下明确原因（不是只靠内存标记）。
    let reason: Option<String> =
        sqlx::query_scalar("SELECT reason FROM import_items WHERE source_url = ?1")
            .bind("https://login.example")
            .fetch_one(&s.shared)
            .await
            .unwrap();
    assert!(
        reason
            .unwrap_or_default()
            .starts_with("skipped_login_required"),
        "台账原因缺失或口径不一致"
    );

    // 加载路径同口径派生，且 enabled 列未被登录判定改写。
    let loaded = load_sources(&s.shared).await.unwrap();
    let login_loaded = loaded
        .iter()
        .find(|x| x.book_source_url == "https://login.example")
        .unwrap();
    assert!(login_loaded.enabled, "登录判定不得改写 enabled");
    assert!(crate::entities::requires_login(login_loaded));
    assert!(!crate::entities::is_searchable_source(login_loaded));
    let open_loaded = loaded
        .iter()
        .find(|x| x.book_source_url == "https://open.example")
        .unwrap();
    assert!(crate::entities::is_searchable_source(open_loaded));

    // 编辑规则去掉 loginUrl：不落库修复，下一次加载自动恢复可搜索。
    let mut edited = login_loaded.clone();
    edited.login_url = None;
    sqlx::query("UPDATE book_sources SET rules = ?1, content = ?1 WHERE id = ?2")
        .bind(serde_json::to_string(&edited).unwrap())
        .bind("https://login.example")
        .execute(&s.shared)
        .await
        .unwrap();
    let reloaded = load_sources(&s.shared).await.unwrap();
    let restored = reloaded
        .iter()
        .find(|x| x.book_source_url == "https://login.example")
        .unwrap();
    assert!(!crate::entities::requires_login(restored));
    assert!(
        crate::entities::is_searchable_source(restored),
        "去掉登录字段后应在加载时自动回到可搜索池"
    );
}

#[tokio::test]
async fn export_is_admin_only() {
    let st = State(shared_state().await);
    let denied = export_all(st.clone(), member(), Query(ExportTierQ::default())).await;
    match denied {
        Ok(_) => panic!("普通用户不应能导出书源"),
        Err(e) => assert_eq!(
            e.into_response().status(),
            axum::http::StatusCode::FORBIDDEN
        ),
    }
    let ok = export_all(st, admin(), Query(ExportTierQ::default()))
        .await
        .expect("管理员应可导出");
    assert_eq!(ok.into_response().status(), axum::http::StatusCode::OK);
}

#[tokio::test]
async fn detail_is_admin_only() {
    let st = State(shared_state().await);
    let q = || {
        Query(UrlQ {
            url: "https://src.example.com".into(),
        })
    };
    let denied = get_one(st.clone(), member(), q()).await;
    match denied {
        Ok(_) => panic!("普通用户不应能读取书源详情"),
        Err(e) => assert_eq!(
            e.into_response().status(),
            axum::http::StatusCode::FORBIDDEN
        ),
    }
    let ok = get_one(st, admin(), q()).await.expect("管理员应可读详情");
    assert_eq!(ok.into_response().status(), axum::http::StatusCode::OK);
}

#[tokio::test]
async fn health_check_is_admin_only() {
    let st = State(shared_state().await);
    let denied = health_check(st.clone(), member(), Query(HealthCheckQ::default())).await;
    match denied {
        Ok(_) => panic!("普通用户不应能触发全量体检"),
        Err(e) => assert_eq!(
            e.into_response().status(),
            axum::http::StatusCode::FORBIDDEN
        ),
    }
    let ok = health_check(st, admin(), Query(HealthCheckQ::default()))
        .await
        .expect("管理员应可触发体检");
    assert_eq!(ok.into_response().status(), axum::http::StatusCode::OK);
}

#[tokio::test]
async fn validate_check_url_is_admin_only() {
    let st = State(shared_state().await);
    let body = || {
        Some(Json(json!({
            "bookSourceUrl": "https://src.example.com",
            "bookSourceName": "示例源",
            "check_url": true
        })))
    };
    // 普通用户带 check_url=true → 403（且不会真的发起外连）
    let denied = validate(st.clone(), member(), body()).await;
    match denied {
        Ok(_) => panic!("普通用户不应能触发 check_url 外连"),
        Err(e) => assert_eq!(
            e.into_response().status(),
            axum::http::StatusCode::FORBIDDEN
        ),
    }
    // 普通用户不带 check_url → 仍可做本地规则校验
    let local_only = validate(
        st.clone(),
        member(),
        Some(Json(json!({
            "bookSourceUrl": "https://src.example.com",
            "bookSourceName": "示例源"
        }))),
    )
    .await
    .expect("本地规则校验不应要求管理员");
    assert_eq!(
        local_only.into_response().status(),
        axum::http::StatusCode::OK
    );
}
