#requires -Version 7
<#
.SYNOPSIS
  验证单实例守卫：同一个 profile 只能有一个实例，并行实例必须显式隔离。

.DESCRIPTION
  产品默认按「tauri identifier + 运行 profile」加锁（见
  himind-agent/src/app/single_instance.rs）。所以：

  - 开发 profile 与已安装的生产 Agent 各占一个键，可以并存、互不顶替；
  - 但同一个 profile 里的第二个进程只会把命令行转发给第一个进程然后退出。

  这个脚本用独立 profile + 临时 HIMIND_AGENT_HOME + 独立端口真实起进程，
  验证三件事：

  1. 首个实例能正常起服务（/health 上线）；
  2. 同 profile 再起一个：立刻退出（退出码 0），首个实例不受影响；
  3. 显式设置 HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE=1、并换 --state 与
     --local-port 后，第二个实例能与第一个并存。

  第 3 条是唯一允许并行的入口，只给验证用；产品自身默认单实例。
  脚本只碰自己的临时目录，不动开发 profile 的真实状态，跑完自动清理。

.PARAMETER Executable
  himind-agent.exe 路径，默认取仓库 target\release 下的制品。

.PARAMETER Profile
  验证专用 profile 名，默认 verify，与 development / production 分开互不干扰。

.PARAMETER KeepTemp
  保留临时目录，便于事后查看日志。

.EXAMPLE
  pwsh -File himind-agent/scripts/verify-parallel-instance.ps1
#>
[CmdletBinding()]
param(
    [string] $Executable = (Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) 'himind-agent\target\release\himind-agent.exe'),
    [string] $Profile = 'verify',
    [string] $ApiBase = 'http://127.0.0.1:18083',
    [int] $LaunchTimeoutSeconds = 90,
    [switch] $KeepTemp
)

$ErrorActionPreference = 'Stop'

if ($Profile -notmatch '^[A-Za-z0-9._-]{1,48}$' -or $Profile -in @('production', 'default', 'development')) {
    throw "验证 profile 必须是独立的 path-safe 名字（不能是 production / default / development）：$Profile"
}
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) {
    throw "找不到 Agent 制品：$Executable（先执行 cargo build --release --bin himind-agent）"
}
$Executable = (Get-Item -LiteralPath $Executable).FullName
$WorkingDirectory = Split-Path -Parent $Executable

$stamp = Get-Date -Format 'yyyyMMdd-HHmmssfff'
$rootDir = Join-Path $env:TEMP "himind-parallel-verify-$stamp"
$homeDir = Join-Path $rootDir 'home'
$stateDir = Join-Path $rootDir 'state'
New-Item -ItemType Directory -Force -Path $homeDir, $stateDir | Out-Null
$rootDir = (Get-Item -LiteralPath $rootDir).FullName
$homeDir = Join-Path $rootDir 'home'
$stateDir = Join-Path $rootDir 'state'

function Get-FreeTcpPort {
    for ($attempt = 0; $attempt -lt 50; $attempt++) {
        $candidate = Get-Random -Minimum 18100 -Maximum 18900
        $inUse = Get-NetTCPConnection -LocalPort $candidate -State Listen -ErrorAction SilentlyContinue
        if (-not $inUse) { return $candidate }
    }
    throw '找不到空闲端口。'
}

function Wait-AgentHealthy {
    param([int] $Port, [int] $Seconds)
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        try {
            $health = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/health" -TimeoutSec 3
            if ($health.status -eq 'online') { return $true }
        }
        catch {
        }
        Start-Sleep -Milliseconds 800
    }
    return $false
}

function Test-ProcessAlive {
    param([int] $ProcessId)
    return [bool](Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)
}

function Start-Agent {
    param(
        [int] $Port,
        [string] $StateFile,
        [switch] $AllowParallel,
        [switch] $SuppressWindow
    )
    $arguments = @(
        '--api', $ApiBase,
        '--mode', 'connected',
        '--local-app',
        '--local-port', "$Port",
        '--state', $StateFile
    )
    if ($SuppressWindow) {
        # 转发到已有实例时，回调只在拿不到有效协议参数时才 show_main_window；
        # 这里给一个不会被识别的协议 URL，避免验证过程弹出窗口。
        $arguments += @('--protocol-url', 'himind-agent://verify-noop')
    }

    $previousProfile = $env:HIMIND_AGENT_PROFILE
    $previousHome = $env:HIMIND_AGENT_HOME
    $previousParallel = $env:HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE
    try {
        $env:HIMIND_AGENT_PROFILE = $Profile
        $env:HIMIND_AGENT_HOME = $homeDir
        if ($AllowParallel) { $env:HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE = '1' }
        else { Remove-Item Env:HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE -ErrorAction SilentlyContinue }
        return Start-Process -FilePath $Executable -ArgumentList $arguments `
            -WorkingDirectory $WorkingDirectory -WindowStyle Hidden -PassThru
    }
    finally {
        if ($null -eq $previousProfile) { Remove-Item Env:HIMIND_AGENT_PROFILE -ErrorAction SilentlyContinue }
        else { $env:HIMIND_AGENT_PROFILE = $previousProfile }
        if ($null -eq $previousHome) { Remove-Item Env:HIMIND_AGENT_HOME -ErrorAction SilentlyContinue }
        else { $env:HIMIND_AGENT_HOME = $previousHome }
        if ($null -eq $previousParallel) { Remove-Item Env:HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE -ErrorAction SilentlyContinue }
        else { $env:HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE = $previousParallel }
    }
}

$primaryPort = Get-FreeTcpPort
$parallelPort = Get-FreeTcpPort
while ($parallelPort -eq $primaryPort) { $parallelPort = Get-FreeTcpPort }

$primaryState = Join-Path $stateDir 'primary\agent-state.json'
$guardState = Join-Path $stateDir 'guard\agent-state.json'
$parallelState = Join-Path $stateDir 'parallel\agent-state.json'
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $primaryState), (Split-Path -Parent $guardState), (Split-Path -Parent $parallelState) | Out-Null

$failures = [System.Collections.Generic.List[string]]::new()
$primary = $null
$parallel = $null
$guard = $null

Write-Host "profile=$Profile home=$homeDir primary-port=$primaryPort parallel-port=$parallelPort"

try {
    Write-Host '[1/3] 启动首个实例（守卫生效）...'
    $primary = Start-Agent -Port $primaryPort -StateFile $primaryState
    if (-not (Wait-AgentHealthy -Port $primaryPort -Seconds $LaunchTimeoutSeconds)) {
        throw "首个实例未在 $LaunchTimeoutSeconds 秒内就绪：http://127.0.0.1:$primaryPort/health"
    }
    Write-Host "      OK pid=$($primary.Id)"

    Write-Host '[2/3] 同 profile 再起一个（预期：转发后立即退出，退出码 0）...'
    $guard = Start-Agent -Port $parallelPort -StateFile $guardState -SuppressWindow
    $guardExited = $guard.WaitForExit(30000)
    if (-not $guardExited) {
        $failures.Add("守卫失效：同 profile 的第二个实例没有退出（pid=$($guard.Id)）。")
    }
    elseif ($guard.ExitCode -ne 0) {
        $failures.Add("守卫失败：第二个实例退出码为 $($guard.ExitCode)，预期 0。")
    }
    # 真起来过的实例一定会写好状态文件、并占用自己的端口；两者都没有才说明
    # 它确实是在建立服务之前就被守卫拦下了，而不是恰好失败退出。
    if (Test-Path -LiteralPath $guardState) {
        $failures.Add("守卫失效：第二个实例写出了自己的状态文件 $guardState。")
    }
    if (Get-NetTCPConnection -LocalPort $parallelPort -State Listen -ErrorAction SilentlyContinue) {
        $failures.Add("守卫失效：第二个实例占用了端口 $parallelPort。")
    }
    if (-not (Test-ProcessAlive -ProcessId $primary.Id)) {
        $failures.Add('首个实例在转发过程中退出了。')
    }
    if (-not (Wait-AgentHealthy -Port $primaryPort -Seconds 10)) {
        $failures.Add('首个实例在转发后不再健康。')
    }
    if ($failures.Count -eq 0) {
        Write-Host '      OK 第二个实例已退出，首个实例仍然健康'
    }

    Write-Host '[3/3] 并行实例（ALLOW_PARALLEL_INSTANCE=1 + 独立 state/port）...'
    $parallel = Start-Agent -Port $parallelPort -StateFile $parallelState -AllowParallel
    if (-not (Wait-AgentHealthy -Port $parallelPort -Seconds $LaunchTimeoutSeconds)) {
        $failures.Add("并行实例未在 $LaunchTimeoutSeconds 秒内就绪：http://127.0.0.1:$parallelPort/health")
    }
    if (-not (Test-ProcessAlive -ProcessId $primary.Id)) {
        $failures.Add('并行实例把首个实例顶掉了。')
    }
    if (-not (Test-ProcessAlive -ProcessId $parallel.Id)) {
        $failures.Add('并行实例没有存活。')
    }
    if ($failures.Count -eq 0) {
        Write-Host "      OK 两个实例并存 pid=$($primary.Id) / $($parallel.Id)"
    }
}
finally {
    foreach ($process in @($guard, $parallel, $primary)) {
        if ($process -and (Test-ProcessAlive -ProcessId $process.Id)) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        }
    }
    Start-Sleep -Milliseconds 800
    if (-not $KeepTemp) {
        Remove-Item -LiteralPath $rootDir -Recurse -Force -ErrorAction SilentlyContinue
    }
    else {
        Write-Host "临时目录保留在：$rootDir"
    }
}

if ($failures.Count -gt 0) {
    Write-Host ''
    Write-Host "验证未通过（$($failures.Count) 项）：" -ForegroundColor Red
    foreach ($failure in $failures) { Write-Host "  - $failure" -ForegroundColor Red }
    exit 1
}

Write-Host ''
Write-Host '单实例守卫验证通过：同 profile 单实例，并行实例需显式隔离。' -ForegroundColor Green
exit 0
