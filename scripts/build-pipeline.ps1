. (Join-Path $PSScriptRoot 'build-support.ps1')

function Invoke-AgentBuildPipeline {
    [CmdletBinding()]
    param(
        [string]$Repository, [string]$BuildRoot,
        [ValidateSet('quick','candidate')][string]$Mode = 'quick',
        [ValidateSet('debug','release')][string]$Configuration = 'release',
        [string]$BuildChannel = 'local', [switch]$Build,
        [switch]$SkipTests, [switch]$SkipIntegration, [switch]$SkipDependencyInstall,
        [string]$RuntimeReleaseDirectory, [switch]$SkipRuntimeBundle,
        [switch]$UseSccache, [switch]$Explain, [switch]$DryRun
    )
    $Repository = Assert-BuildPath $Repository
    $storageRoot = Get-BuildRoot $BuildRoot -ReadOnly:($Explain -or $DryRun)
    $slot = if ($Mode -eq 'quick') { 'dev' } else { 'verify' }
    $source = Get-BuildSource $Repository
    $previousSource = Join-Path $storageRoot "state\source-$slot.json"
    $oldFiles = @{}
    $previousToolsHash = ''
    if (Test-Path -LiteralPath $previousSource) {
        $prior = Get-Content -LiteralPath $previousSource -Raw | ConvertFrom-Json -AsHashtable
        $oldFiles = $prior.files
        if ($prior.ContainsKey('tools_hash')) { $previousToolsHash = $prior.tools_hash }
    }
    $changed = @(@($source.files.Keys) + @($oldFiles.Keys) | Sort-Object -Unique -CaseSensitive | Where-Object {
        -not $source.files.Contains($_) -or -not $oldFiles.ContainsKey($_) -or $source.files[$_] -ne $oldFiles[$_]
    })
    $impact = Get-BuildImpact $changed
    if ($Explain -or $DryRun) {
        return [pscustomobject]@{ schema = 'himind.build-plan.v1'; mode = $Mode; read_only = $true; root = $storageRoot;
            slot = Join-Path $storageRoot "slots\$slot\target"; source_hash = $source.hash; impact = $impact;
            steps = @('dependency identity','frontend identity','format/check or selected tests','content-verified receipt reuse',$(if ($Build) { 'freeze canonical four binaries' }));
            fallback = 'Unknown changes widen verification; missing or modified receipts/outputs execute again.' }
    }
    $lock = Enter-BuildLock $storageRoot
    $environment = @{}; $toolchainEnvironment = @{}
    $run = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfff') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,8)
    $context = [pscustomobject]@{ root = $storageRoot; evidence = Join-Path $storageRoot "evidence\$run"; steps = [Collections.Generic.List[object]]::new() }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $result = [ordered]@{ schema = 'himind.verification.v1'; result = 'FAILED'; mode = $Mode; source_hash = $source.hash;
        build_input_hash = ''; full_tests = $false; manifest = $null; steps = @(); duration_ms = 0; slot = $slot; capacity = @() }
    try {
        $result.capacity = @(Assert-BuildCapacity @(@{Path=$storageRoot; Bytes=6GB}, @{Path=$Repository; Bytes=512MB}))
        $toolchainEnvironment = Initialize-BuildToolchain
        $environment = Enter-BuildEnvironment $storageRoot $slot -UseSccache:$UseSccache
        $oldChannel = [Environment]::GetEnvironmentVariable('HIMIND_BUILD_CHANNEL','Process')
        $env:HIMIND_BUILD_CHANNEL = $BuildChannel
        $tools = Get-BuildTools
        $toolsHash = Get-BuildHash $tools
        if ($toolsHash -ne $previousToolsHash) { $impact = Get-BuildImpact @('__toolchain_or_environment__') }
        $rustFiles = Select-BuildSource $source.files '^(src/|Cargo\.(toml|lock)$|build\.rs$|\.cargo/|tauri\.conf\.json$|icons/|capabilities/|contracts/|resources/)'
        $frontendFiles = Select-BuildSource $source.files '^frontend/'
        $supportFiles = Select-BuildSource $source.files '^scripts/(build|verify|runtime-bundle|check-build)'
        $rustIdentity = [ordered]@{ repository=$Repository; files=$rustFiles; tools=$tools; support=$supportFiles; channel=$BuildChannel; commit=$source.commit }
        $frontDirectory = Join-Path $Repository 'frontend'
        $dependencyDirectory = Join-Path $frontDirectory 'node_modules'
        $npmEnvironment = [ordered]@{}
        foreach ($name in @('npm_config_registry','npm_config_omit','npm_config_ignore_scripts','npm_config_production','NODE_ENV','NODE_OPTIONS')) { $npmEnvironment[$name] = $tools.environment[$name] }
        $dependencyIdentity = [ordered]@{ package=Get-BuildFileHash (Join-Path $frontDirectory 'package.json'); lock=Get-BuildFileHash (Join-Path $frontDirectory 'package-lock.json'); config=Get-BuildFileHash (Join-Path $frontDirectory '.npmrc'); node=$tools.node; npm=$tools.npm; os=$tools.os;arch=$tools.arch; environment=$npmEnvironment }
        # Explicit legacy skip means "assert installed dependencies", not a fake
        # successful npm install. Ordinary reuse verifies the entire output tree.
        if ($SkipDependencyInstall) {
            if (-not (Test-Path -LiteralPath (Join-Path $dependencyDirectory 'typescript\package.json'))) { throw 'Dependency installation skipped but TypeScript is missing' }
        }
        else {
            Invoke-BuildStep $context 'frontend-dependencies' $dependencyIdentity {
                Invoke-BuildCommand 'npm.cmd' @('ci','--no-audit','--no-fund') $frontDirectory (Join-Path $context.evidence 'npm-ci.log')
            } { Get-BuildTree $dependencyDirectory }
        }
        $distDirectory = Join-Path $frontDirectory 'dist'
        $frontendEnvironment = [ordered]@{}
        foreach ($name in $tools.environment.Keys) { if ($name -match '^VITE_|^TAURI_ENV_') { $frontendEnvironment[$name] = $tools.environment[$name] } }
        $frontIdentity = [ordered]@{ files=$frontendFiles; dependencies=$dependencyIdentity; environment=$frontendEnvironment; installed=Get-BuildHash (Get-BuildTree $dependencyDirectory) }
        # Always validate output receipts, including unchanged quick runs. A
        # deleted or corrupted dist must not hide behind source impact alone.
        Invoke-BuildStep $context 'frontend-build' $frontIdentity {
            Invoke-BuildCommand 'npm.cmd' @('run','build') $frontDirectory (Join-Path $context.evidence 'frontend-build.log')
        } { Get-BuildTree $distDirectory }
        if (-not $SkipTests) {
            Invoke-BuildStep $context 'frontend-checks' $frontIdentity {
                Invoke-BuildCommand 'npm.cmd' @('run','check:ui') $frontDirectory (Join-Path $context.evidence 'frontend-checks.log')
            }
        }
        $oldInputHash = [Environment]::GetEnvironmentVariable('HIMIND_BUILD_INPUT_HASH','Process')
        $env:HIMIND_BUILD_INPUT_HASH = Get-BuildHash ([ordered]@{files=$rustFiles;tools=$tools;channel=$BuildChannel;commit=$source.commit;frontend=Get-BuildHash (Get-BuildTree $distDirectory)})
        Invoke-BuildStep $context 'rust-format' $rustIdentity {
            # Existing repository code has inherited formatting differences.
            # Gate changed Rust files without rewriting unrelated modules.
            $formatFiles = @(& git -C $Repository "--work-tree=$Repository" diff HEAD --name-only -- '*.rs')
            $formatFiles += @(& git -C $Repository "--work-tree=$Repository" ls-files --others --exclude-standard -- '*.rs')
            if (-not $formatFiles.Count -and (Test-Path (Join-Path $Repository 'build.rs'))) { $formatFiles = @('build.rs') }
            $formatFiles = @($formatFiles | Sort-Object -Unique | Where-Object { Test-Path (Join-Path $Repository $_) })
            if ($formatFiles.Count) {
                Invoke-BuildCommand 'rustfmt' (@('--check','--edition','2021','--config','skip_children=true') + $formatFiles) $Repository (Join-Path $context.evidence 'rust-format.log')
            }
        }
        if ($Mode -eq 'quick') {
            Invoke-BuildStep $context 'rust-check' $rustIdentity {
                Invoke-BuildCommand 'cargo' @('check','--locked','--bins') $Repository (Join-Path $context.evidence 'rust-check.log')
            } -Always
        }
        if (-not $SkipTests) {
            $selectors = if ($Mode -eq 'candidate' -or $impact.full) { @('ALL') } else { @($impact.selectors) }
            foreach ($selector in $selectors) {
                $testName = 'rust-tests-' + ($selector -replace '[^A-Za-z0-9-]', '_')
                Invoke-BuildStep $context $testName ([ordered]@{ rust=$rustIdentity; inputs=Select-BuildSource $source.files '^(?!docs/|README\.md$)'; frontend=Get-BuildHash (Get-BuildTree $distDirectory); selector=$selector; serial=$true }) {
                    $testArgs = @('test','--locked')
                    if ($selector -ne 'ALL') { $testArgs += @('--bin','himind-agent',$selector) }
                    $testArgs += @('--','--test-threads=1')
                    Invoke-BuildCommand 'cargo' $testArgs $Repository (Join-Path $context.evidence "$testName.log")
                }
            }
        }
        if ($Mode -eq 'candidate' -and -not $SkipIntegration -and -not $SkipTests) {
            $integration = Join-Path $Repository 'integrations\vscode-himind-ai'
            $integrationDependencies = [ordered]@{ package=Get-BuildFileHash (Join-Path $integration 'package.json'); lock=Get-BuildFileHash (Join-Path $integration 'package-lock.json'); config=Get-BuildFileHash (Join-Path $integration '.npmrc'); node=$tools.node; npm=$tools.npm; os=$tools.os; arch=$tools.arch; environment=$npmEnvironment }
            Invoke-BuildStep $context 'integration-dependencies' $integrationDependencies {
                Invoke-BuildCommand 'npm.cmd' @('ci','--no-audit','--no-fund') $integration (Join-Path $context.evidence 'integration-ci.log')
            } { Get-BuildTree (Join-Path $integration 'node_modules') }
            $integrationIdentity = [ordered]@{ files=Select-BuildSource $source.files '^integrations/vscode-himind-ai/'; dependencies=$integrationDependencies; installed=Get-BuildHash (Get-BuildTree (Join-Path $integration 'node_modules')) }
            Invoke-BuildStep $context 'integration-tests' $integrationIdentity {
                Invoke-BuildCommand 'npm.cmd' @('test') $integration (Join-Path $context.evidence 'integration-tests.log')
            }
        }
        $runtime = $null
        if ($Build -and -not $SkipRuntimeBundle) {
            . (Join-Path $PSScriptRoot 'runtime-bundle.ps1')
            # runtime-bundle uses Root for its repository manifest.
            $Root = $Repository
            $runtimeInfo = Resolve-RuntimeReleaseDirectory $RuntimeReleaseDirectory -RequireExactPackage
            $runtime = [ordered]@{ directory=$runtimeInfo.Directory; manifest_sha256=Get-BuildFileHash $runtimeInfo.ManifestPath;
                package=$runtimeInfo.PackageName; package_sha256=$runtimeInfo.Manifest.sha256;
                signature_sha256=Get-BuildFileHash (Join-Path $runtimeInfo.Directory "$($runtimeInfo.PackageName).signature.json") }
        }
        $buildIdentity = [ordered]@{ source=$source.hash; rust=$rustIdentity; frontend=Get-BuildHash (Get-BuildTree $distDirectory);
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
        Write-BuildJson $previousSource @{files=$source.files; hash=$source.hash; tools_hash=$toolsHash}
        Write-BuildJson (Join-Path $storageRoot "state\slot-use-$slot.json") @{used_utc=[DateTime]::UtcNow.ToString('o')}
        $result.result = 'PASS'
        $result.steps = @($context.steps.ToArray())
        Write-BuildJson $verificationPath $result
        if ($Build -and $result.full_tests) {
            $verificationHash = Get-BuildFileHash $verificationPath
            $frozen.verification = @{path=$verificationPath; sha256=$verificationHash}
            $verifiedPath = Join-Path $artifactDirectory ("build-manifest.verified-$verificationHash.json")
            Write-BuildJson $verifiedPath $frozen
            [void](Read-BuildManifest $verifiedPath -RequireVerification)
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
