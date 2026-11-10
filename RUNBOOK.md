# OpenStick 换装 Runbook（v0.3.x 系列）

> 目标设备：OpenStick `aarch64` / Debian 11 / systemd 247 / 约 473 MiB 内存
> 传输工具：任意 SSH/SCP 客户端（本文件不含任何凭据）
> 产物目录（Windows）：`<local-tools>\release\<tag>\`
> 当前状态：设备已换到另一个网络，**不可达**。本文件仅为待执行清单，未执行。
>
> 只做常规换装时，可直接用一键脚本 `.\scripts\deploy.ps1 -Device <device>`
> （构建 → 上传 → 备份 → 重启 → healthz → 失败自动回滚）；用法见 `docs/DEPLOY.md`。
> 涉迁移校验和闸门、手工库备份或发版复核时，仍按本文件逐步执行。

## 0. 前置检查（Windows 侧，设备不可达也能做）

1. 下载 Release 产物与清单，**本地复算 SHA256** 并与 `SHA256SUMS.txt` 比对：
   ```powershell
   Get-FileHash <local-tools>\release\<tag>\boranlib-linux-aarch64 -Algorithm SHA256
   Get-Content <local-tools>\release\<tag>\SHA256SUMS.txt
   ```
2. **内嵌迁移换行校验（必做，防 VersionMismatch）**：用 Node 在二进制里按字节搜索 **20 个**迁移文件
   （v0.3.9：shared 11 + user 9），区分 LF / CRLF：
   ```powershell
   node %TEMP%\boranlib-deploy\check_embedded_migrations.js <local-tools>\release\<tag>\boranlib-linux-aarch64
   ```
   - 期望全部 `LF`（`shared/0003` 应为 `len=410`，CRLF 版是 `len=414`）
   - 若出现 `CRLF` → 该产物不能装到既有库（会 `VersionMismatch`）
3. 前端 `server\web\dist` 打包（不要 npm build，直接打现有 dist），并记录 `index.html` 的 SHA-256。

## 1. 单次探测（只允许一次）

```powershell
ssh <device> 'echo ok'
```
- 失败即视为不可达，**停止本次流程**，不要重试、不要扫网段、不要 ping 多次。

## 2. 基线确认 + **迁移校验和闸门（关键）**

```bash
TS=$(date +%Y%m%dT%H%M%SZ); echo "TS=$TS"
sha256sum /opt/boranlib/boranlib; stat -c %s /opt/boranlib/boranlib
systemctl is-active boranlib; systemctl is-enabled boranlib
curl -s -o /dev/null -w '%{http_code}\n' -m 3 http://127.0.0.1:3000/healthz
ls -l /opt/boranlib/data /opt/boranlib/data/users
```

闸门（不可跳过）：把设备 `shared.db`（只读副本，WAL 非空时先 `systemctl stop boranlib` 复制再起）
与 `users/<uid>.db` 拉到本机，用 Node 读出 `_sqlx_migrations` 的 checksum，逐个与产物内嵌迁移比对：

```powershell
node %TEMP%\boranlib-deploy\db_checksum_vs_repo.js   # 按需改 DB 路径
```
- **只要有任何一条 `stored` 与产物的 LF/CRLF 形态不一致 → 不要安装**，先回 Windows 侧报告。
- v0.3.3 内置 `MIGRATION_REPAIR`（默认 `on`）：仅当库中校验和恰好等于"内嵌字节转 CRLF 后"的
  SHA-384 时，才把它改写为内嵌值（先自动备份 + `integrity_check`）；真实内容差异仍会致命中止。
  所以 v1/v2 的 CRLF 血缘在 v0.3.3 首次启动时会被自愈，闸门用来确认差异**只**来自换行。

- 记录 `/api/sources` 数量（登录后）作为回归对照。

## 3. 手工备份库

```bash
TS=<上一步的 TS>
mkdir -p /opt/boranlib/data/manual-bak.$TS
# WAL 非空时先停服务保证快照一致，复制后立刻拉起
for f in /opt/boranlib/data/shared.db /opt/boranlib/data/users/*.db; do
  [ -f "$f" ] && cp -a "$f" "/opt/boranlib/data/manual-bak.$TS/$(basename "$f")"
done
sha256sum /opt/boranlib/data/manual-bak.$TS/*
```
- 禁止直接改库、禁止用清库脚本（`fix-shared-migrations.js --execute` 一律不跑）。

## 4. 安装产物

**物料来自 `<local-tools>\release\v0.3.3\`，sha 已复核**：

```
boranlib-linux-aarch64        9,544,512 B  c1e54498a9d63d906171ea49b24b149117cc47cae0f30ca34d2920e37cdf3f9e
boranlib-linux-x86_64        12,308,200 B  b056185599ed3ba68d1fdfb41e7e3470615553ac9471808f512e85ef194f1d67
boranlib-windows-x86_64.exe  10,087,424 B  49f2ebf4a711d50c607c22f38052a544a157dc52e3163c5414c3a1e13d73fd96
```
三者均与 `SHA256SUMS.txt` 一致；aarch64 静态 ELF（`PT_INTERP=0`、`e_machine=0xB7`）。

**设备当前已知二进制 `04e52d72…` 是 v0.3.1 换装失败回滚遗留**，安装前必须先实测复核，
确认它仍是旧版本、且没有被其它操作替换过。

```bash
sha256sum /tmp/boranlib-<tag>.bin          # 装前复算，必须等于清单
install -m 755 -o root -g root /tmp/boranlib-<tag>.bin /opt/boranlib/boranlib
sha256sum /opt/boranlib/boranlib           # 装后复算
# dist
tar czf /opt/boranlib/web/dist.bak.$TS.tar.gz -C /opt/boranlib/web dist
rm -rf /opt/boranlib/web/dist && mkdir -p /opt/boranlib/web/dist
tar xzf /tmp/dist-<tag>.tar.gz -C /opt/boranlib/web/dist
chown -R root:root /opt/boranlib/web/dist
sha256sum /opt/boranlib/web/dist/index.html   # 与本地记录一致
```

## 5. 重启并计时

```bash
T0=$(date +%s%3N); systemctl restart boranlib
CODE=000
for i in $(seq 1 100); do
  CODE=$(curl -s -o /dev/null -w '%{http_code}' -m 2 http://127.0.0.1:3000/healthz)
  [ "$CODE" = "200" ] && break; sleep 0.2
done
T1=$(date +%s%3N); echo "healthz=$CODE after $((T1-T0))ms iterations=$i"
systemctl is-active boranlib; systemctl is-enabled boranlib
```
- 起不来 → 直接跳到「R. 回滚三件套」，**不要反复重启尝试**。

## 6. healthz 连续 30 次

```bash
ok=0; bad=0
for i in $(seq 1 30); do
  c=$(curl -s -o /dev/null -w '%{http_code}' -m 2 http://127.0.0.1:3000/healthz)
  [ "$c" = "200" ] && ok=$((ok+1)) || bad=$((bad+1)); sleep 0.2
done
echo "OK=$ok BAD=$bad"
```
- 记录：停机时长、首次 200 耗时、新 PID（`systemctl show boranlib -p MainPID`）。

## 7. 六项回归

1. 登录 + `/api/sources` 数量（与步骤 2 对照）。
2. 健康字段：`/api/sources` 返回里应含健康状态字段。
3. `/k`、`/k/toc`、`/k/read` 三页：零 `<script>`、无横向溢出、底栏 64px、阅读页无顶栏。
4. 灰度转码：Kindle UA + `Accept` 不含 webp 请求 `/api/book/image` → `image/jpeg` + `x-image-transcoded: webp->jpeg-gray;q=78`；二次请求 `x-image-cache: HIT`。
5. 真实关键词搜索：耗时、结果数。
6. 资源：空载 `MemoryCurrent`（`systemctl show boranlib -p MemoryCurrent`）、搜索峰值 RSS、日志名。

## 8. 死源冷却验证

```bash
# botaodz 连打 3 次 → 进冷却
# 之后做限定 source_ids 的搜索，响应中应出现 skipped_cooldown，且该源未被请求
# 对照 include_cooled=1 的耗时差异
```

## 9. v0.3.9 设备默认参数建议（512 MB 内存约束）

> **建议值，待真机日志复核。** 设备约 473 MiB 可用，还要同时跑 systemd/journald/sshd，
> 需给页缓存与图片缓存留余量。

v0.3.9 代码默认：`HTTP_CONCURRENCY=16`（硬上限 16）、单源预算 `SEARCH_TIMEOUT_SECS=8`（curated）/
`SEARCH_ALL_TIMEOUT_SECS=8`（all）、整批预算 `SEARCH_CURATED_BUDGET_SECS=6` / `SEARCH_ALL_BUDGET_SECS=40`、
`JS_TIMEOUT_MS=3000`、`KINDLE_VW=1072` / `KINDLE_VH=1268`。
本版 16 并发实测 RSS ≈154 MB，换到 512 MB 设备需下调：

```bash
# /opt/boranlib/boranlib.env（建议值，待真机日志复核）
BORANLIB_HTTP_CONCURRENCY=8          # 512 MB 首推 8；余量宽松可试 12，硬上限 16
BORANLIB_SEARCH_TIMEOUT_SECS=8       # curated 单源预算
BORANLIB_SEARCH_ALL_TIMEOUT_SECS=8   # all 单源预算；不要压到 5s（会截断慢但能用的源）
BORANLIB_SEARCH_CURATED_BUDGET_SECS=6
BORANLIB_SEARCH_ALL_BUDGET_SECS=40
BORANLIB_JS_TIMEOUT_MS=3000
BORANLIB_KINDLE_VW=1072
BORANLIB_KINDLE_VH=1268
```

理由与注意：
- 16 槽 → RSS ≈154 MB，叠加 `IMAGE_CACHE_MB=64` 与系统常驻后接近设备上限；8–12 槽在 147 源聚合下
  首条结果仍约 2.3 s，OOM 余量更大。
- **整批预算必须显式写**：只设 `SEARCH_TIMEOUT_SECS` 时，v0.3.9 按旧语义把两个 scope 的整批预算
  一起回落到 8s（`resolve_search_budgets` 的 legacy 分支），会把 90% 源挡在门外。
- **覆盖能力 ≈ 并发槽 × 整批预算 ÷ 单源预算**：132 个可搜索源、16 槽、单源 8s 需要约 40s
  才能基本跑完。实测 25s 只跑到 60–70 个源、30/35s 仍会漏掉排在 ~44 位的源
  （`8s/40s` 连续 4 次均命中，`5s/25s` 只有 1/4）。整批预算是上限，源少时提前结束。
- 搜索超时只记 `partial`，不会再把源打成隔离；延迟均值只统计成功请求，
  否则超时会把所有源的平均延迟拉到预算值，入队顺序退化。
- 复核口径：`systemctl show boranlib -p MemoryCurrent`、`/healthz` 的 `rss_mb`/`concurrency`/`search` 块，
  以及 147 源聚合搜索的 `source_total`、`attempted_sources` 与「未执行」条数。

## R. 回滚三件套（任一失败立即执行，然后停手）

```bash
TS=<部署时记录的 TS>
cp -a /opt/boranlib/boranlib.bak.$TS /opt/boranlib/boranlib
rm -rf /opt/boranlib/web/dist
tar xzf /opt/boranlib/web/dist.bak.$TS.tar.gz -C /opt/boranlib/web
chown -R root:root /opt/boranlib/web/dist
# 若库已被新版本迁移过且必须回退，再用 manual-bak.$TS 覆盖 shared.db / users/*.db
systemctl restart boranlib
# 轮询 healthz（同步骤 5），记录耗时
journalctl -u boranlib -n 50 --no-pager   # 原样贴出日志
```
- 回滚后报完整日志原文，不再做其它尝试。

## 边界

- 不重装系统、不动其它服务、不改网络接口。
- 不改 `boranlib.env`（除非必须，且先报告）。
- 不把 IP 写进 `/etc/hosts` 作为修复。
- 不 commit / push，不打印凭据。
- 设备只装 GitHub Release 产物或经过本文件核验 / 门禁通过的部署构建（`scripts\deploy.ps1`）；
  未验证的本机临时包仅限本机独立数据目录测试。
- 迁移换行不一致（LF/CRLF）属于产物问题：停用该产物，等重新发布的 LF 版，**不要改库 checksum 绕过**。

## 产物清单（预取留存）

| 产物 | 位置 | 大小 | SHA-256 |
|---|---|---|---|
| **aarch64 v0.3.3** | `<local-tools>\release\v0.3.3\boranlib-linux-aarch64` | 9,544,512 B | `c1e54498a9d63d906171ea49b24b149117cc47cae0f30ca34d2920e37cdf3f9e` |
| **x86_64 v0.3.3** | `<local-tools>\release\v0.3.3\boranlib-linux-x86_64` | 12,308,200 B | `b056185599ed3ba68d1fdfb41e7e3470615553ac9471808f512e85ef194f1d67` |
| **windows v0.3.3** | `<local-tools>\release\v0.3.3\boranlib-windows-x86_64.exe` | 10,087,424 B | `49f2ebf4a711d50c607c22f38052a544a157dc52e3163c5414c3a1e13d73fd96` |
| aarch64 v0.3.1（历史） | `<local-tools>\release\v0.3.1\boranlib-linux-aarch64` | 9,527,352 B | `8f836178d1afd618ec19b178e1394b2c8e6391bad4e71e66df8a9238b1a8eef6` |
| windows v0.3.1（历史） | `<local-tools>\release\v0.3.1\boranlib-windows-x86_64.exe` | 10,056,192 B | `b2727576f2d43eb445f6140af45e7f58a0b0a68f13e52012e1b2f9ab3209ce0f` |


## 附录：迁移换行（VersionMismatch 根因）实测

`sqlx::migrate!` 把迁移文件**字节原样**内嵌进二进制，并按 SHA-384 记录到库里的
`_sqlx_migrations.checksum`；CRLF 与 LF 算出的校验和不同，于是启动即 `VersionMismatch(n)`。

| 对象 | shared 内嵌形态 | 说明 |
|---|---|---|
| `v0.3.1` windows exe | 14/14 全部 **CRLF** | 撞库里 LF 记录的 v3/v4/v5 → 本机 3000 实测 `VersionMismatch(3)` |
| `v0.3.1` linux-aarch64 | 14/14 全部 **LF**（`0003` len=410） | 与仓库 LF 一致，但撞既有库 CRLF 记录的 v1/v2 |
| `v0.3.0` linux-aarch64 | 12 条 LF + 0006/0007 MISSING | 与设备早期 `VersionMismatch(1)` 吻合 |
| `v0.3.3` linux-aarch64 | 14/14 **LF**，含 `MIGRATION_REPAIR`（默认 on） | 可自愈仅换行差异的库校验和；真实内容差异仍中止 |

本机 `shared.db` 实测血缘（`_sqlx_migrations.checksum` 按 LF/CRLF 复算）：
```
v1 CRLF   v2 CRLF   v3 LF   v4 LF   v5 LF
users/1.db: v1 CRLF  v2 CRLF  v3–v7 LF
```
结论：**同一库内 v1/v2 与 v3+ 换行血缘不同**。任何"统一换行"的产物都无法同时满足，
要么重建库校验和（禁止），要么由发布侧提供与库血缘一致的产物。设备侧部署前必须跑第 2 步闸门。

`v0.3.2` 状态：截至本次检查（2026-11-04），Release 列表只有 `v0.3.1 / v0.3.0 / v0.2.0 / v0.1.0`，
**v0.3.2 尚未发布**；发布后按「0. 前置检查」下载、复算 sha256、并跑同一套内嵌换行校验再做结论。

## 附录：v0.3.4 产物（2026-11-04）

Release `v0.3.4`（id `433458472`，run `41761796283`，六个 job 全绿，总耗时 1139s）。附件已下载并本地复算：

| 产物 | 位置 | 大小 | SHA-256 |
|---|---|---|---|
| **aarch64 v0.3.4（设备用）** | `<local-tools>\release\v0.3.4\boranlib-linux-aarch64` | 9,587,680 B | `29c22f67d833d4e93237b0d16cddcb86836b1cdff29574c3df442f4490cc5cfa` |
| x86_64 v0.3.4 | `<local-tools>\release\v0.3.4\boranlib-linux-x86_64` | 12,365,640 B | `f620f4e33a1e23f975f8404f7cfc96e27b1cfd423b056391a519326cc3096d19` |
| windows v0.3.4 | `<local-tools>\release\v0.3.4\boranlib-windows-x86_64.exe` | 10,139,648 B | `cb2a32a799b2645e4aedaa720473923932114750097a0595fe116879bfe741d9` |
| 清单 | `<local-tools>\release\v0.3.4\SHA256SUMS.txt` | 271 B | 三个产物 sha 逐字符一致 |

内嵌迁移：aarch64 与 windows 均 **14/14 LF**（`0003` len=410），`MIGRATION_REPAIR` 与 `仅换行差异` 各命中 1 次；
budget：aarch64 9.14 MiB / x86_64 11.79 MiB / windows 9.67 MiB，全部 PASS。

## 附录：v0.3.9 Linux 静态包（自建，2026-11-05）

v0.3.9（main `a3b3e85b`，tag `v0.3.9`）的 Linux 包在独立 worktree（`<local-tools>\wt-v039-linux`，
分支 `release/v0.3.9-linux`，detached 起点 `a3b3e85b`）用 Zig + cargo-zigbuild 构建，
fat LTO，与 v0.3.5 同一套方式；未动 `<repo>`，未碰本机 `:3000`。

| 产物 | 位置 | 大小 | SHA-256 |
|---|---|---|---|
| **aarch64 v0.3.9（设备用）** | `<local-tools>\openstick\v0.3.9\boranlib-linux-aarch64` | 10,041,440 B | `fefb9fb607a6e39b99043be1a39582189eefe1ebc2ebb96441c02fa7121d53bf` |
| x86_64 v0.3.9 | `<local-tools>\openstick\v0.3.9\boranlib-linux-x86_64` | 11,965,432 B | `287177dc2f4369b9f0e65dab20ad8a17a25d8ed214a287b08cb6cbc8f3522c38` |
| 清单 | `<local-tools>\openstick\v0.3.9\SHA256SUMS.txt` | — | 与上两行逐字符一致 |
| Windows 包（已发布） | `<local-tools>\release-pack\boranlib-v0.3.9-win64.zip` | 5,004,721 B | `d037019b602ea695d94c41bffd5a3867a29dbb95539b3150acdce412bb4f6df3` |

静态核验：两个 ELF 均 `PT_INTERP_COUNT=0`、`ET_EXEC`、`e_machine` aarch64 `0xB7` / x86_64 `0x3E`；
`file` 等价描述为 `ELF 64-bit LSB executable, aarch64|x86_64, statically linked`。
内嵌迁移 **20/20 全 LF**（shared 11 + user 9，`shared/0003` len=410）。

执行冒烟：本机无 qemu、WSL 未安装发行版、设备当前不可达 → **未执行启动冒烟，仅静态核验**。


