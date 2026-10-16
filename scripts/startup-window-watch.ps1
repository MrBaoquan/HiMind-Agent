<#
.SYNOPSIS
    Measure what a HiMind Agent start puts on screen.

.DESCRIPTION
    A desktop double-click must not flash console windows over the desktop, and
    the main window must appear. This script starts the Agent the way the desktop
    shortcut does (ShellExecute through Start-Process, so a console-subsystem
    launcher would get a console window) and then samples every top-level window
    every few milliseconds, so windows that live a few hundred milliseconds are
    caught with their class, title, owner and lifetime.

    A console window takes the title of the console program running inside it,
    so a flashing window titled `C:\WINDOWS\system32\icacls.exe` means the Agent
    spawned icacls. Only windows that appeared during the sampling window are
    reported: windows that were already there are the desktop's, not the Agent's.

    Read-only apart from starting the Agent. Use -NoLaunch to sample a start you
    trigger yourself (double-click the desktop shortcut) instead.

.PARAMETER Seconds
    Sampling window. A cold start that reaches the main window takes 5-15 s.

.PARAMETER IntervalMs
    Window sampling interval. 5 ms is enough to see console windows.

.PARAMETER Target
    Executable to start. Defaults to the installed launcher, which is what the
    desktop shortcut points at.

.PARAMETER Arguments
    Arguments for the target. Defaults to the installed shortcut's arguments.

.PARAMETER NoLaunch
    Only sample; do not start anything.

.EXAMPLE
    .\scripts\startup-window-watch.ps1 -Seconds 20

.EXAMPLE
    # Sample a start you trigger yourself from the desktop.
    .\scripts\startup-window-watch.ps1 -Seconds 20 -NoLaunch
#>
[CmdletBinding()]
param(
    [int]$Seconds = 20,
    [int]$IntervalMs = 5,
    [string]$Target,
    [string[]]$Arguments,
    [switch]$NoLaunch
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $Target) {
    $Target = Join-Path $env:LOCALAPPDATA 'HiMindAgent\himind-agent-launcher.exe'
}
if (-not $PSBoundParameters.ContainsKey('Arguments')) {
    $Arguments = @('--api', 'https://himind.andcrane.com', '--local-app', '--local-port', '18181')
}

Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;

public sealed class WindowSighting
{
    public string Title = "";
    public string ClassName = "";
    public uint ProcessId;
    public string ProcessName = "";
    public double FirstSeenMs;
    public double LastSeenMs;
    public bool Preexisting;
    public bool Visible;
    public bool EverVisible;
    public bool IsConsole;
    public double LifetimeMs { get { return Math.Max(0.0, LastSeenMs - FirstSeenMs); } }
}

public static class StartupWindowWatch
{
    private delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern int GetClassName(IntPtr hWnd, StringBuilder lpClassName, int nMaxCount);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern int GetWindowText(IntPtr hWnd, StringBuilder lpString, int nMaxCount);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr hWnd);

    private static readonly string[] ConsoleClasses = new string[]
    {
        "ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS", "PseudoConsoleWindow"
    };

    private static bool IsConsoleClass(string value)
    {
        foreach (var candidate in ConsoleClasses)
        {
            if (string.Equals(candidate, value, StringComparison.Ordinal)) { return true; }
        }
        return false;
    }

    public static List<WindowSighting> Sample(int totalMs, int intervalMs, out int passes)
    {
        var sightings = new List<WindowSighting>();
        var current = new Dictionary<string, WindowSighting>();
        var names = new Dictionary<uint, string>();
        var stopwatch = Stopwatch.StartNew();
        var firstPass = true;
        var count = 0;

        while (stopwatch.ElapsedMilliseconds < totalMs)
        {
            var now = stopwatch.Elapsed.TotalMilliseconds;
            var stillCurrent = new HashSet<string>();

            EnumWindows(delegate(IntPtr hWnd, IntPtr lParam)
            {
                var classBuffer = new StringBuilder(256);
                GetClassName(hWnd, classBuffer, classBuffer.Capacity);
                var className = classBuffer.ToString();
                var titleBuffer = new StringBuilder(2048);
                GetWindowText(hWnd, titleBuffer, titleBuffer.Capacity);
                var title = titleBuffer.ToString();
                uint processId = 0;
                GetWindowThreadProcessId(hWnd, out processId);

                // Console hosts retitle one window as each console program runs
                // inside it, so a sighting is a handle *and* a title.
                var key = hWnd.ToString() + "|" + className + "|" + title;
                stillCurrent.Add(key);

                WindowSighting sighting;
                if (!current.TryGetValue(key, out sighting))
                {
                    sighting = new WindowSighting();
                    sighting.Title = title;
                    sighting.ClassName = className;
                    sighting.ProcessId = processId;
                    sighting.Visible = IsWindowVisible(hWnd);
                    sighting.EverVisible = sighting.Visible;
                    sighting.IsConsole = IsConsoleClass(className);
                    sighting.Preexisting = firstPass;
                    sighting.FirstSeenMs = now;
                    current[key] = sighting;
                    sightings.Add(sighting);
                }
                sighting.LastSeenMs = now;
                if (IsWindowVisible(hWnd)) { sighting.EverVisible = true; }
                return true;
            }, IntPtr.Zero);

            foreach (var key in new List<string>(current.Keys))
            {
                if (!stillCurrent.Contains(key)) { current.Remove(key); }
            }

            System.Threading.Thread.Sleep(Math.Max(1, intervalMs));
            firstPass = false;
            count++;
        }

        foreach (var sighting in sightings)
        {
            string name;
            if (!names.TryGetValue(sighting.ProcessId, out name))
            {
                try { name = Process.GetProcessById((int)sighting.ProcessId).ProcessName; }
                catch { name = "<exited>"; }
                names[sighting.ProcessId] = name;
            }
            sighting.ProcessName = name;
        }

        passes = count;
        return sightings;
    }
}
"@

$durationMs = $Seconds * 1000
$launched = $null
if (-not $NoLaunch) {
    Write-Host "sampling $Seconds s at $IntervalMs ms; starting: $Target"
    Write-Host "  arguments: $($Arguments -join ' ')"
    $launched = Start-Process -FilePath $Target -ArgumentList $Arguments -PassThru
}
else {
    Write-Host "sampling $Seconds s at $IntervalMs ms; waiting for a start you trigger by hand"
}

$passes = 0
$sightings = [StartupWindowWatch]::Sample($durationMs, $IntervalMs, [ref]$passes)

if ($launched) {
    $launched | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
}

$new = @($sightings | Where-Object { -not $_.Preexisting })
$newConsole = @($new | Where-Object { $_.IsConsole -and $_.ClassName -ne 'PseudoConsoleWindow' } | Sort-Object FirstSeenMs)
$agentWindows = @($new | Where-Object { $_.ProcessName -like 'himind-*' } | Sort-Object FirstSeenMs)

Write-Host ''
Write-Host ("sampled {0} passes; {1} windows appeared during the sampling window" -f $passes, $new.Count)
Write-Host ("console windows that appeared: {0}" -f $newConsole.Count)

if ($newConsole.Count -gt 0) {
    Write-Host ''
    $newConsole | Format-Table -AutoSize @{Name = 'At(ms)'; Expression = { [int]$_.FirstSeenMs } }, `
        @{Name = 'Lived(ms)'; Expression = { [int]$_.LifetimeMs } }, ClassName, ProcessName, @{Name = 'Shown'; Expression = { $_.EverVisible } }, Title
}

if ($agentWindows.Count -gt 0) {
    Write-Host 'windows created by HiMind processes during the start:'
    $agentWindows | Format-Table -AutoSize @{Name = 'At(ms)'; Expression = { [int]$_.FirstSeenMs } }, `
        @{Name = 'Lived(ms)'; Expression = { [int]$_.LifetimeMs } }, ClassName, ProcessName, @{Name = 'Shown'; Expression = { $_.EverVisible } }, Title
}
else {
    Write-Host 'no window owned by a HiMind process appeared during the sampling window' -ForegroundColor Yellow
}

Write-Host ''
if ($newConsole.Count -eq 0) {
    Write-Host 'RESULT: no console window appeared - the start is quiet.' -ForegroundColor Green
}
else {
    Write-Host ("RESULT: {0} console window(s) flashed during this start." -f $newConsole.Count) -ForegroundColor Yellow
    $newConsole | Group-Object Title | Sort-Object Count -Descending | ForEach-Object {
        Write-Host ("  {0,3} x {1}" -f $_.Count, $(if ($_.Name) { $_.Name } else { '<untitled>' }))
    }
}
