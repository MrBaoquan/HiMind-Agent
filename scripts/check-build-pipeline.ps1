#Requires -Version 7.0
[CmdletBinding()]
param([string]$FixtureRoot)
$ErrorActionPreference = 'Stop'
$previousKeys=@{}
foreach($name in @('HIMIND_SIGNING_PUBLIC_KEY_PATH','HIMIND_EXTENSION_SIGNING_PUBLIC_KEY_PATH')){$previousKeys[$name]=[Environment]::GetEnvironmentVariable($name,'Process')}
try {
. (Join-Path $PSScriptRoot 'build-pipeline.ps1')
if (-not $FixtureRoot) { $FixtureRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\build-pipeline-tests' }
$fixture = Assert-BuildPath (Join-Path $FixtureRoot ([Guid]::NewGuid().ToString('N'))) -Within $FixtureRoot
$repository = Join-Path $fixture 'source with spaces'
$storage = Join-Path $fixture 'managed'
foreach ($path in @('src','frontend','icons','integrations\vscode-himind-ai')) { [IO.Directory]::CreateDirectory((Join-Path $repository $path)) | Out-Null }
function Write-Fixture([string]$Relative, [string]$Text) { [IO.File]::WriteAllText((Join-Path $repository $Relative), ($Text.TrimEnd() + "`n"), [Text.UTF8Encoding]::new($false)) }
function Assert-Pipeline([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
Write-Fixture '.gitignore' "**/node_modules/`n**/dist/`n/target/`n"
Write-Fixture 'Cargo.toml' @'
[package]
name = "himind-agent"
version = "0.0.0"
edition = "2021"
[features]
mcp-console = []
[[bin]]
name = "himind-agent"
path = "src/main.rs"
[[bin]]
name = "himind-agent-mcp"
path = "src/mcp.rs"
[[bin]]
name = "himind-agent-launcher"
path = "src/launcher.rs"
[[bin]]
name = "himind-agent-updater"
path = "src/updater.rs"
'@
Write-Fixture 'Cargo.lock' @'
version = 4
[[package]]
name = "himind-agent"
version = "0.0.0"
'@
Write-Fixture 'src\main.rs' @'
fn main() {
    println!("{}", include_str!("../frontend/dist/index.html"));
    println!("{}", include_str!(concat!(env!("OUT_DIR"), "/embedded-build.json")));
    println!("{}", include_str!(concat!(env!("OUT_DIR"), "/embedded-update-public-key.pem")));
}
#[test]
fn verifies_fixture() {
    assert_eq!(2 + 2, 4);
}
'@
# Exercise the real key/Git watch code with only Tauri packaging replaced by
# a no-op in this dependency-free fixture. Actual Agent packaging runs separately.
Write-Fixture 'build.rs' ("mod tauri_build { pub fn build() {} }`n" + [IO.File]::ReadAllText((Join-Path (Split-Path -Parent $PSScriptRoot) 'build.rs')))
& rustfmt --edition 2021 (Join-Path $repository 'build.rs')
Copy-Item -LiteralPath (Join-Path (Split-Path -Parent $PSScriptRoot) 'icons/icon.ico') -Destination (Join-Path $repository 'icons/icon.ico')
$keyPath=Join-Path $fixture 'fixture-public-key.pem'
[IO.File]::WriteAllText($keyPath,'fixture-public-key')
$env:HIMIND_SIGNING_PUBLIC_KEY_PATH=$keyPath
$env:HIMIND_EXTENSION_SIGNING_PUBLIC_KEY_PATH=$keyPath
foreach ($file in @('mcp','launcher','updater')) { Write-Fixture "src\$file.rs" "fn main() {`n    println!(`"fixture` temporary` binary`");`n}`n" }
Write-Fixture 'frontend\package.json' '{"name":"build-fixture-ui","version":"1.0.0","scripts":{"build":"node build.cjs","check:ui":"node check.cjs"}}'
Write-Fixture 'frontend\package-lock.json' '{"name":"build-fixture-ui","version":"1.0.0","lockfileVersion":3,"requires":true,"packages":{"":{"name":"build-fixture-ui","version":"1.0.0"}}}'
Write-Fixture 'frontend\input.txt' 'first interface'
Write-Fixture 'frontend\build.cjs' 'const fs=require("fs");fs.mkdirSync("dist",{recursive:true});fs.writeFileSync("dist/index.html",fs.readFileSync("input.txt"));'
Write-Fixture 'frontend\check.cjs' 'if(!require("fs").readFileSync("input.txt","utf8").length)throw Error("empty input");'
Write-Fixture 'integrations\vscode-himind-ai\package.json' '{"name":"build-fixture-extension","version":"1.0.0","scripts":{"test":"node -e \"if(2+2!==4)process.exit(1)\""}}'
Write-Fixture 'integrations\vscode-himind-ai\package-lock.json' '{"name":"build-fixture-extension","version":"1.0.0","lockfileVersion":3,"requires":true,"packages":{"":{"name":"build-fixture-extension","version":"1.0.0"}}}'
& rustfmt --edition 2021 (Join-Path $repository 'src/main.rs')
& git init -q $repository
& git -C $repository add .
& git -C $repository -c user.name=HiMindFixture -c user.email=fixture@example.invalid commit -qm fixture
if ($LASTEXITCODE) { throw 'Fixture Git initialization failed' }
$checks = [Collections.Generic.List[string]]::new()
$cold = Invoke-AgentBuildPipeline -Repository $repository -BuildRoot $storage -Mode candidate -Build -Configuration debug -SkipRuntimeBundle
Assert-Pipeline ($cold.result -eq 'PASS' -and $cold.full_tests) 'Cold fixture failed'
[void](Read-BuildManifest $cold.manifest -RequireVerification)
$checks.Add('actual-rust-npm-cold-candidate')
$hot = Invoke-AgentBuildPipeline -Repository $repository -BuildRoot $storage -Mode candidate -Build -Configuration debug -SkipRuntimeBundle
Assert-Pipeline (@($hot.steps | Where-Object {$_.disposition -ne 'REUSED'}).Count -eq 0) 'Unchanged candidate repeated build/test/install'
$checks.Add('identical-candidate-all-steps-reused')
$childScript=Join-Path $fixture 'second-process.ps1'
$pipelinePath=(Join-Path $PSScriptRoot 'build-pipeline.ps1').Replace("'","''")
[IO.File]::WriteAllText($childScript,("`$ErrorActionPreference='Stop'`n. '$pipelinePath'`n`$r=Invoke-AgentBuildPipeline -Repository '"+$repository.Replace("'","''")+"' -BuildRoot '"+$storage.Replace("'","''")+"' -Mode candidate -Build -Configuration debug -SkipRuntimeBundle`nWrite-BuildJson '"+(Join-Path $fixture 'second-process.json').Replace("'","''")+"' `$r"))
& pwsh -NoProfile -File $childScript *> (Join-Path $fixture 'second-process.log')
if($LASTEXITCODE){throw 'Cross-process candidate failed'}
$child=Get-Content (Join-Path $fixture 'second-process.json') -Raw|ConvertFrom-Json
Assert-Pipeline (@($child.steps | Where-Object disposition -ne 'REUSED').Count -eq 0) 'Different PowerShell process missed identical receipts'
$checks.Add('independent-powershell-process-all-reused')

$firstManifest = Read-BuildManifest $hot.manifest -RequireVerification
$executable = Join-Path (Split-Path -Parent $hot.manifest) 'himind-agent.exe'
$executableHash = Get-BuildFileHash $executable
$rustInput = Join-Path $repository 'src\main.rs'
$stamp = [IO.File]::GetLastWriteTimeUtc($rustInput)
[IO.File]::WriteAllText($rustInput, [IO.File]::ReadAllText($rustInput).Replace('println!("{}", include_str!("../frontend/dist/index.html"));','println!("changed {}", include_str!("../frontend/dist/index.html"));'))
[IO.File]::SetLastWriteTimeUtc($rustInput, $stamp)
$rust = Invoke-AgentBuildPipeline -Repository $repository -BuildRoot $storage -Mode candidate -Build -Configuration debug -SkipRuntimeBundle
Assert-Pipeline (($rust.steps | Where-Object {$_.step -eq 'frontend-dependencies'}).disposition -eq 'REUSED') 'Rust-only edit reinstalled UI dependencies'
Assert-Pipeline (($rust.steps | Where-Object {$_.step -eq 'rust-tests-ALL'}).disposition -eq 'EXECUTED') 'Rust change did not execute tests'
Assert-Pipeline ($rust.build_input_hash -ne $hot.build_input_hash) 'Dirty content preserved timestamp but reused old build identity'
Assert-Pipeline ((@(& (Join-Path (Split-Path -Parent $rust.manifest) 'himind-agent.exe')) -join "`n") -match 'changed first interface') 'Preserved-timestamp Rust edit produced stale executable'
$checks.Add('rust-content-delta-without-npm-reinstall')
Write-Fixture 'frontend\input.txt' 'second interface'
$ui = Invoke-AgentBuildPipeline -Repository $repository -BuildRoot $storage -Mode candidate -Build -Configuration debug -SkipRuntimeBundle
Assert-Pipeline (($ui.steps | Where-Object {$_.step -eq 'frontend-build'}).disposition -eq 'EXECUTED') 'UI changed without frontend build'
$newExecutable = Join-Path (Split-Path -Parent $ui.manifest) 'himind-agent.exe'
Assert-Pipeline ((Get-BuildFileHash $newExecutable) -ne $executableHash) 'New interface was not embedded into rebuilt binary'
$checks.Add('ui-delta-rebuilt-embedded-binary')
[IO.File]::WriteAllText($newExecutable, 'damaged binary')
$repaired = Invoke-AgentBuildPipeline -Repository $repository -BuildRoot $storage -Mode candidate -Build -Configuration debug -SkipRuntimeBundle
Assert-Pipeline (($repaired.steps | Where-Object {$_.step -eq 'rust-build'}).disposition -eq 'EXECUTED') 'Damaged artifact reused'
[void](Read-BuildManifest $repaired.manifest -RequireVerification)
$checks.Add('damaged-artifact-recreated')
$keyStamp=[IO.File]::GetLastWriteTimeUtc($keyPath)
[IO.File]::WriteAllText($keyPath,'rotated-fixture-key')
[IO.File]::SetLastWriteTimeUtc($keyPath,$keyStamp)
$keyResult=Invoke-AgentBuildPipeline -Repository $repository -BuildRoot $storage -Mode candidate -Build -Configuration debug -SkipRuntimeBundle
Assert-Pipeline ((@(& (Join-Path (Split-Path -Parent $keyResult.manifest) 'himind-agent.exe')) -join "`n") -match 'rotated-fixture-key') 'Key content rotated but old key embedded'
$checks.Add('public-key-content-with-preserved-timestamp')
# A linked checkout uses a .git file; native Cargo must watch its real branch ref.
$linked=Join-Path $fixture 'linked checkout'
& git -C $repository worktree add -b fixture-linked $linked HEAD 2>&1 | Out-Host
[IO.Directory]::CreateDirectory((Join-Path $linked 'frontend/dist'))|Out-Null
Copy-Item (Join-Path $repository 'frontend/dist/index.html') (Join-Path $linked 'frontend/dist/index.html')
$toolEnv=Initialize-BuildToolchain
$previousTarget=[Environment]::GetEnvironmentVariable('CARGO_TARGET_DIR','Process')
try {
    $env:CARGO_TARGET_DIR=Join-Path $fixture 'git-watch-target'
    Invoke-BuildCommand 'cargo' @('build','--locked','--bin','himind-agent') $linked (Join-Path $fixture 'git-watch-first.log')
    [IO.File]::WriteAllText((Join-Path $linked 'README.md'),'new Git identity')
    & git -C $linked add README.md
    & git -C $linked -c user.name=HiMindFixture -c user.email=fixture@example.invalid commit -qm git-watch
    $newCommit=& git -C $linked rev-parse --short HEAD
    Invoke-BuildCommand 'cargo' @('build','--locked','--bin','himind-agent') $linked (Join-Path $fixture 'git-watch-second.log')
    Assert-Pipeline ((@(& (Join-Path $env:CARGO_TARGET_DIR 'debug/himind-agent.exe')) -join "`n").Contains($newCommit)) 'Linked branch commit remained stale in native Cargo output'
} finally {
    Exit-BuildEnvironment @{CARGO_TARGET_DIR=$previousTarget}
    Exit-BuildEnvironment $toolEnv
}
$checks.Add('native-cargo-linked-worktree-git-watch')
$quick = @()
1..20 | ForEach-Object {
    $quick += Invoke-AgentBuildPipeline -Repository $repository -BuildRoot $storage -Mode quick -SkipIntegration
}
$slots = @(Get-ChildItem -LiteralPath (Join-Path $storage 'slots') -Directory)
Assert-Pipeline ($slots.Count -eq 2) 'Quick calls created unbounded target directories'
Assert-Pipeline (@($quick[-1].steps | Where-Object {$_.step -ne 'rust-check' -and $_.disposition -ne 'REUSED'}).Count -eq 0) 'Unchanged quick repeated install/build/tests'
$checkLog = Join-Path (Split-Path -Parent ($quick[-1].steps | Where-Object step -eq 'rust-check').path) 'rust-check.log'
Assert-Pipeline (-not (Select-String -LiteralPath $checkLog -Pattern 'Compiling |Checking ' -Quiet)) 'Unchanged quick compiled again'
# Dist corruption must be detected even when no source file changed.
[IO.File]::WriteAllText((Join-Path $repository 'frontend/dist/index.html'),'damaged dist')
$distRepair=Invoke-AgentBuildPipeline -Repository $repository -BuildRoot $storage -Mode quick -SkipIntegration
Assert-Pipeline (($distRepair.steps|Where-Object step -eq 'frontend-build').disposition -eq 'EXECUTED') 'Unchanged quick reused damaged dist'
Assert-Pipeline ([IO.File]::ReadAllText((Join-Path $repository 'frontend/dist/index.html')).Contains('second interface')) 'Dist not repaired'
$checks.Add('unchanged-quick-dist-corruption-repaired')
[IO.Directory]::CreateDirectory((Join-Path $repository 'frontend/node_modules'))|Out-Null
[IO.File]::WriteAllText((Join-Path $repository 'frontend/node_modules/tampered'),'bad dependency')
$dependencyRepair=Invoke-AgentBuildPipeline -Repository $repository -BuildRoot $storage -Mode quick -SkipIntegration
Assert-Pipeline (($dependencyRepair.steps|Where-Object step -eq 'frontend-dependencies').disposition -eq 'EXECUTED') 'Corrupted dependency reused'
Assert-Pipeline (-not(Test-Path (Join-Path $repository 'frontend/node_modules/tampered'))) 'npm ci did not restore dependencies'
$checks.Add('corrupt-dependency-tree-reinstalled')
$checks.Add('twenty-quick-calls-two-stable-slots')
$checkScript = Join-Path $repository 'frontend/check.cjs'
$originalCheck = [IO.File]::ReadAllBytes($checkScript)
$originalRust = [IO.File]::ReadAllBytes($rustInput)
try {
    [IO.File]::WriteAllText($checkScript, ([Text.Encoding]::UTF8.GetString($originalCheck) + "`nrequire('fs').appendFileSync('../src/main.rs', '\n// source changed during checks\n');"))
    $driftFailure = $null
    try { Invoke-AgentBuildPipeline -Repository $repository -BuildRoot $storage -Mode candidate } catch { $driftFailure = $_.Exception.Message }
    Assert-Pipeline ($driftFailure -like '*Source changed during operation*') "Expected source drift rejection: $driftFailure"
    $latest = Get-ChildItem -LiteralPath (Join-Path $storage 'evidence') -Directory | Sort-Object Name -Descending | Select-Object -First 1
    $failedResult = Get-Content -LiteralPath (Join-Path $latest.FullName 'result.json') -Raw | ConvertFrom-Json
    foreach ($step in @($failedResult.steps | Where-Object disposition -eq 'EXECUTED')) {
        $pointerPath = Join-Path $storage "state/$($step.step).json"
        Assert-Pipeline (-not (Test-Path -LiteralPath $pointerPath) -or (Get-Content -LiteralPath $pointerPath -Raw | ConvertFrom-Json).path -ne $step.path) 'Source drift left a reusable receipt from the failed run'
    }
    $dependencyPointer = Get-Content -LiteralPath (Join-Path $storage 'state/frontend-dependencies.json') -Raw | ConvertFrom-Json
    Assert-Pipeline (Test-Path -LiteralPath $dependencyPointer.path) 'Source drift removed earlier trusted evidence'
    $checks.Add('source-drift-invalidates-only-current-run-receipts')
} finally {
    [IO.File]::WriteAllBytes($checkScript, $originalCheck)
    [IO.File]::WriteAllBytes($rustInput, $originalRust)
}
$report = @{passed=$true;checks=$checks.ToArray();fixture=$fixture;cold=$cold;hot=$hot;rust=$rust;ui=$ui;quick_last=$quick[-1]}
Write-BuildJson (Join-Path $fixture 'report.json') $report
@{passed=$true;checks=$checks.ToArray();report=(Join-Path $fixture 'report.json')} | ConvertTo-Json -Depth 6

} finally { Exit-BuildEnvironment $previousKeys }
