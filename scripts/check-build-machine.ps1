#Requires -Version 7.0
[CmdletBinding()]
param(
    [string]$Repository = (Split-Path -Parent $PSScriptRoot),
    [string]$BuildRoot
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'build-support.ps1')
$root = Get-BuildRoot $BuildRoot -ReadOnly
$report = Get-BuildMachineReport (Assert-BuildPath $Repository) $root
$report | ConvertTo-Json -Depth 8
if (-not $report.ready) { exit 1 }
