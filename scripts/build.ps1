[CmdletBinding()]
param(
    [ValidateSet("debug", "release")]
    [string]$Configuration = "release",
    [switch]$SkipDependencyInstall,
    [switch]$SkipTests,
    [switch]$SkipIntegration
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

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
Invoke-Checked "Rust compile check" { & cargo check --locked }
if (-not $SkipTests) {
    Invoke-Checked "Rust tests" { & cargo test --locked }
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
Write-Host "HiMind Agent build completed: $TargetDirectory"
