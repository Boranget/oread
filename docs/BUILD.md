# Boranlib 构建与交叉编译指南

> 适用：Windows 本机开发 + 交叉编译到 OpenStick（Debian aarch64）。
> 最后更新：2026-11-06。本文命令与仓库内脚本一一对应，改脚本时请同步改本文。

## 0. 速查

| 目的 | 命令（在仓库根执行） | 产物 |
|---|---|---|
| 本机开发构建（dev 快档，msvc） | `.\scripts\build-dev.ps1` | `<local-tools>\cache\target-dev\debug\boranlib.exe` |
| 本机 release 构建 | `.\scripts\build-dev.ps1 -Release` | `<local-tools>\oread-target-v<版本>\release\boranlib.exe`（按 HEAD 的 tag 推导，如 `oread-target-v0315`） |
| 交叉编译发布（aarch64 musl） | `.\scripts\build-release.ps1` | `<local-tools>\cargo-target-musl\aarch64-unknown-linux-musl\release\boranlib` |
| 交叉编译（加速） | `.\scripts\build-release.ps1 -ThinLto` | 同上（体积略大） |
| 兼容入口（等价 build-dev） | `.\server\run_build.ps1` | 同上，日志写 `server\build.log` |
| 兼容入口（等价 build-release） | `.\server\crossbuild.ps1` | 同上，日志写 `server\crossbuild.log` |
| 可选 gnu 目标构建 | `.\server\build_gnu.bat` | `<local-tools>\oread-target-gnu\x86_64-pc-windows-gnu\debug\boranlib.exe` |
| 环境准备（幂等） | `.\server\install_toolchain.ps1` | 无产物 |
| 清理本轮临时产物 | `.\scripts\clean-dev.ps1`（`-DryRun` 预览） | 白名单删除，打印清理前后可用空间 |

所有脚本的源码路径都由脚本自身位置推导（PowerShell 用 `$PSScriptRoot`，批处理用 `%~dp0`），
不再依赖 `d:\projects\read\server` 之类的历史绝对路径，可以在任意位置 clone。

## 1. 目录约定

| 用途 | 目录 | 覆盖方式 |
|---|---|---|
| 本机 msvc 调试（dev 快档） | `<local-tools>\cache\target-dev`（与 release 分离） | `-TargetDir`；或 `OREAD_TARGET_DEV`（兼容旧 `OREAD_TARGET_WIN`） |
| 本机 msvc 发布 | `<local-tools>\oread-target-v<版本>` | `-TargetDir`；或 `OREAD_TARGET_WIN`（release 禁止指向运行目录） |
| aarch64 musl 发布 | `<local-tools>\cargo-target-musl` | `CARGO_TARGET_DIR`；或 `OREAD_TARGET_MUSL` |
| gnu 目标调试（可选） | `<local-tools>\oread-target-gnu` | `OREAD_TARGET_GNU` |

约定要点：

- **本机调试统一用 `<local-tools>\cache\target-dev`**：dev 快档产物落该目录，不要再让各 agent 各自用 `server\target`；
  也不要把 dev target 与 release target 混用，否则每次切档位都要重编依赖（见 §3.4）。
  本机 `:3000` 的运行目录是 `<local-tools>\oread-target-win`，只由人工升级替换 exe，构建脚本不得写它的 `release\`。
- **release 构建必须落版本目录**：`build-dev.ps1 -Release` 未指定 `-TargetDir` 时按 HEAD 的 tag 推导
  `<local-tools>\oread-target-v<版本>`（如 `oread-target-v0315`）；显式指向运行目录会被脚本拒绝。
  v0.3.12 曾因未指定 `-TargetDir` 把旧产物写进 `oread-target-win\release\boranlib.exe`，故加此约束。
  Windows 包由 `<local-tools>\release-pack\make-pack.ps1` 产出，Linux 包落 `<local-tools>\openstick\<version>`。
- 构建产物与源码分离：仓库里不再产生 `server\target`（历史遗留目录可自行删除以回收空间）。
- `server\target` 里可能残留旧产物，新脚本一律不写它；如果发现脚本往这里写，说明 `CARGO_TARGET_DIR` 没生效。
- 日志：`server\build.log`、`server\crossbuild.log`，根 `.gitignore` 已有 `*.log`，不会进版本库。
- 这些输出目录都在本地工具目录下，建议同时加入 Defender 排除项（见 §7.2）。

## 2. 工具链与 `rust-toolchain.toml`

### 2.1 本机现状（2026-11-02 实测）

```
rustup home:  D:\rust\rustup
CARGO_HOME:   D:\rust\cargo
默认工具链:   stable-x86_64-pc-windows-msvc，rustc 1.96.0
已装目标:     aarch64-unknown-linux-musl、x86_64-pc-windows-msvc
zig:          <local-tools>\zig\zig-x86_64-windows-0.17.0\zig.exe（0.17.0，未加入 PATH）
cargo-zigbuild: 0.24.0
```

> 旧文档里的 `<local-tools>\rust\.rustup`、`stable-x86_64-pc-windows-gnu` 默认 host 已不适用，
> 脚本不再设置 `RUSTUP_HOME` / `CARGO_HOME`（直接继承机器级环境变量，或用 rustup 默认位置）。

### 2.2 仓库根 `rust-toolchain.toml`

```toml
[toolchain]
channel = "1.96.0"
components = ["rustfmt", "clippy"]
targets = ["aarch64-unknown-linux-musl"]
```

版本**固定为 1.96.0**（v0.3.12 / v0.3.13 的发布工具链，与本机 `stable` 的 rustc 版本一致）。
所有在仓库内执行的 `cargo` / `rustup` 都按此文件解析：

- 本机已装 1.96.0，`rustup show` 显示 `active because: overridden by ...rust-toolchain.toml`，
  `rustc -vV` 为 1.96.0，不触发下载。
- CI runner 镜像自带的是 `stable` = 1.99.1，**没有 1.96.0**；首次调用 `cargo` 时 rustup 会按版本号
  安装（含 `rustfmt` / `clippy` 与上述 musl target）。实测代价见 §2.3。

为什么不再写 `channel = "stable"`：`stable` 在本地与云端各自漂移，同一提交可能"本机绿、云端红"。
2026-11 就发生过一次（详见 §2.3）。

### 2.3 工具链固定策略（2026-11 起）

**为什么固定**

- CI runner 的 `stable` 在 2026-11 前进到 **1.99.1**，而本机与已发布二进制是 **1.96.0**。同一提交
  在本地 `cargo clippy --all-targets -D warnings` 全绿，云端 `check` job 却被两条新 lint 拦下：
  - `clippy::result_large_err` —— `server/src/routes/image.rs::collect_image_body`（Err 变体 ≥128 字节）；
  - `clippy::map_or_identity` —— `server/src/rule_engine.rs`（`map_or(x, |v| v)` 应写 `unwrap_or(x)`）。
- 后果是 v0.3.10 起所有 push / tag run 的 `check` 全部变红，而三平台 build 仍成功——"能出包、但门禁坏了"。
- 固定版本后，clippy 的 lint 集合、rustfmt 排版、编译器行为都可复现；升级变成显式动作。

**本机绿 ≠ CI 绿**

本机自检只能证明"在这台机器的这个编译器上通过"，不能证明云端会绿。门禁结论必须来自与 CI 同版本的
工具链；遇到"本地全绿、云端 clippy 报错"，先怀疑工具链版本差，再核对具体 lint 名，最后才怀疑代码。

**升级流程**（四步，缺一不可）

1. 本机安装新版本：
   `rustup toolchain install <版本> --profile minimal --component clippy,rustfmt`；
2. 改 `rust-toolchain.toml` 的 `channel` 为新版本（`components` / `targets` 保持）；
3. 本地用**新版本**跑完整门禁：`cargo fmt --check`、
   `cargo clippy --all-targets --locked -- -D warnings`、`cargo test --locked`；
   建议在无 `server/web/dist` 的干净检出上再跑一遍，确认自检不依赖前端产物；
4. push 到分支，确认 `check + test (linux x86_64)` 全绿再合并；发版前也先跑分支 run，不要直接打 tag。

**实测代价**（2026-11-05 的三次 push run，非估算）

| 项目 | 固定前 [42045688669](https://github.com/Boranget/oread/actions/runs/42045688669)（stable 1.99.1） | 固定后首次 [42048540080](https://github.com/Boranget/oread/actions/runs/42048540080)（1.96.0） | 固定后第二次 [42051192767](https://github.com/Boranget/oread/actions/runs/42051192767)（1.96.0） |
|---|---|---|---|
| rust-cache | — | `No cache found.` | `Cache restored … full match: true` |
| `cargo clippy` | 72s | 136s | 11s |
| `cargo build --release` | 1m09s | 4m46s | 1m08s |
| `cargo test` | 52s | 3m04s | 48s |
| check job 总计 | 4m53s | **11m12s** | **3m31s** |

读法：

- **安装发生在 `Cache cargo` 步骤内**：rust-cache 用 `rustc` 探测版本时会触发 rustup 按
  `rust-toolchain.toml` 自动安装 1.96.0，安装输出被 action 捕获，日志里不单列子步骤。
- **一次性代价来自 rust-cache**：它的键包含 rustc 版本，换版本后第一次必然 miss，全量冷编译
  11m12s；第二次即恢复 `full match`，3m31s——与固定前的 4m53s 同一量级（这次更快是因为
  那次 1.99 的缓存本身只命中了一部分）。
- 只要不改 `channel`，缓存键就稳定，后续 run 的命中率与固定前一致。

**tag run 三个 build job 的首次安装实测**（2026-11-06，tag `v0.3.14`，run
[42066841647](https://github.com/Boranget/oread/actions/runs/42066841647)，非估算）

`Cache cargo` 步骤从开始到打出 `Cache Configuration`（内含 `1.96.0 <host>`）的间隔，
减去同一 job、同一 runner 镜像、未固定工具链的 v0.3.13 tag run
[42029163885](https://github.com/Boranget/oread/actions/runs/42029163885) 的可比间隔：

| job | 无 pin 基线（1.99.1 预装） | 固定后（首次装 1.96.0） | 首次安装开销 |
|---|---|---|---|
| build (windows x86_64 msvc) | 12:41:18→12:41:27 = 8.96s | 16:40:43→16:41:03 = 19.25s | **≈10.3s** |
| build (linux x86_64 musl) | 12:41:09→12:41:13 = 3.95s | 16:40:34→16:40:52 = 17.38s | **≈13.4s** |
| build (linux aarch64 musl) | 12:41:11→12:41:17 = 6.45s | 16:40:34→16:40:46 = 11.97s | **≈5.5s** |
| check + test (linux x86_64)（对照） | 12:41:09→12:41:14 = 4.20s | 16:40:32→16:40:44 = 11.75s | ≈7.6s |

结论：首次安装 1.96.0 一次性耗时 **5–13s**（windows ≈10.3s、x86_64-musl ≈13.4s、
aarch64-musl ≈5.5s），随后由 rust-cache 固化；`rustup target add` 步骤仍是秒级
（x86_64-musl 4s，aarch64 因 `rust-toolchain.toml` 已声明 targets 而 <1s）。
三个 build job 与 check job 在该 tag run 上全部 success。

## 3. 本机开发构建

### 3.1 `scripts\build-dev.ps1`

```powershell
.\scripts\build-dev.ps1                       # dev 快档（默认）
.\scripts\build-dev.ps1 -Release              # release（自动落 <local-tools>\oread-target-v<版本>）
.\scripts\build-dev.ps1 -Release -TargetDir <local-tools>\oread-target-v0.3.16   # 显式版本目录
.\scripts\build-dev.ps1 -TargetDir D:\tmp\oread-win
.\scripts\build-dev.ps1 -CargoArgs '--bin','boranlib'
.\scripts\build-dev.ps1 -LogFile <local-tools>\cache\target-dev\build.log
```

参数：`-Release`、`-TargetDir`、`-CargoArgs`（透传）、`-LogFile`。
默认（dev 快档）落 `<local-tools>\cache\target-dev`（可用 `OREAD_TARGET_DEV` 覆盖，兼容旧 `OREAD_TARGET_WIN`）；
`-Release` 未指定 `-TargetDir`（且未设 `OREAD_TARGET_WIN`）时，按 `git describe --tags --exact-match`
推导 `<local-tools>\oread-target-<tag>`；不在 tag 上则用 `wip-<短 sha>`。`-Release` 指向运行目录
`<local-tools>\oread-target-win` 会直接报错退出（防止旧产物污染运行目录）。
脚本会先检查 `cargo` / `rustc` 是否存在、`server\Cargo.toml` 是否找得到，然后设置
`CARGO_TARGET_DIR` 并执行 `cargo build`；结束时打印产物路径与大小。
前端只在 `server\web` 的源码/配置比 `server\web\dist\index.html` 新（或 dist 不存在）时跑
`npm run build`；只改后端不会重建 dist。详细对照见 §3.4。

### 3.2 `server\run_build.ps1`（兼容入口）

行为等价于 `scripts\build-dev.ps1`，额外把输出 tee 到 `server\build.log`：

```powershell
.\server\run_build.ps1
.\server\run_build.ps1 -Release
```

历史上它编译的是 `x86_64-pc-windows-gnu`（依赖 MinGW）；现在本机默认 host 是 msvc，
直接本机构建即可。需要 gnu 产物请用 `build_gnu.bat`。

### 3.3 `server\build_gnu.bat`（可选，gnu 目标）

```bat
.\server\build_gnu.bat
```

- 自动探测 MinGW：`OREAD_MINGW` → `<local-tools>\MinGW\mingw64` → PATH。
- 缺 `x86_64-pc-windows-gnu` 目标时自动 `rustup target add`（幂等）。
- 产物在 `<local-tools>\oread-target-gnu\x86_64-pc-windows-gnu\debug\boranlib.exe`。
- 脚本内提示文字为 ASCII，避免 `cmd.exe` 代码页导致乱码。

### 3.4 本地开发快档 vs 发版慢档（2026-11-06 实测）

两条命令，两个互不干扰的 target：

| 档位 | 命令 | target | 产物 |
|---|---|---|---|
| dev 快档（默认） | `.\scripts\build-dev.ps1` | `<local-tools>\cache\target-dev` | `debug\boranlib.exe` |
| release 慢档（发版/测量） | `.\scripts\build-dev.ps1 -Release` | `<local-tools>\oread-target-v<版本>` | `release\boranlib.exe` |

**profile 分流（`server/Cargo.toml`）**

- `[profile.dev]`：`opt-level = 0`、`debug = false`、`incremental = true`；
- `[profile.dev.package."*"]`：依赖 `opt-level = 2`，编一次后长期缓存；
- `[profile.release]` **未改动**（`opt-level = "z"`、`lto = true`、`codegen-units = 1`、`panic = "abort"`、`strip = "symbols"`）。

target 分流的意义：dev 与 release 用不同 `CARGO_TARGET_DIR`，切档位不需要重编依赖。
前端只在 `server\web` 改动时才 `npm run build`（见 §3.1），只改后端不重建 dist。

**实测对照（2026-11-06，本机 msvc host，rustc 1.96.0；release 数字为已知对照）**

| 场景 | 耗时 | 产物 |
|---|---|---|
| dev 全量首编（干净 `target-dev`） | **12 分 18 秒**（cargo 报 12m16s） | 36,474,368 B ≈ 34.78 MB |
| dev 改 1 个文件后增量 | **4.6 秒**（cargo 报 4.51s） | 同上 |
| release 冷编（fat LTO） | 约 9 分 40 秒 | 发布口径（见 §4.2） |
| release 改 1 个文件后增量 | 约 3 分 26 秒 | 同上 |

读法：

- **dev 首编不比 release 快**：依赖按 `opt-level=2` 全量编一遍是主要成本（boa_engine / sqlx / image 等）。
  dev 的优势在第二次以后——本 crate 保持 `opt-level=0` + 增量，改一行约 4.6 秒（release 约 3 分 26 秒）。
  第一次请让 `target-dev` 在后台暖起来，之后日常迭代才是秒级。
- **dev 二进制体积不作结论**：34.78 MB 是未 strip、依赖 opt2 的调试产物；体积/首屏只认 release 与
  `check-budget` 的 CI 产物。
- **CI 成本**：CI 的 `cargo test` 同样走 dev 档，首次编译依赖（opt2）会明显变慢，命中 cargo 缓存后回落；
  各平台 release 产物 job 的 profile 未变，不受影响。
- **运行时性能、内存不可比**：dev 档本 crate 未优化，接口耗时会明显高于 release；任何性能/内存结论必须用 release。

**什么时候必须用 release**

- 出包、打 tag、跑 `check-budget`，以及任何体积 / 首屏 / 性能 / 内存数字；
- 复现“只有 release 才出现”的问题（LTO、`panic=abort`、strip 后符号缺失等）；
- 部署到设备或升级本机 `:3000`。

**dev 二进制可用性**（2026-11-06 实测：临时端口 3919 + 数据副本，不碰 `:3000`）：

| 检查 | 结果 |
|---|---|
| `/healthz` | 200 |
| `/k`（带 `k_session`） | 200 |
| `/k/toc`（带 `k_session`） | 200 |
| `/k/read`（带 `k_session`） | 200，底栏 `width="25%"` × 4、「记录」0 次 |
| `/api/k/probe?stage=sync`（带 `k_session`） | 204 |
| HTML 里 `<img src="/api/k/probe?...">` | 2 处 |

结论：dev 二进制足以做版面/路由验证，看一眼页面不必编 release。

**dev profile 对测试的两处影响（已随本次改动处理）**

- 依赖 opt2 后 boa 在 1 MiB 栈上会溢出（`req::tests::source_header_js_is_evaluated`）：
  `rule_engine::js_sender()` 在 `debug_assertions` 下把 JS worker 栈提到 4 MiB，release 仍是 1 MiB；
- `slow_js_rule_reports_budget_timeout` 的慢规则每轮分配从 8 KiB 提到 64 KiB，避免优化后的依赖先撞
  `loop_iteration_limit`（500k）而不是墙钟预算。

门禁命令不变：`cargo fmt --check`、`cargo clippy --all-targets --locked -- -D warnings`、`cargo test --locked`。

#### 产物留存策略（2026-11-06 起，每轮必做）

1. **日常验证**：一律用本机 dev 快档编译（`.\scripts\build-dev.ps1` 默认行为），编译后直接跑起来测；
   不再每轮都打正式包。
2. **正式版**：只有 tag、三平台产物、体积门禁才走 release / 云端 CI（新版本机发布流程见 §4.5）。
3. **每轮测完必须清理**：删除该轮使用的 dev target 与临时 release target（路径登记见下表）；
   删除临时 `server/web/dist` 副本、临时数据目录、临时日志、临时凭据；
   保留运行目录 `<local-tools>\oread-target-win`、成品包 `release-pack/` 与 `openstick/<version>/`、原始日志 `incoming-*`。

| 类别 | 路径（白名单） | 处置 |
|---|---|---|
| dev target（每轮一个） | `<local-tools>\cache\target-dev`（`OREAD_TARGET_DEV` 覆盖） | 删 |
| 临时 release target（每轮一个） | `<local-tools>\oread-target-v*`、`<local-tools>\oread-target-wip-*` | 删 |
| 临时数据/日志/凭据 | `%TEMP%\oread-*`、`%TEMP%\k_session*.txt`、`<local-tools>\cache\*.log` | 删 |
| 运行目录（`:3000`） | `<local-tools>\oread-target-win` | **保留** |
| 成品包 | `<local-tools>\release-pack`、`<local-tools>\openstick\<version>` | **保留** |
| 原始日志 | `incoming-*` | **保留** |
| 源码仓库 / 他人 worktree | `<local-tools>\bench\*` | **保留** |

清理脚本 `scripts\clean-dev.ps1`：

```powershell
.\scripts\clean-dev.ps1 -DryRun     # 只列清单与大小
.\scripts\clean-dev.ps1             # 实际清理（含重命名探测）
.\scripts\clean-dev.ps1 -KeepDevTarget   # 只清临时 release target / 日志 / 凭据
```

安全前置：检测到 `cargo` / `rustc` / `clippy-driver` / `rustdoc` / `MSBuild` / `dotnet` / `link`
进程时拒绝执行；每个目标先**重命名探测**（改名成功才算无占用、可再生），再删除；
结尾打印 D:/C: 清理前后可用空间与删除明细。

## 4. 交叉编译发布（aarch64-unknown-linux-musl）

### 4.1 前置：`server\install_toolchain.ps1`

```powershell
.\server\install_toolchain.ps1                  # 只检查，不做安装
.\server\install_toolchain.ps1 -InstallZigBuild # 缺 cargo-zigbuild 时才 cargo install
.\server\install_toolchain.ps1 -GnuHost         # 额外装 gnu 目标 + gnu host 工具链（不会被设为默认）
```

脚本按 5 步检查：rustup/cargo → musl 目标 → zig → cargo-zigbuild →（可选）gnu。
每一步失败都给可照做的提示；`rustup` 相关下载只用本进程的 `RUSTUP_DIST_SERVER`，不写用户环境变量。
它**不再**在后台偷偷拉起 `run_build.ps1`（旧行为会让人误判构建结果）。

### 4.2 `scripts\build-release.ps1`

```powershell
.\scripts\build-release.ps1                    # fat LTO（Cargo.toml: lto=true, codegen-units=1）
.\scripts\build-release.ps1 -ThinLto           # thin LTO + codegen-units=16，明显更快、体积略大
.\scripts\build-release.ps1 -TargetDir D:\tmp\oread-musl
.\scripts\build-release.ps1 -RustupMirror https://mirrors.tuna.tsinghua.edu.cn/rustup
```

脚本做的事：

1. 校验 `cargo` / `rustc` / `rustup`，按需 `rustup target add aarch64-unknown-linux-musl`；
2. 找 zig（顺序：`OREAD_ZIG` → PATH → `<local-tools>\zig\**\zig.exe` → `%LOCALAPPDATA%\zig`），并把其目录加到本进程 PATH；
3. 校验 `cargo-zigbuild`；
4. 设置 `CARGO_TARGET_DIR`；`-ThinLto` 时再用环境变量覆盖 profile；
5. 执行 `cargo zigbuild --target aarch64-unknown-linux-musl --release`，打印产物大小。

v0.3.1 时的实测样本（本机 msvc host + zig 0.17.0 + cargo-zigbuild 0.24.0，默认 fat LTO；当前体积以 CI 输出与 `check-budget` 为准）：

- **release 全量重建约 98–330 秒**（取决于增量状态；纯 LTO 链接段本身约 4 分钟，冷依赖阶段另计）。
- 产物大小按平台分开看，不要混用：
  - **交叉编译 `aarch64-unknown-linux-musl`（部署到 OpenStick）**：v0.3.1 为 8,530,216 B ≈ **8.14 MiB**（静态 musl，无 `PT_INTERP`）。
  - **本机 Windows msvc release（仅本地运行/调试）**：v0.3.1 为 9,012,224 B ≈ **8.59 MiB**；v0.3.6 的 CI 产物为 10,670,080 B ≈ **10.18 MiB**（`<local-tools>\oread-target-win\release\boranlib.exe`）。

**所有覆盖都走环境变量，不改 `server/Cargo.toml`**：

| 开关 | 等价环境变量 | 说明 |
|---|---|---|
| `-TargetDir` / `OREAD_TARGET_MUSL` | `CARGO_TARGET_DIR` | 输出目录 |
| `-ThinLto` | `CARGO_PROFILE_RELEASE_LTO=thin`、`CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16` | 仅本次进程 |

### 4.3 `server\crossbuild.ps1`（兼容入口）

```powershell
.\server\crossbuild.ps1
.\server\crossbuild.ps1 -ThinLto
```

等价于 `scripts\build-release.ps1`，日志写 `server\crossbuild.log`。

### 4.4 资源预算门禁

体积和首屏不再靠人肉盯，超了 CI 直接失败：

| 指标 | 上限（`scripts/budget.json`） | v0.3.1 实测（历史对照；当前以 CI 输出为准） |
|---|---|---|
| `boranlib-linux-aarch64` | 12 MiB | 9.09 MiB |
| `boranlib-linux-x86_64` | 15 MiB | 11.70 MiB |
| `boranlib-windows-x86_64.exe` | 12 MiB | 9.59 MiB |
| 首屏 JS（入口 chunk + 其静态 import，gzip） | 90 kB | 70.1 kB |
| 首屏 CSS（gzip） | 12 kB | 6.7 kB |

- **阈值只改一处**：`scripts/budget.json`。CI 与本地脚本都读它，不存在第二份。
- **本地能跑**：`node scripts\check-budget.mjs`；只测前端加 `--frontend-only`；产物路径用 `BUDGET_BIN_WIN` / `BUDGET_BIN_X86_64` / `BUDGET_BIN_AARCH64` 覆盖（默认读 `artifacts/` 下 CI 下载的产物）。
- **CI 位置**：`web` job 里跑前端预算（所有 push 都跑，tag 也跑）；单独 `budget` job 在 tag/手动触发时对三平台产物跑严格模式（缺产物即失败）；`release` 依赖 `budget`，超限时发不出 Release。
- **超了怎么办**：先看是不是新增依赖或依赖特性（`cargo tree -e features`、`npm ls`）导致的跳变；确认是有意增加就同步上调 `budget.json` 里的对应上限并在提交正文写理由，否则先优化（`opt-level`/`strip`/前端按需 import/拆包）。对照值在 `scripts/budget-baseline.json`，日志里会打印"上次通过"的大小。

### 4.5 正式版本机发布流程（2026-11-06 起）

**原则**：正式版 = **本机 release 编译 + 本机打包 + 本机验收**，通过后再推 tag；
不再从 GitHub 拉产物（私有仓库下载实测约 43 KB/s，太慢）。

**六步**

1. **合并 / 门禁**：`git merge --no-ff`（先 `git merge-tree --write-tree main <tip>` 确认零冲突），
   然后 `cargo fmt --check`、`cargo clippy --all-targets --locked -- -D warnings`、`cargo test --locked`。
2. **本机 release 编译**：`.\scripts\build-dev.ps1 -Release`，产物落 `<local-tools>\oread-target-v<版本>`；
   `-Release` 禁止写运行目录 `<local-tools>\oread-target-win`（脚本会直接报错）。
3. **前端与打包**：只在 `server\web` 有改动时 `cd server\web; npm run build`（脚本会自动判断并跳过无改动的情况）；
   然后 `<local-tools>\release-pack\make-pack.ps1 -ExePath <release exe> -DistPath <dist> -Version <x.y.z>`。
4. **本机隔离验收**：把 zip 解压到临时目录，用包内 `start.ps1` 在**隔离端口**（如 3919）+
   **临时数据目录**起服务；检查六页 200、底栏四格 / 顶栏三格、`/api/k/probe` 204、解压即启；
   验收完按 §3.4「产物留存策略」清理，不碰 `:3000`。
5. **一致性**：本机包与 CI 包必须同一份源码 + 同一 `start.ps1` 模板 + 同一版本号；
   记录 zip 内 `SHA256SUMS.txt` 的 `boranlib.exe` / `web/dist` 哈希，tag 后与 CI 产物比对；
   不一致先查原因（最常见是 dist 未重建）。
6. **推 tag**：验收通过后 `git tag -a vX.Y.Z` + `git push origin main vX.Y.Z`；
   CI tag 流水线只做门禁复核 + Linux 交叉产物。

**与 CI 的分工**

| 项目 | 本机 | CI tag 流水线 |
|---|---|---|
| Windows exe / zip | **主产出**（编译 + 打包 + 隔离验收） | 复核编译（不再下载） |
| Linux aarch64 / x86_64 musl | 无 zig 交叉链时不做 | **主产出** |
| 门禁 fmt / clippy / test | 合并前必跑 | 复核 |
| 体积门禁 `check-budget` | 用本机 release 产物 | 复核三平台 |
| 正式包获取方式 | 本机产物 | 仅 Linux 包 |

**仍必须走云端的情况**：需要 Linux 静态交叉产物（musl/aarch64）、需要发布门禁复核、
或需要与 CI 环境一致的复现。

> 落地版本：v0.3.20 仍按旧流程（CI 出包）；**v0.3.21 起**按本节执行。

## 5. 前端与运行

```powershell
cd server\web
npm config set registry https://registry.npmmirror.com   # 仅首次（或临时加 --registry）
npm ci            # 没有 lock 变更时优先 npm ci
npm run build     # 产物：server\web\dist
```

后端用 `ServeDir` 托管前端，目录取 `BORANLIB_WEB_DIR`（兼容旧前缀 `LEGADO_WEB_DIR`，默认 `web/dist`）。默认路径是**相对进程工作目录**的，
所以启动二进制时 cwd 应为 `server`（或把 `web/dist` 放在 cwd 下，或把 `BORANLIB_WEB_DIR` 设为绝对路径）。否则静态页 404，API 不受影响。

运行时环境变量（见 `server/src/config.rs`）。**前缀为 `BORANLIB_`，同时兼容旧前缀 `LEGADO_`（新前缀缺失或为空才读旧前缀，两套都缺才用默认值）**：

| 变量（新 / 旧） | 默认 |
|---|---|
| `BORANLIB_HOST` / `LEGADO_HOST` | `0.0.0.0` |
| `BORANLIB_PORT` / `LEGADO_PORT` | `8080` |
| `BORANLIB_DATA_DIR` / `LEGADO_DATA_DIR` | `data` |
| `BORANLIB_JWT_SECRET` / `LEGADO_JWT_SECRET` | `<寮€鍙戝崰浣嶅瘑閽?`（仅开发；生产须 ≥32 字节） |
| `BORANLIB_ADMIN_USER` / `BORANLIB_ADMIN_PASS`（旧前缀同理） | `admin` / `<寮€鍙戦粯璁ゅ彛浠?`（仅开发；生产必须显式设置口令） |
| `BORANLIB_CORS_ORIGINS` / `LEGADO_CORS_ORIGINS` | 空（同源）；逗号分隔的精确来源 |

端口口径：上表是**二进制内置缺省** 8080；Windows 成品包的 `start.ps1` 在未显式配置时会注入
`BORANLIB_PORT=3000`，因此成品包实际监听 `0.0.0.0:3000`（v0.3.12 实测），只有直接运行二进制才落回 8080。

本机冒烟：

```powershell
$env:BORANLIB_DATA_DIR='.\data'; $env:BORANLIB_JWT_SECRET='<绀轰緥瀵嗛挜>'
$env:BORANLIB_ADMIN_USER='admin'; $env:BORANLIB_ADMIN_PASS='<寮€鍙戦粯璁ゅ彛浠?'
cd server; & '<local-tools>\cache\target-dev\debug\boranlib.exe'
# 另开终端： curl http://127.0.0.1:8080/
```

`/k` 埋点写在 `DATA_DIR/logs/k-metrics-YYYYMMDD.jsonl`（`DATA_DIR` 即 `BORANLIB_DATA_DIR`，默认 `data`）；管理员可用 `GET /api/k/metrics?date=YYYY-MM-DD` 看聚合、`GET /api/k/metrics/export?date=YYYY-MM-DD` 导出原始 JSONL（接口细节见 `docs/API.md` §11.5）。

部署到板子（绝不在板上 `cargo build`，链接期会 OOM）：

```powershell
scp <local-tools>\cargo-target-musl\aarch64-unknown-linux-musl\release\boranlib root@<device>:/opt/boranlib/
ssh root@<device> "chmod +x /opt/boranlib/boranlib && /opt/boranlib/boranlib"
```

> 部署路径以你们实际约定为准（旧文档用的是 `/opt/boranlib/`）。

升级已有设备前，请先看 [§11 升级与回滚](#11-升级与回滚数据库迁移)：迁移前会自动备份到 `data/backups/`。

## 6. 常用环境变量一览

| 变量 | 作用 | 用在哪 |
|---|---|---|
| `CARGO_TARGET_DIR` | 构建输出目录 | 所有 cargo 调用 |
| `OREAD_TARGET_WIN` | 覆盖 msvc release 输出目录（dev 下作为旧变量兼容） | build-dev.ps1 |
| `OREAD_TARGET_DEV` | 覆盖 dev 快档输出目录（默认 `<local-tools>\cache\target-dev`） | build-dev.ps1 |
| `OREAD_TARGET_MUSL` | 覆盖 musl 输出目录 | build-release.ps1 |
| `OREAD_TARGET_GNU` | 覆盖 gnu 输出目录 | build_gnu.bat |
| `OREAD_ZIG` | 指定 zig.exe 完整路径 | build-release.ps1 / install_toolchain.ps1 |
| `OREAD_MINGW` | 指定 MinGW 根目录 | build_gnu.bat |
| `RUSTUP_DIST_SERVER` | rustup 发行源（脚本内只对本进程生效） | rustup add target/toolchain |
| `CARGO_PROFILE_RELEASE_LTO` / `_CODEGEN_UNITS` | 覆盖 release profile | build-release.ps1 -ThinLto |
| `BORANLIB_<KEY>` | 运行时配置（新前缀，优先） | 服务进程 |
| `BORANLIB_DB_BACKUP_KEEP` | 迁移前自动备份的保留份数（默认 5，范围 1–50） | 服务进程 |
| `BORANLIB_JS_TIMEOUT_MS` | 单段书源 JS 规则的时间预算（默认 3000ms，范围 20–5000；跑完再判定，不硬中断）。200ms 会把慢设备上约 1.4s 的漫画图片规则误判超时 | 服务进程 |
| `BORANLIB_SEARCH_TIMEOUT_SECS` | 聚合搜索**curated 单源**预算（默认 8s，范围 5–30，决定 `curated` 的 `deadline_ms`） | 服务进程 |
| `BORANLIB_SEARCH_ALL_TIMEOUT_SECS` | 聚合搜索**all 单源**预算（默认 5s，范围 5–30）。`all` 要覆盖上百个源，单源预算越短，同预算下能跑到的源越多；未设置时沿用 `SEARCH_TIMEOUT_SECS` | 服务进程 |
| `BORANLIB_SEARCH_CURATED_BUDGET_SECS` / `BORANLIB_SEARCH_ALL_BUDGET_SECS` | 聚合搜索**整批**预算（默认 curated 6s / all 25s，范围 2–60；按 scope 分档，优先级高于统一覆盖值） | 服务进程 |
| `BORANLIB_SEARCH_TOTAL_BUDGET_SECS` | 聚合搜索整批预算的统一覆盖值（两 scope 同值；显式按 scope 的变量优先。只设旧 `SEARCH_TIMEOUT_SECS` 时整批与之同值以兼容旧部署） | 服务进程 |
| `BORANLIB_MIGRATION_REPAIR` | 换行血缘自愈开关（默认 `on`：备份后单事务修正纯换行差异，内容真不同仍拒绝；`off` 时完全不修，校验和不符直接报错） | 服务进程 |
| `BORANLIB_EINK_JPEG_QUALITY` | 墨水屏灰度 JPEG 质量（默认 70，范围 60–90，越界夹取） | 服务进程（图片代理） |
| `BORANLIB_EINK_JPEG_WIDTH` | 墨水屏转码目标宽度（默认 1200，范围 800–1600，只缩不放） | 服务进程（图片代理） |
| `BORANLIB_EINK_ADAPTIVE` | 自适应质量开关（默认关；`1/true/on/yes/eink` 开启，文字页 q85、画面页 q70） | 服务进程（图片代理） |
| `BORANLIB_EINK_USM` | 轻量 USM 锐化开关（默认关；样本上 CER 变差，不建议开） | 服务进程（图片代理） |
| `LEGADO_<KEY>` | 运行时配置（旧前缀，仅兼容） | 服务进程 |

墨水屏转码默认档：**q70、宽 1200、内置 `image` 编码器、USM 关、自适应关**。
解码像素上限固定 `MAX_TRANSCODE_PIXELS = 8_000_000`（8 MP，不可配置）：源图 `宽×高` 超过则拒绝解码（WebP/JPEG 转码路径都会先读头部做校验）。

## 7. 常见报错与排查顺序

排查顺序建议：**工具在不在 PATH → 有没有并发/杀软干扰 → 目标装没装 → linker 配没配 → 依赖本身**。

### 7.1 `Blocking waiting for file lock on build directory`

同一个 `CARGO_TARGET_DIR` 被两个 cargo 进程同时使用（常见于多个终端/多个 agent 并行编译）。

- 先等前一个构建结束；或换目录：`-TargetDir <local-tools>\oread-target-win-2`。
- 查占用：`Get-CimInstance Win32_Process -Filter "name='cargo.exe'" | Select ProcessId,CommandLine`。
- **不要**在进程还活着时手删 `target\.cargo-lock`，容易留下半成品状态。

### 7.2 Defender / 实时扫描拖慢构建

症状：编译时间异常长、`target` 目录 CPU 满、`rustc` 进程反复读写同一文件。

```powershell
# 管理员 PowerShell
Add-MpPreference -ExclusionPath '<local-tools>\oread-target-win','<local-tools>\cargo-target-musl','<local-tools>\oread-target-gnu','D:\rust\cargo','D:\rust\rustup'
```

### 7.3 gnu target 缺失

症状：`error[E0463]: can't find crate for 'core'`、`target may not be installed`、或 `link.exe not found`（msvc host 缺 VS Build Tools）。

- 只是本机调试 → 用 `scripts\build-dev.ps1`（msvc host），不需要 gnu。
- 确实要 gnu → `rustup target add x86_64-pc-windows-gnu`，并装 MinGW-w64（`build_gnu.bat` 会自动检查）。
- `link.exe not found` 且 host 是 msvc → 装 VS Build Tools（Desktop development with C++）或改用 gnu host（`install_toolchain.ps1 -GnuHost`，但不要随手 `rustup default` 切默认）。

### 7.4 找不到 zig / cargo-zigbuild

脚本会直接报错并给出下载/安装方式：

- zig：https://ziglang.org/download/ ，解压到 `<local-tools>\zig\<版本>\`，或设 `OREAD_ZIG`。
- cargo-zigbuild：`cargo install cargo-zigbuild`（或 `.\server\install_toolchain.ps1 -InstallZigBuild`）。

### 7.5 rustup 下载 403 / `.partial` 卡住

- `RUSTUP_DIST_SERVER` 应指向可用镜像，脚本默认 USTC（2026-11-02 实测 `dist/channel-rust-stable.toml.sha256` 返回 200）。
  旧文档「中科大 rust-static 403」的结论已过期。
- 仍失败时换 tuna：`.\scripts\build-release.ps1 -RustupMirror https://mirrors.tuna.tsinghua.edu.cn/rustup`。
- 卡住的处理：结束残留 `rustup` 进程，删掉 `RUSTUP_HOME\tmp` 与 `RUSTUP_HOME\downloads` 后单进程重试。

### 7.6 bundled sqlite / C 代码编译失败

`libsqlite3-sys` 需要 C 编译器：

- msvc host → 需要 VS Build Tools 的 `cl.exe`；
- gnu 目标 → 需要 MinGW `gcc`（`OREAD_MINGW` 或 PATH）；
- musl 交叉 → 由 zig 的 `zig cc` 处理，前提是 zig 与 cargo-zigbuild 都能找到。

### 7.7 zig 与 cargo-zigbuild 版本不兼容

`cargo zigbuild` 依赖 zig 的具体命令行行为。本机是 zig 0.17.0 + cargo-zigbuild 0.24.0，
**已实测可完成本仓库完整发布构建**（2026-11-02，含 boa_engine / libsqlite3-sys / ring / psm / stacker 等重依赖，
产出 8,530,216 B（v0.3.1 口径）的静态 musl 二进制，无 PT_INTERP）。出现 linker 报错时先在临时小 crate 上验证：

```powershell
cargo new D:\tmp\zigtest; cd D:\tmp\zigtest
cargo zigbuild --target aarch64-unknown-linux-musl --release
```

不通则把 cargo-zigbuild / zig 降到互为已知兼容的版本组合。

### 7.8 zig 的临时目录被拒（`failed to create output directory ... AccessDenied`）

zig 默认把临时文件放在 `%LOCALAPPDATA%\zig\tmp`、全局缓存放在 `%LOCALAPPDATA%\zig`。
安全软件（360 主动防御进程 `ZhuDongFangYu` 即使界面已关闭仍可能常驻）会间歇性拦截目录创建，
表现为第一次编译就失败、不过手工新建同名目录又成功（重试可能换一个 hash 继续失败）。

排查：

```powershell
# 1) 看是否还有安全软件守护进程
Get-Process ZhuDongFangYu,360tray -ErrorAction SilentlyContinue | Select ProcessId,Name
# 2) 手工验证目录可写
New-Item -ItemType Directory -Force "$env:LOCALAPPDATA\zig\tmp" | Out-Null
```

处理优先级：彻底退出实时防护 → 把 zig 临时/缓存目录重定向到排除区内 → 再重试。

```powershell
$env:ZIG_GLOBAL_CACHE_DIR = '<local-tools>\zig-cache'
$env:ZIG_LOCAL_CACHE_DIR  = '<local-tools>\zig-cache'
.\scripts\build-release.ps1
```

脚本本身不会修改 zig 的缓存位置，这两个变量只在当前 shell 生效。

### 7.9 板子上跑不起来 / 被 OOM kill

- 确认产物是 **静态 musl**（本机 Windows 上可用 7-Zip 或 `wsl file boranlib` 检查 ELF）。
- 板子内存红线约 100MB，服务端靠小 SQLite 缓存 / 限制并发保证；构建过程一律不在板上跑。

## 8. 镜像策略

| 用途 | 当前选择 | 说明 |
|---|---|---|
| rustup 发行源 | USTC `https://mirrors.ustc.edu.cn/rust-static` | 本次实测可用（200）；脚本默认值，可用 `-RustupMirror` 换 |
| cargo crates 源 | 本机全局 `CARGO_HOME\config.toml` → 阿里云 sparse | 仓库内未强制覆盖，沿用机器配置 |
| npm | npmmirror `https://registry.npmmirror.com` | `npm config set registry` |

关于 crates 源的速度：本机测速显示 USTC（1.84 MB/s）明显快于阿里云（0.4 MB/s）。
如需切换，改机器级全局配置（不在仓库内，按需自行决定）：

```toml
# %CARGO_HOME%\config.toml
[source.crates-io]
replace-with = "ustc"

[source.ustc]
registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/"

[net]
git-fetch-with-cli = true
retry = 3
```

切换前确认 `https://mirrors.ustc.edu.cn/crates.io-index/config.json` 可访问（实测 200）。

## 9. CI

`.github/workflows/ci.yml` 在每次 push / PR / tag 跑四个并行 job：

- `rust`：Linux runner 上的 `cargo fmt --check` + `cargo clippy --all-targets -D warnings` + `cargo test --locked`，全部 debug 构建。
- `web`：`npm ci` + `npm test` + `npm run build` + `npm run check` + 首屏体积门禁。
- `eink`：`node scripts/eink-lint.mjs`（扫描 `kindle.rs` 与 `kindle/**/*.rs`）。
- `migrations`：索引/工作区 LF 与 blob 一致性门禁。

- **每次 push 不再跑 `cargo build --release`**：fat LTO 只在 tag / 手动触发的三平台 job 执行；`release` 依赖 `rust` / `web` / `eink` / `migrations` 与 `budget`。
- 服务端为「库 + 二进制」：`src/lib.rs` 暴露模块，`src/main.rs` 只负责启动；`tests/routes_smoke.rs` 用 `oneshot` 在临时库上跑路由冒烟，`tests/rule_engine_fixtures.rs` 从公开规则引擎入口跑 13 组 fixture，均随 `cargo test` 一起执行。
- 只做「能不能编译、单测过不过」，**不做** aarch64 交叉编译（runner 上还要装 zig + cargo-zigbuild，收益低于维护成本）。
- 由于 `rust-toolchain.toml` 声明了 musl 目标，CI 首次会额外下载该 target（一次性开销）。
- cargo 使用 `Swatinem/rust-cache@v2`；前端使用 `setup-node` 的 npm 缓存。

## 10. 已知未验证 / 需人工确认

- ~~zig 0.17.0 + cargo-zigbuild 0.24.0 能否完成本仓库完整发布构建~~ —— **已于 2026-11-02 实测通过**（见 §7.7）。
- 发布产物的体积（fat LTO 口径，v0.3.1 历史记录）：**musl 交叉产物** 8,530,216 B ≈ 8.14 MiB；**Windows msvc release** 9,012,224 B ≈ 8.59 MiB；当前版本以 CI 产物与 `check-budget` 输出为准；
  thin LTO（`-ThinLto`）尚未过滤一遍，体积与耗时未记录。
- OpenStick 上的部署路径与启动方式（旧文档 `/opt/boranlib/`）。
- CI 首次在真实 runner 上的耗时与是否需要缓存。

## 11. 升级与回滚（数据库迁移）

### 11.1 迁移前自动备份

服务启动时会先读 `_sqlx_migrations` 判断状态，只有在“确实要动数据库”时才备份：

| 状态 | 是否备份 | 行为 |
|---|---|---|
| 全新库（无 `_sqlx_migrations`） | 否 | 直接建表，没有可丢的数据 |
| 无待应用迁移 | 否 | 正常启动，不产生多余备份 |
| 有本程序支持的待应用迁移 | **是** | 备份后执行迁移 |
| 数据库版本高于本程序 | **是** | 备份后拒绝启动（见 §11.2） |

- 备份位置：`$BORANLIB_DATA_DIR/backups/<UTC 时间戳>/`，内含 `shared.db` 与 `users/<uid>.db`。
- 一致性：用 SQLite 的 `VACUUM INTO` 生成快照（单读事务写出整库），**WAL 里未合并的数据也会包含**；
  随后对每个快照跑 `PRAGMA integrity_check`，不通过就删掉这份不完整备份并中止启动。不是简单复制文件。
- 备份失败（磁盘满、无写权限、库被占用）会**阻止启动**并打印原因，不会“先跑起来再说”。
- 保留份数：默认 5 份，`BORANLIB_DB_BACKUP_KEEP` 可调（1–50）。只清理形如 `<UTC 时间戳>` 的目录，
  人工放进 `backups/` 的东西不会被删。
- eMMC 容量：每份备份约等于库文件大小（VACUUM 后通常更小），5 份即上限；空间紧张时把该值调到 2–3。

> `data/backups/` 里是真实书源/账户数据（可能含 Cookie、token），`server/data/` 已在 `.gitignore` 中；
> 不要把备份目录提交进版本库或上传到公共位置。

### 11.2 版本不匹配（库比程序新）

**现象**：装过“临时构建/更新版本”的二进制后，再装回旧版本，启动直接退出，日志出现 `VersionMismatch(N)` 或
`VersionMissing(N)`，`healthz` 一直是 000。

**现在的输出**（示例，数据库 version 8、程序最高支持 7）：

```text
Error: 数据库版本高于当前程序，无法安全启动。

  数据库版本：8（迁移记录中的最高版本）
  本程序支持的最高版本：7
  程序版本：0.1.0
  数据库文件：/opt/boranlib/data/shared.db
  备份目录：/opt/boranlib/data/backups
  本次已备份到：/opt/boranlib/data/backups/20261103T194459Z

说明：数据库已被更新版本的程序写入，本版本无法安全操作它（继续启动可能损坏数据）。
下一步：
  1. 升级到支持 version 8 或更高版本的程序（当前 0.1.0 最高支持 7）；
  2. 或从备份回滚：停止服务后，用 backups/<时间戳>/shared.db 覆盖回 /opt/boranlib/data/shared.db（升级前的快照都在 /opt/boranlib/data/backups）。
```

- “本程序支持的最高版本”= 二进制内嵌迁移集的最高版本；“程序版本”= `CARGO_PKG_VERSION`（`x.y.z`）。
- 同一版本号但**校验和不同**（`VersionMismatch(N)`）会单独解释为“迁移文件被改过/临时构建写入过”，
  同样给出升级或回滚建议，不再输出裸枚举。
- **换行血缘自愈**（默认开启，`BORANLIB_MIGRATION_REPAIR=off` 关闭）：同一份库先后被 Windows（CRLF）与
  Linux/CI（LF）构建写过时，`_sqlx_migrations` 里的校验和与新二进制内嵌的 LF 字节只差换行。
  启动时对已应用（`success=1`）的记录做保守比对：
  1. 库中校验和 == 当前内嵌字节（LF）的 SHA-384 → 不动；
  2. 库中校验和 == “当前内嵌字节转 CRLF”的 SHA-384 → 判定为仅换行差异，**先做迁移前备份**，
     再单事务 `UPDATE _sqlx_migrations SET checksum = <LF 哈希> WHERE version = ? AND success = 1`；
  3. 其它情况一律不放行（真内容差异仍然拒绝启动，绝不掩盖）。
  归一化前后都会跑 `PRAGMA integrity_check`，失败即中止；每条会记日志
  `migration vN: 仅换行差异，已归一化校验和（CRLF→LF）`，成功后汇总 `本次归一化 N 条迁移记录`。
  如果归一化后仍有不一致，错误里会明确写“已尝试‘换行归一化’，但第 N 条的内容仍与程序内嵌文件不一致”，与普通内容差异区分。
- 其他迁移错误按 SQLite 错误码分类成人话：磁盘满（13）、无写权限（READONLY/PERM）、I/O 错误（10）、
  被占用（BUSY/LOCKED）、文件损坏/非 SQLite（CORRUPT/NOTADB）、脚本语法或约束错误（1/19）。

### 11.3 从备份恢复

先停服务，再覆盖数据库（**同时删掉旧的 `-wal`/`-shm`，否则 SQLite 可能把旧日志回放到新库**）：

```bash
systemctl stop boranlib            # 或直接结束进程
cd /opt/boranlib/data
ls backups/                        # 选一份升级前的时间戳
cp backups/20261103T194459Z/shared.db shared.db
rm -f shared.db-wal shared.db-shm
cp backups/20261103T194459Z/users/*.db users/   # 如有用户库
systemctl start boranlib
```

Windows 调试环境同理：覆盖 `data\shared.db` 前先确认进程已退出，并删除 `shared.db-wal` / `shared.db-shm`。

### 11.4 失败时的重启退避

- 启动失败会在 `$BORANLIB_DATA_DIR/.startup_failure` 写标记（含时间与原因）；下一次启动若距上次失败不足
  **20 秒**，会先等到满 20 秒再尝试，避免 systemd `Restart=always` 在 25 秒内重启上百次。
- 启动成功（迁移通过）会自动删除该标记。
- 建议 unit 文件同时配置（双保险）：

```ini
Restart=always
RestartSec=10
StartLimitIntervalSec=300
StartLimitBurst=5
```

- 退出码保持非 0（`1`），`systemctl status` / `journalctl -u boranlib` 能直接看到上面的多行说明；
  `journalctl` 里同一条错误会同时出现在 tracing 日志与 `Error:` 行。

