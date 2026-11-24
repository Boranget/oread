# Boranlib

个人自用的书源阅读服务：Rust 单二进制 + SQLite + Svelte 前端，面向长期维护与墨水屏（KPW3）
离线阅读。不做书源平台、不做本地书籍导入，只把「书源抓取 → 馆藏 → 阅读 → 导出」这一条链路做稳。

## 能做什么

- **书源**：兼容 Legado 书源 JSON，支持搜索、发现、详情、目录、正文、漫画图片与 JS 规则。
- **馆藏**：一本书只存一份内容，直接落成解压的 EPUB / CBZ 目录（`目录即书`）；导出原样打包，
  不重新生成章节。
- **阅读**：PC / 移动端共用同一个 Svelte 阅读器；`/k` 是独立的服务端渲染阅读页，
  图片在读取时转灰度/限宽，库内始终保留原图。
- **正文模型**：标题 / 段落 / 图片语义块，Web 与 `/k` 渲染同一份数据，不注入书源 HTML。
- **元数据**：书名、作者、系列、标签、封面、简介；书源详情页可自动映射系列与标签，
  管理员可改正文段落、元数据、封面与漫画单页。
- **多用户**：书目与正文全局共享；每个账号只保存自己的书架、分组、进度、书签与设置。

明确不做：音频/视频/RSS、词典翻译、WebView 书源、人机验证绕过、本地书籍导入、旧数据迁移。

## 快速开始（Windows 本机）

```powershell
# 1. 构建（前端 + Rust dev 二进制）
.\scripts\build-dev.ps1

# 2. 运行；首次启动会按 BORANLIB_ADMIN_PASS 创建管理员
$env:BORANLIB_ADMIN_PASS = '换成你自己的密码'
$env:BORANLIB_DATA_DIR  = 'D:\boranlib-data'
& 'D:\tools\cache\target-dev\debug\boranlib.exe'   # 脚本默认 dev target，可用 OREAD_TARGET_DEV 覆盖
```

打开 `http://127.0.0.1:8080`，用管理员登录后导入书源 JSON，再搜索、入馆、阅读。
生产环境至少显式设置 `BORANLIB_ADMIN_PASS` 与 `BORANLIB_JWT_SECRET`。

只跑后端（前端用已有 `server/web/dist`）：

```powershell
cd server
cargo run
```

## 数据目录

`BORANLIB_DATA_DIR`（默认 `./data`）下：

| 路径 | 内容 |
|---|---|
| `shared.db` | 账号、书源、搜索任务、书源健康与导入账本 |
| `library.db` | 馆藏目录层：书目、TOC 索引、标签/标识、采购单 |
| `library/index.db` | 派生索引：馆藏列表排序/搜索，可删除重建 |
| `library/collection/<hash>/` | 馆藏书目录，每本一个解压的 EPUB/CBZ（正文与图片的唯一权威副本） |
| `library/temp/<hash>/` | 临时书目录，按 TTL 回收 |
| `library/trash/` | 删除/移出后的目录，按保留策略清理 |
| `users/<uid>.db` | 个人书架、进度、书签、设置、替换规则、书源登录态 |

`data/` 不做旧版本兼容迁移：升级或换设备时直接删除后重新初始化。

## 文档

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)：模块、数据流、目录结构与设计取舍
- [docs/API.md](docs/API.md)：当前 HTTP 接口契约
- [docs/BUILD.md](docs/BUILD.md)：本机构建、musl 交叉编译、工具链
- [docs/DEPLOY.md](docs/DEPLOY.md)：OpenStick 一键部署与回滚
- [docs/KPW3.md](docs/KPW3.md)、[docs/EINK_STYLE.md](docs/EINK_STYLE.md)：墨水屏硬约束与样式
- [docs/TODO.md](docs/TODO.md)：当前待办、已定决策与已知缺口
- [CHANGELOG.md](CHANGELOG.md)：用户可感知的变化

## 开发

```powershell
cd server
cargo test                 # Rust 单测 + 路由冒烟
cd web
npm ci
npm run check              # svelte-check
npm test                   # 前端纯逻辑单测
npm run build              # 产物写入 server/web/dist
```

仓库根 `cargo` 的工作目录是 `server/`（workspace 根），包名 `boranlib`，
成员 crate：`crates/ebook`（EPUB/CBZ 读写）、`crates/rule-core`（书源规则与实体）。
