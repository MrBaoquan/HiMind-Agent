$ErrorActionPreference = 'Stop'

$extensionRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$providerRoot = (Resolve-Path (Join-Path $extensionRoot 'dist/provider-dev')).Path

& code --new-window "--extensionDevelopmentPath=$providerRoot" --enable-proposed-api himind.himind-ai $extensionRoot
if ($LASTEXITCODE -ne 0) {
    throw 'Unable to start the HiMind VS Code provider development host.'
}
