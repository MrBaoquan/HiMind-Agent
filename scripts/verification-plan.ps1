# Selection is a development aid, never proof of complete release coverage.
function Get-AgentVerificationImpact {
    param([string[]]$Changed)
    $rust = $false; $frontend = $false; $integration = $false; $full = $false
    $selectors = [Collections.Generic.HashSet[string]]::new()
    $bins = [Collections.Generic.HashSet[string]]::new()
    $checks = [Collections.Generic.HashSet[string]]::new()
    $reasons = [Collections.Generic.List[object]]::new()
    foreach ($name in $Changed) {
        $selected = @(); $reason = ''
        if ($name -match '^docs/|^(README|AGENTS|CLAUDE)\.md$|^\.github/copilot-instructions\.md$') {
            $reason = 'Documentation/instructions: links, examples and diff only.'
        }
        elseif ($name -match '^frontend/') {
            $frontend = $true; $reason = 'Frontend inputs: TypeScript/build and UI checks.'
        }
        elseif ($name -match '^integrations/vscode-himind-ai/') {
            $integration = $true; $reason = 'VS Code adapter: its dependencies and integration tests.'
        }
        elseif ($name -match '^scripts/(?:build(?:-support|-pipeline|-cache)?|verify|verification-plan|check-build(?:-[a-z-]+)?|deploy-local|check-deploy-local|runtime-bundle)\.ps1$') {
            [void]$checks.Add('check-build-support.ps1')
            [void]$checks.Add('check-build-pipeline.ps1')
            if ($name -match 'cache|^scripts/(build-support|build-pipeline|verification-plan)\.ps1$') { [void]$checks.Add('check-build-cache.ps1') }
            if ($name -match 'deploy|runtime-bundle|^scripts/(build-support|build-pipeline|verification-plan)\.ps1$') { [void]$checks.Add('check-deploy-local.ps1') }
            $reason = 'Build tooling: PowerShell syntax and isolated pipeline fixtures.'
        }
        elseif ($name -match '^src/(?:launcher|updater|mcp_bridge_main|mcp_bridge|mcp_ipc|mcp_pipe|install_layout)\.rs$') {
            $rust = $true
            foreach ($bin in @('himind-agent-launcher','himind-agent-updater','himind-agent-mcp')) { [void]$bins.Add($bin) }
            $selected = @('install_layout::','mcp::','app::mcp_targets::')
            $reason = 'Companion/installation boundary: all helper binaries and host integration.'
        }
        elseif ($name -match '^src/app/mission[^/]*\.rs$|^src/mission(?:/|\.rs$)') {
            $rust = $true
            $selected = @('mission::','app::mission','workflow::','mcp::','capability::service::')
            $reason = 'Mission changes: store, dispatch, workflow and public MCP/capability callers.'
        }
        elseif ($name -match '^src/(?:session_history(?:_sources)?|session_service)(?:/|\.rs$)|^src/store/acp_sessions\.rs$') {
            $rust = $true
            $selected = @('session_history','session_service::','store::acp_sessions::','mcp::','capability::service::')
            $reason = 'Session changes: adapters, service, persistence and public consumers.'
        }
        elseif ($name -match '^src/(workflow|expert|runtime|executor|store)(?:/|\.rs$)') {
            $rust = $true; $selected = @(($Matches[1] + '::'),'mission::','app::mission','mcp::','capability::service::')
            $reason = 'Runtime module and known workflow/mission/public callers; full release coverage remains separate.'
        }
        else {
            $rust = $true; $frontend = $true; $integration = $true; $full = $true
            $reason = 'Unknown/shared input: conservatively expand to complete verification.'
        }
        foreach ($selector in $selected) { [void]$selectors.Add($selector) }
        $reasons.Add([pscustomobject]@{ path=$name; reason=$reason; selectors=$selected })
    }
    return [pscustomobject]@{ rust=$rust; frontend=$frontend; integration=$integration; full=$full;
        selectors=@($selectors | Sort-Object); bins=@($bins | Sort-Object);
        script_checks=@($checks | Sort-Object); reasons=$reasons.ToArray(); changed=@($Changed) }
}

function Get-BuildChanges {
    param([string]$Repository, [object]$Source, [Collections.IDictionary]$Previous, [string]$BaseRef)
    if ($BaseRef) {
        if ($BaseRef.StartsWith('-')) { throw 'BaseRef must name a Git commit/ref' }
        $revision = & git -C $Repository "--work-tree=$Repository" rev-parse --verify ($BaseRef + '^{commit}')
        if ($LASTEXITCODE) { throw "Cannot resolve verification baseline: $BaseRef" }
        $changed = @(& git -C $Repository "--work-tree=$Repository" -c core.quotePath=false diff --name-only --no-renames $revision --)
        if ($LASTEXITCODE) { throw 'Cannot read cumulative verification diff' }
        $changed += @(& git -C $Repository "--work-tree=$Repository" -c core.quotePath=false ls-files --others --exclude-standard)
        if ($LASTEXITCODE) { throw 'Cannot read untracked verification inputs' }
        return [pscustomobject]@{ baseline=[string]$revision; kind='git-ref'; changed=@($changed | Sort-Object -Unique) }
    }
    $changed = @(@($Source.files.Keys) + @($Previous.Keys) | Sort-Object -Unique -CaseSensitive | Where-Object {
        -not $Source.files.Contains($_) -or -not $Previous.Contains($_) -or $Source.files[$_] -ne $Previous[$_]
    })
    return [pscustomobject]@{ baseline=$null; kind=$(if ($Previous.Count) { 'last-successful-snapshot' } else { 'no-baseline' }); changed=$changed }
}

function Get-BuildTestIdentity {
    param([object]$Source, [object]$Tools, [string]$Repository, [string]$Channel, [string]$FrontendHash, [string]$Selector)
    # Commit is provenance, not a semantic test input. All executable inputs,
    # tests, fixtures, script support and tool options remain conservative inputs.
    $inputs = Select-BuildSource $Source.files '^(?!docs/|(?:README|AGENTS|CLAUDE)\.md$|\.github/copilot-instructions\.md$)'
    return [ordered]@{ schema='himind.test-input.v2'; repository=$Repository; files=$inputs;
        tools=$Tools; channel=$Channel; frontend=$FrontendHash; selector=$Selector; serial=$true }
}

function Get-BuildStepOutputHash {
    param([object]$Context, [string]$Name)
    $step = @($Context.steps | Where-Object step -eq $Name)
    if ($step.Count -ne 1) { throw "Expected one output receipt: $Name" }
    return $step[0].output_hash
}

function Assert-LocalVerificationBaseline {
    param([Collections.IDictionary]$Files, [object]$Tools, [string]$Channel, [string]$Configuration, [object]$Runtime, [object]$Baseline)
    if ($Channel -ne 'local') { throw 'Incremental installation is restricted to the local channel' }
    if ((Get-BuildFileHash $Baseline.path) -ne $Baseline.sha256) { throw 'Local baseline manifest is missing or modified' }
    $manifest = Read-BuildManifest $Baseline.path -RequireVerification
    $original = Get-Content -LiteralPath $manifest.verification.path -Raw | ConvertFrom-Json -AsHashtable
    if (-not $original.ContainsKey('source_files') -or (Get-BuildHash $original.source_files) -ne $manifest.source_hash) {
        throw 'Baseline has no matching source inventory; create one complete candidate with the current scripts'
    }
    $identityChanges = @()
    if ((Get-BuildHash $manifest.tools) -ne (Get-BuildHash $Tools)) { $identityChanges += 'toolchain/options/environment' }
    if ($manifest.channel -ne $Channel) { $identityChanges += 'channel' }
    if ($manifest.configuration -ne $Configuration) { $identityChanges += 'configuration' }
    if ((Get-BuildHash $manifest.runtime) -ne (Get-BuildHash $Runtime)) { $identityChanges += 'Runtime' }
    if ($identityChanges.Count) { throw "Local baseline identity changed ($($identityChanges -join ', ')); complete candidate required" }
    $changed = @(@($Files.Keys) + @($original.source_files.Keys) | Sort-Object -Unique -CaseSensitive | Where-Object {
        -not $Files.Contains($_) -or -not $original.source_files.ContainsKey($_) -or $Files[$_] -ne $original.source_files[$_]
    })
    $unsupported = @($changed | Where-Object { $_ -notmatch '^frontend/|^integrations/vscode-himind-ai/|^docs/|^(README|AGENTS|CLAUDE)\.md$|^\.github/copilot-instructions\.md$' })
    if ($unsupported.Count) { throw "Local baseline cannot cover these inputs; complete candidate required: $($unsupported -join ', ')" }
    return [ordered]@{ path=$Baseline.path;sha256=$Baseline.sha256;policy='local-ui-adapter-v1';
        baseline_commit=$manifest.commit;changed=$changed;rust_tests='REUSED_FROM_COMPLETE_BASELINE';
        original_verification=$manifest.verification }
}

function Invoke-SelectedRustTests {
    param([string]$Repository, [string]$Evidence, [string]$Name, [string]$Selector, [string]$Bin='himind-agent')
    $arguments = @('test','--locked','--bin',$Bin)
    if ($Selector -ne 'ALL') { $arguments += $Selector }
    $listPath = Join-Path $Evidence "$Name.list.log"
    Invoke-BuildCommand 'cargo' ($arguments + @('--','--list')) $Repository $listPath
    $count = @(Get-Content -LiteralPath $listPath | Where-Object { $_ -match ': test$' }).Count
    if (-not $count -and $Selector -ne 'ALL') { throw "Selected Rust tests matched zero cases: $Bin / $Selector" }
    Invoke-BuildCommand 'cargo' ($arguments + @('--','--test-threads=1')) $Repository (Join-Path $Evidence "$Name.log")
    Write-BuildJson (Join-Path $Evidence "$Name.selection.json") @{bin=$Bin;selector=$Selector;matched=$count;list_sha256=Get-BuildFileHash $listPath}
}

function Invoke-BuildScriptChecks {
    param([object]$Context, [string]$Repository, [string[]]$Changed, [string[]]$Checks, [object]$Tools)
    $scripts = @($Changed | Where-Object { $_ -match '\.ps1$' -and (Test-Path -LiteralPath (Join-Path $Repository $_)) })
    $identity = @{files=Get-BuildSource $Repository | ForEach-Object { Select-BuildSource $_.files '^scripts/|^build\.rs$|^icons/icon\.ico$' };tools=$Tools}
    Invoke-BuildStep $Context 'powershell-syntax' $identity {
        foreach ($script in $scripts) {
            $parseErrors = $null
            [void][Management.Automation.Language.Parser]::ParseFile((Join-Path $Repository $script), [ref]$null, [ref]$parseErrors)
            if ($parseErrors.Count) { throw ($parseErrors | Out-String) }
        }
    }
    foreach ($check in $Checks) {
        if ($check -notin @('check-build-support.ps1','check-build-pipeline.ps1','check-build-cache.ps1','check-deploy-local.ps1')) { throw "Unknown isolated check: $check" }
        $path = Join-Path $Repository "scripts/$check"
        if (-not (Test-Path -LiteralPath $path)) { throw "Required script check missing: $check" }
        Invoke-BuildStep $Context ($check -replace '\.ps1$','') $identity {
            Invoke-BuildCommand 'pwsh' @('-NoProfile','-File',$path,'-FixtureRoot',(Join-Path $Context.root 'fixtures')) $Repository (Join-Path $Context.evidence "$check.log")
        }
    }
}
