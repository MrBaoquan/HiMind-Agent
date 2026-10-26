[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ReleaseDirectory,
    [Parameter(Mandatory = $true)][string]$Version,
    [switch]$RequireSignature,
    [string]$PublicKeyPath,
    [string]$WindowsCodeSigningCertificateThumbprint
)

$ErrorActionPreference = "Stop"
$release = (Resolve-Path -LiteralPath $ReleaseDirectory).Path
$required = @(
    "himind-agent-$Version-setup.exe",
    "himind-agent-$Version-windows-x64.zip",
    "himind-agent-update.zip",
    "himind-agent-update.json",
    "manifest.json",
    "checksums.sha256"
)
foreach ($name in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $release $name) -PathType Leaf)) {
        throw "Release pair is missing $name."
    }
}

$manifestPath = Join-Path $release "manifest.json"
$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ([int]$manifest.schema_version -ne 1 -or [string]$manifest.product -ne "himind-agent") {
    throw "Release manifest has an unexpected product or schema version."
}
if ([string]$manifest.version -ne $Version) {
    throw "Release manifest version $($manifest.version) does not match $Version."
}
foreach ($file in @($manifest.files)) {
    $path = Join-Path $release ([string]$file.path)
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Release manifest references missing file: $($file.path)"
    }
    $actualHash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    $actualSize = (Get-Item -LiteralPath $path).Length
    if ($actualHash -ne [string]$file.sha256 -or $actualSize -ne [long]$file.size) {
        throw "Release manifest mismatch: $($file.path)"
    }
}

$checksumPath = Join-Path $release "checksums.sha256"
foreach ($line in Get-Content -LiteralPath $checksumPath -Encoding ASCII) {
    if ([string]::IsNullOrWhiteSpace($line)) { continue }
    if ($line -notmatch '^([0-9a-fA-F]{64})  (.+)$') {
        throw "Malformed checksums.sha256 line: $line"
    }
    $expected = $Matches[1].ToLowerInvariant()
    $relative = $Matches[2].Replace('/', '\')
    $path = Join-Path $release $relative
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "checksums.sha256 references missing file: $relative"
    }
    $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "checksums.sha256 mismatch: $relative"
    }
}

$updatePath = Join-Path $release "himind-agent-update.zip"
$update = Get-Content -LiteralPath (Join-Path $release "himind-agent-update.json") -Raw -Encoding UTF8 | ConvertFrom-Json
if ([string]$update.product -ne "himind-agent" -or
    [string]$update.version -ne $Version -or
    [string]$update.file_name -ne "himind-agent-update.zip" -or
    [string]$update.package_type -ne "directory-zip") {
    throw "Self-update manifest identity is inconsistent."
}
$updateFile = Get-Item -LiteralPath $updatePath
$updateHash = (Get-FileHash -LiteralPath $updatePath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($updateHash -ne [string]$update.sha256 -or $updateFile.Length -ne [long]$update.size_bytes) {
    throw "Self-update manifest does not match the update archive."
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [IO.Compression.ZipFile]::OpenRead($updatePath)
try {
    $names = @($archive.Entries | ForEach-Object { $_.FullName })
    if (($names | Sort-Object -Unique).Count -ne $names.Count) {
        throw "Self-update archive contains duplicate entries."
    }
    $requiredEntries = @(
        "himind-agent.exe",
        "himind-agent-mcp.exe",
        "himind-agent-updater.exe",
        "himind-agent-launcher.exe",
        "himind-runtime-manifest.json",
        "himind-runtime-package.zip",
        "himind-runtime-package.signature.json"
    )
    $allowedEntries = $requiredEntries + @("himind-ai.vsix")
    $unexpected = @($names | Where-Object { $allowedEntries -notcontains $_ })
    $missing = @($requiredEntries | Where-Object { $names -notcontains $_ })
    if ($unexpected.Count -gt 0 -or $missing.Count -gt 0) {
        throw "Self-update archive entries are invalid. Missing=[$($missing -join ', ')] unexpected=[$($unexpected -join ', ')]"
    }
}
finally {
    $archive.Dispose()
}

if ($RequireSignature) {
    if ([string]::IsNullOrWhiteSpace($PublicKeyPath) -or
        -not (Test-Path -LiteralPath $PublicKeyPath -PathType Leaf)) {
        throw "A trusted public key is required to verify the signed update."
    }
    $signature = [string]$update.signature
    if ([string]::IsNullOrWhiteSpace($signature) -or
        [string]$update.signature_algorithm -ne "rsa-pss-sha256") {
        throw "Self-update signature metadata is missing or unsupported."
    }
    $opensslCommand = Get-Command openssl -ErrorAction SilentlyContinue
    $openssl = if ($opensslCommand) {
        $opensslCommand.Source
    } else {
        Join-Path $env:ProgramFiles "Git\usr\bin\openssl.exe"
    }
    if (-not (Test-Path -LiteralPath $openssl -PathType Leaf)) { throw "OpenSSL is required." }
    $signatureFile = Join-Path ([IO.Path]::GetTempPath()) "himind-verify-signature-$([guid]::NewGuid().ToString('N')).bin"
    try {
        [IO.File]::WriteAllBytes($signatureFile, [Convert]::FromBase64String($signature))
        & $openssl dgst -sha256 `
            -sigopt rsa_padding_mode:pss `
            -sigopt rsa_pss_saltlen:-1 `
            -verify $PublicKeyPath `
            -signature $signatureFile `
            $updatePath
        if ($LASTEXITCODE -ne 0) { throw "Self-update signature verification failed." }
    }
    finally {
        Remove-Item -LiteralPath $signatureFile -Force -ErrorAction SilentlyContinue
    }
}

if (-not [string]::IsNullOrWhiteSpace($WindowsCodeSigningCertificateThumbprint)) {
    $expectedThumbprint = ($WindowsCodeSigningCertificateThumbprint -replace '\s', '').ToUpperInvariant()
    foreach ($name in @("himind-agent-$Version-setup.exe", "himind-agent.exe")) {
        $signature = Get-AuthenticodeSignature -LiteralPath (Join-Path $release $name)
        if (-not $signature.SignerCertificate -or
            ($signature.SignerCertificate.Thumbprint -replace '\s', '').ToUpperInvariant() -ne $expectedThumbprint -or
            $signature.Status -notin @("Valid", "NotTrusted")) {
            throw "Authenticode verification failed: $name"
        }
    }
}

[pscustomobject]@{
    version = $Version
    release_directory = $release
    manifest_files = @($manifest.files).Count
    update_sha256 = $updateHash
    signed_update_verified = [bool]$RequireSignature
    authenticode_verified = (-not [string]::IsNullOrWhiteSpace($WindowsCodeSigningCertificateThumbprint))
} | ConvertTo-Json -Depth 4