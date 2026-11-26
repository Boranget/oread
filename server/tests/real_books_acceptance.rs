//! 真实 EPUB 验收（手动运行，不参与日常 `cargo test`）。
//!
//! 用 `oneshot` 直接驱动路由，不监听端口；数据目录是临时目录，跑完即删。
//! 覆盖：批量导入 → 目录/详情 → 分面/首页 → 抽样章节正文与语义块。
//!
//! ```powershell
//! $env:BORANLIB_BOOKS_DIR='D:\boraget\books'
//! cargo test --test real_books_acceptance -- --ignored --nocapture
//! ```
//!
//! 每个采样章节都会打印块类型统计；表格与 MathML 目前只作为纯文本进入块，
//! 这里统计命中章节数，供后续 `Block::Table` / 公式设计取样。

use std::path::{Path, PathBuf};
use std::time::Instant;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::response::Response;
use boranlib::config::Config;
use boranlib::library_store::{read as store_read, Store};
use boranlib::{db, routes};
use serde_json::Value;
use sqlx::SqlitePool;
use tower::ServiceExt;

const MAX_JSON: usize = 64 * 1024 * 1024;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "boranlib-accept-{tag}-{}-{}",
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
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), MAX_JSON)
        .await
        .expect("读取响应体");
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "响应不是 JSON（{status}）: {e}; body={}",
            String::from_utf8_lossy(&bytes[..bytes.len().min(2000)])
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

fn authed_json_post(uri: &str, body: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn epub_upload(file_name: &str, bytes: Vec<u8>, token: &str) -> Request<Body> {
    const BOUNDARY: &str = "----oread-acceptance-boundary";
    let mut body = Vec::with_capacity(bytes.len() + 512);
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n\
             Content-Type: application/epub+zip\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    Request::builder()
        .method("POST")
        .uri("/api/library/import/epub")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body))
        .unwrap()
}

fn query(pairs: &[(&str, &str)]) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        ser.append_pair(k, v);
    }
    ser.finish()
}

async fn register_admin(app: &axum::Router, pool: &SqlitePool) -> String {
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/auth/register",
            r#"{"username":"accept","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "注册管理员");
    sqlx::query("UPDATE users SET role = 'admin' WHERE username = 'accept'")
        .execute(pool)
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/auth/login",
            r#"{"username":"accept","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员登录");
    body_json(resp).await["token"]
        .as_str()
        .expect("admin token")
        .to_string()
}

/// 均匀抽样：总数不超过 `max` 时全取。
fn sample_indices(total: usize, max: usize) -> Vec<usize> {
    if total <= max {
        return (0..total).collect();
    }
    (0..max)
        .map(|i| i * (total - 1) / (max - 1))
        .collect::<Vec<_>>()
        .into_iter()
        .collect()
}

#[derive(Default)]
struct Totals {
    books: usize,
    chapters: usize,
    sampled: usize,
    content_failures: usize,
    empty_blocks: usize,
    table_chapters: usize,
    math_chapters: usize,
    image_blocks: usize,
    images_ok: usize,
    image_failures: usize,
    word_bytes: usize,
}

#[tokio::test]
#[ignore = "需要 BORANLIB_BOOKS_DIR 指向真实 EPUB 目录"]
async fn real_books_import_and_read() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let Ok(books_dir) = std::env::var("BORANLIB_BOOKS_DIR") else {
        eprintln!("跳过：未设置 BORANLIB_BOOKS_DIR");
        return;
    };
    let books_dir = PathBuf::from(books_dir);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&books_dir)
        .expect("读取 BORANLIB_BOOKS_DIR")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .map(|e| e.eq_ignore_ascii_case("epub"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "{} 下没有 EPUB", books_dir.display());

    let (app, pool, dir) = test_app("real-books").await;
    let store = Store::new(&dir);
    let token = register_admin(&app, &pool).await;
    let mut totals = Totals::default();

    println!("\n=== 导入 {} 本 EPUB ===", files.len());
    let mut imported: Vec<(String, String, usize)> = Vec::new(); // (book_url, name, chapters)
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(path).expect("读取 EPUB");
        let started = Instant::now();
        let resp = app
            .clone()
            .oneshot(epub_upload(&name, bytes, &token))
            .await
            .unwrap();
        let status = resp.status();
        let body = body_json(resp).await;
        if status != StatusCode::OK || body["ok"] != true {
            println!("  [失败] {name} → {status}: {body}");
            totals.content_failures += 1;
            continue;
        }
        let book = &body["book"];
        let chapter_count = book["chapter_count"].as_u64().unwrap_or(0) as usize;
        let image_count = book["image_count"].as_u64().unwrap_or(0) as usize;
        let warnings = book["warnings"].as_array().map(Vec::len).unwrap_or(0);
        let duplicates = body["duplicates"].as_array().map(Vec::len).unwrap_or(0);
        println!(
            "  [inspect] {name}\n            标题={} 作者={} 章节={chapter_count} 图片={image_count} \
             封面={} 警告={warnings} 查重={duplicates} 耗时={:?}",
            book["title"].as_str().unwrap_or("?"),
            book["author"].as_str().unwrap_or("-"),
            book["has_cover"].as_bool().unwrap_or(false),
            started.elapsed(),
        );
        for w in book["warnings"].as_array().into_iter().flatten() {
            println!("            警告: {w}");
        }

        let commit_body = serde_json::json!({
            "token": body["token"],
            "mode": "new",
        })
        .to_string();
        let resp = app
            .clone()
            .oneshot(authed_json_post(
                "/api/library/import/commit",
                &commit_body,
                &token,
            ))
            .await
            .unwrap();
        let status = resp.status();
        let body = body_json(resp).await;
        assert_eq!(status, StatusCode::OK, "commit {name}: {body}");
        assert_eq!(body["created"], true, "commit {name}: {body}");
        let book_url = body["book_url"].as_str().expect("book_url").to_string();
        if let Some(check) = body.get("check").filter(|c| !c.is_null()) {
            let issues = check["issues"].as_array().map(Vec::len).unwrap_or(0);
            println!(
                "            检查: revision={} files={} issues={issues}",
                check["revision"], check["checked_files"]
            );
            for issue in check["issues"].as_array().into_iter().flatten() {
                println!(
                    "              - [{}] {} {}: {}",
                    issue["severity"].as_str().unwrap_or("?"),
                    issue["code"].as_str().unwrap_or("?"),
                    issue["path"].as_str().unwrap_or(""),
                    issue["message"].as_str().unwrap_or(""),
                );
            }
        }
        imported.push((book_url, name, chapter_count));
        totals.books += 1;
    }

    // 目录：默认分页只取 24，这里显式拉满，确认全部入馆且字段可用。
    let resp = app
        .clone()
        .oneshot(authed_get(
            "/api/library/catalog?limit=200&sort=title",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let catalog = body_json(resp).await;
    let total = catalog["total"].as_u64().unwrap_or(0);
    println!(
        "\n=== 馆藏目录 ===\ntotal={total} pages={} limit={}",
        catalog["pages"], catalog["limit"]
    );
    assert_eq!(total as usize, totals.books, "目录总数与导入数不一致");

    // 分面与首页
    for uri in ["/api/library/facets", "/api/library/home"] {
        let resp = app.clone().oneshot(authed_get(uri, &token)).await.unwrap();
        let status = resp.status();
        let body = body_json(resp).await;
        println!(
            "{uri} → {status} ok={} keys={:?}",
            body["ok"],
            body.as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        );
        assert_eq!(status, StatusCode::OK);
    }

    // 阅读状态覆盖层：设置一本想读，再按状态过滤。
    if let Some((book_url, _, _)) = imported.first() {
        let resp = app
            .clone()
            .oneshot(authed_json_post(
                "/api/bookshelf/status",
                &serde_json::json!({"book_id": book_url, "status": "want"}).to_string(),
                &token,
            ))
            .await
            .unwrap();
        println!("设置想读 → {}", resp.status());
        let resp = app
            .clone()
            .oneshot(authed_get(
                "/api/library/catalog?status=want&limit=50",
                &token,
            ))
            .await
            .unwrap();
        let body = body_json(resp).await;
        println!("status=want → total={}", body["total"]);
        assert!(body["total"].as_u64().unwrap_or(0) >= 1);
    }

    println!("\n=== 逐本详情 + 抽样正文 ===");
    for (book_url, file_name, expect_chapters) in &imported {
        let resp = app
            .clone()
            .oneshot(authed_get(
                &format!("/api/library/book?{}", query(&[("book_url", book_url)])),
                &token,
            ))
            .await
            .unwrap();
        let status = resp.status();
        let detail = body_json(resp).await;
        assert_eq!(status, StatusCode::OK, "详情 {file_name}: {detail}");
        let book = &detail["book"];
        let toc = book["toc"].as_array().cloned().unwrap_or_default();
        let chapter_rows = book["total_chapter_num"].as_u64().unwrap_or(0) as usize;
        println!(
            "\n--- {file_name}\n    book_url={book_url}\n    详情 toc={} total_chapter_num={} 期望章节={expect_chapters}",
            toc.len(),
            chapter_rows,
        );
        if toc.len() != *expect_chapters {
            println!(
                "    [注意] 详情 toc 与导入摘要章节数不一致（{} vs {expect_chapters}）",
                toc.len()
            );
        }

        // `/api/book/toc` 走书目录（spine/边车），确认与详情一致。
        let resp = app
            .clone()
            .oneshot(authed_get(
                &format!(
                    "/api/book/toc?{}",
                    query(&[("source", ""), ("book_url", book_url)])
                ),
                &token,
            ))
            .await
            .unwrap();
        let status = resp.status();
        let toc_api = body_json(resp).await;
        let toc_api = toc_api.as_array().cloned().unwrap_or_default();
        println!("    /api/book/toc → {status} {} 条", toc_api.len());
        assert_eq!(status, StatusCode::OK, "toc {file_name}");
        assert_eq!(
            toc.len(),
            toc_api.len(),
            "详情目录与 /api/book/toc 章节数不一致: {file_name}"
        );

        let read_urls: Vec<String> = toc_api
            .iter()
            .filter_map(|c| c["url"].as_str())
            .filter(|u| !u.starts_with("volume:"))
            .map(str::to_string)
            .collect();
        totals.chapters += read_urls.len();

        // 表格 / MathML 只在原始章节文件里还认得出来：归一化成块以后表格会变成
        // 逐个单元格的段落。这里扫源文件，给后续 `Block::Table` / 公式设计取样。
        let mut raw_tables = 0usize;
        let mut raw_math = 0usize;
        let mut raw_svg = 0usize;
        let mut table_chapters = 0usize;
        let mut math_chapters = 0usize;
        let mut first_table: Option<(String, String)> = None;
        if let Ok(Some(opened)) = store.open_book(book_url).await {
            for item in &opened.meta.chapters {
                if item.is_volume {
                    continue;
                }
                let Ok(Some(raw)) = store_read::read_chapter_file(&opened.dir, &item.href).await
                else {
                    continue;
                };
                let t = raw.matches("<table").count();
                let m = raw.matches("<math").count();
                raw_tables += t;
                raw_math += m;
                raw_svg += raw.matches("<svg").count();
                if t > 0 {
                    table_chapters += 1;
                    if first_table.is_none() {
                        let url = item.url.clone().unwrap_or_else(|| {
                            boranlib::ebook_source::local_chapter_url(book_url, &item.id)
                        });
                        first_table = Some((url, item.title.clone()));
                    }
                }
                if m > 0 {
                    math_chapters += 1;
                }
            }
        }
        totals.table_chapters += table_chapters;
        totals.math_chapters += math_chapters;
        println!(
            "    源文件：table={raw_tables}（{table_chapters} 章） math={raw_math}（{math_chapters} 章） svg={raw_svg}"
        );

        // 表格章节取样：直接看归一化后的块长什么样，确认当前“拆成单元格段落”的行为。
        if let Some((url, title)) = first_table {
            let uri = format!(
                "/api/book/content?{}",
                query(&[
                    ("source", ""),
                    ("chapter_url", &url),
                    ("book_url", book_url),
                ])
            );
            let resp = app.clone().oneshot(authed_get(&uri, &token)).await.unwrap();
            let status = resp.status();
            let body = body_json(resp).await;
            if status == StatusCode::OK {
                let blocks = body["blocks"].as_array().cloned().unwrap_or_default();
                println!(
                    "    首个表格章节《{title}》{url} → {} 块，前 10 块：",
                    blocks.len()
                );
                for b in blocks.iter().take(10) {
                    let text = b["text"]
                        .as_str()
                        .or_else(|| b["items"].as_array().map(|_| "[list]"))
                        .or_else(|| b["src"].as_str())
                        .unwrap_or("");
                    let text: String = text.chars().take(70).collect();
                    println!("      {}: {text}", b["type"].as_str().unwrap_or("?"));
                }
            } else {
                println!("    [表格章节读取失败] {status}: {body}");
            }
        }

        let picked = sample_indices(read_urls.len(), 25);
        let mut book_images = 0usize;
        for idx in picked {
            let url = &read_urls[idx];
            let uri = format!(
                "/api/book/content?{}",
                query(&[("source", ""), ("chapter_url", url), ("book_url", book_url),])
            );
            let resp = app.clone().oneshot(authed_get(&uri, &token)).await.unwrap();
            let status = resp.status();
            let body = body_json(resp).await;
            if status != StatusCode::OK {
                totals.content_failures += 1;
                println!("    [正文失败] {url} → {status}: {body}");
                continue;
            }
            totals.sampled += 1;
            let content = body["content"].as_str().unwrap_or("");
            totals.word_bytes += content.len();
            let blocks = body["blocks"].as_array().cloned().unwrap_or_default();
            if blocks.is_empty() {
                totals.empty_blocks += 1;
            }
            let mut images = 0usize;
            let mut image_srcs: Vec<String> = Vec::new();
            let mut types = std::collections::BTreeMap::new();
            for b in &blocks {
                let t = b["type"].as_str().unwrap_or("?");
                *types.entry(t.to_string()).or_insert(0usize) += 1;
                if t == "image" {
                    images += 1;
                    if image_srcs.len() < 3 {
                        if let Some(src) = b["src"].as_str() {
                            image_srcs.push(src.to_string());
                        }
                    }
                }
            }
            // 图片块存在不等于能读：真正走一次图片代理，确认本地路径与格式都能出图。
            for src in image_srcs {
                let uri = format!(
                    "/api/book/image?{}",
                    query(&[("url", &src), ("book_url", book_url)])
                );
                let resp = app.clone().oneshot(authed_get(&uri, &token)).await.unwrap();
                let status = resp.status();
                let ctype = resp
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let bytes = axum::body::to_bytes(resp.into_body(), MAX_JSON)
                    .await
                    .unwrap();
                if status == StatusCode::OK && ctype.starts_with("image/") && !bytes.is_empty() {
                    totals.images_ok += 1;
                } else {
                    totals.image_failures += 1;
                    println!(
                        "    [图片失败] {src} → {status} {ctype} {} bytes",
                        bytes.len()
                    );
                }
            }
            book_images += images;
            totals.image_blocks += images;
            if idx < 3 {
                println!(
                    "    [{idx}] {} bytes={} blocks={} types={:?}",
                    url,
                    content.len(),
                    blocks.len(),
                    types,
                );
            }
        }
        println!("    抽样正文：图片块={book_images}");
    }

    println!(
        "\n=== 汇总 ===\n书={} 章节总数={} 抽样={} 正文失败={} 空块章节={} \
         图片块={} 图片可读={} 图片失败={} 含表格章节={} 含 MathML 章节={} 正文字节={}",
        totals.books,
        totals.chapters,
        totals.sampled,
        totals.content_failures,
        totals.empty_blocks,
        totals.image_blocks,
        totals.images_ok,
        totals.image_failures,
        totals.table_chapters,
        totals.math_chapters,
        totals.word_bytes,
    );

    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(totals.content_failures, 0, "存在导入/正文失败，见上方日志");
    assert_eq!(totals.image_failures, 0, "存在图片读取失败，见上方日志");
}
