[CmdletBinding()]
param(
    [ValidateSet("debug", "release")]
    [string]$Configuration = "release",
    [switch]$SkipDependencyInstall,
    [switch]$SkipTests,
    [switch]$SkipIntegration,
    [string]$RuntimeReleaseDirectory,
    [switch]$SkipRuntimeBundle,
    [switch]$EnableIncremental
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root
. (Join-Path $PSScriptRoot "runtime-bundle.ps1")

# Release/package builds are not interactive edit loops. Rust incremental data for
# this crate can grow by several GB per build session, while the release profile
# does not use it anyway. Keep it opt-in for developers who explicitly want it.
if (-not $EnableIncremental) {
    $env:CARGO_INCREMENTAL = "0"
}

function Invoke-Checked {
    param([string]$Description, [scriptblock]$Command)
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "$Description failed." }
}

Push-Location (Join-Path $Root "frontend")
try {
    if (-not $SkipDependencyInstall) {
        Invoke-Checked "Agent frontend dependency installation" { & npm.cmd ci }
    }
    Invoke-Checked "Agent frontend build" { & npm.cmd run build }
}
finally {
    Pop-Location
}

if (-not $SkipIntegration) {
    $IntegrationRoot = Join-Path $Root "integrations\vscode-himind-ai"
    Push-Location $IntegrationRoot
    try {
        if (-not $SkipDependencyInstall) {
            Invoke-Checked "VS Code integration dependency installation" { & npm.cmd ci }
        }
        Invoke-Checked "VS Code integration tests" { & npm.cmd test }
    }
    finally {
        Pop-Location
    }
}

Invoke-Checked "Rust formatting check" { & cargo fmt --check }
$CargoCheckArguments = @("check", "--locked")
if ($Configuration -eq "release") { $CargoCheckArguments += "--release" }
Invoke-Checked "Rust compile check" { & cargo @CargoCheckArguments }
if (-not $SkipTests) {
    # These tests exercise process-wide Agent state, environment variables and temporary homes. Run them serially so a parallel test cannot remove another test's directory or proxy configuration mid-flight.
    Invoke-Checked "Rust tests" { & cargo test --locked -- --test-threads=1 }
}

$AgentBuildArguments = @(
    "build",
    "--locked"
)
if ($Configuration -eq "release") { $AgentBuildArguments += "--release" }
$AgentBuildArguments += @(
    "--bin", "himind-agent",
    "--bin", "himind-agent-launcher",
    "--bin", "himind-agent-updater"
)
Invoke-Checked "Agent binary build" {
    & cargo @AgentBuildArguments
}

$McpBuildArguments = @("build", "--locked")
if ($Configuration -eq "release") { $McpBuildArguments += "--release" }
$McpBuildArguments += @("--features", "mcp-console", "--bin", "himind-agent-mcp")
Invoke-Checked "Agent MCP companion build" {
    & cargo @McpBuildArguments
}

$TargetDirectory = Join-Path $Root "target\$Configuration"
if (-not $SkipRuntimeBundle) {
    $Runtime = Resolve-RuntimeReleaseDirectory $RuntimeReleaseDirectory
    $RuntimeDestination = Copy-RuntimeBundle -RuntimeReleaseDirectory $Runtime.Directory -DestinationDirectory (Join-Path $TargetDirectory "resources\runtime")
    Write-Host "Bundled Runtime pinned: $($Runtime.Manifest.version) -> $RuntimeDestination"
}
Write-Host "HiMind Agent build completed: $TargetDirectory"
