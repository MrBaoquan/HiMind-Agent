. (Join-Path $PSScriptRoot 'build-support.ps1')

function Invoke-AgentBuildPipeline {
    [CmdletBinding()]
    param(
        [string]$Repository, [string]$BuildRoot,
        [ValidateSet('affected','quick','candidate','local')][string]$Mode = 'affected',
        [string]$BaseRef, [string]$BaselineManifest,
        [ValidateSet('debug','release')][string]$Configuration = 'release',
        [string]$BuildChannel = 'local', [switch]$Build,
        [switch]$SkipTests, [switch]$SkipIntegration, [switch]$SkipDependencyInstall,
        [string]$RuntimeReleaseDirectory, [switch]$SkipRuntimeBundle,
        [switch]$UseSccache, [switch]$Explain, [switch]$DryRun
    )
    $Repository = Assert-BuildPath $Repository
    $storageRoot = Get-BuildRoot $BuildRoot -ReadOnly:($Explain -or $DryRun)
    if ($Build -and $Mode -notin @('candidate','local')) { throw 'Artifact builds require candidate or local verification' }
    if ($Mode -eq 'local' -and (-not $Build -or -not $BaselineManifest -or $SkipTests -or $SkipIntegration -or $SkipDependencyInstall)) {
        throw 'Local verification requires -Build, -BaselineManifest and all current frontend/integration checks'
    }
    $development = $Mode -in @('affected','quick')
    $slot = if ($development) { 'dev' } else { 'verify' }
    $machine = $null
    $runtime = $null
    $source = Get-BuildSource $Repository
    $previousSource = Join-Path $storageRoot "state\source-$slot.json"
    $oldFiles = @{}
    $previousToolsHash = ''
    if (Test-Path -LiteralPath $previousSource) {
        $prior = Get-Content -LiteralPath $previousSource -Raw | ConvertFrom-Json -AsHashtable
        $oldFiles = $prior.files
        if ($prior.ContainsKey('tools_hash')) { $previousToolsHash = $prior.tools_hash }
    }
    $changes = Get-BuildChanges $Repository $source $oldFiles $BaseRef
    $impact = Get-BuildImpact $changes.changed
    $plan = [ordered]@{ baseline=$changes.baseline; baseline_kind=$changes.kind; impact=$impact;
        coverage=$(if ($development) { 'affected-only' } elseif ($Mode -eq 'local') { 'complete-baseline-plus-current-ui-adapter' } else { 'complete-candidate' });
        full_tests_required=($Mode -eq 'candidate' -or ($development -and $impact.full));
        frontend=(-not $development -or $Mode -eq 'quick' -or $impact.frontend); rust=(-not $development -or $Mode -eq 'quick' -or $impact.rust);
        integration=(-not $development -or $impact.integration);
        script_checks=@($impact.script_checks); artifact_build=[bool]$Build }
    if ($Explain -or $DryRun) {
        return [pscustomobject]@{ schema = 'himind.build-plan.v1'; mode = $Mode; read_only = $true; root = $storageRoot;
            slot = Join-Path $storageRoot "slots\$slot\target"; source_hash = $source.hash; impact = $impact; plan=$plan;
            steps = @($(if ($plan.frontend) { 'frontend dependencies/build/checks' }),$(if ($Mode -eq 'local') { 'Rust format; complete baseline admission (no current Rust tests)' } elseif ($plan.rust) { 'Rust check/selected tests (toolchain change may widen)' }),$(if ($plan.integration) { 'VS Code integration' }),$impact.script_checks,$(if ($Build) { 'freeze canonical four binaries' }));
            fallback = 'Unknown changes widen verification; missing or modified receipts/outputs execute again.' }
    }
    # A docs-only (or unchanged) development run must not require or start a
    # Rust/Node toolchain. It is partial verification, never a release receipt.
    if ($development -and -not $plan.frontend -and -not $plan.rust -and -not $plan.integration -and -not $impact.script_checks.Count) {
        $docsLock = Enter-BuildLock $storageRoot
        $docsTimer = [Diagnostics.Stopwatch]::StartNew()
        try {
            $diffArguments = @('diff')
            if ($BaseRef) { $diffArguments += $changes.baseline }
            $diffArguments += '--check'
            & git -C $Repository "--work-tree=$Repository" -c core.whitespace=cr-at-eol @diffArguments
            if ($LASTEXITCODE) { throw 'Working tree whitespace check failed' }
            & git -C $Repository "--work-tree=$Repository" -c core.whitespace=cr-at-eol diff --cached --check
            if ($LASTEXITCODE) { throw 'Index whitespace check failed' }
            $after = Get-BuildSource $Repository
            if ($after.hash -ne $source.hash -or $after.commit -ne $source.commit) { throw 'Source changed during operation' }
            Write-BuildJson $previousSource @{files=$source.files;hash=$source.hash;tools_hash=$previousToolsHash}
            $docsTimer.Stop()
            $docsResult = @{schema='himind.verification.v1';result='PASS';mode=$Mode;source_hash=$source.hash;commit=$source.commit;
                full_tests=$false;manifest=$null;steps=@();plan=$plan;duration_ms=$docsTimer.ElapsedMilliseconds;
                note='Diff checks only. Links/examples still require review; this is not complete application coverage.'}
            $docsEvidence = Join-Path $storageRoot ('evidence/' + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfff') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,8))
            Write-BuildJson (Join-Path $docsEvidence 'result.json') $docsResult
            $docsResult['evidence'] = $docsEvidence
            return [pscustomobject]$docsResult
        } finally { $docsLock.Dispose() }
    }
    $machine = Assert-BuildMachineReady $Repository $storageRoot
    if ($Build -and -not $SkipRuntimeBundle) {
        . (Join-Path $PSScriptRoot 'runtime-bundle.ps1')
        $Root = $Repository
        $runtimeInfo = Resolve-RuntimeReleaseDirectory $RuntimeReleaseDirectory -RequireExactPackage
        $runtime = [ordered]@{ directory=$runtimeInfo.Directory; manifest_sha256=Get-BuildFileHash $runtimeInfo.ManifestPath;
            package=$runtimeInfo.PackageName; package_sha256=$runtimeInfo.Manifest.sha256;
            signature_sha256=Get-BuildFileHash (Join-Path $runtimeInfo.Directory "$($runtimeInfo.PackageName).signature.json") }
    }
    $lock = Enter-BuildLock $storageRoot
    $environment = @{}; $toolchainEnvironment = @{}
    $run = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfff') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,8)
    $context = [pscustomobject]@{ root = $storageRoot; evidence = Join-Path $storageRoot "evidence\$run"; steps = [Collections.Generic.List[object]]::new(); provenance=@{commit=$source.commit;source_hash=$source.hash} }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $result = [ordered]@{ schema = 'himind.verification.v1'; result = 'FAILED'; mode = $Mode; source_hash = $source.hash;
        build_input_hash = ''; full_tests = $false; manifest = $null; steps = @(); duration_ms = 0; slot = $slot; capacity = @(); machine = $machine;plan=$plan;commit=$source.commit;evidence=$context.evidence }
    try {
        $result.capacity = @(Assert-BuildCapacity @(@{Path=$storageRoot; Bytes=6GB}, @{Path=$Repository; Bytes=512MB}))
        $toolchainEnvironment = Initialize-BuildToolchain
        $environment = Enter-BuildEnvironment $storageRoot $slot -UseSccache:$UseSccache
        $oldChannel = [Environment]::GetEnvironmentVariable('HIMIND_BUILD_CHANNEL','Process')
        $env:HIMIND_BUILD_CHANNEL = $BuildChannel
        $tools = Get-BuildTools
        $toolsHash = Get-BuildHash $tools
        if ($development -and $plan.rust -and $toolsHash -ne $previousToolsHash) {
            $plan.full_tests_required = $true
            $plan['toolchain_note'] = 'Rust toolchain/options differ from the previous snapshot; expand Rust tests.'
        }
        $result['source_files'] = $source.files
        if ($Mode -eq 'local') {
            $baseline = @{path=(Assert-BuildPath $BaselineManifest);sha256=Get-BuildFileHash $BaselineManifest}
            $result['baseline'] = Assert-LocalVerificationBaseline $source.files $tools $BuildChannel $Configuration $runtime $baseline
            $plan.coverage = 'complete-baseline-plus-current-ui-adapter'
        }
        $rustFiles = Select-BuildSource $source.files '^(src/|Cargo\.(toml|lock)$|build\.rs$|\.cargo/|tauri\.conf\.json$|icons/|capabilities/|contracts/|resources/)'
        $frontendFiles = Select-BuildSource $source.files '^frontend/'
        $supportFiles = Select-BuildSource $source.files '^scripts/(build|verify|verification-plan|runtime-bundle|check-build)'
        $rustIdentity = [ordered]@{ repository=$Repository; files=$rustFiles; tools=$tools; support=$supportFiles; channel=$BuildChannel; commit=$source.commit }
        $frontDirectory = Join-Path $Repository 'frontend'
        $dependencyDirectory = Join-Path $frontDirectory 'node_modules'
        $distDirectory = Join-Path $frontDirectory 'dist'
        $frontOutputHash = Get-BuildHash (Get-BuildTree $distDirectory)
        $frontendState = Join-Path $storageRoot 'state/frontend-source.json'
        $frontendSourceHash = Get-BuildHash $frontendFiles
        $prepareFrontend = [bool]$plan.frontend
        if ($plan.rust -and -not $prepareFrontend) {
            try {
                $priorFrontend = Get-Content -LiteralPath $frontendState -Raw | ConvertFrom-Json
                $prepareFrontend = $priorFrontend.source_hash -ne $frontendSourceHash -or $priorFrontend.output_hash -ne $frontOutputHash
            } catch { $prepareFrontend = $true }
            if ($prepareFrontend) { $plan['frontend_prerequisite'] = 'Rust embeds the UI: missing, changed or unverified dist must be prepared.' }
        }
        $npmEnvironment = [ordered]@{}
        foreach ($name in @('npm_config_registry','npm_config_omit','npm_config_ignore_scripts','npm_config_production','NODE_ENV','NODE_OPTIONS')) { $npmEnvironment[$name] = $tools.environment[$name] }
        $dependencyIdentity = [ordered]@{ package=Get-BuildFileHash (Join-Path $frontDirectory 'package.json'); lock=Get-BuildFileHash (Join-Path $frontDirectory 'package-lock.json'); config=Get-BuildFileHash (Join-Path $frontDirectory '.npmrc'); node=$tools.node; npm=$tools.npm; os=$tools.os;arch=$tools.arch; environment=$npmEnvironment }
        # Explicit legacy skip means "assert installed dependencies", not a fake
        # successful npm install. Ordinary reuse verifies the entire output tree.
        if ($prepareFrontend -and $SkipDependencyInstall) {
            if (-not (Test-Path -LiteralPath (Join-Path $dependencyDirectory 'typescript\package.json'))) { throw 'Dependency installation skipped but TypeScript is missing' }
        }
        elseif ($prepareFrontend) {
            Invoke-BuildStep $context 'frontend-dependencies' $dependencyIdentity {
                Invoke-BuildCommand 'npm.cmd' @('ci','--no-audit','--no-fund') $frontDirectory (Join-Path $context.evidence 'npm-ci.log')
            } { Get-BuildTree $dependencyDirectory }
        }
        if ($prepareFrontend) {
        $frontendEnvironment = [ordered]@{}
        foreach ($name in $tools.environment.Keys) { if ($name -match '^VITE_|^TAURI_ENV_') { $frontendEnvironment[$name] = $tools.environment[$name] } }
        $installedDependencies = if ($SkipDependencyInstall) { Get-BuildHash (Get-BuildTree $dependencyDirectory) } else { Get-BuildStepOutputHash $context 'frontend-dependencies' }
        $frontIdentity = [ordered]@{ files=$frontendFiles; dependencies=$dependencyIdentity; environment=$frontendEnvironment; installed=$installedDependencies }
        # Always validate output receipts, including unchanged quick runs. A
        # deleted or corrupted dist must not hide behind source impact alone.
        Invoke-BuildStep $context 'frontend-build' $frontIdentity {
            Invoke-BuildCommand 'npm.cmd' @('run','build') $frontDirectory (Join-Path $context.evidence 'frontend-build.log')
        } { Get-BuildTree $distDirectory }
        $frontOutputHash = Get-BuildStepOutputHash $context 'frontend-build'
        if ($plan.frontend -and -not $SkipTests) {
            Invoke-BuildStep $context 'frontend-checks' $frontIdentity {
                Invoke-BuildCommand 'npm.cmd' @('run','check:ui') $frontDirectory (Join-Path $context.evidence 'frontend-checks.log')
            }
        }
        }
        $oldInputHash = [Environment]::GetEnvironmentVariable('HIMIND_BUILD_INPUT_HASH','Process')
        $env:HIMIND_BUILD_INPUT_HASH = Get-BuildHash ([ordered]@{files=$rustFiles;tools=$tools;channel=$BuildChannel;commit=$source.commit;frontend=$frontOutputHash})
        if ($plan.rust) {
        # Existing repository code has inherited formatting differences.
        # Include the selected file list in cache identity: another BaseRef can
        # widen formatting scope even when current source bytes are unchanged.
        $formatFiles = @(if ($BaseRef -or ($changes.kind -eq 'last-successful-snapshot' -and $oldFiles.Count)) { $changes.changed | Where-Object { $_ -match '\.rs$' } } else { & git -C $Repository "--work-tree=$Repository" diff HEAD --name-only -- '*.rs' })
        $formatFiles += @(& git -C $Repository "--work-tree=$Repository" ls-files --others --exclude-standard -- '*.rs')
        if (-not $formatFiles.Count -and (Test-Path (Join-Path $Repository 'build.rs'))) { $formatFiles = @('build.rs') }
        $formatFiles = @($formatFiles | Sort-Object -Unique | Where-Object { Test-Path (Join-Path $Repository $_) })
        Invoke-BuildStep $context 'rust-format' @{rust=$rustIdentity;selected=$formatFiles} {
            if ($formatFiles.Count) {
                Invoke-BuildCommand 'rustfmt' (@('--check','--edition','2021','--config','skip_children=true') + $formatFiles) $Repository (Join-Path $context.evidence 'rust-format.log')
            }
        }
        if ($development) {
            Invoke-BuildStep $context 'rust-check' $rustIdentity {
                Invoke-BuildCommand 'cargo' @('check','--locked','--bins') $Repository (Join-Path $context.evidence 'rust-check.log')
            } -Always
        }
        if (-not $SkipTests -and $Mode -ne 'local') {
            $selectors = if ($plan.full_tests_required) { @('ALL') } else { @($impact.selectors) }
            foreach ($selector in $selectors) {
                $testName = 'rust-tests-' + ($selector -replace '[^A-Za-z0-9-]', '_')
                Invoke-BuildStep $context $testName (Get-BuildTestIdentity $source $tools $Repository $BuildChannel $frontOutputHash $selector) {
                    if ($selector -eq 'ALL') {
                        Invoke-BuildCommand 'cargo' @('test','--locked','--','--test-threads=1') $Repository (Join-Path $context.evidence "$testName.log")
                    } else {
                        Invoke-SelectedRustTests $Repository $context.evidence $testName $selector
                    }
                }
            }
            if (-not $plan.full_tests_required) {
                foreach ($bin in $impact.bins) {
                    $testName = 'rust-tests-' + $bin
                    Invoke-BuildStep $context $testName (Get-BuildTestIdentity $source $tools $Repository $BuildChannel $frontOutputHash $bin) {
                        Invoke-SelectedRustTests $Repository $context.evidence $testName 'ALL' $bin
                    }
                }
            }
        }
        }
        if ($plan.integration -and -not $SkipIntegration -and -not $SkipTests) {
            $integration = Join-Path $Repository 'integrations\vscode-himind-ai'
            $integrationDependencies = [ordered]@{ package=Get-BuildFileHash (Join-Path $integration 'package.json'); lock=Get-BuildFileHash (Join-Path $integration 'package-lock.json'); config=Get-BuildFileHash (Join-Path $integration '.npmrc'); node=$tools.node; npm=$tools.npm; os=$tools.os; arch=$tools.arch; environment=$npmEnvironment }
            Invoke-BuildStep $context 'integration-dependencies' $integrationDependencies {
                Invoke-BuildCommand 'npm.cmd' @('ci','--no-audit','--no-fund') $integration (Join-Path $context.evidence 'integration-ci.log')
            } { Get-BuildTree (Join-Path $integration 'node_modules') }
            $integrationIdentity = [ordered]@{ files=Select-BuildSource $source.files '^integrations/vscode-himind-ai/'; dependencies=$integrationDependencies; installed=Get-BuildStepOutputHash $context 'integration-dependencies' }
            Invoke-BuildStep $context 'integration-tests' $integrationIdentity {
                Invoke-BuildCommand 'npm.cmd' @('test') $integration (Join-Path $context.evidence 'integration-tests.log')
            }
        }
        if ($impact.script_checks.Count) { Invoke-BuildScriptChecks $context $Repository $changes.changed $impact.script_checks $tools }
        $buildIdentity = [ordered]@{ source=$source.hash; rust=$rustIdentity; frontend=$frontOutputHash;
            configuration=$Configuration; binaries=@('himind-agent','himind-agent-launcher','himind-agent-updater'); mcp=@{binary='himind-agent-mcp'; features=@('mcp-console')}; runtime=$runtime }
        $result.build_input_hash = Get-BuildHash $buildIdentity
        $result.full_tests = ($Mode -eq 'candidate' -and -not $SkipTests -and -not $SkipIntegration -and -not $SkipDependencyInstall)
        $verificationPath = Join-Path $context.evidence 'verification.json'
        if ($Build) {
            $artifactDirectory = Join-Path $storageRoot "artifacts\$($result.build_input_hash)"
            $manifestPath = Join-Path $artifactDirectory 'build-manifest.json'
            Invoke-BuildStep $context 'rust-build' $buildIdentity {
                $ordinary = @('build','--locked','--bin','himind-agent','--bin','himind-agent-launcher','--bin','himind-agent-updater')
                $companion = @('build','--locked','--features','mcp-console','--bin','himind-agent-mcp')
                if ($Configuration -eq 'release') { $ordinary += '--release'; $companion += '--release' }
                Invoke-BuildCommand 'cargo' $ordinary $Repository (Join-Path $context.evidence 'build-agent.log')
                Invoke-BuildCommand 'cargo' $companion $Repository (Join-Path $context.evidence 'build-mcp.log')
                $outputDirectory = Join-Path $env:CARGO_TARGET_DIR $Configuration
                if ($env:CARGO_BUILD_TARGET) { $outputDirectory = Join-Path $env:CARGO_TARGET_DIR "$($env:CARGO_BUILD_TARGET)\$Configuration" }
                $staging = Assert-BuildPath "$artifactDirectory.staging" -Within (Join-Path $storageRoot 'artifacts')
                if (Test-Path -LiteralPath $staging) { throw "Interrupted staging directory requires explicit cleanup: $staging" }
                [IO.Directory]::CreateDirectory($staging) | Out-Null
                $inventory = @()
                foreach ($name in @('himind-agent','himind-agent-mcp','himind-agent-launcher','himind-agent-updater')) {
                    $file = "$name.exe"
                    Copy-Item -LiteralPath (Join-Path $outputDirectory $file) -Destination (Join-Path $staging $file)
                    $inventory += [ordered]@{file=$file; sha256=Get-BuildFileHash (Join-Path $staging $file); bytes=(Get-Item -LiteralPath (Join-Path $staging $file)).Length; features=$(if ($name -eq 'himind-agent-mcp') { @('mcp-console') } else { @() })}
                }
                $version = [regex]::Match([IO.File]::ReadAllText((Join-Path $Repository 'Cargo.toml')), '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
                $manifest = [ordered]@{schema='himind.build-manifest.v1'; build_input_hash=$result.build_input_hash; source_hash=$source.hash;
                    commit=$source.commit; channel=$BuildChannel; version=$version; configuration=$Configuration; tools=$tools; runtime=$runtime;
                    artifacts=$inventory; verification=$null; created_utc=[DateTime]::UtcNow.ToString('o')}
                Write-BuildJson (Join-Path $staging 'build-manifest.json') $manifest
                if (Test-Path -LiteralPath $artifactDirectory) {
                    # Invalid existing bytes are never overwritten in place.
                    $quarantine = "$artifactDirectory.invalid-$run"
                    Move-Item -LiteralPath $artifactDirectory -Destination $quarantine
                }
                Move-Item -LiteralPath $staging -Destination $artifactDirectory
            } {
                if (-not (Test-Path -LiteralPath $manifestPath)) { return @{missing=$true} }
                [void](Read-BuildManifest $manifestPath)
                return @{manifest=Get-BuildFileHash $manifestPath}
            }
            $frozen = Read-BuildManifest $manifestPath
            $result.manifest = $manifestPath
        }
        $after = Get-BuildSource $Repository
        if ($after.hash -ne $source.hash -or $after.commit -ne $source.commit) { throw 'Source changed during operation; result cannot be reused or deployed' }
        if ($prepareFrontend) { Write-BuildJson $frontendState @{source_hash=$frontendSourceHash;output_hash=$frontOutputHash} }
        Write-BuildJson $previousSource @{files=$source.files; hash=$source.hash; tools_hash=$toolsHash}
        Write-BuildJson (Join-Path $storageRoot "state\slot-use-$slot.json") @{used_utc=[DateTime]::UtcNow.ToString('o')}
        $result.result = 'PASS'
        $result.steps = @($context.steps.ToArray())
        Write-BuildJson $verificationPath $result
        if ($Build -and ($result.full_tests -or $Mode -eq 'local')) {
            $verificationHash = Get-BuildFileHash $verificationPath
            $frozen.verification = @{path=$verificationPath; sha256=$verificationHash}
            $verifiedPath = Join-Path $artifactDirectory ("build-manifest.verified-$verificationHash.json")
            Write-BuildJson $verifiedPath $frozen
            [void](Read-BuildManifest $verifiedPath -RequireVerification -AllowLocalVerification:($Mode -eq 'local'))
            $result.manifest = $verifiedPath
        }
    }
    catch {
        $result.result = 'FAILED'; $result['error'] = $_.Exception.Message
        $finalSource = $null
        try { $finalSource = Get-BuildSource $Repository } catch {}
        if (-not $finalSource -or $finalSource.hash -ne $source.hash -or $finalSource.commit -ne $source.commit) {
            $result['source_consistency'] = 'FAILED'
            # Discard only pointers created by this failed run. Keep immutable
            # evidence and older trusted successes for honest provenance.
            foreach ($step in @($context.steps.ToArray() | Where-Object disposition -eq 'EXECUTED')) {
                $pointerPath = Join-Path $storageRoot "state\$($step.step).json"
                if (Test-Path -LiteralPath $pointerPath) {
                    $pointer = Get-Content -LiteralPath $pointerPath -Raw | ConvertFrom-Json
                    if ([string]$pointer.path -eq [string]$step.path) { Remove-Item -LiteralPath $pointerPath -Force }
                }
            }
        }
        throw
    }
    finally {
        $timer.Stop(); $result.duration_ms = $timer.ElapsedMilliseconds; $result.steps = @($context.steps.ToArray())
        try { Write-BuildJson (Join-Path $context.evidence 'result.json') $result }
        finally {
            if (Get-Variable oldChannel -ErrorAction SilentlyContinue) { Exit-BuildEnvironment @{HIMIND_BUILD_CHANNEL=$oldChannel} }
            if (Get-Variable oldInputHash -ErrorAction SilentlyContinue) { Exit-BuildEnvironment @{HIMIND_BUILD_INPUT_HASH=$oldInputHash} }
            Exit-BuildEnvironment $environment
            Exit-BuildEnvironment $toolchainEnvironment
            $lock.Dispose()
        }
    }
    return [pscustomobject]$result
}
