# 把官方扩展签名公钥补进本机 Agent 的受信公钥目录。
#
# 为什么需要它：0.3.71 及更早的本机自建安装包没有把官方扩展公钥写进
# `trusted-keys`，而公开扩展目录里有条目正是用这把密钥签名的。缺了它，整条
# 官方扩展源会显示为「不可用」，用户只看得到「未找到受信的签名公钥」。
# 安装器从本版本起会自带该公钥；这个脚本用于修复已经装好的实例。
[CmdletBinding()]
param(
    [string]$InstallRoot,
    [string]$PublicKeyPath,
    [string]$KeyId,
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"

if (-not $InstallRoot) { $InstallRoot = Join-Path $env:LOCALAPPDATA "HiMindAgent" }
$InstallRoot = [IO.Path]::GetFullPath($InstallRoot)
$trustedKeys = Join-Path $InstallRoot "trusted-keys"

if (-not (Test-Path -LiteralPath $InstallRoot -PathType Container)) {
    throw "Agent 安装目录不存在：$InstallRoot（用 -InstallRoot 指定实际安装根目录）"
}

if (-not $PublicKeyPath) {
    $candidates = @()
    foreach ($directory in @(
            (Join-Path $env:LOCALAPPDATA "HiMind\signing\trusted"),
            (Join-Path $env:ProgramData "HiMind\signing\trusted"),
            (Join-Path $PSScriptRoot "..\..\..\secrets\signing\trusted"))) {
        if ([string]::IsNullOrWhiteSpace($directory)) { continue }
        if (-not (Test-Path -LiteralPath $directory -PathType Container)) { continue }
        $candidates += @(Get-ChildItem -LiteralPath $directory -Filter "himind-extension-*.pem" -File -ErrorAction SilentlyContinue)
    }
    $candidates = @($candidates | Sort-Object -Property Name -Descending)
    if ($candidates.Count -eq 0) {
        throw "找不到官方扩展公钥。用 -PublicKeyPath 指定，或从 GitHub Release 资产里取 himind-extension-production-2026.pem。"
    }
    $PublicKeyPath = $candidates[0].FullName
}
$PublicKeyPath = [IO.Path]::GetFullPath($PublicKeyPath)
if (-not (Test-Path -LiteralPath $PublicKeyPath -PathType Leaf)) {
    throw "公钥文件不存在：$PublicKeyPath"
}
if (-not $KeyId) { $KeyId = [IO.Path]::GetFileNameWithoutExtension($PublicKeyPath) }
if ($KeyId -notmatch '^[A-Za-z0-9._-]+$') { throw "key ID 非法：$KeyId" }

$pem = [IO.File]::ReadAllText($PublicKeyPath)
if ($pem -notmatch 'BEGIN PUBLIC KEY' -and $pem -notmatch 'BEGIN RSA PUBLIC KEY') {
    throw "不是 PEM 公钥：$PublicKeyPath"
}

$before = if (Test-Path -LiteralPath $trustedKeys -PathType Container) {
    @(Get-ChildItem -LiteralPath $trustedKeys -Filter *.pem -File | Select-Object -ExpandProperty Name)
} else { @() }

if ($DryRun) {
    [pscustomobject]@{
        install_root = $InstallRoot
        trusted_keys = $trustedKeys
        key_id = $KeyId
        source = $PublicKeyPath
        target = (Join-Path $trustedKeys "$KeyId.pem")
        existing = $before
        applied = $false
    } | ConvertTo-Json
    return
}

New-Item -ItemType Directory -Force -Path $trustedKeys | Out-Null
$target = Join-Path $trustedKeys "$KeyId.pem"
Copy-Item -LiteralPath $PublicKeyPath -Destination $target -Force

[pscustomobject]@{
    install_root = $InstallRoot
    trusted_keys = $trustedKeys
    key_id = $KeyId
    source = $PublicKeyPath
    target = $target
    existing = $before
    trusted_keys_after = @(Get-ChildItem -LiteralPath $trustedKeys -Filter *.pem -File | Select-Object -ExpandProperty Name)
    next_step = "在 Agent 的扩展源面板刷新来源；命令行等价操作：himind-agent extension source refresh"
    applied = $true
} | ConvertTo-Json
