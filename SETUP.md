# Boranlib 服务端 — 环境准备与部署文档

> 适用：本机 Windows 开发调试 + OpenStick(Debian aarch64) 部署。
> 最后更新：2026-11-02

## 0. 一句话策略
- **绝不在板子上 `cargo build`**：板子内存极小，Rust 链接期必 OOM。
- **本机 Windows 交叉编译**到 `aarch64-unknown-linux-musl`（静态链接），用 **Zig 作 linker**（`cargo-zigbuild`），产物是单文件静态二进制，`scp` 上去直接跑，板子零依赖。
- 本机另装 `x86_64-pc-windows-gnu` 工具链仅用于**本地调试**验证代码能编译能跑（不可上线）。

## 1. 本机需要准备的环境

### 1.1 Rust 工具链（自定义目录，避免占 C 盘）
- 安装时 `rustup-init.exe` 选自定义：
  - `RUSTUP_HOME = <local-tools>\rust\.rustup`
  - `CARGO_HOME = <local-tools>\rust\.cargo`
- 已就绪路径：`<local-tools>\rust`（含 `.rustup` / `.cargo`）。
- 用户级环境变量（已写入注册表，新开终端生效）：
  - `RUSTUP_HOME = <local-tools>\rust\.rustup`
  - `CARGO_HOME  = <local-tools>\rust\.cargo`
  - PATH 追加 `<local-tools>\rust\.cargo\bin`
- **关键坑（必做）**：本机编译必须装 gnu-host 工具链并设为默认，否则 `cargo` 的 build scripts / proc-macro 用 msvc host 找 `link.exe` 失败：
  ```bat
  rustup toolchain install stable-x86_64-pc-windows-gnu
  rustup default stable-x86_64-pc-windows-gnu
  rustup target add x86_64-pc-windows-gnu
  ```

### 1.2 MinGW-w64（gnu 调试编译用）
- 下载 WinLibs GCC（x86_64 / UCRT / POSIX / SEH 版），解压到 `<local-tools>\MinGW\mingw64\bin`（确保 `<local-tools>\MinGW\mingw64\bin\gcc.exe` 存在）。
- PATH 追加 `<local-tools>\MinGW\mingw64\bin`。
- 用途：gnu-host 的 build scripts 用 MinGW 的 `ld` 链接；`sqlx` bundled sqlite 用 gcc 编译 C 源码。

### 1.3 Zig 0.13.0（交叉编译 musl 用）
- 下载：`https://ziglang.org/download/0.13.0/zig-windows-x86_64-0.13.0.zip`
- 备选：若官网直连不稳定，优先用本机重试脚本（官网 + `--retry` + 断点续传 `-C -`，见 `<local-tools>\zig\get_zig.ps1`）或在能稳定访问的机器手动下载，解压到 `<local-tools>\zig\zig-windows-x86_64-0.13.0\` 并加入 PATH。（⚠ 实测 gh-proxy 对该 URL 返回空响应，**不可用**）
- 解压到 `<local-tools>\zig\zig-windows-x86_64-0.13.0\`，PATH 追加该目录（含 `zig.exe`）。
- `cargo-zigbuild` 会自动用 PATH 里的 zig 作 linker。

### 1.4 Node.js + npm（前端）
- Node 24+。npm 切 npmmirror：
  ```bat
  npm config set registry https://registry.npmmirror.com
  ```

## 2. 镜像配置（务必按要求配，否则下载卡死）

| 用途 | 镜像/源 | 配置位置 | 状态 |
|---|---|---|---|
| cargo **crates** | rsproxy (`sparse+https://rsproxy.cn/index/`) | `.cargo/config.toml` | ✅ 已配 |
| rustup **工具链** | tuna (`https://mirrors.tuna.tsinghua.edu.cn/rustup`) | 用户环境变量 `RUSTUP_DIST_SERVER` | ✅ 已配（rsproxy 装大组件会卡，弃用） |
| npm | npmmirror | `npm config set registry` | ✅ 已配 |
| **Zig 二进制** | ziglang.org 直连 | 直接下载 | ✅ 已配 |

> ⚠ **中科大 `rust-static` 实测 `403`，不要用作 rustup 镜像。**

### 当前 `.cargo/config.toml` 内容
```toml
[source.crates-io]
replace-with = "rsproxy-sparse"
[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"
[registries.rsproxy]
index = "https://rsproxy.cn/crates.io-index"
[net]
git-fetch-with-cli = true
```

### rustup 镜像（用户环境变量）
```
RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup
```
> 注：rsproxy 对**装 toolchain / target 大组件**会卡（`syncing` 不动或 `.partial` 0 字节），已改 tuna 并持久化；中科大 `rust-static` 实测 `403` 弃用。若卡住，清 `rustup` 进程与 `.rustup/tmp`、`.rustup/downloads` 锁后单进程重试。

## 3. 编译命令

### 3.1 本机调试（Windows gnu，不可上线）
```bat
cd d:\projects\read\server
cargo build --target x86_64-pc-windows-gnu
:: 产物 target\x86_64-pc-windows-gnu\debug\boranlib.exe
```

### 3.2 部署交叉编译（aarch64 musl 静态）
```bat
:: 1) 装 zigbuild（拉取走 rsproxy crates 源）
cargo install cargo-zigbuild
:: 2) 交叉编译（Zig 作 linker，静态链接 musl）
cargo zigbuild --target aarch64-unknown-linux-musl --release
:: 产物 target\aarch64-unknown-linux-musl\release\boranlib
:: 3) 部署
scp target\aarch64-unknown-linux-musl\release\boranlib root@<device>:/opt/boranlib/
ssh root@<device> "chmod +x /opt/boranlib/boranlib && /opt/boranlib/boranlib"
```

> 推荐用一键脚本（构建 → 打包 → 上传 → 备份 → 重启 → healthz → 失败自动回滚）：
> ```powershell
> .\scripts\deploy.ps1 -Device <device>
> ```
> 完整参数、离线 `-DryRun`、回滚与验收见 **`docs/DEPLOY.md`**。

## 4. 运行与冒烟测试
- 运行时环境变量（见 `src/config.rs`）：前缀 `BORANLIB_`（兼容 `LEGADO_`，新缺失才读旧），如 `BORANLIB_DATA_DIR`、`BORANLIB_JWT_SECRET`、`BORANLIB_ADMIN_USER`、`BORANLIB_ADMIN_PASS`、`BORANLIB_CORS_ORIGINS`。
- 首次启动自动建 admin（`bootstrap_admin`）。生产环境（release 或 `BORANLIB_ENV=production`）必须显式设置 `BORANLIB_JWT_SECRET`（≥32 字节）与 `BORANLIB_ADMIN_PASS`（≥8 字节且非 <寮€鍙戦粯璁ゅ彛浠?），否则拒绝启动；跨域部署用 `BORANLIB_CORS_ORIGINS` 列精确来源，默认不添加 CORS 层。
- 本机调试冒烟：
  ```bat
  set BORANLIB_DATA_DIR=./data
  set BORANLIB_JWT_SECRET=<绀轰緥瀵嗛挜>
  set BORANLIB_ADMIN_USER=admin
  set BORANLIB_ADMIN_PASS=<寮€鍙戦粯璁ゅ彛浠?
  target\x86_64-pc-windows-gnu\debug\boranlib.exe
  :: 另开终端：
  curl http://127.0.0.1:3000/
  curl -X POST http://127.0.0.1:3000/api/auth/login -H "Content-Type: application/json" -d "{\"username\":\"admin\",\"password\":\"<寮€鍙戦粯璁ゅ彛浠?\"}"
  ```

### 4.1 `/k` 性能埋点（默认开启，无需任何参数）

- 所有 `/k` 请求（书架 / 搜索 / 目录 / 阅读 / 登录页 / 空态 / 4xx / 5xx）都会写一行 JSONL，默认开启，不需要加参数。
- 文件位置：`DATA_DIR/logs/k-metrics-YYYYMMDD.jsonl`（按天分文件，不会进仓库、不会写进 SQLite）。
- 有界：保留最近 7 个日文件；目录总占用超过 20MB 时从最旧文件开始删。写失败只记 warn，不影响请求。
- 行内不含明文凭据：`k_session` cookie 只存 sha256 前 8 位（`sid`），UA 截断 120 字符，`key` 截断 64 字符；`tag` 为可选参数（普通浏览时为 null）。
- 管理员接口（需 `Authorization: Bearer <token>`，非管理员 403）：
  - `GET /api/k/metrics?date=YYYY-MM-DD`：当日聚合（各 path 请求数、`render_ms` p50/p95/max、`bytes` 均值、`since_prev_ms` p50/p95、`ua_class` 分布）+ 最近 20 条慢请求（`render_ms > 1000`）。
  - `GET /api/k/metrics/export?date=YYYY-MM-DD`：直接下载该日 JSONL 原文（`application/x-ndjson`）；当天无文件返回 404。
- **真机测试后，请把 `DATA_DIR/logs` 整个文件夹打包发回**（其中的 `k-metrics-*.jsonl` 就是原始数据）。

## 5. 运行时约束（100MB 空闲红线）
- 靠代码层保证：`PRAGMA` 小缓存（小 `cache_size`、禁用 `mmap`）、WAL 模式、限制并发抓取 / 连接池。
- 与交叉编译方式无关。

## 6. 已知坑（排错速查）
- **`link.exe not found`**：本机宿主是 msvc，build scripts 找 `link.exe`。修法：装并 `default` gnu-host 工具链（见 §1.1）。
- **rustup 装大工具链卡 `syncing` / 0 字节 `.partial`**：rsproxy 对大组件会卡，已切 **tuna**（`RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup`）。清 `rustup` 进程 + `.rustup/tmp` / `downloads` 锁后单进程重试。
- **中科大 `rust-static` 返回 403**：弃用。
- **bundled sqlite 编译失败**：确认 MinGW `gcc` 在 PATH（gnu-host 下由 gcc 编 C）。
- **Zig 下载中断/不全**：官网直连偶发连接重置（只下到十几 MB）。已改用「官网 + `--retry 8` + 断点续传 `-C -`」后台脚本重试（见 `<local-tools>\zig\get_zig.ps1`）；若该网络仍不稳，可手动下载（见 §1.3）解压到 `<local-tools>\zig\zig-windows-x86_64-0.13.0\` 并把该目录加入 PATH。gh-proxy 实测不可用。

## 7. 项目结构（简）
```
server/
  Cargo.toml  src/  migrations/  web/  run_build.ps1  install_toolchain.ps1
server/web/   # Svelte 前端；npm install && npm run build -> dist 由 Rust ServeDir 托管
```
