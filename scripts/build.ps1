#Requires -Version 7.0
[CmdletBinding()]
param(
    [ValidateSet('debug','release')][string]$Configuration = 'release',
    [ValidateSet('candidate','local')][string]$Mode = 'candidate',
    [string]$BaseRef, [string]$BaselineManifest,
    [string]$BuildRoot,
    [switch]$SkipDependencyInstall,
    [switch]$SkipTests,
    [switch]$SkipIntegration,
    [string]$RuntimeReleaseDirectory,
    [switch]$SkipRuntimeBundle,
    [switch]$EnableIncremental,
    [switch]$UseSccache,
    [string]$BuildChannel = 'local',
    [switch]$Explain,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'build-pipeline.ps1')
if ($EnableIncremental) { Write-Warning 'Candidate builds use the fixed verify slot without incremental data. Use verify.ps1 -Mode quick for incremental development.' }
$result = Invoke-AgentBuildPipeline -Repository (Split-Path -Parent $PSScriptRoot) -Mode $Mode -BaseRef $BaseRef -BaselineManifest $BaselineManifest -Build `
    -Configuration $Configuration -BuildRoot $BuildRoot -BuildChannel $BuildChannel `
    -SkipDependencyInstall:$SkipDependencyInstall -SkipTests:$SkipTests -SkipIntegration:$SkipIntegration `
    -RuntimeReleaseDirectory $RuntimeReleaseDirectory -SkipRuntimeBundle:$SkipRuntimeBundle `
    -UseSccache:$UseSccache -Explain:$Explain -DryRun:$DryRun
$result | ConvertTo-Json -Depth 20
