[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ReleaseDirectory,
    [string]$OutputDirectory = "artifacts",
    [string]$Version,
    [string]$ProductVersion,
    [ValidateSet("independent", "connected")]
    [string]$DefaultMode = "independent",
    [string]$DashboardApiBase = "https://himind.andcrane.com",
    [string]$PublicKeyPath,
    [string]$SigningKeyId,
    [string]$ExtensionPublicKeyPath,
    [string]$ExtensionSigningKeyId,
    [string]$VSCodeExtensionPath,
    [string]$WindowsCodeSigningCertificateThumbprint,
    [string]$WindowsTimestampUrl = "http://timestamp.sectigo.com",
    [switch]$SkipWindowsTimestamp,
    [switch]$RequireAuthenticode,
    [switch]$SkipDependencyInstall
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root
$ReleasePath = [IO.Path]::GetFullPath($ReleaseDirectory)
foreach ($Name in @("himind-agent.exe", "himind-agent-mcp.exe", "himind-agent-launcher.exe", "himind-agent-updater.exe")) {
    if (-not (Test-Path -LiteralPath (Join-Path $ReleasePath $Name) -PathType Leaf)) {
        throw "Required installer input is missing: $Name"
    }
}

$CargoText = [IO.File]::ReadAllText((Join-Path $Root "Cargo.toml"))
$PackageVersion = [regex]::Match($CargoText, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $Version) { $Version = $PackageVersion }
if ($Version -notmatch '^[A-Za-z0-9._-]+$') { throw "Invalid installer version." }
if (-not $ProductVersion) { $ProductVersion = $Version }

$ApiUri = $null
if (-not [Uri]::TryCreate($DashboardApiBase.Trim(), [UriKind]::Absolute, [ref]$ApiUri) -or
    $ApiUri.Scheme -notin @("http", "https") -or -not [string]::IsNullOrEmpty($ApiUri.UserInfo)) {
    throw "DashboardApiBase must be an absolute HTTP(S) URL without credentials."
}

$VSCodeExtension = if ($VSCodeExtensionPath) { [IO.Path]::GetFullPath($VSCodeExtensionPath) } else {
    $DefaultExtension = Join-Path $Root "integrations\vscode-himind-ai\dist\himind-ai.vsix"
    & (Join-Path $PSScriptRoot "build-vscode-extension.ps1") -OutputPath $DefaultExtension -SkipDependencyInstall:$SkipDependencyInstall | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "VS Code integration package build failed." }
    $DefaultExtension
}
if (-not (Test-Path -LiteralPath $VSCodeExtension -PathType Leaf)) { throw "VS Code integration package is missing." }

$MakeNsis = Get-Command makensis.exe -ErrorAction SilentlyContinue
$MakeNsisPath = if ($MakeNsis) { $MakeNsis.Source } else {
    @(
        (Join-Path ${env:ProgramFiles(x86)} "NSIS\makensis.exe"),
        (Join-Path $env:ProgramFiles "NSIS\makensis.exe")
    ) | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } | Select-Object -First 1
}
if (-not $MakeNsisPath) { throw "NSIS makensis.exe is required to build the installer." }

$OutputPath = if ([IO.Path]::IsPathRooted($OutputDirectory)) {
    [IO.Path]::GetFullPath($OutputDirectory)
} else {
    [IO.Path]::GetFullPath((Join-Path $Root $OutputDirectory))
}
New-Item -ItemType Directory -Force -Path $OutputPath | Out-Null
$OutFile = Join-Path $OutputPath "himind-agent-$Version-setup.exe"
$AssetPath = Join-Path $Root "installer\generated"
& (Join-Path $Root "installer\generate-installer-assets.ps1") -OutputDirectory $AssetPath

function Resolve-KeyPair {
    param([string]$KeyPath, [string]$KeyId, [string]$Label)
    if (($KeyPath -and -not $KeyId) -or ($KeyId -and -not $KeyPath)) { throw "$Label path and key ID must be provided together." }
    if ($KeyId -and $KeyId -notmatch '^[A-Za-z0-9._-]+$') { throw "Invalid $Label key ID." }
    if ($KeyPath) {
        $Resolved = [IO.Path]::GetFullPath($KeyPath)
        if (-not (Test-Path -LiteralPath $Resolved -PathType Leaf)) { throw "$Label key was not found." }
        return $Resolved
    }
    return ""
}

$PublicKey = Resolve-KeyPair $PublicKeyPath $SigningKeyId "update signing"
$ExtensionPublicKey = Resolve-KeyPair $ExtensionPublicKeyPath $ExtensionSigningKeyId "extension signing"
$CompilerArguments = @(
    "/INPUTCHARSET", "UTF8",
    "/DRELEASE=$ReleasePath",
    "/DOUTFILE=$OutFile",
    "/DVERSION=$Version",
    "/DPRODUCT_VERSION=$ProductVersion",
    "/DDEFAULT_MODE=$DefaultMode",
    "/DAPI_BASE=$($ApiUri.AbsoluteUri.TrimEnd('/'))",
    "/DASSET_DIR=$AssetPath",
    "/DVSCODE_EXTENSION_VSIX=$VSCodeExtension"
)
if ($PublicKey) { $CompilerArguments += @("/DTRUSTED_PUBLIC_KEY=$PublicKey", "/DSIGNING_KEY_ID=$SigningKeyId") }
if ($ExtensionPublicKey) { $CompilerArguments += @("/DTRUSTED_EXTENSION_PUBLIC_KEY=$ExtensionPublicKey", "/DEXTENSION_SIGNING_KEY_ID=$ExtensionSigningKeyId") }
& $MakeNsisPath @CompilerArguments (Join-Path $Root "installer\himind-agent.nsi")
if ($LASTEXITCODE -ne 0) { throw "Agent installer build failed." }

$Thumbprint = if ($WindowsCodeSigningCertificateThumbprint) {
    $WindowsCodeSigningCertificateThumbprint
} else {
    [Environment]::GetEnvironmentVariable("HIMIND_WINDOWS_CODE_SIGNING_CERT_THUMBPRINT", "Process")
}
if ($RequireAuthenticode -and -not $Thumbprint) { throw "An Authenticode certificate is required for this installer build." }
if ($Thumbprint) {
    & (Join-Path $PSScriptRoot "sign-windows-artifact.ps1") -ArtifactPath $OutFile -CertificateThumbprint $Thumbprint -TimestampUrl $WindowsTimestampUrl -SkipTimestamp:$SkipWindowsTimestamp
    if ($LASTEXITCODE -ne 0) { throw "Installer Authenticode signing failed." }
}

[pscustomobject]@{
    path = $OutFile
    version = $Version
    default_mode = $DefaultMode
    sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $OutFile).Hash.ToLowerInvariant()
} | ConvertTo-Json
