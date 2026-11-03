#Requires -Version 5.1
<#
.SYNOPSIS
  aarch64-unknown-linux-musl 发布构建（Zig 作为 linker，静态链接，供 OpenStick 使用）。

.DESCRIPTION
  - 源码路径全部由脚本自身位置推导；输出目录统一为 D:\tools\cargo-target-musl。
  - 通过 CARGO_TARGET_DIR / CARGO_PROFILE_RELEASE_LTO 等环境变量覆盖，不改 Cargo.toml。
  - Cargo.toml 里 [profile.release] 是 lto=true（fat LTO，最小体积但最慢）；
    加 -ThinLto 会用 CARGO_PROFILE_RELEASE_LTO=thin + codegen-units=16 覆盖，构建更快、体积略大。
  - 依赖：rustup target aarch64-unknown-linux-musl、zig、cargo-zigbuild。

.PARAMETER ThinLto
  用 thin LTO（并放宽 codegen-units）加快构建；只影响本次进程，不改仓库配置。

.PARAMETER TargetDir
  覆盖 CARGO_TARGET_DIR。默认取环境变量 OREAD_TARGET_MUSL，否则 D:\tools\cargo-target-musl。

.PARAMETER RustupMirror
  rustup 发行源。默认 USTC（实测最快）；可用 -RustupMirror 换成 tuna/aliyun。

.PARAMETER CargoArgs
  透传给 cargo zigbuild 的额外参数。

.PARAMETER LogFile
  同时把构建输出写入该文件（默认不写）。

.EXAMPLE
  .\scripts\build-release.ps1
.EXAMPLE
  .\scripts\build-release.ps1 -ThinLto
#>
param(
    [switch]$ThinLto,
    [string]$TargetDir = $(if ($env:OREAD_TARGET_MUSL) { $env:OREAD_TARGET_MUSL } else { 'D:\tools\cargo-target-musl' }),
    [string]$RustupMirror = 'https://mirrors.ustc.edu.cn/rust-static',
    [string[]]$CargoArgs,
    [string]$LogFile
)

$ErrorActionPreference = 'Continue'
$TargetTriple = 'aarch64-unknown-linux-musl'
$repoRoot  = Split-Path -Parent $PSScriptRoot
$serverDir = Join-Path $repoRoot 'server'

function Fail([string]$message, [string[]]$hints) {
    Write-Host ''
    Write-Host "[build-release] 错误：$message" -ForegroundColor Red
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

Write-Host "[build-release] 仓库根：$repoRoot"

if (-not (Test-Path (Join-Path $serverDir 'Cargo.toml'))) {
    Fail "找不到 $serverDir\Cargo.toml（脚本必须放在 <repo>\scripts 下）" @(
        '确认脚本位于 scripts/ 目录，而不是被复制到别处'
    )
}

foreach ($tool in @('cargo', 'rustc', 'rustup')) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        Fail "PATH 中找不到 $tool" @(
            '安装 rustup 与 stable 工具链：https://rustup.rs/',
            '装好后重开终端再试'
        )
    }
}

# rustup 发行源只对本次进程生效，不污染用户环境变量
$env:RUSTUP_DIST_SERVER = $RustupMirror
Write-Host "[build-release] RUSTUP_DIST_SERVER=$RustupMirror（仅本进程）"

$installedTargets = & rustup target list --installed 2>$null
if ($installedTargets -notcontains $TargetTriple) {
    Write-Host "[build-release] 未安装目标 $TargetTriple，正在添加（首次需要下载）…" -ForegroundColor Yellow
    & rustup target add $TargetTriple
    if ($LASTEXITCODE -ne 0) {
        Fail "rustup target add $TargetTriple 失败（exit=$LASTEXITCODE）" @(
            '检查网络/镜像；本机可改用 tuna：-RustupMirror https://mirrors.tuna.tsinghua.edu.cn/rustup',
            '若是 .partial 卡住，关闭残留 rustup 进程后删除 RUSTUP_HOME\tmp 与 downloads 再重试'
        )
    }
}

# --- 找 zig ---
function Find-Zig {
    if ($env:OREAD_ZIG -and (Test-Path $env:OREAD_ZIG)) { return $env:OREAD_ZIG }
    $cmd = Get-Command zig -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $roots = @('D:\tools\zig', (Join-Path $env:LOCALAPPDATA 'zig'))
    $found = @()
    foreach ($root in $roots) {
        if (Test-Path $root) {
            $found += Get-ChildItem -Path $root -Filter 'zig.exe' -File -Recurse -Depth 2 -ErrorAction SilentlyContinue
        }
    }
    if ($found.Count -gt 0) {
        return ($found | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
    }
    return $null
}

$zigPath = Find-Zig
if (-not $zigPath) {
    Fail '找不到 zig.exe（cargo-zigbuild 需要它作为 musl 的 linker）' @(
        '下载：https://ziglang.org/download/（解压到 D:\tools\zig\<版本>\）',
        '把含 zig.exe 的目录加入 PATH，或设置 OREAD_ZIG 指向 zig.exe 的完整路径',
        '本机已有：D:\tools\zig\zig-x86_64-windows-0.17.0\zig.exe（仅未在 PATH）'
    )
}
$env:PATH = (Split-Path -Parent $zigPath) + ';' + $env:PATH
$zigVersion = (& $zigPath version)
Write-Host "[build-release] zig：$zigPath（$zigVersion）"

$zigbuild = Get-Command cargo-zigbuild -ErrorAction SilentlyContinue
if (-not $zigbuild) {
    Fail '找不到 cargo-zigbuild' @(
        '安装：cargo install cargo-zigbuild（走本机 cargo 镜像，约几分钟）',
        '装好后确认 CARGO_HOME\bin 在 PATH'
    )
}

$env:CARGO_TARGET_DIR = $TargetDir
if (-not (Test-Path $TargetDir)) {
    New-Item -ItemType Directory -Path $TargetDir -Force | Out-Null
}
Write-Host "[build-release] CARGO_TARGET_DIR=$TargetDir"

if ($ThinLto) {
    $env:CARGO_PROFILE_RELEASE_LTO = 'thin'
    $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = '16'
    Write-Host '[build-release] 已启用 thin LTO（环境变量覆盖 profile，不改 Cargo.toml）' -ForegroundColor Cyan
} else {
    Write-Host '[build-release] 使用 Cargo.toml 的 release profile（lto=true / codegen-units=1，最慢但最小）'
}

$cargoArgsList = @('zigbuild', '--target', $TargetTriple, '--release')
if ($CargoArgs) { $cargoArgsList += $CargoArgs }

if ($LogFile -and -not (Test-Writable $LogFile)) {
    Write-Host "[build-release] 警告：$LogFile 不可写（权限/杀软拦截），本次改为仅输出到控制台，构建继续" -ForegroundColor Yellow
    $LogFile = $null
}

Write-Host "[build-release] 执行：cargo $($cargoArgsList -join ' ')" -ForegroundColor Cyan
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
    Fail "cargo zigbuild 失败（exit=$LASTEXITCODE）" @(
        '常见原因见 docs/BUILD.md 的“常见报错”一节',
        'zig 与 cargo-zigbuild 版本不兼容时，换成已知搭配再试（见 BUILD.md）',
        '若报 linker 相关错误，确认 zig 与 cargo-zigbuild 都在 PATH/可执行'
    )
}

$artifact = Join-Path (Join-Path (Join-Path $TargetDir $TargetTriple) 'release') 'boranlib'
$elapsed = [math]::Round(((Get-Date) - $started).TotalSeconds, 1)
Write-Host ''
if (Test-Path $artifact) {
    $mb = [math]::Round((Get-Item $artifact).Length / 1MB, 2)
    Write-Host "[build-release] 完成，用时 ${elapsed}s" -ForegroundColor Green
    Write-Host "[build-release] 产物：$artifact（$mb MB，静态 musl）"
} else {
    Write-Host "[build-release] 构建命令已返回 0，但未找到 $artifact" -ForegroundColor Yellow
    Write-Host '                 检查 [[bin]] name 是否为 boranlib。'
}
