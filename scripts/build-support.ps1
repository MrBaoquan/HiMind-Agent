# Shared by build, verification, deployment and cache maintenance. Requires PowerShell 7.
# Files under a managed build root are disposable caches or immutable receipts;
# source worktrees and installed Agent homes are never cleanup targets.
Set-StrictMode -Version 3.0

function Get-BuildHash {
    param([Parameter(Mandatory)][object]$Value)
    # Hashtable enumeration varies between PowerShell processes. Canonicalize
    # every object, retaining array order, before computing persistent identities.
    function ConvertTo-CanonicalBuildValue([AllowNull()][object]$Item) {
        if ($null -eq $Item) { return $null }
        if ($Item -is [Collections.IDictionary]) {
            $ordered = [ordered]@{}
            foreach ($key in $Item.Keys | Sort-Object -CaseSensitive) { $ordered[$key] = ConvertTo-CanonicalBuildValue $Item[$key] }
            return $ordered
        }
        if ($Item -is [pscustomobject]) {
            $ordered = [ordered]@{}
            foreach ($property in $Item.PSObject.Properties | Sort-Object Name -CaseSensitive) { $ordered[$property.Name] = ConvertTo-CanonicalBuildValue $property.Value }
            return $ordered
        }
        if ($Item -is [Collections.IList]) {
            $items = @()
            foreach ($entry in $Item) { $items += ,(ConvertTo-CanonicalBuildValue $entry) }
            return ,$items
        }
        return $Item
    }
    $text = ConvertTo-Json -InputObject (ConvertTo-CanonicalBuildValue $Value) -Depth 40 -Compress
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($text))).ToLowerInvariant()
}

function Get-BuildFileHash {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return 'MISSING' }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Write-BuildJson {
    param([string]$Path, [object]$Value)
    [IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
    $temporary = "$Path.$([Guid]::NewGuid().ToString('N')).tmp"
    try {
        [IO.File]::WriteAllText($temporary, (ConvertTo-Json -InputObject $Value -Depth 40), [Text.UTF8Encoding]::new($false))
        [IO.File]::Move($temporary, $Path, $true)
    }
    finally { if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) } }
}

function Assert-BuildPath {
    param([string]$Path, [string]$Within)
    $absolute = [IO.Path]::GetFullPath($Path).TrimEnd('\', '/')
    if ($Within) {
        $parent = [IO.Path]::GetFullPath($Within).TrimEnd('\', '/')
        if (-not $absolute.StartsWith($parent + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Path escapes managed parent: $absolute"
        }
    }
    $cursor = $absolute
    while ($cursor) {
        if (Test-Path -LiteralPath $cursor) {
            if (([IO.File]::GetAttributes($cursor) -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Reparse path is not a managed build location: $cursor"
            }
        }
        $cursor = [IO.Path]::GetDirectoryName($cursor.TrimEnd('\', '/'))
    }
    return $absolute
}

function Get-BuildTree {
    param([string]$Path)
    $tree = [ordered]@{}
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) { return $tree }
    $base = Assert-BuildPath $Path
    $pending = [Collections.Generic.Stack[string]]::new()
    $pending.Push($base)
    $paths = [Collections.Generic.List[string]]::new()
    while ($pending.Count) {
        foreach ($item in Get-ChildItem -LiteralPath $pending.Pop() -Force) {
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "Reparse in identity tree: $($item.FullName)" }
            if ($item.PSIsContainer) { $pending.Push($item.FullName) }
            else { $paths.Add($item.FullName) }
        }
    }
    foreach ($file in $paths | Sort-Object -CaseSensitive) {
        $relative = [IO.Path]::GetRelativePath($base, $file).Replace('\', '/')
        $tree[$relative] = Get-BuildFileHash $file
    }
    return $tree
}

function Get-BuildSource {
    param([string]$Repository)
    # This repository can be a linked worktree of a submodule. A common
    # core.worktree must not redirect reads into .git/modules or another checkout.
    $names = & git -C $Repository "--work-tree=$Repository" -c core.quotePath=false ls-files --cached --others --exclude-standard
    if ($LASTEXITCODE) { throw 'Cannot enumerate source inputs' }
    $files = [ordered]@{}
    foreach ($name in $names | Sort-Object -Unique -CaseSensitive) {
        if ($name -match '^(?:\.himind|\.acceptance|target[^/]*|node_modules|artifacts[^/]*|releases|dist)/') { continue }
        $path = Assert-BuildPath (Join-Path $Repository $name) -Within $Repository
        $files[$name] = Get-BuildFileHash $path
    }
    $commit = & git -C $Repository "--work-tree=$Repository" rev-parse HEAD
    if ($LASTEXITCODE) { throw 'Cannot resolve source commit' }
    return [pscustomobject]@{ files = $files; hash = Get-BuildHash $files; commit = [string]$commit }
}

function Select-BuildSource {
    param([Collections.IDictionary]$Files, [string]$Pattern)
    $selected = [ordered]@{}
    foreach ($name in $Files.Keys) { if ($name -match $Pattern) { $selected[$name] = $Files[$name] } }
    return $selected
}

function Get-BuildRoot {
    param([string]$Requested, [switch]$ReadOnly)
    if (-not $Requested) { $Requested = $env:HIMIND_BUILD_ROOT }
    if (-not $Requested) { $Requested = Join-Path $env:LOCALAPPDATA 'HiMindAgentBuilds\shared-v1' }
    $root = Assert-BuildPath $Requested
    if ($root -eq [IO.Path]::GetPathRoot($root).TrimEnd('\', '/')) { throw 'A volume root is not a build root' }
    $marker = Join-Path $root 'build-root.json'
    if (Test-Path -LiteralPath $marker) {
        $record = Get-Content -LiteralPath $marker -Raw | ConvertFrom-Json
        if ($record.schema -ne 'himind.build-root.v1') { throw 'Build root ownership mismatch' }
    }
    elseif (-not $ReadOnly) {
        if ((Test-Path -LiteralPath $root) -and @(Get-ChildItem -LiteralPath $root -Force).Count) {
            throw 'Refusing to adopt a nonempty unowned build root'
        }
        Write-BuildJson $marker ([ordered]@{ schema = 'himind.build-root.v1'; created_utc = [DateTime]::UtcNow.ToString('o') })
    }
    return $root
}

function Enter-BuildLock {
    param([string]$Root, [string]$Name = 'operation')
    $directory = Assert-BuildPath (Join-Path $Root 'locks') -Within $Root
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    try {
        return [IO.File]::Open((Join-Path $directory "$Name.lock"), 'OpenOrCreate', 'ReadWrite', 'None')
    }
    catch { throw "Another operation owns build lock '$Name'; no files were changed. $($_.Exception.Message)" }
}

function Assert-BuildCapacity {
    param([object[]]$Writes, [long]$ReserveBytes = 2GB)
    $volumes = @{}
    foreach ($write in $Writes) {
        $path = [IO.Path]::GetFullPath([string]$write.Path)
        $volume = [IO.Path]::GetPathRoot($path)
        if (-not $volumes.ContainsKey($volume)) { $volumes[$volume] = 0L }
        $volumes[$volume] += [long]$write.Bytes
    }
    $report = @()
    foreach ($volume in $volumes.Keys) {
        $free = [IO.DriveInfo]::new($volume).AvailableFreeSpace
        $required = $volumes[$volume] + $ReserveBytes
        if ($free -lt $required) { throw "Insufficient capacity on $volume (free=$free, required=$required). Existing Agent remains running." }
        $report += [pscustomobject]@{ volume = $volume; free_bytes = $free; required_bytes = $required }
    }
    return $report
}

function Enter-BuildEnvironment {
    param([string]$Root, [ValidateSet('dev','verify')][string]$Slot, [switch]$UseSccache)
    $values = [ordered]@{
        CARGO_TARGET_DIR = Join-Path $Root "slots\$Slot\target"
        CARGO_INCREMENTAL = $(if ($Slot -eq 'dev') { '1' } else { '0' })
        CARGO_PROFILE_DEV_DEBUG = '1'
        CARGO_PROFILE_TEST_DEBUG = '1'
        npm_config_cache = Join-Path $Root 'cache\npm'
        CARGO_HOME = Join-Path $Root 'cache\cargo'
        TEMP = Join-Path $Root 'tmp\current'
        TMP = Join-Path $Root 'tmp\current'
        HIMIND_AGENT_HOME = Join-Path $Root 'test-home'
        HIMIND_AGENT_PROFILE = 'build-verification'
    }
    if ($UseSccache) {
        $cache = Get-Command sccache -ErrorAction SilentlyContinue
        if ($cache -and $Slot -eq 'verify') {
            $values['RUSTC_WRAPPER'] = $cache.Source
            $values['SCCACHE_DIR'] = Join-Path $Root 'cache\sccache'
            $values['SCCACHE_CACHE_SIZE'] = '10G'
        }
        else { Write-Warning 'sccache unavailable or dev incremental selected; using native Cargo.' }
    }
    $previous = @{}
    foreach ($name in @('CARGO_TARGET_DIR','CARGO_HOME','npm_config_cache','TEMP','TMP','SCCACHE_DIR','HIMIND_AGENT_HOME')) {
        if ($values.Contains($name)) { [void](Assert-BuildPath $values[$name] -Within $Root) }
    }
    try {
        foreach ($name in $values.Keys) {
            $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
            [Environment]::SetEnvironmentVariable($name, $values[$name], 'Process')
        }
        foreach ($path in @($env:CARGO_TARGET_DIR, $env:CARGO_HOME, $env:npm_config_cache, $env:TEMP)) {
            [IO.Directory]::CreateDirectory($path) | Out-Null
        }
        # A secondary NTFS volume may inherit only Modify for a non-elevated
        # owner. Windows label/ACL tests need FullControl in our own temp tree.
        # Add the current user's rule only; never change the volume or source.
        if ($IsWindows) {
            $identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
            $acl = Get-Acl -LiteralPath $env:TEMP
            $rules = @($acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier]) | Where-Object {
                $_.IdentityReference -eq $identity -and $_.AccessControlType -eq 'Allow' -and
                ($_.FileSystemRights -band [Security.AccessControl.FileSystemRights]::FullControl) -eq [Security.AccessControl.FileSystemRights]::FullControl
            })
            if (-not $rules.Count) {
                if ($acl.GetOwner([Security.Principal.SecurityIdentifier]) -ne $identity) { throw 'Build temporary directory must be owned by the current user' }
                $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($identity,'FullControl','ContainerInherit,ObjectInherit','None','Allow'))
                Set-Acl -LiteralPath $env:TEMP -AclObject $acl
            }
        }
    }
    catch { Exit-BuildEnvironment $previous; throw }
    return $previous
}

function Initialize-BuildToolchain {
    # rustc can discover MSVC on its own, but recording the selected SDK/compiler
    # makes reuse decisions reproducible and gives native dependency builds LIB.
    if ($env:VCINSTALLDIR -and (Get-Command cl.exe -ErrorAction SilentlyContinue)) { return @{} }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { return @{} }
    $installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $installation) { return @{} }
    $script = Join-Path ([string]$installation) 'Common7\Tools\VsDevCmd.bat'
    if ($script.Contains('"')) { throw 'Unexpected Visual Studio path' }
    # cmd /s has its own outer-quote grammar. Passing this string through
    # PowerShell's native argument conversion adds another quoting layer.
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $env:ComSpec
    $start.Arguments = '/d /s /c ""' + $script + '" -no_logo -arch=x64 -host_arch=x64 >nul && set"'
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($start)
    try {
        $output = $process.StandardOutput.ReadToEndAsync()
        $errorOutput = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) { $process.Kill($true); throw 'MSVC initialization timed out' }
        if ($process.ExitCode) { throw "MSVC environment initialization failed: $($errorOutput.GetAwaiter().GetResult())" }
        $lines = $output.GetAwaiter().GetResult() -split "`r?`n"
    }
    finally { $process.Dispose() }
    $previous = @{}
    foreach ($line in $lines) {
        if ($line -match '^(PATH|LIB|LIBPATH|INCLUDE|VCINSTALLDIR|VCToolsInstallDir|VCToolsVersion|WindowsSdkDir|WindowsSDKVersion|UniversalCRTSdkDir|UCRTVersion)=(.*)$') {
            $name = $Matches[1]; $value = $Matches[2]
            $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
            [Environment]::SetEnvironmentVariable($name, $value, 'Process')
        }
    }
    return $previous
}

function Exit-BuildEnvironment {
    param([Collections.IDictionary]$Previous)
    foreach ($name in $Previous.Keys) {
        if ($null -eq $Previous[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name, $Previous[$name], 'Process') }
    }
}

function Get-BuildTools {
    $rust = @(& rustc -vV) -join "`n"
    if ($LASTEXITCODE) { throw 'rustc identity failed' }
    $cargo = @(& cargo -V) -join "`n"
    if ($LASTEXITCODE) { throw 'Cargo identity failed' }
    $node = @(& node --version) -join "`n"
    if ($LASTEXITCODE) { throw 'Node identity failed' }
    $npm = @(& npm.cmd --version) -join "`n"
    if ($LASTEXITCODE) { throw 'npm identity failed' }
    $environment = [ordered]@{}
    foreach ($name in @('RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER',
        'CARGO_BUILD_TARGET','CARGO_INCREMENTAL','CARGO_PROFILE_DEV_DEBUG','CARGO_PROFILE_TEST_DEBUG',
        'VCINSTALLDIR','VCToolsVersion','WindowsSDKVersion','LIB','INCLUDE','TAURI_CONFIG',
        'HIMIND_BUILD_CHANNEL','HIMIND_SIGNING_KEY_ID','HIMIND_EXTENSION_SIGNING_KEY_ID',
        'npm_config_registry','npm_config_omit','npm_config_ignore_scripts','npm_config_production','NODE_ENV','NODE_OPTIONS','RUSTUP_TOOLCHAIN')) {
        # Hash configuration values; don't record arbitrary process environments.
        $environment[$name] = Get-BuildHash ([string][Environment]::GetEnvironmentVariable($name, 'Process'))
    }
    foreach ($name in @('HIMIND_SIGNING_PUBLIC_KEY_PATH','HIMIND_EXTENSION_SIGNING_PUBLIC_KEY_PATH')) {
        $value = [Environment]::GetEnvironmentVariable($name, 'Process')
        $environment[$name] = $(if ($value) { Get-BuildFileHash $value } else { 'UNSET' })
    }
    foreach ($item in Get-ChildItem Env: | Sort-Object Name) {
        if ($item.Name -match '^CARGO_(PROFILE_|TARGET_|BUILD_)|^HIMIND_DEFAULT_DASHBOARD_API_BASE$|^RUSTC$|^RUSTDOCFLAGS$|^CC$|^CXX$|^CFLAGS$|^CXXFLAGS$|^VITE_|^TAURI_ENV_') {
            $environment[$item.Name] = Get-BuildHash ([string]$item.Value)
        }
    }
    $linker = Get-Command link.exe -ErrorAction SilentlyContinue
    $linkerIdentity = if ($linker) { Get-BuildFileHash $linker.Source } else { 'RUSTC_AUTO_DISCOVERY' }
    foreach ($config in @('config','config.toml')) { $environment["cargo-home-$config"] = Get-BuildFileHash (Join-Path $env:CARGO_HOME $config) }
    return [ordered]@{ rustc = $rust; cargo = $cargo; node = $node; npm = $npm; os = [Environment]::OSVersion.VersionString; arch = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString(); linker = $linkerIdentity; environment = $environment }
}

function Invoke-BuildCommand {
    param([string]$Executable, [string[]]$Arguments, [string]$Directory, [string]$Log)
    [IO.Directory]::CreateDirectory((Split-Path -Parent $Log)) | Out-Null
    Push-Location $Directory
    try {
        & $Executable @Arguments 2>&1 | Tee-Object -FilePath $Log | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "$Executable failed (exit=$LASTEXITCODE); see $Log" }
    }
    finally { Pop-Location }
}

function Invoke-BuildStep {
    param([object]$Context, [string]$Name, [object]$InputIdentity, [scriptblock]$Action,
          [scriptblock]$Outputs = { [ordered]@{} }, [switch]$Always)
    $inputHash = Get-BuildHash $InputIdentity
    $cachePath = Join-Path $Context.root "state\$Name.json"
    $receiptPath = Join-Path $Context.evidence "$Name.json"
    $prior = $null
    if (-not $Always -and (Test-Path -LiteralPath $cachePath)) {
        try {
            $candidate = Get-Content -LiteralPath $cachePath -Raw | ConvertFrom-Json
            $original = Get-Content -LiteralPath $candidate.path -Raw | ConvertFrom-Json
            if ((Get-BuildFileHash $candidate.path) -eq $candidate.sha256 -and $original.result -eq 'PASS' -and
                $original.input_hash -eq $inputHash -and $original.output_hash -eq (Get-BuildHash (& $Outputs))) {
                $prior = $candidate
            }
        }
        catch { Write-Verbose "Invalid prior $Name receipt: $_" }
    }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $receipt = [ordered]@{ schema = 'himind.build-step.v1'; step = $Name; input_hash = $inputHash;
        result = 'PASS'; disposition = $(if ($prior) { 'REUSED' } else { 'EXECUTED' });
        source = $prior; recorded_utc = [DateTime]::UtcNow.ToString('o'); duration_ms = 0; output_hash = '' }
    try {
        if (-not $prior) { & $Action | Out-Host }
        $receipt.output_hash = Get-BuildHash (& $Outputs)
    }
    catch {
        $receipt.result = 'FAILED'; $receipt.disposition = 'FAILED'; $receipt['error'] = $_.Exception.Message
        if (Test-Path -LiteralPath $cachePath) { Remove-Item -LiteralPath $cachePath -Force }
        throw
    }
    finally {
        $timer.Stop(); $receipt.duration_ms = $timer.ElapsedMilliseconds
        Write-BuildJson $receiptPath $receipt
        $Context.steps.Add([pscustomobject]@{ step = $Name; disposition = $receipt.disposition; result = $receipt.result; path = $receiptPath; sha256 = Get-BuildFileHash $receiptPath; duration_ms = $receipt.duration_ms })
        Write-Host "$($receipt.disposition) $Name ($($receipt.duration_ms) ms)"
        if ($receipt.result -eq 'PASS') {
            # Keep direct provenance to the original execution, not a chain of
            # no-op invocations eventually collected by retention.
            $source = if ($prior) { $prior } else { @{ path = $receiptPath; sha256 = Get-BuildFileHash $receiptPath } }
            Write-BuildJson $cachePath $source
        }
    }
}

function Get-BuildImpact {
    param([string[]]$Changed)
    $rust = $false; $frontend = $false; $full = $false
    $selectors = [Collections.Generic.HashSet[string]]::new()
    foreach ($name in $Changed) {
        if ($name -match '^docs/|^README\.md$') { continue }
        if ($name -match '^frontend/') { $frontend = $true; continue }
        if ($name -match '^src/(mission|workflow|expert|runtime|executor|store)(?:[/.])') {
            $rust = $true
            [void]$selectors.Add(($Matches[1] + '::'))
        }
        else { $rust = $true; $frontend = $true; $full = $true }
    }
    return [pscustomobject]@{ rust = $rust; frontend = $frontend; full = $full;
        selectors = @($selectors | Sort-Object); changed = $Changed }
}

function Read-BuildManifest {
    param([string]$Path, [switch]$RequireVerification)
    $manifestPath = Assert-BuildPath $Path
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($manifest.schema -ne 'himind.build-manifest.v1') { throw 'Unsupported build manifest' }
    $directory = Split-Path -Parent $manifestPath
    $required = @('himind-agent.exe','himind-agent-mcp.exe','himind-agent-launcher.exe','himind-agent-updater.exe')
    if (@($manifest.artifacts).Count -ne $required.Count) { throw 'Incomplete artifact inventory' }
    foreach ($name in $required) {
        $artifact = @($manifest.artifacts | Where-Object { $_.file -ceq $name })
        if ($artifact.Count -ne 1) { throw "Missing or duplicate artifact: $name" }
        $expectedFeatures = if ($name -eq 'himind-agent-mcp.exe') { 'mcp-console' } else { '' }
        if ((@($artifact[0].features) -join ',') -cne $expectedFeatures) { throw "Unexpected features: $name" }
        $file = Assert-BuildPath (Join-Path $directory $name) -Within $directory
        if ((Get-BuildFileHash $file) -ne $artifact[0].sha256 -or (Get-Item -LiteralPath $file).Length -ne $artifact[0].bytes) {
            throw "Artifact hash or length mismatch: $name"
        }
    }
    if ($RequireVerification) {
        $basePath = Join-Path $directory 'build-manifest.json'
        $baseManifest = Get-Content -LiteralPath $basePath -Raw | ConvertFrom-Json
        if ((Get-BuildHash ($baseManifest | Select-Object * -ExcludeProperty verification)) -ne (Get-BuildHash ($manifest | Select-Object * -ExcludeProperty verification))) { throw 'Verified manifest differs from frozen build identity' }
        $verification = $manifest.verification
        if (-not $verification -or (Get-BuildFileHash $verification.path) -ne $verification.sha256) { throw 'Missing or modified verification receipt' }
        $receipt = Get-Content -LiteralPath $verification.path -Raw | ConvertFrom-Json
        if ($receipt.result -ne 'PASS' -or $receipt.mode -ne 'candidate' -or $receipt.build_input_hash -ne $manifest.build_input_hash -or $receipt.source_hash -ne $manifest.source_hash -or -not $receipt.full_tests) {
            throw 'Candidate has not passed matching full verification'
        }
        foreach ($stepName in @('frontend-dependencies','frontend-build','frontend-checks','rust-format','rust-tests-ALL','integration-dependencies','integration-tests','rust-build')) {
            $entries = @($receipt.steps | Where-Object step -eq $stepName)
            if ($entries.Count -ne 1) { throw "Missing full verification step: $stepName" }
            $entry = $entries[0]
            if ($entry.result -ne 'PASS' -or (Get-BuildFileHash $entry.path) -ne $entry.sha256) { throw "Modified verification step: $stepName" }
            $step = Get-Content -LiteralPath $entry.path -Raw | ConvertFrom-Json
            if ($step.result -ne 'PASS' -or $step.step -ne $stepName) { throw "Invalid verification step: $stepName" }
            if ($stepName -eq 'rust-build' -and $step.output_hash -ne (Get-BuildHash @{manifest=Get-BuildFileHash $basePath})) { throw 'Frozen manifest does not match build evidence' }
            if ($step.disposition -eq 'REUSED') {
                if (-not $step.source -or (Get-BuildFileHash $step.source.path) -ne $step.source.sha256) { throw "Modified original evidence: $stepName" }
                $original = Get-Content -LiteralPath $step.source.path -Raw | ConvertFrom-Json
                if ($original.result -ne 'PASS' -or $original.disposition -ne 'EXECUTED' -or $original.input_hash -ne $step.input_hash -or $original.output_hash -ne $step.output_hash) { throw "Invalid reused evidence: $stepName" }
            }
            elseif ($step.disposition -ne 'EXECUTED') { throw "Invalid disposition: $stepName" }
        }
    }
    return $manifest
}
