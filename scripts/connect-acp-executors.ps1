[CmdletBinding()]
param(
    [string]$AgentExecutable = '',
    [ValidateSet('all', 'claude', 'opencode')][string]$Executor = 'all',
    [string]$NpmRegistry = 'https://registry.npmjs.org'
)
$ErrorActionPreference = 'Stop'
$agentDirectory = Join-Path $env:LOCALAPPDATA 'HiMindAgent'
if (-not $AgentExecutable) {
    $activeVersion = (Get-Content -LiteralPath (Join-Path $agentDirectory 'active-version') -Raw).Trim()
    $AgentExecutable = Join-Path $agentDirectory "versions/$activeVersion/himind-agent.exe"
}
if (-not (Test-Path -LiteralPath $AgentExecutable -PathType Leaf)) { throw '找不到 Agent 程序，请指定 -AgentExecutable。' }
$profiles = @(& $AgentExecutable runtime acp-profile list | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0) { throw '无法读取 ACP 执行器配置。' }

function Save-Executor($provider, $name, $executable, $arguments, $version) {
    # Existing profiles may contain private routing or credentials. Keep them intact.
    if ($profiles | Where-Object { $_.provider_id -eq "acp.$provider" }) {
        Write-Output "保留现有 acp.$provider 配置；可在 Agent 的 AI 客户端设置中调整。"
        return
    }
    $argumentJson = ConvertTo-Json -InputObject @($arguments) -Compress
    & $AgentExecutable runtime acp-profile set $provider $executable $argumentJson --name $name --version $version --permission prompt | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "保存 $name 执行器失败。" }
    Write-Output "已接入 $name。默认逐次询问；协作任务选择完全访问后自动批准工具请求。"
}

if ($Executor -in @('all', 'claude')) {
    $nodeCommand = Get-Command node -ErrorAction Stop
    $nodeVersion = (& $nodeCommand.Source --version).TrimStart('v')
    if ([version]$nodeVersion -lt [version]'22.0.0') { throw 'Claude Agent ACP 需要 Node.js 22 或更新版本。' }
    $adapterVersion = '0.78.0'
    $adapterRoot = Join-Path $agentDirectory "runtimes/acp/claude-agent/$adapterVersion"
    $adapterEntry = Join-Path $adapterRoot 'node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js'
    if (-not (Test-Path -LiteralPath $adapterEntry -PathType Leaf)) {
        # A pinned local install avoids a registry lookup every time a task starts.
        & npm.cmd install --prefix $adapterRoot --ignore-scripts --no-audit --no-fund --registry=$NpmRegistry --fetch-retries=0 --fetch-timeout=20000 "@agentclientprotocol/claude-agent-acp@$adapterVersion"
        if ($LASTEXITCODE -ne 0) { throw 'Claude ACP 适配器安装失败，尚未注册执行器。' }
    }
    Save-Executor 'claude' 'Claude Agent' $nodeCommand.Source @($adapterEntry) $adapterVersion
}

if ($Executor -in @('all', 'opencode')) {
    $openCodeCommand = Get-Command opencode -CommandType Application -ErrorAction SilentlyContinue
    $openCodeExecutable = if ($openCodeCommand) { $openCodeCommand.Source } else { $null }
    if (-not $openCodeExecutable) {
        $cliDirectory = Join-Path $env:APPDATA 'ai.opencode.desktop/cli'
        $candidates = if (Test-Path -LiteralPath $cliDirectory) {
            Get-ChildItem -LiteralPath $cliDirectory -Directory | ForEach-Object {
                $releaseVersion = $_.Name -replace '-(x64|arm64)$', ''
                $parsedVersion = $null
                $cliPath = Join-Path $_.FullName 'opencode-cli.exe'
                if ([version]::TryParse($releaseVersion, [ref]$parsedVersion) -and (Test-Path -LiteralPath $cliPath -PathType Leaf)) {
                    [pscustomobject]@{ Version = $parsedVersion; Path = $cliPath }
                }
            }
        }
        $openCodeExecutable = ($candidates | Sort-Object Version -Descending | Select-Object -First 1).Path
    }
    if (-not $openCodeExecutable) {
        $bundled = Join-Path $env:LOCALAPPDATA 'Programs/ai.opencode.desktop/resources/opencode-cli.exe'
        if (Test-Path -LiteralPath $bundled -PathType Leaf) { $openCodeExecutable = $bundled }
    }
    if (-not $openCodeExecutable) { throw '找不到 OpenCode CLI，请先安装 OpenCode。' }
    $openCodeVersion = (& $openCodeExecutable --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'OpenCode 无法启动。' }
    Save-Executor 'opencode' 'OpenCode' $openCodeExecutable @('acp') $openCodeVersion
}
