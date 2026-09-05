[CmdletBinding()]
param(
    [string]$Version,
    [string]$Repository = "MrBaoquan/HiMind-Agent",
    [string]$OutputDirectory = "artifacts-github",
    [string]$PrivateKeyPath,
    [string]$PublicKeyPath,
    [string]$SigningKeyId,
    [string]$WindowsCodeSigningCertificateThumbprint,
    [string]$WindowsTimestampUrl = "http://timestamp.sectigo.com",
    [string]$ReleaseNotes = "",
    [switch]$AllowUnsigned,
    [switch]$SkipWindowsTimestamp,
    [switch]$SkipBuild,
    [switch]$SkipTests,
    [switch]$SkipGhRelease,
    [switch]$Prerelease
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

function Resolve-Setting {
    param([string]$Value, [string]$EnvironmentName)
    if (-not [string]::IsNullOrWhiteSpace($Value)) { return $Value }
    return [Environment]::GetEnvironmentVariable($EnvironmentName, "Process")
}

$PrivateKeyPath = Resolve-Setting $PrivateKeyPath "HIMIND_SIGNING_PRIVATE_KEY_PATH"
$PublicKeyPath = Resolve-Setting $PublicKeyPath "HIMIND_SIGNING_PUBLIC_KEY_PATH"
$SigningKeyId = Resolve-Setting $SigningKeyId "HIMIND_SIGNING_KEY_ID"
$hasSigning = -not [string]::IsNullOrWhiteSpace($PrivateKeyPath) -or
    -not [string]::IsNullOrWhiteSpace($PublicKeyPath) -or
    -not [string]::IsNullOrWhiteSpace($SigningKeyId)
if ($hasSigning -and ([string]::IsNullOrWhiteSpace($PrivateKeyPath) -or
        [string]::IsNullOrWhiteSpace($PublicKeyPath) -or
        [string]::IsNullOrWhiteSpace($SigningKeyId))) {
    throw "签名发布必须同时提供私钥、公钥和 key ID。"
}
if (-not $hasSigning -and -not $AllowUnsigned) {
    throw "正式 GitHub 发布默认要求 Agent 自更新包签名；请配置签名材料或显式传入 -AllowUnsigned。"
}
if ($SigningKeyId -and $SigningKeyId -notmatch '^[A-Za-z0-9._-]+$') {
    throw "SigningKeyId contains invalid characters."
}
if ($hasSigning) {
    $PrivateKeyPath = [IO.Path]::GetFullPath($PrivateKeyPath)
    $PublicKeyPath = [IO.Path]::GetFullPath($PublicKeyPath)
    if (-not (Test-Path -LiteralPath $PrivateKeyPath -PathType Leaf)) { throw "Private signing key not found." }
    if (-not (Test-Path -LiteralPath $PublicKeyPath -PathType Leaf)) { throw "Public signing key not found." }
}

$CargoText = [IO.File]::ReadAllText((Join-Path $Root "Cargo.toml"))
$PackageVersion = [regex]::Match($CargoText, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $Version) { $Version = $PackageVersion }
if ($Version -ne $PackageVersion -or $Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') {
    throw "Version must match the Agent Cargo.toml semantic version ($PackageVersion)."
}
if ($Repository -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') {
    throw "Repository must be GitHub owner/repo."
}
if ($Prerelease) {
    throw "当前 Independent 更新 provider 只消费 stable latest Release，不支持 prerelease。"
}

$OutputRoot = if ([IO.Path]::IsPathRooted($OutputDirectory)) {
    [IO.Path]::GetFullPath($OutputDirectory)
} else {
    [IO.Path]::GetFullPath((Join-Path $Root $OutputDirectory))
}
$ReleaseDirectory = Join-Path $OutputRoot $Version
if (Test-Path -LiteralPath $ReleaseDirectory) { throw "Release output already exists: $ReleaseDirectory" }
New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null

$oldPublicKey = [Environment]::GetEnvironmentVariable("HIMIND_SIGNING_PUBLIC_KEY_PATH", "Process")
$oldKeyId = [Environment]::GetEnvironmentVariable("HIMIND_SIGNING_KEY_ID", "Process")
try {
    if ($hasSigning) {
        [Environment]::SetEnvironmentVariable("HIMIND_SIGNING_PUBLIC_KEY_PATH", $PublicKeyPath, "Process")
        [Environment]::SetEnvironmentVariable("HIMIND_SIGNING_KEY_ID", $SigningKeyId, "Process")
    }
    $packageArgs = @{ Version = $Version; OutputDirectory = $OutputRoot; Configuration = "release" }
    if ($WindowsCodeSigningCertificateThumbprint) {
        $packageArgs.WindowsCodeSigningCertificateThumbprint = $WindowsCodeSigningCertificateThumbprint
    }
    if ($WindowsTimestampUrl) { $packageArgs.WindowsTimestampUrl = $WindowsTimestampUrl }
    if ($SkipWindowsTimestamp) { $packageArgs.SkipWindowsTimestamp = $true }
    if ($SkipBuild) { $packageArgs.SkipBuild = $true }
    if ($SkipTests) { $packageArgs.SkipTests = $true }
    & (Join-Path $PSScriptRoot "package.ps1") @packageArgs | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "Agent package build failed." }
}
finally {
    [Environment]::SetEnvironmentVariable("HIMIND_SIGNING_PUBLIC_KEY_PATH", $oldPublicKey, "Process")
    [Environment]::SetEnvironmentVariable("HIMIND_SIGNING_KEY_ID", $oldKeyId, "Process")
}

$portableArchive = Join-Path $OutputRoot "himind-agent-$Version-windows-x64.zip"
$updateArchive = Join-Path $OutputRoot "himind-agent-update.zip"
foreach ($path in @($portableArchive, $updateArchive)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Package output is missing: $path" }
}
Copy-Item -LiteralPath $portableArchive -Destination (Join-Path $ReleaseDirectory (Split-Path $portableArchive -Leaf))
Copy-Item -LiteralPath $updateArchive -Destination (Join-Path $ReleaseDirectory "himind-agent-update.zip")

$installerArgs = @{
    ReleaseDirectory = $ReleaseDirectory
    OutputDirectory = $ReleaseDirectory
    Version = $Version
    ProductVersion = $Version
    DefaultMode = "independent"
    VSCodeExtensionPath = (Join-Path $ReleaseDirectory "resources\vscode\himind-ai.vsix")
}
if ($WindowsCodeSigningCertificateThumbprint) {
    $installerArgs.WindowsCodeSigningCertificateThumbprint = $WindowsCodeSigningCertificateThumbprint
}
if ($WindowsTimestampUrl) { $installerArgs.WindowsTimestampUrl = $WindowsTimestampUrl }
if ($SkipWindowsTimestamp) { $installerArgs.SkipWindowsTimestamp = $true }
if ($hasSigning) {
    $installerArgs.PublicKeyPath = $PublicKeyPath
    $installerArgs.SigningKeyId = $SigningKeyId
}
& (Join-Path $PSScriptRoot "build-installer.ps1") @installerArgs | Out-Host
if ($LASTEXITCODE -ne 0) { throw "Independent Agent installer build failed." }

$signaturePath = Join-Path $ReleaseDirectory "himind-agent-update.signature.json"
if ($hasSigning) {
    & (Join-Path (Split-Path -Parent $Root) "scripts\release\sign-artifact.ps1") `
        -ArtifactPath (Join-Path $ReleaseDirectory "himind-agent-update.zip") `
        -PrivateKeyPath $PrivateKeyPath -KeyId $SigningKeyId `
        -OutputPath $signaturePath -PackageType "directory-zip"
} else {
    $unsigned = [ordered]@{
        file_name = "himind-agent-update.zip"
        package_type = "directory-zip"
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $ReleaseDirectory "himind-agent-update.zip")).Hash.ToLowerInvariant()
        signature = ""
        signature_key_id = ""
        signature_algorithm = ""
    }
    $unsigned | ConvertTo-Json | Set-Content -LiteralPath $signaturePath -Encoding UTF8
}

$signature = Get-Content -LiteralPath $signaturePath -Raw | ConvertFrom-Json
$updateFile = Get-Item -LiteralPath (Join-Path $ReleaseDirectory "himind-agent-update.zip")
$releaseManifest = [ordered]@{
    product = "himind-agent"
    version = $Version
    channel = "stable"
    file_name = "himind-agent-update.zip"
    package_type = "directory-zip"
    size_bytes = $updateFile.Length
    sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $updateFile.FullName).Hash.ToLowerInvariant()
    signature = [string]$signature.signature
    signature_key_id = [string]$signature.signature_key_id
    signature_algorithm = [string]$signature.signature_algorithm
    mandatory = $false
    min_supported_version = "0.0.0"
    release_notes = $ReleaseNotes
}
$releaseManifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $ReleaseDirectory "himind-agent-update.json") -Encoding UTF8

Remove-Item -LiteralPath (Join-Path $ReleaseDirectory "manifest.json"), (Join-Path $ReleaseDirectory "checksums.sha256") -Force -ErrorAction SilentlyContinue
$commit = (& git rev-parse HEAD).Trim()
& (Join-Path (Split-Path -Parent $Root) "scripts\release\generate-manifest.ps1") `
    -ReleaseDirectory $ReleaseDirectory -Version $Version -GitCommit $commit -SourceDirty ((& git status --porcelain).Count -gt 0)
if ($LASTEXITCODE -ne 0) { throw "Release manifest generation failed." }

$pairVerifier = Join-Path (Split-Path -Parent $Root) "scripts\release\verify-agent-release-pair.ps1"
& $pairVerifier -ReleaseDirectory $ReleaseDirectory -RequireSignature:$hasSigning
if ($LASTEXITCODE -ne 0) { throw "Agent release pair verification failed." }

$checksums = Get-ChildItem -LiteralPath $ReleaseDirectory -File -Recurse | Sort-Object FullName | ForEach-Object {
    $relative = $_.FullName.Substring($ReleaseDirectory.Length).TrimStart('\').Replace('\', '/')
    "{0}  {1}" -f (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant(), $relative
}
[IO.File]::WriteAllLines((Join-Path $ReleaseDirectory "SHA256SUMS"), $checksums, [Text.Encoding]::ASCII)

$gh = Get-Command gh -ErrorAction SilentlyContinue
if (-not $SkipGhRelease) {
    if (-not $gh) { throw "GitHub CLI (gh) is required unless -SkipGhRelease is used." }
    $assets = @(Get-ChildItem -LiteralPath $ReleaseDirectory -File |
        Where-Object { $_.Name -ne "release-notes.md" } |
        ForEach-Object FullName)
    $tag = "v$Version"
    $notesArgs = @()
    if (-not [string]::IsNullOrWhiteSpace($ReleaseNotes)) {
        $notesFile = Join-Path $ReleaseDirectory "release-notes.md"
        Set-Content -LiteralPath $notesFile -Value $ReleaseNotes -Encoding UTF8
        $notesArgs = @("--notes-file", $notesFile)
    } else {
        $notesArgs = @("--generate-notes")
    }
    $preArgs = if ($Prerelease) { @("--prerelease") } else { @() }
    & $gh.Source release create $tag @assets @notesArgs @preArgs --repo $Repository --title "HiMind Agent $Version"
    if ($LASTEXITCODE -ne 0) { throw "GitHub Release creation failed." }
}

[pscustomobject]@{
    repository = $Repository
    version = $Version
    channel = "stable"
    release_directory = $ReleaseDirectory
    installer = (Get-ChildItem -LiteralPath $ReleaseDirectory -Filter "himind-agent-*-setup.exe" -File | Select-Object -First 1).Name
    update_archive = "himind-agent-update.zip"
    signed = $hasSigning
    github_release_created = (-not $SkipGhRelease)
} | ConvertTo-Json
