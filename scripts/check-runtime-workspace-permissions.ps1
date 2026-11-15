#Requires -Version 5.1
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$tool = Join-Path $PSScriptRoot 'runtime-workspace-permissions.ps1'
$repoRoot = Split-Path -Parent $PSScriptRoot
$fixtureRoot = Join-Path $repoRoot ('target/workspace-permissions-check-' + [Guid]::NewGuid().ToString('N'))
$workspace = Join-Path $fixtureRoot 'workspace'
$reports = Join-Path $fixtureRoot 'reports'
New-Item -ItemType Directory -Path $workspace -Force | Out-Null
function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Expect-Refusal([scriptblock]$Action, [string]$Text) {
    $caught = $false
    try { & $Action | Out-Null } catch {
        $caught = $true
        Assert-True ($_.ToString().Contains($Text)) "Unexpected refusal: $_"
    }
    Assert-True $caught "Expected refusal containing: $Text"
}

# Initialize the real native API without changing the fixture, then construct
# a caller-owned Modify + WRITE_DAC fixture without WRITE_OWNER, regardless of
# what access the developer happens to inherit on their machine.
$null = & $tool -Workspace $workspace | ConvertFrom-Json
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
$security = [HiMind.WorkspacePermissions]::Read($workspace)
$owner = $security.GetOwner([Security.Principal.SecurityIdentifier])
Assert-True ($owner.Value -eq $sid.Value) 'Fixture must be owned by the test user'
$security.SetAccessRuleProtection($true, $false)
foreach ($rule in @($security.GetAccessRules($true, $false, [Security.Principal.SecurityIdentifier]))) {
    $security.RemoveAccessRuleSpecific($rule)
}
$security.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, [Security.AccessControl.FileSystemRights]'Modify, ChangePermissions', [Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit', [Security.AccessControl.PropagationFlags]::None, [Security.AccessControl.AccessControlType]::Allow))
[HiMind.WorkspacePermissions]::WriteDacl($workspace, $security)
$baseline = ([HiMind.WorkspacePermissions]::Read($workspace)).GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access)
$observed = & $tool -Workspace $workspace | ConvertFrom-Json
Assert-True (-not $observed.observation.write_owner -and $observed.observation.write_dac) 'Fixture must reproduce the missing WRITE_OWNER precondition'
Assert-True ($observed.observation.dacl -eq $baseline) 'Inspection changed the DACL'

$repair = & $tool -Workspace $workspace -Repair -ReportDirectory $reports | ConvertFrom-Json
Assert-True ($repair.changed -and $repair.after.write_owner -and $repair.after.write_dac) 'Minimal repair did not enable the missing right'
Assert-True ($repair.before.owner_sid -eq $repair.after.owner_sid) 'Repair changed owner'
$raw = [Security.AccessControl.RawSecurityDescriptor]::new($repair.after.dacl)
$grants = @($raw.DiscretionaryAcl | Where-Object { $_.AccessMask -eq 0x80000 -and $_.AceFlags -eq [Security.AccessControl.AceFlags]::None -and $_.SecurityIdentifier.Value -eq $sid.Value })
Assert-True ($grants.Count -eq 1) 'Repair must add precisely WRITE_OWNER, without inheritance or extra rights'
$again = & $tool -Workspace $workspace -Repair -ReportDirectory $reports | ConvertFrom-Json
Assert-True (-not $again.changed) 'Repair is not idempotent'
$restore = & $tool -Workspace $workspace -RestoreBackup $repair.backup | ConvertFrom-Json
Assert-True ($restore.restored -and $restore.observation.dacl -eq $baseline) 'Restoration did not preserve the original DACL'

$repair = & $tool -Workspace $workspace -Repair -ReportDirectory $reports | ConvertFrom-Json
$security = [HiMind.WorkspacePermissions]::Read($workspace)
$security.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-5-32-545'), [Security.AccessControl.FileSystemRights]::ReadAndExecute, [Security.AccessControl.AccessControlType]::Allow))
[HiMind.WorkspacePermissions]::WriteDacl($workspace, $security)
$changedDacl = ([HiMind.WorkspacePermissions]::Read($workspace)).GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access)
Expect-Refusal { & $tool -Workspace $workspace -RestoreBackup $repair.backup } 'Permissions have changed'
Assert-True (([HiMind.WorkspacePermissions]::Read($workspace)).GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access) -eq $changedDacl) 'Stale backup overwrote a subsequent change'

# A real explicit deny must survive a failed repair and its automatic rollback.
$security.SetSecurityDescriptorSddlForm($baseline, [Security.AccessControl.AccessControlSections]::Access)
$security.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, [Security.AccessControl.FileSystemRights]::TakeOwnership, [Security.AccessControl.AccessControlType]::Deny))
[HiMind.WorkspacePermissions]::WriteDacl($workspace, $security)
$deniedDacl = ([HiMind.WorkspacePermissions]::Read($workspace)).GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access)
Expect-Refusal { & $tool -Workspace $workspace -Repair -ReportDirectory $reports } 'minimal grant did not satisfy'
Assert-True (([HiMind.WorkspacePermissions]::Read($workspace)).GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access) -eq $deniedDacl) 'Failed repair did not roll back while preserving the deny'
Expect-Refusal { & $tool -Workspace $workspace -RestoreBackup $repair.backup } 'Permissions have changed'
Expect-Refusal { & $tool -Workspace ([IO.Path]::GetPathRoot($workspace)) -Repair -ReportDirectory $reports } 'not a volume/share root'
Expect-Refusal { & $tool -Workspace (Join-Path $env:LOCALAPPDATA 'HiMindAgent') -Repair -ReportDirectory $reports } 'Agent-managed paths'

$junction = Join-Path $fixtureRoot 'junction'
New-Item -ItemType Junction -Path $junction -Target $workspace | Out-Null
Expect-Refusal { & $tool -Workspace $junction -Repair -ReportDirectory $reports } 'Reparse paths'
Expect-Refusal { & $tool -Workspace (Join-Path 'target' 'relative') } 'absolute directory path'

[ordered]@{
    result = 'passed'
    cases = @('read-only inspection', 'missing-right reproduction', 'exact non-inheritable grant', 'owner preservation', 'idempotence', 'backup restore', 'stale backup refusal', 'deny preservation and automatic rollback', 'volume root refusal', 'managed path refusal', 'junction refusal', 'relative path refusal')
    fixture = $fixtureRoot
} | ConvertTo-Json -Depth 4
