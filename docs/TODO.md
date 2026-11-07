# Boranlib 待办与决策清单

> 本文件只记录**决策与待办**，不替代接口契约。接口字段、参数、错误码以 [API.md](../docs/API.md) 为准。

## 命名与部署口径（Boranlib）

- 产品/服务名：**Boranlib**；历史文档里的 `oread` 均指本项目，已统一改名。
- 二进制：`boranlib`（Windows `boranlib.exe`）；部署目录 `/opt/boranlib`。
- 环境变量：统一前缀 `BORANLIB_*`，兼容旧 `LEGADO_*` 前缀（新名优先）。程序只读取这两个前缀，不读取未加前缀的旧名（`WEB_DIR` 单独设置不生效）。二进制内置缺省端口 8080（`BORANLIB_PORT` / 旧 `LEGADO_PORT`）；Windows 成品包的 `start.ps1` 在未显式配置时会注入 `BORANLIB_PORT=3000`，所以成品包默认访问 `:3000`（v0.3.12 实测日志监听 `0.0.0.0:3000`）。
- 书源 JSON 里的 `Legado` 指上游兼容格式，保留不改；仓库路径 `<repo>` 与文件链接保持磁盘原名。

## 生成日期与依据

- 生成时间：2026-11-02 20:10（Asia/Shanghai，UTC+8）；**收尾校准：2026-11-03 15:16**
- 仓库：`<repo>`
- Git：`HEAD = 7dbe6f14`；工作区有大量未提交改动，**以磁盘源码为准**
- 复核说明：代码正被多个 agent 修改。本清单在 20:05~20:10 重新读取下列文件后整理；若与最新源码冲突，以源码为准并回来更新状态。
- 主要依据：
  - 路由与服务：[mod.rs](../server/src/routes/mod.rs)、[sources.rs](../server/src/routes/sources.rs)、[books.rs](../server/src/routes/books.rs)、[bookshelf.rs](../server/src/routes/bookshelf.rs)、[replace.rs](../server/src/routes/replace.rs)、[stats.rs](../server/src/routes/stats.rs)、[health.rs](../server/src/routes/health.rs)
  - 基础能力：[auth.rs](../server/src/auth.rs)、[config.rs](../server/src/config.rs)、[main.rs](../server/src/main.rs)、[web_book.rs](../server/src/web_book.rs)、[rule_engine.rs](../server/src/rule_engine.rs)
  - 前端消费：[api.js](../server/web/src/lib/api.js)、[Reader.svelte](../server/web/src/pages/Reader.svelte)、[Settings.svelte](../server/web/src/pages/Settings.svelte)
  - 构建/工程：[build-dev.ps1](../scripts/build-dev.ps1)、[build-release.ps1](../scripts/build-release.ps1)、[server/Cargo.toml](../server/Cargo.toml)、[rust-toolchain.toml](../rust-toolchain.toml)、[ci.yml](../.github/workflows/ci.yml)

## 状态图例

| 状态 | 含义 |
|---|---|
| `待做` | 已确认需要做，尚未开始 |
| `处理中` | 有 agent/人正在改，或部分完成 |
| `已完成待验证` | 代码已存在，但缺少端到端验证/回归用例 |
| `已完成` | 代码已实现且有实测/冒烟依据（2026-11-03 起使用） |
| `已修复` | 缺陷已修并回归通过，保留原描述供追溯（2026-11-03） |
| `延后` | 方向成立但优先级低于当前主线，暂不排期（2026-11-03） |
| `已决定不做` | 产品层面明确放弃，不再排期（2026-11-02 与用户确认） |

---

## 一、明确不做（勿再排期）

以下条目已于 **2026-11-02** 与用户确认放弃；**2026-11-03 范围收敛为“小说 + 漫画”**，音频/视频/音乐/游戏/正版音频类同时进入类型黑名单。全部状态为 `已决定不做`，列出来是为了防止后续 agent 重复提案。

### 1.1 类型黑名单（2026-11-03 范围收敛）

| # | 类型 | 状态 | 一句话理由 |
|---|---|---|---|
| 1 | 有声 / 音频内容（TTS 朗读、HTTP TTS、音频播放、有声书） | `已决定不做` | 产品只做小说 + 漫画；Web 端 TTS 体验不稳定，且与阅读进度/分页耦合。FEATURE_MATRIX 模块 12 共 6 条已全部移出“要做”基数。 |
| 2 | 视频 | `已决定不做` | 不在小说 + 漫画范围；无对应功能点（0 条）。 |
| 3 | 音乐 | `已决定不做` | 不在小说 + 漫画范围；无对应功能点（0 条）。 |
| 4 | 游戏 | `已决定不做` | 不在小说 + 漫画范围；无对应功能点（0 条）。 |
| 5 | 正版音频（付费音频内容） | `已决定不做` | 只保留小说/漫画的付费标记展示（`is_pay`），不做音频版权内容。 |

### 1.2 已确认放弃的产品项

| # | 不做的项 | 状态 | 一句话理由 |
|---|---|---|---|
| 6 | RSS 订阅 | `已决定不做` | 与“书源驱动阅读”主线无关；`shared.rss_sources` 表虽然建了，但没有任何接口/解析代码，保留表不影响运行。 |
| 7 | 备份与恢复（含 WebDAV） | `已决定不做` | 当前是单机 SQLite + 文件目录，导入导出成本高、收益低；先把阅读主链路做稳。**例外（已实现）**：迁移前的数据库自动快照（`data/backups/`，见 BUILD.md §11），属于启动自救而非用户侧备份功能。 |
| 8 | 词典与翻译 | `已决定不做` | 属于阅读增强而非核心能力，接入第三方服务会引入网络/合规/成本负担。 |
| 9 | 平台特性：通知栏控制、桌面小组件、耳机线控、快捷方式、开机自启 | `已决定不做` | 依赖原生 Android 壳，当前交付物是 Web 前端 + Rust 服务端，无法可靠实现。 |
| 10 | 边角页面：关于我们、免责声明、隐私政策、检查更新、日志导出、崩溃上报 | `已决定不做` | 不产生核心价值，且需要额外的合规文本与发布流程。 |
| 11 | 本地书籍导入与解析（txt/epub/pdf/mobi） | `已决定不做` | 需要引入解析器与格式兼容层，工作量远大于 Web 书源；当前所有书籍都来自书源。 |
| 12 | 音量键等设备绑定交互 | `已决定不做` | 浏览器无法可靠监听音量键；需要原生壳或侵入式权限，不做。 |
| 13 | WebView / `java.webView*` 桥接（含需要执行页面 JS 的书源） | `已决定不做` | 需要真实浏览器内核（渲染、Cookie、JS 执行环境）；服务端只做静态抓取，无法在无头环境复刻，且资源/稳定性成本高。 |
| 14 | 百度验证页 / 人机验证绕过 | `已决定不做` | 属于风控对抗，需真实浏览器与交互，且涉及合规风险；命中验证页的书源直接按失败处理。 |
| 15 | xGorgon 等客户端签名算法 | `已决定不做` | 依赖 App 私有算法与设备指纹，逆向与维护成本极高且不稳定；不做签名伪造。 |

> 统计口径：黑名单 **5 类**；FEATURE_MATRIX 170 条基线中可对应的仅音频类 6 条（模块 12），视频/音乐/游戏/正版音频各 0 条。

---

## 二、延后做（功能成熟后再排）

### 2.1 替换规则包的导入/导出与试跑预览（已关闭）

- 状态：`已完成`（后端）；**前端入口仍缺**
- 结论：`POST /api/replace-rules/export|import|preview` 已实现并有单测（[replace.rs](../server/src/routes/replace.rs)）；JSON 规则包往返、Legado 片段导入、预览截断均可用。原“尚未实现，仅提案”的描述作废。
- 剩余：`ReplaceRules.svelte` 没有导入/导出/试跑按钮，纯前端缺口；保留在 3.5 与 FEATURE_MATRIX B 类，不再作为后端待办。
- URL 订阅（远程规则包）仍未做，归入低优先，不影响当前闭环。

### 2.2 继承书源 JSON 自带的 `replaceRegex` / `sourceRegex`

- 状态：`已完成待验证`
- 说明（纠正原判断）：后端**已经执行**这两个字段，正文管线先 `sourceRegex` 再 `replaceRegex`（[web_book.rs](../server/src/web_book.rs#L280-L283)、[web_book.rs](../server/src/web_book.rs#L473-L505)）。
- 剩余范围（可延后）：
  - 只作用于正文，**不作用于搜索结果的标题/作者/简介**；
  - 不支持 `sourceRegex` 的动态 JS 段；
  - **全局内置净化开关已完成**（`builtin_clean`，默认关，设置页可开）；仍缺“按书源单独开关净化”。
- 为什么值得做：让 Legado 书源导入后更接近原版行为，减少“导入成功但正文带广告”的投诉。

### 2.3 书籍更新检查（`can_update` / `last_check_time`）

- 状态：`待做`（延后）
- 为什么：书架目前无法提示“有新章节”，这是阅读类产品的核心留存功能；但需要按书源写更新规则，先稳定解析管线。
- 依据：`Book.can_update` 由 `save_book_meta` 每次写成传入值（默认 `false`），没有更新检查任务或接口（[books.rs](../server/src/routes/books.rs#L367-L417)、[entities.rs](../server/src/entities.rs#L270-L280)）。
- 建议：新增 `POST /api/books/check-update`（批量传 `book_ids`），返回每本的 `latest_chapter_title` 与 `has_new`；结果写 `book_meta.last_check_time/last_check_count`。

### 2.4 分组高级字段

- 状态：`处理中`（部分完成）
- 已完成：`cover` / `show` 已支持局部更新（[API.md](../docs/API.md) 7.7），`name` / `order` / `book_sort` 可写。
- 仍缺：`enable_refresh` 无写入路径；分组管理的重命名/合并/批量转移仍缺。
- 依据：`book_groups` 字段见 [user/0003_stats_cache.sql](../server/migrations/user/0003_stats_cache.sql)；写入见 [bookshelf.rs](../server/src/routes/bookshelf.rs#L277-L311)。

### 2.5 公开设置（`public_settings`）

- 状态：`待确认`
- 为什么：`shared.public_settings` 表已建但没有读写接口；若产品需要“无需登录的站点公告/默认书源”，需要明确是否启用。
- 依据：shared 迁移建表，但各路由未引用（grep `public_settings` 在 `routes/` 无命中）。

### 2.6 EPUB / PDF 导出

- 状态：`延后`
- 为什么：Kindle 的替代路径之一是生成 EPUB 推送到设备，绕开浏览器能力限制（KPW3 上无 JS / 无 WebView 富文本）；PDF 的字体嵌入与版面成本更高，先不做。
- 排期：优先级**低于** `/k` 极简页（见 3.2.1）；前置依赖是正文分段（HTML → 段落）与图片处理（下载、内联、尺寸约束），这两项在正文管线里尚未定型。
- **TXT 导出已完成**（`/api/book/export/info` + `/api/book/export/txt`，见 [API.md](../docs/API.md) 6.10/6.11）；EPUB/PDF 仍延后，前置依赖是正文分段与图片处理。
- 依据：`/api/book/content` 返回文本/HTML 混合正文（见 [API.md](../docs/API.md) 6.3），当前没有 EPUB/PDF 打包依赖。

### 2.7 漫画链路（`处理中`）

- 状态：`处理中`（2026-11-03 范围收敛为“小说 + 漫画”后列为在办）
- 已有基础：后端 `apply_image_style` 已按 0/1/2/3 改写或删除 `<img>`（[replace.rs](../server/src/routes/replace.rs#L241-L274)）；目录/正文抓取、缓存、TXT 导出可复用；`is_vip`/`is_pay` 已解析。
- 还差：
  - 阅读器保留并渲染正文 HTML 中的 `<img>`（当前 `htmlToParas` 直接丢弃，见 3.8）；
  - 漫画阅读布局：单页/连续滚动、缩放、预加载；
  - 图片缓存与代理（外链 403/防盗链）；
  - 漫画书源类型识别与图集章节目录；
  - 漫画导出/下载（TXT 导出只覆盖文字）。
- 依据：[Reader.svelte](../server/web/src/pages/Reader.svelte#L198-L199)、[replace.rs](../server/src/routes/replace.rs#L241-L274)、[FEATURE_MATRIX.md](../docs/FEATURE_MATRIX.md) 模块 4 #17 / 模块 5 #17、#21。

---

## 三、已知缺陷与缺口

按严重程度排序。`P0` 上生产前必须处理，`P1` 影响核心阅读链路，`P2` 体验/一致性问题，`P3` 观察项。

### 3.1 P0：生产安全默认值

- 状态：`已修复`（2026-11-07，PR-1）
- 现象（修复前）：
  - 首次启动自动创建管理员 `admin/<寮€鍙戦粯璁ゅ彛浠?`（`BORANLIB_ADMIN_USER`/`BORANLIB_ADMIN_PASS` 可覆盖，兼容旧 `ADMIN_USER`/`ADMIN_PASS`/`LEGADO_*`）；
  - `BORANLIB_JWT_SECRET`（当前实际读取 `JWT_SECRET`）默认 `<寮€鍙戝崰浣嶅瘑閽?`；
  - CORS 使用 `CorsLayer::permissive()`，任意来源可带凭据访问。
- 现状：生产环境（release 或 `BORANLIB_ENV=production`）拒绝默认/过短 JWT secret 与缺失/弱管理员口令，直接非 0 退出；CORS 默认不添加层，只有显式 `BORANLIB_CORS_ORIGINS` 才放行精确来源；开发环境仅告警。
- 涉及：`bootstrap_admin`（[auth.rs](../server/src/auth.rs#L607-L642)）、[config.rs](../server/src/config.rs#L6-L65)、[main.rs](../server/src/main.rs#L30-L45)。

### 3.2 已完成与已修复（2026-11-03，勿再排期）

#### 3.2.1 `/k` 极简阅读页（`已完成`）

- 状态：`已完成`（原列“待做”，2026-11-03 改为已完成）
- 实现位置：[kindle.rs](../server/src/routes/kindle.rs)，路由 `/k`、`/k/toc`、`/k/read`、`/k/search`、`/k/login`、`/k/logout`、`/k/_selftest` 已在 [mod.rs](../server/src/routes/mod.rs) 合并。
- 形态：**无业务 JS、纯 SSR、`table` 布局**；仅注入三类诊断脚本（错误兜底 `K_GUARD_JS`、视口标定 `K_VP_JS`、自报探针 `K_PROBE_JS`，见 [kindle.rs](../server/src/routes/kindle.rs) 顶部说明），不改变阅读行为；Cookie 登录（不依赖 Bearer JWT）；打开阅读页即自动写阅读进度（服务端 upsert，幂等）。
- 字号分档：四档 `?fs=1/2/3/4`（小 28/46、标准 32/52、大 38/62、超小 20/36），也可写 `k_fs` Cookie。
- 真机约束来源（Kindle Paperwhite 3）：屏幕 1072×1268、DPR 1.7964 但**浏览器不做缩放**；不支持 `flex` / `grid` / `vw` / `vh` 单位 / `calc`；`border-radius` 未用；**阅读页贴底元素用 `position:absolute;bottom:0`（相对 body），不用 `fixed`**——老 Silk 把 `fixed` 锚在布局视口（1268），比真实可见高（约 1180）大，底栏会掉出可见区；触摸事件**未证实**，故只用 `click`。视口可用 `?vw=&vh=` 参数化（缺参回落 1072×1268），`vh` 由 `<head>` 内联 ES5 标定真实可见高后（Cookie `k_vh`）带参重载。
- 定位：`/k` 是 EPUB 导出（见 2.6）的低成本替代路径，先保证“能读”，再考虑推送格式。

#### 3.2.2 已修复缺陷（`已修复`，2026-11-03）

| # | 原描述（保留一行） | 修复方式 |
|---|---|---|
| 1 | 书架列表 500：无进度记录时 `COALESCE(rp.position,0)` 返回 INTEGER，sqlx 按 f64 解码报 `mismatched types` | 列表 SQL 改为 `CAST(COALESCE(rp.position,0) AS REAL)`，并抽出 `progress_percent()` 统一口径（[bookshelf.rs](../server/src/routes/bookshelf.rs#L76-L95)、[bookshelf.rs](../server/src/routes/bookshelf.rs#L336-L349)） |
| 2 | `nextTocUrl` 返回字面量 `"null"`，被当 URL 拼成 `/toc/null` 并反复追加目录 | 新增 `blank_value()` 拦截 `""` / `"null"` / `"None"` / `"undefined"`，命中即停止翻页（[web_book.rs](../server/src/web_book.rs#L38-L42)） |
| 3 | 设置接口零校验：`/api/settings` 任意键任意值直接入库 | `validate_settings()`：`theme` ∈ light/dark/sepia、字号 12–30、行距 1.2–2.6、`image_style` 0–3、`cache_limit_mb` 4–512，越界 400（[replace.rs](../server/src/routes/replace.rs#L171-L215)） |
| 4 | 进度读回丢 `total_chapters` / `percent`：GET 只回 `chapter_index` 与 `position` | 补 `total_chapters`（`book_meta` → `book_stats` 回退）与 `percent`（含章内比例）（[bookshelf.rs](../server/src/routes/bookshelf.rs#L362-L406)） |
| 5 | 替换规则按书名失效 / `book_url` 为空：正文接口只带 `chapter_url` 时书名作用域匹配不到 | `book_url` 三级回填（缓存 → 请求参数 → `book_chapters` 反查），`title` 同链路；仍空则 400 `missing_book_url`（[books.rs](../server/src/routes/books.rs#L306-L342)） |
| 6 | `@js:` 规则递归爆栈：JS 段递归/死循环无上限，单条规则可打穿请求线程 | boa 运行时设 `recursion_limit=100`、`loop_iteration_limit=500_000`、`stack_size_limit=512`（KB）（[rule_engine.rs](../server/src/rule_engine.rs#L26-L53)） |
| 7 | 初始选择器不支持 `tag.a`：`tag.` 前缀被当普通 CSS 标签，选不到元素 | 识别 `tag.` 前缀并按标签名选择（[rule_engine.rs](../server/src/rule_engine.rs#L345-L390)） |
| 8 | B1：相对 `searchUrl` 未归并，`/search?q=` 这类地址直接请求失败 | 统一用 `resolve_url(base, maybe)` 归并为绝对 URL（[web_book.rs](../server/src/web_book.rs#L44-L52)） |
| 9 | B2：POST 有 body 但源未声明 Content-Type，站点不解析请求体（搜索空结果） | `ensure_form_content_type()` 仅在 POST 且缺头时补 `application/x-www-form-urlencoded`（[req.rs](../server/src/req.rs#L96-L150)） |
| 10 | B3：`!下标` 不支持，`!0`/`!1` 取第 n 个节点无效 | 遍历标记支持 `!n`，越界返回空（[rule_engine.rs](../server/src/rule_engine.rs#L388-L390)） |
| 11 | B4：裸 `text` / `href` 被当标签选择器，章节标题/链接解析成空 | 无选择器前缀时按“取当前节点属性”处理（[rule_engine.rs](../server/src/rule_engine.rs#L345-L349)） |
| 12 | B5：`@tag.x` 只查直接子元素，取不到后代节点 | 遍历标记改为后代查询（[rule_engine.rs](../server/src/rule_engine.rs#L386-L390)） |
| 13 | `reading_progress` 忽略章内 `position`；`position` 双量纲；`book_url` 为空直接 400 | 只接受 `0..1`（越界 400）；`percent` 含章内比例并钳制 0..100；`book_url` 回填失败 400 `missing_book_url`（[bookshelf.rs](../server/src/routes/bookshelf.rs#L336-L360)） |
| 14 | 两段式搜索 job 淘汰顺序错误：满 8 个后淘汰了最新任务 | 改为按 `elapsed` **淘汰最旧**；`:3000` 实测 9 个 job：第 1 个 404、第 2–9 个 200（2026-11-03 15:00 修复）。当前上限已改为已完成 32 / 进行中 64，旧 8 上限作废 |

以上均于 **2026-11-03** 修复并回归；`/k` 与规则引擎的验证方式同步记录在 [FEATURE_MATRIX.md](../docs/FEATURE_MATRIX.md) 模块 1 / 模块 4。

#### 3.2.3 本轮已落地功能（2026-11-03 15:00–15:16）

- 导出 TXT：`GET /api/book/export/info` + `GET /api/book/export/txt`，前端 `ExportDialog.svelte` 书架/阅读器入口已接。
- 两段式搜索：`q`/`key` 别名、`source_ids` 过滤且前端 `Discover.svelte` 已传参；job 淘汰改为最旧（见上表 #14）。
- 内置净化开关：`builtin_clean`（默认 `false`，设置页可开）。
- 静态托管：`BORANLIB_WEB_DIR`（兼容 `LEGADO_WEB_DIR`；`:3000` 为 `<repo>/server/web/dist`）。
- `is_pay` 解析：由 `ruleToc.isPay` 解析并落库，不再是恒 `false`。

### 3.3 P1：`GET /api/book/info` 覆盖用户在 `book_meta` 的定制字段

- 状态：`待做`
- 现象：`save_book_meta` 的 `ON CONFLICT DO UPDATE` 无条件覆盖 `"group"`、`custom_tag`、`custom_cover_url`、`custom_intro`、`can_update` 等字段，而 `/api/book/info` 传入的是解析出的 `Book::default()` 值（多为 `None`/`false`）。
- 影响：用户设置的书分组与自定义封面/简介会被一次详情刷新清空；`can_update` 永远为 `false`。
- 涉及：[books.rs](../server/src/routes/books.rs#L367-L417)。
- 建议：更新语句对用户字段使用 `COALESCE(excluded.x, book_meta.x)`；`can_update/last_check_*` 仅在更新检查任务里写。
- 为什么：这是静默数据丢失，用户很难察觉原因。

### 3.4 P1：统计前端已接入（已关闭）

- 状态：`已完成`
- 结论：`api.js` 已有 `recordReading`/`statsOverview`/`shelfStats`；[Reader.svelte](../server/web/src/pages/Reader.svelte#L231) 定时上报，[Stats.svelte](../server/web/src/pages/Stats.svelte#L21) 消费 `/api/stats/overview?days=7|30`。原“前端完全未接入”作废。
- 剩余：`/api/bookshelf/stats` 仍只有 `api.js` 封装、书架页未展示；另缺“启用记录”开关（模块 10 #3）。

### 3.5 P1：页面入口仍缺（api.js 已封装，UI 未接）

- 状态：`处理中`
- 已接线：`sources/enabled`、`sources/validate`、`bookshelf/group`、`bookshelf/groups`、`bookshelf/stats`、`book/switch-source`、`stats/reading`、`stats/overview` 均已在 `api.js` 有封装；统计已有页面消费（见 3.4）。
- 仍缺页面入口：
  - 书源页：校验按钮、检查更新按钮与结果列表；
  - 规则页：导入/导出/试跑预览按钮；
  - 设置/账号页：用户管理列表、禁用/删除、改密入口；
  - 书架页：多选批量操作、更新目录入口、`/bookshelf/stats` 展示。
- 影响：后端能力“存在但不可达”，用户会认为功能缺失；也容易让下一个 agent 误判为“未实现”而重复开发。
- 依据：`api.js` 封装齐全；FEATURE_MATRIX §11 的 B 类 20 项即本类口径。

### 3.6 P2：`auto_refresh` 设置无消费方

- 状态：`已解决`（v0.3.9 `fix/settings-truth`）
- 结论：`auto_refresh` 是**用户级设置**（非书源字段），Legado 语义为“打开软件时自动更新书籍”。已在 `Bookshelf.svelte` 消费：打开书架时若为 true，且本机距上次自动刷新超过 24h，则后台调用 `/api/book/toc/batch`，只刷新 `enable_update` 未关闭的书；`Settings.svelte` 文案同步为真实语义。
- 为什么选实现而不是移除：`toc/batch` 与 `enable_update` 基础设施已存在，成本低；语义与书源规则约定一致，对用户有实际价值；不触碰搜索调度/排序。
- 回归：`replace.rs::auto_refresh_has_a_real_consumer` 断言设置默认值、前端消费点与 24h 节流键仍存在。

### 3.7 P2：`timeout_millisecond` 存库但未生效

- 状态：`已解决/已澄清`（v0.3.9 `fix/settings-truth`）
- 结论：该字段属于 **`ReplaceRule`（替换规则）**，不是书源字段。Rust `regex` 为线性时间、无灾难性回溯，因此不需要超时执行；字段保留仅用于 Legado 导入导出兼容，文档已明确“执行时不消费”。
- 书源级 HTTP 超时是另一个需求：已新增 **`BookSource.timeout_millisecond`**（oread 扩展，存于 `book_sources.content` JSON，无需迁移），夹取 1000–60000ms，0/缺失/非法回退全局 `HTTP_TIMEOUT_SECS`，并接入搜索/详情/目录/正文/图片五条抓取链路。
- 回归：`req.rs` 的解析/夹取/回退/实际请求超时测试，`web_book.rs` 四类抓取测试，`image.rs::image_budget_uses_source_timeout_or_image_default`。

### 3.8 P2：正文 HTML/图片的渲染口径不一致（漫画链路的关键缺口）

- 状态：`处理中`（漫画纳入范围后从“待确认”上调）
- 现象：规则引擎默认抽取返回 `inner_html()`（[rule_engine.rs](../server/src/rule_engine.rs#L378-L426)），后端会按 `image_style` 改写 `<img>`（[replace.rs](../server/src/routes/replace.rs#L241-L274)）；但阅读器把正文按 `\n` 切段、以纯文本渲染（[Reader.svelte](../server/web/src/pages/Reader.svelte#L198-L199)、[Reader.svelte](../server/web/src/pages/Reader.svelte#L784-L786)），没有 `{@html}` 或图片渲染。
- 影响：若书源规则返回 HTML，页面会直接显示标签文本；`image_style` 的三种模式用户看不到差异；图片一律不显示。
- 建议：明确正文格式契约（纯文本 or 受控 HTML）；若保留 HTML，前端用白名单 sanitize 后 `{@html}` 渲染；若走纯文本，服务端统一去标签并把图片转成占位文案。漫画链路（2.7）依赖此处的图片渲染结论。
- 为什么：这决定“正文长什么样”，不确认会让前后端各自猜；漫画能否落地也取决于它。

### 3.9 P2：缓存键不含书源（`origin`）

- 状态：`待确认`
- 现象：目录缓存按 `book_url`、正文缓存按 `chapter_url`，都不带 `source`（[books.rs](../server/src/routes/books.rs#L272-L339)）。
- 影响：两个书源共用同一 URL（镜像站/同源转发）时会串目录/正文；换源后旧缓存也不会主动清理。
- 建议：缓存键加 `origin`，或在换源成功时清理该书的 `book_chapters/book_content`。
- 为什么：影响面取决于书源生态，但一旦踩中就是“换源后内容还是旧的”，很难排查。

### 3.10 P3：`GET /api/bookshelf` 的 `book_id` 参数被忽略

- 状态：`待确认`
- 现象：handler 声明了 `book_id` 但显式忽略，始终返回全量书架（[bookshelf.rs](../server/src/routes/bookshelf.rs#L82-L101)）。
- 影响：前端若按“单书查询”使用会拿到整表；契约与实现不一致。
- 建议：要么实现 `WHERE book_id=?` 的单书分支，要么从查询参数中删除并在契约里注明“不支持单书查询”。
- 为什么：小问题，但属于契约与代码不一致，测试会误报。

### 3.11 P3：首次启动偶发 SQLite `disk I/O error (4618)`

- 状态：`部分完成（根因待复现）`
- 现象：首次启动偶发失败，清理 `shared.db-wal/-shm` 后恢复；源码没有对应处理或重试。
- 影响：新设备首次启动可能直接起不来，表现为“服务不可用”而非“数据库损坏”，排查成本高。
- 涉及：[db.rs](../server/src/db.rs)（连接池与 PRAGMA 初始化）、[db_guard.rs](../server/src/db_guard.rs)（迁移重试与错误翻译）、启动脚本。
- 现状：启动时对 `DATA_DIR` 与 `users/` 做可写探测（失败时提示磁盘/权限/只读挂载/杀软）；连接池已设 5s `busy_timeout`；迁移对 `IOERR(10)` / `BUSY/LOCKED(5/6)` 做 3 次指数退避重试；磁盘满/只读/损坏均翻译成“原因 + 下一步”。
- 仍建议：复现时保留 `*-wal/-shm` 与完整启动日志；若确认 4618 与残留 `-shm` 强相关，再补“关闭连接 → 清理 -shm → 重开”的受控恢复。
- 为什么：这是唯一可能导致“开箱即坏”的稳定性问题，但没有稳定复现前不宜盲改。

### 3.12 已修复/已实现，待验证（不要重复排期）

| 项 | 状态 | 现状与依据 | 验证要点 |
|---|---|---|---|
| `GET /api/sources/detail` 曾读错列返回 400 | `已完成待验证` | 读取改为先 `content` 再 `rules`，写入同时写两列（[sources.rs](../server/src/routes/sources.rs#L32-L40)、[sources.rs](../server/src/routes/sources.rs#L105-L112)） | 导入一个只写 `rules` 的旧库数据，再调 detail 应能返回；新导入也应同时具备两列 |
| 聚合搜索分页语义错误 | `已完成待验证` | `/api/search` 与 `/api/search/aggregate` 共用 `aggregated()`：各源只抓第 1 页、去重截断 400、60s 缓存、本地切片（[books.rs](../server/src/routes/books.rs#L103-L205)） | 同一 `key` 连续翻 1/2/3 页无重复、无缺书；`total` 与实际可翻条数一致 |
| `bookshelf` 缺 `group` 字段与“移入分组”接口 | `已完成待验证` | 迁移已加 `"group"`；`POST /api/bookshelf/group` 支持 `book_id`/`book_ids`，空 group 移出（[bookshelf.rs](../server/src/routes/bookshelf.rs#L18-L24)、[bookshelf.rs](../server/src/routes/bookshelf.rs#L157-L186)） | 单本/批量移入、移出、书架列表分组筛选；`book_meta.group` 同步 |
| 阅读统计表已建但无路由 | `已完成待验证` | `/api/stats/reading`、`/api/stats/overview` 已实现并注册（[stats.rs](../server/src/routes/stats.rs)、[mod.rs](../server/src/routes/mod.rs)） | 上报后 `reading_stats` 累加、`book_stats.read_chapters` 取最大值、streak 跨天逻辑；前端接入见 3.4 |
| `cache_limit_mb` 未按用户生效 | `已完成待验证` | `GET /api/book/content` 保存新缓存时用 `user_cache_limit_mb` 与全局上限取 min（[books.rs](../server/src/routes/books.rs#L312-L317)、[replace.rs](../server/src/routes/replace.rs#L218-L228)） | 改小上限后新缓存是否按新值淘汰；已有缓存不会回溯淘汰（见下方“剩余缺口”） |
| `image_style` 未处理图片 | `已完成待验证` | 后端 `apply_image_style` 已按 0/1/2/3 改写或删除 `<img>`（[replace.rs](../server/src/routes/replace.rs#L241-L274)） | 需前端实际渲染图片才能看到效果，见 3.8 |
| `read_mode=paged` 无分页数据 | `已完成待验证` | 前端分页模式用 CSS 多列 + `scrollLeft` 计算，已有 `pageIndex/pageCount` 与底栏（[Reader.svelte](../server/web/src/pages/Reader.svelte#L198-L210)、[Reader.svelte](../server/web/src/pages/Reader.svelte#L799-L805)） | 切换 `scroll/paged/fade` 后页码、进度 0..1 还原、窗口缩放后重新测列数 |
| 构建脚本硬编码旧路径 | `已完成待验证` | 两个脚本都以 `$PSScriptRoot` 推导仓库根，并通过 `CARGO_TARGET_DIR` 输出（[build-dev.ps1](../scripts/build-dev.ps1#L28-L45)、[build-release.ps1](../scripts/build-release.ps1#L33-L45)） | 在仓库根与任意工作目录调用都应成功；当前脚本变量为 `OREAD_TARGET_WIN`/`OREAD_TARGET_MUSL`，工程侧改名后为 `BORANLIB_TARGET_*`（本文以现名为准） |

缓存清理已落地（`POST /api/book/cache/clear`，支持单书/全库）；`cache_limit_mb` 仍只在写入新缓存时参与淘汰，没有“立即清理到上限”的入口，但不再缺“清空缓存”入口（[health.rs](../server/src/routes/health.rs)、[books.rs](../server/src/routes/books.rs)）。

### 3.13 KPW3 真机约束：`/k` 与漫画阅读器必须遵守红线

- 状态：`处理中`（约束已固化，代码尚未逐项对齐）
- 必须遵守：布局/视觉不用 `flex/grid/vw/vh/calc/clamp/sticky/transform/transition/object-fit`；不做动画（无 rAF）；图片单张 ≤100KB、预取 2–3 页。完整事实表与证据见 [KPW3.md](../docs/KPW3.md)。
- 已知坑：**设备时区 UTC（`offset=0`）**，用户可见日期必须服务端格式化，客户端不得用 `new Date()` 本地拼接。
- 适用范围：`/k` 电子书阅读器、漫画阅读器（2.7）、以及所有以 KPW3 为目标的页面。
- 依据：[KPW3.md](../docs/KPW3.md)、真机 run `<local-tools>\kpw3-probe\reports\20261103-172026-B4YFGB.json`（探针 v2.1.0；网络实测 313KB/s，单张图仍按 ≤100KB、预取 2–3 页控制）。

### 3.14 P3：书源保存走 `POST /api/sources` 覆盖语义，无并发保护

- 状态：`待做`
- 现象：没有 `PUT`/`PATCH`，保存统一走 `POST /api/sources`，按 `book_source_url` upsert 并整份 JSON 覆盖；没有版本号、`If-Match` 或字段级合并。
- 影响：两个标签页或两个管理员同时编辑同一书源时，后提交者会静默覆盖前者，丢改动且无提示。
- 建议：前端禁止同一书源双开，或提交前先拉取最新详情并提示合并；后端后续可按需加 `updated_at` 乐观锁，不阻塞当前发布。
- 为什么：单人使用概率低，但多人维护同一源时很难排查；先把契约事实写清，避免被当成 PUT 语义。

### 3.15 P3：SPA 前端体积观察（首屏 85.77 kB 已回落至 90 kB 线内）

- 状态：`观察中`
- 现状：首屏 JS gzip **85.77 kB**（主包 228.87 kB raw）、CSS 主包 gzip **6.79 kB**；编辑表单按需 chunk 为 JS **10.50 kB**、CSS **1.05 kB**（均 gzip），全量合计 **96.27 kB**。
- 影响：首屏已回落至 90 kB 线内，按需 chunk 不拖首屏；“超过警戒线”的说法不再成立，全量 96.27 kB 仅作观察值。后续接漫画阅读器仍会增加包体。
- 建议：守住**首屏 JS ≤90 kB**；漫画模块继续按需加载，并可作为 CI 体积检查项。
- 为什么：当前不构成首屏风险，但漫画是下一阶段主要增量，需要持续观察。

### 3.16 P2：musl 解析器对 AAAA=NXDOMAIN 的误判（待真实用户场景触发）

- 状态：`待做`（建议已写，未实现）
- 现象：静态 musl 产物在「IPv6 查询被解析器错误回答为 NXDOMAIN」的网络后面，会把整次解析直接判失败（`EAI_NONAME`），而 glibc 会容忍 AAAA 缺失继续用 A 记录。已在 OpenStick 上实测复现：内网 DNS 返回 `rcode=3`，换公共 DNS `223.5.5.5` 返回 `rcode=0`。
- 影响：设备所在网络若存在这类误答的 DNS，服务会表现为「所有书源都连不上」，但换网络即恢复，现场极易误判为书源失效。
- 建议（未实现）：请求层对 `EAI_NONAME` 做一次重试（或显式只查 A 记录），或允许配置自定义解析器 / DoH。
- 为什么：只在特定 DNS 环境下触发，且现象与「书源挂了」高度混淆；先记录复现证据与判定方法，避免下次重复排查。

### 3.17 P1：运行时书源健康状态 + 导入账本（已完成，2026-11-04）

- 状态：`已完成并实测`（代码 + 5 个接口 + 迁移；**前端 UI 未做**，仅后端能力）
- 能力：
  - 健康表 `source_health`（主键 `bookSourceUrl`，不按域名）记录 `last_status`/`last_error`/`consecutive_failures`/`last_ok_at`/`cooldown_until`/`avg_latency_ms`；连续 3 次**站点级**失败进入冷却（默认 24h，`SOURCE_COOLDOWN_SECS` 覆盖）。
  - 搜索默认跳过冷却源与隔离区源；`/api/search/start|result` 返回 `skipped_cooldown`/`cooldown_total`，`include_cooled=1` 可越权全打。
  - `FAIL_ENGINE`（选择器/JSONPath/JS）不计连续失败、不冷却、不进隔离区——引擎缺陷不能把可用源判死。
  - 导入账本 `import_batches`/`import_items`/`source_library_state`：记录批次、原始定义快照、规则哈希、`decision(active/quarantined)`、`review_state(update_rejected/conflict)`；隔离区保留原始定义，同合集复检通过可复活。
  - 覆盖三态：新版通过→覆盖；新版站点失败且本地可用→`update_rejected`；本地人工改过→`conflict`；`force=1` 显式越权。先比整份合集哈希，未变则零探测。
  - 导出排除隔离区（避免分发源内认证头）；`prune` 只按源条目隔离/禁用，支持预览+`confirm=1`，引擎性失败永久排除。
- 实测（隔离端口 `:3061`，复制数据目录）：3 个死源连续 3 次失败进入冷却；默认搜索 1.24s / 0 错误 / `skipped_cooldown=2`，`include_cooled=1` 时 2.47s / 多 26 本书；107 条合集首次 `added=23/unchanged=84`、二次 `probe=0`、复活与三态均通过；`cargo test` 默认线程 186 passed。
- 未做：`conflict` / `update_rejected` 的前端交互、回滚快照 UI、账本查询接口（表已在写，未暴露 `GET /api/sources/imports`）。

### 3.18 编译期性能旋钮（`opt-level` / `target-cpu`）暂不启用

- 状态：`暂缓`（用户 2026-11-04 明确"先不动"）
- 背景：当前 `opt-level="z"` + 通用基线，是为 OpenStick（474 MiB 内存、libc 不确定）选的体积优先方案；这套选择仍然成立。
- 结论依据：设备端搜索 18–20 s 由网络串行 + 10 s 单源超时主导，编译优化在这个场景里是噪声级；真实 CPU 热点（CSS/JSONPath/正则/图片转码）为毫秒级。
- 重启条件：先有可复现的压测基准（固定源集 + 固定关键词 + 固定网络条件），再谈。
- 若做，做法：保留 baseline 资产；另发 `x86_64` fast 变体（`opt-level=3` + `target-cpu=x86-64-v2`）并按文件名区分；aarch64 不动（Cortex-A53 是 ARMv8-A 基线，收益 1–3%）；PGO 收益更大（约 10–20%）但 CI 时间翻倍，最后考虑。

---

## 四、工程与长期改进

| # | 项 | 状态 | 现状/依据 | 为什么值得做 |
|---|---|---|---|---|
| 1 | 拆 Rust workspace | `待做` | 目前只有 `server/Cargo.toml` 单 crate（[Cargo.toml](../server/Cargo.toml)），`rule_engine`/`entities` 与路由同包，改动一处会触发整包重编 | 把 `rule_engine`、`entities` 抽成基础 crate，`sources/books/bookshelf/stats` 各自成 crate；多 agent 并行时减少冲突，增量编译更快 |
| 2 | release 构建可选 thin LTO | `已完成待验证` | `build-release.ps1 -ThinLto` 用 `CARGO_PROFILE_RELEASE_LTO=thin` + `codegen-units=16` 覆盖，不改 `Cargo.toml`（[build-release.ps1](../scripts/build-release.ps1#L131-L137)） | 迭代构建从 fat LTO 的分钟级降下来；发布包仍可用默认 fat LTO |
| 3 | 统一 `CARGO_TARGET_DIR` | `已完成待验证` | 脚本默认 `<local-tools>\oread-target-win` / `<local-tools>\cargo-target-musl`，支持 `OREAD_TARGET_WIN`/`OREAD_TARGET_MUSL` 覆盖（工程侧改名后为 `BORANLIB_TARGET_*`；[build-dev.ps1](../scripts/build-dev.ps1#L14-L30)、[build-release.ps1](../scripts/build-release.ps1#L16-L35)） | 避免不同 agent 各自 `target/` 造成重复全量编译与磁盘膨胀 |
| 4 | Windows Defender 排除 `target` 目录 | `待确认` | 属机器环境设置，仓库内无法验证；未在源码/脚本中看到自动配置 | 实时扫描会把编译产物 I/O 放大数倍；排除后可显著缩短构建时间（需在开发机手动操作，不入库） |
| 5 | CI（`cargo check` / `cargo test`） | `已完成待验证` | 已有 `.github/workflows/ci.yml`，在 `server` 下跑 `cargo check --all-targets` 与 `cargo test`（[ci.yml](../.github/workflows/ci.yml)） | 防止跨 agent 改动把主干编译搞坏；当前缺少前端构建与接口冒烟测试，可后续追加 |
| 6 | 资源监控（RSS / 写入量） | `待做` | `/api/healthz` 公开、`/healthz/user` 需登录且带缓存统计（[health.rs](../server/src/routes/health.rs)），但没有进程级 RSS、磁盘写入、请求耗时的持续采集 | 设备内存有限，SQLite WAL + 正文缓存会持续增长；有指标才能定位“越跑越卡” |
| 7 | eMMC 写入量长期观察 | `待做` | 设备侧指标，仓库内无采集代码 | WAL + 频繁 progress upsert 会放大写入；长期观察可决定是否降低 checkpoint 频率或合并进度写入 |
| 8 | 目录/正文缓存的可观测性 | `待做` | 仅有 `healthz_user` 的 `cache{bytes,mb,rows}`；缺少命中率、淘汰次数、按书占用 | 调 `CONTENT_TTL_SECS`/`cache_limit_mb` 时需要数据支撑，否则只能拍脑袋 |

---

## 五、真实数据暴露的环境事实（不是我们的 bug，但必须知道）

> 以下都是外部书源/站点的行为，**不是解析器或接口缺陷**。记录在此，避免下次被当成回归项反复排查。

| 事实 | 具体表现 | 处理方式 |
|---|---|---|
| 24 个真实书源里 **11 个报 `builder error`** | 请求构造阶段就失败（缺必填字段、URL 模板不合法等）；聚合搜索会计入 `source_failed`，原文在 `errors[]` | 属**源端配置问题**，不修改解析器；导入/校验时应提示源本身不可用 |
| 多数站点校验 `Referer`，封面直链 403 | 直接请求封面 URL 返回 403 | 前端已用 `referrerpolicy="no-referrer"` + 书名占位，不强行追封面 |
| 三八小说搜索接口已失效 | 搜索返回错误页/空结果 | 该源应禁用或换源，不作为解析能力判据 |
| 顶点小说部分关键词返回空 `<ul>` | 列表容器存在但没有任何条目 | 源站按关键词降级/风控，非解析 bug |

---

## 附：与 API.md 的对应关系

- 接口契约、字段可空性、示例 JSON：见 [API.md](../docs/API.md)。
- 仍缺的接口：书籍更新检查（2.3）、分组管理扩展（2.4）、漫画图片代理与导出（2.7）、统计启用开关（模块 10 #3）。规则预览与缓存清理已实现，不再列入“仅提案”。
- 源码无法确认的条目：见 [API.md](../docs/API.md) 第 14 节。本清单不再重复，只保留与排期相关的项。

*本文件是活文档：完成一项就把状态改成 `已完成待验证`；验证通过后从待办区移走并记入对应章节的“已完成”记录。品牌与部署口径统一为 Boranlib；接口路径、字段名与环境变量实际值不随改名改动。*
