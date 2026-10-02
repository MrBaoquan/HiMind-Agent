#requires -Version 7
<#
.SYNOPSIS
  验证「一个 Agent 同时服务多个 HiMind AI 工作区会话」的会话身份解析。

.DESCRIPTION
  真实拉起两个 himind-agent-mcp.exe 进程（各自带不同的 HIMIND_AI_WORKSPACE），
  按 MCP 协议依次调用 initialize / capability.catalog.activate / tools/call，检查：

  1. 每个会话不传 workspace_root 时，解析到的是自己的会话工作区；
  2. 显式传 workspace_root 时以传入值为准，不受另一个会话影响；
  3. 每个会话各自绑定工作区时，绑定集合累加、互不覆盖；
  4. 扩展开发能力缺少 workspace_root 时给出可执行的阻塞提示（而不是默默用别人的目录）。

  两种身份来源都要成立：DSH 的每个会话有独立的 MCP 伴生进程（进程环境变量按会话隔离），
  而显式 workspace_root 是跨进程、跨客户端唯一可靠的会话身份。

  脚本会把工作区绑定文件和扩展工作区配置指向临时目录，不会改动当前用户真实的绑定。

.PARAMETER Binary
  himind-agent-mcp.exe 路径，默认取仓库 target\release 下的制品。

.EXAMPLE
  pwsh -File scripts/verify-multisession-workspace.ps1
#>
[CmdletBinding()]
param(
    [string] $Binary = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\himind-agent-mcp.exe'),
    [switch] $KeepTemp
)

$ErrorActionPreference = 'Stop'

function Get-CanonicalPath {
    param([string] $Path)
    # $env:TEMP 之类的目录在本机可能带着 8.3 短名（C:\Users\ADMINI~1\...），
    # 而 Agent 侧一律按长名做规范化比较。这里统一取磁盘上的真实长名。
    return (Get-Item -LiteralPath $Path).FullName
}

if (-not (Test-Path -LiteralPath $Binary)) {
    throw "找不到 MCP 伴生进程：$Binary（先执行 cargo build --release --bin himind-agent-mcp）"
}
$Binary = Get-CanonicalPath $Binary

$stamp = Get-Date -Format 'yyyyMMdd-HHmmssfff'
$rootDir = Join-Path $env:TEMP "himind-multisession-$stamp"
$stateDir = Join-Path $rootDir 'state'
New-Item -ItemType Directory -Force -Path $stateDir | Out-Null
$rootDir = Get-CanonicalPath $rootDir
$stateDir = Join-Path $rootDir 'state'
$stateFile = Join-Path $stateDir 'agent-state.json'
$bindingFile = Join-Path $rootDir 'extensions-bindings.json'
$workspaceConfigFile = Join-Path $rootDir 'extensions-workspace.json'

function New-PluginWorkspace {
    param([string] $Name, [string] $ExtensionId)
    $path = Join-Path $rootDir $Name
    New-Item -ItemType Directory -Force -Path $path | Out-Null
    $manifest = [ordered]@{
        id          = $ExtensionId
        name        = "多工作区验证插件 $Name"
        description = '验证多工作区会话并发创作'
        version     = '0.1.0'
    } | ConvertTo-Json -Depth 4
    Set-Content -LiteralPath (Join-Path $path 'plugin.json') -Value $manifest -Encoding utf8
    return Get-CanonicalPath $path
}

function Start-AgentSession {
    param([string] $Tag, [string] $Workspace)
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Binary
    foreach ($argument in @('--mcp', '--mode', 'independent', '--state', $stateFile)) {
        $startInfo.ArgumentList.Add($argument)
    }
    $startInfo.RedirectStandardInput = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.UseShellExecute = $false
    $startInfo.WorkingDirectory = $Workspace
    # 模拟 DSH：每个会话进程带上自己的会话环境变量。父进程改过 env 之后启动的子进程
    # 各自继承一份快照，互不干扰 —— 这正是多会话在生产里的实际形态。
    $env:HIMIND_AI_WORKSPACE = $Workspace
    # 验证过程只写临时状态，绝不碰用户真实的扩展工作区绑定与配置。
    $env:HIMIND_EXTENSIONS_BINDING_FILE = $bindingFile
    $env:HIMIND_EXTENSIONS_WORKSPACE_FILE = $workspaceConfigFile
    $process = [System.Diagnostics.Process]::Start($startInfo)
    return [pscustomobject]@{
        Tag       = $Tag
        Workspace = $Workspace
        Process   = $process
        NextId    = 1
    }
}

function Send-Rpc {
    param($Session, [string] $Method, $Params, [switch] $Notification)
    $message = [ordered]@{ jsonrpc = '2.0' } 
    if (-not $Notification) {
        $message.id = $Session.NextId
        $Session.NextId++
    }
    $message.method = $Method
    if ($null -ne $Params) { $message.params = $Params }
    $payload = $message | ConvertTo-Json -Depth 12 -Compress
    $Session.Process.StandardInput.WriteLine($payload)
    $Session.Process.StandardInput.Flush()
    return $message.id
}

function Read-RpcResult {
    param($Session, [int] $Id, [int] $TimeoutSeconds = 60)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        $line = $Session.Process.StandardOutput.ReadLine()
        if ($null -eq $line) {
            throw "[$($Session.Tag)] MCP 伴生进程提前退出"
        }
        if ($line.Trim().Length -eq 0) { continue }
        $message = $line | ConvertFrom-Json
        if ($null -eq $message.id) { continue }  # 通知（notifications/*）
        if ([int] $message.id -ne $Id) { continue }
        if ($message.error) {
            return [pscustomobject]@{ Ok = $false; Error = $message.error.message }
        }
        return [pscustomobject]@{ Ok = $true; Result = $message.result }
    }
    throw "[$($Session.Tag)] 等待响应超时：id=$Id"
}

function Invoke-Capability {
    param($Session, [string] $CapabilityId, $Arguments)
    $id = Send-Rpc -Session $Session -Method 'tools/call' -Params ([ordered]@{
            name      = $CapabilityId
            arguments = $Arguments
        })
    $response = Read-RpcResult -Session $Session -Id $id
    if (-not $response.Ok) {
        return [pscustomobject]@{ IsError = $true; Text = $response.Error; Data = $null }
    }
    $result = $response.Result
    $text = ''
    if ($result.content) { $text = ($result.content | Where-Object { $_.type -eq 'text' } | Select-Object -First 1).text }
    return [pscustomobject]@{
        IsError = [bool] $result.isError
        Text    = $text
        Data    = $result.structuredContent
    }
}

function Initialize-Session {
    param($Session)
    $id = Send-Rpc -Session $Session -Method 'initialize' -Params ([ordered]@{
            protocolVersion = '2025-11-25'
            clientInfo      = [ordered]@{ name = 'verify-multisession'; version = '0.1.0' }
        })
    $response = Read-RpcResult -Session $Session -Id $id
    if (-not $response.Ok) { throw "[$($Session.Tag)] initialize 失败：$($response.Error)" }
    Send-Rpc -Session $Session -Method 'notifications/initialized' -Params $null -Notification | Out-Null
    $activation = Invoke-Capability -Session $Session -CapabilityId 'capability.catalog.activate' -Arguments ([ordered]@{
            ids = @(
                'extension.workspace.current',
                'extension.workspace.bind',
                'extension.workspace.clear',
                'extension.authoring.preflight',
                'extension.plugin.validate'
            )
        })
    if ($activation.IsError) { throw "[$($Session.Tag)] 激活扩展能力失败：$($activation.Text)" }
}

$workspaceA = New-PluginWorkspace -Name 'ws-alpha' -ExtensionId 'com.himind.e2e.multisession-alpha'
$workspaceB = New-PluginWorkspace -Name 'ws-beta' -ExtensionId 'com.himind.e2e.multisession-beta'

$sessionA = Start-AgentSession -Tag 'alpha' -Workspace $workspaceA
$sessionB = Start-AgentSession -Tag 'beta' -Workspace $workspaceB

$checks = [System.Collections.Generic.List[object]]::new()
function Assert-Equal {
    param([string] $Name, $Expected, $Actual)
    $passed = ($Expected -eq $Actual)
    $checks.Add([pscustomobject]@{
            Check    = $Name
            Expected = "$Expected"
            Actual   = "$Actual"
            Passed   = $passed
        })
    if (-not $passed) { Write-Host "  ✗ $Name（期望 $Expected，实际 $Actual）" -ForegroundColor Red }
    else { Write-Host "  ✓ $Name" -ForegroundColor Green }
}

function Assert-Contains {
    param([string] $Name, [string] $Needle, [string] $Haystack)
    $passed = $Haystack -and $Haystack.Contains($Needle)
    $checks.Add([pscustomobject]@{
            Check    = $Name
            Expected = "包含 «$Needle»"
            Actual   = if ($Haystack) { $Haystack } else { '<空>' }
            Passed   = $passed
        })
    if (-not $passed) { Write-Host "  ✗ $Name（未包含 «$Needle»）" -ForegroundColor Red }
    else { Write-Host "  ✓ $Name" -ForegroundColor Green }
}

try {
    Initialize-Session -Session $sessionA
    Initialize-Session -Session $sessionB

    Write-Host "`n[1] 会话身份：不传 workspace_root 时各自解析到自己的会话工作区" -ForegroundColor Cyan
    $currentA = Invoke-Capability -Session $sessionA -CapabilityId 'extension.workspace.current' -Arguments @{}
    $currentB = Invoke-Capability -Session $sessionB -CapabilityId 'extension.workspace.current' -Arguments @{}
    Assert-Equal '会话 A 解析到 ws-alpha' $workspaceA $currentA.Data.workspace_root
    Assert-Equal '会话 B 解析到 ws-beta' $workspaceB $currentB.Data.workspace_root

    Write-Host "`n[2] 显式 workspace_root 优先：不受另一个会话影响" -ForegroundColor Cyan
    $crossed = Invoke-Capability -Session $sessionA -CapabilityId 'extension.workspace.current' -Arguments ([ordered]@{
            workspace_root = $workspaceB
        })
    Assert-Equal '会话 A 显式指定 ws-beta 时解析到 ws-beta' $workspaceB $crossed.Data.workspace_root
    Assert-Equal '显式指定时来源标记为 request' 'request' $crossed.Data.source
    Assert-Equal '显式指定时不再标记为历史绑定' $false $crossed.Data.bound

    Write-Host "`n[3] 创作预检按会话工作区通过" -ForegroundColor Cyan
    $preflight = Invoke-Capability -Session $sessionB -CapabilityId 'extension.authoring.preflight' -Arguments ([ordered]@{
            kind           = 'plugin'
            workspace_root = $workspaceB
        })
    $blockerCodes = @()
    if ($preflight.Data -and $preflight.Data.blockers) { $blockerCodes = $preflight.Data.blockers | ForEach-Object { $_.code } }
    Assert-Equal '预检没有工作区类阻塞' $false ([bool] ($blockerCodes -like 'extension_workspace*'))
    if ($preflight.Data.state -eq 'passed') {
        Assert-Equal '预检回读的工作区是会话自己的 ws-beta' $workspaceB $preflight.Data.workspace.root
        Assert-Equal '预检的工作区来源是按次传入' 'request' $preflight.Data.workspace.source
    }
    else {
        # 工具链类阻塞（未安装/版本过低的开发工具插件与 Skill）和本场景无关，
        # 这里只确认阻塞原因不是工作区串了门。
        $toolchainBlockers = @($preflight.Data.blockers | Where-Object { $_.stage -eq 'toolchain' } | ForEach-Object { $_.code })
        Write-Host "  · 预检被工具链阻塞，跳过工作区回读断言：$($toolchainBlockers -join '、')" -ForegroundColor Yellow
        # 阻塞的处置建议是给人看的一手材料：派生阻塞有没有指回根因，看这里最快。
        foreach ($blocker in $preflight.Data.blockers) {
            Write-Host ("      {0}：{1}" -f $blocker.code, $blocker.remediation) -ForegroundColor DarkGray
        }
    }

    Write-Host "`n[4] 多会话绑定累加：两个会话各绑自己的工作区，互不覆盖" -ForegroundColor Cyan
    $bindA = Invoke-Capability -Session $sessionA -CapabilityId 'extension.workspace.bind' -Arguments ([ordered]@{
            workspace_root = $workspaceA
        })
    $bindB = Invoke-Capability -Session $sessionB -CapabilityId 'extension.workspace.bind' -Arguments ([ordered]@{
            workspace_root = $workspaceB
        })
    Assert-Equal '会话 A 绑定成功' $false $bindA.IsError
    Assert-Equal '会话 B 绑定成功' $false $bindB.IsError
    if ($bindA.IsError -or $bindB.IsError) {
        Write-Host "  · A 响应：$($bindA.Text)" -ForegroundColor DarkGray
        Write-Host "  · B 响应：$($bindB.Text)" -ForegroundColor DarkGray
    }
    # 绑定是兜底集合：会话环境变量仍在，各自解析结果不能因为对方绑定而改变。
    $afterBindA = Invoke-Capability -Session $sessionA -CapabilityId 'extension.workspace.current' -Arguments @{}
    Assert-Equal '绑定之后会话 A 仍解析到 ws-alpha' $workspaceA $afterBindA.Data.workspace_root
    $cleared = Invoke-Capability -Session $sessionA -CapabilityId 'extension.workspace.clear' -Arguments @{}
    Assert-Equal '两个会话的绑定都留在集合里（互不覆盖）' 2 @($cleared.Data.removed_workspace_roots).Count

    Write-Host "`n[5] 缺少 workspace_root 时给出可执行的阻塞提示" -ForegroundColor Cyan
    $blocked = Invoke-Capability -Session $sessionA -CapabilityId 'extension.plugin.validate' -Arguments @{}
    Assert-Equal '扩展开发能力缺少 workspace_root 被判阻塞' $true $blocked.IsError
    Assert-Contains '阻塞提示里点名 workspace_root' 'workspace_root' $blocked.Text
}
finally {
    foreach ($session in @($sessionA, $sessionB)) {
        if ($session -and $session.Process -and -not $session.Process.HasExited) {
            try {
                Send-Rpc -Session $session -Method 'exit' -Params $null -Notification | Out-Null
                $session.Process.StandardInput.Close()
                if (-not $session.Process.WaitForExit(5000)) { $session.Process.Kill($true) }
            }
            catch { $session.Process.Kill($true) }
        }
    }
    if (-not $KeepTemp) { Remove-Item -LiteralPath $rootDir -Recurse -Force -ErrorAction SilentlyContinue }
    else { Write-Host "`n临时目录保留在 $rootDir" -ForegroundColor DarkGray }
}

$failed = @($checks | Where-Object { -not $_.Passed })
Write-Host ''
$checks | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
if ($failed.Count -gt 0) {
    Write-Host "多工作区会话验证失败：$($failed.Count)/$($checks.Count) 项未通过" -ForegroundColor Red
    exit 1
}
Write-Host "多工作区会话验证通过：$($checks.Count)/$($checks.Count)" -ForegroundColor Green
exit 0
