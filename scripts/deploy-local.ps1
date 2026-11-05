[CmdletBinding()]
param(
    # 生产实例的本地端口与状态文件（默认与安装版一致）。
    [int]$LocalPort = 18181,
    [string]$ApiBase = "https://himind.andcrane.com",
    [string]$StatePath = "",
    # 官方信任根：不设置时更新校验与扩展签名在本地构建里都会失效。
    [string]$UpdatePublicKeyPath = "",
    [string]$ExtensionPublicKeyPath = "",
    # 与 Agent 固定清单匹配的已签名 Runtime 发布目录；留空时自动解析。
    [string]$RuntimeReleaseDirectory = "",
    [switch]$SkipFrontend,
    [switch]$SkipTests,
    [switch]$NoStart
)

$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "runtime-bundle.ps1")

$Root = Split-Path -Parent $PSScriptRoot
$RepoRoot = $Root
$AgentHome = Join-Path $env:LOCALAPPDATA "HiMindAgent"
$CargoManifest = Join-Path $RepoRoot "Cargo.toml"
if (-not (Test-Path -LiteralPath $CargoManifest -PathType Leaf)) {
    throw "找不到 Cargo 清单：$CargoManifest"
}
$CargoManifestText = Get-Content -LiteralPath $CargoManifest -Raw -Encoding UTF8
if ($CargoManifestText -notmatch '(?m)^\s*version\s*=\s*"([^"]+)"') {
    throw "无法从 Cargo.toml 读取 Agent 版本：$CargoManifest"
}
$ActiveVersion = $Matches[1]
$versionFile = Join-Path $AgentHome "active-version"
$VersionDir = Join-Path $AgentHome "versions\$ActiveVersion"
$InstalledExe = Join-Path $VersionDir "himind-agent.exe"
$InstalledMcp = Join-Path $VersionDir "himind-agent-mcp.exe"
$InstalledLauncher = Join-Path $AgentHome "himind-agent-launcher.exe"
$RuntimeBundleDestination = Join-Path $AgentHome "resources\runtime"

if (-not $StatePath) { $StatePath = Join-Path $AgentHome "data\agent-state.json" }
if (-not (Test-Path $StatePath)) { throw "找不到生产实例状态文件：$StatePath" }
if (-not $UpdatePublicKeyPath) { $UpdatePublicKeyPath = Join-Path $AgentHome "trusted-keys\himind-production-2026.pem" }
if (-not $ExtensionPublicKeyPath) { $ExtensionPublicKeyPath = Join-Path $AgentHome "trusted-keys\himind-extension-production-2026.pem" }
if (-not (Test-Path $UpdatePublicKeyPath)) { $UpdatePublicKeyPath = Join-Path $RepoRoot "artifacts-codesigned\0.3.84\himind-production-2026.pem" }
if (-not (Test-Path $ExtensionPublicKeyPath)) { $ExtensionPublicKeyPath = Join-Path $RepoRoot "artifacts-codesigned\0.3.84\himind-extension-production-2026.pem" }
foreach ($key in @($UpdatePublicKeyPath, $ExtensionPublicKeyPath)) {
    if (-not (Test-Path $key)) { throw "找不到官方公钥：$key（没有它本地构建会丢掉更新与扩展签名信任根）" }
}

$RuntimeRelease = Resolve-RuntimeReleaseDirectory -RequestedDirectory $RuntimeReleaseDirectory -RequireExactPackage -ExcludeInstalledBundle
"已锁定 Runtime：version=$($RuntimeRelease.Manifest.version) sha256=$($RuntimeRelease.Manifest.sha256)"

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
    $children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $processId" -ErrorAction SilentlyContinue)
    foreach ($child in $children) {
        Stop-Agent ([int]$child.ProcessId)
    }
    Stop-Process -Id $processId -ErrorAction SilentlyContinue
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Milliseconds 300
        if (-not (Get-Process -Id $processId -ErrorAction SilentlyContinue)) { return }
    }
    throw "旧实例（pid=$processId）在 10 秒内没有退出"
}

function Stop-InstalledAgentProcesses {
    $processes = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
        $_.ExecutablePath -and
        $_.ExecutablePath.StartsWith($AgentHome, [StringComparison]::OrdinalIgnoreCase) -and
        $_.Name -match '^himind-agent(?:-launcher|-mcp)?\.exe$'
    })
    foreach ($item in $processes) {
        Stop-Process -Id ([int]$item.ProcessId) -Force -ErrorAction SilentlyContinue
    }
    for ($i = 0; $i -lt 20; $i++) {
        $remaining = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
            $_.ExecutablePath -and
            $_.ExecutablePath.StartsWith($AgentHome, [StringComparison]::OrdinalIgnoreCase) -and
            $_.Name -match '^himind-agent(?:-launcher|-mcp)?\.exe$'
        })
        if ($remaining.Count -eq 0) { return }
        foreach ($item in $remaining) {
            Stop-Process -Id ([int]$item.ProcessId) -Force -ErrorAction SilentlyContinue
        }
        Start-Sleep -Milliseconds 250
    }
    throw "本机 Agent 子进程未能全部退出，安装目录仍被占用"
}

function Copy-InstalledExeWithRetry([Parameter(Mandatory = $true)][string]$Source, [Parameter(Mandatory = $true)][string]$Destination) {
    for ($attempt = 0; $attempt -lt 8; $attempt++) {
        try {
            Copy-Item -LiteralPath $Source -Destination $Destination -Force -ErrorAction Stop
            return
        }
        catch {
            if ($attempt -eq 7) { throw }
            Stop-InstalledAgentProcesses
            Start-Sleep -Milliseconds 500
        }
    }
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

function Assert-InstalledRuntime([Parameter(Mandatory = $true)][object]$Manifest) {
    $runtimeStatePath = Join-Path $AgentHome "runtimes\deepseek-harness\state.json"
    if (-not (Test-Path -LiteralPath $runtimeStatePath -PathType Leaf)) {
        throw "Runtime 状态文件不存在：$runtimeStatePath"
    }
    $runtimeState = Get-Content -LiteralPath $runtimeStatePath -Raw -Encoding UTF8 | ConvertFrom-Json
    $expectedPackage = [string]$Manifest.version
    $expectedEngine = [string]$Manifest.engine_version
    if ([string]::IsNullOrWhiteSpace($expectedEngine)) {
        $expectedEngine = $expectedPackage
    }
    if ([string]$runtimeState.version -ne $expectedPackage) {
        throw "Runtime 包版本不一致：期望 $expectedPackage，实际 $($runtimeState.version)"
    }
    $runtimeExecutable = [string]$runtimeState.executable_path
    if ([string]::IsNullOrWhiteSpace($runtimeExecutable) -or -not (Test-Path -LiteralPath $runtimeExecutable -PathType Leaf)) {
        throw "Runtime 可执行文件不可用：$runtimeExecutable"
    }
    $reportedVersion = @(& $runtimeExecutable --version 2>&1 | ForEach-Object { [string]$_ }) -join "`n"
    if ($LASTEXITCODE -ne 0) {
        throw "Runtime 版本探测失败（exit=$LASTEXITCODE）：$reportedVersion"
    }
    if ($reportedVersion -notmatch [Regex]::Escape($expectedEngine)) {
        throw "Runtime 引擎版本不一致：期望 $expectedEngine，实际 $reportedVersion"
    }
    return $runtimeState
}

function Install-RuntimeBundle([Parameter(Mandatory = $true)][object]$Manifest) {
    $manifestPath = Join-Path $RuntimeBundleDestination "himind-runtime-release.json"
    "正在安装已验证的 Runtime：$($Manifest.version)"
    $token = [Guid]::NewGuid().ToString("N")
    $stdoutPath = Join-Path $env:TEMP "himind-runtime-install-$token.out.log"
    $stderrPath = Join-Path $env:TEMP "himind-runtime-install-$token.err.log"
    try {
        # Native installers write progress to stderr. Do not pipe those lines
        # through PowerShell with ErrorActionPreference=Stop, or the first
        # progress message becomes a false terminating error.
        $process = Start-Process -FilePath $InstalledExe `
            -ArgumentList @("--state", "`"$StatePath`"", "runtime", "install", "--manifest", "`"$manifestPath`"") `
            -Wait -PassThru -WindowStyle Hidden `
            -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
        $exitCode = [int]$process.ExitCode
        $output = @(
            Get-Content -LiteralPath $stdoutPath -ErrorAction SilentlyContinue
            Get-Content -LiteralPath $stderrPath -ErrorAction SilentlyContinue
        ) | ForEach-Object { [string]$_ }
        $detail = $output -join "`n"
        if ($exitCode -notin @(0, 2)) {
            throw "Runtime 安装失败（exit=$exitCode）：$detail"
        }
        if ($exitCode -eq 2) {
            Write-Warning "Runtime 已安装，但部分能力降级：`n$detail"
        }
    }
    finally {
        [IO.File]::Delete($stdoutPath)
        [IO.File]::Delete($stderrPath)
    }
    Assert-InstalledRuntime $Manifest | Out-Null
}

function Remove-RuntimeDirectorySafely([Parameter(Mandatory = $true)][string]$Directory) {
    if (-not (Test-Path -LiteralPath $Directory)) { return }
    $resourcesRoot = [IO.Path]::GetFullPath((Join-Path $AgentHome "resources"))
    $target = [IO.Path]::GetFullPath($Directory)
    $prefix = $resourcesRoot.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $target.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "拒绝删除 Runtime 目录以外的路径：$target"
    }
    Remove-Item -LiteralPath $target -Recurse -Force
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
    & $Cargo build --release --bin himind-agent --bin himind-agent-mcp --bin himind-agent-launcher
    if ($LASTEXITCODE -ne 0) { throw "release 构建失败" }
}
finally {
    Pop-Location
}

$BuiltExe = Join-Path $RepoRoot "target\release\himind-agent.exe"
$BuiltMcp = Join-Path $RepoRoot "target\release\himind-agent-mcp.exe"
$BuiltLauncher = Join-Path $RepoRoot "target\release\himind-agent-launcher.exe"
foreach ($built in @($BuiltExe, $BuiltMcp, $BuiltLauncher)) {
    if (-not (Test-Path -LiteralPath $built)) { throw "找不到构建产物：$built" }
}

# 3) 在同一事务中部署 Agent 与固定 Runtime。任一阶段失败都恢复旧二进制和旧 Runtime。
$runningPid = Get-ListenerPid $LocalPort
$wasRunning = [bool]$runningPid
if ($runningPid) {
    "停止旧实例：pid=$runningPid（端口 $LocalPort）"
    Stop-Agent $runningPid
}
# MCP 服务可能由 AI 客户端独立拉起，不挂在 UI 进程树下；替换二进制前必须一并停止。
Stop-InstalledAgentProcesses

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$exeBackup = "$InstalledExe.bak-$stamp"
$mcpBackup = "$InstalledMcp.bak-$stamp"
$launcherBackup = "$InstalledLauncher.bak-$stamp"
$runtimeBackup = "$RuntimeBundleDestination.bak-$stamp"
$hadInstalledExe = Test-Path -LiteralPath $InstalledExe -PathType Leaf
$hadInstalledMcp = Test-Path -LiteralPath $InstalledMcp -PathType Leaf
$hadInstalledLauncher = Test-Path -LiteralPath $InstalledLauncher -PathType Leaf
$hadRuntimeBundle = Test-Path -LiteralPath $RuntimeBundleDestination -PathType Container
$hadActiveVersion = Test-Path -LiteralPath $versionFile -PathType Leaf
$previousActiveVersion = if ($hadActiveVersion) { (Get-Content -LiteralPath $versionFile -Raw).Trim() } else { "" }
$process = $null

try {
    New-Item -ItemType Directory -Path $VersionDir -Force | Out-Null

    if ($hadRuntimeBundle) {
        Move-Item -LiteralPath $RuntimeBundleDestination -Destination $runtimeBackup
    }
    Copy-RuntimeBundle -RuntimeReleaseDirectory $RuntimeRelease.Directory -DestinationDirectory $RuntimeBundleDestination -RequireExactPackage | Out-Null

    if ($hadInstalledExe) {
        Copy-Item -LiteralPath $InstalledExe -Destination $exeBackup -Force
    }
    if ($hadInstalledMcp) {
        Copy-Item -LiteralPath $InstalledMcp -Destination $mcpBackup -Force
    }
    if ($hadInstalledLauncher) {
        Copy-Item -LiteralPath $InstalledLauncher -Destination $launcherBackup -Force
    }
    Copy-InstalledExeWithRetry -Source $BuiltExe -Destination $InstalledExe
    Copy-InstalledExeWithRetry -Source $BuiltMcp -Destination $InstalledMcp
    Copy-InstalledExeWithRetry -Source $BuiltLauncher -Destination $InstalledLauncher
    "已安装本地构建：$InstalledExe（Agent 备份 $exeBackup）"
    "已安装 MCP companion：$InstalledMcp（备份 $mcpBackup）"
    "已安装 launcher：$InstalledLauncher（备份 $launcherBackup）"
    "已部署固定 Runtime：$RuntimeBundleDestination（Runtime 备份 $runtimeBackup）"

    Install-RuntimeBundle $RuntimeRelease.Manifest
    Set-Content -LiteralPath $versionFile -Value $ActiveVersion -Encoding ASCII -NoNewline

    if ($NoStart) {
        if (Test-Path -LiteralPath $runtimeBackup -PathType Container) {
            Remove-RuntimeDirectorySafely $runtimeBackup
        }
        "已跳过启动（-NoStart）；Agent 和 Runtime 均已完成校验。"
        return
    }

    # 4) 启动并验证 Agent 健康、构建来源和 Runtime 引擎身份。
    $process = Start-Agent
    $health = Wait-AgentHealthy
    if (-not $health) {
        throw "健康检查超时，端口 $LocalPort 未返回 online"
    }
    if ([string]$health.version -ne $ActiveVersion) {
        throw "Agent 版本不一致：期望 $ActiveVersion，实际 $($health.version)"
    }
    if ([string]$health.build.channel -ne "local") {
        throw "构建来源不符：期望 local，实际 $($health.build.channel)"
    }
    $runtimeState = Assert-InstalledRuntime $RuntimeRelease.Manifest

    if (Test-Path -LiteralPath $runtimeBackup -PathType Container) {
        Remove-RuntimeDirectorySafely $runtimeBackup
    }

    "部署完成：version=$($health.version) build=$($health.build.channel)/$($health.build.commit) runtime=$($runtimeState.version) profile=$($health.profile) port=$LocalPort"
    "Agent 回滚命令：Copy-Item '$exeBackup' '$InstalledExe' -Force；然后重启实例"
}
catch {
    $deploymentError = $_
    Write-Warning "本地部署失败，正在回滚 Agent 与 Runtime：$($deploymentError.Exception.Message)"
    if ($process) {
        Stop-Agent $process.Id
    }

    foreach ($pair in @(
        @{ Had = $hadInstalledExe; Backup = $exeBackup; Target = $InstalledExe },
        @{ Had = $hadInstalledMcp; Backup = $mcpBackup; Target = $InstalledMcp },
        @{ Had = $hadInstalledLauncher; Backup = $launcherBackup; Target = $InstalledLauncher }
    )) {
        if ($pair.Had -and (Test-Path -LiteralPath $pair.Backup -PathType Leaf)) {
            Copy-InstalledExeWithRetry -Source $pair.Backup -Destination $pair.Target
        }
        elseif (-not $pair.Had -and (Test-Path -LiteralPath $pair.Target -PathType Leaf)) {
            Remove-Item -LiteralPath $pair.Target -Force
        }
    }

    if ($hadRuntimeBundle -and (Test-Path -LiteralPath $runtimeBackup -PathType Container)) {
        Remove-RuntimeDirectorySafely $RuntimeBundleDestination
        Move-Item -LiteralPath $runtimeBackup -Destination $RuntimeBundleDestination
    }
    elseif (-not $hadRuntimeBundle) {
        Remove-RuntimeDirectorySafely $RuntimeBundleDestination
    }

    if ($hadActiveVersion) {
        Set-Content -LiteralPath $versionFile -Value $previousActiveVersion -Encoding ASCII -NoNewline
    }
    elseif (Test-Path -LiteralPath $versionFile -PathType Leaf) {
        Remove-Item -LiteralPath $versionFile -Force
    }

    if (-not $hadInstalledExe -and (Test-Path -LiteralPath $VersionDir -PathType Container)) {
        $remainingVersionFiles = @(Get-ChildItem -LiteralPath $VersionDir -Force -ErrorAction SilentlyContinue)
        if ($remainingVersionFiles.Count -eq 0) {
            Remove-Item -LiteralPath $VersionDir -Force
        }
    }

    if ($wasRunning -and -not $NoStart -and (Test-Path -LiteralPath $InstalledExe -PathType Leaf)) {
        Start-Agent | Out-Null
        if (-not (Wait-AgentHealthy)) {
            Write-Warning "旧 Agent 已恢复文件，但健康检查仍未通过，请检查日志。"
        }
    }
    throw "本地部署失败，已回滚：$($deploymentError.Exception.Message)"
}
