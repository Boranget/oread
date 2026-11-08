# Changelog

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；发布版本由 git tag `v*` 决定。
每次发版把 `[Unreleased]` 改成对应版本并补日期，条目只写用户可感知的变化与迁移注意事项。

## [Unreleased]

### Changed

- 搜索结果的「书名包含关键词」过滤统一放到服务端：网页端、Kindle `/k` 精简页与
  API（legacy、两段式、聚合）在返回前按同一规则丢弃书名不含关键词的条目。
  只比对书名，不做繁简转换；过滤在去重排序之后执行，不影响打分、精选与源健康。
  响应新增 `title_filter` 计数，`/healthz` 暴露进程内累计值。

## [1.0.0] - 2026-11-07

首个稳定版。自 v0.3.24 起完成 Phase 0–5 重构（基线与护栏、安全修复、测试与 CI、
巨型文件拆分、性能与资源、文档与流程），对外行为以修复和收紧为主。

### Added

- 基线与护栏：`scripts/baseline.mjs` 指标采集、`scripts/golden.mjs` 只读接口快照、
  `scripts/clean-dev.ps1` 产物清理；CI 分层为 rust（fmt/clippy/test）、前端、Kindle 样式门禁。
- 路由级冒烟测试与规则引擎 13 组公开 API fixture；`eink-lint` 覆盖 `kindle` 子模块。
- `/healthz` 增加正文缓存命中率、落库与淘汰计数；`/healthz/user` 增加按书占用前 5 名。
- 新增 `docs/ARCHITECTURE.md`（分层、请求链路、缓存与 `/k` 边界）。

### Changed

- **安全默认值收紧**：生产环境弱 JWT secret / 默认口令拒绝启动；CORS 改为白名单。
- 正文缓存按 `(url, origin)` 分区，保存书籍时保护用户字段；启动增加可写探测，
  迁移遇 IOERR/BUSY 退避重试并给出可读错误。
- 后端巨型模块拆分（kindle、books、sources、image、bookshelf、db-guard、
  rule-engine、req、web_book、source_bench、replace、source-health、k-metrics），
  公开 API 不变，单文件回到可维护规模。
- 前端 Reader 拆为顶栏、正文区、目录/搜索/书签/换源/跳转/设置等组件，
 自动滚动、进度、手势与图片状态机下沉为独立模块；Discover / Sources 同步拆分。
- 阅读器改为路由级懒加载，首屏 JS 84.0 kB → 47.0 kB（gzip）。
- `/k` 响应启用 gzip（只动响应编码层，版面几何与分页不变）；搜索按源健康度排序，
  慢源后置；同章正文并发请求 single-flight 合并抓取。
- 版本号改为单源：`Cargo.toml`、`Cargo.lock`、`server/web/package.json` 与 tag 全部对齐。

### Fixed

- 文档纠偏：`/k` 由“零 JS、纯 SSR”改为“无业务 JS，仅按需注入诊断脚本”。

### 升级注意

- 版本号由内部 `0.1.0` 对齐为 `1.0.0`；数据库迁移随启动自动执行。
- 若仍使用默认口令或过短 JWT secret 且 `ENV=production`，进程会拒绝启动，请先改配置。

## 发布记录

历史版本（v0.3.20 及更早）未在本文件逐条整理，以 git tag 与 GitHub Release 为准。
