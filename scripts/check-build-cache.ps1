#Requires -Version 7.0
param([string]$FixtureRoot)
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'build-support.ps1')
$cacheScript=Join-Path $PSScriptRoot 'build-cache.ps1'
if(-not $FixtureRoot){$FixtureRoot=Join-Path (Split-Path -Parent $PSScriptRoot) 'target/build-cache-tests'}
$fixture=Join-Path $FixtureRoot ([Guid]::NewGuid().ToString('N'))
$root=Get-BuildRoot $fixture
function Assert-Check([bool]$ok,[string]$message){if(-not $ok){throw $message}}
$emptyRoot=Get-BuildRoot (Join-Path $fixture 'empty-managed')
[IO.Directory]::CreateDirectory((Join-Path $emptyRoot 'slots/dev/target'))|Out-Null
$emptyText=& pwsh @('-NoProfile','-File',$cacheScript,'-BuildRoot',$emptyRoot)
if($LASTEXITCODE){throw 'Empty-cache plan failed'}
$emptyPlan=$emptyText|ConvertFrom-Json
Assert-Check ($emptyPlan.target_bytes_after -eq 0 -and -not $emptyPlan.deletions.Count) 'Empty cache plan is not stable'
foreach($relative in @('slots/dev/target','slots/verify/target','cache/npm')) {
    $p=Join-Path $root $relative
    [IO.Directory]::CreateDirectory($p)|Out-Null
    [IO.File]::WriteAllText((Join-Path $p 'data'),'1234')
}
(Get-Item (Join-Path $root 'slots/dev/target')).LastWriteTimeUtc=[DateTime]::UtcNow.AddHours(-1)
$dirs=@()
for($i=1;$i -le 5;$i++) {
    $d=Join-Path $root ('artifacts/'+$i.ToString().PadLeft(64,'0'))
    [IO.Directory]::CreateDirectory($d)|Out-Null
    Write-BuildJson (Join-Path $d 'build-manifest.json') @{fixture=$true}
    [IO.File]::WriteAllText((Join-Path $d 'exe'),'fixture')
    (Get-Item $d).LastWriteTimeUtc=[DateTime]::UtcNow.AddMinutes($i-10)
    $dirs+=$d
}
Write-BuildJson (Join-Path $root 'pins/test.json') @{current_manifest=(Join-Path $dirs[0] 'build-manifest.json');previous_manifest=(Join-Path $dirs[1] 'build-manifest.json')}
Write-BuildJson (Join-Path $root 'evidence/keep.json') @{keep=$true}
$argsForCache=@('-NoProfile','-File',$cacheScript,'-BuildRoot',$root,'-TargetBudgetBytes','4','-NpmBudgetBytes','0')
$planText=& pwsh @argsForCache
if($LASTEXITCODE){throw 'Plan failed'}
$plan=$planText|ConvertFrom-Json
Assert-Check ($plan.deletions.Count -eq 3) 'Expected npm, old slot and one unpinned artifact'
Assert-Check (Test-Path $dirs[2]) 'Plan mutated artifact'
$handle=Enter-BuildLock $root
try {
    & pwsh @argsForCache -Apply 2>&1 | Out-Null
    Assert-Check ($LASTEXITCODE -ne 0) 'Concurrent operation did not reject lock'
} finally {$handle.Dispose()}
$node=(Get-Command node).Source
$child=Start-Process -FilePath $node -ArgumentList @('-e','"setTimeout(()=>{},45000)"',('"'+$root+'"')) -PassThru -WindowStyle Hidden
try {
    & pwsh @argsForCache -Apply 2>&1 | Out-Null
    Assert-Check ($LASTEXITCODE -ne 0) 'Live native user did not protect cache'
    Assert-Check (Test-Path $dirs[2]) 'Live-user failure deleted artifact'
} finally {Stop-Process -Id $child.Id -Force -ErrorAction SilentlyContinue}
$appliedText=& pwsh @argsForCache -Apply
if($LASTEXITCODE){throw 'Apply failed'}
$applied=$appliedText|ConvertFrom-Json
Assert-Check ($applied.removed.Count -eq 3) 'Apply did not match plan'
foreach($i in @(0,1,3,4)) {Assert-Check (Test-Path $dirs[$i]) 'Pinned or recent artifact removed'}
Assert-Check (-not(Test-Path $dirs[2])) 'Unpinned old artifact retained'
Assert-Check (Test-Path (Join-Path $root 'evidence/keep.json')) 'Evidence removed'
$outside=Join-Path (Split-Path $root -Parent) ('outside-'+[Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($outside)|Out-Null
[IO.File]::WriteAllText((Join-Path $outside 'keep'),'keep')
New-Item -ItemType Junction -Path (Join-Path $root 'cache/npm') -Target $outside |Out-Null
& pwsh @argsForCache -Apply 2>&1 | Out-Null
Assert-Check ($LASTEXITCODE -ne 0) 'Reparse deletion accepted'
Assert-Check (Test-Path (Join-Path $outside 'keep')) 'External junction target changed'
Write-BuildJson (Join-Path $root 'report.json') @{passed=$true;checks=@('empty-cache-plan','plan-does-not-delete','cross-process-lock','active-native-process-protected','exact-apply','two-recent-artifacts','current-and-previous-pins','evidence-preserved','junction-rejected');plan=$plan;applied=$applied}
Write-Output "PASS maintenance; report=$root/report.json"
