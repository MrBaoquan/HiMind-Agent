[CmdletBinding()]
param(
    # 生产实例的本地端口与状态文件（默认与安装版一致）。
    [int]$LocalPort = 18181,
    [string]$ApiBase = "https://himind.andcrane.com",
    [string]$StatePath = "",
    # 官方信任根：不设置时更新校验与扩展签名在本地构建里都会失效。
    [string]$UpdatePublicKeyPath = "",
    [string]$ExtensionPublicKeyPath = "",
    [switch]$SkipFrontend,
    [switch]$SkipTests,
    [switch]$NoStart
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
$RepoRoot = $Root
$AgentHome = Join-Path $env:LOCALAPPDATA "HiMindAgent"
$ActiveVersion = "0.3.91"
$versionFile = Join-Path $AgentHome "active-version"
if (Test-Path $versionFile) { $ActiveVersion = (Get-Content $versionFile -Raw).Trim() }
$VersionDir = Join-Path $AgentHome "versions\$ActiveVersion"
$InstalledExe = Join-Path $VersionDir "himind-agent.exe"

if (-not $StatePath) { $StatePath = Join-Path $AgentHome "data\agent-state.json" }
if (-not (Test-Path $StatePath)) { throw "找不到生产实例状态文件：$StatePath" }
if (-not $UpdatePublicKeyPath) { $UpdatePublicKeyPath = Join-Path $AgentHome "trusted-keys\himind-production-2026.pem" }
if (-not $ExtensionPublicKeyPath) { $ExtensionPublicKeyPath = Join-Path $AgentHome "trusted-keys\himind-extension-production-2026.pem" }
if (-not (Test-Path $UpdatePublicKeyPath)) { $UpdatePublicKeyPath = Join-Path $RepoRoot "artifacts-codesigned\0.3.84\himind-production-2026.pem" }
if (-not (Test-Path $ExtensionPublicKeyPath)) { $ExtensionPublicKeyPath = Join-Path $RepoRoot "artifacts-codesigned\0.3.84\himind-extension-production-2026.pem" }
foreach ($key in @($UpdatePublicKeyPath, $ExtensionPublicKeyPath)) {
    if (-not (Test-Path $key)) { throw "找不到官方公钥：$key（没有它本地构建会丢掉更新与扩展签名信任根）" }
}

$Cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
if (-not (Test-Path $Cargo)) { $Cargo = "cargo" }

function Get-ListenerPid([int]$port) {
    $connection = Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
        Where-Object { $_.LocalPort -eq $port } |
        Select-Object -First 1
    if ($connection) { return $connection.OwningProcess }
    return 0
}

function Stop-Agent([int]$processId) {
    if (-not $processId) { return }
    Stop-Process -Id $processId -ErrorAction SilentlyContinue
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Milliseconds 300
        if (-not (Get-Process -Id $processId -ErrorAction SilentlyContinue)) { return }
    }
    throw "旧实例（pid=$processId）在 10 秒内没有退出"
}

function Start-Agent {
    $arguments = @("--api", $ApiBase, "--local-app", "--local-port", "$LocalPort", "--show", "--state", $StatePath)
    $process = Start-Process -FilePath $InstalledExe -ArgumentList $arguments -PassThru
    "已启动：pid=$($process.Id)"
    return $process
}

function Wait-AgentHealthy([int]$timeoutSeconds = 40) {
    $deadline = (Get-Date).AddSeconds($timeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 700
        try {
            $health = Invoke-RestMethod "http://127.0.0.1:$LocalPort/health" -TimeoutSec 2
            if ($health.status -eq "online") { return $health }
        }
        catch {
            # 启动中，继续等
        }
    }
    return $null
}

# 1) 前端先构建：Tauri 把 frontend/dist 编进二进制，先 Rust 会打进旧界面。
if (-not $SkipFrontend) {
    Push-Location (Join-Path $RepoRoot "frontend")
    try {
        & npm.cmd run build
        if ($LASTEXITCODE -ne 0) { throw "前端构建失败" }
    }
    finally {
        Pop-Location
    }
}

# 2) 带官方信任根 + 本地构建标记编 release。
$env:HIMIND_SIGNING_PUBLIC_KEY_PATH = $UpdatePublicKeyPath
$env:HIMIND_SIGNING_KEY_ID = "himind-production-2026"
$env:HIMIND_EXTENSION_SIGNING_PUBLIC_KEY_PATH = $ExtensionPublicKeyPath
$env:HIMIND_EXTENSION_SIGNING_KEY_ID = "himind-extension-production-2026"
$env:HIMIND_BUILD_CHANNEL = "local"

Push-Location $RepoRoot
try {
    if (-not $SkipTests) {
        & $Cargo test --locked 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { Write-Warning "cargo test 未全部通过（既有环境失败可忽略），继续部署" }
    }
    & $Cargo build --release --bin himind-agent
    if ($LASTEXITCODE -ne 0) { throw "release 构建失败" }
}
finally {
    Pop-Location
}

$BuiltExe = Join-Path $RepoRoot "target\release\himind-agent.exe"
if (-not (Test-Path $BuiltExe)) { throw "找不到构建产物：$BuiltExe" }

# 3) 停旧实例 → 备份 → 替换。
$runningPid = Get-ListenerPid $LocalPort
if ($runningPid) {
    "停止旧实例：pid=$runningPid（端口 $LocalPort）"
    Stop-Agent $runningPid
}

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$backup = "$InstalledExe.bak-$stamp"
if (Test-Path $InstalledExe) { Copy-Item $InstalledExe $backup -Force }
Copy-Item $BuiltExe $InstalledExe -Force
"已安装本地构建：$InstalledExe（备份 $backup）"

# 4) 启动并验证；失败自动回滚。
if ($NoStart) {
    "已跳过启动（-NoStart）。"
    return
}

$process = Start-Agent
$health = Wait-AgentHealthy
$rollbackNeeded = $true
if ($health) {
    $channel = $health.build.channel
    if ($channel -eq "local") { $rollbackNeeded = $false }
}
if ($rollbackNeeded) {
    Write-Warning "健康检查未通过或构建标记不符，回滚到备份二进制"
    Stop-Agent $process.Id
    if (Test-Path $backup) {
        Copy-Item $backup $InstalledExe -Force
        Start-Agent | Out-Null
        Wait-AgentHealthy | Out-Null
    }
    throw "本地部署失败，已回滚"
}

"部署完成：version=$($health.version) build=$($health.build.channel)/$($health.build.commit) profile=$($health.profile) port=$LocalPort"
"回滚命令：Copy-Item '$backup' '$InstalledExe' -Force ; 然后重启实例"
