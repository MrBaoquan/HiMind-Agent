#Requires -Version 7.0
[CmdletBinding()]
param(
    [string]$BuildRoot,
    [switch]$Apply,
    [long]$TargetBudgetBytes = 20GB,
    [long]$NpmBudgetBytes = 2GB,
    [long]$CargoBudgetBytes = 2GB,
    [long]$SccacheBudgetBytes = 10GB,
    [long]$TemporaryBudgetBytes = 3GB,
    [ValidateRange(2,100)][int]$KeepArtifacts = 2
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'build-support.ps1')
$storage = Get-BuildRoot $BuildRoot -ReadOnly
if (-not (Test-Path -LiteralPath (Join-Path $storage 'build-root.json'))) { throw 'Managed build root does not exist' }
foreach ($budget in @($TargetBudgetBytes,$NpmBudgetBytes,$CargoBudgetBytes,$SccacheBudgetBytes,$TemporaryBudgetBytes)) {
    if ($budget -lt 0) { throw 'Cache budgets must be nonnegative' }
}
$lock = Enter-BuildLock $storage
try {
    # File locks serialize cooperating entry points. Also refuse deletion while
    # a surviving native child is still using this managed root.
    $users = @(Get-CimInstance Win32_Process | Where-Object {
        $_.ProcessId -ne $PID -and
        (($_.ExecutablePath -and $_.ExecutablePath.StartsWith($storage + '\',[StringComparison]::OrdinalIgnoreCase)) -or
         ($_.Name -match '^(cargo|rustc|sccache|node|himind-agent.*)(\.exe)?$' -and $_.CommandLine -and $_.CommandLine.IndexOf($storage,[StringComparison]::OrdinalIgnoreCase) -ge 0) -or
         # A cancelled parent can leave a compiler/npm child whose environment
         # (including TEMP/cache paths) is absent from CommandLine. Be conservative
         # during native builds; ordinary long-lived Node servers do not block.
         ($_.Name -match '^(cargo|rustc|link|sccache)(\.exe)?$') -or
         ($_.Name -eq 'node.exe' -and $_.CommandLine -match 'npm-cli\.js|npmexec\.js|vite[^\r\n]*\bbuild\b|typescript[^\r\n]*\btsc\b'))
    })
    function Measure-ManagedDirectory([string]$Path) {
        $absolute = Assert-BuildPath $Path -Within $storage
        if (-not (Test-Path -LiteralPath $absolute -PathType Container)) { return $null }
        $items = @(Get-ChildItem -LiteralPath $absolute -Force -Recurse)
        if (@($items | Where-Object { ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 }).Count) { throw "Reparse in cleanup target: $absolute" }
        $files = @($items | Where-Object { -not $_.PSIsContainer })
        $bytes = if ($files.Count) { [long](($files | Measure-Object Length -Sum).Sum) } else { 0L }
        return [pscustomobject]@{path=$absolute; bytes=[long]$bytes; files=$files.Count; last_write=(Get-Item -LiteralPath $absolute).LastWriteTimeUtc}
    }
    $deletions = [Collections.Generic.List[object]]::new()
    $protected = [Collections.Generic.List[string]]::new()
    $cachePolicies = @(
        @{relative='cache\npm';budget=$NpmBudgetBytes},
        @{relative='cache\cargo\registry';budget=$CargoBudgetBytes},
        @{relative='cache\sccache';budget=$SccacheBudgetBytes},
        @{relative='tmp\current';budget=$TemporaryBudgetBytes}
    )
    foreach ($policy in $cachePolicies) {
        $item = Measure-ManagedDirectory (Join-Path $storage $policy.relative)
        if ($item -and $item.bytes -gt $policy.budget) { $deletions.Add(@{path=$item.path;bytes=$item.bytes;files=$item.files;reason='over configured cache budget'}) }
    }
    $slots = @()
    foreach ($name in @('dev','verify')) {
        $item = Measure-ManagedDirectory (Join-Path $storage "slots\$name\target")
        if ($item) {
            $usage = Join-Path $storage "state\slot-use-$name.json"
            if (Test-Path -LiteralPath $usage) { $item.last_write = [DateTime]::Parse((Get-Content -LiteralPath $usage -Raw | ConvertFrom-Json).used_utc).ToUniversalTime() }
            $slots += $item
        }
    }
    $slotMeasure = @($slots | Measure-Object bytes -Sum)
    $total = if ($slotMeasure.Count) { [long]$slotMeasure[0].Sum } else { 0L }
    $newestSlot = @($slots | Sort-Object last_write -Descending | Select-Object -First 1)
    if ($newestSlot.Count) { $protected.Add($newestSlot[0].path) }
    foreach ($slot in $slots | Sort-Object last_write) {
        if ($total -le $TargetBudgetBytes) { break }
        if ($protected.Contains($slot.path)) { continue }
        $deletions.Add(@{path=$slot.path;bytes=$slot.bytes;files=$slot.files;reason='inactive slot exceeds aggregate target budget'})
        $total -= $slot.bytes
    }
    $artifactParent = Join-Path $storage 'artifacts'
    $artifactDirectories = @()
    if (Test-Path -LiteralPath $artifactParent) {
        $artifactDirectories = @(Get-ChildItem -LiteralPath $artifactParent -Directory | Where-Object {
            $_.Name -match '^[0-9a-f]{64}$' -and (Test-Path -LiteralPath (Join-Path $_.FullName 'build-manifest.json'))
        } | Sort-Object LastWriteTimeUtc -Descending)
    }
    foreach ($directory in $artifactDirectories | Select-Object -First $KeepArtifacts) { $protected.Add($directory.FullName) }
    $pinParent = Join-Path $storage 'pins'
    if (Test-Path -LiteralPath $pinParent) {
        foreach ($file in Get-ChildItem -LiteralPath $pinParent -Filter '*.json' -File) {
            $pin = Get-Content -LiteralPath $file.FullName -Raw | ConvertFrom-Json
            foreach ($manifest in @($pin.current_manifest,$pin.previous_manifest)) {
                if ($manifest) {
                    $protected.Add((Split-Path -Parent (Assert-BuildPath $manifest -Within $artifactParent)))
                    # Incremental local receipts depend on an immutable complete
                    # baseline. Retain its artifacts while either pin uses it.
                    $pinned = Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json -AsHashtable
                    if ($pinned.ContainsKey('verification') -and $pinned.verification) {
                        $verification = Get-Content -LiteralPath $pinned.verification.path -Raw | ConvertFrom-Json -AsHashtable
                        if ($verification.mode -eq 'local') {
                            [void](Read-BuildManifest $manifest -RequireVerification -AllowLocalVerification)
                            $protected.Add((Split-Path -Parent (Assert-BuildPath $verification.baseline.path -Within $artifactParent)))
                        }
                    }
                }
            }
        }
    }
    foreach ($directory in $artifactDirectories) {
        if ($protected.Contains($directory.FullName)) { continue }
        $item = Measure-ManagedDirectory $directory.FullName
        $deletions.Add(@{path=$item.path;bytes=$item.bytes;files=$item.files;reason='older unpinned complete artifact'})
    }
    $plan = [ordered]@{schema='himind.cache-plan.v1'; root=$storage; disposition=$(if($Apply){'APPLIED'}else{'PLAN'});
        protected=$protected.ToArray(); active_processes=@($users | Select-Object ProcessId,Name);
        deletions=$deletions.ToArray(); target_bytes_after=$total;
        over_budget_protected=($total -gt $TargetBudgetBytes); removed=@()}
    if ($Apply) {
        if ($users.Count) { throw "Managed resources still used by native processes: $($users.ProcessId -join ', ')" }
        foreach ($item in $deletions) {
            $absolute = Assert-BuildPath $item.path -Within $storage
            if ((Resolve-Path -LiteralPath $absolute).ProviderPath -ne $absolute) { throw 'Cleanup target resolves elsewhere' }
            $current = Measure-ManagedDirectory $absolute
            if ($current.bytes -ne $item.bytes -or $current.files -ne $item.files) { throw "Cleanup target changed: $absolute" }
            Remove-Item -LiteralPath $absolute -Recurse -Force
            $plan.removed += $item
        }
        Write-BuildJson (Join-Path $storage ('maintenance\' + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfff') + '.json')) $plan
    }
    $plan | ConvertTo-Json -Depth 12
}
finally { $lock.Dispose() }
