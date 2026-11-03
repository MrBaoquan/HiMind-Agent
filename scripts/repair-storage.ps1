[CmdletBinding()]
param(
    [switch]$RecurseWebView
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$LocalAppData = [Environment]::GetFolderPath("LocalApplicationData")
$InstallRoot = Join-Path $LocalAppData "HiMindAgent"
# 用户把数据搬到别的盘以后，完整性修复要跟着数据走；程序目录单独保留。
$AgentHome = $InstallRoot
$Pointer = Join-Path $InstallRoot "data-location.json"
if (Test-Path -LiteralPath $Pointer) {
    try {
        $location = Get-Content -LiteralPath $Pointer -Raw | ConvertFrom-Json
        if ($location.data_root -and [System.IO.Path]::IsPathRooted($location.data_root)) {
            $AgentHome = $location.data_root
        }
    } catch {
        Write-Warning "数据位置指针无法解析，按默认位置处理：$Pointer"
    }
}
$WebView = Join-Path $LocalAppData "com.himind.agent"

$running = Get-Process -Name "himind-agent*" -ErrorAction SilentlyContinue |
    Where-Object {
        $_.Path -and (
            $_.Path.StartsWith($AgentHome, [StringComparison]::OrdinalIgnoreCase) -or
            $_.Path.StartsWith($InstallRoot, [StringComparison]::OrdinalIgnoreCase)
        )
    }
if ($running) {
    $ids = ($running | ForEach-Object Id) -join ", "
    throw "请先退出 HiMind Agent 后再修复存储完整性（仍在运行的 PID：$ids）。"
}

$database = Join-Path $AgentHome "local-runs.sqlite3"
$paths = @(
    $AgentHome,
    (Join-Path $AgentHome "data"),
    (Join-Path $AgentHome "logs"),
    $database,
    "$database-wal",
    "$database-shm",
    $WebView
) | Where-Object { Test-Path -LiteralPath $_ }

foreach ($path in $paths) {
    $level = if (Test-Path -LiteralPath $path -PathType Container) { "(OI)(CI)M" } else { "M" }
    Write-Host "修复 $path -> Medium"
    & icacls.exe $path /setintegritylevel $level /C
    if ($LASTEXITCODE -ne 0) {
        throw "icacls 修复失败：$path（退出码 $LASTEXITCODE）"
    }
}

if ($RecurseWebView -and (Test-Path -LiteralPath $WebView)) {
    Write-Host "递归修复 WebView2 数据目录：$WebView"
    & icacls.exe $WebView /setintegritylevel "(OI)(CI)M" /T /C
    if ($LASTEXITCODE -ne 0) {
        throw "WebView2 递归修复失败（退出码 $LASTEXITCODE）"
    }
}

Write-Host "存储完整性修复完成。请重新双击 HiMind Agent；如仍无窗口，请运行："
$state = Join-Path $AgentHome "data\agent-state.json"
Write-Host "  & .\himind-agent-launcher.exe --state `"$state`" diagnostics check"
Write-Host "  & .\himind-agent-launcher.exe --state `"$state`" diagnostics export"
