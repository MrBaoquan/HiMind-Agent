[CmdletBinding()]
param()
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'build-support.ps1')
$ReuseRuntime = $false
$deploymentManifest = $null
$repoRoot = Split-Path -Parent $PSScriptRoot
$sourcePath = Join-Path $PSScriptRoot "deploy-local.ps1"
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($sourcePath, [ref]$null, [ref]$errors)
if ($errors.Count) { throw ($errors -join "`n") }
# Load actual functions without executing the build or touching the real installation.
$names = @("Start-Agent", "Stop-Agent", "Invoke-LocalDeployment", "Remove-RuntimeDirectorySafely", "Resolve-DeploymentStatePath")
foreach ($name in $names) {
    $definition = $ast.Find({ param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
    }, $true)
    if (-not $definition) { throw "Missing function: $name" }
    . ([scriptblock]::Create($definition.Extent.Text))
}
function Assert-True([bool]$condition, [string]$message) {
    if (-not $condition) { throw $message }
}

# Exercise the real Windows stop/wait path against a process owned by this check.
$probe = Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList @("-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 30") -WindowStyle Hidden -PassThru
try {
    Stop-Agent $probe.Id
    Assert-True $probe.HasExited "Stop-Agent failed to terminate the isolated probe"
}
finally {
    if (-not $probe.HasExited) { Stop-Process -Id $probe.Id -Force -ErrorAction SilentlyContinue }
}

# Start-Agent must return only a Process-shaped object, never its log text.
$fixtureRoot = Join-Path $repoRoot ("target/deploy-local-tests-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null
$stateHome = Join-Path $fixtureRoot 'unenrolled home'
$missingState = Join-Path $stateHome 'data/agent-state.json'
Assert-True ((Resolve-DeploymentStatePath '' $stateHome) -eq $missingState) 'Default missing state must be accepted'
Assert-True (-not (Test-Path -LiteralPath $missingState)) 'State resolution must not create enrollment data'
$explicitState = Join-Path $fixtureRoot 'custom state/agent.json'
Assert-True ((Resolve-DeploymentStatePath $explicitState $stateHome) -eq $explicitState) 'Explicit missing state must be accepted'
$invalidStateRejected = $false
try { Resolve-DeploymentStatePath $fixtureRoot $stateHome | Out-Null } catch { $invalidStateRejected = $true }
Assert-True $invalidStateRejected 'Directory state path must be rejected'
$AgentHome = Join-Path $fixtureRoot "start-agent home"
$InstalledExe = "fixture.exe"
$ApiBase = "https://example.invalid"
$LocalPort = 18181
$StatePath = "C:\state directory\agent.json"
function Start-Process {
    param($FilePath, $ArgumentList, [switch]$PassThru, $WindowStyle, $RedirectStandardOutput, $RedirectStandardError)
    Assert-True ($ArgumentList[-1] -eq ('"' + $StatePath + '"')) "State path must be quoted"
    Assert-True ((Split-Path $RedirectStandardOutput) -eq (Join-Path $AgentHome "logs")) "Stdout must be saved under Agent logs"
    Assert-True ((Split-Path $RedirectStandardError) -eq (Join-Path $AgentHome "logs")) "Stderr must be saved under Agent logs"
    Assert-True ($RedirectStandardOutput -ne $RedirectStandardError) "Output streams require separate files"
    return [pscustomobject]@{ Id = 201 }
}
$startResult = @(Start-Agent)
Assert-True ($startResult.Count -eq 1 -and $startResult[0].Id -eq 201) "Start-Agent polluted the success stream"
Remove-Item Function:\Start-Process

# Fault injection uses actual on-disk artifact copies in an isolated workspace.
$cases = @(
    "success", "no-start-success", "preflight-failure", "stop-failure",
    "companion-stop-failure", "partial-copy-failure", "runtime-install-failure",
    "health-failure", "rollback-copy-failure", "no-start-failure", "foreign-port",
    "previous-version-failure", "metadata-commit-failure"
)
$results = @()
foreach ($case in $cases) {
    $caseRoot = Join-Path $fixtureRoot $case
    $AgentHome = Join-Path $caseRoot "agent home"
    $VersionDir = Join-Path $AgentHome "versions/0.3.92"
    $InstalledExe = Join-Path $VersionDir "himind-agent.exe"
    $InstalledMcp = Join-Path $VersionDir "himind-agent-mcp.exe"
    $InstalledLauncher = Join-Path $AgentHome "himind-agent-launcher.exe"
    $InstalledUpdater = Join-Path $AgentHome "himind-agent-updater.exe"
    $RuntimeBundleDestination = Join-Path $AgentHome "resources/runtime"
    $versionFile = Join-Path $AgentHome "active-version"
    $BuiltExe = Join-Path $caseRoot "build/agent.exe"
    $BuiltMcp = Join-Path $caseRoot "build/mcp.exe"
    $BuiltLauncher = Join-Path $caseRoot "build/launcher.exe"
    $BuiltUpdater = Join-Path $caseRoot "build/updater.exe"
    $RuntimeRelease = [pscustomobject]@{ Directory = "fixture-runtime"; Manifest = [pscustomobject]@{ version = "runtime-fixture" } }
    $ActiveVersion = "0.3.92"
    $NoStart = $case -in @("no-start-success", "no-start-failure")
    foreach ($directory in @($VersionDir, $RuntimeBundleDestination, (Split-Path $BuiltExe))) {
        New-Item -ItemType Directory -Path $directory -Force | Out-Null
    }
    foreach ($file in @($InstalledExe, $InstalledMcp, $InstalledLauncher, $InstalledUpdater)) { [IO.File]::WriteAllText($file, "old") }
    foreach ($file in @($BuiltExe, $BuiltMcp, $BuiltLauncher, $BuiltUpdater)) { [IO.File]::WriteAllText($file, "new") }
    [IO.File]::WriteAllText((Join-Path $RuntimeBundleDestination "marker"), "old-runtime")
    [IO.File]::WriteAllText($versionFile, $ActiveVersion)
    $originalExe = $InstalledExe
    if ($case -eq "previous-version-failure") {
        [IO.File]::WriteAllText($versionFile, "0.3.91")
        $originalExe = Join-Path $AgentHome "versions/0.3.91/himind-agent.exe"
        New-Item -ItemType Directory -Path (Split-Path $originalExe) -Force | Out-Null
        [IO.File]::WriteAllText($originalExe, "previous-version")
    }
    $script:listening = $true
    $script:listenPid = 123
    $script:stops = 0
    $script:starts = 0
    $script:copyFailure = $false
    $script:restartedExe = $null
    $deploymentManifest = $null
    if ($case -eq 'metadata-commit-failure') {
        $deploymentManifest = [pscustomobject]@{commit='fixture-commit'}
        $ArtifactManifest = 'new-manifest'
        $managedRoot = Join-Path $caseRoot 'managed'
        $runtimeIdentityPath = Join-Path $AgentHome 'deploy/runtime-identity.json'
        $fixturePinPath = Join-Path $managedRoot ('pins/' + (Get-BuildHash $AgentHome.ToLowerInvariant()) + '.json')
        Write-BuildJson $runtimeIdentityPath @{identity='old'}
        Write-BuildJson $fixturePinPath @{current_manifest='old-manifest';previous_manifest='previous-manifest'}
        $runtimeIdentityBefore = Get-BuildFileHash $runtimeIdentityPath
        $pinBefore = Get-BuildFileHash $fixturePinPath
        $actualWriteJson = (Get-Command Write-BuildJson).ScriptBlock
        function Write-BuildJson {
            param([string]$Path, [object]$Value)
            & $actualWriteJson $Path $Value
            if ($Path -eq $fixturePinPath) { throw 'injected metadata commit failure after write' }
        }
        function Get-InstalledRuntimeIdentity { return @{identity='new'} }
    }
    function Get-ListenerPid([int]$port) { if ($script:listening) { return $script:listenPid }; return 0 }
    function Invoke-RestMethod {
        param($Uri, $TimeoutSec)
        if (-not $script:listening) { throw "offline" }
        $path = if ($case -eq "foreign-port") { "C:\unrelated.exe" } else { $originalExe }
        return [pscustomobject]@{ status = "online"; executable_path = $path }
    }
    function Stop-Agent([int]$processId) {
        $script:stops++
        $script:listening = $false
        if ($case -eq "stop-failure" -and $script:stops -eq 1) { throw "injected stop failure" }
    }
    function Stop-InstalledAgentProcesses {
        if ($case -eq "companion-stop-failure" -and $script:starts -eq 0) { throw "injected companion failure" }
    }
    function Copy-RuntimeBundle {
        param($RuntimeReleaseDirectory, $DestinationDirectory, [switch]$RequireExactPackage)
        if ($case -eq "preflight-failure") { throw "injected staging failure" }
        New-Item -ItemType Directory -Path $DestinationDirectory -Force | Out-Null
        [IO.File]::WriteAllText((Join-Path $DestinationDirectory "marker"), "new-runtime")
    }
    function Copy-InstalledExeWithRetry {
        param($Source, $Destination)
        if ($case -eq "rollback-copy-failure" -and $Source -like "*.bak-*" -and $Destination -eq $InstalledMcp) {
            throw "injected restore failure"
        }
        Copy-Item -LiteralPath $Source -Destination $Destination -Force
        if ($case -in @("partial-copy-failure", "rollback-copy-failure") -and $Source -eq $BuiltMcp -and -not $script:copyFailure) {
            $script:copyFailure = $true
            throw "injected partial copy"
        }
    }
    function Install-RuntimeBundle {
        param($Manifest)
        if ($case -in @("runtime-install-failure", "no-start-failure", "previous-version-failure")) {
            throw "injected runtime install failure"
        }
    }
    function Start-Agent {
        $script:starts++
        $script:listening = $true
        $script:listenPid = 200 + $script:starts
        $script:restartedExe = $InstalledExe
        return [pscustomobject]@{ Id = $script:listenPid }
    }
    function Wait-AgentHealthy {
        if ($case -eq "health-failure" -and $script:starts -eq 1) { return $null }
        return [pscustomobject]@{
            status = "online"; version = $ActiveVersion; build = @{ channel = "local"; commit='fixture' }; executable_path = $InstalledExe
        }
    }
    function Assert-InstalledRuntime { param($Manifest) return $Manifest }
    $failure = $null
    try { Invoke-LocalDeployment }
    catch { $failure = $_.Exception.Message }
    $success = $case -in @("success", "no-start-success")
    Assert-True (($null -eq $failure) -eq $success) "Unexpected outcome for ${case}: $failure"
    if ($success) {
        Assert-True ([IO.File]::ReadAllText($InstalledExe) -eq "new") "$case did not install"
        Assert-True ($script:listening -eq (-not $NoStart)) "$case startup policy wrong"
    }
    else {
        Assert-True $script:listening "$case left the original service offline"
        Assert-True ([IO.File]::ReadAllText($InstalledExe) -eq "old") "$case did not restore Agent"
        Assert-True ([IO.File]::ReadAllText((Join-Path $RuntimeBundleDestination "marker")) -eq "old-runtime") "$case did not restore Runtime"
        $expectedVersion = if ($case -eq "previous-version-failure") { "0.3.91" } else { $ActiveVersion }
        Assert-True ([IO.File]::ReadAllText($versionFile) -eq $expectedVersion) "$case changed active-version"
        if ($case -eq "rollback-copy-failure") {
            Assert-True ($failure -like "*injected restore failure*") "Rollback error was hidden"
            Assert-True ($script:starts -eq 1) "File rollback failure prevented recovery startup"
        }
        else {
            Assert-True ([IO.File]::ReadAllText($InstalledMcp) -eq "old") "$case did not restore MCP"
        }
        if ($case -in @("preflight-failure", "foreign-port")) {
            Assert-True ($script:stops -eq 0 -and $script:starts -eq 0) "$case stopped the service before preflight"
        }
        else {
            Assert-True ($script:starts -gt 0) "$case did not attempt service recovery"
            Assert-True ($script:restartedExe -eq $originalExe) "$case restarted wrong installed version"
        }
    }
    if ($case -eq 'metadata-commit-failure') {
        Assert-True ((Get-BuildFileHash $runtimeIdentityPath) -eq $runtimeIdentityBefore) 'Late failure changed Runtime identity'
        Assert-True ((Get-BuildFileHash $fixturePinPath) -eq $pinBefore) 'Late failure changed deployment pin'
        Set-Item Function:Write-BuildJson $actualWriteJson
    }
    $results += [pscustomobject]@{ case = $case; passed = $true; stops = $script:stops; starts = $script:starts; failure = $failure }
}
# The actual OS file lock must reject a concurrent deployment.
$lockPath = Join-Path $fixtureRoot "local.lock"
$lock = [IO.File]::Open($lockPath, "OpenOrCreate", "ReadWrite", "None")
try {
    $rejected = $false
    try { $other = [IO.File]::Open($lockPath, "OpenOrCreate", "ReadWrite", "None"); $other.Dispose() }
    catch { $rejected = $true }
    Assert-True $rejected "Concurrent deployment lock was not exclusive"
}
finally { $lock.Dispose() }
$report = [pscustomobject]@{ passed = $true; cases = $results; unenrolled_state_path = "passed"; start_process_return = "passed"; real_windows_stop = "passed"; exclusive_lock = "passed"; fixtures = $fixtureRoot }
$report | ConvertTo-Json -Depth 8
