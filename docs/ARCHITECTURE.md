# 架构速览

本文只讲「代码放在哪、请求怎么走、数据存在哪」。接口字段见 [API.md](API.md)，构建与部署见 [BUILD.md](BUILD.md) 与 [DEPLOY.md](DEPLOY.md)，墨水屏约束见 [EINK_STYLE.md](EINK_STYLE.md)。

## 1. 进程与分层

```
main.rs                 读取 Config → 校验 → 固定 tokio/req/JS/图片缓存预算 → 起服务
 ├─ db_guard            启动限流、迁移前备份、迁移失败退避重试
 ├─ db::init_shared     data_dir/shared.db（账户、书源、账本、体检语料）
 ├─ db::init_library    data_dir/library.db（馆藏：书目/目录/正文/采购单）
 ├─ auth::bootstrap     首次启动创建管理员
 ├─ routes::app_state   AppState = shared 池 + library 池 + Config + 用户池 LRU + Fetcher
 ├─ library_worker      后台整本下载（采购单）+ 每小时回收过期临时缓存
 ├─ routes::build_router_with_state  axum Router
 └─ ServeDir(web/dist)  SPA 静态资源；Kindle/Silk UA 访问 `/` 由 entry_redirect 303 到 `/k`
```

`server/src/lib.rs` 是库入口，集成测试（`tests/routes_smoke.rs`、`tests/rule_engine_fixtures.rs`）直接调用公开 API；`main.rs` 只做进程装配。

### Cargo workspace

`server/Cargo.toml` 是 workspace 根，同时也是 `boranlib` 二进制包。依赖严格单向：

```
boranlib (server)  →  boranlib-ebook    (server/crates/ebook)
                   →  boranlib-rule-core (server/crates/rule-core)
                   →  （馆藏与路由仍在 server 包内，后续继续拆出）
```

`boranlib-ebook` 的硬约束：**不出现 `sqlx`、不出现 `axum`、不碰 `data_dir` 布局**。它只认识书的数据结构
和「按章/按页读取」的 trait，存储与 HTTP 由调用方注入。这样做是为了让「格式层与存储层解耦」从注释变成编译错误。

`boranlib-rule-core` 收纳书源引擎内核：`config`（抓取/搜索限额）、`req*`（HTTP 预算、Cookie、字符集解码）、
`rule_engine*`（CSS/XPath/JSONPath/JS）、`entities`（书源与书目结构体）、`crypto`（源规则用到的加解密）。
默认构建**不含 `sqlx`/`axum`**，可以脱离服务端单独编译与测试；只有服务端显式开启 `sqlx-row` feature 时，
`entities` 才派生 `sqlx::FromRow`，避免重复定义实体。`web_book*`、`source_*`、`query_class` 因依赖馆藏库
与体检记录，暂时留在 server 包内。

## 2. 模块地图（server/src）

> `config`、`crypto`、`entities`、`req*`、`rule_engine*`、`serde_util`、`search_rank`、`title_filter`
> 已移入 `boranlib-rule-core`，这里通过 `pub use` 保留同名模块路径。

| 模块 | 职责 |
|---|---|
| `config*`（rule-core） | 环境变量（`BORANLIB_` 优先、`LEGADO_` 兼容）、生产校验、预算解析 |
| `auth.rs` | 注册/登录/改密、Bearer 鉴权中间件、管理员、用户库连接池；`/opds` 前缀的 HTTP Basic 校验与 challenge |
| `db*.rs` / `db_guard*` | `shared.db`、`library.db` 与 `users/<uid>.db` 的建库、迁移、备份、可写探测、崩溃自愈 |
| `library.rs` | 馆藏分层（`collection`/`temp`）、书目读写、TTL 清理、磁盘水位、采购单任务表 |
| `library_worker.rs` | 馆藏采购单执行器：后台整本下载、断点续跑、磁盘安全阀、逐章取消；正文/漫画图片直写书目录 |
| `ebook_source.rs` | 只读书目录 + 按章正文/按页图片读取器（`LibraryReader`、`load_book_input`、`load_comic_input`）；存储实现留在服务端 |
| `boranlib-ebook` crate | `server/crates/ebook`：导出/转换领域层，**不依赖 sqlx/axum**。`model`（书视图 `BookInput` / `ComicInput` / `Format` / `WriteReport`）、`pipeline`（`ChapterReader` / `FormatWriter`）、`html`（正文 HTML → 语义块）、`xml`（共用转义）、`format/txt`、`format/cbz`、`format/epub`、`zip`（store-only ZIP）。新增格式 = 加一个 writer，路由与存储不变 |
| `image_fetch.rs` | 图片回源的共用入口：整图抓取（worker 落盘）与只取响应头的体积探测（入馆预估） |
| `routes/` | HTTP 层；每个域一个入口文件 + 按职责拆出的子模块（`books_*`、`library`、`opds`、`kindle_*`、`image_*`、`sources_*` 等） |
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
 → UserDb（users/<uid>.db，LRU 限制同时打开的库数；只取书源登录态/个人覆盖）
 → 书目录（library_store）：Store::open_book 按 `oread:chapter-url` 读章节 XHTML
     命中 → 应用替换规则 → 返回（不查缓存、不回源）
     未命中 → 同用户同章节并发闸门；拿到闸门后重查目录
             → load_source(shared.db)
             → BookStore(book_key) 预载跨请求变量/cache（用户库）
             → web_book::chapter_content_book(&dyn Fetcher, ...)
                  → req：预算/并发槽/字符集解码
                  → rule_engine：目录/正文规则
             → 正文清洗 + 用户替换规则
             → 目录已存在时 save_text_prefer_dir 写章节文件；目录不存在则本次只返回结果
 → JSON（text 源带 content；image 源带 images）
```

目录、搜索走同一条 `web_book` 管线，只是入口函数不同。`Fetcher` 注入点让路由冒烟测试用 fixture HTML 覆盖 toc/content，不依赖真实网络。

## 3.1 一次导出请求

```
GET /api/book/export/txt
 → ebook::source::load_book_input：书目 + 目录 + 缓存统计（只读书目录、不抓网）
 → ebook::source::LibraryReader：按章读章节 XHTML（占位章视为未缓存）
 → ebook::format::txt::TxtWriter：逐章流式写临时文件，未缓存章节显式占位
 → routes/export：16 KiB 分块回传，读完即删；中断由 cleanup_stale 兜底

GET /api/book/export/cbz
 → ebook::source::load_comic_input：图片章节 + 页表（只读书目录）
 → library_store::pack：目录完整时直接原样打包（ComicInfo.xml 首条、排除 oread.json）
 → ebook::zip::ZipWriter：store-only、确定性时间戳（不压缩已压缩的图片）
 → routes/export：同样的临时文件流式回传

GET /api/book/export/epub
 → ebook::source::load_book_input：正文页 + 文本章节统计（漫画章不计入）
 → library_store::pack：目录完整时直接原样打包（mimetype 首条）
 → ebook::format::epub::EpubWriter：仅在目录不完整时逐章生成
 → ebook::zip::ZipWriter：mimetype 首条 store，其余条目同样不压缩
 → routes/export：同样的临时文件流式回传
```

换格式只替换 `FormatWriter`；`Format` 枚举与 `WriteReport` 是格式间唯一共享的契约。`ebook` 层不碰 `data_dir` 布局，产物路径由路由决定。TXT/EPUB 读正文，CBZ 读图片；三者共用 `cleanup_stale` 与 16 KiB 回传通道。目录不存在或形态不符时直接 409，不再回退旧表。

## 3.2 一次 OPDS 请求

```
GET /opds/books?kind=text
 → auth_middleware：`/opds` 前缀且无 Bearer → HTTP Basic
      → users 实时校验收/禁用 → bcrypt（5 分钟进程内缓存）→ 注入 AuthUser/UserDb/LibraryDb
      失败 → 401 + WWW-Authenticate: Basic realm="oread OPDS"
 → routes/opds::books：只查 tier=collection，keyset 分页（sort_ts DESC, book_url ASC）
      按 book_source_type 读 library_books.cached_chapters（目录写入/编辑/doctor --sync-db 维护）
      → 决定是否输出 EPUB / CBZ acquisition 链接
      标签批量查 library_tags，元数据复用 language/publisher/pubdate/series_*
 → Atom acquisition feed（相对链接以 Host / X-Forwarded-Proto 补成绝对地址）

GET /opds/download/epub
 → export::epub_download → ebook 管线 → 临时文件 → 16 KiB 流式回传 → 读完即删
GET /opds/cover
 → library_books.cover_url → image 代理（书源 Referer/UA/Cookie、本地目录文件、墨水屏转码）
```

OPDS 与网页端共用同一套导出 writer 和图片代理，不复制第二份文件/图片逻辑；也可直接用
Bearer 访问 `/opds*`，便于调试。

## 4. 数据与缓存

> **目录即书（已完成切换）**：每本书一个解压目录 + 可重建索引，见 §4.1。
> `library_content` / `library_images` 已在迁移 `0004_drop_legacy_cache.sql` 中删除；
> 正文、目录与漫画图片的唯一副本都在书目录里，读写路径不再回退旧表。
> 旧设备上的正文/图片缓存直接丢弃，不迁移；书重新打开时会按目录重新抓取。
> `library_chapters` 暂时保留，只作为 TOC 缓存与 `chapter_url → book_url` 反查，后续再移除。

### 4.1 目标存储：目录即书

- `data_dir/library/{collection,temp,trash}/<sha256(book_url)>/`
  - 文字书：解压后的 EPUB 3（`mimetype` + `META-INF/container.xml` + `OEBPS/`）。
  - 漫画：解压后的 CBZ（扁平图片 + 标准 `ComicInfo.xml` + 目录边车 `oread.json`）。
    `oread.json` 只保存章节标题/章节 URL/卷标记/页范围与图片尺寸，不复制图片字节；
    缺失时按 `NNNN-MMM.ext` 文件名前缀反推章节，手工放入的 CBZ 也能直接读。
- **`content.opf` 是元数据权威**；`index.db` 只存可重建的派生字段（书名/作者/系列/主题/
  修订号/章页计数/封面 href），靠 `oread:revision` 判脏。`index.db` 放在
  `data_dir/library/index.db`，删掉不会丢内容：进程启动时后台全量重建，
  `boranlib doctor --rebuild-index` 可手动重建。
- 馆藏搜索（`GET /api/library/books?q=`）先查 `index.db`（书名/作者/系列/主题/简介），
  再与 `library.db` 的 `name`/`author` 取并集；写路径（TOC/正文/元数据/封面/漫画页/
  入馆/移出/删除）在落盘后增量刷新索引。排序与分页仍用 `library.db` 的访问时间。
- 馆藏列表的「已缓存章节 / 正文 / 图片占用」仍读 `library_books` 的冗余列，但数值由目录计算
  （`library_store::usage`）：文字书逐章读文件、占位章不算，漫画逐页 `stat`。管理员编辑后
  立即重算，`doctor --sync-db` 全量重算；手工放进 `collection/` 的目录书因此也有正确进度，
  OPDS 也按这个计数决定是否给 EPUB / CBZ 下载链接。
- 保存顺序固定：正文文件 → `nav.xhtml` → `content.opf` → 索引事务；单文件同目录临时文件 + `fsync` + `rename`（Windows 退避重试）。
- 章节文件序号只增不复用、永不改名；顺序只存在于 `spine`，插入新章不需要重写任何 href。
- 章节正文是归一化后的「标题 + 段落」XHTML；段落编辑保存时由服务端重新序列化，结构上写不出非法文件。
- `promote`（临时→馆藏）= 目录 `rename`；失败写 `.oread-promote` 标记（内容是 `book_url`），
  后台每 60 s 重试；期间读路径优先选有内容的目录，不被空馆藏骨架挡住。`demote` / `move_to_trash`
  同理，删除与 TTL 清理都把目录移进 `trash/`。
- 不再有迁移命令或启动后台补齐：`library_content` / `library_images` 已随迁移删除，
  旧设备缓存按「全新开始」直接丢弃；书重新打开时由抓取路径按需重建目录。
- 写路径（`library_store::write`）：TOC 抓取成功后 `sync_toc` 按章节 URL 合并 spine——
  旧章保留 id/href 只更新标题，新章追加新序号，目录变短只移出 spine、不删文件也不复用序号；
  正文抓取成功后 `save_text_chapter` 写归一化段落（首段等于章节标题时去掉，避免重复）；
  漫画整章所有页都拿到才 `save_comic_pages` 写页表与 `ComicInfo.xml`，部分成功不写，
  下次任务重试；页数变少时删除旧的多余页文件。漫画图片下载完直接在内存里写进
  书目录（`oread.json` 页表 + 扁平图片文件）。书目录还没建时本章失败、下次任务重下。
  所有写入先章节/图片、再 `nav.xhtml`、最后 `content.opf`，并受每本书一把写锁保护。
- 读路径：`Store::open_book` 解析 OPF，按 `oread:chapter-url` 定位章节文件；`<body>` 还原成
  段落 HTML（去掉与 OPF 重复的首行标题）。未抓取的章节写成带 `oread-pending` 标记的占位文件，
  读路径见到标记按“未缓存”处理，不会被当成正文返回或计入完成度。
- 漫画读路径：`Store::open_comic` 解析 `oread.json`（或反推 CBZ），页地址统一为
  `oread-img://<sha256(book_url)>/<文件名>`。`/api/comic/chapter` 直接返回这些本地地址，
  `/api/book/image` 在解析书源之前先识别该 scheme 并直读目录文件，因此**没有书源、源站
  下线也能读已入馆漫画**；`/api/book/images/meta`、`/k` 漫画页与 `/api/book/toc` 同样目录优先。
  图片代理的 WebP→灰度 JPEG、长条切片与 ETag 逻辑与数据库图片完全共用。
- `/k`（Kindle/Silk）与 SPA 同源：`/k` 的目录（`ensure_chapters`）先读 OPF spine / `oread.json`，
  `/k/read` 的正文先读章节文件；命中后管理员编辑与离线阅读立即生效，未命中才查缓存/回源。
  书源已失效或不存在时合成一个「馆藏」书源，阅读路径按目录实际形态选择正文或漫画。
- 管理员编辑（`library_store::edit` + `/api/library/book/edit*`）：只改已归一化的目录内容，
  v1 覆盖元数据、正文段落、封面、漫画单页。元数据复用 `/api/library/book/meta`，DB 更新后
  同步写 `content.opf` / `oread.json`；正文与图片直接写文件，再按
  「正文/图片 → `nav.xhtml` → `content.opf`」或「图片 → `oread.json` → `ComicInfo.xml`」更新。
  每本书一把写锁，正文/封面/页保存用 `oread:revision` 做乐观锁（冲突 409）。图片以文件内容
  识别格式并决定扩展名（只接受 JPEG/WebP），避免把非图片写进目录。本地图片 URL 带
  `?v=<revision>`：换页/换封面后 URL 变化，图片缓存与 ETag 自动失效。
- 运维检查：`boranlib doctor` 只读比对「目录 ↔ `index.db` ↔ `library.db`」，报告解析失败的
  目录、索引滞后/残留、目录存在但 DB 没有（馆藏列表里看不到）、DB 有但目录缺失（该书当前不可读）。
  `--rebuild-index` 先重建索引再检查；`--sync-db` 隐含重建，并把孤立目录补登成书目
  （手工放进 `collection/` 的书目录因此能在列表里出现）。有需要处理的问题时退出码 1，
  便于脚本/定时任务报警。
- 导出（`library_store::pack`）：直接按 store-only ZIP 原样打包目录——EPUB 把
  `mimetype` 放第一条、随后 `META-INF/container.xml`；CBZ 把 `ComicInfo.xml` 放第一条并排除
  内部边车 `oread.json`。目录不存在或形态不符时直接 409，不再回退逐章/逐页生成；
  因此导出的字节与磁盘上的书一致（包括管理员编辑结果）。

### 4.2 数据库分工（现状）

- **`shared.db`**：账户、书源、导入账本、体检语料。全局一份。
- **`library.db`**：馆藏目录层，全局一份。`library_books`（书目 + `tier` + 冗余统计）、`library_chapters`（TOC 缓存与 `chapter_url → book_url` 反查，后续移除）、`library_identifiers` / `library_tags`（OPF 语义扩展，键 `book_url`）、`library_jobs`/`library_job_items`（采购单）。正文与漫画图片不在库里，只存书目录。
  - `tier=collection`：馆藏，永久保存，不自动淘汰；只有管理员能移出或删除。
  - `tier=temp`：临时缓存，按 `last_accessed_at` 做 TTL 回收（默认 30 天），每小时后台清理。
  - **不设容量上限**，只做占用监控；`/api/healthz` 与 `/api/library/stats` 暴露分层占用。
  - **元数据分层**：系列、语言、出版社、出版日期、标识（ISBN 等）、标签属于「谁抓到都一样」的全局字段，写在 `library_books` / `library_identifiers` / `library_tags`；星级、分组、私人标签留在用户库。读写接口见 [API.md](API.md) 6.12.3。
- **`users/<uid>.db`**：书架、阅读进度、设置、书签、替换规则、书源登录态（`book_vars`/`book_cache`）、个人覆盖字段。物理分桶，连接池有 LRU 上限。`book_meta.total_chapter_num` 保留为馆藏权威值的副本，书架进度口径不变。
- **正文读取**：先按书目录读章节文件，命中直接返回；未命中才抓取，目录存在时写入章节文件，否则本次结果不落盘。
  - `tier=collection` 的书**不按 TTL 过期**：正文、目录、漫画图片都直接读书目录，书源失效后已入馆的书仍可打开。
  - 回源失败时不再有旧缓存可回退，正文/目录直接返回错误；已落盘的章节不受影响。
- **采购单**：入馆时排队，`library_worker` 在服务端整本下载（默认 1 任务 × 3 章并发），进程重启后把 `running` 放回队列断点续跑；剩余磁盘低于 `LIBRARY_DISK_FLOOR_MB`（默认 1 GiB）时暂停，交互阅读不受影响。
- **冷却与来源回填**：任务开始前查 `source_health.cooldown_until`，冷却中的书源把任务推迟到冷却结束（`library_jobs.updated_at` 即「下次可执行时间」），先跑别的书；worker 加载到真实书源后回填馆藏书目缺失的 `origin` / `origin_name` / `book_source_type`。
- **漫画图片**：馆藏漫画由 worker 把解密后的原始字节下载到内存，整章直接写进书目录（`oread.json` 页表 + 扁平图片文件）；单张 > 2 MiB 只留清单不落盘，整章 > 256 MiB 放弃。`/api/book/image` 直接读目录里的 `oread-img://` 文件，不再有 BLOB 回退；墨水屏灰度/WebP 转码变体仍运行时生成，不落盘。
- **体积预估**：`POST /api/library/comic/estimate` 抽 3 章 × 2 图，只探测响应头（`Range: bytes=0-0` 优先），按 `book_url + source + uid` 缓存 30 分钟；估算只警告不硬拦，前端展示区间与「抽样估算，以实际下载为准」。
- **图片缓存**：进程内 LRU（`image_cache::stats()`），与正文分开预算。
- **规则缓存**：`<data_dir>/cache/rule/`，仅 `cache.getFile/putFile` 可写。
- **搜索任务（`shared.db`.`search_jobs`）**：两段式搜索的元数据与最终快照。运行期进度以进程内注册表（`books_search_job`）为准；任务收尾时把 `books/errors` 写入 `snapshot_json`。内存只保留运行中 + 最近 20 个已完成任务（完成后再留 10 分钟），数据库保留每用户最近 50 个。`/api/search/result` 内存未命中时回退数据库快照，游标翻页继续复用 `rank_bucket/rank_seq`。进程启动时把上次遗留的 `running` 行标为 `interrupted`，避免前端永远轮询一个不会动的任务。
- **搜索取消语义**：`/api/search/jobs/cancel` 置取消标志并 `notify_one()` 唤醒调度循环；立即停止取新源、丢弃在飞请求，未完成的源计入 `cancelled`（不写源健康、不算站点失败），已完成结果保留。运行中的任务必须先取消才能移除。

## 5. 前端（SPA）

- Svelte 4 + Vite，产物 `server/web/dist`，由 `ServeDir` 托管；`src/App.svelte` 负责登录态与 tab 路由。
- 首屏只静态引入 Login/Bookshelf/Discover；Reader、ComicReader 与统计/馆藏/书源/替换/设置页均 `import()` 懒加载，失败有可见错误与重试。
- **阅读器独立目录 `src/reader/`**：`Reader.svelte`、`ComicReader.svelte`、`Reader*.svelte`、`reader*.js` 与其单测都在这里。硬规则：`reader/` 只能 import `./` 与 `../lib/`，**不许 import `pages/`**——管理端组件里的 `object-fit`/`transform`/`vw` 会让 KPW3 阅读页白屏，而故障现场在别处。门禁 `node scripts/check-reader-boundary.mjs`（CI 的 web job 已接入）。
- **PC 与移动端共用同一阅读器**：不按 UA 分叉，只按视口/触控/键盘调整页宽、手势与控制栏；`/k` 是唯一的第二套前端（服务端渲染，无业务 JS）。
- **正文长期模型是语义块**（标题/段落/图片）：Web 与 `/k` 从同一份块数据各自渲染，不把书源 HTML 直接注入 DOM——插值天然防注入，分页与墨水屏布局也可预测。当前 Web 端暂由 `readerHtml.js` 解析 HTML，迁移计划见 [TODO.md](TODO.md) 第 1 项。
- 馆藏页（`pages/Library.svelte`）所有登录账号可浏览占用与进度；入馆/移出/删除/回收临时缓存按 `getRole() === 'admin'` 收口。
- 纯逻辑下沉在 `src/lib/`：`api.js`（请求封装，错误/URL 拆到 `apiErrors.js`、`apiUrl.js`）、`searchJobsModel`（任务状态/进度/时间纯函数）、`readingPrefs`、`theme` 等，均有 `node --test` 单测；阅读器相关的 `readerHtml/images/stats/auto/progress/gestures/math` 已随阅读器移入 `src/reader/`。
- 搜索任务共享状态在 `lib/searchJobsStore.js`：App 登录后轮询 `/api/search/jobs`（有运行中任务 2.5s、空闲 15s），退出时停止并清空；Discover 的 `SearchJobsPanel` 只读取 store，支持取消、移除与「查看结果」重新挂载（运行中继续轮询，已结束直接读快照）。
- 组件间共享 DOM 引用或双向状态用 Svelte 原生 `bind:`（如 `bind:scrollEl`），**不要**用函数 prop 在子组件 `$:` 里回传：函数 prop 每次 `$set` 都会被判定为“已变化”，容易形成「子更新 → 回传 → 父更新 → 重新 `$set` → 子更新」的更新回环；`bind:` 由编译器生成 `updating_*` 标志，结构上阻断反向回写。
- 渲染期异常兜底：`App.svelte` 监听 `error` / `unhandledrejection`，用原生 DOM 挂出可关闭的提示条与「重新加载」按钮。Svelte 的 `flush()` 抛错后调度器不会复位，之后所有状态更新都不再执行，因此不能依赖 Svelte 自身状态来提示错误。
- 阅读偏好与 eink 主题走 CSS 变量；墨水屏样式门禁见 [EINK_STYLE.md](EINK_STYLE.md)。

## 6. `/k` 与 SPA 的边界

- `/k`、`/k/toc`、`/k/read`、`/k/search`、`/k/login`、`/k/logout`、`/k/_selftest` 是服务端渲染的 HTML（`table` 布局、ES5 友好），**不含业务 JS**。
- `/k/search?key=...` 只创建后台搜索任务并 303 到 `/k/search?job=...`：运行中渲染带 `<meta http-equiv="refresh">` 的进度页（2 秒），完成后从任务快照本地分页。翻页链接只带 `job`，与 SPA 共用同一套 `search_jobs` 元数据、调度和证据；离开页面任务继续跑，进程重启后仍可读数据库快照。
- 仅有三类诊断脚本按需注入：错误兜底 `K_GUARD_JS`、视口标定 `K_VP_JS`、自报探针 `K_PROBE_JS`；它们只打点，不改变阅读行为。
- Kindle/Silk UA 访问 `/` 会被 303 到 `/k`；`/k` 埋点写 `DATA_DIR/logs/k-metrics-*.jsonl`。

## 7. 工程门禁

| 范畴 | 命令 |
|---|---|
| Rust | `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`（工具链固定 `rust-toolchain.toml`） |
| 前端 | `npm test`（`node --test`）、`npm run build`、`npm run check`（svelte-check） |
| 阅读器边界 | `node scripts/check-reader-boundary.mjs` |
| 预算 | `node scripts/check-budget.mjs`（`scripts/budget.json` 为唯一阈值来源） |
| 墨水屏 | `node scripts/eink-lint.mjs` |
| 快照 | `powershell -File D:\tools\golden-check.ps1`（只读接口 + 临时空库） |

CI（`.github/workflows/ci.yml`）分层为 `rust` / `web` / `eink` / `migrations`；三平台 release 构建只在 tag 或手动触发时跑。
