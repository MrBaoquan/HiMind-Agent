[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$RepoManifestPath = Join-Path $Root "resources\runtime\himind-runtime-release.json"

function Read-RepoRuntimeManifest {
    if (-not (Test-Path -LiteralPath $RepoManifestPath -PathType Leaf)) {
        throw "Repository Runtime manifest is missing: $RepoManifestPath"
    }
    return Get-Content -LiteralPath $RepoManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
}

function Assert-RuntimeReleaseDirectory {
    param([Parameter(Mandatory = $true)][string]$RuntimeReleaseDirectory)

    $runtime = [IO.Path]::GetFullPath($RuntimeReleaseDirectory)
    $manifestPath = Join-Path $runtime "himind-runtime-release.json"
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "Bundled Runtime release manifest is missing: $manifestPath"
    }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    $repo = Read-RepoRuntimeManifest
    if ([string]$manifest.version -ne [string]$repo.version -or
        [string]$manifest.sha256 -ne [string]$repo.sha256 -or
        [string]$manifest.file_name -ne [string]$repo.file_name) {
        throw "Bundled Runtime does not match resources/runtime/himind-runtime-release.json (expected $($repo.version) / $($repo.sha256), got $($manifest.version) / $($manifest.sha256))."
    }
    if ([string]$manifest.product_id -ne "com.himind.runtime.deepseek-harness" -or
        [string]$manifest.channel -ne "stable" -or
        [string]$manifest.platform -ne "windows" -or
        [string]$manifest.architecture -ne "x64" -or
        [string]$manifest.package_type -ne "directory-zip") {
        throw "Bundled Runtime release manifest does not match the Windows Runtime contract."
    }
    $packageName = [string]$manifest.file_name
    if ($packageName -notmatch '^[A-Za-z0-9._-]+\.zip$') {
        throw "Bundled Runtime package file name is unsafe: $packageName"
    }
    $packagePath = Join-Path $runtime $packageName
    $signaturePath = Join-Path $runtime "$packageName.signature.json"
    foreach ($path in @($packagePath, $signaturePath)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Bundled Runtime file is missing: $path"
        }
    }
    $actualHash = (Get-FileHash -LiteralPath $packagePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne ([string]$manifest.sha256).ToLowerInvariant()) {
        throw "Bundled Runtime SHA-256 mismatch: manifest $($manifest.sha256), file $actualHash"
    }
    $signature = Get-Content -LiteralPath $signaturePath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ([string]$signature.file_name -ne $packageName -or
        [string]$signature.sha256 -ne [string]$manifest.sha256 -or
        [string]$signature.signature_algorithm -ne "rsa-pss-sha256" -or
        [string]::IsNullOrWhiteSpace([string]$signature.signature) -or
        [string]::IsNullOrWhiteSpace([string]$signature.signature_key_id)) {
        throw "Bundled Runtime signature metadata does not match the release manifest."
    }
    return [pscustomobject]@{
        Directory = $runtime
        ManifestPath = $manifestPath
        PackageName = $packageName
        Manifest = $manifest
    }
}

function Resolve-RuntimeReleaseDirectory {
    param([string]$RequestedDirectory)

    $candidates = [System.Collections.Generic.List[string]]::new()
    foreach ($value in @(
            $RequestedDirectory,
            [Environment]::GetEnvironmentVariable("HIMIND_RUNTIME_RELEASE_DIRECTORY", "Process"),
            (Join-Path $env:LOCALAPPDATA "HiMindAgent\resources\runtime")
        )) {
        if (-not [string]::IsNullOrWhiteSpace([string]$value)) {
            $candidates.Add([IO.Path]::GetFullPath([string]$value))
        }
    }
    $manualRoot = Join-Path $Root "manual-releases\deepseek-harness-runtime"
    if (Test-Path -LiteralPath $manualRoot -PathType Container) {
        Get-ChildItem -LiteralPath $manualRoot -Directory -Recurse -ErrorAction SilentlyContinue |
            Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName "himind-runtime-release.json") -PathType Leaf } |
            Sort-Object LastWriteTime -Descending |
            ForEach-Object { $candidates.Add($_.FullName) }
    }
    $lastError = $null
    foreach ($candidate in $candidates | Select-Object -Unique) {
        try {
            return Assert-RuntimeReleaseDirectory $candidate
        }
        catch {
            $lastError = $_
        }
    }
    $detail = if ($lastError) { " Last error: $($lastError.Exception.Message)" } else { "" }
    throw "No signed Runtime release matching the Agent's pinned manifest was found. Pass -RuntimeReleaseDirectory or set HIMIND_RUNTIME_RELEASE_DIRECTORY.$detail"
}

function Copy-RuntimeBundle {
    param(
        [Parameter(Mandatory = $true)][string]$RuntimeReleaseDirectory,
        [Parameter(Mandatory = $true)][string]$DestinationDirectory
    )

    $runtime = Assert-RuntimeReleaseDirectory $RuntimeReleaseDirectory
    $destination = [IO.Path]::GetFullPath($DestinationDirectory)
    if (Test-Path -LiteralPath $destination) {
        Remove-Item -LiteralPath $destination -Recurse -Force
    }
    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    Copy-Item -LiteralPath $runtime.ManifestPath -Destination $destination
    Copy-Item -LiteralPath (Join-Path $runtime.Directory $runtime.PackageName) -Destination $destination
    Copy-Item -LiteralPath (Join-Path $runtime.Directory "$($runtime.PackageName).signature.json") -Destination $destination
    return $destination
}