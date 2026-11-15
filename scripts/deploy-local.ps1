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
$lockDirectory = Join-Path $AgentHome "deploy"
New-Item -ItemType Directory -Path $lockDirectory -Force | Out-Null
$deploymentLock = $null
try {
    $deploymentLock = [IO.File]::Open((Join-Path $lockDirectory "local.lock"), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
}
catch {
    throw "另一个本地部署正在运行或部署锁不可写，未停止 Agent：$($_.Exception.Message)"
}
try {
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
    $process = Get-Process -Id $processId -ErrorAction SilentlyContinue
    if (-not $process) { return }
    $children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $processId" -ErrorAction Stop)
    try {
        Stop-Process -Id $processId -Force -ErrorAction Stop
    }
    catch {
        if (Get-Process -Id $processId -ErrorAction SilentlyContinue) {
            throw "无法停止 Agent（pid=$processId）：$($_.Exception.Message)"
        }
        return
    }
    if (-not $process.WaitForExit(10000)) {
        $remaining = Get-Process -Id $processId -ErrorAction SilentlyContinue
        if ($remaining -and -not $remaining.HasExited) {
            throw "旧实例（pid=$processId）在 10 秒内没有退出"
        }
    }
    # Stop the parent first so WebView cannot recreate a child during teardown.
    foreach ($child in $children) {
        Stop-Agent ([int]$child.ProcessId)
    }
}

function Get-InstalledAgentProcesses {
    $prefix = [IO.Path]::GetFullPath($AgentHome).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    return @(Get-CimInstance Win32_Process -ErrorAction Stop | Where-Object {
        $_.ExecutablePath -and
        $_.ExecutablePath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) -and
        $_.Name -match '^himind-agent(?:-launcher|-mcp)?\.exe$'
    })
}

function Stop-InstalledAgentProcesses {
    for ($attempt = 0; $attempt -lt 4; $attempt++) {
        $remaining = @(Get-InstalledAgentProcesses)
        if ($remaining.Count -eq 0) { return }
        foreach ($item in $remaining) {
            Stop-Agent ([int]$item.ProcessId)
        }
    }
    $remaining = @(Get-InstalledAgentProcesses)
    if ($remaining.Count) {
        throw "本机 Agent 进程仍占用安装目录：$($remaining.ProcessId -join ', ')"
    }
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
    $arguments = @("--api", $ApiBase, "--local-app", "--local-port", "$LocalPort", "--show", "--state", "`"$StatePath`"")
    $logDirectory = Join-Path $AgentHome "logs"
    New-Item -ItemType Directory -Path $logDirectory -Force | Out-Null
    $logStamp = (Get-Date -Format "yyyyMMdd-HHmmss") + "-" + [Guid]::NewGuid().ToString("N").Substring(0, 8)
    $stdoutPath = Join-Path $logDirectory "deploy-agent-$logStamp.stdout.log"
    $stderrPath = Join-Path $logDirectory "deploy-agent-$logStamp.stderr.log"
    $process = Start-Process -FilePath $InstalledExe -ArgumentList $arguments -PassThru -WindowStyle Hidden -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
    Write-Host "已启动：pid=$($process.Id)"
    Write-Host "启动日志：$stdoutPath；$stderrPath"
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
        if ($LASTEXITCODE -ne 0) { throw "cargo test 失败，未停止现有 Agent；修复失败或显式使用 -SkipTests" }
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

# 3) 预备、停服、替换、校验和失败恢复都由同一脚本执行，不依赖调用方会话。
function Invoke-LocalDeployment {
    $runningPid = Get-ListenerPid $LocalPort
    $wasRunning = [bool]$runningPid
    $stamp = (Get-Date -Format "yyyyMMdd-HHmmss") + "-" + [Guid]::NewGuid().ToString("N").Substring(0, 8)
    $runtimeStaging = "$RuntimeBundleDestination.staging-$stamp"
    $runtimeBackup = "$RuntimeBundleDestination.bak-$stamp"
    $hadRuntimeBundle = Test-Path -LiteralPath $RuntimeBundleDestination -PathType Container
    $hadActiveVersion = Test-Path -LiteralPath $versionFile -PathType Leaf
    $previousActiveVersion = if ($hadActiveVersion) { [IO.File]::ReadAllBytes($versionFile) } else { $null }
    $recoveryExe = $InstalledExe
    if ($hadActiveVersion) {
        $previousVersion = [Text.Encoding]::ASCII.GetString($previousActiveVersion).Trim()
        if ($previousVersion -notmatch '^[0-9A-Za-z][0-9A-Za-z.+_-]*$') {
            throw "已安装 active-version 格式无效，未停止 Agent"
        }
        $recoveryExe = Join-Path $AgentHome "versions\$previousVersion\himind-agent.exe"
    }
    if ($wasRunning) {
        # Do not terminate an unrelated application that happens to own LocalPort.
        $previousHealth = Invoke-RestMethod "http://127.0.0.1:$LocalPort/health" -TimeoutSec 3
        if ([string]$previousHealth.executable_path -ne $recoveryExe) {
            throw "端口 $LocalPort 不属于预期 Agent：$($previousHealth.executable_path)；未停止进程"
        }
    }
    $artifacts = @(
        @{ Source = $BuiltExe; Target = $InstalledExe; Backup = "$InstalledExe.bak-$stamp"; Touched = $false },
        @{ Source = $BuiltMcp; Target = $InstalledMcp; Backup = "$InstalledMcp.bak-$stamp"; Touched = $false },
        @{ Source = $BuiltLauncher; Target = $InstalledLauncher; Backup = "$InstalledLauncher.bak-$stamp"; Touched = $false }
    )
    $stopAttempted = $false
    $runtimeTouched = $false
    $versionTouched = $false
    $process = $null
    $rollbackErrors = [System.Collections.Generic.List[string]]::new()
    try {
        # All large copies and backups finish while the gateway is still available.
        foreach ($artifact in $artifacts) {
            $artifact.Had = Test-Path -LiteralPath $artifact.Target -PathType Leaf
            $artifact.Hash = (Get-FileHash -LiteralPath $artifact.Source -Algorithm SHA256).Hash
            if ($artifact.Had) {
                Copy-Item -LiteralPath $artifact.Target -Destination $artifact.Backup -ErrorAction Stop
            }
        }
        Copy-RuntimeBundle -RuntimeReleaseDirectory $RuntimeRelease.Directory -DestinationDirectory $runtimeStaging -RequireExactPackage | Out-Null

        $stopAttempted = $true
        if ($runningPid) {
            Write-Host "停止旧实例：pid=$runningPid（端口 $LocalPort）"
            Stop-Agent $runningPid
        }
        # MCP and launcher processes may belong to another client process tree.
        Stop-InstalledAgentProcesses

        New-Item -ItemType Directory -Path $VersionDir -Force | Out-Null
        if ($hadRuntimeBundle) {
            Move-Item -LiteralPath $RuntimeBundleDestination -Destination $runtimeBackup -ErrorAction Stop
        }
        $runtimeTouched = $true
        Move-Item -LiteralPath $runtimeStaging -Destination $RuntimeBundleDestination -ErrorAction Stop
        foreach ($artifact in $artifacts) {
            $artifact.Touched = $true
            Copy-InstalledExeWithRetry -Source $artifact.Source -Destination $artifact.Target
            $installedHash = (Get-FileHash -LiteralPath $artifact.Target -Algorithm SHA256).Hash
            if ($installedHash -ne $artifact.Hash) {
                throw "安装文件 SHA-256 不一致：$($artifact.Target)"
            }
            Write-Host "已安装：$($artifact.Target) sha256=$installedHash（备份 $($artifact.Backup)）"
        }

        Install-RuntimeBundle $RuntimeRelease.Manifest
        $versionTouched = $true
        Set-Content -LiteralPath $versionFile -Value $ActiveVersion -Encoding ASCII -NoNewline

        if (-not $NoStart) {
            $process = Start-Agent
            $health = Wait-AgentHealthy
            if (-not $health) {
                throw "健康检查超时，端口 $LocalPort 未返回 online"
            }
            if ([string]$health.version -ne $ActiveVersion -or [string]$health.build.channel -ne "local") {
                throw "Agent 版本或构建来源不符：version=$($health.version) channel=$($health.build.channel)"
            }
            if ([string]$health.executable_path -ne $InstalledExe -or (Get-ListenerPid $LocalPort) -ne $process.Id) {
                throw "健康检查未命中新启动的 Agent 进程"
            }
        }
        $runtimeState = Assert-InstalledRuntime $RuntimeRelease.Manifest
        Write-Host "部署完成：version=$ActiveVersion runtime=$($runtimeState.version) port=$LocalPort started=$(-not $NoStart)"
        # Keep backups for explicit rollback; cleanup must never trigger rollback.
        Write-Host "Agent 备份：$($artifacts[0].Backup)；Runtime 备份：$runtimeBackup"
    }
    catch {
        $deploymentError = $_
        Write-Warning "本地部署失败，正在恢复：$($deploymentError.Exception.Message)"
        function Invoke-RecoveryStep([string]$name, [scriptblock]$action) {
            try { & $action }
            catch {
                $rollbackErrors.Add("${name}：$($_.Exception.Message)")
                Write-Warning "恢复步骤失败（$name）：$($_.Exception.Message)"
            }
        }
        # Each recovery step is independent. A locked file must not prevent the
        # script from restoring other files and trying to bring the service back.
        if (@($artifacts | Where-Object { $_.Touched }).Count -gt 0) {
            Invoke-RecoveryStep "停止部署进程" {
                if ($process) { Stop-Agent $process.Id }
                Stop-InstalledAgentProcesses
            }
        }
        foreach ($artifact in $artifacts) {
            if (-not $artifact.Touched) { continue }
            Invoke-RecoveryStep "恢复 $($artifact.Target)" {
                if ($artifact.Had) {
                    Copy-InstalledExeWithRetry -Source $artifact.Backup -Destination $artifact.Target
                    if ((Get-FileHash -LiteralPath $artifact.Backup).Hash -ne (Get-FileHash -LiteralPath $artifact.Target).Hash) {
                        throw "恢复文件 SHA-256 不一致"
                    }
                }
                elseif (Test-Path -LiteralPath $artifact.Target -PathType Leaf) {
                    Remove-Item -LiteralPath $artifact.Target -Force
                }
            }
        }
        if ($runtimeTouched) {
            Invoke-RecoveryStep "恢复 Runtime 包" {
                Remove-RuntimeDirectorySafely $RuntimeBundleDestination
                if ($hadRuntimeBundle) {
                    Move-Item -LiteralPath $runtimeBackup -Destination $RuntimeBundleDestination -ErrorAction Stop
                }
            }
        }
        if ($versionTouched) {
            Invoke-RecoveryStep "恢复 active-version" {
                if ($hadActiveVersion) { [IO.File]::WriteAllBytes($versionFile, $previousActiveVersion) }
                elseif (Test-Path -LiteralPath $versionFile -PathType Leaf) { Remove-Item -LiteralPath $versionFile -Force }
            }
        }
        # NoStart applies only to a successful deployment. Failure restores the
        # availability that existed before stopping, even when NoStart was given.
        if ($wasRunning -and $stopAttempted) {
            Invoke-RecoveryStep "恢复 Agent 可用性" {
                $existingHealth = $null
                try { $existingHealth = Invoke-RestMethod "http://127.0.0.1:$LocalPort/health" -TimeoutSec 2 } catch {}
                if (-not $existingHealth -or $existingHealth.status -ne "online") {
                    $deploymentExe = $InstalledExe
                    try {
                        $InstalledExe = $recoveryExe
                        [void](Start-Agent)
                    }
                    finally { $InstalledExe = $deploymentExe }
                    if (-not (Wait-AgentHealthy)) { throw "恢复后的 Agent 健康检查失败" }
                }
            }
        }
        $detail = if ($rollbackErrors.Count) { "；恢复尚有错误：$($rollbackErrors -join '；')" } else { "；已恢复部署前状态" }
        throw "本地部署失败：$($deploymentError.Exception.Message)$detail"
    }
    finally {
        if (Test-Path -LiteralPath $runtimeStaging -PathType Container) {
            try { Remove-RuntimeDirectorySafely $runtimeStaging }
            catch { Write-Warning "清理 Runtime 暂存失败：$($_.Exception.Message)" }
        }
    }
}

Invoke-LocalDeployment
}
finally {
    if ($deploymentLock) { $deploymentLock.Dispose() }
}
