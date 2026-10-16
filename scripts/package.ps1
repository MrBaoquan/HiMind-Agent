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
Set-Content -LiteralPath (Join-Path $ReleaseDirectory "himind-agent.version") -Value $Version -Encoding ASCII

$BinaryRoot = Join-Path $Root "target\$Configuration"
foreach ($Name in @("himind-agent.exe", "himind-agent-launcher.exe", "himind-agent-updater.exe", "himind-agent-mcp.exe")) {
    $Source = Join-Path $BinaryRoot $Name
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) { throw "Built binary is missing: $Source" }
    Copy-Item -LiteralPath $Source -Destination (Join-Path $ReleaseDirectory $Name)
}
# Subsystem gate: the Agent and the launcher must be Windows GUI binaries. A
# console-subsystem launcher is handed a console window by Explorer before it can
# detach it, and a console-subsystem Agent would give every console child
# (icacls, cmd, helpers) a window of its own during startup — the black windows
# users see between the double-click and the main window. The MCP companion
# stays a console binary because stdio *is* its interface.
function Get-PeSubsystem {
    param([Parameter(Mandatory = $true)][string]$Path)
    $Bytes = [IO.File]::ReadAllBytes($Path)
    $PeOffset = [BitConverter]::ToInt32($Bytes, 0x3C)
    if ([BitConverter]::ToUInt32($Bytes, $PeOffset) -ne 0x00004550) {
        throw "Not a PE image: $Path"
    }
    return [BitConverter]::ToUInt16($Bytes, $PeOffset + 92)
}
$ExpectedSubsystem = @{
    "himind-agent.exe"          = @{ Value = 2; Name = "GUI" }
    "himind-agent-launcher.exe" = @{ Value = 2; Name = "GUI" }
    "himind-agent-mcp.exe"      = @{ Value = 3; Name = "console" }
}
foreach ($Name in $ExpectedSubsystem.Keys) {
    $Expectation = $ExpectedSubsystem[$Name]
    $Actual = Get-PeSubsystem -Path (Join-Path $ReleaseDirectory $Name)
    if ($Actual -ne $Expectation.Value) {
        throw "$Name must be a $($Expectation.Name)-subsystem binary (subsystem $($Expectation.Value)), found $Actual; a console binary here flashes a black window at every desktop start."
    }
}

# Console-spawn gate: the Agent and the launcher are GUI-subsystem binaries with
# no console, so a console child started without CREATE_NO_WINDOW gets a window
# of its own on the desktop - the black windows users report. The subsystem gate
# above only proves the *parent* cannot flash; this one checks the spawn sites.
$ConsoleSpawnGate = Join-Path $PSScriptRoot "check-console-spawns.ps1"
if (Test-Path -LiteralPath $ConsoleSpawnGate -PathType Leaf) {
    & $ConsoleSpawnGate
    if ($LASTEXITCODE -ne 0) {
        throw "A console program is spawned without CREATE_NO_WINDOW; a desktop start would flash a black window."
    }
}

$RepairScript = Join-Path $PSScriptRoot "repair-storage.ps1"
if (Test-Path -LiteralPath $RepairScript -PathType Leaf) {
    Copy-Item -LiteralPath $RepairScript -Destination (Join-Path $ReleaseDirectory "repair-storage.ps1")
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

# The self-update contract is intentionally stricter than the portable
# package: every entry is a file at the archive root so the updater can
# validate and atomically replace only runtime files.
$UpdateArchivePath = Join-Path $OutputRoot "himind-agent-update.zip"
if (Test-Path -LiteralPath $UpdateArchivePath) { throw "Self-update archive already exists: $UpdateArchivePath" }
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$UpdateArchive = [System.IO.Compression.ZipFile]::Open($UpdateArchivePath, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    $UpdateFiles = @(
        @{ Source = (Join-Path $ReleaseDirectory "himind-agent.exe"); Entry = "himind-agent.exe" },
        @{ Source = (Join-Path $ReleaseDirectory "himind-agent-mcp.exe"); Entry = "himind-agent-mcp.exe" },
        @{ Source = (Join-Path $ReleaseDirectory "himind-agent-updater.exe"); Entry = "himind-agent-updater.exe" },
        @{ Source = (Join-Path $ReleaseDirectory "himind-agent-launcher.exe"); Entry = "himind-agent-launcher.exe" }
    )
    $VSCodeSource = Join-Path $ReleaseDirectory "resources\vscode\himind-ai.vsix"
    if (Test-Path -LiteralPath $VSCodeSource -PathType Leaf) {
        $UpdateFiles += @{ Source = $VSCodeSource; Entry = "himind-ai.vsix" }
    }
    foreach ($File in $UpdateFiles) {
        if (-not (Test-Path -LiteralPath $File.Source -PathType Leaf)) {
            throw "Self-update input is missing: $($File.Source)"
        }
        [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
            $UpdateArchive,
            $File.Source,
            $File.Entry,
            [System.IO.Compression.CompressionLevel]::Optimal
        ) | Out-Null
    }
}
finally {
    $UpdateArchive.Dispose()
}

[pscustomobject]@{
    version = $Version
    release_directory = $ReleaseDirectory
    archive = $ArchivePath
    sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $ArchivePath).Hash.ToLowerInvariant()
    update_archive = $UpdateArchivePath
    update_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $UpdateArchivePath).Hash.ToLowerInvariant()
} | ConvertTo-Json
