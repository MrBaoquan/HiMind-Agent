[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ArtifactPath,
    [Parameter(Mandatory = $true)][string]$PrivateKeyPath,
    [Parameter(Mandatory = $true)][ValidatePattern('^[A-Za-z0-9._-]+$')][string]$KeyId,
    [Parameter(Mandatory = $true)][string]$OutputPath,
    [string]$PackageType = "directory-zip"
)

$ErrorActionPreference = "Stop"
$artifact = (Resolve-Path -LiteralPath $ArtifactPath).Path
$privateKey = (Resolve-Path -LiteralPath $PrivateKeyPath).Path
if ([string]::IsNullOrWhiteSpace($PackageType)) { throw "PackageType is required." }

$opensslCommand = Get-Command openssl -ErrorAction SilentlyContinue
$openssl = if ($opensslCommand) {
    $opensslCommand.Source
} else {
    Join-Path $env:ProgramFiles "Git\usr\bin\openssl.exe"
}
if (-not (Test-Path -LiteralPath $openssl -PathType Leaf)) { throw "OpenSSL is required." }

$outputFullPath = [IO.Path]::GetFullPath($OutputPath)
$outputDirectory = Split-Path -Parent $outputFullPath
New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
$signatureFile = Join-Path ([IO.Path]::GetTempPath()) "himind-artifact-signature-$([guid]::NewGuid().ToString('N')).bin"
try {
    & $openssl dgst -sha256 -sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:-1 -sign $privateKey -out $signatureFile $artifact
    if ($LASTEXITCODE -ne 0) { throw "Artifact signing failed." }
    $signatureBytes = [IO.File]::ReadAllBytes($signatureFile)
    if ($signatureBytes.Length -eq 0) { throw "Artifact signing produced an empty signature." }
    $file = Get-Item -LiteralPath $artifact
    $metadata = [ordered]@{
        file_name = $file.Name
        file_size = $file.Length
        package_type = $PackageType
        sha256 = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash.ToLowerInvariant()
        signature = [Convert]::ToBase64String($signatureBytes)
        signature_key_id = $KeyId
        signature_algorithm = "rsa-pss-sha256"
    }
    [IO.File]::WriteAllText($outputFullPath, ($metadata | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
    $metadata | ConvertTo-Json
}
finally {
    if (Test-Path -LiteralPath $signatureFile) { Remove-Item -LiteralPath $signatureFile -Force }
}