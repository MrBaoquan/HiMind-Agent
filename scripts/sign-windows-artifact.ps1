[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ArtifactPath,
    [Parameter(Mandatory = $true)]
    [string]$CertificateThumbprint,
    [string]$TimestampUrl = "http://timestamp.sectigo.com",
    [switch]$SkipTimestamp
)

$ErrorActionPreference = "Stop"
$Artifact = [IO.Path]::GetFullPath($ArtifactPath)
if (-not (Test-Path -LiteralPath $Artifact -PathType Leaf)) { throw "Windows artifact not found: $Artifact" }
$Thumbprint = ($CertificateThumbprint -replace '\s', '').ToUpperInvariant()
if ($Thumbprint -notmatch '^[0-9A-F]{40,128}$') { throw "Invalid Windows code-signing certificate thumbprint." }

$TimestampUri = $null
if (-not $SkipTimestamp -and
    (-not [Uri]::TryCreate($TimestampUrl, [UriKind]::Absolute, [ref]$TimestampUri) -or $TimestampUri.Scheme -notin @("http", "https"))) {
    throw "TimestampUrl must be an absolute HTTP(S) URL."
}

$Certificate = $null
$MachineStore = $false
foreach ($Candidate in @(Get-ChildItem "Cert:\CurrentUser\My\$Thumbprint" -ErrorAction SilentlyContinue)) {
    if ($Candidate.HasPrivateKey) { $Certificate = $Candidate; break }
}
if (-not $Certificate) {
    foreach ($Candidate in @(Get-ChildItem "Cert:\LocalMachine\My\$Thumbprint" -ErrorAction SilentlyContinue)) {
        if ($Candidate.HasPrivateKey) { $Certificate = $Candidate; $MachineStore = $true; break }
    }
}
if (-not $Certificate) { throw "Windows code-signing certificate with a private key was not found." }

$SignTool = Get-Command signtool.exe -ErrorAction SilentlyContinue
$SignToolPath = if ($SignTool) { $SignTool.Source } else {
    $KitsRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    if (Test-Path -LiteralPath $KitsRoot) {
        Get-ChildItem -LiteralPath $KitsRoot -Filter signtool.exe -File -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
            Sort-Object FullName -Descending |
            Select-Object -ExpandProperty FullName -First 1
    }
}
if (-not $SignToolPath) { throw "signtool.exe is required for Authenticode signing." }

$Arguments = @("sign", "/fd", "SHA256", "/sha1", $Thumbprint)
if ($MachineStore) { $Arguments += "/sm" }
if (-not $SkipTimestamp) { $Arguments += @("/tr", $TimestampUri.AbsoluteUri, "/td", "SHA256") }
$Arguments += $Artifact
& $SignToolPath @Arguments
if ($LASTEXITCODE -ne 0) { throw "Authenticode signing failed: $Artifact" }

$Signature = Get-AuthenticodeSignature -LiteralPath $Artifact
if ($Signature.Status -ne "Valid" -or -not $Signature.SignerCertificate) {
    throw "Authenticode verification failed: $($Signature.StatusMessage)"
}
Write-Host "Authenticode signed: $Artifact"
