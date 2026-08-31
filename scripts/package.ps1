[CmdletBinding()]
param(
    [string]$Version,
    [string]$OutputDirectory = "artifacts",
    [ValidateSet("debug", "release")]
    [string]$Configuration = "release",
    [string]$WindowsCodeSigningCertificateThumbprint,
    [string]$WindowsTimestampUrl = "http://timestamp.sectigo.com",
    [switch]$SkipWindowsTimestamp,
    [switch]$RequireAuthenticode,
    [switch]$SkipBuild,
    [switch]$SkipDependencyInstall,
    [switch]$SkipTests
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

$CargoText = [IO.File]::ReadAllText((Join-Path $Root "Cargo.toml"))
$PackageVersion = [regex]::Match($CargoText, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $Version) { $Version = $PackageVersion }
if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9._-]+)?$') {
    throw "Version must be a valid package version."
}
if ($PackageVersion -ne $Version) {
    throw "Requested version $Version does not match Cargo.toml version $PackageVersion."
}

if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot "build.ps1") -Configuration $Configuration -SkipDependencyInstall:$SkipDependencyInstall -SkipTests:$SkipTests
    if ($LASTEXITCODE -ne 0) { throw "Agent build failed." }
}

$OutputRoot = if ([IO.Path]::IsPathRooted($OutputDirectory)) {
    [IO.Path]::GetFullPath($OutputDirectory)
} else {
    [IO.Path]::GetFullPath((Join-Path $Root $OutputDirectory))
}
$ReleaseDirectory = Join-Path $OutputRoot $Version
if (Test-Path -LiteralPath $ReleaseDirectory) {
    throw "Release output already exists: $ReleaseDirectory"
}
New-Item -ItemType Directory -Path $ReleaseDirectory | Out-Null

$BinaryRoot = Join-Path $Root "target\$Configuration"
foreach ($Name in @("himind-agent.exe", "himind-agent-launcher.exe", "himind-agent-updater.exe", "himind-agent-mcp.exe")) {
    $Source = Join-Path $BinaryRoot $Name
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) { throw "Built binary is missing: $Source" }
    Copy-Item -LiteralPath $Source -Destination (Join-Path $ReleaseDirectory $Name)
}

$Thumbprint = if ($WindowsCodeSigningCertificateThumbprint) {
    $WindowsCodeSigningCertificateThumbprint
} else {
    [Environment]::GetEnvironmentVariable("HIMIND_WINDOWS_CODE_SIGNING_CERT_THUMBPRINT", "Process")
}
if ($RequireAuthenticode -and -not $Thumbprint) {
    throw "An Authenticode certificate is required for this release package."
}
if ($Thumbprint) {
    foreach ($Name in @("himind-agent.exe", "himind-agent-launcher.exe", "himind-agent-updater.exe", "himind-agent-mcp.exe")) {
        & (Join-Path $PSScriptRoot "sign-windows-artifact.ps1") -ArtifactPath (Join-Path $ReleaseDirectory $Name) -CertificateThumbprint $Thumbprint -TimestampUrl $WindowsTimestampUrl -SkipTimestamp:$SkipWindowsTimestamp
    }
}

$ContractDirectory = Join-Path $ReleaseDirectory "contracts\business-integration\v1"
New-Item -ItemType Directory -Force -Path $ContractDirectory | Out-Null
Copy-Item -LiteralPath (Join-Path $Root "contracts\business-integration\v1\catalog.schema.json") -Destination $ContractDirectory

$VSCodeArtifact = Join-Path $ReleaseDirectory "resources\vscode\himind-ai.vsix"
& (Join-Path $PSScriptRoot "build-vscode-extension.ps1") -OutputPath $VSCodeArtifact -SkipDependencyInstall:$SkipDependencyInstall | Out-Host
if ($LASTEXITCODE -ne 0) { throw "VS Code integration package build failed." }

$TrackedStatus = @(& git status --porcelain=v1 --untracked-files=normal)
$Commit = (& git rev-parse HEAD).Trim()
$Files = @(Get-ChildItem -LiteralPath $ReleaseDirectory -File -Recurse | Sort-Object FullName | ForEach-Object {
    [pscustomobject]@{
        path = $_.FullName.Substring($ReleaseDirectory.Length).TrimStart('\').Replace('\', '/')
        size = $_.Length
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant()
    }
})
$Manifest = [ordered]@{
    schema_version = 1
    product = "himind-agent"
    version = $Version
    source_commit = $Commit
    source_dirty = ($TrackedStatus.Count -gt 0)
    authenticode_signed = [bool]$Thumbprint
    default_mode = "independent"
    protocol = "himind-agent.business-integration"
    protocol_version = "1"
    files = $Files
}
$Manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $ReleaseDirectory "manifest.json") -Encoding UTF8
$Files | ForEach-Object { "$($_.sha256)  $($_.path)" } | Set-Content -LiteralPath (Join-Path $ReleaseDirectory "checksums.sha256") -Encoding ASCII

$ArchivePath = Join-Path $OutputRoot "himind-agent-$Version-windows-x64.zip"
if (Test-Path -LiteralPath $ArchivePath) { throw "Release archive already exists: $ArchivePath" }
Compress-Archive -Path (Join-Path $ReleaseDirectory "*") -DestinationPath $ArchivePath -CompressionLevel Optimal

[pscustomobject]@{
    version = $Version
    release_directory = $ReleaseDirectory
    archive = $ArchivePath
    sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $ArchivePath).Hash.ToLowerInvariant()
} | ConvertTo-Json
