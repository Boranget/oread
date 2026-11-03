#Requires -Version 5.1
<#
.SYNOPSIS
  [兼容入口] 本机开发构建 —— 等价于仓库根 scripts\build-dev.ps1，额外把输出写入 server\build.log。

.DESCRIPTION
  历史上这个脚本是「交叉命令行 + 写死 d:\projects\read\server」的 gnu 构建；
  现在统一走 scripts\build-dev.ps1（Windows msvc host 本机调试构建，产物在 D:\tools\oread-target-win）。
  需要 x86_64-pc-windows-gnu 目标的构建，请用 build_gnu.bat。

.PARAMETER Release
  以 release profile 构建（默认 debug）。

.PARAMETER TargetDir
  覆盖 CARGO_TARGET_DIR。

.PARAMETER CargoArgs
  透传给 cargo build 的额外参数。
#>
param(
    [switch]$Release,
    [string]$TargetDir,
    [string[]]$CargoArgs
)

$scriptPath = Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts\build-dev.ps1'
if (-not (Test-Path $scriptPath)) {
    Write-Host "[run_build] 错误：找不到 $scriptPath" -ForegroundColor Red
    Write-Host '           脚本必须与 scripts\ 目录同级（<repo>\server\run_build.ps1）。' -ForegroundColor Yellow
    exit 1
}

$invoke = @{ LogFile = (Join-Path $PSScriptRoot 'build.log') }
if ($Release) { $invoke['Release'] = $true }
if ($TargetDir) { $invoke['TargetDir'] = $TargetDir }
if ($CargoArgs) { $invoke['CargoArgs'] = $CargoArgs }

$global:LASTEXITCODE = 0
& $scriptPath @invoke
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
exit 0
