[CmdletBinding()]
param(
    [string]$OutputPath,
    [switch]$SkipDependencyInstall
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$ExtensionRoot = Join-Path $Root "integrations\vscode-himind-ai"
if (-not $OutputPath) {
    $OutputPath = Join-Path $ExtensionRoot "dist\himind-ai.vsix"
}
$OutputPath = [IO.Path]::GetFullPath($OutputPath)

Push-Location $ExtensionRoot
try {
    if (-not $SkipDependencyInstall) {
        & npm.cmd ci
        if ($LASTEXITCODE -ne 0) { throw "VS Code integration dependency installation failed." }
    }
    & npm.cmd run package
    if ($LASTEXITCODE -ne 0) { throw "VS Code integration package build failed." }

    $Artifact = (Resolve-Path "dist\himind-ai.vsix").Path
    if ($Artifact -ne $OutputPath) {
        New-Item -ItemType Directory -Force -Path ([IO.Path]::GetDirectoryName($OutputPath)) | Out-Null
        Copy-Item -LiteralPath $Artifact -Destination $OutputPath -Force
    }
    [pscustomobject]@{
        path = $OutputPath
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $OutputPath).Hash.ToLowerInvariant()
    } | ConvertTo-Json
}
finally {
    Pop-Location
}
