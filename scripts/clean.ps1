#Requires -Version 5.1
<#
.SYNOPSIS
  清理可重建的中间产物，保留源码、依赖缓存和 server/data。

.DESCRIPTION
  默认（不带 -Apply）只列出候选和体积，不删除。
  -Apply 删除安全项：cargo incremental、kindle-sim/out、旧 backup_bin_*。
  -Release 额外清理 oread-target-*/release（若没有进程正在使用）。
  -Web 额外清理 server/web/dist。
  -All 等价于 -Release -Web，并清理仓库内 server/target。

  永远不会删除：server/data、server/web/node_modules、.git、源码。
#>
param(
  [switch]$Apply,
  [switch]$Release,
  [switch]$Web,
  [switch]$All
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$targets = New-Object System.Collections.Generic.List[object]

function Add-Candidate([string]$path, [string]$reason) {
  if (Test-Path $path) {
    $bytes = (Get-ChildItem $path -Recurse -File -Force -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
    $targets.Add([pscustomobject]@{ Path = $path; Reason = $reason; Bytes = [double]$bytes }) | Out-Null
  }
}

# 仓库内 target（默认不在仓库，但容错）
if ($All -and (Test-Path (Join-Path $root 'server\target'))) {
  Add-Candidate (Join-Path $root 'server\target') '仓库内 cargo target'
}

# 本地 target 的 incremental：可重建，删除只会让下次增量编译稍慢
Get-ChildItem 'D:\tools' -Directory -Filter 'oread-target-*' -ErrorAction SilentlyContinue | ForEach-Object {
  Add-Candidate (Join-Path $_.FullName 'debug\incremental') "$($_.Name) 的 incremental 缓存"
  Get-ChildItem $_.FullName -Directory -Filter 'backup_bin_*' -ErrorAction SilentlyContinue | ForEach-Object {
    Add-Candidate $_.FullName '旧二进制备份'
  }
}

# Kindle 模拟器输出
Add-Candidate (Join-Path $root 'tools\kindle-sim\out') 'Kindle 模拟器截图/输出'

# 前端 dist（可重建）
if ($Web -or $All) {
  Add-Candidate (Join-Path $root 'server\web\dist') '前端构建产物'
}

# release 产物：只在没有进程占用时清理
if ($Release -or $All) {
  Get-ChildItem 'D:\tools' -Directory -Filter 'oread-target-*' -ErrorAction SilentlyContinue | ForEach-Object {
    $release = Join-Path $_.FullName 'release'
    if (-not (Test-Path $release)) { return }
    $inUse = Get-Process -ErrorAction SilentlyContinue | Where-Object {
      $_.Path -and $_.Path.StartsWith($release, [System.StringComparison]::OrdinalIgnoreCase)
    }
    if ($inUse) {
      Write-Host "[skip] $release 正在被进程 $($inUse.Id -join ',') 使用" -ForegroundColor Yellow
    } else {
      Add-Candidate $release "$($_.Name) 的 release 产物"
    }
  }
}

if ($targets.Count -eq 0) {
  Write-Host '没有可清理的中间产物。'
  exit 0
}

$total = ($targets | Measure-Object Bytes -Sum).Sum
$targets | Sort-Object Bytes -Descending | ForEach-Object {
  '{0,10:N1} MB  {1}  ({2})' -f ($_.Bytes / 1MB), $_.Path, $_.Reason
}
'总计: {0:N1} MB' -f ($total / 1MB)

if (-not $Apply) {
  Write-Host "`n这是 dry-run；加 -Apply 执行删除。" -ForegroundColor Cyan
  exit 0
}

$freed = 0.0
foreach ($t in $targets) {
  try {
    Remove-Item $t.Path -Recurse -Force -ErrorAction Stop
    $freed += $t.Bytes
    Write-Host "[deleted] $($t.Path)"
  } catch {
    Write-Host "[failed]  $($t.Path): $($_.Exception.Message)" -ForegroundColor Yellow
  }
}
'已释放约 {0:N1} MB' -f ($freed / 1MB)
