<#
.SYNOPSIS
    Record what a HiMind Agent start puts on screen.

.DESCRIPTION
    A desktop double-click must not flash console windows over the desktop, and
    the main window must appear. This script starts the Agent the way the desktop
    shortcut does (ShellExecute through Start-Process) and records every top-level
    window the desktop creates, plus every process that starts, for the recording
    window.

    Two instruments, because either one alone can lie:

    * windows come from SetWinEventHook, which the system calls for every window
      creation and show. Sampling EnumWindows every few milliseconds looks cheaper
      but ran every ~20 ms in practice, which walks straight past a console window
      that lives 15 ms.
    * processes come from a Toolhelp32 snapshot, recording each new pid with its
      parent. That is the fallback when a window is too short-lived to be seen:
      the process chain still says who started what.

    A console window appears when a console program is started without
    CREATE_NO_WINDOW. On Windows 11 the window class of such a child can be
    `PseudoConsoleWindow` as well as `ConsoleWindowClass`, and the default
    terminal host adds a `CASCADIA_HOSTING_WINDOW_CLASS` window; all three are
    counted. An earlier version filtered `PseudoConsoleWindow` out of its console
    count, which is how a flashing start could be reported as quiet.

    A console window takes the title of the console program running inside it, so
    a window titled `C:\WINDOWS\system32\icacls.exe` means the Agent spawned
    icacls.

    Read-only apart from starting the Agent. Use -NoLaunch to record a start you
    trigger yourself (double-click the desktop shortcut) instead.

.PARAMETER Seconds
    Recording window. A cold start that reaches the main window takes 5-15 s.

.PARAMETER Target
    Executable to start. Defaults to the installed launcher, which is what the
    desktop shortcut points at.

.PARAMETER Arguments
    Arguments for the target. Defaults to the installed shortcut's arguments.
    Pass an array for several values: -Arguments @('--api', 'https://...').

.PARAMETER NoLaunch
    Only record; do not start anything.

.EXAMPLE
    .\scripts\startup-window-watch.ps1 -Seconds 20

.EXAMPLE
    # Record a start you trigger yourself from the desktop.
    .\scripts\startup-window-watch.ps1 -Seconds 20 -NoLaunch
#>
[CmdletBinding()]
param(
    [int]$Seconds = 30,
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

public sealed class StartupWindowRow
{
    public double AtMs;
    public double LastMs;
    public uint Event;
    public bool Preexisting;
    public uint ProcessId;
    public string ProcessName = "";
    public string ClassName = "";
    public string Title = "";
    public bool Visible;
    public bool EverVisible;
    public double LifetimeMs { get { return Math.Max(0.0, LastMs - AtMs); } }
}

public sealed class StartupProcessRow
{
    public double AtMs;
    public double LastMs;
    public bool Preexisting;
    public uint ProcessId;
    public uint ParentProcessId;
    public string Name = "";
}

public sealed class StartupRecording
{
    public List<StartupWindowRow> Windows = new List<StartupWindowRow>();
    public List<StartupProcessRow> Processes = new List<StartupProcessRow>();
}

public static class StartupRecorder
{
    // Console window classes. PseudoConsoleWindow is what this Windows build
    // hands a console child of a GUI process; the default terminal host adds a
    // CASCADIA window on top. Leaving any of them out of the count is how a
    // flashing start was reported as quiet.
    private static readonly string[] ConsoleClasses = new string[]
    {
        "ConsoleWindowClass", "PseudoConsoleWindow", "CASCADIA_HOSTING_WINDOW_CLASS"
    };

    public static bool IsConsoleClass(string value)
    {
        foreach (var candidate in ConsoleClasses)
        {
            if (string.Equals(candidate, value, StringComparison.Ordinal)) { return true; }
        }
        return false;
    }

    private delegate void WinEventDelegate(IntPtr hook, uint evt, IntPtr hwnd, int idObject, int idChild, uint thread, uint time);
    private delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential)]
    private struct MSG
    {
        public IntPtr hwnd;
        public uint message;
        public IntPtr wParam;
        public IntPtr lParam;
        public uint time;
        public int ptX;
        public int ptY;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct PROCESSENTRY32W
    {
        public uint dwSize;
        public uint cntUsage;
        public uint th32ProcessID;
        public IntPtr th32DefaultHeapID;
        public uint th32ModuleID;
        public uint cntThreads;
        public uint th32ParentProcessID;
        public int pcPriClassBase;
        public uint dwFlags;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)]
        public string szExeFile;
    }

    [DllImport("user32.dll")] private static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr hmod, WinEventDelegate callback, uint pid, uint tid, uint flags);
    [DllImport("user32.dll")] private static extern bool UnhookWinEvent(IntPtr hook);
    [DllImport("user32.dll")] private static extern bool PeekMessage(out MSG msg, IntPtr hWnd, uint min, uint max, uint remove);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassName(IntPtr hWnd, StringBuilder buffer, int count);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetWindowText(IntPtr hWnd, StringBuilder buffer, int count);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint pid);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)] private static extern bool Process32FirstW(IntPtr snapshot, ref PROCESSENTRY32W entry);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)] private static extern bool Process32NextW(IntPtr snapshot, ref PROCESSENTRY32W entry);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool CloseHandle(IntPtr handle);

    private static readonly Stopwatch Clock = Stopwatch.StartNew();
    private static List<StartupWindowRow> windowRows;
    // Keyed by window handle, not by handle and title: a window that is retitled
    // is the same window, and keying by title turned "an existing window was
    // shown" into "a window appeared".
    private static Dictionary<IntPtr, StartupWindowRow> rowByHandle;
    private static List<StartupProcessRow> processRows;
    private static Dictionary<uint, StartupProcessRow> processIndex;
    // Names come from the Toolhelp snapshot rather than Process.GetProcessById:
    // opening a protected process throws, and the exception storm from doing that
    // once per desktop window made a 3 second recording take 75 seconds under
    // Windows PowerShell 5.1.
    private static Dictionary<uint, string> processNames;

    private static string ClassOf(IntPtr hwnd)
    {
        var buffer = new StringBuilder(256);
        GetClassName(hwnd, buffer, buffer.Capacity);
        return buffer.ToString();
    }

    private static string TitleOf(IntPtr hwnd)
    {
        var buffer = new StringBuilder(2048);
        GetWindowText(hwnd, buffer, buffer.Capacity);
        return buffer.ToString();
    }

    private static string ProcessNameOf(uint pid)
    {
        if (pid == 0) { return "Idle"; }
        string name;
        return processNames.TryGetValue(pid, out name) ? name : "<unknown>";
    }

    private static StartupWindowRow Sight(IntPtr hwnd, uint evt, bool preexisting)
    {
        StartupWindowRow row;
        if (rowByHandle.TryGetValue(hwnd, out row))
        {
            string title = TitleOf(hwnd);
            if (!string.IsNullOrEmpty(title)) { row.Title = title; }
            row.LastMs = Clock.Elapsed.TotalMilliseconds;
            if (IsWindowVisible(hwnd)) { row.EverVisible = true; }
            return row;
        }

        uint pid;
        GetWindowThreadProcessId(hwnd, out pid);
        row = new StartupWindowRow();
        row.AtMs = preexisting ? 0 : Clock.Elapsed.TotalMilliseconds;
        row.LastMs = Clock.Elapsed.TotalMilliseconds;
        row.Event = evt;
        row.Preexisting = preexisting;
        row.ProcessId = pid;
        row.ProcessName = ProcessNameOf(pid);
        row.ClassName = ClassOf(hwnd);
        row.Title = TitleOf(hwnd);
        row.Visible = IsWindowVisible(hwnd);
        row.EverVisible = row.Visible;
        rowByHandle[hwnd] = row;
        windowRows.Add(row);
        return row;
    }

    private static void OnWindowEvent(IntPtr hook, uint evt, IntPtr hwnd, int idObject, int idChild, uint thread, uint time)
    {
        if (idObject != 0 || hwnd == IntPtr.Zero) { return; }
        Sight(hwnd, evt, false);
    }

    private static void SnapshotProcesses(bool baseline)
    {
        double now = Clock.Elapsed.TotalMilliseconds;
        var entry = new PROCESSENTRY32W();
        uint entrySize = (uint)Marshal.SizeOf(typeof(PROCESSENTRY32W));
        entry.dwSize = entrySize;
        IntPtr snapshot = CreateToolhelp32Snapshot(0x2, 0);
        if (snapshot == new IntPtr(-1)) { return; }
        try
        {
            if (Process32FirstW(snapshot, ref entry))
            {
                do
                {
                    processNames[entry.th32ProcessID] = entry.szExeFile;
                    StartupProcessRow row;
                    if (!processIndex.TryGetValue(entry.th32ProcessID, out row))
                    {
                        row = new StartupProcessRow();
                        row.AtMs = now;
                        row.Preexisting = baseline;
                        row.ProcessId = entry.th32ProcessID;
                        row.ParentProcessId = entry.th32ParentProcessID;
                        row.Name = entry.szExeFile;
                        processIndex[row.ProcessId] = row;
                        processRows.Add(row);
                    }
                    row.LastMs = now;
                    entry.dwSize = entrySize;
                } while (Process32NextW(snapshot, ref entry));
            }
        }
        finally
        {
            CloseHandle(snapshot);
        }
    }

    public static StartupRecording Run(int seconds)
    {
        windowRows = new List<StartupWindowRow>();
        rowByHandle = new Dictionary<IntPtr, StartupWindowRow>();
        processRows = new List<StartupProcessRow>();
        processIndex = new Dictionary<uint, StartupProcessRow>();
        processNames = new Dictionary<uint, string>();

        // Baseline and process table first: everything already on the desktop
        // belongs to the desktop, not to this start. Windows that existed before
        // the hook was installed produce no creation event of their own, so they
        // are sighted once here and stay marked as preexisting - otherwise the
        // first show or focus event for the user's existing window would be
        // reported as a window this start created.
        SnapshotProcesses(true);
        var baseline = new List<IntPtr>();
        EnumWindows(delegate(IntPtr hWnd, IntPtr lParam) { baseline.Add(hWnd); return true; }, IntPtr.Zero);
        foreach (IntPtr hwnd in baseline) { Sight(hwnd, 0, true); }

        var callback = new WinEventDelegate(OnWindowEvent);
        IntPtr hook = SetWinEventHook(0x0001, 0x8002, IntPtr.Zero, callback, 0, 0, 0x0002);
        if (hook == IntPtr.Zero) { throw new InvalidOperationException("SetWinEventHook failed"); }

        double deadline = Clock.Elapsed.TotalSeconds + seconds;
        try
        {
            while (Clock.Elapsed.TotalSeconds < deadline)
            {
                MSG message;
                while (PeekMessage(out message, IntPtr.Zero, 0, 0, 1)) { }
                SnapshotProcesses(false);
                System.Threading.Thread.Sleep(8);
            }
        }
        finally
        {
            UnhookWinEvent(hook);
            GC.KeepAlive(callback);
        }

        var recording = new StartupRecording();
        recording.Windows = windowRows;
        recording.Processes = processRows;
        return recording;
    }
}
"@

$startedAt = Get-Date
$launched = $null
if (-not $NoLaunch) {
    Write-Host ("recording {0} s; starting: {1}" -f $Seconds, $Target)
    Write-Host ("  arguments: {0}" -f ($Arguments -join ' '))
    $launched = Start-Process -FilePath $Target -ArgumentList $Arguments -PassThru
}
else {
    Write-Host ("recording {0} s; waiting for a start you trigger by hand" -f $Seconds)
}

$recording = [StartupRecorder]::Run($Seconds)

if ($launched) {
    $launched | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
}

$windows = @($recording.Windows | Where-Object { -not $_.Preexisting })
$processes = @($recording.Processes | Where-Object { -not $_.Preexisting })
$consoleWindows = @($windows | Where-Object { [StartupRecorder]::IsConsoleClass($_.ClassName) -and $_.EverVisible } | Sort-Object AtMs)
$hiddenConsoleWindows = @($windows | Where-Object { [StartupRecorder]::IsConsoleClass($_.ClassName) -and -not $_.EverVisible } | Sort-Object AtMs)
$agentWindows = @($windows | Where-Object { $_.ProcessName -like 'himind-*' } | Sort-Object AtMs)

# Ancestry for the process table: name(pid) <- parent <- grandparent.
$byPid = @{}
foreach ($process in $processes) { $byPid[[int]$process.ProcessId] = $process }
function Get-ProcessChain {
    param([int]$ProcessId)
    $parts = New-Object System.Collections.Generic.List[string]
    $current = $ProcessId
    for ($depth = 0; $depth -lt 8 -and $byPid.ContainsKey($current); $depth++) {
        $entry = $byPid[$current]
        $parts.Add(('{0}({1})' -f $entry.Name, $entry.ProcessId))
        $current = [int]$entry.ParentProcessId
    }
    return ($parts -join ' <- ')
}
function Test-ChainHasHimind {
    param([int]$ProcessId)
    $current = $ProcessId
    for ($depth = 0; $depth -lt 8 -and $byPid.ContainsKey($current); $depth++) {
        $entry = $byPid[$current]
        if ($entry.Name -like 'himind-*') { return $true }
        $current = [int]$entry.ParentProcessId
    }
    return $false
}

$spawnedByAgent = @($processes | Where-Object { (Test-ChainHasHimind -ProcessId ([int]$_.ProcessId)) -and $_.Name -notlike 'himind-*' } | Sort-Object AtMs)

Write-Host ''
Write-Host ("recorded {0:HH:mm:ss.fff} - {1:HH:mm:ss.fff}; {2} windows appeared, {3} processes started" -f $startedAt, (Get-Date), $windows.Count, $processes.Count)

Write-Host ''
if ($consoleWindows.Count -gt 0) {
    Write-Host ("console windows that appeared and were shown: {0}" -f $consoleWindows.Count) -ForegroundColor Yellow
    $consoleWindows | Format-Table -AutoSize @{Name = 'At(ms)'; Expression = { [int]$_.AtMs } }, `
        @{Name = 'Lived(ms)'; Expression = { [int]$_.LifetimeMs } }, ClassName, ProcessName, Title
}
else {
    Write-Host 'console windows that appeared and were shown: 0' -ForegroundColor Green
}
if ($hiddenConsoleWindows.Count -gt 0) {
    Write-Host ("console windows created without a window (CREATE_NO_WINDOW): {0}" -f $hiddenConsoleWindows.Count)
    $hiddenConsoleWindows | Group-Object ProcessName | Sort-Object Count -Descending | ForEach-Object {
        Write-Host ("  {0,3} x {1}" -f $_.Count, $_.Name)
    }
}

Write-Host ''
if ($agentWindows.Count -gt 0) {
    Write-Host 'windows created by HiMind processes during this start:'
    $agentWindows | Format-Table -AutoSize @{Name = 'At(ms)'; Expression = { [int]$_.AtMs } }, `
        @{Name = 'Lived(ms)'; Expression = { [int]$_.LifetimeMs } }, ClassName, ProcessName, `
        @{Name = 'Shown'; Expression = { $_.EverVisible } }, Title
}
else {
    Write-Host 'no window owned by a HiMind process appeared during the recording' -ForegroundColor Yellow
}

Write-Host ''
if ($spawnedByAgent.Count -gt 0) {
    Write-Host 'processes started under a HiMind process (this is where a flash comes from):'
    $spawnedByAgent | Format-Table -AutoSize @{Name = 'At(ms)'; Expression = { [int]$_.AtMs } }, `
        @{Name = 'Lived(ms)'; Expression = { [int]($_.LastMs - $_.AtMs) } }, Name, `
        @{Name = 'Chain'; Expression = { Get-ProcessChain -ProcessId ([int]$_.ProcessId) } }
}
else {
    Write-Host 'no process was started under a HiMind process during the recording'
}

Write-Host ''
if ($consoleWindows.Count -eq 0) {
    Write-Host 'RESULT: no console window was shown - this start is quiet.' -ForegroundColor Green
}
else {
    Write-Host ("RESULT: {0} console window(s) flashed during this start." -f $consoleWindows.Count) -ForegroundColor Yellow
    $consoleWindows | Group-Object Title | Sort-Object Count -Descending | ForEach-Object {
        Write-Host ("  {0,3} x {1}" -f $_.Count, $(if ($_.Name) { $_.Name } else { '<untitled>' }))
    }
    Write-Host 'A title naming a program path (icacls.exe, cmd.exe, node.exe) is the program the Agent started; fix that spawn site to use runtime::process::hidden_command.'
}
