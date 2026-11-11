# OpenStick 一键部署

> 适用：Windows 本机 → OpenStick（Debian 11 / aarch64 / systemd）。
> 脚本：`scripts\deploy.ps1`（PowerShell 5.1+）。仓库内不写入设备地址与任何凭据，
> 地址用 `-Device` 或 `OREAD_DEVICE` 传入，认证走 ssh-agent / `~/.ssh/config` / `-SshKey`。
> 正式发版仍以 `docs/RELEASE.md` 和 `RUNBOOK.md` 为准；本脚本是“构建 + 上传 + 安装 + 回滚”的自动化封装。

## 0. 一条命令

```powershell
# 在仓库根目录执行
.\scripts\deploy.ps1 -Device 192.168.1.191
```

脚本会依次完成：

1. `scripts\build-release.ps1` 交叉编译 `aarch64-unknown-linux-musl`（Zig linker，静态链接）
2. `npm --prefix server/web run build` 生成 `server\web\dist`
3. 打包 `dist/` 并计算二进制、前端包、`index.html` 的 SHA-256
4. **单次** SSH 探测（失败立即停止：不重试、不扫网段）
5. `scp` 上传到安装目录（`/opt/boranlib/.upload-<stamp>.*`），远端先核对 SHA-256；通过后备份旧二进制 / 旧 `dist`
6. 新二进制先写 `boranlib.new.<stamp>` 再 `mv` 原子替换（避免覆盖运行中文件的 `Text file busy`）
7. `systemctl restart boranlib`，轮询 `http://127.0.0.1:3000/healthz`
8. 健康检查失败：先停服务再恢复旧二进制与旧 `dist` 并重启，打印 `journalctl`，退出码非 0

设备地址也可以放进环境变量，避免每次输入：

```powershell
$env:OREAD_DEVICE = '192.168.1.191'
.\scripts\deploy.ps1
```

需要非 root 用户、非默认端口或指定私钥时：

```powershell
.\scripts\deploy.ps1 -Device 192.168.1.191 -User admin -Port 2222 `
    -SshKey "$env:USERPROFILE\.ssh\id_ed25519"
```

### 认证方式

- 默认用户 `root`；`-Device` 写成 `user@host` 或传 `-User` 可覆盖。
- **推荐密钥登录**：本机 `~/.ssh/config` 建 Host 条目（`IdentityFile` 指向 OpenStick 私钥），
  或每次传 `-SshKey`。仓库不保存任何私钥。
- 用密码登录也能部署，但脚本会建立多次连接（1 次探测 + 3 次上传 + 1 次执行），
  密码模式下可能提示多次；密钥模式无提示。

## 1. 设备不可达时：离线预演

设备换网络、当前连不上时，用 `-DryRun` 只做本机构建、打包与校验：

```powershell
.\scripts\deploy.ps1 -DryRun                              # 完整构建 + 打包
.\scripts\deploy.ps1 -SkipBuild -SkipWebBuild -DryRun     # 复用已有产物，最快
```

- 不会执行任何 `ssh` / `scp`。
- 产物在 `%TEMP%\boranlib-deploy\<UTC时间戳>\`：`dist-<stamp>.tar.gz`、`deploy-<stamp>.sh`。
- 可用 Git Bash / WSL 离线检查远端脚本语法（脚本生成的远端脚本是纯 LF）：

```powershell
& 'C:\Program Files\Git\bin\bash.exe' -n "$env:TEMP\boranlib-deploy\<stamp>\deploy-<stamp>.sh"
```

## 2. 参数

| 参数 | 默认值 | 说明 |
|---|---|---|
| `-Device` | `$env:OREAD_DEVICE` | 设备 IP 或 `user@host`；`-DryRun` 时可为空 |
| `-User` | `root` | `-Device` 不含 `@` 时拼接为 `user@host` |
| `-Port` | `22` | SSH 端口 |
| `-SshKey` | 无 | 私钥路径；默认走 ssh-agent / `~/.ssh/config` |
| `-Binary` | 交叉编译产物 | 直接指定现成的 aarch64 静态二进制（常用于部署已核对过的 Release 产物） |
| `-TargetDir` | `D:\tools\cargo-target-musl` | 交叉编译输出目录（兼容 `OREAD_TARGET_MUSL`） |
| `-SkipBuild` | 关 | 跳过 cargo 交叉编译 |
| `-SkipWebBuild` | 关 | 跳过 `npm run build`，直接用现有 `server\web\dist` |
| `-DryRun` | 关 | 只构建打包，不连接设备 |
| `-RemoteRoot` | `/opt/boranlib` | 设备安装目录 |
| `-Service` | `boranlib` | systemd 服务名 |
| `-HealthUrl` | `http://127.0.0.1:3000/healthz` | 远端健康检查地址 |
| `-ConnectTimeoutSec` | `8` | SSH 连接超时 |
| `-HealthTimeoutSec` | `60` | 重启后等待 healthz 200 的秒数 |

## 3. 设备侧布局

与 `RUNBOOK.md` 一致，脚本只动这三处：

```
/opt/boranlib/boranlib                       # 静态二进制（755 root:root）
/opt/boranlib/web/dist/                      # 前端静态文件
/opt/boranlib/data/                          # 数据库，脚本不读写
```

不改 `boranlib.env`、不重装系统、不动 nginx 配置、不改网络接口。

### 3.1 反向代理 gzip（手动，部署脚本不覆盖）

设备上的 nginx 站点配置（如 `/etc/nginx/sites-available/boranlib`）建议开启 gzip。
应用本身只给 `/k` 挂压缩层，网页端的 JS/CSS/JSON 由 nginx 压缩；Funnel 或慢链路下收益最大：

```nginx
gzip on;
gzip_vary on;
gzip_proxied any;
gzip_comp_level 5;
gzip_min_length 1024;
gzip_types text/plain text/css text/javascript application/javascript
           application/json application/xml text/xml image/svg+xml application/manifest+json;
```

改完先 `nginx -t`，再 `systemctl reload nginx`。验收：

```bash
curl -sI -H 'Accept-Encoding: gzip' http://127.0.0.1:8080/assets/<主 JS> \
  | grep -Ei 'content-encoding|vary|content-length'
```

应看到 `Content-Encoding: gzip` 与 `Vary: Accept-Encoding`。设备实测：
主 JS 149KB → 49.8KB、Reader chunk 99.9KB → 37.9KB、Reader CSS 21.5KB → 3.5KB。

## 4. 回滚

### 自动回滚

以下任一情况会触发，脚本会恢复旧二进制 / 旧 `dist` 并重启：

- 二进制安装失败
- 前端包结构异常或解压失败
- `systemctl restart` 失败
- `HealthTimeoutSec` 内 healthz 未返回 200

自动回滚后仍会再轮询一次 healthz；若仍失败，日志明确提示“请人工介入”。
回滚前会先 `systemctl stop`，避免覆盖运行中的二进制导致 `Text file busy`。
脚本以远端输出的 `REMOTE_RESULT=ok` 作为成功标记，不只依赖 SSH 退出码。

### 手动回滚

```bash
TS=<部署输出的 STAMP>
systemctl stop boranlib
cp -a /opt/boranlib/boranlib.bak.$TS /opt/boranlib/boranlib
rm -rf /opt/boranlib/web/dist
tar xzf /opt/boranlib/web/dist.bak.$TS.tar.gz -C /opt/boranlib/web
chown -R root:root /opt/boranlib/web/dist
systemctl start boranlib
journalctl -u boranlib -n 50 --no-pager
```

### 数据库不在自动回滚范围内

脚本只回滚程序与前端。**跨迁移版本升级前**，按 `RUNBOOK.md` 第 3 步手工备份
`/opt/boranlib/data/shared.db` 与 `users/*.db`；若新版本已执行迁移、必须回退数据库，
用那份备份覆盖后再启动。

## 5. 部署后验收

```bash
systemctl is-active boranlib
systemctl is-enabled boranlib
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:3000/healthz
journalctl -u boranlib -n 50 --no-pager
sha256sum /opt/boranlib/boranlib
sha256sum /opt/boranlib/web/dist/index.html
```

- 二进制 / `index.html` 的 SHA-256 应与脚本输出一致。
- `healthz` 连续多次 200（RUNBOOK 第 6 步的 30 次脚本可直接复用）。

### 本次升级（迁移 `0012`）的额外检查

`0012_repair_budget_truncation_poison.sql` 会把“最后错误为全局搜索预算耗尽”的隔离源
降回 `observing`。设备若装有 `sqlite3`：

```bash
sqlite3 /opt/boranlib/data/shared.db \
  "select version, description from _sqlx_migrations order by version desc limit 3;"

sqlite3 /opt/boranlib/data/shared.db \
  "select count(*) from source_health where tier='quarantined' and last_error like '全局搜索预算耗尽%';"
```

- 第一句应能看到 `12|repair budget truncation poison`。
- 第二句应为 `0`。

然后做一次真实搜索（登录后，两段式接口）：

```bash
# 1) 建任务，记下返回的 job_id（需要登录 Cookie / Bearer token）
curl -s 'http://127.0.0.1:3000/api/search/start?key=%E7%99%BE%E5%B9%B4%E5%AD%A4%E7%8B%AC'
# 2) 取结果
curl -s 'http://127.0.0.1:3000/api/search/result?job_id=<job_id>'
```

也可以直接用网页端搜索“百年孤独”。

若旧健康数据仍在冷却（未重启过、或想立即重试），管理员可手动重新信任：

```bash
curl -s -X POST http://127.0.0.1:3000/api/sources/health/reset \
  -H 'Content-Type: application/json' \
  -H "Cookie: <管理员会话 Cookie>" \
  -d '{"source_ids":["https://www.kxwx.org"]}'
```

`source_ids` 省略或传空数组表示重置全部。该接口需要管理员权限。

## 6. 常见问题

| 现象 | 原因 / 处理 |
|---|---|
| `无法连接 <device>` | 设备不在同一网络。属正常现象：停止流程，不要反复重试、不要扫网段。网络恢复后再执行 |
| `PATH 中找不到 ssh/scp` | 安装 Windows OpenSSH 客户端（设置 → 应用 → 可选功能） |
| `npm run build` 失败 | 先 `npm --prefix server/web ci`；或已有可用 `dist` 时加 `-SkipWebBuild` |
| `找不到 aarch64 二进制` | 去掉 `-SkipBuild` 重新编译，或用 `-Binary` 指向现成产物 |
| healthz 一直非 200 | 看脚本打印的 `journalctl`；常见为迁移失败（`VersionMismatch`）、端口占用、`boranlib.env` 配置（弱口令 / JWT secret） |
| 前端 404 / 白屏 | 检查 `/opt/boranlib/web/dist/index.html` 是否存在、nginx root 是否指向 `web/dist` |
| 上传很慢 | 静态二进制约 10 MB，属正常；避免在设备上编译 |
| `上传文件缺失` / `校验失败` | 远端没收到 `/opt/boranlib/.upload-<stamp>.*` 或哈希不符；脚本会在改动任何文件前中止。检查设备磁盘空间、目录权限、网络后重跑 |
| `Text file busy` | 旧版脚本直接覆盖运行中的二进制。新版先写 `boranlib.new.<stamp>` 再 rename，回滚也会先停服务；直接重跑即可 |
| 迁移校验和失败 | 说明产物内嵌迁移换行或内容与库不一致。不要改库绕过，先按 `RUNBOOK.md` 第 2 步闸门核验 |

### 关于“只装 Release 产物”

`RUNBOOK.md` 的默认约束是优先装 GitHub Release 产物。需要严格遵循时，先下载并复算
`SHA256SUMS.txt`，再用 `-SkipBuild -Binary <release>\boranlib-linux-aarch64` 部署。
本脚本默认从当前仓库构建，适合“本地修复后尽快上机验证”；正式发版仍走 tag + CI。

## 7. 收尾

```bash
# 部署成功后可清理旧备份（确认新版本稳定后再删）
rm -f /opt/boranlib/boranlib.bak.*
rm -f /opt/boranlib/web/dist.bak.*.tar.gz
rm -f /opt/boranlib/.upload-*
rm -f /tmp/deploy-*.sh
```

本机暂存目录也可整体删除：`Remove-Item -Recurse "$env:TEMP\boranlib-deploy"`。
