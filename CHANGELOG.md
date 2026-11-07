# Changelog

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；发布版本由 git tag `v*` 决定。
每次发版把 `[Unreleased]` 改成对应版本并补日期，条目只写用户可感知的变化与迁移注意事项。

## [Unreleased]

### Added

- `/healthz` 增加正文缓存命中率、落库与淘汰计数；`/healthz/user` 增加按书占用前 5 名。
- 新增 `docs/ARCHITECTURE.md`（分层、请求链路、缓存与 `/k` 边界）。

### Changed

- 阅读器首次打开时才加载（路由级懒加载），首屏 JS 预算实测 84.0 kB → 47.0 kB（gzip）。
- `api.js` 的错误文案与查询串拼装下沉到 `apiErrors.js` / `apiUrl.js`，行为不变。
- 发布构建脚本默认带 `--locked`，与 CI 一致。

### Fixed

- 文档纠偏：`/k` 由“零 JS、纯 SSR”改为“无业务 JS，仅按需注入诊断脚本”。

## 发布记录

历史版本（v0.3.20 及更早）未在本文件逐条整理，以 git tag 与 GitHub Release 为准。
