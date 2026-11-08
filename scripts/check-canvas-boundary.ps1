param(
  [Parameter(Mandatory = $false)]
  [string]$ExtensionWorkspace = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$issues = New-Object System.Collections.Generic.List[string]
function Add-Issue([string]$message) { $issues.Add($message) }

$contracts = Join-Path $root "contracts\agent-core"
if (-not (Test-Path -LiteralPath $contracts)) {
  Add-Issue "missing Agent Core contracts directory: $contracts"
} else {
  $domainPattern = '(?i)("|\b)(canvas|storyboard|timeline|shot|script_beat|asset_anchor)("|\b)|media\.workbench\.'
  $matches = Get-ChildItem -LiteralPath $contracts -Recurse -File -Include *.json,*.md |
    Select-String -Pattern $domainPattern
  foreach ($match in $matches) {
    Add-Issue "Agent Core contract contains application domain term: $($match.Path):$($match.LineNumber)"
  }
}

if ($ExtensionWorkspace.Trim()) {
  $catalogPath = Join-Path $ExtensionWorkspace "extensions.json"
  if (-not (Test-Path -LiteralPath $catalogPath)) {
    Add-Issue "missing extensions.json: $catalogPath"
  } else {
    $catalog = Get-Content -Raw -LiteralPath $catalogPath | ConvertFrom-Json
    if ([int]$catalog.schema_version -lt 2) {
      Add-Issue "extensions.json must use schema_version 2 for boundary audit"
    }
    foreach ($item in @($catalog.extensions)) {
      $consumerCount = @($item.consumer_ids | Where-Object { $_ -and $_.Trim() }).Count
      if (-not $item.ownership -or -not $item.reuse_scope -or -not $item.lifecycle -or $null -eq $item.disable_safe -or -not $item.owner -or $consumerCount -eq 0) {
        Add-Issue "extension metadata incomplete: $($item.id)"
      }
      if ($item.ownership -eq "extension") {
        if ($consumerCount -lt 2) { Add-Issue "extension has fewer than 2 consumers: $($item.id)" }
        if ($item.reuse_scope -ne "ecosystem") { Add-Issue "extension reuse_scope must be ecosystem: $($item.id)" }
        if ($item.lifecycle -ne "independently_versioned") { Add-Issue "extension lifecycle must be independently_versioned: $($item.id)" }
        if ($item.disable_safe -ne $true) { Add-Issue "extension must be disable_safe: $($item.id)" }
      }
    }

    $pluginManifests = Get-ChildItem -LiteralPath (Join-Path $ExtensionWorkspace "plugins") -Recurse -Filter plugin.json -ErrorAction SilentlyContinue
    $nodeKinds = @('domain_ref', 'artifact', 'operation', 'generator', 'workflow', 'annotation')
    foreach ($manifest in $pluginManifests) {
      $manifestJson = Get-Content -Raw -LiteralPath $manifest.FullName | ConvertFrom-Json
      $topLevelCanvasKeys = @($manifestJson.PSObject.Properties.Name | Where-Object { $_ -match '^canvas' })
      foreach ($key in $topLevelCanvasKeys) {
        Add-Issue "canvas contribution must live under contributes.canvas, not top-level ${key}: $($manifest.FullName)"
      }
      $contributionNames = @()
      if ($manifestJson.contributes) { $contributionNames = @($manifestJson.contributes.PSObject.Properties.Name) }
      foreach ($flatKey in @($contributionNames | Where-Object { $_ -match '^canvas\.' })) {
        Add-Issue "canvas contribution must use contributes.canvas object, not flat ${flatKey}: $($manifest.FullName)"
      }
      if ($contributionNames -notcontains 'canvas') { continue }
      $canvas = $manifestJson.contributes.canvas
      $namespace = [string]$manifestJson.capability_namespace
      if (-not $namespace.Trim()) {
        Add-Issue "plugin with canvas contributions must declare capability_namespace: $($manifest.FullName)"
        continue
      }
      $allowedKeys = @('nodeDefinitions', 'tools', 'renderer', 'assistantTools')
      foreach ($key in @($canvas.PSObject.Properties.Name | Where-Object { $allowedKeys -notcontains $_ })) {
        Add-Issue "unsupported canvas contribution key ${key}: $($manifest.FullName)"
      }
      foreach ($node in @($canvas.nodeDefinitions)) {
        if (-not $node) { continue }
        $id = [string]$node.id
        $kind = [string]$node.kind
        if (-not $id.StartsWith("$namespace.", [System.StringComparison]::Ordinal)) {
          Add-Issue "canvas node id must use plugin namespace ${namespace}: $id"
        }
        if ($nodeKinds -notcontains $kind) {
          Add-Issue "canvas node $id has unsupported kind: $kind"
        }
        if ($kind -in @('generator', 'operation', 'workflow') -and -not ([string]$node.capability_id).Trim()) {
          Add-Issue "canvas node $id requires capability_id for kind $kind"
        }
        $rendererEntry = [string]$node.renderer.entry
        if ($rendererEntry -and ($rendererEntry -match '^[A-Za-z]:' -or $rendererEntry -match '^[\\/]' -or $rendererEntry -match '(^|[\\/])\.\.([\\/]|$)')) {
          Add-Issue "canvas node renderer entry must stay inside plugin package: $id -> $rendererEntry"
        }
      }
      foreach ($collectionName in @('tools', 'assistantTools')) {
        foreach ($tool in @($canvas.$collectionName)) {
          if (-not $tool) { continue }
          $id = [string]$tool.id
          if (-not $id.StartsWith("$namespace.", [System.StringComparison]::Ordinal)) {
            Add-Issue "canvas $collectionName id must use plugin namespace ${namespace}: $id"
          }
          if (-not ([string]$tool.capability_id).Trim()) {
            Add-Issue "canvas $collectionName $id requires capability_id"
          }
        }
      }
    }
  }
}

if ($issues.Count -gt 0) {
  $issues | ForEach-Object { Write-Error $_ -ErrorAction Continue }
  exit 1
}

Write-Output "canvas boundary check passed"

