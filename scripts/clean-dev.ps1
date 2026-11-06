<#
.SYNOPSIS
  清理本机开发/测试轮的临时产物（按白名单删除），保留运行目录、成品包与原始日志。

.DESCRIPTION
  白名单（只删这些）：
    - dev target：D:\tools\cache\target-dev（可用 OREAD_TARGET_DEV 覆盖）
    - 临时 release target：D:\tools\oread-target-v* 与 D:\tools\oread-target-wip-*
    - 临时文件：%TEMP%\oread-*、%TEMP%\k_session*.txt、D:\tools\cache\*.log

  永不删除：
    - 运行目录 D:\tools\oread-target-win（本机 :3000）
    - 成品包 D:\tools\release-pack、D:\tools\openstick
    - 源码仓库与他人 worktree（D:\tools\bench\*）
    - 原始日志 incoming-*

  安全措施：
    1. 有 cargo/rustc/clippy-driver/rustdoc/MSBuild/dotnet 进程时直接退出；
    2. 每个目标先做「重命名探测」（改名成功才说明无占用、可再生），再删除；
    3. 输出清理前后磁盘可用量与删除明细。

.PARAMETER DryRun
  只列清单与占用大小，不重命名、不删除。

.PARAMETER KeepDevTarget
  保留 dev target（只清临时 release target、临时日志与凭据）。

.EXAMPLE
  .\scripts\clean-dev.ps1 -DryRun
.EXAMPLE
  .\scripts\clean-dev.ps1
#>
param(
    [switch]$DryRun,
    [switch]$KeepDevTarget
)

$ErrorActionPreference = 'Stop'
$RunDir = 'D:\tools\oread-target-win'
$Keep = @(
    $RunDir,
    'D:\tools\release-pack',
    'D:\tools\openstick',
    'D:\tools\bench'
)
$DevTarget = if ($env:OREAD_TARGET_DEV) { $env:OREAD_TARGET_DEV } else { 'D:\tools\cache\target-dev' }

function Test-IsKept([string]$p) {
    $full = [System.IO.Path]::GetFullPath($p).TrimEnd('\')
    foreach ($k in $Keep) {
        $kf = [System.IO.Path]::GetFullPath($k).TrimEnd('\')
        if ($full -ieq $kf) { return $true }
        if ($full.StartsWith($kf + '\', [StringComparison]::OrdinalIgnoreCase)) {
            # 白名单内的成品包/源码一律保留；只有 %TEMP% 下的临时副本例外
            if ($full.StartsWith('D:\tools\bench\', [StringComparison]::OrdinalIgnoreCase)) { return $true }
            if ($full.StartsWith('D:\tools\release-pack\', [StringComparison]::OrdinalIgnoreCase)) { return $true }
            if ($full.StartsWith('D:\tools\openstick\', [StringComparison]::OrdinalIgnoreCase)) { return $true }
            if ($full -ieq $RunDir) { return $true }
        }
    }
    return $false
}

function Get-SizeMB([string]$p) {
    try {
        if (Test-Path -LiteralPath $p -PathType Container) {
            $sum = (Get-ChildItem -LiteralPath $p -Recurse -File -Force -ErrorAction SilentlyContinue |
                    Measure-Object -Property Length -Sum).Sum
            return [math]::Round(($sum / 1MB), 1)
        }
        if (Test-Path -LiteralPath $p) {
            return [math]::Round(((Get-Item -LiteralPath $p).Length / 1MB), 1)
        }
    } catch { }
    return 0
}

function Get-FreeGB([string]$drive) {
    try {
        $d = Get-PSDrive -Name $drive -ErrorAction Stop
        return [math]::Round($d.Free / 1GB, 2)
    } catch { return $null }
}

# ---- 前置：构建进程占用检查 ----
$busy = Get-Process cargo, rustc, clippy-driver, rustdoc, MSBuild, dotnet, link -ErrorAction SilentlyContinue
if ($busy) {
    Write-Host '[clean-dev] 检测到构建进程，先停止再清理：' -ForegroundColor Red
    $busy | Select-Object Id, ProcessName, CPU | Format-Table -AutoSize
    exit 1
}

# ---- 前置：浏览器占用检查（避免删掉其它会话正在用的临时 profile）----
$browserBusy = Get-Process chrome, msedge, chromium, firefox -ErrorAction SilentlyContinue
if ($browserBusy) {
    Write-Host ('[clean-dev] 检测到浏览器进程（{0} 个）：临时 *chrome* / *cdp* 目录本轮跳过' -f $browserBusy.Count) -ForegroundColor Yellow
}

# ---- 候选清单（白名单） ----
$candidates = @()
if (-not $KeepDevTarget) { $candidates += $DevTarget }
if (Test-Path 'D:\tools') {
    $candidates += Get-ChildItem 'D:\tools' -Directory -Force -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like 'oread-target-v*' -or $_.Name -like 'oread-target-wip-*' } |
        ForEach-Object { $_.FullName }
}
$candidates += Get-ChildItem $env:TEMP -Force -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -like 'oread-*' -or $_.Name -like 'k_session*.txt' } |
    ForEach-Object { $_.FullName }
if (Test-Path 'D:\tools\cache') {
    $candidates += Get-ChildItem 'D:\tools\cache' -File -Force -ErrorAction SilentlyContinue |
        Where-Object { $_.Extension -eq '.log' } |
        ForEach-Object { $_.FullName }
}

$browserSkip = @()
if ($browserBusy) {
    $browserSkip = @($candidates | Where-Object {
        (Split-Path -Leaf $_) -match '(chrome|cdp)' -and -not (Test-IsKept $_)
    } | Sort-Object -Unique)
    $candidates = @($candidates | Where-Object { $browserSkip -notcontains $_ })
}

$targets = @($candidates | Where-Object { (Test-Path -LiteralPath $_) -and -not (Test-IsKept $_) } |
             Sort-Object -Unique)
$skipped = @(@($candidates | Where-Object { Test-IsKept $_ }) + $browserSkip | Sort-Object -Unique)

$freeBeforeD = Get-FreeGB 'D'
$freeBeforeC = Get-FreeGB 'C'
$totalMB = 0
Write-Host ("[clean-dev] 模式：" + $(if ($DryRun) { 'DryRun（只预览）' } else { '实际清理' }))
Write-Host ("[clean-dev] 清理前可用空间：D: {0} GB / C: {1} GB" -f $freeBeforeD, $freeBeforeC)
Write-Host '[clean-dev] 白名单内待处理：'
foreach ($t in $targets) {
    $mb = Get-SizeMB $t
    $totalMB += $mb
    Write-Host ("  - {0}  ({1} MB)" -f $t, $mb)
}
if ($skipped.Count -gt 0) {
    Write-Host '[clean-dev] 命中保留规则（不动）：'
    $skipped | ForEach-Object { Write-Host ("  = {0}" -f $_) }
}
if ($targets.Count -eq 0) {
    Write-Host '[clean-dev] 没有需要清理的临时产物。'
}

if ($DryRun) {
    Write-Host ("[clean-dev] DryRun 合计可释放约 {0} MB（未删除）" -f ([math]::Round($totalMB, 1)))
    exit 0
}

# ---- 重命名探测 + 删除 ----
$removed = @()
$failed = @()
foreach ($t in $targets) {
    $parent = Split-Path -Parent $t
    $leaf = Split-Path -Leaf $t
    $probe = Join-Path $parent ("{0}.deleting-{1}" -f $leaf, $PID)
    try {
        Rename-Item -LiteralPath $t -NewName (Split-Path -Leaf $probe) -ErrorAction Stop
    } catch {
        $failed += $t
        Write-Host ("  ! 跳过（占用或不可重命名）：{0}" -f $t) -ForegroundColor Yellow
        continue
    }
    try {
        Remove-Item -LiteralPath $probe -Recurse -Force -ErrorAction Stop
        $removed += $t
        Write-Host ("  - 已删除 {0}" -f $t)
    } catch {
        $failed += $t
        Write-Host ("  ! 已重命名但删除失败（可手动删 {0}）：{1}" -f $probe, $_.Exception.Message) -ForegroundColor Yellow
    }
}

$freeAfterD = Get-FreeGB 'D'
$freeAfterC = Get-FreeGB 'C'
Write-Host ("[clean-dev] 清理后可用空间：D: {0} GB (Δ {1}) / C: {2} GB (Δ {3})" -f `
        $freeAfterD, [math]::Round($freeAfterD - $freeBeforeD, 2), `
        $freeAfterC, [math]::Round($freeAfterC - $freeBeforeC, 2))
Write-Host ("[clean-dev] 完成：删除 {0} 项，失败 {1} 项，保留 {2} 项。" -f $removed.Count, $failed.Count, $skipped.Count)
if ($failed.Count -gt 0) { exit 2 }
