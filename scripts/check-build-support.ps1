#Requires -Version 7.0
[CmdletBinding()]
param([string]$FixtureRoot)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'build-support.ps1')
if (-not $FixtureRoot) { $FixtureRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\build-support-tests' }
$fixture = Assert-BuildPath (Join-Path $FixtureRoot ([Guid]::NewGuid().ToString('N'))) -Within $FixtureRoot
[IO.Directory]::CreateDirectory($fixture) | Out-Null
$results = [Collections.Generic.List[string]]::new()
function Assert-Check([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Assert-Rejected([scriptblock]$Action, [string]$Message) {
    $rejected = $false
    try { & $Action | Out-Null } catch { $rejected = $true }
    Assert-Check $rejected $Message
}
$unowned = Join-Path $fixture 'unowned'
[IO.Directory]::CreateDirectory($unowned) | Out-Null
[IO.File]::WriteAllText((Join-Path $unowned 'keep.txt'),'user file')
Assert-Rejected { Get-BuildRoot $unowned } 'Adopted a nonempty unowned directory'
$results.Add('unowned-root-rejected')
Assert-Rejected { Assert-BuildPath (Join-Path $fixture '..\outside') -Within $fixture } 'Path escape accepted'
$results.Add('path-escape-rejected')
$root = Get-BuildRoot (Join-Path $fixture 'managed')
$lock = Enter-BuildLock $root
try { Assert-Rejected { Enter-BuildLock $root } 'Concurrent writer acquired an exclusive lock' }
finally { $lock.Dispose() }
$again = Enter-BuildLock $root
$again.Dispose()
$results.Add('exclusive-lock-and-release')
Assert-Rejected { Assert-BuildCapacity @(@{Path=$fixture;Bytes=[long]::MaxValue - 2GB}) } 'Impossible capacity accepted'
$results.Add('capacity-rejected-before-action')
$before = $env:CARGO_TARGET_DIR
$prior = Enter-BuildEnvironment $root 'dev'
try {
    Assert-Check ($env:CARGO_INCREMENTAL -eq '1' -and $env:CARGO_TARGET_DIR.StartsWith($root)) 'Dev slot is not isolated/incremental'
    Assert-Check ($env:TEMP.StartsWith($root) -and $env:npm_config_cache.StartsWith($root)) 'Temporary/cache output escapes managed root'
}
finally { Exit-BuildEnvironment $prior }
Assert-Check ($env:CARGO_TARGET_DIR -eq $before) 'Environment leaked to caller'
$results.Add('isolated-environment-restored')
$context = [pscustomobject]@{root=$root; evidence=Join-Path $root 'evidence\one';steps=[Collections.Generic.List[object]]::new()}
$product = Join-Path $root 'product.txt'
$script:executions = 0
$action = { $script:executions++; [IO.File]::WriteAllText($product,'compiled result') }
$outputs = { @{ file=Get-BuildFileHash $product } }
Invoke-BuildStep $context 'fixture' @{source='v1'; flags='debug'} $action $outputs
$original = $context.steps[0].path
$context.evidence = Join-Path $root 'evidence\two'
Invoke-BuildStep $context 'fixture' @{source='v1'; flags='debug'} $action $outputs
Assert-Check ($script:executions -eq 1 -and $context.steps[-1].disposition -eq 'REUSED') 'Unchanged successful result did not reuse'
$receipt = Get-Content -LiteralPath $context.steps[-1].path -Raw | ConvertFrom-Json
Assert-Check ($receipt.source.path -eq $original) 'Reuse hid the original execution source'
$results.Add('unchanged-reuse-retains-original-source')
[IO.File]::WriteAllText($product,'corrupt')
$context.evidence = Join-Path $root 'evidence\three'
Invoke-BuildStep $context 'fixture' @{source='v1'; flags='debug'} $action $outputs
Assert-Check ($script:executions -eq 2 -and [IO.File]::ReadAllText($product) -eq 'compiled result') 'Corrupt output incorrectly reused'
$results.Add('output-corruption-reexecuted')
$context.evidence = Join-Path $root 'evidence\four'
Invoke-BuildStep $context 'fixture' @{source='v2'; flags='debug'} $action $outputs
Assert-Check ($script:executions -eq 3) 'Changed content reused stale result'
$results.Add('changed-source-reexecuted')
$context.evidence = Join-Path $root 'evidence\five'
Invoke-BuildStep $context 'fixture' @{source='v2'; flags='release'} $action $outputs
Assert-Check ($script:executions -eq 4) 'Changed profile reused stale result'
$results.Add('changed-profile-reexecuted')
$context.evidence = Join-Path $root 'evidence\failure'
Assert-Rejected { Invoke-BuildStep $context 'failure' @{source='v1'} { throw 'injected failure' } } 'Failure suppressed'
Assert-Check ($context.steps[-1].disposition -eq 'FAILED') 'Failure recorded as success'
Assert-Check (-not (Test-Path -LiteralPath (Join-Path $root 'state\failure.json'))) 'Failed step entered reusable cache'
$results.Add('failure-never-cached')
$priorSuccess = Get-Content -LiteralPath (Join-Path $root 'state\fixture.json') -Raw | ConvertFrom-Json
[IO.File]::AppendAllText($priorSuccess.path,' ')
$context.evidence = Join-Path $root 'evidence\tampered'
Invoke-BuildStep $context 'fixture' @{source='v2'; flags='release'} $action $outputs
Assert-Check ($script:executions -eq 5) 'Modified receipt accepted'
$results.Add('modified-receipt-reexecuted')
$impact = Get-BuildImpact @('src/mission/store.rs')
Assert-Check ($impact.rust -and -not $impact.frontend -and $impact.selectors -contains 'mission::') 'Known module not selected'
$unknown = Get-BuildImpact @('unexpected.file')
Assert-Check ($unknown.full -and $unknown.rust -and $unknown.frontend) 'Unknown input did not widen checks'
$results.Add('impact-selection-and-unknown-fallback')
$tree = Join-Path $root 'tree'
[IO.Directory]::CreateDirectory($tree) | Out-Null
$treeFile = Join-Path $tree 'input.txt'
[IO.File]::WriteAllText($treeFile,'one')
$stamp = [IO.File]::GetLastWriteTimeUtc($treeFile)
$firstHash = Get-BuildHash (Get-BuildTree $tree)
[IO.File]::WriteAllText($treeFile,'two')
[IO.File]::SetLastWriteTimeUtc($treeFile,$stamp)
Assert-Check ($firstHash -ne (Get-BuildHash (Get-BuildTree $tree))) 'Timestamp-preserving content change was missed'
$results.Add('content-identity-not-mtime')
$junction = Join-Path $root 'external-link'
New-Item -ItemType Junction -Path $junction -Target $unowned | Out-Null
Assert-Rejected { Assert-BuildPath (Join-Path $junction 'keep.txt') -Within $root } 'Junction traversal accepted'
$results.Add('reparse-ancestor-rejected')
$report = @{passed=$true; checks=$results.ToArray(); fixture=$fixture}
Write-BuildJson (Join-Path $fixture 'report.json') $report
$report | ConvertTo-Json -Depth 5
