#Requires -Version 7.0
[CmdletBinding()]
param(
    [ValidateSet('affected','quick','candidate','local')][string]$Mode = 'affected',
    [string]$BaseRef, [string]$BaselineManifest,
    [string]$BuildRoot,
    [string]$RuntimeReleaseDirectory,
    [switch]$Build,
    [switch]$UseSccache,
    [switch]$Explain,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'build-pipeline.ps1')
if ($Build -and $Mode -notin @('candidate','local')) { throw '-Build requires -Mode candidate or local' }
Invoke-AgentBuildPipeline -Repository (Split-Path -Parent $PSScriptRoot) -BuildRoot $BuildRoot -Mode $Mode `
    -BaseRef $BaseRef -BaselineManifest $BaselineManifest -Build:$Build -RuntimeReleaseDirectory $RuntimeReleaseDirectory -UseSccache:$UseSccache -Explain:$Explain -DryRun:$DryRun |
    ConvertTo-Json -Depth 20
