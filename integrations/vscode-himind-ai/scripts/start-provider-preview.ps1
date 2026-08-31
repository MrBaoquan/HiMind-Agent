param(
    [string]$WorkspacePath
)

$ErrorActionPreference = 'Stop'
$extensionRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$packagePath = Join-Path $extensionRoot 'dist/himind-ai-provider-preview.vsix'
if (-not (Test-Path -LiteralPath $packagePath)) {
    throw 'Provider Preview VSIX is missing. Run npm run package:provider-preview first.'
}
if (-not $WorkspacePath) {
    $WorkspacePath = $extensionRoot
}
$resolvedWorkspace = (Resolve-Path -LiteralPath $WorkspacePath).Path

& code --install-extension $packagePath --force
if ($LASTEXITCODE -ne 0) {
    throw 'Unable to install the HiMind Provider Preview extension.'
}

& code --new-window --enable-proposed-api himind.himind-ai $resolvedWorkspace
if ($LASTEXITCODE -ne 0) {
    throw 'Unable to start the HiMind Provider Preview window.'
}
