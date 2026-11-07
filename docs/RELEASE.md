# 发版流程

## 版本号

- **发布版本以 git tag `v*` 为准**（CI 用 `github.ref_name` 生成 Release 标题）。
- `server/Cargo.toml` 与 `server/web/package.json` 目前都是内部版本 `0.1.0`，与 tag 不一致；这一点**尚未拍板**：
  - 方案 A（当前事实）：tag 即发布版本，Cargo/npm 版本只作内部标识，不对外暴露；
  - 方案 B：每次发版把 `Cargo.toml` / `package.json` 改成与 tag 相同，并在构建时注入版本号。
  未拍板前不要单方面改版本号，避免 CI 产物与 tag 语义漂移。
- 工具链由根目录 `rust-toolchain.toml` 固定；CI 与本地 release 构建都带 `--locked`。

## 发版清单

1. 确认 `main` 上四道门禁全绿：
   - `cargo fmt --all -- --check`
   - `cargo clippy --all-targets --locked -- -D warnings`
   - `cargo test --locked`
   - 前端 `npm test` / `npm run build` / `npm run check`
   - `node scripts/eink-lint.mjs`、`node scripts/check-budget.mjs`、`D:\tools\golden-check.ps1`
2. 更新 `CHANGELOG.md`：把 `[Unreleased]` 改成 `## [vX.Y.Z] - YYYY-MM-DD`，只留用户可感知的变化与升级注意事项。
3. 提交（`docs: 发布 vX.Y.Z`）后打 tag：`git tag vX.Y.Z`，确认 tag 与提交对应。
4. 推送：`git push origin main --tags`（由人执行；不要强推、不要重写历史）。
5. CI 在 tag 上构建三平台产物并生成 `SHA256SUMS.txt`；`release` job 用本文件的模板拼 Release notes。
6. 发版后核对：下载产物、`sha256sum -c SHA256SUMS.txt`、至少一台 Linux（OpenStick）与一台 Windows 启动成功、`/healthz` 正常。

## Release notes 模板

```markdown
版本 vX.Y.Z

Linux 两个包均为静态 musl 二进制（无 PT_INTERP 依赖），其中 `boranlib-linux-aarch64` 适用于 OpenStick。
Windows 包为 x86_64 msvc 构建。

校验：`sha256sum -c SHA256SUMS.txt`

## 变更

- （从 CHANGELOG 的对应版本段落摘取，按 Added / Changed / Fixed 分组）

## 升级注意

- （数据库迁移、配置项更名、需要人工干预的步骤；没有就写“无”）

## 已知问题

- （仅列本次发布仍存在、且用户可能撞上的问题；没有就写“无”）
```

## 回滚

- 发版后发现问题：先回退 tag 指向的发布记录（GitHub Release 可编辑/删除），**不要**强推或重写 `main`；
- 需要修复时发新 tag（`vX.Y.Z+1`），不要复用旧 tag。

## 校验和与可复现

- CI 与 `scripts/build-release.ps1` 都使用 `--locked` + 固定 toolchain；本地手工验证可用 `-ThinLto`，发版包仍用 fat LTO。
- `scripts/budget.json` 是体积阈值的唯一来源，`scripts/budget-baseline.json` 只作报告对照。
