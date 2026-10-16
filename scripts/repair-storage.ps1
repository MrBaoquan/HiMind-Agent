[CmdletBinding()]
param(
    [switch]$RecurseWebView
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$LocalAppData = [Environment]::GetFolderPath("LocalApplicationData")
$AgentHome = Join-Path $LocalAppData "HiMindAgent"
$WebView = Join-Path $LocalAppData "com.himind.agent"

$running = Get-Process -Name "himind-agent*" -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($AgentHome, [StringComparison]::OrdinalIgnoreCase) }
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
