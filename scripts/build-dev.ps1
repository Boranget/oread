#Requires -Version 5.1
<#
.SYNOPSIS
  本机开发构建（Windows，msvc host）。默认走 dev 快档，落 D:\tools\cache\target-dev。

.DESCRIPTION
  - 所有源码路径都以「脚本自身位置」推导（仓库根 = scripts 的上一级），不写死盘符。
  - 通过 CARGO_TARGET_DIR 环境变量覆盖输出目录，不改 server/Cargo.toml。
  - 默认 dev 快档：debug=false、本 crate opt-level=0、依赖 opt-level=2（见 server/Cargo.toml）。
  - dev 与 release 使用不同 target 目录，避免切档位时依赖整体重编。
  - 前端只在 server\web 有改动（比 dist\index.html 新）时才跑 npm run build。
  - 仅做本机构建，不产出可部署的 aarch64 二进制（发布构建用 build-release.ps1）。

.PARAMETER Release
  以 release profile 构建（默认 dev 快档）。release 仅用于发版/性能/内存测量。

.PARAMETER TargetDir
  覆盖 CARGO_TARGET_DIR。dev 默认取 OREAD_TARGET_DEV，否则 D:\tools\cache\target-dev；
  release 默认按 HEAD 的 tag 推导 D:\tools\oread-target-<tag>（禁止写运行目录 oread-target-win）。

.PARAMETER CargoArgs
  透传给 cargo build 的额外参数，例如 -CargoArgs '--bin','boranlib'。

.PARAMETER LogFile
  同时把构建输出写入该文件（默认不写）。

.EXAMPLE
  .\scripts\build-dev.ps1
.EXAMPLE
  .\scripts\build-dev.ps1 -Release -LogFile D:\tools\oread-target-v0316\dev-release.log
#>
param(
    [switch]$Release,
    [string]$TargetDir,
    [string[]]$CargoArgs,
    [string]$LogFile
)

$ErrorActionPreference = 'Continue'
$repoRoot  = Split-Path -Parent $PSScriptRoot
$serverDir = Join-Path $repoRoot 'server'

function Fail([string]$message, [string[]]$hints) {
    Write-Host ''
    Write-Host "[build-dev] 错误：$message" -ForegroundColor Red
    foreach ($h in $hints) { Write-Host "  -> $h" -ForegroundColor Yellow }
    Write-Host ''
    exit 1
}

function Test-Writable([string]$path) {
    try {
        $dir = Split-Path -Parent $path
        if ($dir -and -not (Test-Path $dir)) {
            New-Item -ItemType Directory -Path $dir -Force -ErrorAction Stop | Out-Null
        }
        $fs = [System.IO.File]::Open($path, [System.IO.FileMode]::Append, [System.IO.FileAccess]::Write)
        $fs.Close()
        return $true
    } catch {
        return $false
    }
}

# 发布构建禁止写进运行目录：D:\tools\oread-target-win 是本机 :3000 的运行目录，
# 曾因未指定 -TargetDir 把 v0.3.12 的 release 产物直接写进它的 release\boranlib.exe。
$RunDir = 'D:\tools\oread-target-win'
if ($Release) {
    $dirExplicit = $PSBoundParameters.ContainsKey('TargetDir') -or $env:OREAD_TARGET_WIN
    if (-not $dirExplicit) {
        $tag = (& git -C $repoRoot describe --tags --exact-match HEAD 2>$null | Select-Object -First 1)
        $suffix = if ($LASTEXITCODE -eq 0 -and $tag) {
            ($tag.Trim() -replace '\.', '')
        } else {
            'wip-' + (& git -C $repoRoot rev-parse --short HEAD 2>$null | Select-Object -First 1)
        }
        $TargetDir = "D:\tools\oread-target-$suffix"
        Write-Host "[build-dev] release 构建未指定 -TargetDir，改用版本目录 $TargetDir" -ForegroundColor Cyan
    } elseif (-not $PSBoundParameters.ContainsKey('TargetDir')) {
        $TargetDir = $env:OREAD_TARGET_WIN
    }
    if ($TargetDir.TrimEnd('\') -ieq $RunDir) {
        Fail "release 构建不能写进运行目录 $RunDir" @(
            '改用版本目录：.\scripts\build-dev.ps1 -Release -TargetDir D:\tools\oread-target-vX.Y.Z',
            'Windows 包交给 D:\tools\release-pack\make-pack.ps1 产出；Linux 包落 D:\tools\openstick\<version>',
            '运行目录只在人工升级时替换 exe，不要由构建脚本写入'
        )
    }
} elseif (-not $PSBoundParameters.ContainsKey('TargetDir')) {
    # dev 快档与 release 分 target 目录：切档位不必重编依赖
    $TargetDir = if ($env:OREAD_TARGET_DEV) { $env:OREAD_TARGET_DEV }
                 elseif ($env:OREAD_TARGET_WIN) { $env:OREAD_TARGET_WIN }  # 兼容旧变量
                 else { 'D:\tools\cache\target-dev' }
}

$profileName = if ($Release) { 'release（发版/测量慢档）' } else { 'dev（本地快档）' }
Write-Host "[build-dev] 档位：$profileName"

Write-Host "[build-dev] 仓库根：$repoRoot"

if (-not (Test-Path (Join-Path $serverDir 'Cargo.toml'))) {
    Fail "找不到 $serverDir\Cargo.toml（脚本必须放在 <repo>\scripts 下）" @(
        '确认脚本位于 scripts/ 目录，而不是被复制到别处'
    )
}

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    Fail 'PATH 中找不到 cargo' @(
        '安装 Rust（含 rustup）：https://rustup.rs/',
        '装好后重开终端（PATH 变更只对新进程生效）',
        '若用自定义目录，确认 CARGO_HOME\bin 已在 PATH'
    )
}

$rustc = Get-Command rustc -ErrorAction SilentlyContinue
if (-not $rustc) {
    Fail 'PATH 中找不到 rustc' @('cargo 与 rustc 应同目录，检查 rustup 安装是否完整')
}

$hostLine = (& rustc -vV | Select-String -Pattern '^host:').Line
$hostTriple = if ($hostLine) { ($hostLine -replace '^host:\s*', '').Trim() } else { '' }
$rustcVersion = (& rustc -V) -replace '^rustc\s*', ''
Write-Host "[build-dev] 工具链：$rustcVersion（host=$hostTriple）"

if ($hostTriple -and $hostTriple -notlike '*-msvc') {
    Write-Host "[build-dev] 提示：当前 host 为 $hostTriple，不是 msvc。" -ForegroundColor Yellow
    Write-Host '           本脚本按「本机 host 直接构建」执行；若 host 是 gnu，需要 MinGW 的 gcc 在 PATH。' -ForegroundColor Yellow
}

# ---- 前端：只在 server\web 有改动（比 dist\index.html 新）时才重建 ----
$webDir  = Join-Path $serverDir 'web'
$distIdx = Join-Path $webDir 'dist\index.html'

function Test-WebChanged {
    if (-not (Test-Path $distIdx)) { return 'dist 不存在' }
    $distTime = (Get-Item $distIdx).LastWriteTimeUtc
    $watch = @(
        (Join-Path $webDir 'src'),
        (Join-Path $webDir 'index.html'),
        (Join-Path $webDir 'package.json'),
        (Join-Path $webDir 'package-lock.json'),
        (Join-Path $webDir 'vite.config.js'),
        (Join-Path $webDir 'svelte.config.js'),
        (Join-Path $webDir 'jsconfig.json')
    ) | Where-Object { Test-Path $_ }
    $newest = Get-ChildItem -Path $watch -Recurse -File -ErrorAction SilentlyContinue |
              Sort-Object LastWriteTimeUtc | Select-Object -Last 1
    if ($newest -and $newest.LastWriteTimeUtc -gt $distTime) {
        return "源码较新：$($newest.FullName)"
    }
    return $null
}

$webReason = Test-WebChanged
if ($webReason) {
    $npm = Get-Command npm -ErrorAction SilentlyContinue
    if (-not $npm) {
        Fail "server\web 有改动（$webReason），需要重建 dist，但 PATH 中找不到 npm" @(
            '安装 Node.js 后重开终端',
            '后端改动不需要重建 dist；仅前端改动才会触发这一步'
        )
    }
    if (-not (Test-Path (Join-Path $webDir 'node_modules'))) {
        Fail 'server\web 缺少 node_modules，无法构建前端' @(
            'cd server\web; npm ci'
        )
    }
    Write-Host "[build-dev] 前端有改动（$webReason），执行 npm run build" -ForegroundColor Cyan
    Push-Location $webDir
    try {
        & npm run build
    } finally {
        Pop-Location
    }
    if ($LASTEXITCODE -ne 0) {
        Fail "npm run build 失败（exit=$LASTEXITCODE）" @('看上面的 vite/svelte 报错')
    }
} else {
    Write-Host '[build-dev] 前端无改动，跳过 npm run build'
}

$env:CARGO_TARGET_DIR = $TargetDir
Write-Host "[build-dev] CARGO_TARGET_DIR=$TargetDir"
if (-not (Test-Path $TargetDir)) {
    New-Item -ItemType Directory -Path $TargetDir -Force | Out-Null
}

$cargoArgsList = @('build')
if ($Release) { $cargoArgsList += '--release' }
if ($CargoArgs) { $cargoArgsList += $CargoArgs }

if ($LogFile -and -not (Test-Writable $LogFile)) {
    Write-Host "[build-dev] 警告：$LogFile 不可写（权限/杀软拦截），本次改为仅输出到控制台，构建继续" -ForegroundColor Yellow
    $LogFile = $null
}

Write-Host "[build-dev] 执行：cargo $($cargoArgsList -join ' ')" -ForegroundColor Cyan
$started = Get-Date

Push-Location $serverDir
try {
    if ($LogFile) {
        & cargo @cargoArgsList 2>&1 | Tee-Object -FilePath $LogFile
    } else {
        & cargo @cargoArgsList
    }
} finally {
    Pop-Location
}

if ($LASTEXITCODE -ne 0) {
    Fail "cargo build 失败（exit=$LASTEXITCODE）" @(
        '看上面的第一条 error；常见原因见 docs/BUILD.md 的“常见报错”',
        '若提示 Blocking waiting for file lock：关掉其它正在跑 cargo 的进程'
    )
}

$profileDir = if ($Release) { 'release' } else { 'debug' }
$artifact = Join-Path (Join-Path $TargetDir $profileDir) 'boranlib.exe'
$elapsed = [math]::Round(((Get-Date) - $started).TotalSeconds, 1)
Write-Host ''
if (Test-Path $artifact) {
    $mb = [math]::Round((Get-Item $artifact).Length / 1MB, 2)
    Write-Host "[build-dev] 完成，用时 ${elapsed}s" -ForegroundColor Green
    Write-Host "[build-dev] 产物：$artifact（$mb MB）"
} else {
    Write-Host "[build-dev] 构建命令已返回 0，但未找到 $artifact" -ForegroundColor Yellow
    Write-Host '            检查 Cargo.toml 里的 [[bin]] name 是否被改过。'
}
