param([int]$AgentProcessId, [string]$OutputPath)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class ReviewWindowCapture {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
}
'@
$agentProcess = Get-Process -Id $AgentProcessId
$acceptance = Invoke-RestMethod 'http://127.0.0.1:18082/health' -TimeoutSec 5
if ($acceptance.profile -ne 'development' -or $agentProcess.Path -ne $acceptance.executable_path -or
    $agentProcess.Path -notmatch '\\Temp\\himind-agent-test-release-[A-F0-9]+\\himind-agent.exe$') {
  throw 'Only capture the temporary acceptance Agent.'
}
$handle = $agentProcess.MainWindowHandle
if ($handle -eq [IntPtr]::Zero) { throw 'No native main window.' }
$bounds = [ReviewWindowCapture+RECT]::new()
if (-not [ReviewWindowCapture]::GetWindowRect($handle, [ref]$bounds)) { throw 'Cannot read window bounds.' }
$bitmap = [Drawing.Bitmap]::new($bounds.Right-$bounds.Left, $bounds.Bottom-$bounds.Top)
$graphics = [Drawing.Graphics]::FromImage($bitmap)
$dc = $graphics.GetHdc()
try {
  if (-not [ReviewWindowCapture]::PrintWindow($handle, $dc, 2)) { throw 'Native window capture failed.' }
}
finally { $graphics.ReleaseHdc($dc); $graphics.Dispose() }
try { $bitmap.Save($OutputPath, [Drawing.Imaging.ImageFormat]::Png) }
finally { $bitmap.Dispose() }
[pscustomobject]@{
  process_id=$agentProcess.Id; executable=$agentProcess.Path; title=$agentProcess.MainWindowTitle
  hwnd=$handle.ToInt64(); visible=[ReviewWindowCapture]::IsWindowVisible($handle)
  width=$bounds.Right-$bounds.Left; height=$bounds.Bottom-$bounds.Top; screenshot=$OutputPath
} | ConvertTo-Json
