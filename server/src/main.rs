use boranlib::config::{self, Config};
use boranlib::{auth, db, db_guard, image_cache, req, routes, rule_engine};

use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::services::ServeDir;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = Config::from_env();
    // 生产环境拒绝默认/过短 JWT secret 与不合法的 CORS 来源；失败直接退出，不带病启动。
    if let Err(err) = cfg.validate() {
        eprintln!("配置错误: {err}");
        std::process::exit(2);
    }
    if !cfg.production && cfg.jwt_secret == config::DEFAULT_JWT_SECRET {
        tracing::warn!(
            "JWT_SECRET 仍是默认值，仅限本地开发；生产环境请显式设置 BORANLIB_JWT_SECRET（至少 {} 字节）",
            config::MIN_JWT_SECRET_LEN
        );
    }
    if !cfg.cors_origins.is_empty() {
        tracing::info!(origins = ?cfg.cors_origins, "CORS 白名单已启用");
    }
    // 切片参数与开关的生效值：排障时只看启动日志即可确认是否开了、用的是哪组参数。
    tracing::info!(
        comic_slice = cfg.comic_slice,
        dw = cfg.comic_slice_dw,
        vh = cfg.comic_slice_vh,
        ov = cfg.comic_slice_ov,
        mu = cfg.comic_slice_mu,
        sd = cfg.comic_slice_sd,
        r = cfg.comic_slice_r,
        kindle_vw = cfg.kindle_vw,
        kindle_vh = cfg.kindle_vh,
        "comic slice config (off = whole image)"
    );

    // 低资源预算：474 MiB 可用的 OpenStick。
    // worker 线程固定 2 个、线程栈 512 KiB（默认 2 MiB），避免栈内存随线程数放大。
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(512 * 1024)
        .max_blocking_threads(4)
        .enable_all()
        .build()?;

    // 运维子命令：一次性把旧库正文导出成「目录即书」；doctor 检查/重建派生索引。
    // 服务本身不带业务参数。
    match std::env::args().nth(1).as_deref() {
        Some("migrate-store") => rt.block_on(run_migrate_store(cfg))?,
        Some("doctor") => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            rt.block_on(run_doctor(cfg, &args))?;
        }
        Some(other) => {
            eprintln!("未知子命令: {other}\n可用: migrate-store, doctor");
            std::process::exit(2);
        }
        None => rt.block_on(run(cfg))?,
    }
    Ok(())
}

/// `boranlib doctor [--rebuild-index]`：检查目录、`index.db` 与 `library.db` 是否一致。
///
/// 目录是权威：`index.db` 随时可以重建，`library.db` 里没有的目录说明书在磁盘上、
/// 但用户在馆藏列表里看不到（需要重新入馆或手工补书目）。
async fn run_doctor(cfg: Config, args: &[String]) -> anyhow::Result<()> {
    let mut rebuild = false;
    let mut sync_db = false;
    for arg in args {
        match arg.as_str() {
            "--rebuild-index" => rebuild = true,
            "--sync-db" => sync_db = true,
            other => anyhow::bail!(
                "未知参数: {other}\n用法: boranlib doctor [--rebuild-index] [--sync-db]"
            ),
        }
    }

    let library = db::init_library(&cfg).await?;
    let store = boranlib::library_store::Store::new(&cfg.data_dir);
    store.create_layout()?;
    let index = boranlib::library_store::index::Index::open(store.root()).await?;

    // `--sync-db` 需要先看到最新目录，因此隐含一次索引重建。
    if rebuild || sync_db {
        let r = index.rebuild(&store).await?;
        println!(
            "index.db 已重建：扫描 {} 个目录，写入 {} 行，删除 {} 行",
            r.scanned, r.written, r.removed
        );
        print_list("解析失败", &r.failed);
        print_list("跳过空骨架", &r.skipped);
    }

    if sync_db {
        let n = index.adopt_orphans(&library).await?;
        println!("已补登 {n} 个目录书到 library.db");
        let refreshed = boranlib::library_store::usage::refresh_all(&library, &store).await?;
        println!("已按目录刷新 {refreshed} 本书的缓存统计");
    }

    let v = index.verify(&store, &library).await?;
    println!(
        "目录 {} 个，索引 {} 行；library.db 中尚无目录的书目 {} 个",
        v.dirs,
        v.indexed,
        v.db_without_dir.len()
    );
    print_list("目录解析失败", &v.parse_failed);
    print_list("索引缺少（重建即可）", &v.missing_in_index);
    print_list("索引残留（重建即可）", &v.stale_in_index);
    print_list("索引修订号滞后（重建即可）", &v.revision_drift);
    print_list(
        "目录存在但 library.db 没有（列表里看不到）",
        &v.dir_without_db,
    );
    print_list("library.db 有但目录缺失（仍靠 DB 回退）", &v.db_without_dir);

    // 旧缓存表能否删：目录已经是唯一副本时才会给出提示。
    let legacy = boranlib::library_store::migrate::legacy_coverage(&library, &store).await?;
    if legacy.content_rows + legacy.image_rows > 0 {
        println!(
            "旧缓存表：正文 {} 行（目录已覆盖 {}，仅 DB {}）；图片 {} 行（目录已覆盖 {}，仅 DB {}）",
            legacy.content_rows,
            legacy.content_covered,
            legacy.content_only_db(),
            legacy.image_rows,
            legacy.image_covered,
            legacy.image_only_db()
        );
        print_list("旧缓存仅 DB（没有书目录）", &legacy.books_without_dir);
        print_list("旧缓存仅 DB（书目录不完整）", &legacy.books_partial);
        if legacy.complete() {
            println!("旧缓存已能全部由书目录重建，可以考虑删除 library_content / library_images");
        }
    }

    if v.has_problems() {
        eprintln!("doctor: 发现需要处理的问题");
        std::process::exit(1);
    }
    println!("doctor: OK");
    Ok(())
}

fn print_list(label: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    const LIMIT: usize = 20;
    println!("{label}：{} 条", items.len());
    for item in items.iter().take(LIMIT) {
        println!("  - {item}");
    }
    if items.len() > LIMIT {
        println!("  … 还有 {} 条", items.len() - LIMIT);
    }
}

/// `boranlib migrate-store`：把 `library_content` / `library_images` 导出成书目录。
///
/// 只读旧表、只写目录，不删任何数据；已存在的书目录会跳过。
async fn run_migrate_store(cfg: Config) -> anyhow::Result<()> {
    let library = db::init_library(&cfg).await?;
    let store = boranlib::library_store::Store::new(&cfg.data_dir);
    tracing::info!(root = %store.root().display(), "migrating library store");
    let mut report = boranlib::library_store::migrate::migrate_from_db(&library, &store).await?;
    boranlib::library_store::migrate::verify_tree(&store, &mut report);
    tracing::info!(
        books = report.books,
        skipped = report.skipped,
        chapters = report.chapters,
        images = report.images,
        comics = report.comics,
        failures = report.failures.len(),
        "migrate-store done"
    );
    for failure in &report.failures {
        eprintln!("失败: {failure}");
    }
    if !report.failures.is_empty() {
        anyhow::bail!("{} 本书迁移失败", report.failures.len());
    }
    Ok(())
}

async fn run(cfg: Config) -> anyhow::Result<()> {
    // 上次启动失败（例如库版本高于二进制）时先退避，避免 systemd 风暴式重启。
    db_guard::throttle_startup(&cfg).await;
    // HTTP 抓取预算与 JS 规则预算在启动时一次性固化（进程内单例）
    req::init(&cfg);
    rule_engine::init_js(cfg.js_enabled, cfg.js_timeout_ms);
    // `cache.getFile/putFile` 的落盘目录固定到数据目录下，不暴露任意路径。
    rule_engine::init_rule_cache_dir(
        std::path::Path::new(&cfg.data_dir)
            .join("cache")
            .join("rule"),
    );
    image_cache::init(cfg.image_cache_limit_bytes());

    // 初始化共享桶（书源/账户），缺失则自动建库并执行迁移；
    // 迁移前会自动做一致性备份，失败时输出可读原因并保持非 0 退出码。
    let shared = db::init_shared(&cfg).await?;
    // 馆藏库（全局内容层）：书目 / 目录 / 正文 / 采购单，单独文件避免与账号写入争锁。
    let library = db::init_library(&cfg).await?;
    // 迁移已通过：清除失败标记，恢复正常启动速度。
    db_guard::clear_startup_failure(&cfg);
    // 上次进程遗留的搜索任务不会再有进度：标为 interrupted，重新登录后仍可查看/移除。
    routes::books::mark_interrupted_search_jobs(&shared).await;
    // 上次进程遗留的馆藏采购单放回队列，由后台 worker 续跑（断点续跑）。
    match boranlib::library::requeue_interrupted(&library).await {
        Ok(n) if n > 0 => tracing::info!(count = n, "requeued interrupted library jobs"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "requeue library jobs failed"),
    }
    // 首次启动自动创建管理员账号（凭据来自 ADMIN_USER/ADMIN_PASS）
    auth::bootstrap_admin(&shared, &cfg).await?;

    let state = routes::app_state(
        shared,
        library,
        cfg.clone(),
        std::sync::Arc::new(boranlib::web_book::ReqFetcher),
    );
    // 馆藏采购单执行器：低优先级后台整本下载 + 定期回收临时缓存。
    boranlib::library_worker::spawn(state.clone());
    // 旧库 → 目录：后台补齐仍只在旧表里的书，之后读写都走目录（失败只记日志）。
    boranlib::library_store::migrate::spawn_bootstrap(state.library.clone(), state.store());
    // index.db：目录的派生索引，后台重建一次；失败不影响服务可用性。
    boranlib::library_store::index::spawn_rebuild(state.store());
    let api = routes::build_router_with_state(state);

    // 前端静态资源（Svelte 构建产物 web/dist）；目录不存在时该路由返回 404，不影响 API。
    // 路径可被 WEB_DIR 覆盖（生产从 D:\tools\oread-target-win 启动，不能依赖 cwd 相对路径）。
    let mut app = api
        .fallback_service(ServeDir::new(&cfg.web_dir))
        // 入口门禁：Kindle/Silk UA 访问 `/`（及 SPA 路由入口）303 到 `/k`
        .layer(axum::middleware::from_fn(routes::kindle::entry_redirect));
    // CORS 默认不添加（同源部署）；只有显式配置 BORANLIB_CORS_ORIGINS 才放行这些精确来源。
    if !cfg.cors_origins.is_empty() {
        let origins = cfg
            .cors_origins
            .iter()
            .map(|o| o.parse().expect("cors origin 已在 validate 中校验"))
            .collect::<Vec<_>>();
        app = app.layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods(Any)
                .allow_headers(Any),
        );
    }

    let addr = format!("{}:{}", cfg.host, cfg.port);
    tracing::info!("boranlib listening on {addr}");
    let listener = TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
