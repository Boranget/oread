# Boranlib API 契约文档

> 本文档是前端、测试、后端共同遵守的接口契约。所有条目均以**源码实际状态**为准，不从产品愿望反推。

## 命名与部署口径（Boranlib）

- 产品/服务名：**Boranlib**；历史文档里的 `oread` 均指本项目，已统一改名。
- 二进制：`boranlib`（Windows `boranlib.exe`）；部署目录 `/opt/boranlib`。
- 环境变量：统一前缀 `BORANLIB_*`，并兼容旧 `LEGADO_*` 前缀（新名优先）。程序**只读取这两个前缀**，不读取未加前缀的旧名（例如只设 `WEB_DIR` 不生效，应设 `BORANLIB_WEB_DIR` 或 `LEGADO_WEB_DIR`）。程序内置端口缺省为 **8080**（`BORANLIB_PORT` / 旧 `LEGADO_PORT`）；但 **Windows 成品包**（v0.3.12 实测）的 `start.ps1` 在未显式配置时会注入 `BORANLIB_PORT=3000`，因此成品包默认实际监听 **`0.0.0.0:3000`**，只有直接运行二进制才落回 8080。15.4 表同时列出“新名 / 兼容旧名”。
- 书源 JSON 里的 `Legado` 指**上游兼容格式**，不是本项目品牌，保留不改。
- 仓库路径 `<repo>` 与 `../...` 链接是磁盘既有路径，未随品牌改名，保持原样。

## 生成时间与依据

- 生成时间：2026-11-02 19:34（Asia/Shanghai，UTC+8）；最后对齐：2026-11-03 15:16（第四轮复核）
- 仓库：`<repo>`
- Git：`HEAD = 7dbe6f14`（2026-11-02 16:31:55 +0800），工作区存在大量未提交改动，因此**以磁盘上读取到的源码为准**
- 读取范围与文件修改时间（第四轮复核后更新）：

| 依据文件 | 文件修改时间 | 用途 |
|---|---|---|
| `server/src/routes/mod.rs` | 2026-11-03 14:09 | 路由注册、公开路径、全局中间件 |
| `server/src/routes/sources.rs` | 2026-11-03 09:10 | 书源接口、书源更新检查 |
| `server/src/routes/books.rs` | 2026-11-03 14:43 | 搜索/两段式搜索 job（`q`/`key` 别名、`source_ids` 过滤/淘汰修复）/发现/详情/目录/目录批量/正文/书内搜索/整本缓存/缓存清理/换源 |
| `server/src/routes/export.rs` | 2026-11-03 14:08 | 导出信息与 TXT 流式导出 |
| `server/src/main.rs`、`server/src/config.rs` | 2026-11-03 14:43 | `BORANLIB_WEB_DIR`（兼容 `LEGADO_WEB_DIR`）静态资源目录、环境变量默认值 |
| `server/src/routes/bookshelf.rs` | 2026-11-03 10:48 | 书架/分组/批量操作/进度 |
| `server/src/routes/bookmarks.rs` | 2026-11-03 09:35 | 书签 CRUD |
| `server/src/routes/replace.rs` | 2026-11-03 11:00 | 替换规则 CRUD、导入/导出/预览、设置 |
| `server/src/routes/stats.rs` | 2026-11-03 08:37 | 阅读统计 |
| `server/src/routes/health.rs` | 2026-11-02 19:15 | 健康检查（未跟踪） |
| `server/src/auth.rs` | 2026-11-03 10:49 | 注册/登录/改密/管理员用户管理/JWT/令牌版本 |
| `server/src/entities.rs` | 2026-11-03 10:41 | 书源/书籍/章节（含 `update_time`/`word_count`/`volume_title`）/正文/替换规则结构 |
| `server/src/models.rs` | 2026-11-02 19:24 | 书源列表项、书架项 |
| `server/src/serde_util.rs` | 2026-11-02 18:42 | camelCase/snake_case 双向转换 |
| `server/src/error.rs` | 2026-11-02 17:11 | 统一错误体与状态码（含 504 `Timeout`） |
| `server/src/web_book.rs` | 2026-11-03 09:07 | 抓取/解析/去重/排序/聚合行为、模板变量、目录多页、分卷行 URL |
| `server/src/rule_engine.rs` | 2026-11-03 13:19 | 规则引擎、`@js` 内置与 DOM 桥、`##` 替换 |
| `server/src/config.rs`、`server/src/req.rs` | 2026-11-03 13:07 | 环境变量默认值、出站 HTTP 并发与搜索预算 |
| `server/migrations/shared/*`、`migrations/user/*` | 2026-11-02 19:00 ~ 2026-11-03 09:25 | 两张库的表结构；`user/0004_bookmarks_settings.sql` 新增书签表与 `page_width`/`font_family` 默认值 |
| `server/web/src/lib/api.js` | 工作区已改 | 前端现有调用方式（仅作交叉参考） |

**重要提醒**：代码可能正在被其他 agent 修改。若接口与本文不一致，以重新读取源码后的结果为准，并及时更新本文。

**20:05 复核记录（2026-11-02）**：`books.rs` 于 19:55 更新，`/api/search` 与 `/api/search/aggregate` 已共用 `aggregated()`（各源只抓第 1 页 + 本地切片 + 60s 缓存）。本文 2.3、5.1、14#8 已同步修订；原先的“双重分页不可靠”结论作废。

**09:20 复核记录（2026-11-03）**：按最新磁盘源码补齐 4 个新接口，并更新 5 处既有行为（`@js` 抛错、模板与目录多页、`##` 捕获组、分组字段、统计 `days`），接口总数由 39 增至 **43**。

| 变更 | 落点 | 验证状态 |
|---|---|---|
| 新增 `GET /api/replace-rules/export` | 8.3 | **已验证**（回归 B4：非空导出、camelCase、无本地 id、`?id` 单条/404） |
| 新增 `POST /api/replace-rules/import` | 8.4 | **已验证**（回归 B4：非空导入 `inserted=1`、非法结构 400） |
| 新增 `POST /api/replace-rules/preview` | 8.5 | **已验证**（回归 B4：替换/坏正则/私网拒绝）；5 MiB/15s 预算未验证 |
| 新增 `POST /api/sources/check-updates` | 4.9 | **已验证**（回归 B5：四类差异各 1、只读、空数组） |
| `@js` 未支持内置改为抛错 | 2.7、6.4 | 未验证（无单测） |
| 搜索/目录模板支持 `${page}`/`${key}`；`nextTocUrl` 多页 | 2.7、5.1 | 仅单测（`${page}`、多页链接）；`${key}` 未验证 |
| `##正则##` 支持 `$1` | 2.7、8.7 | 仅单测 |
| 分组 `cover`/`show` 局部更新 | 7.7 | 仅单测（COALESCE） |
| `GET /api/stats/overview` 的 `days` 语义 | 10.2 | 仅单测（两条 SQL） |

> 09:20 表中 #40~#43（export/import/preview/check-updates）已在下方 **11:08 回归（`report-20261103-1108.md`）**中验证到 HTTP 层，状态以回归记录为准。

**11:00 复核记录（2026-11-03，第二轮）**：补齐 11 个已注册但文档缺失的接口，并修订账号鉴权、目录字段、设置校验等既有条目。接口总数由 43 增至 **54**（不含 `/k/*` 的 8 个 HTML 路由）。

| 变更 | 落点 | 验证状态 |
|---|---|---|
| 新增 `POST /api/auth/password` | 3.3 | **已验证**（回归 7/7：成功改密、`relogin_required=true`、旧 token 401、旧密码 401、新密码 200、错误旧密码/过短 400） |
| 新增 `GET /api/admin/users` | 3.4 | **已验证**（回归 7/7：4 个账号、普通用户 403） |
| 新增 `POST /api/admin/users/status` | 3.5 | **已验证**（回归 7/7：禁用后旧 JWT 401、启用后旧 JWT 仍 401、重新登录成功、禁自己 400）；最后管理员保护仅单测 |
| JWT 增加 `ver`，禁用/改密后旧令牌立即失效 | 1.2、3.2 | **已验证**（回归 B8：禁用/启用/改密后旧 JWT 立即 401，旧密码失效、新密码可登录） |
| 新增 `POST /api/book/toc/batch` | 6.3 | **已验证**（回归 3/3：成功抓取、混合批 `updated=2 skipped=2`、失败隔离、别名）；并发 3 未被直接观测 |
| `GET /api/book/toc` 新增 `update_time`/`word_count`/`volume_title`，`is_volume`/`is_vip` 有真实值 | 6.2 | **已验证**（HTTP + 回归：200 响应含三字段）；分卷行真实数据渲染仅单测 |
| 分卷行 URL `volume:<book_url>#<index>` 禁止抓正文 | 6.2、6.4 | 仅单测（URL 生成）；**无 HTTP 证据** |
| 新增书签 `GET/POST /api/bookmarks`、`DELETE /api/bookmarks/delete` | 6.6 | **已验证**（回归 6/6：字段、`position` 0/1 放行与 1.5/-0.1 400、2001 字 400、按书 3 条、`deleted=1`、`id=0` 400） |
| 新增 `GET /api/book/search` | 6.7 | **已验证**（回归 6/6：命中 2 章、`complete` 两种状态、参数校验、`limit` clamp）；**504 未触发** |
| 新增 `POST /api/book/cache-all` | 6.8 | **已验证**（回归 4/4：逐章失败隔离、`limited` 截断、无目录/空 url 400）；`skipped_budget` 未单独验证 |
| 新增 `POST /api/book/cache/clear` | 6.9 | **已验证**（回归 4/4：按书 `deleted>=3`、用户级 `scope=user` + `bytes_freed`） |
| 新增 `POST /api/bookshelf/batch` | 7.11 | **已验证**（回归 4/4：`set_group updated=2 failed=1`、`remove_group`/`remove`、非法 action/空 ids 400） |
| 设置新增 `page_width`/`font_family`，并改为全字段校验 | 9.1、9.2 | **已验证**（回归 4/4：默认值、`640/mono` 写入读回、300/2100/`comic` 400 不覆盖、320/2000 与枚举） |

**11:08 冒烟 + 回归复核（2026-11-03）**：证据来源 **`<local-tools>\tests\report-20261103-1108.md`**，被测二进制与 `:3000` 一致（**9,224,704 B / mtime 2026-11-03 11:08:53 / sha256 `F24B000C…`**）。总结果：旧用例 **75/75**、新接口 **46/46**、`/k` **10/10**，零失败。

| 接口 / 行为 | 回归证据 | 仍缺证据 |
|---|---|---|
| 书签 GET/POST/过滤/DELETE | 6/6：完整字段、`position=0/1` 放行、`1.5/-0.1` 400、2001 字/空 `book_id`/负章节 400、按书 3 条、`deleted=1`、`id=0` 400 | 同章多条（无唯一约束） |
| `GET /api/book/search` | 6/6：4 章缓存 3 章、命中 2 章、`complete=false/true` 两态、空 `q`/201 字 400、`limit=999`→50 | **超时 504 未触发**（实测 200 / 1 ms） |
| `POST /api/book/cache-all` | 4/4：`limited=true attempted=2 cached=2`、逐章失败隔离（`cached=3 failed=1`）、无目录/空 `book_url` 400 | `skipped_budget` 预算跳过未单独验证 |
| `POST /api/book/cache/clear` | 4/4：按书 `deleted>=3`、用户级 `scope=user` + `bytes_freed` | — |
| 规则包 export/import/preview | 8/8：非空导出（camelCase、无本地 id）、`?id` 单条/404、`inserted=1` 可回读、坏正则 200+`invalid_regex`、私网 400、非法结构 400 | 5 MiB / 15 s 预算分支 |
| `POST /api/sources/check-updates` | 4/4：四类差异各 1（`updated=1 local_newer=1 same=1 new=1`）、只读、空数组 | — |
| `POST /api/bookshelf/batch` | 4/4：`set_group updated=2 failed=1`、`remove_group`/`remove`、非法 action/空 ids 400 | — |
| `POST /api/book/toc/batch` | 3/3：混合批 `updated=2 skipped=2`（重复+缺源）、单本失败隔离、空列表 400、`book_ids` 别名 | **并发 3 未被直接观测** |
| `GET /api/admin/users` / `POST .../status` | 7/7：4 账号、普通用户 403、禁用后旧 JWT 401、启用后旧 JWT 仍 401、重新登录成功、禁自己 400 | 最后管理员保护（未见单独用例） |
| `POST /api/auth/password` | 7/7：成功改密 `relogin_required=true`、旧 token 401、旧密码 401、新密码 200、旧密码错/新密码过短 400 | — |
| `GET /api/book/toc` | 200 响应含 `update_time`/`word_count`/`volume_title` | **分卷行真实数据渲染**、`volume:` URL 抓正文被拒 |
| `/k/*` | 10/10：登录发 `k_session`、错误密码 303、未登录登录页、书架/目录页、正文 `?fs=1/2/3` → 28/32/38px | — |

> 本文所有“仅单测”均指 `server/src/**` 内 `#[cfg(test)]` 已覆盖对应函数；除非条目明确写“已验证（HTTP 冒烟/回归…）”，否则**没有** HTTP 级联调证据。未列出的新行为一律按“未验证”对待。

**14:16 复核记录（2026-11-03，第三轮）**：补齐两段式聚合搜索接口，并同步 `@js`/DOM 桥行为与环境变量默认值。接口总数由 54 增至 **56**（不含 `/k/*` 的 8 个 HTML 路由）。

| 变更 | 落点 | 验证状态 |
|---|---|---|
| 新增 `GET /api/search/start` | 5.5 | **已验证**（HTTP 实测 `:3000`：200 字段齐全、空/缺 `key` 400） |
| 新增 `GET /api/search/result` | 5.6 | **已验证**（HTTP 实测 `:3000`：中间态 `done=false`+`has_more=true`、终态 `done=true`、分页 clamp、`page=99` 空页、伪造/过期 job 404） |
| job 60s TTL | 5.5、5.6 | **已验证**（HTTP：70s 后 404）。当时的“最多 8 个”与淘汰顺序 bug 均已被后来的 32/64 上限取代，见 5.5 正文 |
| 单源 200 本、总结果 400 本上限 | 5.5 | 源码级，未直接触发截断 |
| `@js:` 返回数组改为 JSON 字符串 | 2.7 | 仅单测（`rule_engine.rs` 用例） |
| DOM 桥 `getElements/getString/getStringList` 仅字面量选择器；输入 1 MiB / 节点 2000 / 抽取 1 MiB；JS 3000ms 预算 | 2.7、6.4 | 字面量选择器与 DOM 桥**仅单测**；三个上限与 3000ms 超预算为**源码级，未验证** |
| `HTTP_CONCURRENCY` 默认 16（1..16）、`SEARCH_TIMEOUT_SECS` 8（5..30）、`SEARCH_ALL_TIMEOUT_SECS` 8（5..30）、`SEARCH_CURATED_BUDGET_SECS` 6 / `SEARCH_ALL_BUDGET_SECS` 40（2..60） | 15.4 | `HTTP_CONCURRENCY` 源码级；预算经 `/healthz` 的 `search` 块与 `deadline_ms`/`total_budget_ms` **HTTP 实测**（2026-11-04 扫参） |

> 本轮 HTTP 证据来自 `:3000` 上运行的工程侧旧构建名 `legado-server.exe`（对外发布名 `boranlib.exe`；**9,259,520 B / mtime 2026-11-03 13:32:29**），与上一轮 11:08 回归二进制不同。`books.rs` 磁盘 mtime 为 14:06（晚于被测二进制）；两段式搜索的字段与行为按磁盘源码核对，若后续源码改动与实测不一致，以重新构建后的结果为准。

**15:01 复核记录（2026-11-03，第四轮）**：后端已上线新二进制 **9,315,840 B / mtime 2026-11-03 14:46:58 / sha256 `B498BB42…`**（`cargo test` **99 项通过**）。接口总数由 56 增至 **58**（不含 `/k/*` 的 8 个 HTML 路由）。

| 变更 | 落点 | 验证状态 |
|---|---|---|
| 新增 `GET /api/book/export/info` | 6.10 | **已验证**（HTTP `:3000`：72 章样本 `total=72/cached=5/missing=67`；缺参/空参/无目录 400） |
| 新增 `GET /api/book/export/txt` | 6.11 | **已验证**（HTTP `:3000`：`text/plain` 附件 + 5 个 `x-*` 头；头部/占位块/页脚齐全；72 章样本 78,718 B / 15 ms）。性能验收样本为 34,586 B / 17 ms；常驻内存 18.4→18.4 MB 见下 |
| `search/start` 支持 `q`/`key` 别名与 `source_ids` 过滤，响应新增三字段 | 5.5、12.2#55 | **已验证**（HTTP：`q` 可用；`source_ids=1,999,abc` → `requested=[1,999,abc]`、`ignored=[999,abc]`、`warning` 非空、`source_total=1`；全无效 → `source_total=0` 且 `done=true`）；`key` 优先于 `q` 为**源码级** |
| 修复 job 淘汰顺序：超过 8 个时淘汰 `elapsed` **最大**的最旧任务 | 5.5 | **已验证**（HTTP：连开 9 个 → 第 1 个 404，第 2~9 个 200；2026-11-03 15:00 上线）。**该结论已被当前 32/64 上限取代**，见 5.5 正文 |
| `BORANLIB_WEB_DIR` / `LEGADO_WEB_DIR` 静态资源目录（默认 `web/dist`；`:3000` 设为 `<repo>/server/web/dist`） | 11.4、15.4 | **已验证**（HTTP：`/` 200、`/assets/*` 200、`/k/login` 200；`/k` 带会话 200、无会话 303 → `/k/login`） |
| 单源 200 / 总 400 截断 | 5.5 | **未验证**：未真实触发截断 |
| `is_pay=true` 章节 | 6.10、6.11 | **仅单测**：导出不做付费过滤，真实付费样本链路未验证 |
| 超大书导出压力样本 | 6.11 | **未验证**：本轮仅 72 章；未做 10k+ 章/大文件压力 |

> 本轮 `:3000` 证据与上面第三轮记录的 13:32 二进制不是同一构建；第四轮条目以 **14:46:58 新二进制** 为准。

### 第五轮（全景校准，2026-11-04 20:10）

- 本轮只在 **隔离数据目录 + 端口 `3061`** 上实测（复制 `server/data`，未触碰 `:3000/:3500` / OpenStick）；测试完成即关闭，`cargo test`（默认线程）**186 passed / 0 failed**。
- 新增 **5 个接口**（健康巡检 3 个 + `prune` + `quarantine`），接口总数 **58 → 63**；书源类 **9 → 14**。
- 关键口径：健康状态主键是 `bookSourceUrl`（不是域名）；只有“源站不可达”（连接失败/超时/4xx/5xx）计入连续失败，**规则引擎失败（`FAIL_ENGINE`）不冷却、不进隔离区**。
- 已实测：3 个站点级死源连续 3 次失败后写 `cooldown_until`；默认搜索 `skipped_cooldown=2`、`source_total=2`、0 错误、1.24s；`include_cooled=1` 时 `source_total=4`、耗时 2.47s、多出 26 本书，证明冷却源确实被跳过/按需放行；`FAIL_ENGINE` 源连续 3 次 `partial` 后 `consecutive_failures` 仍为 0。
- 导入账本：`boranlib-sources.json`（107 条）首次 `added=23 / unchanged=84 / distinct_hosts=74`；同合集二次导入 `collection_unchanged=true`、`probed=0`、`unchanged=107`；手工隔离后再导入 `resurrected=1`；三态实测 `update_rejected=1` / `conflict=1` 均保留本地版本。

---

## 目录

1. [阅读约定](#1-阅读约定)
2. [共享数据契约](#2-共享数据契约)
3. [鉴权](#3-鉴权)
4. [书源](#4-书源)
5. [搜索与发现](#5-搜索与发现)
6. [书籍与正文](#6-书籍与正文)
7. [书架与进度](#7-书架与进度)
8. [替换规则](#8-替换规则)
9. [设置](#9-设置)
10. [阅读统计](#10-阅读统计)
11. [健康检查/其他](#11-健康检查其他)
12. [接口总表与覆盖统计](#12-接口总表与覆盖统计)
13. [待实现接口（草案；已落地条目标注“已实现”）](#13-待实现接口草案已落地条目标注已实现)
14. [待确认清单](#14-待确认清单)
15. [附录：错误状态码与前端处理建议](#15-附录错误状态码与前端处理建议)

---

## 1. 阅读约定

### 1.1 基础地址与协议

- 二进制默认监听 `0.0.0.0:8080`（`BORANLIB_HOST`/`BORANLIB_PORT` 可改；未加前缀的 `HOST`/`PORT` 不生效）。Windows 成品包的 `start.ps1` 默认注入 3000，实际监听 `0.0.0.0:3000`。API 前缀统一为 `/api`。
- 请求/响应均为 `application/json; charset=utf-8`，以下两类例外：
  - `GET /api/health` 返回纯文本 `ok`。
  - Axum 参数提取失败（缺 query、JSON 解析失败、缺 `Content-Type`）返回**纯文本**错误，不是统一 JSON 错误体（详见 15 节）。
- 所有示例 JSON 中的值均为**伪造示例值**，仅演示形状；真实值由抓取结果/数据库决定。

### 1.2 认证方式

- 除公开路径外，所有接口必须携带：

```http
Authorization: Bearer <jwt>
```

- 公开路径（`auth_middleware` 精确匹配，见 [mod.rs](../server/src/routes/mod.rs#L46-L56) 与 [auth.rs](../server/src/auth.rs#L186-L200)）：
  - `GET /api/health`
  - `GET /healthz`
  - `POST /api/auth/login`
  - `POST /api/auth/register`
- 特别注意：`GET /api/healthz` **不在公开列表**，必须登录。
- 需要管理员（`role == "admin"`）的接口：`POST /api/sources`、`POST /api/sources/import`、`POST /api/sources/enabled`、`DELETE /api/sources/delete`、`GET /api/admin/users`、`POST /api/admin/users/status`。其余接口任何已登录用户均可访问。
- **令牌实时校验**：每个需要鉴权的请求，除验签外还会按 `sub` 查 `users` 表，要求 `disabled = 0` 且 `token_version == claims.ver`；不满足即 401。因此**禁用账号或改密后，旧 JWT 立即失效**（详见 3.2~3.5）。
- Kindle 端 `/k/*` 不使用 Bearer，而用 Cookie `k_session`（值为 JWT）；校验规则与 Bearer 相同，失败时返回 `303 Location: /k/login`，不是 401。`/k/login` 本身公开。
- **HTTP 回归（2026-11-03，`report-20261103-1108.md`，10/10）**：登录成功下发 `k_session`；错误密码 303 回登录页并提示“账号或密码错误”；未登录 `/k` 显示登录页；书架/目录页可访问；正文 `?fs=1/2/3` 的 computed `font-size` 为 28/32/38px，三档均无脚本、无横向溢出。

### 1.3 字段命名

- 业务实体（书源、书籍、章节、正文、书架项、替换规则）在**响应中**一律使用 `snake_case`。
- 例外：与 Legado 互通的**导入/导出**使用 `camelCase`——书源导入/导出（见 2.7）、替换规则导出/导入（见 8.3/8.4）；其余读写接口仍是 `snake_case`。
- 常规接口的查询参数与 JSON 请求体字段为 `snake_case`（`source`、`book_url`、`page_size`、`chapter_url` 等）；Legado 导入/导出接口（2.7、8.3、8.4）接受/输出 camelCase。

### 1.4 状态码与错误体

统一错误体（`AppError::into_response`）：

```json
{ "error": "错误描述" }
```

| HTTP | 触发条件（源码依据） |
|---|---|
| 400 | 业务参数错误、书源 JSON 反序列化失败、`url or urls required`、`book_id or book_ids required`、`group name required`、上游抓取/解析失败（`anyhow` 被映射为 400，文案前缀 `internal:`） |
| 401 | 无 Bearer、JWT 无效/过期（`{"error":"unauthorized"}` 或 `{"error":"jwt: ..."}`） |
| 403 | 非 admin 调用管理员接口（`{"error":"forbidden"}`） |
| 404 | 书源不存在（`source not found`）、`GET /api/sources/detail` 找不到 |
| 500 | SQLite 错误（`db error: ...`）、bcrypt、IO |

### 1.5 各接口是否返回 null

Rust 结构体凡未标注 `skip_serializing_if` 的 `Option<T>` 字段，**JSON 中会显式出现 `null`**，而不是省略。前端应按“字段存在且可能为 null”处理。本文每个响应表都标注“可空”。

---

## 2. 共享数据契约

本节是跨接口的全局语义，所有模块章节都默认遵守，不再重复解释。

### 2.1 `reading_progress.position` 的口径

**结论：`position` 是章内滚动比例，服务端只接受 `0..1`；越界（负数、`>1`、NaN）直接 400，不再做 0..100 的兼容换算。**

源码依据：

- 表定义 `position REAL NOT NULL DEFAULT 0`（[user/0001_init.sql](../server/migrations/user/0001_init.sql#L17-L22)）。
- `set_progress` 用 `validate_position()` 严格校验：仅当 `is_finite()` 且落在 `0.0..=1.0` 才通过，否则返回 `400 position 必须是 0..1 的章内比例`（[bookshelf.rs](../server/src/routes/bookshelf.rs#L351-L360)）。
- `GET /api/bookshelf/progress` 原样返回 `position`；`percent` 由 `progress_percent()` 统一计算，**包含**章内比例（[bookshelf.rs](../server/src/routes/bookshelf.rs#L336-L406)）。
- `GET /api/bookshelf` 的 `position` 与 `reading_progress` 同口径（[bookshelf.rs](../server/src/routes/bookshelf.rs#L76-L95)）。

前端必须遵守：

- 上报 `0..1`（例如滚到 60% 传 `0.6`）。
- 传 `60`、`-0.2`、`NaN` 都会被 **400 拒绝**（旧版“>1 自动除 100”的宽松行为已移除）。
- `1` 合法，表示章节末尾。

### 2.2 `chapter_index` 是数组下标，不是章节 id

**结论：`chapter_index` 是 0 基的章节数组下标，与 `BookChapter.index` 一致；不是章节 id，也不是章节 URL。**

源码依据：

- 目录解析时 `index: chaps.len() as i64`，第一项为 0（[web_book.rs](../server/src/web_book.rs#L223-L230)）。
- 目录落库写入 `"index"` 列（[books.rs](../server/src/routes/books.rs#L421-L455)）。
- 进度默认值 0，`set_progress` 用 `chapter_index.max(0)`（[bookshelf.rs](../server/src/routes/bookshelf.rs#L371-L374)）。
- 单本统计 `read_chapters = chapter_index + 1`，说明 `chapter_index` 是下标（[bookshelf.rs](../server/src/routes/bookshelf.rs#L412)，[stats.rs](../server/src/routes/stats.rs#L98)）。
- 返回的 `BookChapter` 中唯一的 id 是 `url`（`book_chapters.url` 主键）；没有数值章节 id。

前端必须遵守：

- 用 `toc[index]` 定位章节；保存时传该数组下标。
- 目录刷新后若章节顺序变化，`chapter_index` 可能指向不同章节（服务端没有按 URL 校正的逻辑）。前端宜同时保存章节 URL（目前契约未提供进度 URL 字段，见 13.1）。
- `percent` / `reading_progress` = `(chapter_index + clamp(position, 0, 1)) / total_chapters * 100`，**包含章内比例**，钳制在 `0..100`。

### 2.3 分页语义

| 接口 | `page` | `page_size` | 默认/上限 | `total` 语义 |
|---|---|---|---|---|
| `GET /api/search` | 从 1 开始；`unwrap_or(1).max(1)`，0/负数当 1 | 可选，`clamp(1, 200)` | 默认 30 | **无 `total` 字段**；返回纯数组；切片基于与 aggregate 相同的完整聚合结果 |
| `GET /api/search/aggregate` | 同上 | 可选，`clamp(1, 200)` | 默认 30 | 去重后完整结果条数，最多 400；来自各源**第 1 页**，60s 内缓存复用 |
| `GET /api/explore` | 从 1 开始；只影响“按书源 ID 展开分类”的 `{{page}}` | 无 | 无 | 无分页字段 |
| `GET /api/explore/categories` | 从 1 开始；替换 `exploreUrl` 中的 `{{page}}` | 无 | 无 | 返回全部分类 |
| `GET /api/search/start` | 无 | 无 | 无 | 无分页字段，仅创建 job |
| `GET /api/search/result` | 从 1 开始；`unwrap_or(1).max(1)` | 可选，`clamp(1, 200)` | 默认 30 | 已完成源去重排序后的条数，最多 400；**随源完成而增长** |
| `GET /api/book/export/info` | 无 | 无 | 无 | 返回全书统计，不分页 |
| `GET /api/book/export/txt` | 无 | 无 | 无 | 流式返回整本 TXT（可能含未缓存占位），不分页 |
| 其余接口 | 无分页参数 | 无 | 无 | 返回全量 |

`GET /api/search` 与 `GET /api/search/aggregate` 的分页实现（2026-11-02 20:0x 复核，源码已更新）：

1. 两个入口共用 `aggregated()`：只向书源抓取**第 1 页**（`search_all_with_errors(sources, key, 1)`），合并、去重、按权重排序，截断到 400 本，并写入 60s 缓存。
2. 再对这份完整结果执行 `skip((page-1)*page_size).take(page_size)`。

因此 `page>=2` 不再重复抓取书源第 N 页，跨页不会重复或遗漏；但可翻到的内容上限是“各源第 1 页去重后的前 400 本”。`total` 只在这份结果内可信（见 5.2）。

### 2.4 聚合搜索的有序性与去重

- 去重键：`name|author`（均 `trim().to_lowercase()`）；若 `name` 为空，则用 `book_url`。**同名同作者的多书源结果只保留排序后的第一条**（[web_book.rs](../server/src/web_book.rs#L316-L344)）。
- 排序：书源 `weight` 降序 → `custom_order` 升序 → `book_url` 升序（[web_book.rs](../server/src/web_book.rs#L316-L331)）。
- 参与搜索的书源条件：`enabled == true && rule_search != null && search_url != null`。
- 单源失败不使整体失败；`/api/search` 静默丢弃失败，`/api/search/aggregate` 与 `/api/search/result` 在 `errors` 中返回。
- 两段式搜索（5.5/5.6）沿用同一套 `dedupe_and_sort` 与 400 本截断；参与源为 **job 创建时的启用快照**（若传 `source_ids`，则是按该参数过滤后的子集），之后改书源不影响该 job；分页与去重都基于这份快照。

### 2.5 时间字段格式与时区

| 字段 | 存储/返回格式 | 时区 | 依据 |
|---|---|---|---|
| `bookshelf.added_at`、`reading_progress.updated_at`、`book_stats.last_read_at`、`book_content.updated_at/accessed_at` | `"YYYY-MM-DD HH:MM:SS"`（无时区后缀） | SQLite `datetime('now')` = **UTC** | 迁移默认值 + 各 SQL |
| `reading_stats.day`、`reading_streak.last_day`、统计接口的 `day` | `"YYYY-MM-DD"` | **服务器本地日期**（`chrono::Local` / `date('now','localtime')`） | [stats.rs](../server/src/routes/stats.rs#L44-L50)，[bookshelf.rs](../server/src/routes/bookshelf.rs#L199-L210) |
| `reading_streak` 对外字段 | 整数天数（`streak_days`/`longest_streak_days`） | — | 同上 |
| JWT `exp` | Unix 秒 | UTC | [auth.rs](../server/src/auth.rs#L121-L134) |
| `Book.latest_chapter_time`、`BookChapter.start/end` | `i64` | 语义未定义/当前恒 0 | [entities.rs](../server/src/entities.rs#L255-L273) |
| `SearchBook.time` | `String?`（当前不填充，恒 `null`） | — | [web_book.rs](../server/src/web_book.rs#L117-L129) |

前端必须遵守：

- 形如 `2026-11-02 11:34:36` 的字符串是 **UTC**，解析时补 `Z`（或 `+00:00`）再转本地，否则会差 8 小时。
- 形如 `2026-11-02` 的 `day` 是**服务器本地日期**，不应再做时区换算。
- `updated_at` 等字段可能为 `null`（例如从未阅读）。
- 服务器本地时区由部署环境决定（当前设备为 `Asia/Shanghai`）；跨时区部署时 `day` 与 UTC 时间戳会不一致，见 14 节“待确认”。

### 2.6 `bookshelf` / `reading_progress` / `book_meta` 的一致性

- `bookshelf.book_id` 与 `reading_progress.book_id`、`book_stats.book_id`、`book_meta.book_url` 是同一个值（书籍 URL）。
- 书架列表用 `LEFT JOIN book_meta` 与 `LEFT JOIN reading_progress` 组合展示，因此即使 `book_meta` 不存在也能返回，但 `total_chapter_num` 为 0、`reading_progress` 为 0（[bookshelf.rs](../server/src/routes/bookshelf.rs#L84-L101)）。
- 分组有两个来源：`bookshelf."group"` 与 `book_meta."group"`；读取用 `COALESCE(bs."group", bm."group")`（书架优先）。写入 `POST /api/bookshelf`、`POST /api/bookshelf/group` 时会尽量同步两表。
- `book_meta` 是按 `book_url` 的唯一缓存；`GET /api/book/info`、`POST /api/bookshelf` 会 upsert。

### 2.7 书源 JSON 的命名映射与 通用书源格式

**导入方向（camelCase → snake_case）**

- `POST /api/sources`、`POST /api/sources/import`、`POST /api/sources/validate` 都先调用 `camel_to_snake_value` 递归转换，再反序列化为 `BookSource`（[sources.rs](../server/src/routes/sources.rs#L97-L101)）。
- 规则：遇到大写字母在其前插入 `_`（首字符除外）并转小写；`_` 保留。示例：
  - `bookSourceUrl` → `book_source_url`
  - `ruleSearch` → `rule_search`
  - `bookList` → `book_list`
  - `lastChapter` → `last_chapter`
  - `checkKeyWord` → `check_key_word`
  - 已经是 snake_case 的键保持不变。
- 已知边界：连续大写缩写会被拆开（`URL` → `u_r_l`），因此**导入时请使用 Legado 的标准拼写**（`bookSourceUrl`，而不是 `bookSourceURL`）。
- 数字字段允许字符串：`string_or_i64` 接受 `"30"`/`30`；布尔字段允许 `"true"`/`"false"`/`"1"`/`"0"`（[serde_util.rs](../server/src/serde_util.rs#L8-L35)）。

**导出方向（snake_case → camelCase）**

- `GET /api/sources/export` 把 `BookSource` 序列化后递归 `snake_to_camel_value`，返回数组，可直接给 Legado 导入（[sources.rs](../server/src/routes/sources.rs#L182-L194)）。
- 未知字段通过 `#[serde(flatten)] extra` 原样保存，导出时也会转回 camelCase，实现“无损往返”。
- `GET /api/sources/detail` 返回的是 **snake_case**，与 export/import 不对称，前端不要混用。

**通用书源格式字段清单（实际建模的字段）**

| 层级 | camelCase 字段 |
|---|---|
| 书源 | `bookSourceUrl`、`bookSourceName`、`bookSourceGroup`、`bookSourceType`（0=图书，1=音频）、`bookUrlPattern`、`customOrder`、`enabled`、`enabledExplore`、`jsLib`、`enabledCookieJar`、`concurrentRate`、`header`、`loginUrl`、`loginUi`、`loginCheckJs`、`coverDecodeJs`、`bookSourceComment`、`variableComment`、`lastUpdateTime`、`respondTime`、`weight`、`exploreUrl`、`exploreScreen` |
| 书源（oread 扩展） | `timeoutMillisecond`（单源 HTTP 超时，Legado 无此字段；非法值宽松归 0，回退全局） |
| `ruleSearch` | `checkKeyWord`、`bookList`、`name`、`author`、`intro`、`kind`、`lastChapter`、`updateTime`、`bookUrl`、`coverUrl`、`wordCount` |
| `ruleBookInfo` | `init`、`name`、`author`、`intro`、`kind`、`lastChapter`、`updateTime`、`coverUrl`、`tocUrl`、`wordCount`、`canReName`、`downloadUrls` |
| `ruleToc` | `preUpdateJs`、`chapterList`、`chapterName`、`chapterUrl`、`formatJs`、`isVolume`、`isVip`、`isPay`、`updateTime`、`nextTocUrl` |
| `ruleContent` | `content`、`title`、`nextContentUrl`、`webJs`、`sourceRegex`、`replaceRegex`、`imageStyle`、`imageDecode`、`payAction` |
| `ruleExplore` | 与 `ruleSearch` 相同但无 `checkKeyWord` |
| `ruleReview` | 原样保留（`Value`），服务端不解析 |
| 其他 | 所有未建模字段进入 `extra`，往返保留 |

- `concurrentRate`（Legado 字段）：完整实现「次数/间隔」语义。`"N/间隔ms"` → 在飞上限 **N**，且每发出 **N** 次请求后等待 **间隔 ms**（`"3/1000"` = 3 并发、每 3 次等 1s）；只写整数（`"3"`）、缺省、空串、非法、`0` 时退化为纯并发上限（无速率等待），并发默认 **8**。间隔上限 600000ms（10 分钟）防呆。按源等待时间计入 `/healthz` 的 `search.source_limit_wait_ms`。默认仍为 8 的理由见原实测（收紧到 2/4 只在同源图片突发时增加排队，收益不足）。**验证状态：解析与速率门仅单测；真实站点未做长跑观测。**
- 生效范围：搜索、详情、目录、正文四类抓取与图片代理（`/api/book/image`、`/api/book/images/meta`）都经过同一个按源信号量；先拿按源许可、再抢全局 `HTTP_CONCURRENCY` 槽，因此某个源的排队不会占住全局槽（其他源照常推进）。按源等待次数见 `/healthz` 的 `search.source_limit_waits`。

**服务端实际使用的 Legado 行为子集（写文档时别假设全部生效）**

- 搜索 URL 支持 `url,{"method":"POST","body":...,"headers":...}`；URL 与 POST `body` 中的 `{{key}}`/`${key}`、`{{page}}`/`${page}` 会被替换；模板里的 JS 也按 Legado `AnalyzeUrl` 求值：`{{表达式}}` 以 `key`/`page` 绑定求值、`<js>…</js>` 块就地替换为执行结果、整个模板可用 `@js:` 前缀（oread 扩展）。任一段 JS 异常 → 该段返回空并记规则异常台账，不拖垮整源。**验证状态：仅单测（三种写法各 1 例）。**
  - 注意：`GET /api/search` 与 `/api/search/aggregate` 固定向书源抓第 1 页（`page=1`），因此搜索模板里的 `${page}` 实际恒为 `1`；需要翻页由服务端对聚合结果本地切片完成（2.3/5.1）。`${page}` 取非 1 值只发生在发现分类或目录模板。
- 目录地址本身可以是模板：`.../toc_{{page}}.html` 或 `.../toc_${page}.html`，首屏固定代入 `page=1`；目录规则内也可用当前页号（[web_book.rs](../server/src/web_book.rs#L251-L279)）。**验证状态：仅单测（`${page}`）；`${key}` 未验证。**
- 目录支持 `nextTocUrl` 翻页，**最多抓 10 页**，按 URL 去重防循环；`nextTocUrl` 为字面 URL/`{{page}}`/`${page}` 模板时按“下一页页码”代入；为 CSS/JSON 等规则时先按列表规则取**全部**匹配值（多页目录可一次给出），复合规则（`&&`/`||`/`##`/`@js:`）退回单值（[web_book.rs](../server/src/web_book.rs#L211-L248)、[web_book.rs](../server/src/web_book.rs#L259-L311)）。**验证状态：仅单测（多页链接与 10 页上限）。**
- 正文支持 `nextContentUrl` 拼接，最多 10 页；`sourceRegex` 先于 `replaceRegex`，按 `正则##替换` 或 `/pat/repl/` 执行（[web_book.rs](../server/src/web_book.rs#L245-L292)、[web_book.rs](../server/src/web_book.rs#L473-L505)）。
- `##正则##替换` 的替换串支持 Rust regex 风格的捕获组引用 `$1`、`$2`…（[rule_engine.rs](../server/src/rule_engine.rs#L521-L546)、[rule_engine.rs](../server/src/rule_engine.rs#L1447-L1458)）。**验证状态：仅单测。**
- **`@js:` 的返回值**：表达式结果为数组时，统一用 `JSON.stringify` 序列化为 JSON 字符串（不再用逗号拼接）。当该规则被当作**列表规则**（`get_string_list`）消费时，会按书源章节语义展开：`string[]` → 逐项；`[{text/name/title,href/url}]` 对象数组 → 每项保留为紧凑 JSON（后续 `chapterName`/`chapterUrl` 等裸字段规则按键取值，别名 `text/name/title`、`href/url/src`）；`[[title,url]]` 二元数组（含嵌套层）→ 规范化为带别名对象；`[{header,list:[...]}]` 分卷结构 → 生成一条分卷头（`volume=true`）+ 递归子项；`null`/空串/缺字段项跳过并记一条 `tracing::warn`（含跳过计数）。漫画图片列表（`keep_objects=false`）取 `src/url/href`。单值规则拿到的仍是 JSON 字符串本身（例如 `["a","b"]`）（[rule_engine.rs](../server/src/rule_engine.rs)）。**验证状态：单测 5 例（对象/二元嵌套/分卷/空项/列表数组 result）+ 真实站点（中文万维目录 1→30、开心漫画 1→60、武道文学 0→1665，见 `docs/ENGINE_ALIGN_20261105.md`）。**
- **规则级变量链 `@put:{key:表达式}` / `@get:{key}`（书源规则语义约定）**：`@put` 在规则任意位置执行，写入**单次规则求值**的变量字典；`@get` 读出（支持 `@get:key` 与 `@get:{key}`），取不到返回**空串**；可链式（前一条写入的键被后续规则/后续片段读回）。作用域是**单次规则求值实例（一次抓取请求）**：详情/目录/正文各请求互不泄漏；`java.put`/`book.putVariable` 与 `@put` 共用同一变量表，并通过尾部 sentinel 回传 Rust（同书变量可落库）。无全局表，并发请求互不影响。**验证状态：单测 4 例（链式、取空、并发隔离、与 JSONPath/`@js` 组合）+ 真实站点（小说下载网/手机小说下载网目录 0→101/89）。**
- **`@js:` / `<js>` 段内 `{{表达式}}` 先展开**：与书源规则约定一致，`{{ }}` 属规则级模板，**先于 JS 求值**展开；展开结果按上下文拼接进源码——位于字符串字面量内时转义引号/反斜杠/换行（模板串额外转义 `\${`），否则原样拼接；展开失败记 `tracing::warn` 并保留原文（不静默清空）。**验证状态：单测 3 例（展开后语法合法、引号转义、失败保留原文）；真实站点无可归因证据。**
- **`cookie` JS 全局（书源规则最小面）**：`cookie.getCookie(url)` → `name=value; ...`（无值空串）；`cookie.setCookie(url, value)`（`value` 支持 `a=b; c=d` 多段）、`cookie.removeCookie(url|domain)`、`cookie.clear()` → 均返回空串，可直接拼进 `{{ }}` 模板 URL。底层复用 `req.rs` 的按源 Cookie 桶（`source:<bookSourceUrl>`，与抓取请求共享；无源键时退化为 host 桶），**仅进程内、不持久化到库**；不同源各自成桶，并发不串味。注意 `java.getCookie`/`java.setCookie` 仍是另一套 API，继续显式抛未实现（[req.rs](../server/src/req.rs)、[rule_engine.rs](../server/src/rule_engine.rs)）。**验证状态：单测 4 例（取无值、设置后取、删除后取、并发两源隔离）；真实站点无可归因证据。**
- **DOM 桥（Phase 1）**：`java.getElements`、`java.getString`、`java.getStringList` 已支持，但**只接受字符串字面量选择器**（`"..."`/`'...'`），不能是变量或动态拼接；动态选择器仍抛 `UNSUPPORTED_JS_API:...（仅支持字面量选择器参数）`。桥在 Rust 侧对**当前页 HTML**（传入规则求值的 `input`）预抽取，JS 内只做查表；`getElements`/`getStringList` 返回匹配元素的 **HTML 字符串数组**，`getString` 返回首个匹配的抽取结果（[rule_engine.rs](../server/src/rule_engine.rs#L816-L865)）。上限：页面 HTML > **1 MiB** 不做预抽取（调用会抛错）；每个列表截断到 **2000** 个节点；所有选择器累计抽取文本 > **1 MiB** 时停止继续预抽取；选择器自身含 `@js:`/`<js>` 的跳过。**验证状态：字面量选择器与 DOM 桥仅单测；上述三个上限为源码级，未验证。**
- **`org.jsoup` 桥（新增）**：JS 里可直接用 `org.jsoup.Jsoup.parse(html[, baseUri])` 得到文档对象，再 `select`/`selectFirst`/`get`/`eq`/`first`/`last`/`size`/`attr`/`hasAttr`/`text`/`html`/`outerHtml`/`is`/`parent`/`nextSiblingElements` 等；`Elements` 支持数字下标、`length` 与 `for…in`（只枚举下标）。`attr("abs:href")` 等 `abs:` 前缀按 `baseUri` 解析。选择器用 `scraper`（html5ever）后端，**jsoup 专有伪类**（`:contains`/`:containsOwn`/`:matches`/`:matchesOwn`/`:eq(`/`:lt(`/`:gt(`/`:nth(`）显式抛 `UNSUPPORTED_JS_API`，不静默返回空。147 源实际调用面只有 `parse`+`select`+索引/`attr`/`text`/`html`，覆盖面无缺口。**验证状态：仅单测（5 例）；未做真实站点逐源验证。**
- **剩余 `java.*`（新增）**：`aesBase64DecodeToString`、`createSymmetricCrypto`（AES/CBC + PKCS5/7/NoPadding；key 支持字符串/字节数组；ECB 显式报错）、`bytesToStr`、`timeFormatUTC(t,format,offsetHours)`、`getElement(selector)`（基于当前页）、`refreshTocUrl()`（等价 `book.tocUrl` 变更标记，随后重取详情页更新目录）。**验证状态：全部仅单测；哔哩漫画 `image_decode` 链路（`base64Decode`+`createSymmetricCrypto`+`bytesToStr`+`Buffer.concat`）有单测，但真机仍被 jsLib/`Reload` 卡住。**
- `@js:` 中依赖网络/Cookie/WebView 的 Legado 内置**仍抛错**：`java.ajax`、`java.post`、`java.connect`、`java.getCookie`、`java.setCookie`、`java.webView`、`java.webViewGetSource`、`java.webViewGetOverrideUrl`（[rule_engine.rs](../server/src/rule_engine.rs#L366-L397)）。**字段级容错**：书源解析中任一字段规则抛错 → 该字段按空值处理、记 `tracing::warn` 与规则异常台账，**不再整源失败**；`/api/search` 仍静默丢弃“最终解析为空”的源，`/api/search/aggregate`/`result` 仅在网络/空解析时放 `errors[]`。**验证状态：字段级容错有单测；HTTP 级行为经 147 源运行时探测（见 `docs/ENGINE_ALIGN_20261105.md`）。**
- **JS 时间预算**：默认 **3000ms**（`JS_TIMEOUT_MS`，可配 20..5000）。预算是在脚本执行**结束后**按墙钟耗时判定，超过则规则失败并返回 `js eval exceeded Nms budget (took Xms)`；不是抢占式中断，脚本仍可能先跑完。默认值由 200ms 放宽到 3000ms：慢设备上腾讯漫画正文图片规则单次约 1.4s，200ms 会把整章图片判超时（0 张）；常见规则仍为毫秒级，不受影响。**验证状态：默认值与超时语义有单测；耗时分布见 v0.3.8 实测。**
- `ruleBookInfo.init` 的结果现在参与编排：可改写 `book.bookUrl`（返回给 Rust 侧），目录更新时也用它作为 `preUpdateJs` 的输入（[web_book.rs](../server/src/web_book.rs#L175-L177)）。
- `ruleContent.imageDecode` 与 `ruleToc.preUpdateJs`/`formatJs` 状态：`imageDecode` 已接入图片代理（有 `imageDecode` 的源先整图缓冲解密再直出，失败回退原图并记异常）；`preUpdateJs` 已在目录抓取前执行，`refreshTocUrl()`/bookUrl 变化会触发重取详情页刷新目录；`formatJs`、`imageStyle`、`payAction` 与登录相关规则仍未接入。

## 3. 鉴权

### 3.1 `POST /api/auth/register`

- 鉴权：公开
- 请求体：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `username` | string | 是 | 唯一；无格式/长度校验 |
| `password` | string | 是 | 直接 bcrypt 哈希（`DEFAULT_COST`），无强度校验 |

示例请求（伪造值）：

```json
{ "username": "alice", "password": "secret123" }
```

- 成功 `200`：

```json
{ "token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.FAKE.FAKE" }
```

| 字段 | 类型 | 可空 |
|---|---|---|
| `token` | string | 否 |

- 错误：
  - `400 {"error":"username exists"}`：用户名已存在。注意这是 `BadRequest` 而非 409。
  - `500 {"error":"db error: ..."}` / bcrypt 错误。
- 行为：
  - 新用户 `role = "user"`；注册成功直接返回 token（不要求再登录）。
  - 首次访问任何需登录接口时，按 `sub` 自动创建 `users/{id}.db` 并跑用户库迁移（[auth.rs](../server/src/auth.rs#L56-L96)）。
  - 不存在的 `/api/auth/me`、`logout`、`refresh` 仍未实现（见 13.11）；自助改密已实现，见 3.3。

### 3.2 `POST /api/auth/login`

- 鉴权：公开
- 请求体：同上 `{username, password}`
- 成功 `200`：`{"token": "..."}`
- 错误：
  - `401 {"error":"unauthorized"}`：用户不存在或密码错误（两种情况不区分，避免枚举用户）。
  - `500`：bcrypt/DB 异常。
- 行为：
  - token 为 HS256 JWT，claims：`sub`（用户 id，i64）、`username`、`role`（`"user"|"admin"`）、`ver`（令牌版本，i64，缺省 0）、`exp`（UTC 秒）（[auth.rs](../server/src/auth.rs#L99-L108)）。
  - 签发时 `ver` 取 `users.token_version`；注册的新用户为 0。
  - 有效期 **7 天**，无刷新机制（[auth.rs](../server/src/auth.rs#L121-L135)）。
  - **每个请求实时校验**：`disabled = 0` 且 `token_version == claims.ver` 才放行；否则 `401 {"error":"unauthorized"}`。因此改密（3.3）或管理员禁用（3.5）后，所有旧 JWT 立即失效，不依赖过期时间。
  - **无 `ver` 的旧令牌按 0 处理**：仅当该用户 `token_version` 仍为 0 时兼容；用户一旦改密或被禁用过，旧令牌即 401。
  - `/k` 的 `k_session` Cookie 使用同一套 claims 与校验，失效时 303 回 `/k/login`。
  - 前端可解码 payload 获取 `role`（现有 `api.js` 即如此），但服务端权限判定始终以签名验证后的 claims 为准。
- **HTTP 证据（2026-11-03 回归报告）**：`report-20261103-1108.md` B8 已验证**禁用后旧 JWT 立即 401、启用后旧 JWT 仍 401、改密后旧 token 立即 401**，且旧密码失效、新密码可登录。新增 `ver` + 每请求查库的失效链路已不再是源码级推断。
- 默认管理员：首次启动 `users` 表为空时创建 `BORANLIB_ADMIN_USER`/`BORANLIB_ADMIN_PASS`（兼容旧 `LEGADO_*`）。开发环境缺省 `admin`/`<寮€鍙戦粯璁ゅ彛浠?`；**生产环境必须显式设置 `BORANLIB_ADMIN_PASS`（至少 8 字节，且不能是 `<寮€鍙戦粯璁ゅ彛浠?`），否则拒绝启动**（[auth.rs](../server/src/auth.rs#L607-L642)）。

### 3.3 `POST /api/auth/password`

- 鉴权：登录（任意角色）
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B8，7/7）：成功改密返回 `relogin_required=true`；**旧 token 立即 401**；旧密码登录 401、新密码登录 200；旧密码错/新密码过短均 400。
- 请求体：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `old_password` | string | 是 | 当前密码，bcrypt 校验 |
| `new_password` | string | 是 | 新密码，**6–128 个字符**（按 Unicode 字符数，不是字节） |

示例（伪造值）：

```json
{ "old_password": "secret123", "new_password": "newSecret456" }
```

- 成功 `200`：

```json
{ "ok": true, "relogin_required": true, "previous_token_version": 3 }
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 true |
| `relogin_required` | boolean | 否 | 恒 true；当前 token 已失效，需重新登录 |
| `previous_token_version` | integer | 否 | 改密前的 `token_version` |

- 错误：
  - `400 {"error":"新密码长度需在 6–128 个字符之间"}`：新密码长度越界（**先校验长度，再校验旧密码**）。
  - `400 {"error":"旧密码不正确"}`：旧密码 bcrypt 不匹配。
  - `401 {"error":"unauthorized"}`：未登录，或当前 token 已失效。
  - `500`：bcrypt/DB 异常。
- 行为：
  - 成功后 `password_hash` 更新、`token_version = token_version + 1`、`password_changed_at = datetime('now')`（UTC）。
  - **当前请求的 token 立即失效**（版本已递增），后续请求 401；`/k` Cookie 同样 303 回登录页。
  - 注册接口（3.1）仍**不校验**密码长度，只有本接口和登录后改密路径校验 6–128。

### 3.4 `GET /api/admin/users`

- 鉴权：登录 + 管理员（非 admin → `403 {"error":"forbidden"}`）
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B8，7/7）：返回 4 个账号（含 admin 与新用户，`disabled` 为布尔）；普通用户访问 403。
- 参数：无
- 成功 `200`：

```json
{
  "ok": true,
  "users": [
    { "id": 1, "username": "admin", "role": "admin", "disabled": false, "created_at": "2026-11-01 08:00:00" },
    { "id": 2, "username": "alice", "role": "user", "disabled": true, "created_at": "2026-11-02 10:00:00" }
  ]
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 true |
| `users` | object[] | 否 | 按 `id` 升序，全量（无分页） |
| `users[].id` | integer | 否 | 用户 id |
| `users[].username` | string | 否 | 用户名 |
| `users[].role` | string | 否 | `"user"` / `"admin"` |
| `users[].disabled` | boolean | 否 | 是否已禁用（DB 存 0/1，输出布尔） |
| `users[].created_at` | string | 否 | 共享库 UTC `datetime('now')` 字符串 |
| `users[].last_login_at` | string \| null | 是 | 最近一次 `POST /api/auth/login` 成功时间（UTC）；从未网页登录为 `null`。`/k` 表单登录不更新该字段 |

- 错误：`401`；`403`；`500`。
- 行为：只读 `shared.db.users`；不返回 `password_hash`、`token_version`、`password_changed_at`。
- `last_login_at` 由登录成功时写入（迁移 `shared/0011_account_last_login.sql`）；写失败不影响登录本身。

### 3.5 `POST /api/admin/users/status`

- 鉴权：登录 + 管理员（非 admin → `403 {"error":"forbidden"}`）
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B8，7/7）：**禁用后旧 JWT 立即 401、登录也 401**；启用后旧 JWT 仍 401（版本已失效），重新登录成功；禁用自己 400。**最后管理员保护仍仅单测**（报告未见单独用例）。
- 请求体：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `user_id` | integer | 是 | 目标用户 id |
| `disabled` | boolean | 是 | `true` 禁用，`false` 启用 |

示例（伪造值）：

```json
{ "user_id": 2, "disabled": true }
```

- 成功 `200`：`{"ok": true, "user_id": 2, "disabled": true}`（原样回显请求值）。
- 错误：
  - `400 {"error":"用户不存在"}`：`user_id` 不在 `users` 表。
  - `400 {"error":"不能禁用当前登录的管理员账号"}`：`user_id` 等于当前管理员。
  - `400 {"error":"不能禁用最后一个启用中的管理员"}`：目标为 admin，且 `role='admin' AND disabled=0` 仅剩 1 个。
  - `401`；`403`；`500`。
- 行为：
  - **禁用**：若目标原本 `disabled=0`，执行 `disabled=1` 且 `token_version = token_version + 1`；若已禁用则**幂等**，不再递增版本。
  - **启用**：只执行 `disabled=0`，**不**还原 `token_version`。因此被禁用前的旧 JWT 仍然 401，用户必须重新登录。
  - 禁用后目标用户的 Bearer 立即 401；`/k` Cookie 立即 303 回 `/k/login`。
  - 不能通过本接口修改角色（没有 `role` 字段），也没有删除用户接口（见 13.11）。

---

## 4. 书源

> 书源是全局共享数据（`shared.db` 的 `book_sources`），所有用户看到同一份；写操作仅管理员。
> 存储方式：同一份完整 JSON 同时写入 `rules` 与 `content` 两列，读取时优先 `content`、回退 `rules`（[sources.rs](../server/src/routes/sources.rs#L32-L45)、[shared/0002_extend.sql](../server/migrations/shared/0002_extend.sql)、[shared/0003_backfill_content.sql](../server/migrations/shared/0003_backfill_content.sql)）。

### 4.1 `GET /api/sources`

- 鉴权：任意登录用户
- 参数：无
- 成功 `200`：数组，元素为 `BookSourceListItem`（**snake_case**）：

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `book_source_url` | string | 否 | 书源唯一 ID（DB 的 `id`） |
| `book_source_name` | string | 否 | 名称 |
| `enabled` | boolean | 否 | 是否启用 |
| `book_source_group` | string | 是 | 分组 |
| `weight` | integer | 否 | 越大越靠前 |
| `custom_order` | integer | 否 | 越小越靠前 |
| `rule_error_count` | integer | 否 | 该源累计“字段规则异常”次数（内存台账，进程重启清零） |
| `last_rule_error_at` | integer | 是 | 最近一次规则异常的 Unix 秒；无异常为 `null` |

示例（伪造值）：

```json
[
  {
    "book_source_url": "https://example.com",
    "book_source_name": "示例书源",
    "enabled": true,
    "book_source_group": "默认",
    "weight": 10,
    "custom_order": 0
  }
]
```

- 错误：`401`；`500`（书源 JSON 全部解析失败时不会报错，而是被静默过滤，可能返回空数组）。
- 行为：
  - 只返回摘要，不含规则；详情用 `/api/sources/detail`。
  - 排序：`weight` 降序 → `custom_order` 升序 → `book_source_name` 升序（[sources.rs](../server/src/routes/sources.rs#L70-L95)）。
  - 参数 `enabled` 不做过滤，前端自行筛。
  - 解析失败的行被 `filter_map` 丢弃（不会让整个接口失败）。

### 4.2 `POST /api/sources`

- 鉴权：**仅 admin**
- 请求体：单个完整书源，camelCase 或 snake_case 均可（推荐 Legado camelCase）。
- 必填字段：`bookSourceUrl`（string）、`bookSourceName`（string）。其余可选，`enabled` 缺省 `true`。
- 查询参数（可选）：

| 参数 | 类型 | 说明 |
|---|---|---|
| `probe` | string | `1`/`true`/`yes`/`on` 时保存后立即做**一次“搜索”探测**（不跑目录+首图），结果写入健康表 |
| `key` | string | 探测关键词，默认“测试” |

- 示例（伪造值，camelCase）：

```json
{
  "bookSourceUrl": "https://example.com",
  "bookSourceName": "示例书源",
  "bookSourceGroup": "默认",
  "enabled": true,
  "weight": 10,
  "searchUrl": "https://example.com/search?q={{key}}&page={{page}}",
  "ruleSearch": {
    "bookList": ".result .item",
    "name": ".title",
    "author": ".author",
    "bookUrl": "a@href"
  }
}
```

- 成功 `200`：
  - 不带 `probe`：`{ "ok": true }`。
  - `probe=1`：追加 `probe_results`（数组）与 `probe_summary`（对象），见 4.3 的同名字段；单条探测不受 90s 批预算限制，仍受单源搜索预算约束。

```json
{
  "ok": true,
  "probe_results": [
    {
      "book_source_url": "https://example.com",
      "name": "示例书源",
      "probe_status": "ok",
      "latency_ms": 812,
      "error": null,
      "consecutive_failures": 0,
      "cooldown_until": null,
      "engine_limited": false,
      "suggested_enabled": true
    }
  ],
  "probe_summary": { "probed": 1, "ok": 1, "fail_site": 0, "engine_limited": 0, "kept_enabled": 1, "disable_suggested": 0, "skipped": 0, "elapsed_ms": 812, "budget_exhausted": false }
}
```

- 错误：
  - `400 {"error":"invalid source: ..."}`：反序列化失败（缺 `bookSourceUrl`/`bookSourceName`、类型不符、缩写键名被拆坏等）。
  - `400 {"error":"json: ..."}`：JSON 语法错误。
  - `401` / `403 {"error":"forbidden"}`。
- 行为：
  - 按 `book_source_url` upsert；同时写 `name`、`url = book_source_url`、`rules`、`content`、`enabled`。
  - `updated_by` 固定写 `'admin'`，不记录真实用户名（[sources.rs](../server/src/routes/sources.rs#L103-L121)）。
  - 其他字段（权重、分组、规则）全部在 JSON 内，不在 DB 列上。
  - 不做校验（`validate` 是独立接口），非法 URL 也可保存。
  - **没有 `PUT /api/sources`**：保存/更新统一走本接口的 `POST` 覆盖语义；按 `book_source_url` upsert，整份 JSON 覆盖。**多标签页/多人同时编辑同一书源时，后提交者覆盖前者**，没有版本号、`If-Match` 或字段级合并，也没有并发保护。
  - `probe=1` 只**观测并建议**，不改 `enabled`，也不写导入账本（账本只由 4.3 写入）；`suggested_enabled=false` 仅表示“源站不可达”。

### 4.3 `POST /api/sources/import`

- 鉴权：**仅 admin**
- 请求体：`BookSource[]`，或单个 `BookSource`（自动包装为数组）。元素可为 camelCase 或 snake_case。
- 查询参数（均可选）：

| 参数 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `probe` | string | 开 | `0`/`false`/`no`/`off` 关闭体检；其余视为开。开启时只探测“新增 / 规则变化 / 隔离区待复活”的源 |
| `source` | string | `inline` | 导入来源标识（合集 URL、文件名或自定义标签）。**再导入必须用同一个 `source`**，否则无法做 diff |
| `source_kind` | string | `inline` | `collection` / `file` / `api` / `inline`，仅记账 |
| `force` | string | 关 | `1` 时人工确认覆盖 `update_rejected` / `conflict` |
| `key` | string | 测试 | 探测关键词 |

- 成功 `200`（示例为节选）：

```json
{
  "ok": true,
  "batch_id": "1793356800000-1a2b3c4d",
  "source": "boranlib-sources.json",
  "source_kind": "file",
  "collection_unchanged": false,
  "count": 107,
  "inserted": 23,
  "added": 23,
  "updated": 0,
  "unchanged": 84,
  "update_rejected": 0,
  "conflict": 0,
  "resurrected": 0,
  "local_only": 0,
  "local_only_urls": [],
  "newly_quarantined": 0,
  "force": false,
  "disabled_unsupported": 0,
  "distinct_hosts": 74,
  "probe_summary": {
    "probe_total": 107, "probed": 23, "unprobed": 84, "ineligible": 0, "probe_timed_out": false,
    "ok": 22, "fail_site": 0, "engine_limited": 1,
    "kept_enabled": 23, "disable_suggested": 0,
    "probe_elapsed_ms": 9700, "probe_budget_secs": 90, "probe_skipped_same_collection": false
  },
  "probe_results": [
    {
      "book_source_url": "https://example.com",
      "name": "示例书源",
      "probe_status": "ok",
      "engine_limited": false,
      "latency_ms": 812,
      "error": null,
      "suggested_enabled": true,
      "decision": "active",
      "category": "added",
      "reason": null,
      "consecutive_failures": 0,
      "cooldown_until": null
    }
  ]
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `batch_id` | string | 导入批次 ID（写入 `import_batches` / `import_items`） |
| `collection_unchanged` | boolean | 整份请求体规范化哈希与同 `source` 上次批次相同；为 true 时**一个探测请求都不发** |
| `count` | integer | 批内去重后的条目数 |
| `inserted` / `added` | integer | 本地原本不存在，写入活跃库 |
| `updated` | integer | 规则变化、新版体检通过且版本不旧于本地，已覆盖 |
| `unchanged` | integer | 规则相同；或规则变化但本地版本不旧于上游（保留本地） |
| `update_rejected` | integer | 新版体检失败而本地旧版仍可用，**未覆盖** |
| `conflict` | integer | 本地规则被人工改过（与账本快照哈希不一致），**未覆盖** |
| `resurrected` | integer | 隔离区复检通过，回到 `active` |
| `local_only` / `local_only_urls` | integer / string[] | 同 `source` 上次批次有、本次没有的 URL；默认保留，不自动删 |
| `newly_quarantined` | integer | 本次新进入隔离区的条数（站点级失败） |
| `disabled_unsupported` | integer | 导入时因类型不受支持而自动 `enabled=false` 的条数 |
| `distinct_hosts` | integer | 本次条目的**去重站点数**（仅统计口径；健康与剔除仍按 `bookSourceUrl`） |
| `probe_summary` | object | `probe_total` / `probed` / `unprobed` / `ineligible` / `probe_timed_out` / `ok` / `fail_site` / `engine_limited` / `kept_enabled` / `disable_suggested` / `probe_elapsed_ms` / `probe_budget_secs`（导入接口另有 `probe_skipped_same_collection`） |
| `probe_results[]` | object[] | 逐条：`book_source_url`、`name`、`probe_status`(`ok`/`fail`/`never`)、`engine_limited`、`latency_ms`、`error`、`suggested_enabled`、`decision`(`active`/`quarantined`)、`category`（同上分类）、`reason`、`consecutive_failures`、`cooldown_until` |

- 错误：任一元素解析失败 → `400 invalid source: ...`，**整批不写入**（先解析全部，再落库）。
- 行为（三层模型）：
  - **A 本地源库**：`book_sources` 仍是唯一搜索真相；活跃源按 `bookSourceUrl` 精确归一，不区分来自哪个合集。
  - **B 导入账本**：`import_batches` 记录 `batch_id`、`source`、`source_kind`、`imported_at`、`item_count`、`raw_sha`、`probe_summary`；`import_items` 记录每条 URL、**完整原始定义快照**、规则哈希、`probe_status`、`decision`、`reason`。账本不进搜索路径，只用于追溯 / diff / 回滚。
  - **C 隔离区**：站点级失败（`FAIL_SOURCE`）的源 `decision=quarantined`，不参与搜索、不再反复巡检，但**原始定义保留**；再次导入同一 `source` 且体检通过 → 自动复活并计入 `resurrected`。**引擎性失败（`FAIL_ENGINE` / `engine_limited`）绝不进隔离区**，只标记“待引擎支持”。
  - **D 再导入 = diff**：按 `bookSourceUrl` 精确匹配；规则内容用**规范化 JSON 的 SHA-256** 比较（排除 `enabled`/`name`/`group`/`weight`/`updateTime`），不只看 URL。
  - 覆盖三态：新版体检通过 → 覆盖；新版站点失败且本地旧版可用 → `update_rejected`；本地被人为改过 → `conflict`。`force=1` 才是显式越权。
  - 版本口径：同 URL 规则不同时，若本地仍可用且上游 `lastUpdateTime` 不更晚（含相同），保留本地，避免每次导入都抖动换版本。
  - 去重与探测：同 URL 的多个变体各自独立；探测按 `bookSourceUrl` 去重，并发 ≤8，整批预算 90 秒；超时后返回“已体检 x/y，其余 `probe_status=never`”。体检**只跑一次搜索**，不跑目录+首图。
  - 批内按 `book_source_url` 去重，**后者覆盖前者**；导入写库为逐条 upsert，非单一事务。


### 4.4 `POST /api/sources/enabled`

- 鉴权：**仅 admin**
- 请求体：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `url` | string | 否（与 `urls` 至少一个） | 单个书源 ID |
| `urls` | string[] | 否（与 `url` 至少一个） | 批量书源 ID |
| `enabled` | boolean | 是 | 目标状态 |

示例（伪造值）：

```json
{ "urls": ["https://a.example", "https://b.example"], "enabled": false }
```

- 成功 `200`：

```json
{ "ok": true, "enabled": false, "updated": 2, "missing": ["https://c.example"] }
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 true |
| `enabled` | boolean | 否 | 回显目标状态 |
| `updated` | integer | 否 | 成功更新数 |
| `missing` | string[] | 否 | 找不到或 JSON 解析失败的 ID |

- 错误：
  - `400 {"error":"url or urls required"}`：两个 ID 字段都为空。
  - `401` / `403`；`500`。
- 行为：
  - 逐个 `load_source` → 改 `enabled` → 整份 `store_source` 回写（会刷新 `updated_at`）。
  - 不存在/损坏的 ID 不报错，收集进 `missing`（[sources.rs](../server/src/routes/sources.rs#L204-L235)）。
  - 重复 ID 会重复更新并重复计数。

### 4.5 `GET /api/sources/export`

- 鉴权：任意登录用户
- 参数：无
- 成功 `200`：`BookSource[]`（**camelCase**，含 `ruleSearch` 等嵌套对象；未知字段原样返回）。
- 错误：`401`；`500`。
- 行为：
  - **排除隔离区**：`decision=quarantined` 的源一律不导出，避免把源里自带的认证头（如 `authorization`）再次分发；`active` 源照常导出。
  - 顺序**未定义**：`SELECT rules, content FROM book_sources` 没有 `ORDER BY`（[sources.rs](../server/src/routes/sources.rs#L48-L57)）。前端如需稳定顺序请自行排序。
  - 解析失败的行会被丢弃。
  - 导出内容可直接交给 Legado 导入。

### 4.6 `GET /api/sources/detail`

- 鉴权：任意登录用户
- 查询参数：

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `url` | string | 是 | 书源 ID（`book_source_url`） |

- 成功 `200`：完整 `BookSource`（**snake_case**）。示例（伪造值，节选）：

```json
{
  "book_source_url": "https://example.com",
  "book_source_name": "示例书源",
  "book_source_group": "默认",
  "enabled": true,
  "weight": 10,
  "custom_order": 0,
  "search_url": "https://example.com/search?q={{key}}",
  "timeout_millisecond": 0,
  "rule_search": {
    "book_list": ".result .item",
    "name": ".title",
    "author": ".author",
    "book_url": "a@href"
  },
  "explore_url": null,
  "rule_explore": null,
  "extra": {}
}
```

> `timeout_millisecond`（导出 camelCase `timeoutMillisecond`）是 **oread 扩展字段**，表示单源 HTTP 超时，不是 Legado `ReplaceRule` 的同名正则超时。
>
> - `0` / 缺失 / 非法值 → 回退全局 `HTTP_TIMEOUT_SECS`（默认 15s，环境变量范围 3–60）。
> - 有效值保留 **100ms–120000ms**（取消旧的 1–60s 人为夹取，中间值原样生效；仅保留 100ms 极小值保护与 120s 全局上限兜底）；覆盖搜索、详情、目录、正文与图片回源（图片未设置时沿用自身 5s 预算）。
> - 存储在 `book_sources.content` JSON 中，无独立列，无需迁移；Legado 导入通常不带此键，缺失即按 0。

- 错误：
  - `400`：缺 `url`（Axum 纯文本）。
  - `404 {"error":"source not found"}`：不存在或两列 JSON 都解析失败。
- 行为：
  - 读取时先尝试 `content`，再尝试 `rules`（修复历史“详情永远为空”的问题）。两列都为空/损坏才 404（[sources.rs](../server/src/routes/sources.rs#L35-L67)）。
  - 返回 snake_case，不是 export 的 camelCase。
  - **状态：已完成待验证**（修复后需用真实旧数据回归，见 TODO）。

### 4.7 `POST /api/sources/validate`

- 鉴权：任意登录用户
- 请求体（可选）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `source` | object | 否 | 待校验书源；缺省时把整个 body 当书源 |
| `check_url` | boolean | 否 | 是否做 URL 可达性检查，默认 `false` |

- 注意：外层信封字段是 **`source` / `check_url`（snake_case）**，不做 camelCase 转换；只有 `source` 内部的键会做 camel→snake。
- 示例（伪造值）：

```json
{
  "source": {
    "bookSourceUrl": "https://example.com",
    "bookSourceName": "示例书源",
    "searchUrl": "https://example.com/search?q={{key}}",
    "ruleSearch": { "bookList": ".result .item" }
  },
  "check_url": false
}
```

- 成功 `200`（即便不合法也返回 200）：

```json
{ "valid": true, "errors": [], "warnings": [] }
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `valid` | boolean | 否 | `errors` 为空即 true |
| `errors` | string[] | 否 | 阻塞保存的问题 |
| `warnings` | string[] | 否 | 非阻塞提示 |

- 错误：仅 `401`；解析失败也返回 200 且 `valid=false`（`errors` 内为 `invalid source: ...`）。
- 校验规则（按源码逐条）：
  1. `bookSourceUrl` 必须以 `http://` 或 `https://` 开头，否则 error。
  2. `bookSourceName.trim()` 非空，否则 error。
  3. `searchUrl` 与 `exploreUrl` 都为空 → warning“不可用于搜索/发现”。
  4. 配置了 `searchUrl` 但 `ruleSearch.bookList` 为空 → error。
  5. 存在 `ruleToc` 但 `chapterList` 为空 → error。
  6. 存在 `ruleContent` 但 `content` 为空 → error。
  7. 配置了 `exploreUrl` 但没有 `ruleExplore` → error。
  8. `check_url=true` 且无 error 时，GET `bookSourceUrl`，成功/失败都只进 `warnings`（[sources.rs](../server/src/routes/sources.rs#L255-L318)）。
- 行为：不落库、不修改任何数据。

### 4.8 `DELETE /api/sources/delete`

- 鉴权：**仅 admin**
- 查询参数：`url`（string，必填，书源 ID）
- 成功 `200`：`{"ok": true}`
- 错误：缺 `url` → 400（文本）；`401`；`403`；`500`。
- 行为：直接 `DELETE`，不检查是否存在，不级联清理任何用户数据或缓存。
  - 结果 `ok:true` 可能表示“删了 0 行”（[sources.rs](../server/src/routes/sources.rs#L321-L332)）。

### 4.9 `POST /api/sources/check-updates`

- 鉴权：登录（**不要求 admin**）
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B5，4/4）：`checked=4`，`updated=1`/`local_newer=1`/`same=1`/`new=1` 四类各 1，`has_updates=true`、`errors=[]`；比对后本地未写库（只读）；空数组 `checked=0`。
- 语义：**只读**。拿一份 Legado 书源包与本地 `book_sources` 按 `bookSourceUrl` 比对，不写库、不 fetch 远端。
- 请求体：三种形态均可（`Content-Type: application/json`）：

| 形态 | 示例 | 说明 |
|---|---|---|
| 裸数组 | `[{"bookSourceUrl":"https://a.com","bookSourceName":"A","lastUpdateTime":101}]` | Legado 导出数组 |
| 包装对象 | `{"sources":[...]}` | 只取 `sources` 键 |
| 单条对象 | `{"bookSourceUrl":"https://a.com",...}` | 视为一条 |
| `null` / `{}` / `{"sources":null}` | — | 视为空列表，`checked=0`，不报错 |

- 每条书源先走与 `POST /api/sources/import` 相同的 `camelCase → snake_case` 规整；单条解析失败不中止，记入 `errors[]`。
- 成功 `200`：

```json
{
  "ok": true,
  "checked": 4,
  "errors": ["第 3 条: invalid book source: missing field `bookSourceUrl`"],
  "updated": [
    { "book_source_url": "https://a.com", "book_source_name": "A", "local_update_time": 100, "incoming_update_time": 101 }
  ],
  "new": [
    { "book_source_url": "https://d.com", "book_source_name": "D", "local_update_time": 0, "incoming_update_time": 50 }
  ],
  "local_newer": [
    { "book_source_url": "https://b.com", "book_source_name": "B", "local_update_time": 200, "incoming_update_time": 199 }
  ],
  "same": 1,
  "has_updates": true
}
```

> 上述 JSON 为**伪造示例值**；`errors` 文案随解析错误变化。

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 `true` |
| `checked` | integer | 否 | 成功规整的**入参数目**（不含 `errors` 中的条目） |
| `errors` | string[] | 否 | 单条解析失败；无错误时为 `[]` |
| `updated` | object[] | 否 | 本地存在且 `incoming_update_time > local_update_time` |
| `new` | object[] | 否 | 本地不存在（`local_update_time` 恒 `0`） |
| `local_newer` | object[] | 否 | 本地存在且 `incoming_update_time < local_update_time` |
| `same` | integer | 否 | 时间完全相等（或两侧都为 0）的条数 |
| `has_updates` | boolean | 否 | `updated` 或 `new` 非空 |

- 列表项 `SourceUpdateItem`：

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `book_source_url` | string | 否 | 比对键 |
| `book_source_name` | string | 否 | 入参名为空时回退本地名（`new` 项沿用入参，可能为空串） |
| `local_update_time` | integer | 否 | 本地 `last_update_time`；不存在为 `0` |
| `incoming_update_time` | integer | 否 | 入参 `lastUpdateTime` |

- 错误：仅框架层——非法 JSON → 400/422 纯文本；`401`；`500`。业务层不因单条坏数据失败（见 `errors`）。
- 行为：
  - 按 `book_source_url` 精确匹配，不做 trim/归一化；同一 URL 在入参里出现多次会重复计入。
  - 本地比对集合与 `GET /api/sources` 相同（`book_sources` 全表）。
  - 时间比较只看 `lastUpdateTime`；缺失时按实体默认 `0`，负值原样参与比较。
  - 只给“分类结果”，实际导入仍需调用 `POST /api/sources/import`（该接口写库，见 4.3）。

### 4.10 `POST /api/sources/health/check`

- 鉴权：任意登录用户
- 查询参数：

| 参数 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `source_ids` | string | 全部 | 逗号分隔的 `bookSourceUrl`；缺省 = 所有 `enabled` 且可搜索、非隔离的源 |
| `only_due` | string | 关 | `1` 时只测“从未测过”或“距上次检测已满冷却期”的源 |
| `key` | string | 测试 | 搜索探测关键词 |

- 探测方式：**只发一次搜索请求**（复用单源搜索预算，默认 10s），不跑目录+首图；并发 ≤8，整批 90s 预算。
- 成功 `200`：

```json
{
  "ok": true,
  "selected": 5,
  "checked": 5,
  "elapsed_ms": 10120,
  "budget_secs": 90,
  "cooldown_secs": 86400,
  "fail_threshold": 3,
  "results": [
    {
      "source_url": "http://www.botaodz.com/",
      "name": "快眼看书botaodz",
      "status": "fail",
      "engine_limited": false,
      "latency_ms": 10024,
      "error": "error sending request: ...",
      "consecutive_failures": 3,
      "cooldown_until": 1793443200000
    }
  ]
}
```

| 字段 | 说明 |
|---|---|
| `selected` / `checked` | 入选源数 / 实际完成探测数（预算超时后小于 `selected`） |
| `status` | `ok`（搜索成功）/ `partial`（站点可达但规则失败）/ `fail`（站点不可达） |
| `engine_limited` | `status=partial` 时为 true；这类失败**不计入连续失败、不冷却** |
| `cooldown_until` | 冷却结束的 epoch 毫秒；未冷却为 `null` |

- 错误：`401`；`500`。
- 行为：结果批量 upsert 到 `source_health`；连续 3 次站点级失败写 `cooldown_until = now + SOURCE_COOLDOWN_SECS`（默认 24h）。
- 搜索产出证据（迁移 `0015`）：真实搜索（`usage=true`）除 `hit_count`（解析条数）外，还累计
  `kept_hit_count`（书名匹配命中条数）与 `yield_ok_count`（`kept_hits > 0` 的次数），
  用于搜索入队的「每秒有效结果」估计；体检/巡检不写这两列。`evidence_json` 同时暴露
  `kept_hit_count` / `yield_ok_count` / `yield_rate`。设计见 [DESIGN_SEARCH.md](DESIGN_SEARCH.md)。

### 4.11 `GET /api/sources/health`

- 鉴权：任意登录用户
- 查询参数：`detail`（string，可选，`1`/`true` 返回逐源明细）。
- 新增观测字段：顶层 `rule_anomaly_sources`（曾有字段规则异常的源数，内存台账）；`detail=1` 时逐源附 `rule_error_count`/`last_rule_error_at`（语义见 4.1）。规则异常不等于源失败：只是某个字段降级为空，源是否健康仍由网络/空解析结果决定。
- 成功 `200`：

```json
{
  "ok": true,
  "total": 184,
  "distinct_hosts": 118,
  "active": 179,
  "quarantined": 5,
  "ok": 32,
  "partial": 2,
  "fail": 4,
  "cooling": 1,
  "never": 146,
  "cooldown_secs": 86400,
  "fail_threshold": 3,
  "now": 1793356800000,
  "detail": false,
  "sources": null
}
```

| 字段 | 说明 |
|---|---|
| `total` / `distinct_hosts` | 源条目数 / **去重后的站点数**（`bookSourceUrl` 的 host 仅作统计，绝不作健康键） |
| `active` / `quarantined` | 本地库活跃数 / 隔离区数 |
| `ok` / `partial` / `fail` / `never` | 最近一次健康状态分布（`never` = 从未探测） |
| `cooling` | 当前仍在冷却中的源数 |
| `sources` | `detail=1` 时为明细数组：`book_source_url`、`name`、`enabled`、`last_status`、`last_checked_at`、`last_error`、`consecutive_failures`、`last_ok_at`、`cooldown_until`、`avg_latency_ms`、`cooling`、`decision`、`review_state`；否则 `null` |

- 错误：`401`；`500`。

### 4.12 `POST /api/sources/health/reset`

- 鉴权：**仅 admin**
- 请求体（可选）：`{"source_ids":["..."],"url":"..."}`；两者都为空 = 清空全部。
- 成功 `200`：`{"ok":true,"cleared":1,"scope":"selected"}`（`scope` 为 `selected` / `all`）。
- 错误：`401`；`403`；`500`。
- 用途：用户手动“重新信任”某个源；清零连续失败与冷却，不改变 `decision`。

### 4.13 `POST /api/sources/prune`

- 鉴权：**仅 admin**
- 查询参数 / 请求体（同名，body 优先）：`within_days`（默认 7，clamp 1..365）、`action`（`quarantine` 默认 / `disable`）、`confirm`（`1`/`true`）。
- 候选条件：`last_status='fail'`（站点级失败）、`last_checked_at` 在最近 N 天内、且不在隔离区。**`partial`/`engine_limited` 永久排除**；只按 `bookSourceUrl` 操作，**禁止按域名批量**。
- 预览（不带 `confirm`）：

```json
{
  "ok": true,
  "confirm_required": true,
  "within_days": 7,
  "action": "quarantine",
  "would_affect": 4,
  "affected_hosts": 4,
  "engine_limited_excluded": true,
  "sources": [ { "book_source_url": "...", "name": "...", "enabled": true, "probe_status": "fail_site", "last_status": "fail", "last_checked_at": 1793356800000, "consecutive_failures": 2, "last_error": "...", "cooldown_until": null } ]
}
```

- 确认执行（`confirm=1`）：`{"ok":true,"confirm_required":false,"affected":4,"failed":0,"affected_hosts":4,"physical_delete":false,...}`。
- 错误：`action=delete` → `400`（不提供物理删除，隔离保留原始定义）；`401`；`403`；`500`。
- 行为：`quarantine` 只改 `decision`，不删 `book_sources` 行、不删账本快照；`disable` 额外写 `enabled=false`。

### 4.14 `POST /api/sources/quarantine`

- 鉴权：**仅 admin**
- 请求体：`{"source_ids":["..."],"reason":"manual"}`（`reason` 可选）。
- 成功 `200`：`{"ok":true,"affected":1,"missing":[],"reason":"manual"}`。
- 错误：`401`；`403`；`500`。
- 行为：逐条移入隔离区，保留原始定义；不存在的 URL 收集到 `missing`；重复导入同一合集且体检通过可复活。

### 4.15 书源体检与精选集

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/sources/bench/run` | 触发体检；body `{"source_ids":"all"}` 或 `{"source_ids":["<book_source_url>", ...]}`，可选 `limit`（每源样本数，默认 5） |
| GET | `/api/sources/bench/run/{run_id}` | 轮询进度：`status`(`running`/`done`/`failed`)、`sources_total`、`sources_done`、`curated_total`、`novel_total`、`comic_total`、`elapsed_ms` |
| GET | `/api/sources/bench/curated` | 当前精选清单（`novel[]` / `comic[]`，含 `rank`、`name`、`bench_score`、`samples`、`cut_gap`） |
| GET/POST | `/api/sources/bench/corpus`、`/corpus/update`、`/corpus/toggle` | 查看 / 追加 / 停用语料（内置条目只能停用，不能删除） |

判分与入选规则（运行时判定，每次体检 / 搜索结果变化后都会重算）：

- **语料抽样**：内置 24 条（小说 12、漫画 12），按 `category` 轮转抽样；每次抽样 `salt` 同时旋转 category 顺序与**类内起点**，跨源累计能覆盖全部 24 条。漫画拆为 `日漫` / `国漫` 两个 category。
- **不计分条目**：出版/实体书（《活着》《平凡的世界》《三体》）标记 `scored=false` —— 仍会跑、仍落库，但不进入 `samples` / `hits` / `hit_rate` / 分数分母（2026-11 实测命中率仅 3.0%、13.6%，会把所有网文源压到 0.6 上限）。
- **命中口径**：每条语料看搜索结果**前 5 条**；`is_hit` 不放宽（近似同名，或包含 + 作者归一化一致）。只有衍生作/同人时仍判未命中。
- **分数**：`bench_score = 60×hit_rate + 30×latency_score + 10×success`，其中 `latency_score = 1 − avg_latency/5000ms`。
- **回写健康**：每批体检对每个源折算 **1 条**观测（有响应 → `ok`、全失败 → `partial`；`usage=true`，命中/搜索计数进入健康分）。**冷启动直通**：`samples>=2 && hit_rate>0 && bench_score>=50 && 未隔离` 的源直接授予 `preferred`（`promote_streak` 抬到 promote 阈值），使“导入 → 跑一次体检 → 精选有内容”成为可能；真实搜索的 `demote_streak` 降级仍生效。
- **精选资格**：`tier=preferred` + `samples>=2` + `fail_streak<2` + `hit_rate>0`，按 `bench_score` 降序，小说取 8–14、漫画取 3–14，相邻分数差 ≥8 时断层截断。
- 全新库不会自动体检（`last_bench_at` 为空时不触发 72h 巡检），需手动触发一次；之后每 72h 视情况自动巡检。

---

## 5. 搜索与发现

> 所有搜索/发现接口只返回“书源条目摘要”，不写库、不产生书架记录；同名同作者会去重。抓取失败可能表现为 `400 internal: ...`（详情/目录/正文）或静默忽略（搜索）或 `errors` 数组（聚合搜索）。
> 搜索有两套入口：同步的 5.1/5.2（一次返回完整结果），以及两段式 5.5/5.6（先创建 job 再轮询，适合多源长搜索与“先到先渲染”）。

### 5.1 `GET /api/search`

- 鉴权：登录
- 查询参数：

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `key` | string | 否 | — | 正式参数名；`trim()` 后使用 |
| `q` | string | 否 | — | `key` 的别名；仅在未传 `key` 时生效（传了空 `key` 也不会回退到 `q`） |
| `page` | integer | 否 | 1 | 最小 1；仅用于本地切片，不传给书源 `{{page}}`（书源固定取第 1 页） |
| `page_size` | integer | 否 | 30 | clamp 1..200 |
| `include_cooled` | string | 否 | 关 | `1`/`true` 时让冷却中的源也参与（排查用）；默认跳过冷却源与隔离区源 |

> `SearchQ` 为 5.1/5.2/5.5 共用；`key`/`q` 都不传时按空串搜索，通常是 `200 []`（已实测），**不再返回 400**。
> 5.1/5.2 默认只打**活跃源**：`enabled && rule_search && search_url`、非隔离区、且不在冷却期；`include_cooled=1` 才会把冷却源加回来。
> **服务端书名过滤（5.1/5.2/5.5/5.6/`/k/search` 全入口共用）**：结果在交给客户端前只保留**书名包含关键词**的条目；只比对书名，作者/简介不参与。比对前做归一化：全角→半角、忽略大小写、剔除空白与标点（《》、`·`、括号等），因此《百年孤独》与“百年孤独”等价。不做繁简转换，也不为别名/译名/副标题保留，字面不匹配直接丢弃。过滤在去重/排序之后执行，不改变打分与精选；被过滤不等于源失败；全部被过滤时正常返回空列表。搜索入队使用过滤后的产出证据（`kept_hits`）排序，见 [DESIGN_SEARCH.md](DESIGN_SEARCH.md)。

- 成功 `200`：`SearchBook[]`（**无 `total`、无分页元数据**）
- `SearchBook` 结构（响应对所有字段都会出现，未填充的为 `null`/`0`）：

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `book_url` | string | 否 | 绝对 URL |
| `origin` | string | 是 | 书源 ID（`book_source_url`） |
| `origin_name` | string | 是 | 书源名 |
| `type` | string | 是 | 内部字段 `book_type`，序列化为 `type`；当前不填充 |
| `name` | string | 是 | 书名 |
| `author` | string | 是 | 作者 |
| `kind` | string | 是 | 分类 |
| `cover_url` | string | 是 | 封面 |
| `intro` | string | 是 | 简介 |
| `word_count` | string | 是 | 当前不填充 |
| `latest_chapter_title` | string | 是 | 最新章节名 |
| `toc_url` | string | 是 | 当前不填充 |
| `time` | string | 是 | 当前不填充 |
| `variable` | string | 是 | 当前不填充 |
| `origin_order` | integer | 否 | 当前恒 0 |
| `chapter_word_count_text` | string | 是 | 当前不填充 |

示例（伪造值）：

```json
[
  {
    "book_url": "https://example.com/book/123",
    "origin": "https://example.com",
    "origin_name": "示例书源",
    "type": null,
    "name": "示例小说",
    "author": "张三",
    "kind": "玄幻",
    "cover_url": "https://example.com/cover/123.jpg",
    "intro": "一段简介",
    "word_count": null,
    "latest_chapter_title": "第 300 章",
    "toc_url": null,
    "time": null,
    "variable": null,
    "origin_order": 0,
    "chapter_word_count_text": null
  }
]
```

- 错误：缺 `key`/`q` → `200 []`（已实测；不再 400）；`401`；上游抓取失败被**静默丢弃**（不会 400/500，可能返回空数组）。
- 行为/坑：
  - 分页基于 60s 缓存的完整聚合结果（见 2.3），`page>=2` 稳定；但只能翻到各源第 1 页去重后的前 400 本。
  - 去重 + 排序见 2.4。
  - 搜索参与源：`enabled && rule_search && search_url`；`enabled_explore` 不参与搜索。

### 5.2 `GET /api/search/aggregate`

- 鉴权：登录
- 查询参数：同 5.1（`key` 或别名 `q`、`page`、`page_size`），`key` 会 `trim()` 后使用并作为缓存键；缺 `key`/`q` 时按空串搜索并照常返回 `200`（可能带大量源失败 `errors`）。
- 成功 `200`：

```json
{
  "books": [ /* SearchBook[]，见 5.1 */ ],
  "errors": [
    { "source": "https://bad.example", "source_name": "坏源", "error": "error sending request", "kind": "site" }
  ],
  "page": 1,
  "page_size": 30,
  "total": 87,
  "title_filter": { "raw": 240, "kept": 87, "dropped": 153 },
  "has_more": true,
  "source_failed": 1,
  "source_skipped": 0,
  "source_total": 5,
  "include_cooled": false,
  "skipped_cooldown": 2,
  "cooldown_total": 3,
  "cache_ttl_secs": 60
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `books` | SearchBook[] | 否 | 当前页数据 |
| `errors` | object[] | 否 | 单源失败明细：`source`(string)、`source_name`(string)、`error`(string)、`kind`(string) |
| `page` | integer | 否 | 回显（最小 1） |
| `page_size` | integer | 否 | 回显（1..200） |
| `total` | integer | 否 | 书名过滤后、去重排序的完整结果数，**最多 400**；只基于各源第 1 页 |
| `title_filter` | object | 否 | `raw`=过滤前候选数、`kept`=过滤后（= `total`）、`dropped`=书名不含关键词而丢弃的条数 |
| `has_more` | boolean | 否 | `skip + page_size < total` |
| `source_failed` | integer | 否 | **数字**，真实失败源数量（不含 `truncated`/`skipped`）。明细看 `errors` |
| `source_skipped` | integer | 否 | **数字**，因全局预算耗尽未执行/未完成的源数量（不算站点故障） |
| `source_total` | integer | 否 | 本次实际参与搜索的活跃源数（已排除隔离区与冷却源；`source_ids` 过滤后） |
| `include_cooled` | boolean | 否 | 回显是否放行冷却源 |
| `skipped_cooldown` | integer | 否 | 因冷却被跳过的源数（`source_ids` 过滤后计数） |
| `cooldown_total` | integer | 否 | 本地全部处于冷却中的源数（不受 `source_ids` 影响） |
| `cache_ttl_secs` | integer | 否 | 恒 60 |

- 错误：缺 `key` → 400 纯文本；`401`；`500`。
- **`source_failed` / `source_skipped` 与 `errors` 的关系**：两者都是**整数计数**；失败明细在 `errors[]`，每项 `{source, source_name, error, kind}`。`kind` 为 `site`/`engine`/`timeout`/`truncated`/`skipped`；`source_failed` 只统计前三类，后两类（全局预算耗尽）计入 `source_skipped`。前端不要按数组解析计数字段。
- 行为（详细）：
  - 先查进程级缓存（`key` 精确匹配，TTL 60s，最多 4 个查询，每查询最多 400 本）；命中则直接对完整结果切片（[books.rs](../server/src/routes/books.rs#L68-L171)）。
  - 未命中：并发搜索所有源**第 1 页** → 去重排序 → `truncate(400)` → 仅当非空才写缓存。
  - 空结果不缓存，因此“冷查询”每次都会打全部源。
  - 缓存全局共享，不区分用户（结果与用户无关）。
  - 分页稳定：同一 `key` 60s 内翻页不重不漏；超过 TTL 后重新抓取，`total` 可能变化。
  - `total` 不是全站总数，只是“本次聚合的可用结果数（≤400）”。

### 5.3 `GET /api/explore`

- 鉴权：登录
- 查询参数：

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `url` | string | 是 | ① 书源 ID（`book_source_url`）或 ② `http(s)` 分类 URL |
| `page` | integer | 否 | 默认 1；仅在书源 ID 分支用于展开 `{{page}}` |

- 行为分两种：

**A. `url` 以 `http://`/`https://` 开头（分类 URL 模式）**

- 并发查询所有 `enabled && enabled_explore && rule_explore != null` 的书源，统一请求该 URL。
- 结果去重排序（同 2.4）。
- 单源失败静默忽略，无 `errors`。
- `page` 在此分支**不生效**（`explore_all` 不接收页码）。

**B. `url` 是书源 ID（非 http 开头）**

- 在书源表中精确查找 `book_source_url == url`，找不到 → `404 {"error":"source not found"}`。
- 用 `page` 展开该源 `exploreUrl` 的分类；取**第一个**分类 URL。
- 没有可展开分类 → `400 {"error":"source has no expandable exploreUrl"}`。
- 再次抓取并只解析该源的第一个分类。
- 前端若想选分类，应先用 5.4 拿列表，再把分类 URL 作为 `url` 调用本接口（A 模式会跨源查询，而非只查该源；这是当前实际行为）。

- 成功 `200`：`SearchBook[]`（同 5.1）
- 错误：缺 `url` → 400 文本；`401`；`404`；`400`；上游抓取/解析失败 → `400 internal: ...`（分支 B）。
- 无分页字段。

### 5.4 `GET /api/explore/categories`

- 鉴权：登录
- 查询参数：

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `source` | string | 是 | 书源 ID（`book_source_url`） |
| `page` | integer | 否 | 默认 1，用于替换 `exploreUrl` 中的 `{{page}}` |

- 成功 `200`：`ExploreCategory[]`

```json
[
  { "title": "玄幻", "url": "https://example.com/category/xuanhuan?page=1" },
  { "title": "都市", "url": "https://example.com/category/dushi?page=1" }
]
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `title` | string | 否 | 分类名；无 `::` 时用书源名 |
| `url` | string | 否 | 已替换 `{{page}}`/`{{key}}` 的绝对/相对 URL |

- 错误：缺 `source` → 400 文本；`404 source not found`；`400`（`exploreUrl` 的 JS 执行失败）。
- 行为：
  - `exploreUrl` 为空 → 返回 `[]`（不是错误）。
  - 支持格式：每行 `分类名::url`；无 `::` 时整行是 URL；`#` 开头与空行忽略；`{{page}}` 替换。
  - 支持 `@js:` / `<js>` 求值，结果可为多行文本或 `[{"title","url"}]` JSON 数组（[web_book.rs](../server/src/web_book.rs#L384-L452)）。
  - 重复 URL 去重（按替换后的 URL 精确去重）。

### 5.5 `GET /api/search/start`

- 鉴权：登录
- 验证状态：**已验证**（HTTP 实测 `:3000`，2026-11-03 14:0x / 15:0x，新二进制 14:46:58）：`q` 别名可用；`source_ids` 的数字序号与 URL 精确匹配均可用，`requested/ignored/warning` 回显正确；全部 id 无效时 `source_total=0` 且 `done=true`；缺参/空参 400 JSON。job 保留上限为**源码级**（已完成 32 / 进行中 64），旧的“8 个上限”HTTP 结论已作废。`key` 优先于 `q` 为**源码级**（未用能区分的搜索词做 HTTP 对照）。
- 用途：创建一次异步聚合搜索 job，立即返回；随后用 5.6 轮询，**先到先渲染**。
- 查询参数（`SearchQ` 为 5.1/5.2/5.5 共用结构；`page/page_size` 在本接口被解析但**不生效**）：

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `key` | string | 否* | 正式参数名；`trim()` 后使用。**只要出现 `key` 就以它为准**（即使为空，也不会回退到 `q`），空串 → 400 |
| `q` | string | 否* | `key` 的别名；仅在未传 `key` 时生效 |
| `source_ids` | string | 否 | 逗号分隔；去空、去重、保留输入顺序。每项先按 `book_source_url` 精确匹配；匹配不到再按**排序后的可搜索启用源**（`enabled && rule_search && search_url`，与 `/api/sources` 同一 `sort_sources` 顺序）的 1-based 序号解析；非法/禁用/越界项忽略。若前端列出的源含禁用/不可搜索项，序号只在该可搜索子集内计数 |
| `include_cooled` | string | 否 | `1`/`true` 时把冷却中的源也加入 job（排查用）；默认跳过。隔离区源无论该参数如何都不参与 |
| `scope` | string | 否 | `curated`（默认，只跑精选/优选源，总预算 6s）/ `all`（**加强搜索**，全量源，总预算 40s；源级并发 16）。单源预算按 scope 分档：`curated` 用 `SEARCH_TIMEOUT_SECS`（默认 8s），`all` 用 `SEARCH_ALL_TIMEOUT_SECS`（默认 8s）。<br>**curated 退化行为**：精选集合（`8535ed22…`）未就绪时按 `tier=preferred` 近似；一个 preferred 都没有（tier 数据未建立）则退回全量，避免 0 命中 |

\* `key` 与 `q` 至少提供一个，且最终值非空；否则 400。`q` 别名同样适用于 5.1/5.2（共用 `SearchQ`；`/api/search?q=…` 已实测 200）。

- 成功 `200`（示例为伪造值）：

```json
{
  "job_id": "j67864-1",
  "source_total": 1,
  "skipped_cooldown": 2,
  "cooldown_total": 3,
  "include_cooled": false,
  "scope": "curated",
  "deadline_ms": 6000,
  "total_budget_ms": 6000,
  "phase": "first",
  "partial": true,
  "scheduled_sources": 0,
  "attempted_sources": 0,
  "pending_sources": 1,
  "source_ids_requested": ["1", "999", "abc"],
  "ignored_source_ids": ["999", "abc"],
  "warning": "已忽略无效书源 id：999, abc"
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `job_id` | string | 否 | 形如 `j<进程号>-<自增序号>`；**不透明**，前端只原样回传，不要解析 |
| `source_total` | integer | 否 | 参与源数：`enabled && rule_search && search_url`、非隔离、且不在冷却期的活跃源快照，经 `source_ids` 过滤后的数量；全部无效时为 `0` |
| `skipped_cooldown` | integer | 否 | 因冷却被跳过的源数（在 `source_ids` 过滤后的候选里计数） |
| `cooldown_total` | integer | 否 | 本地全部处于冷却中的源数（不受 `source_ids` 影响） |
| `include_cooled` | boolean | 否 | 回显是否放行冷却源 |
| `deadline_ms` | integer | 否 | **单源**搜索预算（毫秒），按 scope 取值：`curated` = `SEARCH_TIMEOUT_SECS × 1000`（默认 `8000`），`all` = `SEARCH_ALL_TIMEOUT_SECS × 1000`（默认 `8000`）；不是整次搜索的硬上限 |
| `total_budget_ms` | integer | 否 | **整批**搜索总预算（毫秒）。优先级：`SEARCH_CURATED_BUDGET_SECS`/`SEARCH_ALL_BUDGET_SECS`（按 scope）> `SEARCH_TOTAL_BUDGET_SECS`（统一覆盖）> scope 默认（curated 6000 / all 40000） |
| `scope` | string | 否 | 回显生效范围：`curated` / `all` |
| `phase` | string | 否 | 两段式阶段：仍有未完成源时为 `first`，全部完成（`done=true`）为 `final` |
| `partial` | boolean | 否 | 是否为“部分结果”（`!done`）；与 `phase=first` 同义 |
| `scheduled_sources` | integer | 否 | 已进入调度的源数（两段式下逐批累加），`scheduled_sources <= source_total` |
| `attempted_sources` | integer | 否 | 真正拿到并发槽并发出过至少一次请求的源数；排队到预算耗尽、未发出请求的**不计入**，`attempted_sources <= scheduled_sources` |
| `pending_sources` | integer | 否 | **尚未排上**的源数，`= source_total - scheduled_sources`（不是“未完成数”） |
| `source_ids_requested` | string[] | 否 | 解析后的请求 id（原样字符串、去空去重、保留输入顺序）；未传时为 `[]` |
| `ignored_source_ids` | string[] | 否 | 未能匹配到任何启用可搜索源的 id；未传 `source_ids` 时为 `[]` |
| `warning` | string \| null | 是 | 仅当 `ignored_source_ids` 非空时给出，文案 `已忽略无效书源 id：...` |

- 错误：
  - `key`/`q` 都缺，或 `key` 存在但为空（即使同时给了 `q`）→ `400 {"error":"key（或别名 q）不能为空"}`
  - `401`（未登录/令牌失效）；`500`（书源表读取失败）
- 行为/上限（TTL 为 HTTP 已验证；32/64 保留上限为**源码级**，旧的 8 个上限结论已作废；200/400 截断为源码级）：
  - 创建即返回，后台对快照中的每个源并发抓取**第 1 页**（每个源受 `deadline_ms` 预算）。
  - **job 自创建起存活 60s**（HTTP 实测：70s 后查询 404）；每次 start/result 都会清理过期 job。过期后查询 → 404（见 5.6）。
  - 进程级注册表：**已完成 job 最多保留 32 个**（`SEARCH_JOB_MAX`），超出时只淘汰“已完成且 `elapsed` 最旧”的任务；**进行中的 job 不参与该淘汰**，另有 **64 个硬上限**（`SEARCH_JOB_MAX_ACTIVE`），超过才淘汰最旧的活跃任务，兜底防内存膨胀。这里的 32/64 是**同时保留的搜索任务数**，不是“每源并发”；出站 HTTP 并发由 `HTTP_CONCURRENCY` 的全局信号量（默认 16、硬上限 16）控制，且每个请求还要先通过该书源的 `concurrentRate` 在飞上限（默认 8，见书源字段说明）。
  - 单源结果最多 **200** 本（超出截断）；所有已完成源合并、去重、排序后总结果最多 **400** 本（**源码级，未直接触发截断**）。
  - `source_ids` 全部无效时仍会创建 job，`source_total=0`、`pending=0`，首次查询即 `done=true`；前端可据 `warning` 提示并决定是否重新搜索。
  - **前端接线（2026-11-03 复核）**：`api.searchStart(key, sourceIds)` 已把 `source_ids` 传给本接口；`Discover.svelte` 提供“全部/指定书源”范围切换，并用 `ignored_source_ids` 提示被忽略的源。此前“前端仍未传 `source_ids`”的记录作废。
  - 分页、去重与权重排序都使用 job 创建时的源快照（`source_ids` 过滤后的子集）；之后改书源不影响该 job。
  - **冷却源默认不进入 job**：不会发出请求，也不会出现在 `errors`；`skipped_cooldown` 只做展示。job 收尾时会把本次每源的探测结果批量写回健康表（成功清零、真实站点级失败累加），同一任务只写一次；**全局预算耗尽（`truncated`/`skipped`）不写健康**，避免把健康源误冷却/误隔离。
  - `deadline_ms` 只约束单个源；多个源共享 `HTTP_CONCURRENCY` 信号量，网络慢时整次搜索可能明显超过 `deadline_ms`。搜索对每个源只发 1 个请求，因此同一次 job 内按源上限（默认 8）不会触发排队；它主要约束「打开一本书」时的目录/正文/图片并发（同一源多请求），排队次数见 `/healthz` 的 `search.source_limit_waits`。

### 5.6 `GET /api/search/result`

- 鉴权：登录
- 验证状态：**已验证**（HTTP 实测 `:3000`，2026-11-03 14:0x）：字段齐全；捕捉到 `done=false` + `has_more=true`（`finished=4, pending=21, total=13`）；终态 `done=true`（`finished=25, pending=0, total=183`）；`page=0→1`、`page_size=500→200`；`page=99` 返回空页 `has_more=false`；伪造/过期 `job_id` → 404 且错误体为统一 JSON。
- 查询参数：

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `job_id` | string | 是 | — | 5.5 返回的 id |
| `page` | integer | 否 | 1 | `unwrap_or(1).max(1)`，0/负数当 1 |
| `page_size` | integer | 否 | 30 | `clamp(1, 200)` |

- 成功 `200`（示例为伪造值；`books` 元素结构同 5.1 的 `SearchBook`）：

```json
{
  "books": [ /* SearchBook[] */ ],
  "errors": [
    { "source": "https://bad.example", "source_name": "坏源", "error": "搜索超时（单源 8s 预算）", "kind": "timeout" }
  ],
  "page": 1,
  "page_size": 30,
  "total": 183,
  "title_filter": { "raw": 296, "kept": 183, "dropped": 113 },
  "has_more": true,
  "done": false,
  "phase": "first",
  "partial": true,
  "scheduled_sources": 4,
  "attempted_sources": 4,
  "pending_sources": 21,
  "scope": "curated",
  "finished": 4,
  "pending": 21,
  "source_total": 25,
  "source_failed": 5,
  "source_skipped": 3,
  "skipped_cooldown": 2,
  "cooldown_total": 3,
  "deadline_ms": 6000,
  "total_budget_ms": 6000
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `books` | SearchBook[] | 否 | 当前页数据（见 5.1 字段表） |
| `errors` | object[] | 否 | 已完成源的失败明细：`source`、`source_name`、`error`、`kind`（`site`/`engine`/`timeout`/`truncated`/`skipped`） |
| `page` | integer | 否 | 回显（最小 1） |
| `page_size` | integer | 否 | 回显（1..200） |
| `total` | integer | 否 | 书名过滤后、当前已完成源的去重排序条数，最多 400；**会随源完成而增长** |
| `title_filter` | object | 否 | `raw`=过滤前候选、`kept`=过滤后（= `total`）、`dropped`=被丢弃条数；全部被过滤时 `books=[]` 且不报错 |
| `has_more` | boolean | 否 | `skip + page_size < total`；`page` 超出末尾时为 `false` |
| `done` | boolean | 否 | `pending == 0`，即所有源都已结束（成功/失败/超时都算结束） |
| `phase` | string | 否 | `first`（仍有未完成源）/ `final`（全部完成） |
| `partial` | boolean | 否 | `!done`，即当前列表是部分结果；前端可显示“已搜 X/Y，还在继续” |
| `scheduled_sources` | integer | 否 | 已进入调度的源数；`pending_sources = source_total - scheduled_sources` 才是“未排上”的源数 |
| `attempted_sources` | integer | 否 | 真正发过请求的源数（排队未及执行的不算），`attempted_sources <= scheduled_sources <= source_total` |
| `pending_sources` | integer | 否 | 尚未排上的源数（兼容旧字段 `pending` 仍表示“未完成数”） |
| `scope` | string | 否 | job 创建时的搜索范围：`curated` / `all`；`all` 的源级并发默认 16，显式调低 `HTTP_CONCURRENCY` 时取 `min(配置,16)` |
| `total_budget_ms` | integer | 否 | job 的整批总预算（毫秒），与 5.5 一致 |
| `finished` | integer | 否 | 已结束的源数（含失败） |
| `pending` | integer | 否 | 尚未结束的源数；`finished + pending == source_total` |
| `source_total` | integer | 否 | 创建 job 时的参与源数（`source_ids` 过滤后的活跃源快照，已排除隔离区与冷却源；不随书源配置变化） |
| `source_failed` | integer | 否 | **数字**，真实失败源数（`kind` 为 `site`/`engine`/`timeout`）；不含预算未完成 |
| `source_skipped` | integer | 否 | **数字**，因全局搜索总预算耗尽未执行/未完成的源数（`kind` 为 `truncated`/`skipped`），不写健康、不触发冷却 |
| `skipped_cooldown` | integer | 否 | 因冷却被跳过、未参与本 job 的源数 |
| `cooldown_total` | integer | 否 | 本地全部处于冷却中的源数 |
| `deadline_ms` | integer | 否 | 单源预算（与 5.5 一致），每次响应都回传 |

- 错误：
  - `404 {"error":"搜索任务不存在或已过期（60s TTL）"}`：job **不存在、已超过 60s TTL、或已被 32/64 保留上限淘汰**。服务端不区分这三种情况。
  - 缺 `job_id` → `400` 纯文本：``Failed to deserialize query string: missing field `job_id` ``；`401`。
- **`done=false` 但 `has_more=true` 的正确用法（先到先渲染 + 翻页）**：
  1. 用户搜索 → `start` 拿 `job_id` → 立即请求 `result?job_id=...&page=1&page_size=...`。
  2. 只要 `books` 非空就先渲染；`done=false` 时继续轮询（建议 300–1000ms 一次），用同一 `job_id`、`page=1` 覆盖刷新，直到 `done=true`。
  3. `done=false && has_more=true` 只表示“**目前已完成的结果已超过一页**”，不代表搜索结束；此时翻到第 2 页看到的只是已完成源的结果子集。
  4. **分页可能位移**：新源完成后会按权重插入到前面，`total` 增长，之前第 2 页的内容可能被挤到第 3 页。需要稳定翻页时，等 `done=true` 后再翻；或接受轮询期间的分页位移。
  5. `404` 时不要重试同一 `job_id`，直接重新 `start`。
- 行为：
  - 每次请求都会先清理过期/超限 job，再按 `job_id` 精确查找；查找不到一律 404。
  - 分页基于当前已完成结果的快照切片；`page`/`page_size` 的 clamp 与 5.1/5.2 一致。
  - `errors` 中单源失败不影响整体；超时文案为 `搜索超时（单源 Ns 预算）`。

---

## 6. 书籍与正文

> 6.1~6.9 依赖用户库（`users/{uid}.db`）：`book_meta`、`book_chapters`、`book_content`、`bookmarks`、`user_settings`。其中抓取类接口（详情/目录/目录批量/正文/整本缓存）的失败统一映射为 `400 {"error":"internal: ..."}`（`anyhow` → `AppError::BadRequest`，见 [error.rs](../server/src/error.rs#L61-L65)），不是 502/504；唯一例外是 6.7 书内搜索超过 5s 返回 `504`。

### 6.1 `GET /api/book/info`

- 鉴权：登录
- 查询参数：

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `source` | string | 是 | 书源 ID（`book_source_url`） |
| `book_url` | string | 是 | 书籍详情页 URL |

- 成功 `200`：`Book`（**snake_case**，所有字段都会出现）：

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `book_url` | string | 否 | 回显请求的书籍 URL |
| `toc_url` | string | 是 | 目录 URL（由 `ruleBookInfo.tocUrl` 解析） |
| `origin` | string | 是 | 书源 ID（接口强制填请求的 `source`） |
| `origin_name` | string | 是 | 书源名 |
| `name` | string | 是 | 书名 |
| `author` | string | 是 | 作者 |
| `kind` | string | 是 | 分类 |
| `custom_tag` | string | 是 | 自定义标签（接口不填） |
| `cover_url` | string | 是 | 封面 |
| `custom_cover_url` | string | 是 | 自定义封面（接口不填） |
| `intro` | string | 是 | 简介 |
| `custom_intro` | string | 是 | 自定义简介（接口不填） |
| `charset` | string | 是 | 编码（接口不填） |
| `book_source_type` | integer | 否 | 0=图书，1=音频；接口不填，恒 0 |
| `group` | string | 是 | 分组；接口不填，保持库中旧值 |
| `latest_chapter_title` | string | 是 | 最新章节标题 |
| `latest_chapter_time` | integer | 否 | 接口不填，恒 0（语义待确认） |
| `last_check_time` | integer | 否 | DB 无此列，`sqlx(default)` 恒 0 |
| `last_check_count` | integer | 否 | DB 无此列，`sqlx(default)` 恒 0 |
| `total_chapter_num` | integer | 否 | 接口不填；若无旧缓存则 0 |
| `dur_chapter_title` | string | 是 | 接口不填 |
| `dur_chapter_index` | integer | 否 | 接口不填，恒 0 |
| `dur_chapter_pos` | integer | 否 | 接口不填，恒 0 |
| `dur_chapter_time` | integer | 否 | 接口不填，恒 0 |
| `word_count` | string | 是 | 接口不填 |
| `can_update` | boolean | 否 | 接口不填，保存时默认 false（注意与建表默认 1 不同） |
| `order` | integer | 否 | 接口不填，恒 0 |
| `origin_order` | integer | 否 | 接口不填，恒 0 |
| `variable` | string | 是 | 接口不填 |

示例（伪造值，节选，实际字段一个不少）：

```json
{
  "book_url": "https://example.com/book/123",
  "toc_url": "https://example.com/book/123/toc",
  "origin": "https://example.com",
  "origin_name": "示例书源",
  "name": "示例小说",
  "author": "张三",
  "kind": "玄幻",
  "custom_tag": null,
  "cover_url": "https://example.com/cover/123.jpg",
  "custom_cover_url": null,
  "intro": "一段简介",
  "custom_intro": null,
  "charset": null,
  "book_source_type": 0,
  "group": null,
  "latest_chapter_title": "第 300 章",
  "latest_chapter_time": 0,
  "last_check_time": 0,
  "last_check_count": 0,
  "total_chapter_num": 0,
  "dur_chapter_title": null,
  "dur_chapter_index": 0,
  "dur_chapter_pos": 0,
  "dur_chapter_time": 0,
  "word_count": null,
  "can_update": false,
  "order": 0,
  "origin_order": 0,
  "variable": null
}
```

- 错误：缺参 → 400 纯文本；`404 source not found`（在全部书源中找，含 disabled）；上游抓取/规则失败 → `400 internal: ...`；`401`。
- 行为：
  - 书源没有 `ruleBookInfo` 时**不报错**，直接返回只填了 `book_url`/`origin`/`origin_name` 的对象（`web_book::book_info` 立即返回）。
  - 成功后 upsert `book_meta`（`ON CONFLICT(book_url)` 更新解析字段），但 `can_update` 会被写成 `false`，`group` 会被写成 `null`——即**可能清空已有分组**（[books.rs](../server/src/routes/books.rs#L333-L383)、[entities.rs](../server/src/entities.rs#L221-L284)）。这是当前实际行为，见 TODO。
  - `POST /api/bookshelf` 之后调本接口可覆盖书架展示数据；前端若先加书架再取详情，注意分组可能丢失。

### 6.2 `GET /api/book/toc`

- 鉴权：登录
- 查询参数：

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `source` | string | 是 | 书源 ID |
| `book_url` | string | 是 | 书籍 ID / URL；也是缓存键 |

- 成功 `200`：`BookChapter[]`，按 `index` 升序：

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `url` | string | 否 | 章节 URL（唯一键）。**分卷行为合成 URL `volume:<book_url>#<index>`，不可用于抓正文**，见下方警告 |
| `title` | string | 是 | 章节标题 |
| `is_volume` | boolean | 否 | 是否分卷行；由书源 `ruleToc.isVolume` 解析，**不再是恒 false** |
| `base_url` | string | 是 | 抓取该章节时的目录页 URL |
| `book_url` | string | 是 | 所属书籍 URL |
| `index` | integer | 否 | **0 基数组下标**（分卷行也占一个下标） |
| `is_vip` | boolean | 否 | 是否 VIP 章节；由书源 `ruleToc.isVip` 解析，**不再是恒 false** |
| `is_pay` | boolean | 否 | 是否付费章节；由书源 `ruleToc.isPay` 解析并落库，**不再是恒 false**（真实付费样本仅单测） |
| `resource_url` | string | 是 | 当前 null |
| `tag` | string | 是 | 当前 null |
| `start` | integer | 否 | 当前恒 0 |
| `end` | integer | 否 | 当前恒 0 |
| `update_time` | string | 是 | 来源站点给出的更新时间，**原样字符串**（不保证格式）；书源未提供则 `null` |
| `word_count` | integer | 否 | 正文字数；已缓存正文的章节回填为 `length(trim(content))`（字符数），未缓存为 0 |
| `volume_title` | string | 是 | 所属分卷标题：分卷行为自身标题，普通章节为最近的前置分卷；全书无分卷则为 `null` |

示例（伪造值）：

```json
[
  {
    "url": "volume:https://example.com/book/123#0",
    "title": "第一卷 风起",
    "is_volume": true,
    "base_url": "https://example.com/book/123/toc",
    "book_url": "https://example.com/book/123",
    "index": 0,
    "is_vip": false,
    "is_pay": false,
    "resource_url": null,
    "tag": null,
    "start": 0,
    "end": 0,
    "update_time": null,
    "word_count": 0,
    "volume_title": "第一卷 风起"
  },
  {
    "url": "https://example.com/book/123/1.html",
    "title": "第一章 开端",
    "is_volume": false,
    "base_url": "https://example.com/book/123/toc",
    "book_url": "https://example.com/book/123",
    "index": 1,
    "is_vip": true,
    "is_pay": false,
    "resource_url": null,
    "tag": null,
    "start": 0,
    "end": 0,
    "update_time": "2026-10-31 12:00:00",
    "word_count": 3200,
    "volume_title": "第一卷 风起"
  }
]
```

> **警告（必读）**：分卷行的 `url` 是服务端合成的稳定主键，形如 `volume:<book_url>#<index>`（`index` 为 0 基全局下标），**不是可抓取的章节地址**。
> 前端必须按 `is_volume` 跳过这些行再请求正文；把它们当作 `chapter_url` 传给 [`GET /api/book/content`](#64-get-apibookcontent) 会因找不到真实章节而抓取失败（通常 `400 internal: ...` 或 `404`）。

- 错误：缺参 → 400 纯文本；`404 source not found`（仅缓存未命中时才会加载书源）；`400 internal: ...`（目录抓取/解析失败）；`401`。
- 行为：
  - **缓存优先**：只要该 `book_url` 在 `book_chapters` 中有任意一行 `updated_at >= now - TOC_TTL_SECS`，就直接返回全部缓存行，**不再访问网络，也忽略 `source` 参数**（[books.rs](../server/src/routes/books.rs#L238-L259)、[books.rs](../server/src/routes/books.rs#L393-L418)）。
  - TTL 来自 `BORANLIB_TOC_TTL_SECS`（兼容旧前缀 `LEGADO_TOC_TTL_SECS`），默认 21600 秒（6 小时），启动时钳制到 60..604800（[config.rs](../server/src/config.rs#L40-L62)）。
  - 未命中：读取 `book_meta.toc_url`（若有）作为目录页，解析 `ruleToc`；支持 `nextTocUrl`，最多 10 页。
  - 落库策略：先 `DELETE FROM book_chapters WHERE book_url=?` 再逐条插入（整本替换）。若解析结果为 0 章，会保存 0 行，下次仍会重新抓取。
  - **缓存按 `book_url` 而非 `source` 或 `origin`**：换源后同一 `book_url` 会返回旧源目录；不同书源的同名 URL 也会串数据。见 TODO。
  - `chapter_index` 使用返回数组下标（`index`），见 2.2。
  - **新字段来源**：`is_volume` / `is_vip` / `update_time` 由 `ruleToc.isVolume` / `isVip` / `updateTime` 解析；`update_time` 原样保存，**不保证是标准时间格式**，前端不要直接当时间戳解析。
  - **分卷 URL 生成**：`is_volume = true` 的行，`url` 固定为 `volume:{book_url}#{index}`；普通章节若站点给出的 URL 为空，则拼 `{book_url}{index}`。前者**只作主键/分组用途**，不能抓正文（见上方警告）。
  - **`volume_title` 回填**：返回前统一执行 `enrich_volumes`，分卷行取自身 `title`，其后普通章节继承最近的前置分卷标题。
  - **`word_count` 回填**：目录新抓取（本接口未命中 TTL 缓存时）或 `toc/batch` 写库后执行 `backfill_word_counts`，用已缓存正文的 `length(trim(content))` 回填；未缓存章节为 0。**TTL 缓存命中时不会重算**，直接返回库内值。清理正文缓存（6.9）不会立刻改动已存的 `word_count`，下次目录刷新时才会按当前缓存重算。
  - 验证状态：**已验证**（HTTP + 回归 `report-20261103-1108.md`）：200 响应含 `update_time`/`word_count`/`volume_title`。**分卷行真实数据下的渲染、`is_volume`/`is_vip` 真实值、`volume:` URL 生成仍仅单测**；回归报告未覆盖分卷样本。

### 6.3 `POST /api/book/toc/batch`

- 鉴权：登录
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B7，3/3）：**成功抓取**；混合批 `updated=2 skipped=2`（重复项 + 缺源各一）；单本失败隔离 `failed=1 updated=1`；空列表 400；`book_ids` 别名可用。**并发 3 未被直接观测**。
- 请求体（**JSON**）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `book_urls` | string[] | 是 | 书籍 URL 列表；也接受 Legado 风格的别名 `book_ids` |
| `source` | string | 否 | 统一书源 ID；缺省时逐本读 `book_meta.origin` |

示例（伪造值）：

```json
{ "book_urls": ["https://example.com/book/123", "https://example.com/book/456"] }
```

- 成功 `200`：

```json
{
  "ok": true,
  "total": 2,
  "updated": 1,
  "failed": 1,
  "skipped": 0,
  "results": [
    { "book_url": "https://example.com/book/123", "status": "updated", "chapters": 300, "error": null },
    { "book_url": "https://example.com/book/456", "status": "failed", "chapters": 0, "error": "抓取目录失败：timeout" }
  ],
  "elapsed_ms": 1234
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 true |
| `total` | integer | 否 | `results` 长度（含重复项产生的 `skipped`；不含被静默忽略的空串） |
| `updated` | integer | 否 | `status=updated` 的数量 |
| `failed` | integer | 否 | `status=failed` 的数量 |
| `skipped` | integer | 否 | `status=skipped` 的数量 |
| `results[].book_url` | string | 否 | 书籍 URL |
| `results[].status` | string | 否 | `"updated"` / `"failed"` / `"skipped"` |
| `results[].chapters` | integer | 否 | 成功写入的章节数；失败/跳过为 0 |
| `results[].error` | string | 是 | 失败/跳过原因；成功为 `null` |
| `elapsed_ms` | integer | 否 | 总耗时（毫秒） |

- 错误：
  - 空列表/全为空白 → `400 {"error":"book_urls 不能为空"}`。
  - 去重后超过 **100** 本 → `400 {"error":"单次最多 100 本，请分批调用"}`。
  - JSON 非法/缺 `book_urls` → 400/422 纯文本；`401`；`500`。
- 行为：
  - **并发 3**（`TOC_BATCH_CONCURRENCY = 3`）、单次最多 **100** 本（`TOC_BATCH_MAX = 100`）。
  - 输入逐项 `trim`；空串**静默忽略**；重复项不重复抓取，第二个及以后记 `status="skipped"`、`error="重复的 book_url"`。
  - 每本的源：请求 `source` 非空时统一用它；否则读 `book_meta.origin`。两者都没有 → `skipped`（`缺少书源，且 book_meta 未记录 origin`）；源不存在 → `skipped`（`书源不存在：...`）。
  - 抓取目录失败 → `failed`（`抓取目录失败：...`）；目录为空 → `failed`（`目录为空`）；写库失败 → `failed`（`写入目录失败：...`）。
  - 成功 → **整本替换** `book_chapters`（先删后插）、更新 `book_meta.total_chapter_num`、回填 `word_count`，状态 `updated`。
  - **`results` 顺序不保证与输入一致**：重复项的 `skipped` 先写入结果，其余由 `buffer_unordered` 并发完成顺序决定。前端应按 `book_url` 对齐，不要按数组下标。
  - 与 6.2 不同，本接口**不读 TOC TTL**，总是重新抓取并覆盖缓存。

### 6.4 `GET /api/book/content`

- 鉴权：登录
- 查询参数：

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `source` | string | 是 | 书源 ID（仅缓存未命中时使用） |
| `chapter_url` | string | 是 | 章节 URL；也是缓存键。**不要传 6.2 的分卷行 URL（`volume:<book_url>#<index>`）**，它不是可抓取地址（见 6.2 警告） |
| `book_url` | string | 否 | 所属书籍 URL；不传时服务端会反查（见下） |
| `title` | string | 否 | 章节标题兜底；不传时服务端会反查（见下） |

- 成功 `200`：`BookContent`（**snake_case**）：

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `book_url` | string | 否 | 所属书籍 URL；**必定非空**（回填失败时整个请求返回 400） |
| `url` | string | 否 | 章节 URL |
| `title` | string | 是 | 章节标题；优先缓存值，其次请求参数 `title`，再按 `chapter_url` 反查 `book_chapters.title`；都拿不到才为 `null` |
| `content` | string | 是 | 正文（经用户替换规则 + 图片样式处理） |
| `chapter_index` | integer | 否 | 章节下标；新抓取时恒 0（前端用目录数据补全） |
| `resource_url` | string | 是 | 当前 null |
| `is_local` | boolean | 否 | 当前 false |

示例（伪造值）：

```json
{
  "book_url": "https://example.com/book/123",
  "url": "https://example.com/book/123/1.html",
  "title": "第一章 开端",
  "content": "<p>正文第一段</p><p>正文第二段</p>",
  "chapter_index": 0,
  "resource_url": null,
  "is_local": false
}
```

- 错误：缺参 → 400 纯文本；`404 source not found`（仅缓存未命中）；`400 internal: ...`；`401`；**`400 {"error":"missing_book_url","message":"缺少书籍地址，请从书架重新打开"}`**（见下）。
- 行为：
  - 缓存命中（`book_content.updated_at >= now - CONTENT_TTL_SECS`）直接返回，并更新 `accessed_at`（LRU 依据）。TTL 来自 `CONTENT_TTL_SECS`，默认 `604800` 秒（7 天），钳制 60..2592000。
  - 缓存未命中：加载书源（不在缓存中的源 → 404）→ 仅用 `url` 构造 `BookChapter`，再按下面的回填链补齐 `book_url` / `title` → 抓取（[books.rs](../server/src/routes/books.rs#L263-L305)、[web_book.rs](../server/src/web_book.rs#L284-L292)）。
  - **`book_url` 回填链**（依次尝试，任一命中即用）：① 缓存/抓取结果自带；② 请求参数 `book_url`；③ `SELECT book_url FROM book_chapters WHERE url = chapter_url`（章节表的主键就是书籍 URL。`book_meta` 有 `book_url` 列，但只对有元数据的书；缓存书走这里）。三步都为空 → 返回 `400 {"error":"missing_book_url","message":"缺少书籍地址，请从书架重新打开"}`，不再返回空 `book_url`。
  - **`title` 回填链**：① 缓存/抓取结果；② 请求参数 `title`；③ 按 `chapter_url` 反查 `book_chapters.title`；都无则为 `null`。前端无需自行补 `book_url`，`title` 仅用于展示，缺失不影响正文。
  - 正文抓取支持 `nextContentUrl` 拼接（最多 10 页）；`sourceRegex` → `replaceRegex` 先源清洗后可读性替换。
  - **每次都执行用户替换规则**（包括缓存命中）：若 `use_replace_rule` 未关闭，按 `replace_rules.order,id` 应用；标题受 `scope_title` 控制，正文受 `scope_content` 控制；`scope`/`exclude_scope` 每行一个书名或书源域名，命中任一即生效（[replace.rs](../server/src/routes/replace.rs#L335-L359)）。
  - 书名取自 `book_meta.name`，按回填后的 `book_url` 查；回填成功（即响应 `book_url` 非空）时书名作用域规则可正常匹配，回填失败已直接 400，不会带着空书名执行规则。
  - 随后按用户 `image_style` 改写 `<img>`：`0` 原样、`1` 追加 `max-width:100%;height:auto;`、`2` 追加 `display:block;margin:0 auto;`、`3` 删除图片标签（[replace.rs](../server/src/routes/replace.rs#L229-L255)）。
  - 缓存上限：用户 `cache_limit_mb` 若存在且 >0，则 `min(用户值, 全局 CACHE_LIMIT_MB)`；否则用全局值。全局默认 50 MiB，钳制 4..512。
  - 淘汰：插入后若 `SUM(size_bytes) > limit`，循环最多 8 轮，每轮按 `accessed_at ASC` 删 16 行（[books.rs](../server/src/routes/books.rs#L481-L532)）。
  - `size_bytes` 用 `content.len()`（UTF-8 字节数）近似。
  - 缓存按 `chapter_url` 唯一；同一章节 URL 不会被不同书源区分。
  - 若书源 `ruleContent` 使用了 `@js:` 且调用了不支持的内置（`java.ajax`/`post`/`getCookie`/`webView*` 等），规则求值抛错，本章抓取失败，返回 `400 internal: ...`；不再像旧版那样返回空正文。
  - `java.getElements`/`getString`/`getStringList` 在正文规则中可用，但只支持**字面量选择器**，且只作用于当前页 HTML；上限与失败语义见 2.7。JS 超过 `JS_TIMEOUT_MS`（默认 3000ms）预算同样会导致本章抓取失败。

### 6.5 `POST /api/book/switch-source`

- 鉴权：登录
- 请求体（**JSON，不是 query**）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `name` | string | 是 | 书名（用于搜索） |
| `author` | string | 否 | 作者；提供时要求候选作者完全相等（小写后） |
| `exclude_source` | string | 否 | 排除的书源 ID |

示例（伪造值）：

```json
{ "name": "示例小说", "author": "张三", "exclude_source": "https://old.example" }
```

- 成功 `200`：`SearchBook[]`（同 5.1），**只返回候选，不自动切换**。
- 错误：缺 `name` 或 JSON 非法 → 422/400（Axum 纯文本）；`401`；无 `source not found` 之类错误。
- 行为：
  - 用 `name` 在所有书源搜索**第 1 页**（同 `/api/search` 的源条件、去重、排序）。
  - 过滤：候选书名小写后 `== name` 或 `contains(name)`；作者有值时要求 `== author`；`exclude_source` 精确排除 `origin`（[books.rs](../server/src/routes/books.rs#L307-L329)）。
  - 单源失败静默忽略。
  - 不存在分页；候选数量受各源首页限制。

### 6.6 书签：`GET/POST /api/bookmarks`、`DELETE /api/bookmarks/delete`

- 鉴权：登录
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B1，6/6）：新增返回完整字段；`position=0`/`1` 放行，`1.5`/`-0.1` 400；2001 字备注、空 `book_id`、负章节均 400；按书返回 3 条（含章节索引与 0..1 位置）；删除 `deleted=1` 且不影响其他书签；`id=0` 400。
- 数据在用户库 `bookmarks`，按用户隔离。

**`GET /api/bookmarks`**

- 查询参数：`book_id`（string，可选；`trim` 后为空视为未传，返回该用户全部书签）。
- 成功 `200`：**裸数组**（不是 `{ok,items}`），按 `created_at DESC, id DESC`：

```json
[
  {
    "id": 7,
    "book_id": "https://example.com/book/123",
    "chapter_index": 12,
    "position": 0.6,
    "chapter_title": "第十二章",
    "note": "这里提到伏笔",
    "created_at": "2026-11-02 11:30:00"
  }
]
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `id` | integer | 否 | 书签 id |
| `book_id` | string | 否 | 书籍 URL |
| `chapter_index` | integer | 否 | 0 基章节下标（见 2.2） |
| `position` | number | 否 | 章内位置 0..1（见 2.1） |
| `chapter_title` | string | 是 | 章节标题快照；可空 |
| `note` | string | 是 | 备注；可空 |
| `created_at` | string | 否 | UTC `datetime('now')` 字符串 |

- 注意：**当前没有 `chapter_url` 字段**，书签定位依赖 `book_id + chapter_index + position`，前端跳转时需自行从目录取 URL。

**`POST /api/bookmarks`**

- 请求体：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `book_id` | string | 是 | 书籍 URL；`trim` 后不能为空 |
| `chapter_index` | integer | 否 | 默认 0；必须 ≥ 0 |
| `position` | number | 否 | 默认 0；**必须 0..=1 的有限数**（NaN/Infinity/越界 → 400） |
| `chapter_title` | string | 否 | 标题快照 |
| `note` | string | 否 | 备注；最多 **2000 字符**（按 Unicode 字符数） |

- 成功 `200`：返回**新增的单个 `Bookmark` 对象**（字段同上表），不是 `{ok,id}`。
- 错误：
  - `400 {"error":"book_id 不能为空"}` / `"chapter_index 必须大于等于 0"` / `"position 必须是 0..1 的章内比例"` / `"note 最多 2000 字"`。
  - JSON 非法 → 400/422 纯文本；`401`；`500`。
- 行为：**纯 INSERT，不做 upsert**；同一本书同一章同一位置可产生多条记录。创建时间由 DB 写入 UTC。

**`DELETE /api/bookmarks/delete?id=<id>`**

- 查询参数：`id`（integer，必填）。
- 成功 `200`：`{"ok": true, "deleted": 1}`；`deleted` 为实际删除行数（不存在时为 0，仍返回 `ok:true`）。
- 错误：缺 `id`/类型不符 → 400 纯文本；`id <= 0` → `400 {"error":"id 必须大于 0"}`；`401`；`500`。

### 6.7 `GET /api/book/search`

- 鉴权：登录
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B2，6/6）：4 章缓存 3 章（1 章故障）；关键词命中 **2 章**，`snippet` 含关键词；未缓存 1 章时 `complete=false`，全部缓存时 `complete=true`；空 `q`/201 字 400；`limit=999` 夹到 50。**超时 504 未触发**（实测 200 / 1 ms，小数据量）。
- 查询参数：

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `book_id` | string | 是 | — | 书籍 URL（不是书名/ID 数字） |
| `q` | string | 是 | — | 关键词；`trim` 后 1..200 字符 |
| `limit` | integer | 否 | 20 | 命中章节数上限，clamp 1..50 |

- 成功 `200`：

```json
{
  "ok": true,
  "book_id": "https://example.com/book/123",
  "query": "伏笔",
  "limit": 20,
  "truncated": false,
  "hits": [
    {
      "chapter_index": 12,
      "chapter_url": "https://example.com/book/123/12.html",
      "chapter_title": "第十二章",
      "match_count": 3,
      "offset": 120,
      "snippet": "……这里提到伏笔，后面会回收……"
    }
  ],
  "total_chapters": 300,
  "cached_chapters": 300,
  "uncached_chapters": 0,
  "complete": true,
  "note": "已覆盖全部缓存章节",
  "elapsed_ms": 15
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 true |
| `book_id` | string | 否 | 回显请求值（已 `trim`） |
| `query` | string | 否 | 回显关键词（已 `trim`） |
| `limit` | integer | 否 | 生效上限（clamp 后） |
| `truncated` | boolean | 否 | 是否还有更多命中章节未返回 |
| `hits` | object[] | 否 | 命中章节，按 `chapter_index` 升序；**每章最多 1 条**（首个命中） |
| `hits[].chapter_index` | integer | 否 | 0 基章节下标 |
| `hits[].chapter_url` | string | 否 | 章节 URL |
| `hits[].chapter_title` | string | 是 | 章节标题；可空 |
| `hits[].match_count` | integer | 否 | 该章内命中次数（ASCII 大小写不敏感，按 LIKE 语义） |
| `hits[].offset` | integer | 否 | **首个命中在正文中的字符偏移**（不是字节偏移） |
| `hits[].snippet` | string | 否 | 上下文片段：命中前 40 字符 + 关键词 + 后 40 字符，两端截断处加 `…` |
| `total_chapters` | integer | 否 | `book_chapters` 中该书的章节总数；目录未缓存为 0 |
| `cached_chapters` | integer | 否 | `book_content` 中该书已缓存正文的章节数 |
| `uncached_chapters` | integer | 否 | `max(0, total_chapters - cached_chapters)` |
| `complete` | boolean | 否 | `uncached_chapters == 0 && total_chapters > 0` |
| `note` | string | 否 | 覆盖度提示，三种固定文案见下 |
| `elapsed_ms` | integer | 否 | 查询耗时（毫秒） |

- `note` 文案：
  - `total_chapters == 0`：`"目录未缓存，无法判断是否覆盖全书；仅返回已缓存正文中的命中"`，此时 `complete=false`。
  - `uncached_chapters == 0` 且 `total_chapters > 0`：`"已覆盖全部缓存章节"`。
  - 其余：`"仅搜索已缓存章节，未缓存章节未参与检索"`。
- 错误：
  - `400 {"error":"book_id 不能为空"}` / `{"error":"q 不能为空"}` / `{"error":"q 最多 200 字"}`。
  - `504 {"error":"书内搜索超过 5s 预算，请缩短关键词"}`：查询超过 5 秒（`SEARCH_TIMEOUT_SECS = 5`）。
  - 缺参/JSON 非法 → 400/422 纯文本；`401`；`500`。
- 行为：
  - **只搜索 `book_content` 中已缓存且 `content` 非空的章节**，不触发任何网络抓取；未缓存章节不会出现在结果里。
  - 匹配使用 SQLite `LIKE`（ASCII 大小写不敏感），`%`/`_`/`\` 按字面量转义。
  - 同一章节只返回首个命中（`hits` 按章节去重）。
  - 先取 `limit + 1` 条判断 `truncated`，再截断到 `limit`。
  - `complete=true` 仅表示**已缓存章节全覆盖**，不等于整本书；目录未缓存时永远 `false`。

### 6.8 `POST /api/book/cache-all`

- 鉴权：登录
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B3，4/4）：`limit` 截断 `limited=true attempted=2 cached=2`；全量缓存单章失败 `cached=3 failed=1`，失败项定位到 `b1-c3`，整体 `ok=true`（**逐章失败隔离已验证**）；无目录/空 `book_url` 400。**`skipped_budget` 预算跳过未单独验证**（预算预留函数仅单测）。
- 请求体：

| 字段 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `book_url` | string | 是 | — | 书籍 URL；也接受别名 `book_id` |
| `source` | string | 否 | — | 书源 ID；缺省读 `book_meta.origin` |
| `limit` | integer | 否 | 200 | 本次最多抓取章节数，clamp **1..500** |

示例（伪造值）：

```json
{ "book_url": "https://example.com/book/123", "limit": 50 }
```

- 成功 `200`：

```json
{
  "ok": true,
  "book_url": "https://example.com/book/123",
  "source": "https://example.com",
  "total_chapters": 300,
  "already_cached": 20,
  "attempted": 50,
  "cached": 48,
  "failed": 2,
  "skipped_budget": 0,
  "limited": true,
  "limit": 50,
  "budget_bytes": 52428800,
  "used_bytes_before": 10485760,
  "used_bytes_after": 20971520,
  "failures": [
    { "chapter_index": 30, "chapter_url": "https://example.com/book/123/30.html", "chapter_title": "第三十章", "error": "timeout" }
  ],
  "elapsed_ms": 8000
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 true |
| `book_url` / `source` | string | 否 | 回显/解析后的书籍 URL 与书源 ID |
| `total_chapters` | integer | 否 | 目录中的章节总数（**含分卷行**） |
| `already_cached` | integer | 否 | `total_chapters - pending.len()`：已缓存正文的章节 + 被跳过的分卷行；**不是** `total_chapters - attempted`（`attempted` 已被 `limit` 截断） |
| `attempted` | integer | 否 | 本次实际尝试抓取的章节数（≤ `limit`） |
| `cached` | integer | 否 | 本次成功写入的章节数 |
| `failed` | integer | 否 | 本次失败数，= `failures.length` |
| `skipped_budget` | integer | 否 | 因超出缓存预算被跳过的章节数 |
| `limited` | boolean | 否 | 是否因 `limit` 截断（待抓数量 > `limit`） |
| `limit` | integer | 否 | 生效的 `limit`（clamp 后） |
| `budget_bytes` | integer | 否 | 本次可用缓存预算（字节） |
| `used_bytes_before` / `used_bytes_after` | integer | 否 | 执行前后该用户正文缓存占用 |
| `failures[]` | object[] | 否 | 失败明细，按 `chapter_index` 升序 |
| `failures[].chapter_index` | integer | 否 | 章节下标 |
| `failures[].chapter_url` | string | 否 | 章节 URL |
| `failures[].chapter_title` | string | 是 | 章节标题 |
| `failures[].error` | string | 否 | 失败原因 |
| `elapsed_ms` | integer | 否 | 总耗时（毫秒） |

- 错误：
  - `400 {"error":"book_url 不能为空"}`。
  - `400 {"error":"目录未缓存，请先调用 GET /api/book/toc"}`：`book_chapters` 中该书无行。
  - `400 {"error":"缺少 source，且 book_meta 未记录书源"}`：既没传 `source`，`book_meta.origin` 也为空。
  - `404 source not found`：`source` 不存在；`401`；`500`。
- 行为：
  - 需要目录已缓存；否则先调 6.2。
  - 跳过 `is_volume = true` 的分卷行；跳过 `book_content` 中已有正文的章节。
  - **并发 3**；按 `index` 升序取前 `limit` 个待抓章节。
  - 预算 = `min(用户 cache_limit_mb, 全局 CACHE_LIMIT_MB) - 当前已用`（下限 0）；抓取正文后按字节**原子预留**，预留失败即置 `skipped_budget`，并停止后续抓取（后续都计为 `skipped_budget`）。
  - **部分失败仍返回 `200`**，通过 `failed`/`failures` 表达；不会整批回滚已成功的章节。
  - 写入使用与 6.4 相同的 upsert，并回填 `book_url`/`chapter_index`。

### 6.9 `POST /api/book/cache/clear`

- 鉴权：登录
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B3，4/4）：按书清理 `scope=book`、`deleted>=3`；用户级 `scope=user` 并返回 `bytes_freed`；不传 `book_id` 走用户级（不是 400）。
- 请求体：可为空对象 `{}`；字段全部可选：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `book_id` | string | 否 | 只清该书的正文缓存；也接受别名 `book_url` |
| — | — | — | 不传/空串/全空白 → 清空**当前用户的全部**正文缓存 |

示例（伪造值）：

```json
{ "book_id": "https://example.com/book/123" }
```

- 成功 `200`：

```json
{ "ok": true, "scope": "book", "book_id": "https://example.com/book/123", "deleted": 120, "bytes_freed": 10485760 }
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 true |
| `scope` | string | 否 | `"book"`（按书）或 `"user"`（全部） |
| `book_id` | string | 是 | 按书清理时回显；全部清理为 `null` |
| `deleted` | integer | 否 | 删除的 `book_content` 行数 |
| `bytes_freed` | integer | 否 | 删除行的 `size_bytes` 合计（近似释放字节） |

- 错误：JSON 非法 → 400/422 纯文本；`401`；`500`。
- 行为：
  - **只删 `book_content`**，不动 `book_chapters`、`book_meta`、书架/进度/统计。
  - 删除不存在的书返回 `deleted=0`，仍 `ok:true`。
  - 清理后服务端会尝试回收 SQLite 空闲页，文件大小不保证立即下降。

### 6.10 `GET /api/book/export/info`

- 鉴权：登录
- 验证状态：**已验证**（HTTP `:3000`，新二进制 14:46:58）：72 章样本返回 `total_chapters=72 / cached_chapters=5 / missing_chapters=67 / complete=false`；缺参、空参、无目录均 400。
- 用途：导出前预览：这本书有多少章、已缓存多少、还缺多少、预计导出多大。**只读当前用户库，不联网。**
- 查询参数：

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `book_url` | string | 是 | 书籍主键；`trim()` 后使用，空串 → 400 |

- 成功 `200`（示例为伪造值）：

```json
{
  "book_url": "https://example.com/book/123",
  "name": "示例书名",
  "author": "示例作者",
  "source": "示例书源",
  "total_chapters": 120,
  "cached_chapters": 80,
  "missing_chapters": 40,
  "cached_bytes": 1048576,
  "complete": false,
  "export_url": "/api/book/export/txt?book_url=https%3A%2F%2Fexample.com%2Fbook%2F123"
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `book_url` | string | 否 | 回显 `trim()` 后的值 |
| `name` | string | 否 | `book_meta.name`；为空时回落为 `book_url` |
| `author` | string | 否 | `book_meta.author`；为空时回落为 `"未知"` |
| `source` | string | 否 | 优先 `book_meta.origin_name`，其次 `origin`；都为空时是空串 |
| `total_chapters` | integer | 否 | **正文章节数**：`book_chapters` 中 `is_volume=0` 且 `url NOT LIKE 'volume:%'` 的行数；分卷行不计 |
| `cached_chapters` | integer | 否 | `book_content` 中存在且 `trim(content)` 非空的章节数 |
| `missing_chapters` | integer | 否 | `max(total - cached, 0)` |
| `cached_bytes` | integer | 否 | 已缓存章节的 `size_bytes` 合计（字节） |
| `complete` | boolean | 否 | `cached_chapters >= total_chapters`；全书未缓存但 `total=0` 时也为 `true` |
| `export_url` | string | 否 | 相对路径，已对 `book_url` 做 URL 编码；调用方自行拼接 origin，并需带登录态 |

- 错误：
  - 缺 `book_url` → `400` 纯文本：``Failed to deserialize query string: missing field `book_url` ``
  - `book_url=""` → `400 {"error":"book_url 不能为空"}`
  - 目录未缓存（`book_chapters` 无行）→ `400 {"error":"目录未缓存，请先调用 GET /api/book/toc"}`
  - `401`；`500`
- 行为：
  - 每次实时统计，不使用导出缓存。
  - `is_pay=true` 不做特殊处理；导出统计按“正文是否已缓存”计算。**`is_pay=true` 的真实样本链路仅单测，未验证。**

### 6.11 `GET /api/book/export/txt`

- 鉴权：登录
- 验证状态：**已验证**（HTTP `:3000`，新二进制 14:46:58）：`200 text/plain; charset=utf-8` + 附件头 + 5 个 `x-*` 头；文件头字段、`【未缓存】` 占位、页脚计数齐全；72 章样本 **78,718 B / 15 ms**。
- 性能实测（验收样本，2026-11-03）：**34,586 B / 17 ms**，常驻内存 **18.4 → 18.4 MB**（峰值约等于单章正文 + 16 KiB 缓冲，不随总章节数线性增长）。本轮未独立复测内存。
- 用途：把**当前用户已缓存的正文**导出为单个 TXT；缺失章节写占位说明，**不静默丢章**。本接口**只读缓存、不联网抓取**。
- 查询参数：

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `book_url` | string | 是 | 书籍主键；空串/无目录的错误与 6.10 相同 |

- 成功 `200` 响应头：

| 头 | 值 | 说明 |
|---|---|---|
| `Content-Type` | `text/plain; charset=utf-8` | UTF-8 文本 |
| `Content-Disposition` | `attachment; filename="export.txt"; filename*=UTF-8''<编码后文件名>` | ASCII 兜底名 + RFC 5987 UTF-8 名，兼容中文书名 |
| `x-total-chapters` | 整数 | 正文章节总数（不含分卷） |
| `x-cached-chapters` | 整数 | 计划写入的已缓存章节数 |
| `x-missing-chapters` | 整数 | `max(total - cached, 0)` |
| `x-written-chapters` | 整数 | **实际**写入的已缓存章节数；导出途中缓存被并发清理时可能小于 `x-cached-chapters` |
| `x-file-bytes` | 整数 | 生成文件最终字节数（响应体为 chunked 流，不一定有 `Content-Length`） |

- 文件名：`<清洗后的书名>-YYYYMMDD-HHMMSS.txt`；书名中控制字符与 `/\:*?"<>|` 替换为 `_`，去首尾点号，最长 60 字符，空则用 `book`。
- 文件内容（UTF-8）：

```text
书名：{name}
作者：{author}
书源：{source}
书籍地址：{book_url}
导出时间：YYYY-MM-DD HH:MM:SS      ← 服务器本地时区
章节：已缓存 {cached}/{total} 章，缺 {missing} 章
提示：缺失章节已用占位说明标出，不会静默丢章。   ← 仅 missing>0 时出现

============================================================

===== [1/72] 第一章标题 =====

<正文>

===== [2/72] 第二章标题 =====

【未缓存】本章正文尚未缓存。章节地址：<chapter_url>
请先在阅读端打开本章，或执行“缓存全部”后重新导出。

【分卷】第一卷

===== [5/72] ... =====

===== 导出结束：实际写入 {written}/{total} 章 =====
```

- 规则：
  - 章节按 `book_chapters."index"` 排序；标题为空时回落为 `第 N 章`。
  - **分卷行**（`is_volume=1` 或 `url` 以 `volume:` 开头）只写 `【分卷】<title>` 分隔，**不计入** `total`，也不占章节序号。
  - 正文按 `trim()` 后写入；只有空白的内容按“未缓存”处理。
  - **多本导出不提供 zip**：由调用方对每本书各请求一次本接口，得到多个独立 TXT；服务端没有批量打包接口。
  - **前端入口（2026-11-03 复核）**：`ExportDialog.svelte` 先调 6.10 `export/info` 展示缓存/缺失章节，再下载本接口；书架页与阅读器均有入口，并已接入 `api.exportInfo`/`api.exportTxt`。
- 错误：
  - 缺 `book_url` → `400` 纯文本：``Failed to deserialize query string: missing field `book_url` ``
  - `book_url=""` → `400 {"error":"book_url 不能为空"}`
  - 目录未缓存 → `400 {"error":"目录未缓存，请先调用 GET /api/book/toc"}`
  - `401`；`500`
- 实现/资源（低内存约束）：
  - 逐章查询、逐章写入临时文件（`{DATA_DIR}/exports/{uid}/`），**一次只在内存持有一章正文**；随后 16 KiB 分块流式回传，读完删除临时文件。
  - 客户端中断时流式删除不会执行，服务端在后续导出时清理 **1 小时前**的残留文件。
  - 临时文件是完整生成的；导出开始后即使缓存被清理，已生成的文件仍会继续传输。
- **未验证**：
  - 超大书压力样本：本轮仅 72 章，未做 10k+ 章 / GB 级文件。
  - `is_pay=true` 章节：导出不做付费过滤（只要已缓存就导出），真实付费样本链路仅单测。

---

## 7. 书架与进度

> 数据均在用户库，天然按用户隔离。所有接口需要登录，但 **不校验 admin**。
> 书架接口不返回书籍正文/目录，只返回 `BookshelfItem` 摘要。

### 7.1 `GET /api/bookshelf`

- 鉴权：登录
- 查询参数：

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `group` | string | 否 | 精确匹配分组；空串视为未传（返回全部） |
| `book_id` | string | 否 | **当前被忽略**（`let _ = q.book_id;`），见 TODO |

- 成功 `200`：`BookshelfItem[]`

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `book_id` | string | 否 | 书籍 URL |
| `title` | string | 是 | 优先 `book_meta.name`，否则 `bookshelf.title`（后者非空） |
| `author` | string | 是 | 优先 `book_meta.author`，否则 `bookshelf.author` |
| `cover` | string | 是 | 优先 `book_meta.cover_url`，否则 `bookshelf.cover` |
| `source_id` | string | 是 | 加架时记录的书源 ID |
| `group` | string | 是 | `COALESCE(bookshelf.group, book_meta.group)` |
| `chapter_index` | integer | 否 | 最近章节下标；无进度为 0 |
| `position` | number | 否 | 章内位置 0..1；无进度为 0 |
| `total_chapter_num` | integer | 否 | 优先 `book_meta.total_chapter_num`，为 0 时回退 `book_stats.total_chapters`，都无则 0 |
| `reading_progress` | number | 否 | 0..100，`(chapter_index + clamp(position,0,1)) / total * 100`，**含章内比例** |
| `added_at` | string | 是 | 入架时间（UTC `YYYY-MM-DD HH:MM:SS`） |
| `last_read_at` | string | 是 | 最近进度更新时间；无进度 `null` |

示例（伪造值）：

```json
[
  {
    "book_id": "https://example.com/book/123",
    "title": "示例小说",
    "author": "张三",
    "cover": "https://example.com/cover/123.jpg",
    "source_id": "https://example.com",
    "group": "默认",
    "chapter_index": 12,
    "position": 0.6,
    "total_chapter_num": 300,
    "reading_progress": 4.0,
    "added_at": "2026-11-01 10:00:00",
    "last_read_at": "2026-11-02 11:30:00"
  }
]
```

- 错误：`401`；`500`。
- 行为：
  - 排序：`COALESCE(reading_progress.updated_at, bookshelf.added_at) DESC`（最近读/最近加在前），**没有置顶、没有自定义排序**（[bookshelf.rs](../server/src/routes/bookshelf.rs#L76-L103)）。
  - 分组筛选用 `COALESCE(bs.group, bm.group) = ?`，因此只在 `book_meta` 上设置了分组的书也能按该分组命中。
  - `total_chapter_num` 只有调用过 `/api/book/info` 或进度上报后才会被写入，刚加架的书通常为 0。
  - 不返回 `can_update`、最新章节、自定义封面/简介等字段（见 13.2 提案）。

### 7.2 `POST /api/bookshelf`

- 鉴权：登录
- 请求体：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `book_id` | string | 是 | 书籍 URL |
| `source_id` | string | 否 | 书源 ID |
| `name` | string | 否 | 书名，写入 `bookshelf.title`（该列 NOT NULL，但缺失时会被拒绝） |
| `author` | string | 否 | 作者 |
| `cover` | string | 否 | 封面 |
| `group` | string | 否 | 分组 |

- 注意：`bookshelf.title` 是 `NOT NULL` 且无默认值。首次插入若缺 `name`，SQLite 会报 500（`NOT NULL constraint failed`）。**首次加架必须传 `name`**。
- 成功 `200`：`{"ok": true}`
- 错误：缺 `book_id`/`name`（首次）→ 500 DB 错误；`401`；`500`。
- 行为：
  - 按 `book_id` upsert；**冲突时只更新 `group`**（`COALESCE(excluded.group, 现有 group)`），`title/author/cover/source_id` 不会被新值覆盖（[bookshelf.rs](../server/src/routes/bookshelf.rs#L105-L141)）。
  - 同时 upsert `book_meta`，用 `COALESCE` 填充 `name/author/cover_url/group`，不覆盖已有值。
  - 不创建 `reading_progress` 记录。
  - 不存在“阅读时自动加入”的独立开关；前端打开阅读器自行调用本接口（现有 `Reader.svelte` 即如此）。

### 7.3 `DELETE /api/bookshelf`

- 鉴权：登录
- 查询参数：`book_id`（string，必填）
- 成功 `200`：`{"ok": true}`
- 错误：缺参 → 400 纯文本；`401`；`500`。
- 行为：只删 `bookshelf` 行，**不删** `book_meta`、`reading_progress`、`book_stats`、正文缓存；删不存在的书也返回 `ok:true`。

### 7.4 `POST /api/bookshelf/group`

- 鉴权：登录
- 请求体：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `book_id` | string | 否（与 `book_ids` 至少一个） | 单本 |
| `book_ids` | string[] | 否（与 `book_id` 至少一个） | 批量 |
| `group` | string | 否 | 目标分组；缺省/空串/空白 → 移出分组（写 `NULL`） |

- 成功 `200`：`{"ok": true, "updated": 2}`
- 错误：两个 ID 字段都空 → `400 {"error":"book_id or book_ids required"}`；`401`；`500`。
- 行为：
  - 逐本更新 `bookshelf.group` 与 `book_meta.group`；`updated` 是**请求 ID 数**，不校验是否真的存在（可能大于实际行数）。
  - 非事务；重复 ID 会重复更新。
  - 不校验分组是否存在于 `book_groups`。

### 7.5 `GET /api/bookshelf/stats`

- 鉴权：登录
- 参数：无
- 成功 `200`：

```json
{
  "streak_days": 3,
  "longest_streak_days": 9,
  "today_seconds": 1260,
  "books": [
    {
      "book_id": "https://example.com/book/123",
      "name": "示例小说",
      "total_chapters": 300,
      "read_chapters": 13,
      "completion": 4.33,
      "last_read_at": "2026-11-02 11:30:00"
    }
  ]
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `streak_days` | integer | 否 | 当前连续天数；`last_day` 不是今天/昨天则 0 |
| `longest_streak_days` | integer | 否 | 历史最长 |
| `today_seconds` | integer | 否 | 今日阅读秒数（本地日期） |
| `books` | object[] | 否 | `book_stats` 按 `last_read_at DESC` 取前 50 |
| `books[].book_id` | string | 否 | 书籍 URL |
| `books[].name` | string | 是 | 可能 null |
| `books[].total_chapters` | integer | 否 | 0 表示未知 |
| `books[].read_chapters` | integer | 否 | 已读章节数（可能大于总章节数） |
| `books[].completion` | number | 否 | 0..100，`read/total`；total=0 时 0 |
| `books[].last_read_at` | string | 否 | UTC 时间字符串 |

- 错误：`401`；`500`。
- 行为：只读聚合，供书架页内联展示；数据源是 `reading_streak` / `reading_stats` / `book_stats`（[bookshelf.rs](../server/src/routes/bookshelf.rs#L188-L244)）。不返回阅读时长（`book_stats` 没有 seconds 列）。

### 7.6 `GET /api/bookshelf/groups`

- 鉴权：登录
- 参数：无
- 成功 `200`：数组，按 `order, id` 升序：

```json
[
  {
    "group_id": 1,
    "group_name": "默认",
    "cover": null,
    "order": 0,
    "book_sort": 0,
    "enable_refresh": true,
    "show": true
  }
]
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `group_id` | integer | 否 | 分组 id |
| `group_name` | string | 否 | DB `name`（NOT NULL） |
| `cover` | string | 是 | 分组封面（可用 `POST /api/bookshelf/groups` 写入） |
| `order` | integer | 否 | 分组排序 |
| `book_sort` | integer | 否 | 组内排序方式（语义未定义） |
| `enable_refresh` | boolean | 否 | 当前无写入接口 |
| `show` | boolean | 否 | 是否展示（可用 `POST /api/bookshelf/groups` 写入） |

- 错误：`401`；`500`。
- 行为：`SELECT id,name,cover,order,book_sort,enable_refresh,show,order`（`order` 查了两次但只输出一次）。

### 7.7 `POST /api/bookshelf/groups`

- 鉴权：登录
- 验证状态：**仅单测**（`cover`/`show` 的 `COALESCE` 局部更新有单测）；**该接口无 HTTP 冒烟证据**。
- 请求体：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `id` | integer | 否 | 有值 = 更新；无值 = 新增 |
| `name` | string | 新增时必填 | 分组名 |
| `order` | integer | 否 | 排序，新增默认 0；更新时未传保持原值 |
| `book_sort` | integer | 否 | 组内排序，新增默认 0；更新时未传保持原值 |
| `cover` | string | 否 | 分组封面；更新时未传/空串保持原值，传非空覆盖 |
| `show` | boolean | 否 | 是否展示；新增默认 `true`，更新时未传保持原值 |

- 成功 `200`：`{"ok": true, "id": 3}`
- 错误：新增但 `name` 为空 → `400 {"error":"group name required"}`；更新时 `id` 不存在 → 仍返回 `ok:true`（更新 0 行）；`401`；`500`。
- 行为：
  - 更新全部用 `COALESCE(?, 原值)`：请求里没给的字段保持原值；`name`/`cover` 空串被当作“不更新”，不会清空。
  - **仍不能更新** `enable_refresh`（请求结构里没有该字段）。
  - 不校验重名；允许同名分组。

### 7.8 `DELETE /api/bookshelf/groups` 与 `DELETE /api/bookshelf/groups/delete`

- 鉴权：登录
- 查询参数：`id`（integer，必填）
- 成功 `200`：`{"ok": true}`
- 错误：缺参/类型不符 → 400 纯文本；`401`；`500`。
- 行为：**两个路径同一个 handler**（别名）。只删 `book_groups` 行，**不清空**书籍的 `group` 字段（Legado 行为，[bookshelf.rs](../server/src/routes/bookshelf.rs#L313-L325)）。删除不存在的 id 仍返回 `ok:true`。

### 7.9 `GET /api/bookshelf/progress`

- 鉴权：登录
- 查询参数：`book_id`（string，必填）
- 成功 `200`（有记录时）：

```json
{
  "chapter_index": 12,
  "position": 0.6,
  "total_chapters": 300,
  "percent": 4.2,
  "updated_at": "2026-11-02 11:30:00"
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `chapter_index` | integer | 否 | 0 基章节下标 |
| `position` | number | 否 | 0..1 章内位置（原样返回） |
| `total_chapters` | integer | 否 | 优先 `book_meta.total_chapter_num`，为 0 时回退 `book_stats.total_chapters`，都无则 0 |
| `percent` | number | 否 | `(chapter_index + clamp(position,0,1)) / total_chapters * 100`，**已含章内比例**，钳制 0..100；无记录 0 |
| `updated_at` | string | 是 | UTC 时间字符串；无记录 `null` |

- 无记录时固定返回：

```json
{ "chapter_index": 0, "position": 0, "total_chapters": 0, "percent": 0.0, "updated_at": null }
```

（`total_chapters` 仍会尝试从 `book_meta` 读取，有值则返回该值，上例假设无元数据。）

- 错误：缺参 → 400 纯文本；`401`；`500`。
- 行为：`percent` 恒在 `0..100`（服务端钳制），`chapter_index > total_chapters` 也不会超过 100。

### 7.10 `POST /api/bookshelf/progress`

- 鉴权：登录
- 请求体：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `book_id` | string | 是 | 书籍 URL |
| `chapter_index` | integer | 否 | 默认 0；负数被压成 0 |
| `position` | number | 否 | 默认 0；**只接受 0..1**（`0`/`1` 合法）；负数、`>1`、NaN 一律 400 |
| `total_chapters` | integer | 否 | 若 >0，覆盖 `book_meta.total_chapter_num` 用于统计；否则回退读取 |

- 成功 `200`：

```json
{ "ok": true, "chapter_index": 12, "position": 0.6 }
```

（`position` 是**归一化后**的值。）

- 错误：缺 `book_id` / JSON 非法 → 400/422 纯文本；`position` 越界 → `400 position 必须是 0..1 的章内比例`；`401`；`500`。
- 行为：
  - upsert `reading_progress`（按 `book_id`），刷新 `updated_at`（UTC）。
  - 同时 upsert `book_stats`：`read_chapters = MAX(现有, chapter_index + 1)`，`last_read_at = now`，`total_chapters` 仅在 >0 时覆盖（[bookshelf.rs](../server/src/routes/bookshelf.rs#L365-L420)）。
  - **不更新** `reading_streak` 与 `reading_stats`；连续天数只由 `/api/stats/reading` 维护。
  - 若 `book_meta` 不存在，`total_chapters` 取 0，`book_stats` 仍会写入。
  - 非事务，两次 upsert 之间失败可能只写进度不写统计。

### 7.11 `POST /api/bookshelf/batch`

- 鉴权：登录
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B6，4/4）：`set_group updated=2 failed=1`，不存在项 `ok=false`「不在书架」；`remove_group`/`remove` 均逐项返回；非法 action/空 ids 400。
- 请求体（**JSON**）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `action` | string | 是 | `set_group` / `remove_group` / `remove`；大小写不敏感，前后空白被裁剪 |
| `book_ids` | string[] | 是 | 书籍 URL 列表；逐项 `trim`、去空、去重（重复项静默丢弃） |
| `group` | string | 否 | 仅 `set_group` 使用；缺省/空串/全空白 → 移出分组（写 `NULL`） |

- 示例（伪造值）：

```json
{ "action": "set_group", "book_ids": ["https://example.com/book/123", "https://example.com/book/456"], "group": "在追" }
```

- 成功 `200`：

```json
{
  "ok": true,
  "action": "set_group",
  "group": "在追",
  "updated": 1,
  "failed": 1,
  "results": [
    { "book_id": "https://example.com/book/123", "ok": true, "error": null },
    { "book_id": "https://example.com/book/456", "ok": false, "error": "书籍不在书架" }
  ]
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 true |
| `action` | string | 否 | 归一化后的动作（`add_group` 会变成 `set_group`） |
| `group` | string | 是 | 生效的分组：`remove_group` 强制 `null`；`set_group` 空/空白 → `null`；`remove` 原样透传请求里的 `group`（通常不传） |
| `updated` | integer | 否 | 成功项数 |
| `failed` | integer | 否 | 失败项数 |
| `results[]` | object[] | 否 | **逐项结果**，顺序与去重后的 `book_ids` 一致 |
| `results[].book_id` | string | 否 | 书籍 URL |
| `results[].ok` | boolean | 否 | 该项是否成功 |
| `results[].error` | string | 是 | 失败原因；成功为 `null` |

- 错误：
  - `action` 不支持 → `400 {"error":"不支持的 action：xxx（可用 set_group/remove_group/remove）"}`。
  - 去重后无有效 ID → `400 {"error":"book_ids 不能为空"}`。
  - 超过 **500** 本 → `400 {"error":"单次最多 500 本，请分批调用"}`。
  - JSON 非法 → 400/422 纯文本；`401`；`500`。
- 行为：
  - **逐项独立事务，不是全有或全无**；某一项失败不影响其他项，最终由 `results` 表达。
  - `add_group` 是 `set_group` 的别名（Legado 习惯），不单独处理。
  - `set_group`：同时更新 `bookshelf.group` 与 `book_meta.group`；`group` 为空/空白 → 写 `NULL`（移出分组）。
  - `remove_group`：**强制忽略**请求里的 `group`，按“移出分组”处理。
  - `remove`：只删 `bookshelf` 行；**不删** `book_meta`、`reading_progress`、`book_stats`、正文缓存（与 7.3 一致）。
  - 书架中不存在的 `book_id` → `ok:false`、`error:"书籍不在书架"`（`UPDATE`/`DELETE` 影响 0 行）。
  - 其他 DB 错误以该项 `error` 的原文返回，不影响其他项。
  - 不校验分组是否存在于 `book_groups`（与 7.4 一致）。

---

## 8. 替换规则

> 数据在用户库 `replace_rules`，按用户隔离。规则按 `order` 升序执行（`purify` 中为 `ORDER BY "order", id`）。
> 规则只在 `GET /api/book/content` 输出时生效，不修改缓存里的原始正文。

### 8.1 `GET /api/replace-rules`

- 鉴权：登录
- 参数：无
- 成功 `200`：`ReplaceRule[]`（**snake_case**，按 `order` 升序，无 id 次序兜底）：

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `id` | integer | 是 | 新增前为 null；列表中有值 |
| `name` | string | 是 | 规则名 |
| `group` | string | 是 | 分组（仅展示，无分组接口） |
| `pattern` | string | 否 | 匹配串/正则；DB NOT NULL |
| `replacement` | string | 否 | 替换串；DB NOT NULL |
| `scope` | string | 是 | 作用域：每行一个书名或书源域名 |
| `scope_title` | boolean | 否 | 是否作用于标题 |
| `scope_content` | boolean | 否 | 是否作用于正文 |
| `exclude_scope` | string | 是 | 排除作用域，命中则跳过 |
| `is_enabled` | boolean | 否 | 是否启用 |
| `is_regex` | boolean | 否 | 是否按正则解释 `pattern` |
| `timeout_millisecond` | integer | 否 | Legado `ReplaceRule` 兼容字段（正则超时毫秒）。Rust `regex` 为线性时间，**执行时不消费**；仅原样存取与导入导出。**不是书源 HTTP 超时**，后者见 4.6 `BookSource.timeout_millisecond` |
| `order` | integer | 否 | 执行顺序 |

示例（伪造值）：

```json
[
  {
    "id": 1,
    "name": "去广告",
    "group": "默认",
    "pattern": "本站域名.*?阅读",
    "replacement": "",
    "scope": null,
    "scope_title": true,
    "scope_content": true,
    "exclude_scope": null,
    "is_enabled": true,
    "is_regex": true,
    "timeout_millisecond": 0,
    "order": 0
  }
]
```

- 错误：`401`；`500`。
- 行为：
  - `SELECT * FROM replace_rules ORDER BY "order"`（[replace.rs](../server/src/routes/replace.rs#L52-L61)）。
  - 不返回用户设置（`use_replace_rule` 通过 `/api/settings`）。

### 8.2 `POST /api/replace-rules`

- 鉴权：登录
- 请求体：`ReplaceRule`（**整体覆盖语义**，不是 PATCH）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `id` | integer | 否 | 有值 = 更新该 id；无值 = 新增 |
| `name` | string | 否 | 缺省 null |
| `group` | string | 否 | 缺省 null |
| `pattern` | string | 否 | 缺省 `""`（会写入 NOT NULL 列） |
| `replacement` | string | 否 | 缺省 `""` |
| `scope` | string | 否 | 缺省 null |
| `scope_title` | boolean | 否 | 缺省 false |
| `scope_content` | boolean | 否 | 缺省 false |
| `exclude_scope` | string | 否 | 缺省 null |
| `is_enabled` | boolean | 否 | 缺省 false |
| `is_regex` | boolean | 否 | 缺省 false |
| `timeout_millisecond` | integer | 否 | 缺省 0 |
| `order` | integer | 否 | 缺省 0 |

- 成功 `200`：

```json
{ "ok": true, "id": 5 }
```

- 错误：JSON 非法/类型不符 → 400/422 纯文本；`401`；`500`。
- 行为：
  - 带 `id`：`UPDATE ... WHERE id=?`，**所有字段都写入**；未传的字段按上表默认值覆盖（会把已有规则清空）。
  - `id` 不存在：不报 404，返回 `ok:true` 且回显该 id（实际更新 0 行）。
  - 不带 `id`：`INSERT`，返回 `last_insert_rowid()`。
  - 无字段级校验（`pattern` 可为空，正则可非法）。
  - 更新/新增都**不更新 `updated_at`**（表没有该列）。

### 8.3 `GET /api/replace-rules/export`

- 鉴权：登录
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B4，8/8）：非空导出为数组，camelCase 布尔字段齐全且**不含本地 `id`**；`?id` 返回单条，不存在 404；空库 `[]`。
- 查询参数：

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `id` | integer | 否 | 给出 = 只导出该条；不给出 = 导出全部 |

- 成功 `200`：
  - 带 `id`：**单个对象**（camelCase）。
  - 不带 `id`：**裸数组**（camelCase），顺序为 `order ASC, id ASC`，可直接交给 Legado 导入。
  - 无论哪种形态，**都不含本地自增 `id`**。

单条示例（伪造值）：

```json
{
  "name": "去广告",
  "group": null,
  "pattern": "广告",
  "replacement": "",
  "scope": "某书",
  "isRegex": false,
  "isEnabled": true,
  "scopeTitle": true,
  "scopeContent": true,
  "excludeScope": null,
  "order": 0,
  "timeoutMillisecond": 1000
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `name` | string | 是 | 规则名 |
| `group` | string | 是 | 分组 |
| `pattern` | string | 否 | 匹配串/正则 |
| `replacement` | string | 否 | 替换串 |
| `scope` | string | 是 | 作用域 |
| `isRegex` | boolean | 否 | 是否正则 |
| `isEnabled` | boolean | 否 | 是否启用 |
| `scopeTitle` | boolean | 否 | 作用于标题 |
| `scopeContent` | boolean | 否 | 作用于正文 |
| `excludeScope` | string | 是 | 排除作用域 |
| `order` | integer | 否 | 执行顺序 |
| `timeoutMillisecond` | integer | 否 | 原样导出数据库值（**不保证为正**；导入时才会补 1000） |

- 错误：`id` 不存在 → `404 {"error":"replace rule not found"}`；`401`；`500`；`id` 非整数 → 400 纯文本。
- 行为：导出不做 Legado 默认值补齐，只是“数据库值 → camelCase”；未知字段不存在（`ReplaceRule` 没有 `extra`）。

### 8.4 `POST /api/replace-rules/import`

- 鉴权：登录
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B4，8/8）：导入 camelCase 规则 `inserted=1` 且可回读；空数组 `inserted=0 skipped=0`；缺规则/非 http URL 400；非法结构 400 并提示“第 1 条规则”。
- 请求体：Legado 规则包，三种形态：

| 形态 | 示例 | 说明 |
|---|---|---|
| 裸数组 | `[{"pattern":"a","replacement":"b"}]` | 每条新增 |
| 包装对象 | `{"name":"规则包","rules":[...]}` | 只取 `rules`；`rules` 为 `null` 视为空 |
| 单条对象 | `{"pattern":"a","replacement":"b"}` | 视为一条 |

- 导入规整规则（**契约行为，前端应依赖**）：
  - `id` **一律丢弃**，永远新增（不会覆盖同 id 规则）。
  - `isEnabled` 缺省 → `true`；`isRegex` 缺省 → `false`。
  - `scopeTitle` 与 `scopeContent` **都缺省** → 都置 `true`；只要显式给了其中一个，就保留原值（未给的那个保持 `false`）。
  - `timeoutMillisecond` 缺省/`<=0` → `1000`。
  - `pattern` 为空/纯空白 → 该条计入 `skipped`，不落库。
  - 字段名兼容 camelCase 与 snake_case（`is_enabled` 等同样识别）。
  - 单条结构非法（如 `pattern` 类型错误）→ **整批 400**，文案带序号：`第 N 条规则: invalid replace rule: ...`；校验发生在写库前，因此不会出现“导入了一半”。
  - **不去重、不合并**：重复调用会重复追加；`order` 缺省 `0`。
- 成功 `200`：

```json
{ "ok": true, "inserted": 3, "skipped": 1 }
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 `true` |
| `inserted` | integer | 否 | 实际写入条数 |
| `skipped` | integer | 否 | 因 `pattern` 为空被跳过的条数 |

- 错误：请求体为 `null` → `400 {"error":"规则包不能为空"}`；`{}` 或 `{"name":"x"}`（无 `rules`）→ 视为空规则包，`200 {ok:true,inserted:0,skipped:0}`；非法 JSON/类型 → 400/422 纯文本；`401`；`500`（写库失败时已写入的前几条会保留，**无事务**）。
- 行为：追加语义；不校验正则能否编译；不更新已有规则。

### 8.5 `POST /api/replace-rules/preview`

- 鉴权：登录
- 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B4，8/8）：字面替换 `final` 正确、`steps[0].hits=1`、`fetched=false`；**坏正则 200 不中断**，`skipped_reason` 含 `invalid_regex`、原文不变；**私网 URL 400**（拒绝回环/私网/链路本地）；缺规则/非 http URL 400。**5 MiB / 15 s 预算分支仍未验证**。
- 请求体（`Content-Type: application/json`）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `rules` | array \| object | 二选一 | 规则集合，接受与导入相同的三种形态 |
| `rule` | object \| array | 二选一 | 单条规则/多条；`rules` 与 `rule` 至少给一个 |
| `input` | string | 二选一 | 直接给待处理文本 |
| `url` | string | 二选一 | 由服务端 GET 抓取后处理；**仅 `http://` / `https://`** |
| `headers` | object | 否 | 抓取用请求头（键值均为字符串） |
| `book` | string | 否 | 书名，用于 `scope`/`excludeScope` 匹配 |
| `origin` | string | 否 | 书源域名/ID，用于作用域匹配 |
| `for_title` | boolean | 否 | `true` 按“标题”作用域（`scopeTitle`）；缺省 `false` 按正文作用域 |

- 优先级：`url` 非空时**忽略 `input`**；两者都缺 → 400。
- 成功 `200`：

```json
{
  "ok": true,
  "fetched": false,
  "input_length": 8,
  "final": "第1章 内容",
  "output_length": 8,
  "final_truncated": false,
  "steps": [
    {
      "index": 0,
      "name": "章号",
      "pattern": "第(\\d+)章",
      "replacement": "第$1章",
      "is_regex": true,
      "applied": true,
      "skipped_reason": null,
      "hits": 1,
      "output": "第1章 内容",
      "output_truncated": false
    }
  ]
}
```

> 示例为**伪造值**。

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 `true` |
| `fetched` | boolean | 否 | `true` 表示正文来自 `url` 抓取；`false` 表示来自 `input` |
| `input_length` | integer | 否 | 原始输入**字节数**（UTF-8） |
| `final` | string | 否 | 全部规则应用后的最终文本；最多 20 万字节 |
| `output_length` | integer | 否 | 最终文本完整字节数（可能大于 `final` 长度） |
| `final_truncated` | boolean | 否 | `final` 是否被截断 |
| `steps` | object[] | 否 | 按执行顺序，每条规则一项（**未启用的也出现**） |

- `steps[]` 字段：

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `index` | integer | 否 | 规则在本次请求中的 0 基下标（不是数据库 id） |
| `name` | string | 是 | 规则名 |
| `pattern` | string | 否 | 原样回显 |
| `replacement` | string | 否 | 原样回显 |
| `is_regex` | boolean | 否 | 原样回显 |
| `applied` | boolean | 否 | 是否真正执行（跳过为 `false`） |
| `skipped_reason` | string | 是 | 跳过原因，见下 |
| `hits` | integer | 否 | 执行前文本中的匹配次数；跳过为 `0` |
| `output` | string | 否 | 该步后的中间值。**已执行**的步骤最多 2 万字节；`disabled`/`scope_flag`/`scope_mismatch`/`excluded`/`empty_pattern`/`pattern_too_long` 直接回显当前中间值（最长受 5 MiB 输入预算限制）；`invalid_regex` 仍按 2 万字节截断 |
| `output_truncated` | boolean | 否 | `output` 是否被截断；上述前 6 种跳过原因恒 `false`，`invalid_regex` 可能为 `true` |

- `skipped_reason` 取值（与正文实际执行条件一致）：
  - `disabled`：`isEnabled=false`
  - `scope_flag`：`for_title` 时 `scopeTitle=false`，或正文时 `scopeContent=false`
  - `empty_pattern`：`pattern` 为空/纯空白
  - `scope_mismatch`：`scope` 非空且未命中 `book`/`origin`
  - `excluded`：`excludeScope` 命中
  - `pattern_too_long`：正则模式长度 > 512
  - `invalid_regex: <编译错误>`：正则无法编译
- 错误（均为 `400 {"error":"..."}`）：
  - `rules 或 rule 至少提供一个`
  - `input 或 url 至少提供一个`
  - `url 必须是 http(s) URL`
  - `预览超时：超过 15 s 预算`
  - `响应超过 5 MiB 预算` / `input 超过 5 MiB 预算`
  - `规则包不能为空` / `invalid replace rule: ...`（预览侧不带“第 N 条”序号）
  - 抓取失败 → `400 internal: ...`
  - 另：`401`；`500`。
- 行为与限制：
  - 预算：输入/响应 ≤ 5 MiB，单次预览 ≤ 15 s。超时在抓取完成后判定（不是强中断）。
  - SSRF 防护：`url` 抓取默认拒绝回环/私网/链路本地地址，并逐跳校验重定向；本地调试可设 `PREVIEW_ALLOW_PRIVATE=true` 放行。
  - 截断：已执行步骤与 `invalid_regex` 步骤的 `output` 2 万字节、`final` 20 万字节，均按 UTF-8 字符边界截断并置对应 `*_truncated`；其余跳过原因不截断。
  - 预览只执行传入规则，**不读取用户库里的 `replace_rules`**，也不写库。
  - 预览不处理图片样式（`image_style` 只在正文接口生效）。

### 8.6 `DELETE /api/replace-rules/delete`

- 鉴权：登录
- 查询参数：`id`（integer，必填）
- 成功 `200`：`{"ok": true}`
- 错误：缺参 → 400 纯文本；`401`；`500`。
- 行为：删除不存在 id 仍 `ok:true`。

### 8.7 规则执行语义（正文输出时）

- 总开关：`user_settings.use_replace_rule`；缺省/值不是 `"0"`/`"false"` 视为开启（[replace.rs](../server/src/routes/replace.rs#L206-L215)）。
- 只取 `is_enabled=1` 的规则，按 `order, id` 升序。
- 标题应用条件：`scope_title == true`；正文应用条件：`scope_content == true`。
- `pattern.trim()` 为空则跳过。
- `scope` 非空时，按行/`;`/`|` 拆分，任意一项满足 `book_name.contains(项)` 或 `source_id.contains(项)` 才执行；`scope` 为空表示全体。
  - 注意是**子串包含**，不是精确匹配；`book_name` 来自 `book_meta`，首次抓正文时可能为空。
- `exclude_scope` 命中任一项则跳过。
- `is_regex=false`：字面量 `String.replace`。
- `is_regex=true`：`pattern.len() > 512` 直接跳过；正则编译失败跳过；不捕获替换结果，`replacement` 支持 `$1` 等正则展开（Rust regex crate）。
- `timeout_millisecond` 是 Legado `ReplaceRule` 兼容字段；Rust `regex` 线性时间无回溯风险，因此执行时**不消费**（仅存库/导入导出）。书源级 HTTP 超时是另一个字段：`BookSource.timeout_millisecond`（见 4.6）。
- 图片样式在所有替换之后统一应用。

---

## 9. 设置

> 数据在用户库 `user_settings`，纯 key/value（字符串存储），按用户隔离。
> 现有 **11 个键**；未存时 GET 返回默认值，不会写库。

### 9.1 `GET /api/settings`

- 鉴权：登录
- 参数：无
- 成功 `200`（所有字段必出现）：

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `use_replace_rule` | boolean | `true` | 是否启用用户替换规则 |
| `builtin_clean` | boolean | `false` | 内置净化开关；开启后与用户替换规则走同一条 `apply_rules` 链路，默认关闭 |
| `image_style` | integer | `0` | 0 原样 / 1 宽度自适应 / 2 居中 / 3 隐藏 |
| `theme` | string | `"light"` | 阅读器主题；设置页与阅读器共用 |
| `font_size` | integer | `18` | 阅读器字号，写回 `user_settings` |
| `line_height` | number | `1.8` | 阅读器行距，写回 `user_settings` |
| `cache_limit_mb` | integer | `50` | 正文缓存上限（MB），实际使用时与全局上限取 min |
| `auto_refresh` | boolean | `true` | 打开书架时自动检查更新（前端 `Bookshelf.svelte` 消费；24h 本机节流，只刷新 `enable_update` 未关闭的书） |
| `read_mode` | string | `"scroll"` | `scroll` / `paged` / `fade`（POST 白名单）；`paged` 无分页数据支撑 |
| `page_width` | integer | `800` | 阅读页宽（CSS px），POST 范围 **320–2000** |
| `font_family` | string | `"serif"` | 字体族，POST 白名单见 9.2 |

示例（伪造值）：

```json
{
  "use_replace_rule": true,
  "builtin_clean": false,
  "image_style": 1,
  "theme": "light",
  "font_size": 18,
  "line_height": 1.8,
  "cache_limit_mb": 50,
  "auto_refresh": true,
  "read_mode": "scroll",
  "page_width": 800,
  "font_family": "serif"
}
```

- 错误：`401`；`500`。
- 行为：
  - 读取 `user_settings` 全部行，缺省键补 `SETTINGS_DEFAULTS`（[replace.rs](../server/src/routes/replace.rs#L40-L50)）。
  - 布尔解析：值为 `"1"` 或 `"true"` 才是 true，其余 false；因此库里存 `"True"` 会被当 false。
  - 数字解析失败回退默认值。
  - `cache_limit_mb` 不在此处钳制；非法/<=0 时在正文缓存逻辑里回退全局值。
  - 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B9，4/4）：缺省返回默认值（`page_width=800`、`font_family=serif`）；写入后读回一致。`builtin_clean` 键为 2026-11-03 新增（设置页开关已接，默认 `false`），未纳入该回归用例。

### 9.2 `POST /api/settings`

- 鉴权：登录
- 请求体：上述字段的**任意子集**，全部可选：

```json
{ "use_replace_rule": false, "image_style": 2, "font_size": 20 }
```

- 成功 `200`：`{"ok": true}`
- 错误：JSON 非法/类型不符 → 400/422 纯文本；`401`；`500`。
- 行为：
  - 只 upsert 请求中出现的键，未传键保持原值（[replace.rs](../server/src/routes/replace.rs#L171-L202)）。
  - 布尔存为 `"1"`/`"0"`，数字 `to_string()`，字符串原样。
  - **写入前有全字段校验**（任一非法即 `400`，整次请求不落库）：
    - `theme`：`light` / `dark` / `sepia`。
    - `font_size`：12–30。
    - `line_height`：有限数且 1.2–2.6。
    - `cache_limit_mb`：10–1000。
    - `image_style`：0–3。
    - `read_mode`：`scroll` / `paged` / `fade`。
    - `page_width`：320–2000。
    - `font_family`：`serif` / `sans` / `sans-serif` / `kai` / `kaiti` / `song` / `songti` / `hei` / `heiti` / `mono` / `system`（共 11 个值）。
  - 错误文案固定，例如 `theme 必须是 light/dark/sepia`、`font_size 必须在 12–30 之间`、`page_width 必须在 320–2000 之间`、`font_family 必须是 serif/sans/kai/song/hei/mono/system 等枚举值`。
  - `use_replace_rule`/`builtin_clean`/`auto_refresh` 为布尔，无额外校验。
  - GET（9.1）**不再校验**库内旧值：若历史数据写了非法 `font_family`，会原样返回。
  - `theme`/`font_size`/`line_height`/`page_width`/`font_family` 已由阅读器消费；`auto_refresh` 已由书架页消费：打开书架时若为 true 且距上次自动刷新超过 24h，则后台调用 `/api/book/toc/batch` 刷新未停更的书（本机 localStorage 节流，`Bookshelf.svelte`）。
  - 验证状态：**已验证**（回归报告 `report-20261103-1108.md` B9，4/4）：默认 `page_width=800`/`font_family=serif`；写入 `640`/`mono` 读回一致；`300`/`2100`/`comic` → 400 且不覆盖已存值；`320`/`2000` 与 `songti`/`kaiti`/`system` 均接受。

---

## 10. 阅读统计

> 已实现（`stats.rs` 是未跟踪的新文件，路由已注册）。数据在用户库 `reading_stats` / `book_stats` / `reading_streak`。

### 10.1 `POST /api/stats/reading`

- 鉴权：登录
- 请求体：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `book_id` | string | 是 | 书籍 URL |
| `name` | string | 否 | 书名；缺省时读 `book_meta.name` |
| `seconds` | integer | 否 | 本次阅读秒数；服务端钳制 0..86400，累加到当天 |
| `chars` | integer | 否 | 本次阅读字数；钳制 0..10,000,000，累加到当天 |
| `chapter_index` | integer | 否 | 当前章节下标；用于更新 `read_chapters = index+1` |
| `total_chapters` | integer | 否 | >0 时覆盖总章节数，否则读 `book_meta` |

- 成功 `200`：

```json
{ "ok": true, "day": "2026-11-02", "streak_days": 3 }
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 true |
| `day` | string | 否 | 服务器本地日期 `YYYY-MM-DD` |
| `streak_days` | integer | 否 | 当天更新后的连续天数（若同日重复上报，返回原 current） |

- 错误：缺 `book_id` / JSON 非法 → 400/422 纯文本；`401`；`500`。
- 行为：
  - `reading_stats` 按 `day` upsert，`seconds`/`chars` **累加**（不是覆盖）。
  - `book_stats` 按 `book_id` upsert，`read_chapters` 取 `MAX(旧, chapter_index+1)`；`chapter_index` 缺省时 `read=0`，不会降低旧值。
  - 连续天数：`last_day` 不等于今天时，若等于昨天则 `+1`，否则重置为 1；同一天重复上报不增加；`longest_days = max(longest, current)`（[stats.rs](../server/src/routes/stats.rs#L114-L139)）。
  - **不更新** `reading_progress`；进度由 7.10 维护。
  - 前端应在阅读页按心跳/离开页时上报增量，避免一次传超大值（会被 86400 钳制）。

### 10.2 `GET /api/stats/overview`

- 鉴权：登录
- 验证状态：**仅单测**（“传/不传 `days` 的 books 过滤”两条 SQL 有单测）；**该接口无 HTTP 冒烟证据**。
- 查询参数：

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `days` | integer | 否 | 7 | 聚合天数，clamp 1..365。**不传**：`books` 返回全量中最近 50 本（按 `last_read_at DESC`，不按日期过滤）；**传了**：`books` 额外按 `substr(last_read_at,1,10) >= since` 过滤后再取 50 |

- 成功 `200`：

```json
{
  "days": 7,
  "daily": [
    { "day": "2026-10-27", "seconds": 600, "chars": 12000 },
    { "day": "2026-11-02", "seconds": 1260, "chars": 24000 }
  ],
  "weekly": [
    { "week": "2026-10-26", "seconds": 600, "chars": 12000 },
    { "week": "2026-11-02", "seconds": 1260, "chars": 24000 }
  ],
  "total_seconds": 1860,
  "total_chars": 36000,
  "today_seconds": 1260,
  "streak_days": 3,
  "longest_streak_days": 9,
  "books": [
    {
      "book_id": "https://example.com/book/123",
      "name": "示例小说",
      "total_chapters": 300,
      "read_chapters": 13,
      "completion": 4.33,
      "last_read_at": "2026-11-02 11:30:00"
    }
  ]
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `days` | integer | 否 | 回显钳制后的天数 |
| `daily` | object[] | 否 | 窗口内**有数据**的日期，按 `day` 升序；格式 `{day,seconds,chars}` |
| `weekly` | object[] | 否 | 按周一聚合，`{week(周一日期),seconds,chars}` |
| `total_seconds` | integer | 否 | 窗口内秒数合计 |
| `total_chars` | integer | 否 | 窗口内字数合计 |
| `today_seconds` | integer | 否 | 今日秒数；无数据 0 |
| `streak_days` | integer | 否 | 当前连续天数；`last_day` 非今天/昨天则 0 |
| `longest_streak_days` | integer | 否 | 历史最长 |
| `books` | object[] | 否 | 同 7.5 的 `books`，最多 50；**传 `days` 时仅含该窗口内有阅读记录的书**，不传时为全量最近 50 本 |

- 错误：`401`；`500`。
- 行为：
  - 窗口起点 = 今天本地日期 - (days-1)；只查 `reading_stats.day >= 起点`。
  - `daily` 不补零，前端需自行按 `days` 生成完整日期轴。
  - `weekly` 是把 `daily` 按“周一日期”连续分组；因为 `daily` 已按日期升序，同一周相邻会合并。若窗口跨周且中间缺日，分组仍按实际出现的周一 key 处理，不会补齐空周。
  - 周聚合是“以周一为起点”的近似，不是严格 ISO 周（跨年周可能不同）。
  - `books`：
    - **未传 `days`**（旧客户端）：不按窗口过滤，取全量 `book_stats` 最近 50（保持向后兼容）。
    - **传了 `days`**：只保留 `last_read_at` 日期部分 `>= 窗口起点` 的书，仍按 `last_read_at` 倒序、最多 50。
  - 两个分支都用同一条排序/上限，前端无需区分。

---

## 11. 健康检查/其他

### 11.1 `GET /api/health`

- 鉴权：公开
- 参数：无
- 成功 `200`：纯文本 `ok`（不是 JSON）
- 用途：容器/反向代理探活。

### 11.2 `GET /healthz`

- 鉴权：公开
- 参数：无
- 成功 `200`：

```json
{
  "ok": true,
  "rss_bytes": 52428800,
  "rss_mb": 50.0,
  "open_user_dbs": 2,
  "cache_limit_mb": 50,
  "js": { "enabled": true, "last_ms": 12, "max_ms": 48, "budget_ms": 200 },
  "http": { "concurrency": 3, "timeout_secs": 15, "max_body_mb": 5 }
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ok` | boolean | 否 | 恒 true |
| `rss_bytes` | integer | 是 | Linux `/proc/self/statm`、Windows `GetProcessMemoryInfo`；其他平台 null |
| `rss_mb` | number | 是 | 保留 2 位小数；`rss_bytes` 为 null 时 null |
| `open_user_dbs` | integer | 否 | 当前缓存中的用户连接池数 |
| `cache_limit_mb` | integer | 否 | 全局正文缓存上限（MiB） |
| `js.enabled` | boolean | 否 | `JS_ENABLED` |
| `js.last_ms` | integer | 否 | 最近一次 JS 规则耗时；未执行过 0 |
| `js.max_ms` | integer | 否 | 历史最大耗时 |
| `js.budget_ms` | integer | 否 | `JS_TIMEOUT_MS` |
| `http.concurrency` | integer | 否 | 源级并发槽（`HTTP_CONCURRENCY`，1..16） |
| `http.timeout_secs` | integer | 否 | `HTTP_TIMEOUT_SECS` |
| `http.max_body_mb` | integer | 否 | `HTTP_MAX_BODY_MB` |
| `search.per_source_secs` | integer | 否 | `curated` 单源搜索预算（`SEARCH_TIMEOUT_SECS`，默认 8） |
| `search.per_source_ms` | integer | 否 | 同上，毫秒（= `curated` 的 `deadline_ms` 取值来源） |
| `search.all_per_source_secs` | integer | 否 | `all` 单源搜索预算（`SEARCH_ALL_TIMEOUT_SECS`，默认 8） |
| `search.all_per_source_ms` | integer | 否 | 同上，毫秒（= `all` 的 `deadline_ms` 取值来源） |
| `search.total_curated_secs` | integer | 否 | `scope=curated` 整批预算（`SEARCH_CURATED_BUDGET_SECS`，默认 6） |
| `search.total_all_secs` | integer | 否 | `scope=all` 整批预算（`SEARCH_ALL_BUDGET_SECS`，默认 40） |
| `search.source_slots` | integer | 否 | `curated` 的源级槽（= `HTTP_CONCURRENCY`） |
| `search.all_source_slots` | integer | 否 | `all` 的源级槽（显式设置时为 `min(HTTP_CONCURRENCY,16)`，未设置时 16） |
| `search.comic_slots` / `search.comic_slots_all` | integer | 否 | 漫画源独立小池（小说槽的 1/4，至少 1） |
| `search.source_concurrency_default` | integer | 否 | 书源未声明 `concurrentRate` 时的每源在飞上限（`SOURCE_CONCURRENCY_DEFAULT`，默认 8） |
| `search.source_concurrency_min` / `search.source_concurrency_max` | integer | 否 | 每源在飞上限的夹取范围（1 / 8） |
| `search.source_limit_waits` | integer | 否 | 进程生命周期内「按源限流导致的等待次数」（超限排队才计数，立即拿到许可不计） |
| `search.source_limit_wait_ms` | integer | 否 | 上述等待的累计毫秒 |
| `search.title_filter` | object | 否 | 服务端书名包含过滤的进程内累计计数：`calls`（过滤调用次数）、`raw`（过滤前候选）、`kept`（保留）、`dropped`（书名不含关键词而丢弃）；用于判断过滤是否过狠 |

- 说明：`search` 块与启动日志 `search budgets resolved` 同源，直接反映进程真实生效值；改环境变量后无需再靠日志猜。

- 错误：无（不会 401）。
- 行为：只含进程级指标，不含任何用户数据；`open_user_dbs` 读取全局缓存锁，失败则 0（不会报错）。

### 11.3 `GET /api/healthz`

- 鉴权：**登录**（不在公开白名单）
- 参数：无
- 成功 `200`：`/healthz` 的全部字段，外加：

```json
{
  "ok": true,
  "rss_bytes": 52428800,
  "rss_mb": 50.0,
  "open_user_dbs": 2,
  "cache_limit_mb": 50,
  "js": { "enabled": true, "last_ms": 12, "max_ms": 48, "budget_ms": 200 },
  "http": { "concurrency": 3, "timeout_secs": 15, "max_body_mb": 5 },
  "cache": { "bytes": 10485760, "mb": 10.0, "rows": 42 }
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `cache.bytes` | integer | 否 | 当前用户正文缓存总字节 |
| `cache.mb` | number | 否 | 保留 2 位小数 |
| `cache.rows` | integer | 否 | `book_content` 行数 |

- 错误：`401`；`500`（缓存查询失败被 `unwrap_or(0)` 吞掉，实际不会 500）。
- 行为：与 `/healthz` 共用 `base_metrics`，追加当前用户库缓存统计（[health.rs](../server/src/routes/health.rs#L119-L148)）。

### 11.4 其他

- 静态资源：未匹配 `/api/*` 的 GET 由 `ServeDir` 兜底，目录取 `BORANLIB_WEB_DIR`（兼容旧前缀 `LEGADO_WEB_DIR`；默认 `web/dist`，相对进程工作目录）。`:3000` 现设为绝对路径 `<repo>/server/web/dist`；目录不存在时相关路径 404，不影响 API（[main.rs](../server/src/main.rs#L52-L55)）。**已验证**：`/`、`/assets/*`、`/k/login` 均 200；`/k` 无会话 303 到 `/k/login`，带 `k_session` 200。
- CORS：默认**不添加 CORS 层**（同源部署即可）；需要跨域时设置 `BORANLIB_CORS_ORIGINS`（逗号分隔的精确 `scheme://host[:port]`），拒绝 `*`、路径、查询串与片段。白名单只放行来源，不启用 credentials（[main.rs](../server/src/main.rs#L100-L125)）。
- 没有 `/api/rss`、备份、词典等接口（用户在 2026-11-02 明确不做，见 TODO）。

### 11.5 `GET /api/k/metrics` 与 `GET /api/k/metrics/export`

`/k` 的极简阅读页埋点写入 `DATA_DIR/logs/k-metrics-YYYYMMDD.jsonl`（`DATA_DIR` 取 `BORANLIB_DATA_DIR` / `LEGADO_DATA_DIR`，默认 `data`）。两个接口都**仅管理员**可用（JWT Bearer）。

| 方法 | 路径 | 鉴权 | 参数 | 成功响应 |
|---|---|---|---|---|
| GET | `/api/k/metrics` | JWT + admin | `date?`（`YYYY-MM-DD`，缺省服务器本地当天） | 聚合 JSON（见下） |
| GET | `/api/k/metrics/export` | JWT + admin | `date?` | 该日 JSONL 原文（无文件 404），`application/x-ndjson`，`Content-Disposition` 附件 |

- `date` 只做 `YYYY-MM-DD` 形状校验，非法 `400`；当日文件不存在时 `metrics` 返回 200 空统计，`export` 返回 `404`（附件名为 `k-metrics-YYYY-MM-DD.jsonl`）。
- 聚合返回：按路径的 `count`、`render_ms`（`p50/p95/max`）、`bytes_avg`、`since_prev_ms` 分位、`ua_class` 分布，外加 `slow`（最近 20 条 `render_ms > 1000`）。
- JSONL 每行字段：`ts`、`path`、`status`、`render_ms`、`bytes`、`key`、`book`、`ch`、`p`、`fs`、`vw`、`vh`、`scope`、`tag`、`ua_class`、`ua`、`sid`、`since_prev_ms`。
- 会话只记 cookie 哈希前 8 位；写盘失败仅告警，不影响页面响应；保留最近 7 天、目录超 20 MB 时从最旧文件删。

### 11.6 `/k` HTML 路由与查询参数

共有 7 个路径：`/k`、`/k/toc`、`/k/read`、`/k/search`、`/k/login`（GET/POST）、`/k/logout`、`/k/_selftest`。除登录页外都需要 Cookie `k_session`，未登录 `303` 到 `/k/login`（不使用 Bearer）。阅读进度不再有手动提交入口（原 `POST /k/progress` 已删除）：打开 `/k/read`（含漫画）时服务端渲染路径自动 upsert `reading_progress`/`book_stats`，内容未变则幂等不写。

| 路径 | 必填 | 可选参数 |
|---|---|---|
| `/k`（书架） | — | `fs`、`sp`、`vw`、`vh`、`vmh` |
| `/k/toc` | `book`、`source` | `tp`（页码）、`fs`、`ch`（当前章高亮）、`vw`、`vh`、`vmh` |
| `/k/read` | `book`、`source` | `ch`、`p`（章内页码，1 起）、`fs`、`vw`、`vh`、`vmh` |
| `/k/search` | — | `key`、`fs`、`sp`、`scope`（显式 `all`/`expanded`/`enhanced` 走全量，其余 curated）、`include_cooled`、`vw`、`vh`、`vmh` |

- `fs`：字号档位 1..4；缺参回落 Cookie `k_fs`，再缺省 2。
- `vw` / `vh`：视口宽/高（CSS px）。`vh` **优先读 Cookie `k_vh`**（版本 `k_vv` 与当前 `CALIB_VER` 一致且在 480–2400 内才采用），否则用请求参数；`vw` 用请求参数。缺参回落环境默认 `KINDLE_VW=1072` / `KINDLE_VH=1268`；显式 `vw` 时页面锁 `width={vw}`。
- `vmh`：**试验开关**（400–2400，缺省关闭）。给出时 viewport meta 变成 `width=…,height=<vmh>,initial-scale=1,maximum-scale=1,user-scalable=0`，并写 Cookie `k_vmh`（30 天，后续不带参数也沿用）。**清除开关**：`?vmh=0`（或 `vmh=off`）删除该 Cookie（`max-age=0`），本次不输出 `height=`，页面正常渲染不跳转；解析顺序是「先参数、再决定写/删/沿用」，因此带 `vmh=0` 时不会沿用旧 Cookie；非法值（如 `300`）忽略参数、沿用 Cookie，不写也不删。用于在真机对比「带 height 的 meta」与「不带」两种表现——若老引擎认 `height=`，布局视口会变矮，整页平移与右侧竖条应同时消失。探针 `meta_h`/`vw_meta_height` 回读该 meta 的 `height=` 值（没有则 `-`），用于确认是否生效。
- 视口自校正（服务端 + 两段内联脚本）：①`<head>` 先注入错误兜底（`window.onerror` + `window.__kerr`，用 `<img>` 打点上报 `stage=gerr`），再注入 ES5 标定脚本。v0.3.21 起**不再用 `scrollTo` 标定**（真机 `scrollY`/`scrollTop` 恒 0，标定永不可能成功）：版本一致直接用 `k_vh`；否则取保守值 `F = min(clientHeight−24, outerHeight−8, innerHeight)`（取不到的项忽略；真机 `clientHeight=1268`、`outerHeight=1232` → `F=1224`），写 Cookie `k_vh`（`Path=/`，30 天）、`k_vv`（`CALIB_VER=2`）与 `k_ck=0`/`k_my=-1`（仅作「未实测/失败」标记，页面不依赖）。**服务端渲染直接读 `k_vh`**（`cookie_vh`：版本一致且在 480–2400 时优先于 URL 参数），`location.replace` 只承担「补齐 `vw`/`vh`」的补参职责（最多一次，去环用版本化的 `sessionStorage.k_vhfix{CALIB_VER}`），**不写 `kfix` 闩锁**。
- 文档高锁成 `visibleH − DOC_BOTTOM_MARGIN`（常量 88；`V=1224` → **1136**，约 `outerHeight−96`），底栏等贴底元素按 `body` 的 `absolute;bottom:0` 排布。**硬性不可滚动锁定**：`html,body{position:fixed;top:0;left:0;width:100%;height:1136px;overflow:hidden;overscroll-behavior:none}`（像素高，老引擎不支持 `calc`），顶栏同步用 `position:absolute`（该引擎把 `fixed` 当普通元素），`#k-stage` 的 `bottom` 与 `#k-bar` 的 `top` 都按新文档高重算；`touchmove` 可取消时仍 `preventDefault`，但**已确认真机事件到不了 JS，只作保留手段，不作为主要修复**。
- 探针为**单通道** `<img>`（同 stage 不再 img+XHR 各发一条）：`sync` 延后到首次布局后（避免 `inner_h=0`），`load`、`scroll`、`touchmove`（200ms 节流）、`touchend` 均上报，后三者统一 `stage=move` 并用 `move_src`（`touchmove|scroll|end|settle`）区分；`touchend` 立即发一条 `end`，最后一次 `scroll` 后 800ms 发一条 `settle`。**触摸可达性探测**：`touchstart/touchmove/touchend` 各打一条 `stage=touch`（字段 `tev`、`cancelable`、`touches_len`、`clientY`、`pageY`），**不 preventDefault**，用于判断事件是否到页面、是否可取消。位移：`max_seen` 取 `scrollTop | scrollY | |bar_rect_top 变化|` 的最大值；`bar_top_delta = bar_rect_top − load 时值`（当前唯一能量到 pan 的代理量）。另报 `doc_h`/`doc_minus_outer`/`doc_minus_client`/`outer_minus_doc`/`meta_h`/`vw_meta_height`；`body_scroll_h` 保留但**是引擎假象，不作为溢出证据**。
- `tag` **不是** `/k` 的功能参数：handler 不读取；只有埋点中间件把它当最多 64 字节标签记入 JSONL（用于人工标注来源/设备），可省略。

---

## 12. 接口总表与覆盖统计

### 12.1 覆盖统计

- **实际存在并已注册的 JSON HTTP 接口：65 个**（另有 `/k/*` 8 个 HTML 路由，不计入）
  - 鉴权 3 个（register / login / **password**）
  - 管理员 2 个（**users / users/status**）
  - 书源 14 个（原 9 个 + **health/check、health、health/reset、prune、quarantine**）
  - 搜索与发现 6 个（含两段式 **search/start** 与 **search/result**）
  - 书籍与正文 10 个（info / toc / **toc/batch** / content / **book/search** / **cache-all** / **cache/clear** / switch-source / **export/info** / **export/txt**）
  - 书架/分组/进度/批量 12 个（含 **bookshelf/batch**；`DELETE /api/bookshelf/groups` 与 `/groups/delete` 是两个注册路径、同一 handler）
  - 书签 3 个（**GET/POST `/api/bookmarks`、DELETE `/api/bookmarks/delete`**）
  - 替换规则 6 个（CRUD + 导出/导入/预览）
  - 设置 2 个
  - 统计 2 个
  - 健康检查 3 个（`/api/health`、`/healthz`、`/api/healthz`）
  - K 埋点 2 个（`/api/k/metrics`、`/api/k/metrics/export`，仅管理员）
- 计数口径：按“方法 + 路径”计；`/api/bookshelf` 的 GET/POST/DELETE 算 3 个，`/api/bookmarks` 的 GET/POST 算 2 个。
- **确认口径**：
  - 已在 `:3000` 上对**回归所用二进制（9,224,704 B / mtime 2026-11-03 11:08:53 / sha256 `F24B000C…`）**完成 HTTP 冒烟与回归；证据来源 `report-20261103-1108.md`：旧用例 75/75、新接口 46/46、`/k` 10/10，零失败。
  - 当前 `:3000` 实例运行的是工程侧旧构建名的 `legado-server.exe`（对外发布名 `boranlib.exe`）：**9,315,840 B / mtime 2026-11-03 14:46:58 / sha256 `B498BB42…`**（99 项单测通过）；#55~#56 的复测与 #57~#58 的实测均基于该实例。
  - #40~#43（export/import/preview/check-updates）与 #44~#54 的主要成功/错误路径均已验证；#55~#56（两段式搜索）已于 2026-11-03 14:0x / 15:0x 在 `:3000` 上 HTTP 实测（字段、中间态、分页 clamp、404、`source_ids`、淘汰顺序）；#57~#58（导出）已实测 `info` 与 `txt`（含文件头/占位/x-* 头）。覆盖明细见开头复核表。
  - **#59~#63（健康巡检 / 汇总 / 重置 / prune / quarantine）已在 2026-11-04 于隔离端口 `:3061` 完成 HTTP 实测**（隔离数据目录复制自 `server/data`，未触碰 `:3000/:3500`）；覆盖连续 3 次失败进入冷却、默认搜索跳过与 `include_cooled=1` 放行、`FAIL_ENGINE` 不冷却、导入体检三态、`prune` 预览/确认、`quarantine` 与复用复活、导出排除隔离区。`cargo test` 默认线程 **186 passed / 0 failed**。
  - 仍无 HTTP 证据的关键项：`book/search` 超时 504（实测 200 / 1 ms 未触发）、`toc` 分卷行真实数据渲染、用 `volume:` URL 请求正文、`cache-all` 的 `skipped_budget`、`toc/batch` 并发 3、`admin/status` 最后管理员保护、`preview` 5 MiB / 15 s 预算、`@js` 200ms 超预算、DOM 桥上限、`search` 单源 200 / 总 400 截断、`export` 的 `is_pay=true` 真实样本与 10k+ 章压力。
- **待确认**：`latest_chapter_time`/`start`/`end` 的数值语义、部署时区对 `day` 的影响、SQLite `disk I/O error` 根因、`Book.can_update` 默认 false 是否预期（见 14 节）。

### 12.2 全量清单

| # | 方法 | 路径 | 鉴权 | 管理员 | 主要参数 | 成功响应 |
|---|---|---|---|---|---|---|
| 1 | GET | `/api/health` | 公开 | 否 | — | `text/plain: ok` |
| 2 | GET | `/healthz` | 公开 | 否 | — | 进程指标 JSON |
| 3 | GET | `/api/healthz` | JWT | 否 | — | 进程指标 + 用户缓存 |
| 4 | POST | `/api/auth/register` | 公开 | 否 | `username,password` | `{token}` |
| 5 | POST | `/api/auth/login` | 公开 | 否 | `username,password` | `{token}` |
| 6 | GET | `/api/sources` | JWT | 否 | — | `BookSourceListItem[]` |
| 7 | POST | `/api/sources` | JWT | **是** | `BookSource` | `{ok}` |
| 8 | POST | `/api/sources/import` | JWT | **是** | `BookSource[]`；`probe?,source?,source_kind?,force?,key?` | `{ok,batch_id,source,source_kind,collection_unchanged,count,inserted,added,updated,unchanged,update_rejected,conflict,resurrected,local_only,newly_quarantined,distinct_hosts,probe_summary,probe_results[]}` |
| 9 | POST | `/api/sources/enabled` | JWT | **是** | `url?/urls?,enabled` | `{ok,enabled,updated,missing}` |
| 10 | GET | `/api/sources/export` | JWT | 否 | — | `BookSource[]`（camelCase） |
| 11 | GET | `/api/sources/detail` | JWT | 否 | `url` | `BookSource`（snake_case） |
| 12 | POST | `/api/sources/validate` | JWT | 否 | `source?,check_url?` | `{valid,errors,warnings}` |
| 13 | DELETE | `/api/sources/delete` | JWT | **是** | `url` | `{ok}` |
| 14 | GET | `/api/search` | JWT | 否 | `key,page?,page_size?,include_cooled?` | `SearchBook[]` |
| 15 | GET | `/api/search/aggregate` | JWT | 否 | `key,page?,page_size?,include_cooled?` | 聚合对象（含 `include_cooled/skipped_cooldown/cooldown_total/source_failed/source_skipped`；`errors[].kind` 区分失败类型） |
| 16 | GET | `/api/explore` | JWT | 否 | `url,page?` | `SearchBook[]` |
| 17 | GET | `/api/explore/categories` | JWT | 否 | `source,page?` | `ExploreCategory[]` |
| 18 | GET | `/api/book/info` | JWT | 否 | `source,book_url` | `Book` |
| 19 | GET | `/api/book/toc` | JWT | 否 | `source,book_url` | `BookChapter[]` |
| 20 | GET | `/api/book/content` | JWT | 否 | `source,chapter_url` | `BookContent` |
| 21 | POST | `/api/book/switch-source` | JWT | 否 | `name,author?,exclude_source?` | `SearchBook[]` |
| 22 | GET | `/api/bookshelf` | JWT | 否 | `group?` | `BookshelfItem[]` |
| 23 | POST | `/api/bookshelf` | JWT | 否 | `book_id,source_id?,name?,author?,cover?,group?` | `{ok}` |
| 24 | DELETE | `/api/bookshelf` | JWT | 否 | `book_id` | `{ok}` |
| 25 | POST | `/api/bookshelf/group` | JWT | 否 | `book_id?/book_ids?,group?` | `{ok,updated}` |
| 26 | GET | `/api/bookshelf/stats` | JWT | 否 | — | 书架统计对象 |
| 27 | GET | `/api/bookshelf/groups` | JWT | 否 | — | 分组数组 |
| 28 | POST | `/api/bookshelf/groups` | JWT | 否 | `id?,name?,order?,book_sort?,cover?,show?` | `{ok,id}` |
| 29 | DELETE | `/api/bookshelf/groups` | JWT | 否 | `id` | `{ok}` |
| 30 | DELETE | `/api/bookshelf/groups/delete` | JWT | 否 | `id` | `{ok}` |
| 31 | GET | `/api/bookshelf/progress` | JWT | 否 | `book_id` | 进度对象 |
| 32 | POST | `/api/bookshelf/progress` | JWT | 否 | `book_id,chapter_index?,position?,total_chapters?` | `{ok,chapter_index,position}` |
| 33 | GET | `/api/replace-rules` | JWT | 否 | — | `ReplaceRule[]` |
| 34 | POST | `/api/replace-rules` | JWT | 否 | `ReplaceRule` | `{ok,id}` |
| 35 | DELETE | `/api/replace-rules/delete` | JWT | 否 | `id` | `{ok}` |
| 36 | GET | `/api/settings` | JWT | 否 | — | 设置对象 |
| 37 | POST | `/api/settings` | JWT | 否 | 设置子集 | `{ok}` |
| 38 | POST | `/api/stats/reading` | JWT | 否 | `book_id,...` | `{ok,day,streak_days}` |
| 39 | GET | `/api/stats/overview` | JWT | 否 | `days?` | 统计总览对象 |
| 40 | GET | `/api/replace-rules/export` | JWT | 否 | `id?` | 单对象/裸数组（camelCase，无 id） |
| 41 | POST | `/api/replace-rules/import` | JWT | 否 | 裸数组 / `{name,rules}` / 单条对象 | `{ok,inserted,skipped}` |
| 42 | POST | `/api/replace-rules/preview` | JWT | 否 | `rules?/rule?,input?/url?,headers?,book?,origin?,for_title?` | `{ok,fetched,input_length,final,output_length,final_truncated,steps[]}` |
| 43 | POST | `/api/sources/check-updates` | JWT | 否 | 裸数组 / `{sources:[]}` / 单条对象 | `{ok,checked,errors,updated,new,local_newer,same,has_updates}` |
| 44 | POST | `/api/auth/password` | JWT | 否 | `old_password,new_password`（6–128） | `{ok,relogin_required,previous_token_version}` |
| 45 | GET | `/api/admin/users` | JWT | **是** | — | `{ok,users[]}` |
| 46 | POST | `/api/admin/users/status` | JWT | **是** | `user_id,disabled` | `{ok,user_id,disabled}` |
| 47 | POST | `/api/book/toc/batch` | JWT | 否 | `book_urls`（别名 `book_ids`）`,source?` | `{ok,total,updated,failed,skipped,results[],elapsed_ms}` |
| 48 | GET | `/api/book/search` | JWT | 否 | `book_id,q,limit?` | `{ok,book_id,query,limit,truncated,hits[],total_chapters,cached_chapters,uncached_chapters,complete,note,elapsed_ms}` |
| 49 | POST | `/api/book/cache-all` | JWT | 否 | `book_url`（别名 `book_id`）`,source?,limit?` | 缓存统计对象（`cached/failed/skipped_budget/...`） |
| 50 | POST | `/api/book/cache/clear` | JWT | 否 | `book_id?`（别名 `book_url`） | `{ok,scope,book_id,deleted,bytes_freed}` |
| 51 | GET | `/api/bookmarks` | JWT | 否 | `book_id?` | `Bookmark[]`（裸数组） |
| 52 | POST | `/api/bookmarks` | JWT | 否 | `book_id,chapter_index?,position?,chapter_title?,note?` | `Bookmark` |
| 53 | DELETE | `/api/bookmarks/delete` | JWT | 否 | `id` | `{ok,deleted}` |
| 54 | POST | `/api/bookshelf/batch` | JWT | 否 | `action,book_ids,group?` | `{ok,action,group,updated,failed,results[]}` |
| 55 | GET | `/api/search/start` | JWT | 否 | `key`/`q`（别名）,`source_ids?`,`include_cooled?`,`scope?` | `{job_id,scope,source_total,skipped_cooldown,cooldown_total,include_cooled,deadline_ms,total_budget_ms,phase,partial,scheduled_sources,attempted_sources,pending_sources,source_ids_requested,ignored_source_ids,warning}` |
| 56 | GET | `/api/search/result` | JWT | 否 | `job_id,page?,page_size?` | `{books,errors,page,page_size,total,has_more,done,phase,partial,scheduled_sources,attempted_sources,pending_sources,scope,finished,pending,source_total,source_failed,source_skipped,skipped_cooldown,cooldown_total,deadline_ms,total_budget_ms}` |
| 57 | GET | `/api/book/export/info` | JWT | 否 | `book_url` | `{book_url,name,author,source,total_chapters,cached_chapters,missing_chapters,cached_bytes,complete,export_url}` |
| 58 | GET | `/api/book/export/txt` | JWT | 否 | `book_url` | `text/plain` 附件 + `x-total/cached/missing/written-chapters`、`x-file-bytes` |
| 59 | POST | `/api/sources/health/check` | JWT | 否 | `source_ids?,only_due?,key?` | `{ok,selected,checked,elapsed_ms,budget_secs,cooldown_secs,fail_threshold,results[]}` |
| 60 | GET | `/api/sources/health` | JWT | 否 | `detail?` | `{ok,total,distinct_hosts,active,quarantined,ok,partial,fail,cooling,never,detail,sources[]?}` |
| 61 | POST | `/api/sources/health/reset` | JWT | **是** | `{source_ids?/url?}` | `{ok,cleared,scope}` |
| 62 | POST | `/api/sources/prune` | JWT | **是** | `within_days?,action?,confirm?` | 预览 `{confirm_required,would_affect,affected_hosts,sources[]}`；确认 `{affected,failed,physical_delete:false}` |
| 63 | POST | `/api/sources/quarantine` | JWT | **是** | `{source_ids,reason?}` | `{ok,affected,missing[],reason}` |
| 64 | GET | `/api/k/metrics` | JWT | **是** | `date?` | 按路径聚合的埋点统计 |
| 65 | GET | `/api/k/metrics/export` | JWT | **是** | `date?` | 该日 JSONL 原文 |

---

## 13. 待实现接口（草案；已落地条目标注“已实现”）

> **本节保留的接口均“尚未实现，仅提案”**，前端不可按此直接联调。少数条目已在 2026-11-03 落地（各小节单独标注“已实现”并指向正文），其余仍是提案。
> 说明：用户举例中的“批量启停书源”“书籍分组”“阅读统计”在本次生成时**已经存在**：
> - 批量启停：`POST /api/sources/enabled`（4.4）
> - 分组：`GET/POST /api/bookshelf/groups`、`POST /api/bookshelf/group`（7.4/7.6/7.7）
> - 阅读统计：`POST /api/stats/reading`、`GET /api/stats/overview`（10.1/10.2）
>
> 因此本节只列“确实没有”的接口，并把已有能力的字段缺口写成“扩展现有接口”。每条都给出建议落库与后端改动点，方便直接拆任务。
> 2026-11-03 更新：原 13.12“替换规则导入/导出/试跑预览”已实现并转入正文 8.3~8.5，故从本节删除。

### 13.1 书架：扩展列表字段（扩展现有 `GET /api/bookshelf`，非新路由）

- 状态：**尚未实现，仅提案**
- 现状：`BookshelfItem` 只有 12 个字段（见 7.1），前端无法在书架直接显示最新章节、更新标记、自定义封面/简介、置顶顺序。
- 建议在 `models.rs::BookshelfItem` 与 SQL 中追加（均为 `book_meta` 已有列，除 `sort_order`）：

| 新字段 | 类型 | 可空 | 来源 |
|---|---|---|---|
| `latest_chapter_title` | string | 是 | `book_meta.latest_chapter_title` |
| `latest_chapter_time` | integer | 否 | `book_meta.latest_chapter_time` |
| `can_update` | boolean | 否 | `book_meta.can_update` |
| `custom_cover_url` | string | 是 | `book_meta.custom_cover_url` |
| `custom_intro` | string | 是 | `book_meta.custom_intro` |
| `book_source_type` | integer | 否 | `book_meta.book_source_type` |
| `sort_order` | integer | 否 | 需新增 `bookshelf.sort_order`（新增迁移，当前最新为 user/0004，默认 0） |
| `is_pinned` | boolean | 否 | 可由 `sort_order < 0` 推导，或新增列 |

- SQL 改动：`LEFT JOIN book_meta` 已存在，直接补 `SELECT` 列；排序改为 `is_pinned DESC, sort_order ASC, COALESCE(rp.updated_at, bs.added_at) DESC`。
- 风险：`GET /api/book/info` 会覆盖 `book_meta` 的 `group`/`can_update`，需先修 13.2/14 中的缺陷，否则扩展字段不稳定。

### 13.2 书架：自定义封面 / 标签 / 简介

- 状态：**尚未实现，仅提案**
- 建议路由：`POST /api/bookshelf/book-meta`
- 鉴权：登录
- 请求体：

```json
{
  "book_id": "https://example.com/book/123",
  "custom_cover_url": "https://cdn.example/custom.jpg",
  "custom_tag": "在追",
  "custom_intro": "自己写的简介",
  "group": "默认"
}
```

- 所有字段可选，只更新提供的项（真正 PATCH 语义）。
- 成功：`{"ok": true}`
- 落库：`book_meta.custom_cover_url/custom_tag/custom_intro/group`；`book_id` 必须已存在于 `book_meta` 或 `bookshelf`。
- 前端展示优先级建议：`custom_cover_url || cover_url`、`custom_intro || intro`。
- 后端改动点：`routes/bookshelf.rs` 新增 handler；`models::BookshelfItem.cover` 改为 `COALESCE(custom_cover_url, cover_url, bs.cover)`。

### 13.3 书架：置顶 / 拖动排序

- 状态：**尚未实现，仅提案**
- 建议路由：`PUT /api/bookshelf/order`
- 鉴权：登录
- 请求体：

```json
{
  "mode": "manual",
  "items": [
    { "book_id": "https://example.com/book/1", "sort_order": 0, "is_pinned": true },
    { "book_id": "https://example.com/book/2", "sort_order": 1, "is_pinned": false }
  ]
}
```

- `mode` 枚举：`manual`（按 `items` 顺序）/ `recent`（恢复默认最近阅读排序，清空 `sort_order`）。
- 成功：`{"ok": true, "updated": 2}`
- 落库：新增 `bookshelf.sort_order INTEGER NOT NULL DEFAULT 0`、`is_pinned INTEGER NOT NULL DEFAULT 0`（新增迁移，当前最新为 user/0004）。
- 后端改动点：事务内逐条 `UPDATE`；`GET /api/bookshelf` 排序接入。
- 前端注意：批量拖动只需提交变化项；服务端需保证同一用户内 `sort_order` 唯一或至少稳定。

### 13.4 书架：批量删除

- 状态：**部分已实现（2026-11-03）**：批量删除可直接用 `POST /api/bookshelf/batch` 的 `action="remove"`（见 7.11），逐项返回 `ok/error`，没有独立的 `deleted/missing` 汇总。独立路由 `POST /api/bookshelf/batch-delete` 仍是提案。
- 鉴权：登录
- 请求体：`{ "book_ids": ["https://a", "https://b"] }`（1..200 条）
- 成功：

```json
{ "ok": true, "deleted": 2, "missing": [] }
```

- 行为：事务内删除 `bookshelf` 行；是否级联删 `reading_progress`/`book_stats` 需产品决策（建议 `keep_progress=true` 可选，默认保留，与单删 7.3 一致）。
- 后端改动点：`routes/bookshelf.rs` 新增 handler；`DELETE /api/bookshelf` 可内部复用。

### 13.5 书架：批量更新目录 / 检查更新

- 状态（2026-11-03 更新）：**部分已实现**。批量刷新目录改用 `POST /api/book/toc/batch`（6.3），逐本返回 `updated/failed/skipped`、并发 3、上限 100；但它不回写 `latest_chapter_title`/`can_update` 等书架字段。独立 `POST /api/bookshelf/refresh` 与“比较章节数”的增强仍是提案。
- 建议路由：`POST /api/bookshelf/refresh`（未实现）
- 鉴权：登录
- 请求体：

```json
{ "book_ids": ["https://a", "https://b"], "force": false }
```

- 成功：

```json
{
  "ok": true,
  "results": [
    { "book_id": "https://a", "updated": true, "total_chapters": 310, "latest_chapter_title": "第 310 章", "error": null },
    { "book_id": "https://b", "updated": false, "total_chapters": 0, "latest_chapter_title": null, "error": "timeout" }
  ]
}
```

- 行为：逐本调用现有 `chapter_list`，比较章节数/末章标题写回 `book_meta`；`force=true` 忽略 TOC TTL。
- 约束：小机内存有限，建议服务端限制批量 ≤20 本、串行或并发 ≤3，并返回部分失败。
- 后端改动点：复用 `books.rs::save_chapters` 与 `web_book::chapter_list`；需要按 `bookshelf.book_id` 找到 `source_id`（`bookshelf.source_id` 可能为空，需回退 `book_meta.origin`）。
- 风险：当前 `bookshelf.source_id` 可为空；需先补数据或从 `book_meta.origin` 推断，否则无法抓取（标“待确认”）。

### 13.6 书签 CRUD

- 状态：**已实现（2026-11-03）**：`GET/POST /api/bookmarks`、`DELETE /api/bookmarks/delete?id=`，契约见 6.6。实际实现与提案的差异：
  - 表字段为 `id/book_id/chapter_index/position/chapter_title/note/created_at`，**没有 `chapter_url`**，标题列名为 `chapter_title`。
  - `GET` 返回裸数组，按 `created_at DESC, id DESC`（不是 `chapter_index` 升序）。
  - `POST` 只新增（没有 `id` 更新语义），同书同章**不会 upsert**；返回单个 `Bookmark` 对象，不是 `{ok,id}`。
  - 删除路径是 `/api/bookmarks/delete?id=`，不是 `/api/bookmarks?id=`。
- 仍未实现：`GET /api/bookmarks/export`（Legado 书签 JSON 兼容导出）仍是提案。

### 13.7 书内搜索

- 状态：**已实现（2026-11-03），仅 cached 模式**：`GET /api/book/search?book_id=&q=&limit=`，契约见 6.7。实际参数没有 `source`/`key`/`scope`/`page`；响应是 `hits/complete/note`，不是 `matches/page`；每章只返回首个命中；总查询超 5s 返回 504。
- 仍未实现：`all` 模式（按目录逐章抓取后搜索）。下面是原提案，仅作后续扩展参考：

- 查询参数：`source`、`book_url`、`key`（均必填）、`scope=cached|all`（默认 `cached`）、`limit`（默认 20，最大 50）、`page`（默认 1）
- 响应：

```json
{
  "matches": [
    {
      "chapter_index": 12,
      "chapter_url": "https://example.com/ch/12",
      "chapter_title": "第十二章",
      "snippet": "……包含关键字的片段……",
      "offset": 128
    }
  ],
  "total": 4,
  "scanned_chapters": 30,
  "page": 1,
  "page_size": 20,
  "has_more": false
}
```

- 行为：
  - `cached`：只扫 `book_content` 中该书已缓存章节，零网络请求，快。
  - `all`：按目录逐章抓取（受 `HTTP_CONCURRENCY` 与超时约束），建议服务端限流并设置总预算。
  - 大小写不敏感；`snippet` 由服务端截取（如前后 30 字符）。
- 风险：`all` 在几百章书上可能超过请求超时，建议改为“创建任务 + 轮询”，但当前项目没有任务表，**待产品决策**。
- 后端改动点：`routes/books.rs` 新增；复用 `load_cached_content` 或 `web_book::chapter_content`。

### 13.8 阅读统计：按书时长 / 开关

- 状态：**尚未实现，仅提案**
- 现状：`book_stats` 没有 `seconds`，只能看完成度；`GET /api/stats/overview` 的 `books` 不含时长。
- 建议：
  1. 新增列（新增迁移，当前最新为 user/0004）：

```sql
ALTER TABLE book_stats ADD COLUMN seconds INTEGER NOT NULL DEFAULT 0;
ALTER TABLE book_stats ADD COLUMN chars INTEGER NOT NULL DEFAULT 0;
```

  2. `POST /api/stats/reading` 在累加 `reading_stats` 的同时累加 `book_stats.seconds/chars`（现有 handler 小改）。
  3. 新路由 `GET /api/stats/books?days=30&sort=seconds|last_read`：

```json
{
  "days": 30,
  "total_seconds": 7200,
  "books": [
    { "book_id": "https://a", "name": "示例", "seconds": 3600, "chars": 80000, "last_read_at": "2026-11-02 11:30:00", "completion": 4.33 }
  ]
}
```

  4. 记录开关：复用 `user_settings` 新增键 `enable_reading_stats`（`"1"`/`"0"`），通过现有 `GET/POST /api/settings` 扩展字段 `enable_reading_stats: boolean`；`record` 入口先判断开关。
- 兼容：现有 `book_stats` 行 `seconds/chars` 默认 0，不影响旧统计。

### 13.9 搜索历史（表已存在但无接口）

- 状态：**尚未实现，仅提案**
- 现状：`search_history` 表在 user/0001 已建（`id, query, searched_at`），无任何路由/写入（确认未使用）。
- 建议：
  - `GET /api/search/history?limit=20` → `{items:[{id, query, searched_at}], total}`
  - `POST /api/search/history` → `{query}`；同 query 去重（更新 `searched_at`）或保留最近 N 条。
  - `DELETE /api/search/history?id=...` 与 `DELETE /api/search/history`（清空）。
- 写入时机建议：由前端在发起 `/api/search` 时显式调用，避免服务端搜索接口产生副作用（当前搜索是无状态只读）。
- 后端改动点：`routes/books.rs` 或新 `routes/history.rs`。

### 13.10 缓存管理

- 状态：**部分已实现（2026-11-03）**：
  - 已落地 `POST /api/book/cache/clear`（6.9）：按 `book_id` 或清空当前用户全部正文缓存，返回 `{ok,scope,book_id,deleted,bytes_freed}`；只删正文，不动目录。
  - 已落地 `POST /api/book/cache-all`（6.8）：整本补缓存，带预算与并发。
  - 仍未实现：`GET /api/cache/stats`（独立统计）、删目录缓存（强制重抓，解决 6.2 换源串目录）、二次确认参数。
- 现状（已更新）：缓存用量可从 `/api/healthz` 或 6.8 返回的 `budget_bytes/used_bytes_*` 查看；清理走 6.9，不需要 `confirm`。
- 后端改动点：仍是 `routes/books.rs`；只操作当前用户库。

### 13.11 账号与用户管理

- 状态：**部分已实现（2026-11-03）**：
  - 已落地 `POST /api/auth/password`（3.3）：6–128 字符、校验旧密码；成功后 `token_version+1`，**旧 JWT 立即 401**，不是“旧 token 仍有效”。
  - 已落地 `GET /api/admin/users`（3.4）、`POST /api/admin/users/status`（3.5）：启用/禁用，禁用会递增 `token_version`；不能禁自己或最后一个启用管理员。
  - 仍未实现：
    - `GET /api/auth/me`（登录）→ `{id, username, role}`；token 里已有，但前端每次都解码不便。
    - `POST /api/admin/users/{id}/role`（admin）→ `{role:"user"|"admin"}`；禁止把最后一个 admin 降级。
    - `DELETE /api/admin/users/{id}`（admin）→ 删除账户；是否删 `users/{id}.db` 需产品决策（建议保留备份）。
- 注意：这些接口涉及 `shared.db.users` 与用户库文件，需事务/文件操作顺序；当前没有审计日志。
- 相关缺陷：`ensure_admin` 已在源码中启用（书源写接口），FEATURE_MATRIX 的旧描述“全仓无调用”已过时。

---

## 14. 待确认清单

以下条目源码无法给出确定答案，或依赖部署/产品决策，前端实现前应先确认。

| # | 待确认项 | 现状/依据 | 建议 |
|---|---|---|---|
| 1 | `Book.latest_chapter_time` 的单位 | 字段为 `i64`，解析管线从不填充，恒 0（[entities.rs](../server/src/entities.rs#L255-L258)） | 若后续接入，明确用 Unix 毫秒（Legado 习惯）还是秒；前端暂按 0 处理 |
| 2 | `BookChapter.start/end` 语义 | 建表默认 0，解析不填（[user/0002_extend.sql](../server/migrations/user/0002_extend.sql#L43-L57)） | 确认是否用于本地书字符偏移；Web 书源暂恒 0 |
| 3 | `day` 与 UTC 时间戳的时区一致性 | `reading_stats.day` 用服务器本地日期，`updated_at` 用 UTC（[stats.rs](../server/src/routes/stats.rs#L44-L50)） | 跨时区部署时明确“统计日”以服务器时区为准；或统一改 UTC |
| 4 | ~~`position > 1` 的归一化~~ **已修复（2026-11-03 复核）** | `POST /api/bookshelf/progress` 只接受 0..=1 的有限值，越界直接 400（[bookshelf.rs](../server/src/routes/bookshelf.rs#L375-L384)）；读取时 `progress_percent` 再 clamp | 前端无需再做 `/100` 归一化；历史数据越界时读取仍会 clamp |
| 5 | `Book.can_update` 被写为 false 是否符合预期 | `save_book_meta` 每次写 `b.can_update`（默认 false），会覆盖建表默认 1（[books.rs](../server/src/routes/books.rs#L333-L383)） | 确认更新检查逻辑；否则书架“可更新”字段永远 false |
| 6 | `GET /api/sources/detail` 返回 snake_case 是否有意 | export/import 是 camelCase，detail 是 snake_case（[sources.rs](../server/src/routes/sources.rs#L237-L243)） | 若前端表单复用它，建议统一为 camelCase 或同时提供 `format=camel` |
| 7 | `GET /api/sources/export` 顺序是否需稳定 | 无 `ORDER BY`（[sources.rs](../server/src/routes/sources.rs#L48-L57)） | 若需可复现导出，补 `ORDER BY weight DESC, custom_order ASC, name` |
| 8 | ~~`GET /api/search` 双重分页~~ **已修复（2026-11-02 20:0x 复核）** | 已改为与 aggregate 共用 `aggregated()`：各源只取第 1 页 + 本地切片（2.3） | 保留两个入口；若要全量翻页需另加分页抓取 |
| 9 | `GET /api/explore` 的 A/B 两种模式是否应拆分 | 传分类 URL 会跨源查询，而非只查该源（5.3） | 建议新增 `source` 参数，避免前端误以为只查单源 |
| 10 | 正文缓存按 `chapter_url`、目录缓存按 `book_url` 是否够用 | 换源/同 URL 会串数据（6.2/6.4） | 缓存键加 `origin`，或换源时清理旧缓存 |
| 11 | 首次启动 SQLite `disk I/O error (4618)` | 环境曾有报告，源码无对应处理；清理 WAL/SHM 后恢复，根因未定位（见 TODO） | 复现时保留 `shared.db-wal/-shm` 与启动日志；确认文件锁/磁盘写满/杀软扫描 |
| 12 | `GET /api/bookshelf` 的 `book_id` 参数是否为遗留 | handler 显式忽略（[bookshelf.rs](../server/src/routes/bookshelf.rs#L82-L83)） | 若要单书查询，改为 `WHERE book_id=?`；否则从契约移除 |
| 13 | `timeout_millisecond` 是否应生效 | 字段存了但 `apply_rules` 不用；Rust regex 无内置超时（[replace.rs](../server/src/routes/replace.rs#L294-L333)） | 确认改为模式长度限制 + 线程超时，或从契约移除 |
| 14 | 静态资源 fallback 对所有 GET 生效 | `ServeDir(BORANLIB_WEB_DIR)` 未限制路径（默认 `web/dist`，`:3000` 为 `<repo>/server/web/dist`；[main.rs](../server/src/main.rs#L52-L55)） | 生产建议只对非 `/api` 路径启用，并加 SPA fallback；部署时确保 `BORANLIB_WEB_DIR` 指向实际 `dist` |
| 15 | `BookSource` 未知字段的 `extra` 是否会污染导出 | `#[serde(flatten)] extra` 原样保存并转 camel（[entities.rs](../server/src/entities.rs#L70-L72)） | 验证一轮 Legado 导入→导出无字段丢失/重复 |

---

## 15. 附录：错误状态码与前端处理建议

### 15.1 两类错误体

1. **业务错误（统一 JSON）**：`{"error":"..."}`，由 `AppError` 产生（400/401/403/404/500/504）。`504` 目前只在 `GET /api/book/search` 超过 5 秒预算时出现。
2. **框架提取错误（纯文本）**：Axum 的 `Query`/`Json` 提取失败，响应不是 `{"error":...}`：
   - 缺必需 query 参数：`400`，body 形如 `Failed to deserialize query string: missing field \`key\``。
   - 缺 `Content-Type: application/json` 或 body 为空：`415`/`422`，body 形如 `Expected request with \`Content-Type: application/json\`` / `Failed to deserialize the JSON body...`。
   - 路径方法不匹配：`405`（Axum 默认，可能为空 body）。
3. 前端处理建议：
   - 先按 `res.ok` 判断，再尝试 `JSON.parse`；失败则用文本作为错误文案（现有 `api.js` 已这样做）。
   - `401`：清除 token 并跳登录；`403`：提示“需要管理员”；`404`：提示“书源不存在/资源不存在”；`400` 且文案以 `internal:` 开头：提示“书源站点抓取失败”，可换源重试。
   - 不要假设错误总是 JSON。

### 15.2 成功状态码

- 全部成功响应均为 `200`；源码中没有 `201 Created`/`204 No Content`。
- `GET /api/health` 成功是 `text/plain`，其余成功均为 JSON。

### 15.3 超时与重试建议（前端侧）

- 搜索/发现（多源并发）可能耗时较长；需要“先到先渲染”时用两段式 `GET /api/search/start` + `GET /api/search/result`（5.5/5.6），单次 result 请求很短，轮询即可；需要一次性完整结果时可用 `/api/search/aggregate`（60s 缓存可降低重试成本）。
- 详情/目录/正文抓取受服务端 `BORANLIB_HTTP_TIMEOUT_SECS`（当前实际读取 `HTTP_TIMEOUT_SECS`，默认 15s）限制；失败返回 400，不建议自动无限重试。
- 进度上报/统计上报是轻量写库，可失败后下次心跳重试；`position` 与 `chapter_index` 幂等（upsert）。
- `GET /api/book/search` 有 5s 总预算，超时返回 `504`；不要对同一关键词自动重试，应收窄关键词，或先 `POST /api/book/cache-all` 减少未缓存章节。

### 15.4 环境变量索引（影响接口行为）

> 命名口径：统一使用 `BORANLIB_*`，兼容旧 `LEGADO_*` 前缀（新名优先；空串按未设置处理）。程序**不读取未加前缀的旧名**（`HOST`、`PORT`、`WEB_DIR` 等单独设置均无效）。下表“兼容旧名”为可用的旧前缀写法。

| 变量（新名） | 兼容旧名 | 默认 | 影响 |
|---|---|---|---|
| `BORANLIB_HOST` / `BORANLIB_PORT` | `LEGADO_HOST` / `LEGADO_PORT` | `0.0.0.0` / **`8080`**（二进制内置缺省） | 监听地址；直接运行二进制时为 8080。Windows 成品包由 `start.ps1` 注入 **3000**（实测 `0.0.0.0:3000`） |
| `BORANLIB_DATA_DIR` | `LEGADO_DATA_DIR` | `data` | `shared.db` 与 `users/{uid}.db` 位置 |
| `BORANLIB_WEB_DIR` | `LEGADO_WEB_DIR` | `web/dist`（相对工作目录） | 静态资源目录；`:3000` 实际设为 `<repo>/server/web/dist`。`/`、`/assets/*`、`/k/*` 由它提供 |
| `BORANLIB_ENV` | `LEGADO_ENV` | 未设置时：debug=开发、release=生产 | `production`/`prod`/`release` 视为生产；生产环境会校验 JWT secret 与首次管理员口令 |
| `BORANLIB_JWT_SECRET` | `LEGADO_JWT_SECRET` | `<寮€鍙戝崰浣嶅瘑閽?` | token 签名；**生产环境必须设置且至少 32 字节**，否则拒绝启动 |
| `BORANLIB_CORS_ORIGINS` | `LEGADO_CORS_ORIGINS` | 空（不添加 CORS 层） | 逗号分隔的精确来源白名单，如 `https://reader.example,http://127.0.0.1:5173`；拒绝 `*` |
| `BORANLIB_CACHE_LIMIT_MB` | `LEGADO_CACHE_LIMIT_MB` | `50`（4..512） | 全局正文缓存上限 |
| `BORANLIB_HTTP_CONCURRENCY` | `LEGADO_HTTP_CONCURRENCY` | `16`（1..16） | **出站 HTTP 请求并发**（进程级全局信号量），与「每源在飞上限」叠加：单源先受 `concurrentRate`（默认 8）约束，再参与全局 16 槽竞争。默认 16：2026-11-04 扫参照（同预算 10s、单源 8s）下 8→16 命中源 12.5%→15.8%（+3.3pt），首屏不变；显式设置时 `scope=all` 也取该值（上限 16） |
| `BORANLIB_SOURCE_CONCURRENCY_DEFAULT` | `LEGADO_SOURCE_CONCURRENCY_DEFAULT` | `8`（1..8） | **每源在飞请求上限的默认值**，仅在书源未声明 `concurrentRate` 时使用；`0`/非法回退 8，超过 8 夹回。默认 8 与旧行为等价；收紧到 2/4 只在同源图片突发时增加排队、RSS 无显著收益（2026-11-05 对照），按设备需要自行下调。运行中改它需重启；单源覆盖见书源字段 `concurrentRate` |
| `BORANLIB_SEARCH_TIMEOUT_SECS` | `LEGADO_SEARCH_TIMEOUT_SECS` | `8`（5..30） | **curated 单源**搜索预算；影响 `/api/search`、`/api/search/aggregate`、`/api/search/start`、`/api/search/result` 的 `deadline_ms`。只设它、不设任何整批变量时整批预算与它同值（兼容旧部署） |
| `BORANLIB_SEARCH_ALL_TIMEOUT_SECS` | `LEGADO_SEARCH_ALL_TIMEOUT_SECS` | `8`（5..30） | **all 单源**搜索预算，默认与 `curated` 相同。实测（149 源、16 槽、4 核）：5s 会把「慢但能用」的源全部截断，承载结果的源经常拿不到命中；8s 才能让它们跑完（8s/40s 连续 4 次均命中，5s/25s 只有 1/4）。覆盖不足靠整批预算解决，不要压单源预算。未设置时沿用 `SEARCH_TIMEOUT_SECS` |
| `BORANLIB_SEARCH_CURATED_BUDGET_SECS` | `LEGADO_SEARCH_CURATED_BUDGET_SECS` | `6`（2..60） | **curated 整批**搜索总预算；优先级高于 `SEARCH_TOTAL_BUDGET_SECS` |
| `BORANLIB_SEARCH_ALL_BUDGET_SECS` | `LEGADO_SEARCH_ALL_BUDGET_SECS` | `40`（2..60） | **all 整批**搜索总预算；优先级高于 `SEARCH_TOTAL_BUDGET_SECS`。这是**上限**不是固定等待：源少时任务提前结束 |
| `BORANLIB_SEARCH_TOTAL_BUDGET_SECS` | `LEGADO_SEARCH_TOTAL_BUDGET_SECS` | 按 scope（curated `6` / all `40`，2..60） | **整批**搜索总预算的统一覆盖值（两 scope 同值）；显式按 scope 的变量优先。启动日志打印 `per_source_secs` / `all_per_source_secs` / `total_curated_secs` / `total_all_secs`，`/healthz` 的 `search` 块同源可查 |
| `BORANLIB_HTTP_TIMEOUT_SECS` | `LEGADO_HTTP_TIMEOUT_SECS` | `15`（3..60） | 单次抓取超时 |
| `BORANLIB_HTTP_MAX_BODY_MB` | `LEGADO_HTTP_MAX_BODY_MB` | `5`（1..32） | 响应体截断上限 |
| `BORANLIB_JS_ENABLED` | `LEGADO_JS_ENABLED` | `true` | 书源 JS 规则开关 |
| `BORANLIB_JS_TIMEOUT_MS` | `LEGADO_JS_TIMEOUT_MS` | `3000`（20..5000） | JS 规则时间预算；脚本执行完后按墙钟耗时判定，超过则规则失败（非硬中断）。200ms 会把慢设备上的漫画图片规则（约 1.4s）误判超时 |
| `BORANLIB_TOC_TTL_SECS` | `LEGADO_TOC_TTL_SECS` | `21600`（60..604800） | 目录缓存 TTL |
| `BORANLIB_CONTENT_TTL_SECS` | `LEGADO_CONTENT_TTL_SECS` | `604800`（60..2592000） | 正文缓存 TTL |
| `BORANLIB_DB_MAX_OPEN_USERS` | `LEGADO_DB_MAX_OPEN_USERS` | `8`（1..32） | 同时打开的用户库连接池上限 |
| `BORANLIB_ADMIN_USER` / `BORANLIB_ADMIN_PASS` | `LEGADO_ADMIN_USER` / `LEGADO_ADMIN_PASS` | `admin` / `<寮€鍙戦粯璁ゅ彛浠?`（仅开发） | 首次启动管理员；**生产环境必须显式设置 `ADMIN_PASS`（≥8 字节且非 <寮€鍙戦粯璁ゅ彛浠?），否则拒绝启动** |
| `BORANLIB_RUST_LOG` | `LEGADO_RUST_LOG` | `info` | 日志级别 |

依据：[config.rs](../server/src/config.rs#L60-L75)、[main.rs](../server/src/main.rs#L52-L55)、[req.rs](../server/src/req.rs#L64-L67)、[auth.rs](../server/src/auth.rs#L225-L243)。

---

*文档结束。任何接口变更请同步更新本文件；未实现内容统一放在第 13 节，禁止在正文中混入“计划中的字段”。*

