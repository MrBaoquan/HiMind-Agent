#Requires -Version 5.1
<#
.SYNOPSIS
Inspect the current Windows token's Runtime workspace permissions; repair only
the missing WRITE_OWNER right on an explicitly selected, caller-owned directory.
.DESCRIPTION
Inspection is the default. -Repair requires -ReportDirectory and backs up the
DACL before adding a non-inheritable TakeOwnership right (WRITE_OWNER only;
never FullControl). No elevation is used.
-RestoreBackup restores only if the DACL still matches the recorded post-edit
state, so later sandbox grants or concurrent edits cannot be overwritten.
#>
[CmdletBinding(DefaultParameterSetName = 'Inspect')]
param(
    [Parameter(Mandatory = $true)]
    [string]$Workspace,
    [Parameter(Mandatory = $true, ParameterSetName = 'Repair')]
    [switch]$Repair,
    [Parameter(Mandatory = $true, ParameterSetName = 'Repair')]
    [string]$ReportDirectory,
    [Parameter(Mandatory = $true, ParameterSetName = 'Restore')]
    [string]$RestoreBackup
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
if ($env:OS -ne 'Windows_NT') { throw 'This diagnostic requires Windows.' }

if (-not ('HiMind.WorkspacePermissions' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
namespace HiMind {
    public static class WorkspacePermissions {
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
        static extern IntPtr CreateFileW(string path, uint access, uint share, IntPtr security, uint creation, uint flags, IntPtr template);
        [DllImport("kernel32.dll", SetLastError=true)] static extern bool CloseHandle(IntPtr handle);
        [DllImport("advapi32.dll", CharSet=CharSet.Unicode)]
        static extern uint GetNamedSecurityInfoW(string path, uint type, uint info, out IntPtr owner, out IntPtr group, out IntPtr dacl, out IntPtr sacl, out IntPtr descriptor);
        [DllImport("advapi32.dll", CharSet=CharSet.Unicode)]
        static extern uint SetNamedSecurityInfoW(string path, uint type, uint info, IntPtr owner, IntPtr group, IntPtr dacl, IntPtr sacl);
        [DllImport("advapi32.dll", SetLastError=true)]
        static extern bool GetSecurityDescriptorDacl(IntPtr descriptor, out bool present, out IntPtr dacl, out bool defaulted);
        [DllImport("advapi32.dll")] static extern uint GetSecurityDescriptorLength(IntPtr descriptor);
        [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr memory);
        public static int Check(string path, uint access) {
            IntPtr handle = CreateFileW(path, access, 7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero);
            if (handle == new IntPtr(-1)) return Marshal.GetLastWin32Error();
            if (!CloseHandle(handle)) throw new Win32Exception(Marshal.GetLastWin32Error());
            return 0;
        }
        public static DirectorySecurity Read(string path) {
            IntPtr owner, group, dacl, sacl, descriptor;
            uint result = GetNamedSecurityInfoW(path, 1, 5, out owner, out group, out dacl, out sacl, out descriptor);
            if (result != 0) throw new Win32Exception((int)result);
            try {
                byte[] bytes = new byte[GetSecurityDescriptorLength(descriptor)];
                Marshal.Copy(descriptor, bytes, 0, bytes.Length);
                var security = new DirectorySecurity();
                security.SetSecurityDescriptorBinaryForm(bytes);
                return security;
            } finally { LocalFree(descriptor); }
        }
        public static void WriteDacl(string path, DirectorySecurity security) {
            byte[] bytes = security.GetSecurityDescriptorBinaryForm();
            GCHandle pinned = GCHandle.Alloc(bytes, GCHandleType.Pinned);
            try {
                bool present, defaulted; IntPtr dacl;
                if (!GetSecurityDescriptorDacl(pinned.AddrOfPinnedObject(), out present, out dacl, out defaulted))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                if (!present || dacl == IntPtr.Zero) throw new InvalidOperationException("Refusing an absent/null DACL.");
                // DACL only: keep owner, group, mandatory label and audit SACL.
                uint result = SetNamedSecurityInfoW(path, 1, 4, IntPtr.Zero, IntPtr.Zero, dacl, IntPtr.Zero);
                if (result != 0) throw new Win32Exception((int)result);
            } finally { pinned.Free(); }
        }
    }
}
'@
}

function Get-DaclText($Security) {
    $Security.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access)
}

function Get-Observation([string]$Path) {
    $security = [HiMind.WorkspacePermissions]::Read($Path)
    $writeDac = [HiMind.WorkspacePermissions]::Check($Path, 0x40000)
    $writeOwner = [HiMind.WorkspacePermissions]::Check($Path, 0x80000)
    [ordered]@{
        workspace = $Path
        current_user_sid = $callerSid.Value
        owner_sid = $security.GetOwner([Security.Principal.SecurityIdentifier]).Value
        write_dac = ($writeDac -eq 0)
        write_dac_win32 = $writeDac
        write_owner = ($writeOwner -eq 0)
        write_owner_win32 = $writeOwner
        dacl = Get-DaclText $security
    }
}

if (-not [IO.Path]::IsPathRooted($Workspace)) { throw 'Workspace must be an absolute directory path.' }
$workspacePath = (Get-Item -LiteralPath $Workspace -Force).FullName
if (-not (Test-Path -LiteralPath $workspacePath -PathType Container)) { throw 'Workspace must be an existing directory.' }
$callerSid = [Security.Principal.WindowsIdentity]::GetCurrent().User
$before = Get-Observation $workspacePath
if ($PSCmdlet.ParameterSetName -eq 'Inspect') {
    [ordered]@{ mode = 'inspect'; observation = $before; permission_prerequisites_met = ($before.write_dac -and $before.write_owner); shell_verified = $false } | ConvertTo-Json -Depth 6
    return
}

# Mutations are confined to the selected local directory; no ancestor repair,
# inherited grant, reparse traversal, drive root, or Agent-managed directory.
if ($workspacePath.StartsWith('\\') -or $workspacePath.TrimEnd('\') -eq [IO.Path]::GetPathRoot($workspacePath).TrimEnd('\')) {
    throw 'Select a local workspace directory, not a volume/share root.'
}
$cursor = Get-Item -LiteralPath $workspacePath -Force
while ($null -ne $cursor) {
    if (($cursor.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "Reparse paths cannot be repaired: $($cursor.FullName)" }
    $cursor = $cursor.Parent
}
$managedRoots = @(
    (Join-Path $env:LOCALAPPDATA 'HiMindAgent'),
    (Join-Path $env:LOCALAPPDATA 'com.himind.agent')
)
if ($env:HIMIND_AGENT_HOME) { $managedRoots += [IO.Path]::GetFullPath($env:HIMIND_AGENT_HOME) }
foreach ($managed in $managedRoots) {
    $managed = $managed.TrimEnd('\')
    if ($workspacePath.Equals($managed, [StringComparison]::OrdinalIgnoreCase) -or $workspacePath.StartsWith($managed + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw "Agent-managed paths cannot be repaired as workspaces: $workspacePath"
    }
}
if ($before.owner_sid -ne $callerSid.Value -or -not $before.write_dac) {
    throw 'The current user must own this directory and have permission to edit its access rules. Choose another workspace or ask its owner to authorize it.'
}

if ($PSCmdlet.ParameterSetName -eq 'Restore') {
    $backup = Get-Content -LiteralPath $RestoreBackup -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($backup.schema -ne 'himind.workspace_permission_backup.v1' -or $backup.workspace -ne $workspacePath -or $backup.current_user_sid -ne $callerSid.Value) {
        throw 'Backup does not match this workspace and user.'
    }
    if ($backup.after_dacl -ne $before.dacl) { throw 'Permissions have changed since repair; refusing to overwrite later edits or sandbox grants.' }
    $security = [HiMind.WorkspacePermissions]::Read($workspacePath)
    $security.SetSecurityDescriptorSddlForm($backup.before_dacl, [Security.AccessControl.AccessControlSections]::Access)
    [HiMind.WorkspacePermissions]::WriteDacl($workspacePath, $security)
    $after = Get-Observation $workspacePath
    if ($after.dacl -ne $backup.before_dacl) { throw 'Backup restoration did not match the original DACL.' }
    [ordered]@{ mode = 'restore'; observation = $after; restored = $true } | ConvertTo-Json -Depth 6
    return
}

if ($before.write_owner) {
    [ordered]@{ mode = 'repair'; observation = $before; changed = $false; shell_verified = $false } | ConvertTo-Json -Depth 6
    return
}
if ($before.write_owner_win32 -ne 5) { throw "WRITE_OWNER probe failed with Win32 $($before.write_owner_win32); no repair attempted." }
if (-not [IO.Path]::IsPathRooted($ReportDirectory)) { throw 'ReportDirectory must be absolute.' }
New-Item -ItemType Directory -Path $ReportDirectory -Force | Out-Null
$backupPath = Join-Path $ReportDirectory ('workspace-permissions-' + [Guid]::NewGuid().ToString('N') + '.json')
$record = [ordered]@{
    schema = 'himind.workspace_permission_backup.v1'
    workspace = $workspacePath
    current_user_sid = $callerSid.Value
    created_at = [DateTime]::UtcNow.ToString('o')
    before_dacl = $before.dacl
    after_dacl = $null
    grant = 'WRITE_OWNER (0x80000), current user, this directory only'
}
$security = [HiMind.WorkspacePermissions]::Read($workspacePath)
if ((Get-DaclText $security) -ne $before.dacl) { throw 'Permissions changed during inspection; no repair attempted.' }
if (-not $security.AreAccessRulesCanonical) { throw 'Non-canonical DACL requires manual diagnosis; no repair attempted.' }
# FileSystemAccessRule implicitly adds SYNCHRONIZE to Allow entries. Use an
# exact raw ACE so this repair adds only the demonstrated missing access bit.
$raw = [Security.AccessControl.RawSecurityDescriptor]::new($security.GetSecurityDescriptorBinaryForm(), 0)
$insertAt = 0
while ($insertAt -lt $raw.DiscretionaryAcl.Count -and -not $raw.DiscretionaryAcl[$insertAt].IsInherited) { $insertAt++ }
$ace = [Security.AccessControl.CommonAce]::new([Security.AccessControl.AceFlags]::None, [Security.AccessControl.AceQualifier]::AccessAllowed, 0x80000, $callerSid, $false, $null)
$raw.DiscretionaryAcl.InsertAce($insertAt, $ace)
$bytes = [byte[]]::new($raw.BinaryLength)
$raw.GetBinaryForm($bytes, 0)
$security.SetSecurityDescriptorBinaryForm($bytes, [Security.AccessControl.AccessControlSections]::Access)
$record.after_dacl = Get-DaclText $security
$record | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $backupPath -Encoding UTF8
try {
    [HiMind.WorkspacePermissions]::WriteDacl($workspacePath, $security)
    $after = Get-Observation $workspacePath
    if (-not $after.write_dac -or -not $after.write_owner) { throw 'The minimal grant did not satisfy effective access checks.' }
    if ($after.dacl -ne $record.after_dacl) { throw 'Post-repair DACL differs from the planned DACL.' }
} catch {
    $repairError = $_
    $current = [HiMind.WorkspacePermissions]::Read($workspacePath)
    if ((Get-DaclText $current) -eq $record.after_dacl) {
        $current.SetSecurityDescriptorSddlForm($record.before_dacl, [Security.AccessControl.AccessControlSections]::Access)
        [HiMind.WorkspacePermissions]::WriteDacl($workspacePath, $current)
        if ((Get-DaclText ([HiMind.WorkspacePermissions]::Read($workspacePath))) -ne $record.before_dacl) { throw "Rollback failed; inspect backup: $backupPath" }
    } elseif ((Get-DaclText $current) -ne $record.before_dacl) {
        throw "Repair failed and concurrent permissions prevent safe rollback. Inspect backup: $backupPath. $repairError"
    }
    throw $repairError
}
[ordered]@{ mode = 'repair'; before = $before; after = $after; changed = $true; backup = $backupPath; shell_verified = $false } | ConvertTo-Json -Depth 6
