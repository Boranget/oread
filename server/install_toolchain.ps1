#Requires -Version 5.1
<#
.SYNOPSIS
  一次性/幂等地准备本机交叉编译环境（aarch64-unknown-linux-musl + Zig + cargo-zigbuild）。

.DESCRIPTION
  - 所有路径基于脚本自身位置，不再写死 d:\projects\read 与 D:\tools\rust。
  - rustup 发行源默认 USTC（已实测可用且最快）；旧的「中科大 rust-static 403」结论已过期。
  - 每一步都会检查结果，缺工具时给出可直接照做的提示。
  - 不再自动后台启动编译（历史上会隐藏拉起 run_build.ps1，容易误判构建结果）。
  - 默认不会把默认 host 切成 gnu：本机 msvc host 可用，切 gnu 会带来额外 MinGW 依赖。

.PARAMETER InstallZigBuild
  若缺少 cargo-zigbuild，则真的执行 cargo install cargo-zigbuild（耗时数分钟）。

.PARAMETER GnuHost
  额外安装 stable-x86_64-pc-windows-gnu 工具链与 x86_64-pc-windows-gnu 目标（仅 build_gnu.bat 需要）。
  安装后不会自动设为默认，需要时手工 rustup default stable-x86_64-pc-windows-gnu。

.PARAMETER RustupMirror
  rustup 发行源，默认 https://mirrors.ustc.edu.cn/rust-static 。

.EXAMPLE
  .\install_toolchain.ps1
.EXAMPLE
  .\install_toolchain.ps1 -InstallZigBuild
#>
param(
    [switch]$InstallZigBuild,
    [switch]$GnuHost,
    [string]$RustupMirror = 'https://mirrors.ustc.edu.cn/rust-static'
)

$ErrorActionPreference = 'Continue'
$repoRoot  = Split-Path -Parent $PSScriptRoot
$serverDir = Join-Path $repoRoot 'server'
$target    = 'aarch64-unknown-linux-musl'

function Fail([string]$message, [string[]]$hints) {
    Write-Host ''
    Write-Host "[install] 错误：$message" -ForegroundColor Red
    foreach ($h in $hints) { Write-Host "  -> $h" -ForegroundColor Yellow }
    Write-Host ''
    exit 1
}

function Step([string]$text) {
    Write-Host ''
    Write-Host "[install] $text" -ForegroundColor Cyan
}

Write-Host "[install] 仓库根：$repoRoot"

# ---------- 1. rustup / cargo ----------
Step '1/5 检查 rustup 与 cargo'
foreach ($tool in @('rustup', 'cargo')) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        Fail "PATH 中找不到 $tool" @(
            '安装 rustup：https://rustup.rs/',
            '装好后重开终端再运行本脚本'
        )
    }
}
Write-Host "[install]    rustup=$((Get-Command rustup).Source)"
Write-Host "[install]    cargo=$((Get-Command cargo).Source)"

$env:RUSTUP_DIST_SERVER = $RustupMirror
Write-Host "[install]    RUSTUP_DIST_SERVER=$RustupMirror（仅本进程）"

& rustup show active-toolchain
if ($LASTEXITCODE -ne 0) {
    Write-Host '[install]    当前目录没有可用的工具链覆盖，将使用 rustup 默认工具链' -ForegroundColor Yellow
}
& rustc -vV

# ---------- 2. musl 目标 ----------
Step "2/5 检查交叉编译目标 $target"
$installed = & rustup target list --installed 2>$null
if ($installed -contains $target) {
    Write-Host "[install]    已安装 $target"
} else {
    Write-Host "[install]    未安装 $target，开始添加（首次需下载）…" -ForegroundColor Yellow
    & rustup target add $target
    if ($LASTEXITCODE -ne 0) {
        Fail "rustup target add $target 失败（exit=$LASTEXITCODE）" @(
            '换镜像重试：-RustupMirror https://mirrors.tuna.tsinghua.edu.cn/rustup',
            '若 .partial 卡住：结束 rustup 进程，删除 RUSTUP_HOME\tmp 与 downloads 后重试',
            '检查 RUSTUP_HOME 是否有写权限'
        )
    }
}

# ---------- 3. zig ----------
Step '3/5 检查 zig（cargo-zigbuild 的 linker）'
$zigPath = $null
if ($env:OREAD_ZIG -and (Test-Path $env:OREAD_ZIG)) {
    $zigPath = $env:OREAD_ZIG
} else {
    $cmd = Get-Command zig -ErrorAction SilentlyContinue
    if ($cmd) { $zigPath = $cmd.Source }
}
if (-not $zigPath) {
    $roots = @('D:\tools\zig', (Join-Path $env:LOCALAPPDATA 'zig'))
    $found = @()
    foreach ($root in $roots) {
        if (Test-Path $root) {
            $found += Get-ChildItem -Path $root -Filter 'zig.exe' -File -Recurse -Depth 2 -ErrorAction SilentlyContinue
        }
    }
    if ($found.Count -gt 0) {
        $zigPath = ($found | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
    }
}

if ($zigPath) {
    Write-Host "[install]    zig=$zigPath（$(& $zigPath version)）"
    Write-Host "[install]    提示：zig 不在 PATH 时，build-release.ps1 会自动发现它；其它工具需要时请自行加入 PATH。"
} else {
    Write-Host '[install]    未找到 zig，交叉编译会失败' -ForegroundColor Yellow
    Write-Host '            下载：https://ziglang.org/download/ （Windows x86_64 压缩包）'
    Write-Host '            解压到 D:\tools\zig\<版本>\ ，或设置 OREAD_ZIG 指向 zig.exe'
}

# ---------- 4. cargo-zigbuild ----------
Step '4/5 检查 cargo-zigbuild'
$zigbuild = Get-Command cargo-zigbuild -ErrorAction SilentlyContinue
if ($zigbuild) {
    Write-Host "[install]    cargo-zigbuild=$($zigbuild.Source)"
} elseif ($InstallZigBuild) {
    Write-Host '[install]    未安装，开始 cargo install cargo-zigbuild（会走本机 crates 镜像，耗时几分钟）…' -ForegroundColor Yellow
    & cargo install cargo-zigbuild
    if ($LASTEXITCODE -ne 0) {
        Fail "cargo install cargo-zigbuild 失败（exit=$LASTEXITCODE）" @(
            '确认 crates 镜像可用（见 docs/BUILD.md“镜像策略”一节）',
            '确认 CARGO_HOME\bin 在 PATH，否则装完也找不到'
        )
    }
    Write-Host '[install]    cargo-zigbuild 安装完成'
} else {
    Write-Host '[install]    未安装 cargo-zigbuild' -ForegroundColor Yellow
    Write-Host '            执行：cargo install cargo-zigbuild'
    Write-Host '            或：.\install_toolchain.ps1 -InstallZigBuild'
}

# ---------- 5. 可选 gnu ----------
Step '5/5 可选：x86_64-pc-windows-gnu（仅 build_gnu.bat 需要）'
if (-not $GnuHost) {
    Write-Host '[install]    跳过（加 -GnuHost 才会安装 gnu host 工具链与 gnu 目标）'
} else {
    $gnuTargets = & rustup target list --installed 2>$null
    if ($gnuTargets -contains 'x86_64-pc-windows-gnu') {
        Write-Host '[install]    当前活跃工具链已有 x86_64-pc-windows-gnu 目标'
    } else {
        Write-Host '[install]    为当前活跃工具链添加 gnu 目标…' -ForegroundColor Yellow
        & rustup target add x86_64-pc-windows-gnu
        if ($LASTEXITCODE -ne 0) {
            Fail 'gnu 目标安装失败' @('换镜像：-RustupMirror https://mirrors.tuna.tsinghua.edu.cn/rustup')
        }
    }
    $chains = & rustup toolchain list 2>$null
    if (-not ($chains -match 'stable-x86_64-pc-windows-gnu')) {
        Write-Host '[install]    另装 stable-x86_64-pc-windows-gnu host 工具链（仅 msvc 缺 link.exe 时才需要）…' -ForegroundColor Yellow
        & rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
        if ($LASTEXITCODE -ne 0) {
            Fail 'gnu host 工具链安装失败' @('换镜像：-RustupMirror https://mirrors.tuna.tsinghua.edu.cn/rustup')
        }
    }
    if (-not (Get-Command x86_64-w64-mingw32-gcc -ErrorAction SilentlyContinue) -and
        -not (Get-Command gcc -ErrorAction SilentlyContinue)) {
        Write-Host '[install]    警告：仍未找到 MinGW gcc，build_gnu.bat 会失败' -ForegroundColor Yellow
        Write-Host '            WinLibs GCC 解压到 D:\tools\MinGW\mingw64，并把其 bin 加入 PATH'
    }
    Write-Host '[install]    注意：默认 host 保持不变（msvc），需要时手工 rustup default stable-x86_64-pc-windows-gnu'
}

# ---------- 汇总 ----------
Step '完成，后续步骤'
Write-Host '    本机开发构建： scripts\build-dev.ps1'
Write-Host '    交叉编译发布： scripts\build-release.ps1  （加 -ThinLto 可加速）'
Write-Host '    详细说明：     docs\BUILD.md'
Write-Host ''
