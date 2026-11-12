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
    rt.block_on(run(cfg))?;
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
    // 迁移已通过：清除失败标记，恢复正常启动速度。
    db_guard::clear_startup_failure(&cfg);
    // 上次进程遗留的搜索任务不会再有进度：标为 interrupted，重新登录后仍可查看/移除。
    routes::books::mark_interrupted_search_jobs(&shared).await;
    // 首次启动自动创建管理员账号（凭据来自 ADMIN_USER/ADMIN_PASS）
    auth::bootstrap_admin(&shared, &cfg).await?;

    let api = routes::build_router(shared, cfg.clone());

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
