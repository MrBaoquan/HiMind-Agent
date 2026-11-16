#Requires -Version 7.0
[CmdletBinding()]
param(
    [ValidateSet('quick','candidate')][string]$Mode = 'quick',
    [string]$BuildRoot,
    [string]$RuntimeReleaseDirectory,
    [switch]$Build,
    [switch]$UseSccache,
    [switch]$Explain,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'build-pipeline.ps1')
if ($Build -and $Mode -ne 'candidate') { throw '-Build requires -Mode candidate' }
Invoke-AgentBuildPipeline -Repository (Split-Path -Parent $PSScriptRoot) -BuildRoot $BuildRoot -Mode $Mode `
    -Build:$Build -RuntimeReleaseDirectory $RuntimeReleaseDirectory -UseSccache:$UseSccache -Explain:$Explain -DryRun:$DryRun |
    ConvertTo-Json -Depth 20
