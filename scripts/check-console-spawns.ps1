<#
.SYNOPSIS
    Fail when a console program is spawned without CREATE_NO_WINDOW.

.DESCRIPTION
    The Agent and its launcher are Windows GUI-subsystem binaries: they have no
    console. When a GUI process starts a console program (cmd, powershell, git,
    node, icacls, taskkill, ...) Windows gives that child a console window of its
    own, right on top of whatever the user is doing. That is the black window
    users report between a double-click and the main window, and it is invisible
    to every test that does not look at the desktop.

    `src/runtime/process.rs` already provides the two correct shapes:

        hidden_command("git")                     // a Command, already hidden
        let mut command = Command::new(x);         // or the flag applied to an
        configure_hidden_process(&mut command);    // existing Command

    This gate scans for literal console program names started from
    `Command::new(...)` in a function that never applies either one. Sites that
    are deliberately visible - a remote desktop client the user has to see, an
    editor the user has to see - opt out with a `console-window-allow:` comment
    in the same function, so the exception is reviewed instead of inferred.

    Test modules are skipped: a test that shells out to git does not run on a
    user's desktop.

.EXAMPLE
    .\scripts\check-console-spawns.ps1
#>
[CmdletBinding()]
param(
    [string]$SourceRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $SourceRoot) {
    $SourceRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'src'
}
if (-not (Test-Path -LiteralPath $SourceRoot -PathType Container)) {
    throw "Source root not found: $SourceRoot"
}

# Programs with a console. A literal name from this list started without
# CREATE_NO_WINDOW is a window on the user's desktop.
$ConsolePrograms = @(
    'cmd', 'cmd.exe', 'powershell', 'powershell.exe', 'pwsh', 'pwsh.exe',
    'icacls', 'icacls.exe', 'taskkill', 'taskkill.exe', 'tasklist', 'tasklist.exe',
    'wmic', 'reg', 'reg.exe', 'netsh', 'schtasks', 'where', 'where.exe',
    'git', 'git.exe', 'node', 'node.exe', 'npm', 'npx', 'npx.cmd',
    'python', 'python.exe', 'uv', 'uvx', 'cargo', 'cargo.exe',
    'curl', 'tar', 'svn', 'dotnet', 'msbuild',
    'codex', 'claude', 'opencode'
)

$HiddenMarkers = @('hidden_command', 'configure_hidden_process', 'creation_flags')
$FunctionPattern = '^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:async\s+)?fn\s'

$offenders = New-Object System.Collections.Generic.List[object]
$checked = 0

foreach ($file in Get-ChildItem -LiteralPath $SourceRoot -Recurse -Filter *.rs -File) {
    $lines = [System.IO.File]::ReadAllLines($file.FullName)

    # Cut the file at its test module: tests may shell out freely.
    $limit = $lines.Count
    for ($index = 0; $index -lt $lines.Count; $index++) {
        if ($lines[$index] -match '^\s*(?:pub\s+)?mod\s+tests\s*\{') {
            $previous = $index - 1
            while ($previous -ge 0 -and [string]::IsNullOrWhiteSpace($lines[$previous])) { $previous-- }
            if ($previous -ge 0 -and $lines[$previous] -match '^#\[cfg\(test\)\]') {
                $limit = $previous
            }
            break
        }
    }

    for ($index = 0; $index -lt $limit; $index++) {
        if ($lines[$index] -notmatch 'Command::new\(') { continue }
        $literal = [regex]::Match($lines[$index], 'Command::new\(\s*"([^"]+)"\s*\)')
        if (-not $literal.Success) { continue }
        $program = $literal.Groups[1].Value
        if ($ConsolePrograms -notcontains $program) { continue }
        $checked++

        # Enclosing function: back to the previous `fn`, forward to the next one.
        $start = 0
        for ($scan = $index; $scan -ge 0; $scan--) {
            if ($lines[$scan] -match $FunctionPattern) { $start = $scan; break }
        }
        $end = $limit - 1
        for ($scan = $index + 1; $scan -lt $limit; $scan++) {
            if ($lines[$scan] -match $FunctionPattern) { $end = $scan - 1; break }
        }

        $body = ($lines[$start..$end] -join "`n")
        $hidden = $false
        foreach ($marker in $HiddenMarkers) {
            if ($body.Contains($marker)) { $hidden = $true; break }
        }
        if ($hidden) { continue }
        if ($body.Contains('console-window-allow:')) { continue }

        $relative = $file.FullName.Substring($SourceRoot.Length).TrimStart('\', '/')
        $offenders.Add([pscustomobject]@{
            Location = "src\$relative`:$($index + 1)"
            Program  = $program
            Code     = $lines[$index].Trim()
            Function = $lines[$start].Trim()
        })
    }
}

if ($offenders.Count -eq 0) {
    Write-Host "console spawn gate: $checked spawn site(s) checked, all of them hidden." -ForegroundColor Green
    exit 0
}

Write-Host "console spawn gate: $($offenders.Count) spawn site(s) would flash a black window on the user's desktop." -ForegroundColor Yellow
$offenders | Format-Table -AutoSize Location, Program, Code
Write-Host ''
Write-Host 'Use crate::runtime::process::hidden_command(...) or configure_hidden_process(&mut command),'
Write-Host 'or add a `console-window-allow: <why>` comment in that function when the window is the point.'
exit 1
