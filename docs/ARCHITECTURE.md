# 架构速览

本文只讲「代码放在哪、请求怎么走、数据存在哪」。接口字段见 [API.md](API.md)，构建与部署见 [BUILD.md](BUILD.md)，规则语义见 [ENGINE_REFERENCE_NOTES.md](ENGINE_REFERENCE_NOTES.md)。

## 1. 进程与分层

```
main.rs                 读取 Config → 校验 → 固定 tokio/req/JS/图片缓存预算 → 起服务
 ├─ db_guard            启动限流、迁移前备份、迁移失败退避重试
 ├─ db::init_shared     data_dir/shared.db（账户、书源、账本、体检语料）
 ├─ auth::bootstrap     首次启动创建管理员
 ├─ routes::build_router  axum Router（AppState = shared 池 + Config + 用户池 LRU + Fetcher）
 └─ ServeDir(web/dist)  SPA 静态资源；Kindle/Silk UA 访问 `/` 由 entry_redirect 303 到 `/k`
```

`server/src/lib.rs` 是库入口，集成测试（`tests/routes_smoke.rs`、`tests/rule_engine_fixtures.rs`）直接调用公开 API；`main.rs` 只做进程装配。

## 2. 模块地图（server/src）

| 模块 | 职责 |
|---|---|
| `config*` | 环境变量（`BORANLIB_` 优先、`LEGADO_` 兼容）、生产校验、预算解析 |
| `auth.rs` | 注册/登录/改密、Bearer 鉴权中间件、管理员、用户库连接池 |
| `db*.rs` / `db_guard*` | `shared.db` 与 `users/<uid>.db` 的建库、迁移、备份、可写探测、崩溃自愈 |
| `routes/` | HTTP 层；每个域一个入口文件 + 按职责拆出的子模块（`books_*`、`kindle_*`、`image_*`、`sources_*` 等） |
| `web_book*` | 书源抓取管线：搜索、详情、目录翻页、正文、图片、探索/榜单；`Fetcher` trait 可注入 |
| `req*` | HTTP 预算、按源并发槽/限流、Cookie、字符集解码、重试、SSRF 防护 |
| `rule_engine*` | 规则解释器：CSS/XPath/JSONPath/`@js:`（boa）、替换与变量；JS 资源与 DOM 桥独立文件 |
| `image_cache.rs` / `routes/image*` | 图片代理、墨水屏转码、进程内 LRU |
| `source_health*` / `source_bench*` / `source_ledger*` | 书源健康度、体检任务、导入账本 |
| `query_class.rs` / `source_query_stats.rs` / `search_priority.rs` | 搜索查询分类、按「源 × 查询类」的衰减产出证据、入队优先级（EV + UCB） |
| `k_metrics*` | `/k` 请求日志、聚合与探针 |
| `cache_metrics.rs` | 正文缓存命中/落库/淘汰计数（`/healthz` 暴露） |
| `search_rank.rs` | 排序/配额策略（暂未接入生产路由，模块级 `dead_code`） |
| `title_filter.rs` | 服务端书名包含过滤；`count_title_matches` 供搜索产出统计 |

## 3. 一次正文请求

```
GET /api/book/content
 → auth_middleware（Bearer；公开路径白名单）
 → UserDb（users/<uid>.db，LRU 限制同时打开的库数）
 → load_cached_content：(chapter_url, origin) + TTL + cache_version 指纹
     命中 → 更新 accessed_at（LRU 依据）→ 应用替换规则 → 返回
     未命中 → load_source(shared.db)
             → BookStore(book_key) 预载跨请求变量/cache
             → web_book::chapter_content_book(&dyn Fetcher, ...)
                  → req：预算/并发槽/字符集解码
                  → rule_engine：目录/正文规则
             → 正文清洗 + 用户替换规则
             → save_content_cached：写 book_content，按 size_bytes 上限淘汰最久未读
 → JSON（text 源带 content；image 源带 images）
```

目录、搜索走同一条 `web_book` 管线，只是入口函数不同。`Fetcher` 注入点让路由冒烟测试用 fixture HTML 覆盖 toc/content，不依赖真实网络。

## 4. 数据与缓存

- **`shared.db`**：账户、书源、导入账本、体检语料。全局一份。
- **`users/<uid>.db`**：书架、阅读进度、设置、书签、替换规则、`book_meta`/`book_chapters`、`book_content`。物理分桶，连接池有 LRU 上限。
- **正文缓存**：存在用户库 `book_content`，键 `(url, origin)`，带 TTL 与管线指纹 `cache_version`；空正文不落库；超上限按 `accessed_at` 升序淘汰。
- **图片缓存**：进程内 LRU（`image_cache::stats()`），与正文缓存分开预算。
- **规则缓存**：`<data_dir>/cache/rule/`，仅 `cache.getFile/putFile` 可写。
- **搜索任务（`shared.db`.`search_jobs`）**：两段式搜索的元数据与最终快照。运行期进度以进程内注册表（`books_search_job`）为准；任务收尾时把 `books/errors` 写入 `snapshot_json`。内存只保留运行中 + 最近 20 个已完成任务（完成后再留 10 分钟），数据库保留每用户最近 50 个。`/api/search/result` 内存未命中时回退数据库快照，游标翻页继续复用 `rank_bucket/rank_seq`。进程启动时把上次遗留的 `running` 行标为 `interrupted`，避免前端永远轮询一个不会动的任务。
- **搜索取消语义**：`/api/search/jobs/cancel` 置取消标志并 `notify_one()` 唤醒调度循环；立即停止取新源、丢弃在飞请求，未完成的源计入 `cancelled`（不写源健康、不算站点失败），已完成结果保留。运行中的任务必须先取消才能移除。

## 5. 前端（SPA）

- Svelte 4 + Vite，产物 `server/web/dist`，由 `ServeDir` 托管；`src/App.svelte` 负责登录态与 tab 路由。
- 首屏只静态引入 Login/Bookshelf/Discover；Reader、ComicReader 与统计/书源/替换/设置页均 `import()` 懒加载，失败有可见错误与重试。
- 纯逻辑下沉在 `src/lib/`：`api.js`（请求封装，错误/URL 拆到 `apiErrors.js`、`apiUrl.js`）、`readerHtml/images/stats/auto/progress/gestures/math`、`searchJobsModel`（任务状态/进度/时间纯函数）、`readingPrefs`、`theme` 等，均有 `node --test` 单测。
- 搜索任务共享状态在 `lib/searchJobsStore.js`：App 登录后轮询 `/api/search/jobs`（有运行中任务 2.5s、空闲 15s），退出时停止并清空；Discover 的 `SearchJobsPanel` 只读取 store，支持取消、移除与「查看结果」重新挂载（运行中继续轮询，已结束直接读快照）。
- 组件间共享 DOM 引用或双向状态用 Svelte 原生 `bind:`（如 `bind:scrollEl`），**不要**用函数 prop 在子组件 `$:` 里回传：函数 prop 每次 `$set` 都会被判定为“已变化”，容易形成「子更新 → 回传 → 父更新 → 重新 `$set` → 子更新」的更新回环；`bind:` 由编译器生成 `updating_*` 标志，结构上阻断反向回写。
- 渲染期异常兜底：`App.svelte` 监听 `error` / `unhandledrejection`，用原生 DOM 挂出可关闭的提示条与「重新加载」按钮。Svelte 的 `flush()` 抛错后调度器不会复位，之后所有状态更新都不再执行，因此不能依赖 Svelte 自身状态来提示错误。
- 阅读偏好与 eink 主题走 CSS 变量；墨水屏样式门禁见 [EINK_STYLE.md](EINK_STYLE.md)。

## 6. `/k` 与 SPA 的边界

- `/k`、`/k/toc`、`/k/read`、`/k/search`、`/k/login`、`/k/logout`、`/k/_selftest` 是服务端渲染的 HTML（`table` 布局、ES5 友好），**不含业务 JS**。
- 仅有三类诊断脚本按需注入：错误兜底 `K_GUARD_JS`、视口标定 `K_VP_JS`、自报探针 `K_PROBE_JS`；它们只打点，不改变阅读行为。
- Kindle/Silk UA 访问 `/` 会被 303 到 `/k`；`/k` 埋点写 `DATA_DIR/logs/k-metrics-*.jsonl`。

## 7. 工程门禁

| 范畴 | 命令 |
|---|---|
| Rust | `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`（工具链固定 `rust-toolchain.toml`） |
| 前端 | `npm test`（`node --test`）、`npm run build`、`npm run check`（svelte-check） |
| 预算 | `node scripts/check-budget.mjs`（`scripts/budget.json` 为唯一阈值来源） |
| 墨水屏 | `node scripts/eink-lint.mjs` |
| 快照 | `powershell -File D:\tools\golden-check.ps1`（只读接口 + 临时空库） |

CI（`.github/workflows/ci.yml`）分层为 `rust` / `web` / `eink` / `migrations`；三平台 release 构建只在 tag 或手动触发时跑。
