#Requires -Version 5.1
<#
.SYNOPSIS
  本机开发构建（Windows，msvc host），产物统一放到 D:\tools\oread-target-win。

.DESCRIPTION
  - 所有源码路径都以「脚本自身位置」推导（仓库根 = scripts 的上一级），不写死盘符。
  - 通过 CARGO_TARGET_DIR 环境变量覆盖输出目录，不改 server/Cargo.toml。
  - 仅做本机调试构建，不产出可部署的 aarch64 二进制（发布构建用 build-release.ps1）。

.PARAMETER Release
  以 release profile 构建（默认 debug）。

.PARAMETER TargetDir
  覆盖 CARGO_TARGET_DIR。默认取环境变量 OREAD_TARGET_WIN，否则 D:\tools\oread-target-win。

.PARAMETER CargoArgs
  透传给 cargo build 的额外参数，例如 -CargoArgs '--bin','boranlib'。

.PARAMETER LogFile
  同时把构建输出写入该文件（默认不写）。

.EXAMPLE
  .\scripts\build-dev.ps1
.EXAMPLE
  .\scripts\build-dev.ps1 -Release -LogFile D:\tools\oread-target-win\dev-release.log
#>
param(
    [switch]$Release,
    [string]$TargetDir = $(if ($env:OREAD_TARGET_WIN) { $env:OREAD_TARGET_WIN } else { 'D:\tools\oread-target-win' }),
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
