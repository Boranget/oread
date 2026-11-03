#Requires -Version 5.1
<#
.SYNOPSIS
  [兼容入口] aarch64-unknown-linux-musl 交叉编译 —— 等价于仓库根 scripts\build-release.ps1，
  额外把输出写入 server\crossbuild.log。

.DESCRIPTION
  历史上这个脚本写死 d:\projects\read\server 与 D:\tools\rust 路径；
  现在统一走 scripts\build-release.ps1：Zig 作 linker，产物在 D:\tools\cargo-target-musl。

.PARAMETER ThinLto
  用 thin LTO 加快构建（体积略大）。

.PARAMETER TargetDir
  覆盖 CARGO_TARGET_DIR。

.PARAMETER CargoArgs
  透传给 cargo zigbuild 的额外参数。
#>
param(
    [switch]$ThinLto,
    [string]$TargetDir,
    [string[]]$CargoArgs
)

$scriptPath = Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts\build-release.ps1'
if (-not (Test-Path $scriptPath)) {
    Write-Host "[crossbuild] 错误：找不到 $scriptPath" -ForegroundColor Red
    Write-Host '           脚本必须与 scripts\ 目录同级（<repo>\server\crossbuild.ps1）。' -ForegroundColor Yellow
    exit 1
}

$invoke = @{ LogFile = (Join-Path $PSScriptRoot 'crossbuild.log') }
if ($ThinLto) { $invoke['ThinLto'] = $true }
if ($TargetDir) { $invoke['TargetDir'] = $TargetDir }
if ($CargoArgs) { $invoke['CargoArgs'] = $CargoArgs }

$global:LASTEXITCODE = 0
& $scriptPath @invoke
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
exit 0
