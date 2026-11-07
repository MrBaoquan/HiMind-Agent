[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')]
    [string]$Channel = 'Release',
    [switch]$Build,
    [int]$Port = 18082,
    [string]$Profile = 'development',
    [string]$CertificateThumbprint = '',
    [int]$LaunchTimeoutSeconds = 90
)

$ErrorActionPreference = 'Stop'
# Acceptance binaries are built from a local checkout, not published by CI.
# Mark them as local so `/health` exposes the real build identity.
$env:HIMIND_BUILD_CHANNEL = 'local'
$Root = Split-Path -Parent $PSScriptRoot
$TargetRoot = [IO.Path]::GetFullPath((Join-Path $Root 'target')).TrimEnd('\') + '\'
$ChannelDirectory = $Channel.ToLowerInvariant()
$SourceExecutable = Join-Path $TargetRoot "$ChannelDirectory\himind-agent.exe"
$SourceCompanion = Join-Path $TargetRoot "$ChannelDirectory\himind-agent-mcp.exe"
$Signer = Join-Path $PSScriptRoot 'sign-windows-artifact.ps1'
$Cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'

if ($Port -ne 18082) { throw "Test Agent must use port 18082 (got $Port)." }
if ($Profile -ne 'development') { throw "Test Agent must use the development profile (got $Profile)." }
if (-not (Test-Path -LiteralPath $Signer -PathType Leaf)) { throw "Missing signing script: $Signer" }

$MyStore = [System.Security.Cryptography.X509Certificates.X509Store]::new('My', 'CurrentUser')
$MyStore.Open('ReadOnly')
try {
    $candidates = @($MyStore.Certificates | Where-Object {
        $_.HasPrivateKey -and $_.Subject -like '*CN=HiMind Agent 2026*'
    })
}
finally {
    $MyStore.Close()
}
if ($CertificateThumbprint) {
    $CertificateThumbprint = ($CertificateThumbprint -replace '\s', '').ToUpperInvariant()
    if (-not ($candidates | Where-Object { ($_.Thumbprint -replace '\s', '').ToUpperInvariant() -eq $CertificateThumbprint })) {
        throw "The requested test signing certificate is not available with a private key: $CertificateThumbprint"
    }
}
else {
    if ($candidates.Count -ne 1) {
        throw "Expected exactly one HiMind Agent code-signing certificate in CurrentUser\My; found $($candidates.Count)."
    }
    $CertificateThumbprint = $candidates[0].Thumbprint
}

$TrustedStore = [System.Security.Cryptography.X509Certificates.X509Store]::new('TrustedPublisher', 'CurrentUser')
$TrustedStore.Open('ReadOnly')
try {
    $PublishingCert = $TrustedStore.Certificates | Where-Object {
        ($_.Thumbprint -replace '\s', '').ToUpperInvariant() -eq $CertificateThumbprint
    }
}
finally {
    $TrustedStore.Close()
}
if (-not $PublishingCert) {
    throw "Signing certificate is not in CurrentUser\TrustedPublisher. Add it explicitly before running tests."
}

function Invoke-Checked([string]$Label, [scriptblock]$Command) {
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "$Label failed with exit code $LASTEXITCODE." }
}

function Get-Sha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function Assert-SignedTestBinary([string]$Path) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne 'Valid') {
        throw "Refusing to launch an unverified test binary: $($signature.StatusMessage)"
    }
    $actual = if ($signature.SignerCertificate) { ($signature.SignerCertificate.Thumbprint -replace '\s', '').ToUpperInvariant() } else { '' }
    if ($actual -ne $CertificateThumbprint) {
        throw 'Refusing to launch: test binary signer does not match the expected certificate.'
    }
    return $signature
}

if ($Build) {
    Push-Location (Join-Path $Root 'frontend')
    try { Invoke-Checked 'Agent frontend build' { & npm.cmd run build } }
    finally { Pop-Location }

    if (-not (Test-Path -LiteralPath $Cargo -PathType Leaf)) { $Cargo = 'cargo' }
    Push-Location $Root
    try {
        if ($Channel -eq 'Release') {
            Invoke-Checked 'Agent release build' { & $Cargo build --locked --release --bin himind-agent --bin himind-agent-mcp }
        }
        else {
            Invoke-Checked 'Agent debug build' { & $Cargo build --locked --bin himind-agent --bin himind-agent-mcp }
        }
    }
    finally { Pop-Location }
}

if (-not (Test-Path -LiteralPath $SourceExecutable -PathType Leaf)) {
    throw "Missing $Channel binary: $SourceExecutable. Run with -Build after building the frontend and Agent."
}
if (-not (Test-Path -LiteralPath $SourceCompanion -PathType Leaf)) {
    throw "Missing $Channel MCP companion: $SourceCompanion. Run with -Build so the test instance can start AI/MCP sessions."
}

$sourceSignature = Get-AuthenticodeSignature -LiteralPath $SourceExecutable
$sourceThumbprint = if ($sourceSignature.SignerCertificate) { ($sourceSignature.SignerCertificate.Thumbprint -replace '\s', '').ToUpperInvariant() } else { '' }
if ($sourceSignature.Status -ne 'Valid' -or $sourceThumbprint -ne $CertificateThumbprint) {
    & $Signer -ArtifactPath $SourceExecutable -CertificateThumbprint $CertificateThumbprint -SkipTimestamp
    if ($LASTEXITCODE -ne 0) { throw "Signing failed for $SourceExecutable." }
}
$signature = Assert-SignedTestBinary $SourceExecutable
$sourceSha = Get-Sha256 $SourceExecutable

$existing = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1
if ($existing) {
    $owner = Get-Process -Id $existing.OwningProcess -ErrorAction SilentlyContinue
    if (-not $owner -or -not $owner.Path) {
        throw "Port $Port is owned by an unidentifiable process: pid=$($existing.OwningProcess)"
    }
    $activeSha = Get-Sha256 $owner.Path
    if ($activeSha -ne $sourceSha) {
        throw "Port $Port is owned by a different test binary: pid=$($owner.Id) path=$($owner.Path) sha=$activeSha expected=$sourceSha"
    }
    $signature = Assert-SignedTestBinary $owner.Path
    $launchExecutable = [IO.Path]::GetFullPath($owner.Path)
    $launchRoot = Split-Path -Parent $launchExecutable
    $logRoot = Join-Path $launchRoot 'logs'
    $process = $null
    Write-Host "Reusing $Channel test Agent with matching SHA: pid=$($owner.Id) path=$launchExecutable"
}
else {
    $copyRoot = Join-Path $env:TEMP "himind-agent-test-$ChannelDirectory-$($sourceSha.Substring(0, 12))"
    New-Item -ItemType Directory -Path $copyRoot -Force | Out-Null
    $launchExecutable = Join-Path $copyRoot 'himind-agent.exe'
    $launchCompanion = Join-Path $copyRoot 'himind-agent-mcp.exe'
    Copy-Item -LiteralPath $SourceExecutable -Destination $launchExecutable -Force
    Copy-Item -LiteralPath $SourceCompanion -Destination $launchCompanion -Force
    if ((Get-Sha256 $launchExecutable) -ne $sourceSha) {
        throw 'Copied test binary hash does not match the repository Release artifact.'
    }
    # A binary under the Codex-managed workspace inherits a Low Mandatory Label
    # and cannot write the real development profile. The %TEMP% copy carries the
    # normal Medium label while remaining byte-for-byte the same artifact.
    if (((icacls $launchExecutable 2>&1) -join "`n") -match 'Mandatory Label\\Low Mandatory Level') {
        throw "Refusing to launch a Low-integrity test binary: $launchExecutable"
    }
    $signature = Assert-SignedTestBinary $launchExecutable
    $logRoot = Join-Path $copyRoot 'logs'
    New-Item -ItemType Directory -Path $logRoot -Force | Out-Null
    $logStamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $stdoutLog = Join-Path $logRoot "$Channel-$logStamp.stdout.log"
    $stderrLog = Join-Path $logRoot "$Channel-$logStamp.stderr.log"

    $env:HIMIND_AGENT_PROFILE = $Profile
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9311'
    $env:WEBVIEW2_USER_DATA_FOLDER = Join-Path $copyRoot 'webview'
    Remove-Item Env:HIMIND_AGENT_HOME -ErrorAction SilentlyContinue
    Remove-Item Env:HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE -ErrorAction SilentlyContinue

    $arguments = @('--profile', $Profile, '--local-app', '--local-port', "$Port")
    $process = Start-Process -FilePath $launchExecutable -WorkingDirectory $copyRoot -ArgumentList $arguments -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog -PassThru -WindowStyle Normal
    Write-Host "Started signed $Channel test Agent: pid=$($process.Id) port=$Port profile=$Profile"
    Write-Host "Source artifact: $SourceExecutable"
    Write-Host "Medium-label launch copy: $launchExecutable"
    Write-Host "stdout: $stdoutLog"
    Write-Host "stderr: $stderrLog"
}

$deadline = (Get-Date).AddSeconds($LaunchTimeoutSeconds)
while ((Get-Date) -lt $deadline) {
    try {
        $health = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/health" -TimeoutSec 3
        if ($health.status -eq 'online' -and [int]$health.capabilities -gt 0) {
            if ($health.profile -ne $Profile -or [int]$health.local_port -ne $Port) {
                throw "Health identity mismatch: profile=$($health.profile) port=$($health.local_port)"
            }
            if ([IO.Path]::GetFullPath($health.executable_path) -ne [IO.Path]::GetFullPath($launchExecutable)) {
                throw "Health executable mismatch: $($health.executable_path)"
            }
            $sha = Get-Sha256 $launchExecutable
            $instance = [pscustomobject]@{
                pid = if ($process) { $process.Id } else { $existing.OwningProcess }
                channel = $Channel
                port = $Port
                profile = $Profile
                source_artifact = $SourceExecutable
                launch_executable = $launchExecutable
                sha256 = $sha
                capabilities = $health.capabilities
                webview_pid = (Get-NetTCPConnection -LocalPort 9311 -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty OwningProcess)
            }
            if ($logRoot) {
                New-Item -ItemType Directory -Path $logRoot -Force | Out-Null
                $instancePath = Join-Path $logRoot ("$Channel-" + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.instance.json')
                $instance | ConvertTo-Json | Set-Content -LiteralPath $instancePath -Encoding UTF8
            }
            Write-Host "Test Agent ready: channel=$Channel version=$($health.version) profile=$($health.profile) port=$($health.local_port) capabilities=$($health.capabilities)"
            Write-Host "Source artifact SHA256: $sourceSha"
            Write-Host "Launch executable SHA256: $sha"
            Write-Host "Authenticode: $($signature.Status) / $($signature.SignerCertificate.Subject)"
            exit 0
        }
    }
    catch {
        if ($_.Exception.Message -match 'Health (identity|executable) mismatch') { throw }
    }
    Start-Sleep -Milliseconds 700
}
throw "Test Agent did not become healthy within $LaunchTimeoutSeconds seconds."