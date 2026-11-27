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
//! 每个采样章节都会打印块类型统计；表格 / MathML / SVG 章节会额外取正文，
//! 断言真表格、独立公式、纯 SVG 正文与 SVG 封面都能恢复成对应块。

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

/// 单本书的专项探针：真表格、独立公式、SVG 正文与封面的恢复情况。
#[derive(Default)]
struct BookProbe {
    table_blocks: usize,
    math_blocks: usize,
    svg_checked: usize,
    svg_empty: usize,
    svg_with_text: usize,
    svg_with_images: usize,
}

async fn fetch_blocks(
    app: &axum::Router,
    token: &str,
    book_url: &str,
    url: &str,
) -> Option<Vec<Value>> {
    let uri = format!(
        "/api/book/content?{}",
        query(&[("source", ""), ("chapter_url", url), ("book_url", book_url),])
    );
    let resp = app.clone().oneshot(authed_get(&uri, token)).await.ok()?;
    if resp.status() != StatusCode::OK {
        return None;
    }
    let body = body_json(resp).await;
    Some(body["blocks"].as_array().cloned().unwrap_or_default())
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
    let mut probes: Vec<(String, BookProbe)> = Vec::new();
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

        // 表格 / MathML / SVG 只在原始章节文件里还认得出来；这里扫源文件并收集
        // 章节地址，随后逐章取正文，验证归一化后的块类型。
        let mut raw_tables = 0usize;
        let mut raw_math = 0usize;
        let mut raw_svg = 0usize;
        let mut table_chapters = 0usize;
        let mut math_chapters = 0usize;
        let mut first_table: Option<(String, String)> = None;
        let mut svg_urls: Vec<(String, String)> = Vec::new();
        let mut table_urls: Vec<(String, String)> = Vec::new();
        let mut math_urls: Vec<(String, String)> = Vec::new();
        if let Ok(Some(opened)) = store.open_book(book_url).await {
            for item in &opened.meta.chapters {
                if item.is_volume {
                    continue;
                }
                let Ok(Some(raw)) = store_read::read_chapter_file(&opened.dir, &item.href).await
                else {
                    continue;
                };
                let url = item.url.clone().unwrap_or_else(|| {
                    boranlib::ebook_source::local_chapter_url(book_url, &item.id)
                });
                let t = raw.matches("<table").count();
                let m = raw.matches("<math").count();
                let s = raw.matches("<svg").count();
                raw_tables += t;
                raw_math += m;
                raw_svg += s;
                if t > 0 {
                    table_chapters += 1;
                    table_urls.push((url.clone(), item.title.clone()));
                    if first_table.is_none() {
                        first_table = Some((url.clone(), item.title.clone()));
                    }
                }
                if m > 0 {
                    math_chapters += 1;
                    math_urls.push((url.clone(), item.title.clone()));
                }
                if s > 0 {
                    svg_urls.push((url, item.title.clone()));
                }
            }
        }
        totals.table_chapters += table_chapters;
        totals.math_chapters += math_chapters;
        println!(
            "    源文件：table={raw_tables}（{table_chapters} 章） math={raw_math}（{math_chapters} 章） svg={raw_svg}（{} 章）",
            svg_urls.len()
        );

        // 表格章节取样：直接看归一化后的块长什么样。
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

        // 专项探针：SVG 正文/封面、真表格、独立公式。
        let mut probe = BookProbe::default();
        for (url, _title) in svg_urls.iter().take(40) {
            if let Some(blocks) = fetch_blocks(&app, &token, book_url, url).await {
                probe.svg_checked += 1;
                if blocks.is_empty() {
                    probe.svg_empty += 1;
                }
                if blocks.iter().any(|b| b["type"] == "image") {
                    probe.svg_with_images += 1;
                }
                if blocks.iter().any(|b| {
                    matches!(
                        b["type"].as_str(),
                        Some("paragraph") | Some("heading") | Some("list")
                    )
                }) {
                    probe.svg_with_text += 1;
                }
            }
        }
        for (url, _title) in table_urls.iter().take(40) {
            if let Some(blocks) = fetch_blocks(&app, &token, book_url, url).await {
                probe.table_blocks += blocks.iter().filter(|b| b["type"] == "table").count();
            }
        }
        for (url, _title) in math_urls.iter().take(40) {
            if let Some(blocks) = fetch_blocks(&app, &token, book_url, url).await {
                probe.math_blocks += blocks.iter().filter(|b| b["type"] == "math").count();
            }
        }
        println!(
            "    专项：SVG 章={} 空={} 含文字={} 含图={} table 块={} math 块={}",
            probe.svg_checked,
            probe.svg_empty,
            probe.svg_with_text,
            probe.svg_with_images,
            probe.table_blocks,
            probe.math_blocks
        );
        probes.push((file_name.clone(), probe));

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

    let probe_of = |needle: &str| -> &BookProbe {
        probes
            .iter()
            .find(|(name, _)| name.contains(needle))
            .map(|(_, p)| p)
            .unwrap_or_else(|| panic!("未找到验收书：{needle}"))
    };

    // P0：固定版式 SVG 正文必须恢复出文字，SVG 封面必须恢复为图片块。
    let sous = probe_of("Sous le vent");
    assert_eq!(sous.svg_checked, 13, "Sous le vent 应有 13 个 SVG 章节");
    assert!(
        sous.svg_with_text >= 10,
        "Sous le vent 应恢复出至少 10 章 SVG 文字，实际 {}",
        sous.svg_with_text
    );
    assert!(
        sous.svg_with_images >= 2,
        "Sous le vent 应恢复出至少 2 页 SVG 图片，实际 {}",
        sous.svg_with_images
    );
    assert!(
        sous.svg_empty <= 1,
        "Sous le vent 只允许 1 个真正的空白 SVG 页，实际 {}",
        sous.svg_empty
    );
    let secret = probe_of("诡秘之主");
    assert!(secret.svg_checked >= 1, "诡秘之主 应有 SVG 封面章节");
    assert!(
        secret.svg_with_images >= 1,
        "诡秘之主 的 SVG 封面应恢复为图片块"
    );

    // P1：真表格识别；单格公式包装表与 1 行 2 图版式表保持原状。
    let spec = probe_of("EPUB 3.0 Specification");
    assert!(
        spec.table_blocks >= 1,
        "EPUB 3.0 规范应识别出真表格，实际 {} 个",
        spec.table_blocks
    );
    let la = probe_of("A First Course in Linear Algebra");
    assert!(
        la.math_blocks >= 1,
        "线性代数应识别出独立公式，实际 {} 个",
        la.math_blocks
    );
    assert_eq!(
        la.table_blocks, 0,
        "线性代数的 505 个单格公式包装表不应变成数据表"
    );
    let jack = probe_of("The House That Jack Built");
    assert_eq!(jack.table_blocks, 0, "绘本的 1 行 2 图版式表不应变成数据表");

    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(totals.content_failures, 0, "存在导入/正文失败，见上方日志");
    assert_eq!(totals.image_failures, 0, "存在图片读取失败，见上方日志");
}

fn is_cbz_image(name: &str) -> bool {
    let ext = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "jpg" | "jpeg" | "png" | "webp" | "gif" | "avif" | "bmp"
    )
}

/// 真实 CBZ 验收：按“手工放入的漫画目录”读取（无 `oread.json`，从文件名/ComicInfo 反推）。
///
/// 这里不做 CBZ 导入（导入管线只接受 EPUB）；只验证解压后的目录能被漫画读路径识别、
/// 页表数量正确，并且图片代理能实际读出图片。
#[tokio::test]
#[ignore = "需要 BORANLIB_BOOKS_DIR 指向真实 CBZ 目录"]
async fn real_cbz_comics_read_from_directory() {
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
                .map(|e| e.eq_ignore_ascii_case("cbz"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "{} 下没有 CBZ", books_dir.display());

    let (app, pool, dir) = test_app("real-cbz").await;
    let store = Store::new(&dir);
    let token = register_admin(&app, &pool).await;
    let mut total_pages = 0usize;

    println!("\n=== 读取 {} 本 CBZ ===", files.len());
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let book_url = format!("local:cbz:{name}");
        let book_dir = store.dir_for(boranlib::library_store::Tier::Collection, &book_url);
        std::fs::create_dir_all(&book_dir).expect("创建漫画目录");

        let mut archive =
            zip::ZipArchive::new(std::fs::File::open(path).expect("打开 CBZ")).expect("解析 CBZ");
        let mut images = 0usize;
        let mut comic_info: Option<Vec<u8>> = None;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).expect("读取 ZIP 条目");
            if entry.is_dir() {
                continue;
            }
            let entry_name = entry.name().expect("ZIP 条目名").to_string();
            let base = entry_name
                .rsplit('/')
                .next()
                .unwrap_or(entry_name.as_str())
                .to_string();
            if base.is_empty() || base.starts_with('.') {
                continue;
            }
            if base.eq_ignore_ascii_case("ComicInfo.xml") {
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut buf).expect("读取 ComicInfo");
                comic_info = Some(buf);
                continue;
            }
            if !is_cbz_image(&base) {
                continue;
            }
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buf).expect("读取图片");
            // 不同子目录可能有同名文件：加序号前缀避免互相覆盖。
            let file_name = if book_dir.join(&base).exists() {
                format!("{i:04}-{base}")
            } else {
                base
            };
            std::fs::write(book_dir.join(file_name), buf).expect("写入图片");
            images += 1;
        }
        assert!(images > 0, "{name} 没有图片");
        match comic_info {
            Some(bytes) => std::fs::write(book_dir.join("ComicInfo.xml"), bytes).unwrap(),
            None => std::fs::write(
                book_dir.join("ComicInfo.xml"),
                format!(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                     <ComicInfo><Title>{}</Title></ComicInfo>",
                    boranlib_ebook::xml::xml_escape(name.trim_end_matches(".cbz"))
                ),
            )
            .unwrap(),
        }

        let opened = store
            .open_comic(&book_url)
            .await
            .expect("打开漫画目录")
            .unwrap_or_else(|| panic!("{name} 未被识别为漫画"));
        let pages: usize = opened.meta.chapters.iter().map(|c| c.pages.len()).sum();
        assert_eq!(pages, images, "{name} 页表与图片数不一致");

        let resp = app
            .clone()
            .oneshot(authed_get(
                &format!(
                    "/api/comic/source?{}",
                    query(&[("book_url", book_url.as_str())])
                ),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "漫画源探测 {name}");
        let body = body_json(resp).await;
        assert_eq!(body["local"], true, "{name} 应走本地漫画目录");
        assert_eq!(body["comic"], true, "{name} 应识别为漫画");

        let resp = app
            .clone()
            .oneshot(authed_get(
                &format!(
                    "/api/comic/chapter?{}",
                    query(&[("book_url", book_url.as_str()), ("index", "1")])
                ),
                &token,
            ))
            .await
            .unwrap();
        let status = resp.status();
        let body = body_json(resp).await;
        assert_eq!(status, StatusCode::OK, "漫画章节 {name}: {body}");
        let urls = body["images"].as_array().cloned().unwrap_or_default();
        assert_eq!(urls.len(), images, "{name} 章节图片数不一致");
        assert!(
            urls.iter().any(|v| v["width"].as_u64().unwrap_or(0) > 0),
            "{name} 至少应解析出一张图的宽度"
        );

        let first = urls[0]["url"].as_str().expect("图片 URL");
        let resp = app
            .clone()
            .oneshot(authed_get(
                &format!(
                    "/api/book/image?{}",
                    query(&[("url", first), ("book_url", book_url.as_str())])
                ),
                &token,
            ))
            .await
            .unwrap();
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
        assert!(
            status == StatusCode::OK && ctype.starts_with("image/") && !bytes.is_empty(),
            "{name} 首图读取失败：{status} {ctype} {} bytes",
            bytes.len()
        );

        println!(
            "  {name}: 图片={images} 页={pages} 章节={} 首图={} {} bytes",
            opened.meta.chapters.len(),
            ctype,
            bytes.len()
        );
        total_pages += pages;
    }

    let _ = std::fs::remove_dir_all(&dir);
    println!("=== CBZ 汇总：{} 本，{} 页 ===", files.len(), total_pages);
}
