#Requires -Version 5.1
<#
.SYNOPSIS
  OpenStick 一键部署：交叉编译 → 打包 → 上传 → 备份 → 安装 → 重启 → 健康检查 → 失败自动回滚。

.DESCRIPTION
  默认流程（全部在本机执行，设备上不编译）：
    1. scripts\build-release.ps1 交叉编译 aarch64-unknown-linux-musl 静态二进制
    2. server\web 下 npm run build 生成 dist
    3. 打包二进制 + dist 到临时目录并计算 SHA256
    4. 单次 SSH 探测（失败立即停止，不重试、不扫网段）
    5. scp 上传，远端脚本先备份旧二进制/dist，再原子替换
    6. systemctl restart 并轮询 /healthz
    7. 健康检查失败：自动恢复旧二进制/dist 并重启，打印 journalctl，退出码非 0

  凭据不写入仓库：使用本机 ssh-agent / ~/.ssh/config，或显式传 -SshKey。
  设备网络不可达时加 -DryRun：只构建打包并打印将执行的命令，不连接设备。

.PARAMETER Device
  设备地址：IP 或 user@host。例如 192.168.1.191；省略时读环境变量 OREAD_DEVICE。

.PARAMETER SshKey
  可选私钥路径；默认使用 ssh-agent / ~/.ssh/config。

.PARAMETER Binary
  可选：直接指定已构建好的 aarch64 静态二进制，跳过编译。

.PARAMETER TargetDir
  交叉编译输出目录；默认 D:\tools\cargo-target-musl（与 build-release.ps1 一致）。

.PARAMETER SkipBuild
  跳过 cargo 交叉编译（要求 -Binary 或 target 目录里已有产物）。

.PARAMETER SkipWebBuild
  跳过 npm run build，直接使用现有 server\web\dist。

.PARAMETER DryRun
  只准备产物、打印步骤与哈希，不进行任何 SSH/SCP。

.PARAMETER RemoteRoot
  设备安装目录，默认 /opt/boranlib。

.PARAMETER Service
  systemd 服务名，默认 boranlib。

.EXAMPLE
  .\scripts\deploy.ps1 -Device 192.168.1.191

.EXAMPLE
  .\scripts\deploy.ps1 -Device root@192.168.1.191 -SshKey $env:USERPROFILE\.ssh\id_ed25519

.EXAMPLE
  .\scripts\deploy.ps1 -Device 192.168.1.191 -SkipBuild -SkipWebBuild -DryRun
#>
param(
    [string]$Device = $(if ($env:OREAD_DEVICE) { $env:OREAD_DEVICE } else { '' }),
    [string]$User = 'root',
    [int]$Port = 22,
    [string]$SshKey,
    [string]$Binary,
    [string]$TargetDir = $(if ($env:OREAD_TARGET_MUSL) { $env:OREAD_TARGET_MUSL } else { 'D:\tools\cargo-target-musl' }),
    [switch]$SkipBuild,
    [switch]$SkipWebBuild,
    [switch]$DryRun,
    [string]$RemoteRoot = '/opt/boranlib',
    [string]$Service = 'boranlib',
    [string]$HealthUrl = 'http://127.0.0.1:3000/healthz',
    [int]$ConnectTimeoutSec = 8,
    [int]$HealthTimeoutSec = 60
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$serverDir = Join-Path $repoRoot 'server'
$webDir = Join-Path $serverDir 'web'
$distDir = Join-Path $webDir 'dist'
$stamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')

function Fail([string]$message, [string[]]$hints = @()) {
    Write-Host ''
    Write-Host "[deploy] 错误：$message" -ForegroundColor Red
    foreach ($h in $hints) { Write-Host "  -> $h" -ForegroundColor Yellow }
    Write-Host ''
    exit 1
}

function Info([string]$message) { Write-Host "[deploy] $message" -ForegroundColor Cyan }
function Ok([string]$message) { Write-Host "[deploy] $message" -ForegroundColor Green }

function Invoke-External([string]$exe, [string[]]$arguments, [string]$what) {
    & $exe @arguments
    if ($LASTEXITCODE -ne 0) {
        Fail "$what 失败（exit=$LASTEXITCODE）：$exe $($arguments -join ' ')"
    }
}

if (-not (Test-Path (Join-Path $serverDir 'Cargo.toml'))) {
    Fail "找不到 $serverDir\Cargo.toml" @('确认脚本位于 <repo>\scripts 目录')
}

if (-not $Device -and -not $DryRun) {
    Fail '没有指定设备地址' @(
        '用 -Device <ip> 或设置环境变量 OREAD_DEVICE=<ip>',
        '设备不在同一网络时先加 -DryRun 只做本机构建打包'
    )
}

$target = if (-not $Device) {
    '<device>'
} elseif ($Device.Contains('@')) {
    $Device
} else {
    "${User}@${Device}"
}

# ---------------------------------------------------------------------------
# 1. 构建
# ---------------------------------------------------------------------------
if (-not $SkipBuild) {
    Info "交叉编译 aarch64-unknown-linux-musl（fat LTO，可能数分钟）…"
    $LASTEXITCODE = 0
    & (Join-Path $PSScriptRoot 'build-release.ps1') -TargetDir $TargetDir
    if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) {
        Fail '交叉编译失败' @('先单独运行 .\scripts\build-release.ps1 查看完整报错')
    }
} else {
    Info '跳过编译（-SkipBuild）'
}

$binaryPath = if ($Binary) {
    $Binary
} else {
    Join-Path $TargetDir 'aarch64-unknown-linux-musl\release\boranlib'
}
if (-not (Test-Path $binaryPath)) {
    Fail "找不到 aarch64 二进制：$binaryPath" @(
        '去掉 -SkipBuild 重新编译',
        '或用 -Binary 指向现有产物'
    )
}

if (-not $SkipWebBuild) {
    Info '构建前端 dist…'
    Push-Location $webDir
    try {
        if (-not (Test-Path (Join-Path $webDir 'node_modules'))) {
            Invoke-External 'npm' @('ci') 'npm ci'
        }
        Invoke-External 'npm' @('run', 'build') 'npm run build'
    } finally {
        Pop-Location
    }
} else {
    Info '跳过前端构建（-SkipWebBuild）'
}

$indexHtml = Join-Path $distDir 'index.html'
if (-not (Test-Path $indexHtml)) {
    Fail "找不到前端产物：$indexHtml" @('去掉 -SkipWebBuild，或先执行 npm --prefix server/web run build')
}

# ---------------------------------------------------------------------------
# 2. 打包与校验
# ---------------------------------------------------------------------------
$stageDir = Join-Path $env:TEMP "boranlib-deploy\$stamp"
New-Item -ItemType Directory -Path $stageDir -Force | Out-Null
$distTar = Join-Path $stageDir "dist-$stamp.tar.gz"

Info "打包前端 dist（保留顶层 dist/ 目录）…"
Invoke-External 'tar' @('-czf', $distTar, '-C', $webDir, 'dist') 'tar 打包前端'

$binHash = (Get-FileHash $binaryPath -Algorithm SHA256).Hash.ToLower()
$distHash = (Get-FileHash $distTar -Algorithm SHA256).Hash.ToLower()
$indexHash = (Get-FileHash $indexHtml -Algorithm SHA256).Hash.ToLower()

Write-Host ''
Write-Host '  二进制: ' -NoNewline; Write-Host $binaryPath
Write-Host '    SHA256: ' -NoNewline; Write-Host $binHash
Write-Host '  前端包: ' -NoNewline; Write-Host $distTar
Write-Host '    SHA256: ' -NoNewline; Write-Host $distHash
Write-Host '  index.html SHA256: ' -NoNewline; Write-Host $indexHash
Write-Host ''

if ($DryRun) {
    Write-Host '（DryRun：远端脚本仍会生成到上面的暂存目录，可用 bash -n 离线校验）'
    Write-Host ''
}

# ---------------------------------------------------------------------------
# 3. 生成远端脚本
# ---------------------------------------------------------------------------
$remoteScriptLocal = Join-Path $stageDir "deploy-$stamp.sh"
$remoteBin = "/tmp/boranlib-$stamp.bin"
$remoteDist = "/tmp/dist-$stamp.tar.gz"
$remoteScript = "/tmp/deploy-$stamp.sh"

$remoteBody = @'
#!/usr/bin/env bash
# 由 scripts/deploy.ps1 生成；参数顺序固定。
set -uo pipefail
ROOT="$1"; SERVICE="$2"; HEALTH_URL="$3"; STAMP="$4"; BIN_TMP="$5"; DIST_TMP="$6"; HEALTH_TIMEOUT="$7"
HEALTH_ITER=$(( HEALTH_TIMEOUT * 10 / 3 ))
[ "$HEALTH_ITER" -lt 10 ] && HEALTH_ITER=10

log() { printf '[remote] %s\n' "$*"; }

backup_existing() {
  if [ -f "$ROOT/boranlib" ]; then
    cp -a "$ROOT/boranlib" "$ROOT/boranlib.bak.$STAMP"
    log "已备份旧二进制：$ROOT/boranlib.bak.$STAMP"
  fi
  if [ -d "$ROOT/web/dist" ]; then
    tar czf "$ROOT/web/dist.bak.$STAMP.tar.gz" -C "$ROOT/web" dist
    log "已备份旧前端：$ROOT/web/dist.bak.$STAMP.tar.gz"
  fi
}

wait_health() {
  local i code
  for i in $(seq 1 "$HEALTH_ITER"); do
    code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "$HEALTH_URL" 2>/dev/null || true)
    [ "$code" = "200" ] && return 0
    sleep 0.3
  done
  return 1
}

restore() {
  if [ -f "$ROOT/boranlib.bak.$STAMP" ]; then
    cp -a "$ROOT/boranlib.bak.$STAMP" "$ROOT/boranlib"
    chmod 755 "$ROOT/boranlib"
    log "已恢复旧二进制"
  fi
  if [ -f "$ROOT/web/dist.bak.$STAMP.tar.gz" ]; then
    rm -rf "$ROOT/web/dist"
    mkdir -p "$ROOT/web"
    tar xzf "$ROOT/web/dist.bak.$STAMP.tar.gz" -C "$ROOT/web"
    chown -R root:root "$ROOT/web/dist"
    log "已恢复旧前端"
  fi
  systemctl restart "$SERVICE" 2>/dev/null || true
  log "已重启服务（回滚后）"
}

backup_existing

log "安装二进制：$BIN_TMP -> $ROOT/boranlib"
if ! install -m 755 -o root -g root "$BIN_TMP" "$ROOT/boranlib"; then
  log "安装二进制失败，回滚"
  restore
  exit 1
fi

if [ -f "$DIST_TMP" ]; then
  log "安装前端：$DIST_TMP -> $ROOT/web/dist"
  # dist.tar.gz 内含顶层 dist/，先解压到同级 incoming 目录再整体替换
  rm -rf "$ROOT/web/dist.incoming"
  mkdir -p "$ROOT/web/dist.incoming"
  if ! tar xzf "$DIST_TMP" -C "$ROOT/web/dist.incoming"; then
    log "解压前端失败，回滚"
    restore
    exit 1
  fi
  if [ ! -d "$ROOT/web/dist.incoming/dist" ]; then
    log "前端包结构异常（缺少 dist/），回滚"
    restore
    exit 1
  fi
  chown -R root:root "$ROOT/web/dist.incoming/dist"
  if [ -d "$ROOT/web/dist" ]; then
    rm -rf "$ROOT/web/dist.old.$STAMP"
    mv "$ROOT/web/dist" "$ROOT/web/dist.old.$STAMP"
  fi
  mv "$ROOT/web/dist.incoming/dist" "$ROOT/web/dist"
  rmdir "$ROOT/web/dist.incoming" 2>/dev/null || true
fi

log "重启服务：$SERVICE"
if ! systemctl restart "$SERVICE"; then
  log "systemctl restart 失败，回滚"
  journalctl -u "$SERVICE" -n 50 --no-pager 2>/dev/null || true
  restore
  exit 1
fi

if wait_health; then
  systemctl is-active "$SERVICE" 2>/dev/null || true
  rm -rf "$ROOT/web/dist.old.$STAMP" 2>/dev/null || true
  rm -f "$BIN_TMP" "$DIST_TMP"
  log "healthz 200，部署完成（STAMP=$STAMP）"
  exit 0
fi

log "healthz 未在 ${HEALTH_TIMEOUT}s 内返回 200，开始回滚"
journalctl -u "$SERVICE" -n 50 --no-pager 2>/dev/null || true
restore
if wait_health; then
  log "回滚后 healthz 200"
else
  log "回滚后 healthz 仍失败，请人工介入"
fi
exit 1
'@

# 远端脚本必须是 LF，否则 bash 会把 \r 当作命令的一部分
$remoteBody = $remoteBody -replace "`r`n", "`n"
[System.IO.File]::WriteAllText($remoteScriptLocal, $remoteBody, (New-Object System.Text.UTF8Encoding($false)))
Ok "远端脚本已生成：$remoteScriptLocal"

if ($DryRun) {
    Ok "DryRun 完成，未连接设备。产物目录：$stageDir"
    Write-Host ''
    Write-Host '将执行（示意）：'
    Write-Host "  scp <binary> ${target}:/tmp/boranlib-$stamp.bin"
    Write-Host "  scp <dist.tar.gz> ${target}:/tmp/dist-$stamp.tar.gz"
    Write-Host "  scp <deploy.sh> ${target}:/tmp/deploy-$stamp.sh"
    Write-Host "  ssh ${target} 'bash /tmp/deploy-$stamp.sh $RemoteRoot $Service $HealthUrl $stamp ...'"
    Write-Host ''
    exit 0
}

# ---------------------------------------------------------------------------
# 4. SSH 预检（只探测一次）与上传
# ---------------------------------------------------------------------------
foreach ($tool in @('ssh', 'scp')) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        Fail "PATH 中找不到 $tool" @('安装 Windows OpenSSH 客户端（设置 → 应用 → 可选功能）')
    }
}

$commonArgs = @('-o', "ConnectTimeout=$ConnectTimeoutSec", '-o', 'ServerAliveInterval=15')
if ($SshKey) {
    if (-not (Test-Path $SshKey)) { Fail "SSH 私钥不存在：$SshKey" }
    $commonArgs += @('-i', $SshKey)
}
$sshArgs = @() + $commonArgs + @('-p', "$Port")
$scpArgs = @() + $commonArgs + @('-P', "$Port")

Info "SSH 预检（仅一次）：$target"
& ssh @sshArgs $target 'echo ok'
if ($LASTEXITCODE -ne 0) {
    Fail "无法连接 $target" @(
        '设备当前不在同一网络时属正常现象；网络恢复后再执行',
        '不要反复重试、不要扫网段或写 /etc/hosts',
        '检查 IP/端口/密钥：-Device、-Port、-SshKey'
    )
}
Ok 'SSH 预检通过'

Info "上传二进制…"
Invoke-External 'scp' (@() + $scpArgs + @($binaryPath, "${target}:${remoteBin}")) 'scp 上传二进制'

Info '上传前端包…'
Invoke-External 'scp' (@() + $scpArgs + @($distTar, "${target}:${remoteDist}")) 'scp 上传前端包'

Info '上传远端脚本…'
Invoke-External 'scp' (@() + $scpArgs + @($remoteScriptLocal, "${target}:${remoteScript}")) 'scp 上传脚本'

# ---------------------------------------------------------------------------
# 5. 远端执行（备份 + 安装 + 重启 + 健康检查 + 失败回滚）
# ---------------------------------------------------------------------------
Info '开始在设备上安装并重启…'
$remoteCmd = "bash '$remoteScript' '$RemoteRoot' '$Service' '$HealthUrl' '$stamp' '$remoteBin' '$remoteDist' '$HealthTimeoutSec'"
& ssh @sshArgs $target $remoteCmd
if ($LASTEXITCODE -ne 0) {
    Write-Host ''
    Write-Host '[deploy] 部署失败。设备已尝试自动回滚；请查看上面的 remote/journalctl 输出。' -ForegroundColor Red
    Write-Host "[deploy] 手动回滚产物保留在：$RemoteRoot/boranlib.bak.$stamp、$RemoteRoot/web/dist.bak.$stamp.tar.gz" -ForegroundColor Yellow
    exit $LASTEXITCODE
}

Ok '部署完成'
Write-Host ''
Write-Host '建议验收：'
Write-Host "  ssh $target 'systemctl is-active $Service; curl -s -o /dev/null -w \"%{http_code}\\n\" $HealthUrl'"
Write-Host "  ssh $target 'journalctl -u $Service -n 30 --no-pager'"
