use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use reqwest::Url;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pss, RsaPublicKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::error::Error;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::app::types::RemoteConnectRequest;
use crate::store::credentials::{
    configured_unity_editor_path, discovered_unity_editor_path, unity_editor_environment_path,
};
use crate::Options;

const CREATE_NO_WINDOW: u32 = 0x08000000;
const AUTO_START_REG_PATH: &str = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Run";
const AUTO_START_VALUE: &str = "ProjectDashboardAgent";
const EMBEDDED_UPDATE_PUBLIC_KEY_PEM: &str =
    include_str!(concat!(env!("OUT_DIR"), "/embedded-update-public-key.pem"));
const EMBEDDED_UPDATE_KEY_ID: &str =
    include_str!(concat!(env!("OUT_DIR"), "/embedded-update-key-id.txt"));

pub(crate) fn validate_update_download_url(
    api_base: &str,
    download_url: &str,
) -> Result<(), Box<dyn Error>> {
    let api = Url::parse(api_base).map_err(|_| "Dashboard API 地址无效")?;
    let download = Url::parse(download_url).map_err(|_| "Agent 更新包下载地址无效")?;
    if !matches!(download.scheme(), "http" | "https") {
        return Err("Agent 更新包只允许使用 HTTP 或 HTTPS 下载".into());
    }
    if api.scheme() != download.scheme()
        || api.host_str() != download.host_str()
        || api.port_or_known_default() != download.port_or_known_default()
    {
        return Err("Agent 更新包下载地址必须与当前 Dashboard API 同源".into());
    }
    Ok(())
}

pub(crate) fn verify_agent_package_signature(
    staged_package: &Path,
    signature: &str,
    key_id: &str,
    algorithm: &str,
) -> Result<(), Box<dyn Error>> {
    let signature = signature.trim();
    let key_id = key_id.trim();
    let algorithm = algorithm.trim();
    let require_signed = signed_agent_updates_required();
    validate_signature_metadata(signature, key_id, algorithm, require_signed)?;
    if signature.is_empty() && key_id.is_empty() && algorithm.is_empty() {
        return Ok(());
    }
    let public_key = trusted_agent_update_public_key(key_id)?;
    verify_rsa_pss_sha256(staged_package, &public_key, signature)
}

pub(crate) fn verify_runtime_component_signature(
    staged_package: &Path,
    signature: &str,
    key_id: &str,
    algorithm: &str,
) -> Result<(), Box<dyn Error>> {
    validate_signature_metadata(signature.trim(), key_id.trim(), algorithm.trim(), true)?;
    let public_key = trusted_agent_update_public_key(key_id.trim())?;
    verify_rsa_pss_sha256(staged_package, &public_key, signature.trim())
}

pub(crate) fn signed_agent_updates_required() -> bool {
    env::var("HIMIND_REQUIRE_SIGNED_UPDATES")
        .map(|value| value.eq_ignore_ascii_case("true") || value == "1")
        .unwrap_or_else(|_| !EMBEDDED_UPDATE_PUBLIC_KEY_PEM.trim().is_empty())
}

pub(crate) fn trusted_agent_update_key_ids() -> Vec<String> {
    let embedded = EMBEDDED_UPDATE_KEY_ID.trim();
    if embedded.is_empty() {
        Vec::new()
    } else {
        vec![embedded.to_string()]
    }
}

fn trusted_agent_update_public_key(key_id: &str) -> Result<String, Box<dyn Error>> {
    if let Some(trusted_dir) = env::var_os("HIMIND_TRUSTED_SIGNING_KEYS_DIR") {
        let public_key_path = PathBuf::from(trusted_dir).join(format!("{key_id}.pem"));
        if public_key_path.is_file() {
            return Ok(std::fs::read_to_string(public_key_path)?);
        }
    }
    if key_id == EMBEDDED_UPDATE_KEY_ID.trim() && !EMBEDDED_UPDATE_PUBLIC_KEY_PEM.trim().is_empty()
    {
        return Ok(EMBEDDED_UPDATE_PUBLIC_KEY_PEM.to_string());
    }
    Err(format!("未找到受信的 Agent 更新公钥：{key_id}").into())
}

pub(crate) fn verify_rsa_pss_sha256(
    artifact_path: &Path,
    public_key_pem: &str,
    signature_base64: &str,
) -> Result<(), Box<dyn Error>> {
    let public_key = RsaPublicKey::from_public_key_pem(public_key_pem)?;
    let signature_bytes = BASE64_STANDARD.decode(signature_base64)?;
    let digest = Sha256::digest(std::fs::read(artifact_path)?);
    public_key
        .verify(Pss::new::<Sha256>(), &digest, &signature_bytes)
        .map_err(|_| "Agent 更新包签名验证失败".into())
}

pub(crate) fn validate_signature_metadata(
    signature: &str,
    key_id: &str,
    algorithm: &str,
    require_signed: bool,
) -> Result<(), Box<dyn Error>> {
    if signature.is_empty() && key_id.is_empty() && algorithm.is_empty() {
        return if require_signed {
            Err("Agent 更新策略要求签名，但更新包未提供签名".into())
        } else {
            Ok(())
        };
    }
    if signature.is_empty() || key_id.is_empty() || algorithm != "rsa-pss-sha256" {
        return Err("Agent 更新签名元数据不完整或算法不受支持".into());
    }
    if key_id.len() > 64
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("Agent 更新签名 key ID 无效".into());
    }
    Ok(())
}

pub(crate) fn schedule_agent_replace_and_restart(
    staged_executable: &Path,
    staged_package: &Path,
    staged_updater: &Path,
    staged_launcher: &Path,
    current_executable: &Path,
    options: &Options,
    target_version: &str,
) -> Result<(), Box<dyn Error>> {
    let installation_root =
        crate::install_layout::installation_root_from_executable(current_executable);
    if !crate::install_layout::updater_path(&installation_root).is_file() {
        return Err("无法定位已安装的 Agent updater".into());
    }
    for (path, name) in [
        (staged_executable, "himind-agent.exe"),
        (staged_updater, "himind-agent-updater.exe"),
        (staged_launcher, "himind-agent-launcher.exe"),
    ] {
        if !path.is_file() || path.file_name().and_then(|value| value.to_str()) != Some(name) {
            return Err(format!("Agent directory update is missing {name}").into());
        }
    }
    if !staged_package.is_file()
        || staged_package.extension().and_then(|value| value.to_str()) != Some("zip")
    {
        return Err("Agent directory update package is unavailable".into());
    }
    let updater = crate::install_layout::updater_path(&installation_root);
    let arguments = agent_restart_arguments(options);
    let payload = serde_json::json!({
        "current_executable": current_executable,
        "staged_executable": staged_executable,
        "staged_package": staged_package,
        "staged_updater": staged_updater,
        "staged_launcher": staged_launcher,
        "api_base": options.api_base,
        "from_version": crate::VERSION,
        "target_version": target_version,
        "local_port": options.local_port,
        "state_path": options.state_path,
        // Old updaters terminate old_pid with taskkill /T, which also kills
        // their own child process. Keep it zero for backward compatibility;
        // new updaters use wait_pid and never terminate the process tree.
        "old_pid": 0,
        "wait_pid": std::process::id(),
        "arguments": arguments,
    });
    Command::new(updater)
        .arg(payload.to_string())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

fn agent_restart_arguments(options: &Options) -> Vec<String> {
    let mut arguments = vec![
        "--api".to_string(),
        options.api_base.clone(),
        "--local-app".to_string(),
        "--local-port".to_string(),
        options.local_port.to_string(),
    ];
    if !options.state_path.as_os_str().is_empty() {
        arguments.push("--state".to_string());
        arguments.push(options.state_path.to_string_lossy().to_string());
    }
    // Reopen the main window after a successful replacement. The protocol URL
    // is deliberately final so the Agent's normal protocol validation applies.
    arguments.push("--protocol-url".to_string());
    arguments.push("himind-agent://open".to_string());
    arguments
}

pub(crate) fn local_agent_executable_metadata() -> Value {
    match env::current_exe() {
        Ok(path) => json!({
            "name": path.file_name().map(|item| item.to_string_lossy().to_string()).unwrap_or_else(|| "himind-agent.exe".to_string()),
            "path": path.to_string_lossy().to_string(),
        }),
        Err(_) => json!({
            "name": "himind-agent.exe",
            "path": Value::Null,
        }),
    }
}

pub(crate) fn open_agent_install_directory() -> Result<(), Box<dyn Error>> {
    let executable = crate::install_layout::stable_launcher_for_executable(&env::current_exe()?);
    let folder = crate::install_layout::installation_root_from_executable(&executable);
    open_folder(&folder.to_string_lossy())
}

pub(crate) fn create_plugin_view_shortcut(
    plugin_id: &str,
    view_id: &str,
    title: &str,
) -> Result<(), Box<dyn Error>> {
    let executable = env::current_exe()?;
    let launcher = crate::install_layout::stable_launcher_for_executable(&executable);
    let installation_root = crate::install_layout::installation_root_from_executable(&executable);
    let desktop = env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|path| path.join("Desktop"))
        .filter(|path| path.is_dir())
        .ok_or_else(|| "Windows 桌面目录不可用")?;
    let shortcut_name = format!("{}.lnk", sanitize_shortcut_name(title));
    let shortcut = desktop.join(shortcut_name);
    let shortcut_path = shortcut.to_string_lossy().to_string();
    let target_path = launcher.to_string_lossy().to_string();
    let arguments = format!(
        "--local-app --plugin-id \"{}\" --view-id \"{}\"",
        plugin_id, view_id
    );
    let working_directory = installation_root.to_string_lossy().to_string();
    let description = format!("HiMind 插件: {title}");
    let script = r#"
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($env:HIMIND_SHORTCUT_PATH)
$shortcut.TargetPath = $env:HIMIND_SHORTCUT_TARGET
$shortcut.Arguments = $env:HIMIND_SHORTCUT_ARGUMENTS
$shortcut.WorkingDirectory = $env:HIMIND_SHORTCUT_WORKING_DIRECTORY
$shortcut.Description = $env:HIMIND_SHORTCUT_DESCRIPTION
$shortcut.Save()
"#;
    let output = run_hidden_powershell_with_env(
        script,
        &[
            ("HIMIND_SHORTCUT_PATH", &shortcut_path),
            ("HIMIND_SHORTCUT_TARGET", &target_path),
            ("HIMIND_SHORTCUT_ARGUMENTS", &arguments),
            ("HIMIND_SHORTCUT_WORKING_DIRECTORY", &working_directory),
            ("HIMIND_SHORTCUT_DESCRIPTION", &description),
        ],
    )?;
    if !output.status.success() {
        return Err(powershell_failure("创建插件桌面快捷方式失败", &output).into());
    }
    Ok(())
}

fn sanitize_shortcut_name(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            _ => character,
        })
        .collect();
    let sanitized = sanitized.trim().trim_end_matches('.');
    if sanitized.is_empty() {
        "HiMind 插件".to_string()
    } else {
        sanitized.to_string()
    }
}

pub(crate) fn is_agent_auto_start_enabled(
    dashboard_base: &str,
    local_port: u16,
    state_path: &Path,
) -> Result<bool, Box<dyn Error>> {
    let expected = build_auto_start_command(dashboard_base, local_port, state_path)?;
    let current = read_auto_start_command()?;
    Ok(current.as_deref() == Some(expected.as_str()))
}

pub(crate) fn set_agent_auto_start(
    enabled: bool,
    dashboard_base: &str,
    local_port: u16,
    state_path: &Path,
) -> Result<bool, Box<dyn Error>> {
    let launch_command = build_auto_start_command(dashboard_base, local_port, state_path)?;
    if enabled {
        write_auto_start_command(&launch_command)?;
    } else {
        remove_auto_start_command()?;
    }

    let current = read_auto_start_command()?;
    if enabled {
        if current.as_deref() != Some(launch_command.as_str()) {
            return Err("设置 Agent 开机自启失败：注册表回读结果与预期不一致".into());
        }
        return Ok(true);
    }

    if current.is_some() {
        return Err("关闭 Agent 开机自启失败：注册表项仍然存在".into());
    }

    Ok(false)
}

fn build_auto_start_command(
    dashboard_base: &str,
    local_port: u16,
    state_path: &Path,
) -> Result<String, Box<dyn Error>> {
    let executable = crate::install_layout::stable_launcher_for_executable(&env::current_exe()?);
    let mut parts = vec![
        quote_cmd_arg(&executable.to_string_lossy()),
        "--api".to_string(),
        quote_cmd_arg(dashboard_base),
        "--local-app".to_string(),
        "--local-port".to_string(),
        local_port.to_string(),
    ];
    if !state_path.as_os_str().is_empty() {
        let absolute_state = if state_path.is_absolute() {
            state_path.to_path_buf()
        } else {
            env::current_dir()?.join(state_path)
        };
        parts.push("--state".to_string());
        parts.push(quote_cmd_arg(&absolute_state.to_string_lossy()));
    }
    Ok(parts.join(" "))
}

fn quote_cmd_arg(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn read_auto_start_command() -> Result<Option<String>, Box<dyn Error>> {
    let output = run_hidden_powershell(
        r#"
$path = $env:HIMIND_AUTO_START_REG_PATH
$name = $env:HIMIND_AUTO_START_VALUE_NAME
$item = Get-ItemProperty -Path $path -Name $name -ErrorAction SilentlyContinue
if ($null -eq $item) {
    exit 2
}
$value = [string]$item.$name
if ([string]::IsNullOrWhiteSpace($value)) {
    exit 2
}
[Console]::Out.Write($value)
"#,
        &[],
    )?;

    match output.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        )),
        Some(2) => Ok(None),
        _ => Err(powershell_failure("读取 Agent 开机自启配置失败", &output).into()),
    }
}

fn write_auto_start_command(launch_command: &str) -> Result<(), Box<dyn Error>> {
    let output = run_hidden_powershell(
        r#"
$path = $env:HIMIND_AUTO_START_REG_PATH
$name = $env:HIMIND_AUTO_START_VALUE_NAME
$command = $env:HIMIND_AUTO_START_COMMAND
New-Item -Path $path -Force | Out-Null
New-ItemProperty -Path $path -Name $name -Value $command -PropertyType String -Force | Out-Null
"#,
        &[("HIMIND_AUTO_START_COMMAND", launch_command)],
    )?;

    if !output.status.success() {
        return Err(powershell_failure("设置 Agent 开机自启失败", &output).into());
    }

    Ok(())
}

fn remove_auto_start_command() -> Result<(), Box<dyn Error>> {
    let output = run_hidden_powershell(
        r#"
$path = $env:HIMIND_AUTO_START_REG_PATH
$name = $env:HIMIND_AUTO_START_VALUE_NAME
Remove-ItemProperty -Path $path -Name $name -ErrorAction SilentlyContinue
"#,
        &[],
    )?;

    if !output.status.success() {
        return Err(powershell_failure("关闭 Agent 开机自启失败", &output).into());
    }

    Ok(())
}

fn run_hidden_powershell(
    script: &str,
    extra_env: &[(&str, &str)],
) -> Result<std::process::Output, Box<dyn Error>> {
    let script = format!(
        "$OutputEncoding = [System.Text.UTF8Encoding]::new($false); [Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false); [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false);\n{}",
        script
    );
    let mut command = Command::new("powershell");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .env("HIMIND_AUTO_START_REG_PATH", AUTO_START_REG_PATH)
        .env("HIMIND_AUTO_START_VALUE_NAME", AUTO_START_VALUE);

    for (key, value) in extra_env {
        command.env(key, value);
    }

    Ok(command.output()?)
}

fn run_hidden_powershell_with_env(
    script: &str,
    extra_env: &[(&str, &str)],
) -> Result<std::process::Output, Box<dyn Error>> {
    let script = format!(
        "$OutputEncoding = [System.Text.UTF8Encoding]::new($false); [Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false); [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false);\n{}",
        script
    );
    let mut command = Command::new("powershell");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    Ok(command.output()?)
}

fn powershell_failure(prefix: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        format!("{prefix}: {stderr}")
    } else if !stdout.is_empty() {
        format!("{prefix}: {stdout}")
    } else {
        prefix.to_string()
    }
}

pub(crate) fn open_url(url: &str) -> Result<(), Box<dyn Error>> {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()?;
    Ok(())
}

pub(crate) fn capture_browser_page_text(source_url: &str) -> Result<Value, Box<dyn Error>> {
    let source_url = source_url.trim();
    if source_url.is_empty() {
        return Err("source_url is required".into());
    }

    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName Microsoft.VisualBasic

$url = $env:HIMIND_CAPTURE_URL
$candidates = @(
    @{ name = 'Edge'; path = Join-Path $env:ProgramFiles 'Microsoft\Edge\Application\msedge.exe' },
    @{ name = 'Edge'; path = Join-Path ${env:ProgramFiles(x86)} 'Microsoft\Edge\Application\msedge.exe' },
    @{ name = 'Chrome'; path = Join-Path $env:ProgramFiles 'Google\Chrome\Application\chrome.exe' },
    @{ name = 'Chrome'; path = Join-Path ${env:ProgramFiles(x86)} 'Google\Chrome\Application\chrome.exe' }
) | Where-Object { $_.path -and (Test-Path $_.path) }

if ($candidates.Count -eq 0) {
    throw 'BROWSER_NOT_FOUND'
}

$selected = $candidates[0]
$process = Start-Process -FilePath $selected.path -ArgumentList @('--new-window', $url) -PassThru
$deadline = (Get-Date).AddSeconds(18)
$target = $null

do {
    $running = Get-Process -Name ([System.IO.Path]::GetFileNameWithoutExtension($selected.path)) -ErrorAction SilentlyContinue |
        Where-Object { $_.MainWindowHandle -ne 0 } |
        Sort-Object StartTime -Descending
    if ($running) {
        $target = $running | Select-Object -First 1
        break
    }
    Start-Sleep -Milliseconds 400
} while ((Get-Date) -lt $deadline)

if ($null -eq $target) {
    throw 'BROWSER_WINDOW_TIMEOUT'
}

$previousClipboard = $null
$hasPreviousClipboard = $false
try {
    $previousClipboard = Get-Clipboard -Raw -Format Text -ErrorAction Stop
    $hasPreviousClipboard = $true
} catch {}

if (-not [Microsoft.VisualBasic.Interaction]::AppActivate($target.Id)) {
    $windowTitle = $target.MainWindowTitle
    if ([string]::IsNullOrWhiteSpace($windowTitle) -or -not [Microsoft.VisualBasic.Interaction]::AppActivate($windowTitle)) {
        throw 'BROWSER_WINDOW_ACTIVATE_FAILED'
    }
}

Start-Sleep -Milliseconds 3200
[System.Windows.Forms.SendKeys]::SendWait('{ESC}')
Start-Sleep -Milliseconds 150
[System.Windows.Forms.SendKeys]::SendWait('^a')
Start-Sleep -Milliseconds 180
[System.Windows.Forms.SendKeys]::SendWait('^a')
Start-Sleep -Milliseconds 180
[System.Windows.Forms.SendKeys]::SendWait('^c')
Start-Sleep -Milliseconds 900

$text = ''
try {
    $text = Get-Clipboard -Raw -Format Text -ErrorAction Stop
} catch {}

if ($hasPreviousClipboard) {
    try {
        Set-Clipboard -Value $previousClipboard
    } catch {}
}

if ([string]::IsNullOrWhiteSpace($text)) {
    throw 'BROWSER_COPY_EMPTY'
}

[Console]::Out.Write((@{
    ok = $true
    source_url = $url
    browser = $selected.name
    mode = 'browser_copy'
    text = $text
} | ConvertTo-Json -Depth 6 -Compress))
"#;

    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .env("HIMIND_CAPTURE_URL", source_url)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let message = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            "本机浏览器自动读取失败。".to_string()
        };
        return Err(message.into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err("本机浏览器未返回可解析文本。".into());
    }
    let value: Value = serde_json::from_str(&stdout)?;
    Ok(value)
}

pub(crate) fn open_folder(path: &str) -> Result<(), Box<dyn Error>> {
    let target = PathBuf::from(path);
    let folder = if target.is_file() {
        target.parent().map(Path::to_path_buf).unwrap_or(target)
    } else {
        target
    };
    std::process::Command::new("explorer.exe")
        .arg(folder)
        .spawn()?;
    Ok(())
}

pub(crate) fn inspect_project_workspace(
    path: &str,
    engine_type: Option<&str>,
    _engine_version: Option<&str>,
) -> Result<Value, Box<dyn Error>> {
    let folder = PathBuf::from(path.trim());
    let path_exists = folder.is_dir();
    let engine = normalized_engine_type(engine_type.unwrap_or_default(), &folder);
    let (project_file, launcher) = if path_exists {
        resolve_project_launcher(&folder, &engine)
    } else {
        (None, None)
    };
    let build_script = path_exists
        .then(|| find_workspace_build_script(&folder))
        .flatten();
    let open_project_reason = if !path_exists {
        "本机工程目录不存在"
    } else if engine.is_empty() {
        "未配置或识别工程引擎"
    } else if project_file.is_none() {
        "工程目录与引擎类型不匹配"
    } else if launcher.is_none() {
        "未找到对应引擎编辑器"
    } else {
        ""
    };
    Ok(json!({
        "path_exists": path_exists,
        "engine_type": engine,
        "project_file": project_file,
        "editor_path": launcher,
        "build_script": build_script,
        "can_open_folder": path_exists,
        "can_open_project": open_project_reason.is_empty(),
        "can_build": build_script.is_some(),
        "open_folder_reason": if path_exists { "" } else { "本机工程目录不存在" },
        "open_project_reason": open_project_reason,
        "build_reason": if !path_exists { "本机工程目录不存在" } else if build_script.is_none() { "需在工程的 .himind 目录配置 build.ps1、build.cmd 或 build.bat" } else { "" },
    }))
}

pub(crate) fn launch_project_workspace(
    path: &str,
    engine_type: Option<&str>,
    engine_version: Option<&str>,
) -> Result<Value, Box<dyn Error>> {
    let status = inspect_project_workspace(path, engine_type, engine_version)?;
    if !status["can_open_project"].as_bool().unwrap_or(false) {
        return Err(status["open_project_reason"]
            .as_str()
            .unwrap_or("工程不可打开")
            .into());
    }
    let editor = status["editor_path"]
        .as_str()
        .ok_or("editor path is missing")?;
    let project = status["project_file"]
        .as_str()
        .ok_or("project file is missing")?;
    let engine = status["engine_type"].as_str().unwrap_or_default();
    let mut command = Command::new(editor);
    if engine == "unity" {
        command.arg("-projectPath").arg(project);
    } else {
        command.arg(project);
    }
    command.spawn()?;
    Ok(json!({ "ok": true, "engine_type": engine, "editor_path": editor, "project_file": project }))
}

pub(crate) fn launch_workspace_build(path: &str) -> Result<Value, Box<dyn Error>> {
    let workspace = PathBuf::from(path.trim()).canonicalize()?;
    if !workspace.is_dir() {
        return Err("本机工程目录不存在".into());
    }
    let script = find_workspace_build_script(&workspace)
        .ok_or("需在工程的 .himind 目录配置 build.ps1、build.cmd 或 build.bat")?
        .canonicalize()?;
    if !script.starts_with(&workspace) {
        return Err("构建脚本必须位于当前工程目录内".into());
    }

    let extension = script
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut command = if extension == "ps1" {
        let mut command = Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&script);
        command
    } else if matches!(extension.as_str(), "cmd" | "bat") {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/S", "/C"]).arg(&script);
        command
    } else {
        return Err("不支持的构建脚本类型".into());
    };
    let child = command
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(json!({
        "ok": true,
        "started": true,
        "process_id": child.id(),
        "workspace": workspace,
        "build_script": script,
    }))
}

fn find_workspace_build_script(workspace: &Path) -> Option<PathBuf> {
    ["build.ps1", "build.cmd", "build.bat"]
        .into_iter()
        .map(|name| workspace.join(".himind").join(name))
        .find(|candidate| candidate.is_file())
}

fn normalized_engine_type(configured: &str, folder: &Path) -> String {
    let configured = configured.trim().to_ascii_lowercase();
    if configured.contains("unity") || configured == "u3d" {
        return "unity".to_string();
    }
    if configured.contains("unreal")
        || configured == "ue"
        || configured.starts_with("ue4")
        || configured.starts_with("ue5")
    {
        return "unreal".to_string();
    }
    if is_unity_project(folder) {
        return "unity".to_string();
    }
    if first_file_with_extension(folder, "uproject").is_some() {
        return "unreal".to_string();
    }
    String::new()
}

fn resolve_project_launcher(folder: &Path, engine: &str) -> (Option<String>, Option<String>) {
    if engine == "unity" {
        let project_file = is_unity_project(folder).then(|| folder.to_string_lossy().to_string());
        let launcher = unity_editor_environment_path()
            .filter(|value| Path::new(value).is_file())
            .or_else(configured_unity_editor_path)
            .or_else(discovered_unity_editor_path)
            .filter(|value| Path::new(value).is_file());
        return (project_file, launcher);
    }
    if engine == "unreal" {
        let project_file = first_file_with_extension(folder, "uproject")
            .map(|value| value.to_string_lossy().to_string());
        let launcher = std::env::var("HIMIND_UNREAL_EDITOR")
            .ok()
            .filter(|value| Path::new(value).is_file())
            .or_else(find_unreal_editor);
        return (project_file, launcher);
    }
    (None, None)
}

fn is_unity_project(folder: &Path) -> bool {
    folder.join("Assets").is_dir()
        && folder.join("Packages").is_dir()
        && folder.join("ProjectSettings").is_dir()
}

fn first_file_with_extension(folder: &Path, extension: &str) -> Option<PathBuf> {
    std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .map(|item| item.path())
        .find(|path| {
            path.extension()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(extension))
                .unwrap_or(false)
        })
}

fn find_unreal_editor() -> Option<String> {
    let root = Path::new(r"C:\Program Files\Epic Games");
    let mut candidates = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|item| item.path().join(r"Engine\Binaries\Win64\UnrealEditor.exe"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    candidates.sort();
    candidates
        .pop()
        .map(|path| path.to_string_lossy().to_string())
}

pub(crate) fn launch_remote_connection(
    payload: &RemoteConnectRequest,
    agent_state_path: &Path,
) -> Result<Value, Box<dyn Error>> {
    let vendor = payload.vendor.trim().to_lowercase();
    let code = payload
        .code
        .chars()
        .filter(|item| !item.is_whitespace())
        .collect::<String>();
    if code.is_empty() {
        return Err(remote_connect_error("device code is required"));
    }

    let password = payload.password.as_deref().unwrap_or_default().trim();
    let label = payload.label.as_deref().unwrap_or_default().trim();
    if let Some(value) = launch_remote_connection_from_env(&vendor, &code, password, label)? {
        return Ok(value);
    }

    let resolved_client = crate::app::remote_clients::resolve(&vendor, agent_state_path)?;

    // ToDesk's Flutter UI does not expose editable controls reliably. Prefer
    // its documented command-line entry point when the resolved executable is
    // available, even if another ToDesk window is already running. The vendor
    // process is responsible for forwarding the request to its existing instance.
    if vendor.contains("todesk") {
        if let Some(executable) = resolved_client.as_ref().map(|item| item.path.as_path()) {
            if let Some(value) = launch_builtin_todesk_cli(executable, &code, password, label)? {
                return Ok(value);
            }
        }
        if let Some(value) = launch_builtin_todesk_cli_from_path_search(&code, password, label)? {
            return Ok(value);
        }
    }

    if activate_sunlogin_session(&vendor, &code, 0) {
        return Ok(json!({
            "ok": true,
            "vendor": vendor,
            "mode": "existing_session",
            "client_reused": true,
            "code_copied": false,
            "label": label,
            "connection_verified": true,
        }));
    }

    let process_names = remote_client_process_names(&vendor)?;
    if let Some(process_id) = find_running_remote_process(&process_names) {
        let automation = try_auto_fill_remote_connection(process_id, &vendor, &code, password);
        let submitted = automation.as_ref().copied().unwrap_or(false);
        let connection_verified = submitted && activate_sunlogin_session(&vendor, &code, 20);
        let automation_error = automation.err().map(|error| error.to_string()).or_else(|| {
            (submitted && !connection_verified).then(|| "未检测到向日葵远程桌面窗口".to_string())
        });
        let code_copied = if submitted {
            clear_clipboard()
        } else {
            set_clipboard_text(&code).is_ok()
        };
        return Ok(json!({
            "ok": true,
            "vendor": vendor,
            "mode": if connection_verified { "gui_verified" } else { "client_opened" },
            "client": process_names.first().cloned().unwrap_or_default(),
            "client_reused": true,
            "code_copied": code_copied,
            "label": label,
            "automation_error": automation_error,
            "connection_verified": connection_verified,
        }));
    }

    let clients = resolved_client
        .as_ref()
        .map(|item| vec![item.path.to_string_lossy().to_string()])
        .unwrap_or_default();
    let mut last_error = String::new();
    for client in clients {
        match Command::new(&client).spawn() {
            Ok(child) => {
                // The vendor process may forward to an already running instance, so
                // the child PID is only a hint. The automation helper resolves the
                // real foreground process by PID and vendor process names.
                let automation =
                    try_auto_fill_remote_connection(child.id(), &vendor, &code, password);
                let submitted = automation.as_ref().copied().unwrap_or(false);
                let connection_verified =
                    submitted && activate_sunlogin_session(&vendor, &code, 20);
                let automation_error =
                    automation.err().map(|error| error.to_string()).or_else(|| {
                        (submitted && !connection_verified)
                            .then(|| "未检测到向日葵远程桌面窗口".to_string())
                    });
                let code_copied = if submitted {
                    clear_clipboard()
                } else {
                    set_clipboard_text(&code).is_ok()
                };
                return Ok(json!({
                    "ok": true,
                    "vendor": vendor,
                    "mode": if connection_verified { "gui_verified" } else { "client_opened" },
                    "client": client,
                    "client_reused": false,
                    "code_copied": code_copied,
                    "label": label,
                    "automation_error": automation_error,
                    "connection_verified": connection_verified,
                }));
            }
            Err(error) => last_error = format!("{client}: {error}"),
        }
    }

    Err(remote_connect_error(&format!(
        "remote client not found; configure the client in Agent settings or set HIMIND_{}_CLI ({last_error})",
        remote_env_prefix(&vendor)?
    )))
}

fn launch_builtin_todesk_cli(
    executable: &Path,
    code: &str,
    password: &str,
    label: &str,
) -> Result<Option<Value>, Box<dyn Error>> {
    if !executable.is_file() {
        return Ok(None);
    }

    let mut args = vec!["-control".to_string(), "-id".to_string(), code.to_string()];
    if !password.trim().is_empty() {
        args.extend(["-passwd".to_string(), password.to_string()]);
    }
    Command::new(executable).args(&args).spawn()?;
    Ok(Some(json!({
        "ok": true,
        "vendor": "todesk",
        "mode": "cli_template",
        "client": executable.to_string_lossy(),
        "client_reused": true,
        "code_copied": false,
        "label": label,
        "connection_verified": false,
        "verification_pending": true,
    })))
}

fn launch_builtin_todesk_cli_from_path_search(
    code: &str,
    password: &str,
    label: &str,
) -> Result<Option<Value>, Box<dyn Error>> {
    let Some(executable) = remote_client_candidates("todesk")?
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
    else {
        return Ok(None);
    };
    launch_builtin_todesk_cli(&executable, code, password, label)
}

fn activate_sunlogin_session(vendor: &str, code: &str, timeout_seconds: u64) -> bool {
    if !(vendor.contains("sunlogin") || vendor.contains("向日葵") || vendor.contains("oray")) {
        return false;
    }
    let script = r#"
$needle = $env:HIMIND_REMOTE_WINDOW_TITLE
$deadline = (Get-Date).AddSeconds([int]$env:HIMIND_REMOTE_WINDOW_TIMEOUT)
Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class HimindRemoteWindow {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    public static readonly IntPtr HWND_TOPMOST = new IntPtr(-1);
    public static readonly IntPtr HWND_NOTOPMOST = new IntPtr(-2);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder text, int maxCount);
    [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr hWnd, int command);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr SetActiveWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr SetFocus(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool attach);
    [DllImport("user32.dll")] public static extern void SwitchToThisWindow(IntPtr hWnd, bool fAltTab);
    public static bool IsFocusedForProcess(IntPtr hWnd) {
        if (hWnd == IntPtr.Zero || !IsWindowVisible(hWnd)) return false;
        var foreground = GetForegroundWindow();
        if (foreground == IntPtr.Zero) return false;
        uint targetProcessId;
        uint foregroundProcessId;
        GetWindowThreadProcessId(hWnd, out targetProcessId);
        GetWindowThreadProcessId(foreground, out foregroundProcessId);
        return targetProcessId != 0 && targetProcessId == foregroundProcessId;
    }
    public static bool Focus(IntPtr hWnd) {
        if (hWnd == IntPtr.Zero || !IsWindowVisible(hWnd)) return false;
        ShowWindowAsync(hWnd, 9);
        var foreground = GetForegroundWindow();
        uint ignoredTargetProcessId;
        uint ignoredForegroundProcessId;
        var targetThread = GetWindowThreadProcessId(hWnd, out ignoredTargetProcessId);
        var currentThread = GetCurrentThreadId();
        var foregroundThread = foreground == IntPtr.Zero ? 0 : GetWindowThreadProcessId(foreground, out ignoredForegroundProcessId);
        var attachedTarget = targetThread != 0 && targetThread != currentThread && AttachThreadInput(currentThread, targetThread, true);
        var attachedForeground = foregroundThread != 0 && foregroundThread != currentThread && AttachThreadInput(currentThread, foregroundThread, true);
        BringWindowToTop(hWnd);
        SetWindowPos(hWnd, HWND_TOPMOST, 0, 0, 0, 0, 0x0003);
        SetWindowPos(hWnd, HWND_NOTOPMOST, 0, 0, 0, 0, 0x0003);
        SwitchToThisWindow(hWnd, true);
        SetForegroundWindow(hWnd);
        SetActiveWindow(hWnd);
        SetFocus(hWnd);
        var focused = false;
        for (var attempt = 0; attempt < 4; attempt++) {
            System.Threading.Thread.Sleep(150);
            if (IsFocusedForProcess(hWnd)) { focused = true; break; }
            BringWindowToTop(hWnd);
            SwitchToThisWindow(hWnd, true);
            SetForegroundWindow(hWnd);
        }
        if (attachedForeground) AttachThreadInput(currentThread, foregroundThread, false);
        if (attachedTarget) AttachThreadInput(currentThread, targetThread, false);
        return focused;
    }
    public static bool Activate(string needle) {
        bool found = false;
        EnumWindows((hWnd, _) => {
            if (!IsWindowVisible(hWnd)) return true;
            var title = new StringBuilder(512);
            GetWindowText(hWnd, title, title.Capacity);
            if (title.ToString().IndexOf(needle, StringComparison.OrdinalIgnoreCase) < 0) return true;
            found = Focus(hWnd);
            return false;
        }, IntPtr.Zero);
        return found;
    }
}
'@
do {
    if ([HimindRemoteWindow]::Activate($needle)) { exit 0 }
    Start-Sleep -Milliseconds 300
} while ((Get-Date) -lt $deadline)
exit 1
"#;
    Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .env("HIMIND_REMOTE_WINDOW_TITLE", code)
        .env("HIMIND_REMOTE_WINDOW_TIMEOUT", timeout_seconds.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn launch_remote_connection_from_env(
    vendor: &str,
    code: &str,
    password: &str,
    label: &str,
) -> Result<Option<Value>, Box<dyn Error>> {
    let prefix = remote_env_prefix(vendor)?;
    let Some((cli_var, cli)) = [
        format!("HIMIND_{prefix}_CLI"),
        format!("PROJECT_DASHBOARD_{prefix}_CLI"),
    ]
    .into_iter()
    .find_map(|name| {
        env::var(&name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| (name, value))
    }) else {
        return Ok(None);
    };
    let args_var = if cli_var.starts_with("PROJECT_DASHBOARD_") {
        format!("PROJECT_DASHBOARD_{prefix}_ARGS")
    } else {
        format!("HIMIND_{prefix}_ARGS")
    };
    let args_template = env::var(&args_var)
        .or_else(|_| env::var(format!("PROJECT_DASHBOARD_{prefix}_ARGS")))
        .unwrap_or_default();
    // Accept either an executable path or a quoted command prefix. A plain
    // Windows path may contain spaces and does not need to be quoted when it
    // points to an existing file.
    let cli_value = cli.trim();
    let (executable, mut cli_parts) = if Path::new(cli_value).is_file() {
        (cli_value.to_string(), Vec::new())
    } else {
        let mut parts = split_command_template(cli_value);
        let executable = parts
            .first()
            .cloned()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| remote_connect_error("remote client CLI is empty"))?;
        parts.remove(0);
        (executable, parts)
    };
    cli_parts.extend(render_remote_args(&args_template, code, password, label));
    Command::new(executable).args(&cli_parts).spawn()?;
    Ok(Some(json!({
        "ok": true,
        "vendor": vendor,
        "mode": "cli_template",
        "cli_var": cli_var,
        "args_var": args_var,
        "label": label,
        "client_reused": false,
        "code_copied": false,
        "connection_verified": false,
    })))
}

fn render_remote_args(template: &str, code: &str, password: &str, label: &str) -> Vec<String> {
    split_command_template(template)
        .into_iter()
        .map(|item| {
            item.replace("{code}", code)
                .replace("{password}", password)
                .replace("{label}", label)
        })
        .filter(|item| !item.is_empty())
        .collect()
}

fn split_command_template(template: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in template.chars() {
        match (character, quote) {
            ('"', None) | ('\'', None) => quote = Some(character),
            (character, Some(active)) if character == active => quote = None,
            (character, None) if character.is_whitespace() => {
                if !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                }
            }
            (character, _) => current.push(character),
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

fn remote_client_process_names(vendor: &str) -> Result<Vec<String>, Box<dyn Error>> {
    crate::app::remote_clients::process_names(vendor)
}

fn find_running_remote_process(process_names: &[String]) -> Option<u32> {
    let names = process_names
        .iter()
        .map(|name| format!("'{}'", name.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "$names=@({names}); Get-Process -Name $names -ErrorAction SilentlyContinue | Where-Object {{ $_.MainWindowHandle -ne 0 }} | Select-Object -First 1 -ExpandProperty Id"
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()
}

fn remote_client_candidates(vendor: &str) -> Result<Vec<String>, Box<dyn Error>> {
    crate::app::remote_clients::discovery_candidates(vendor)
}

fn remote_env_prefix(vendor: &str) -> Result<&'static str, Box<dyn Error>> {
    if vendor.contains("todesk") {
        return Ok("TODESK");
    }
    if vendor.contains("sunlogin") || vendor.contains("向日葵") || vendor.contains("oray") {
        return Ok("SUNLOGIN");
    }
    Err(remote_connect_error("unsupported remote vendor"))
}

fn set_clipboard_text(value: &str) -> Result<(), Box<dyn Error>> {
    let escaped = value.replace('`', "``").replace('\'', "''");
    let script = format!("Set-Clipboard -Value '{escaped}'");
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    if !status.success() {
        return Err(remote_connect_error("clipboard update failed"));
    }
    Ok(())
}

fn clear_clipboard() -> bool {
    set_clipboard_text("").is_ok()
}

fn try_auto_fill_remote_connection(
    process_id: u32,
    vendor: &str,
    code: &str,
    password: &str,
) -> Result<bool, Box<dyn Error>> {
    if code.trim().is_empty() {
        return Ok(false);
    }

    if vendor.contains("sunlogin") || vendor.contains("向日葵") || vendor.contains("oray") {
        return try_sunlogin_gui_connection(process_id, code, password);
    }

    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName Microsoft.VisualBasic

$pidValue = [int]$env:HIMIND_REMOTE_TARGET_PID
$vendor = $env:HIMIND_REMOTE_VENDOR
$code = $env:HIMIND_REMOTE_CODE
$password = $env:HIMIND_REMOTE_PASSWORD
$processNames = $env:HIMIND_REMOTE_PROCESS_NAMES -split ';'
$deadline = (Get-Date).AddSeconds(12)
$process = $null

do {
    $process = Get-Process -Id $pidValue -ErrorAction SilentlyContinue
    if ($null -eq $process -or $process.MainWindowHandle -eq 0) {
        $process = Get-Process -Name $processNames -ErrorAction SilentlyContinue |
            Where-Object { $_.MainWindowHandle -ne 0 } |
            Select-Object -First 1
        if ($null -ne $process) { $pidValue = $process.Id }
    }
    if ($null -ne $process -and $process.MainWindowHandle -ne 0) { break }
    Start-Sleep -Milliseconds 250
} while ((Get-Date) -lt $deadline)

if ($null -eq $process -or $process.MainWindowHandle -eq 0) {
    exit 2
}

# Flutter vendor windows may reject AppActivate even while exposing a usable
# UI Automation tree. Continue with control-based input and keep activation
# only as a best-effort aid for the keyboard fallback.
$activated = [Microsoft.VisualBasic.Interaction]::AppActivate($pidValue)
Start-Sleep -Milliseconds 500

# Prefer accessibility controls when the vendor exposes them. This is less
# sensitive to focus and DPI than a pure SendKeys sequence; keyboard input is
# retained as the compatibility fallback for older/vendor-customized clients.
try {
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
    $editCondition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Edit
    )
    $edits = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $editCondition)
    $visibleEdits = @($edits | Where-Object {
        $current = $_.Current
        -not $current.IsOffscreen -and $current.IsEnabled
    })
    $usableEdits = @($visibleEdits | Where-Object {
        $current = $_.Current
        $description = "{0} {1} {2}" -f $current.Name, $current.AutomationId, $current.ClassName
        $visibleEdits.Count -eq 1 -or
            $description -match '(?i)device|remote|code|id|设备|识别|伙伴|连接'
    })
    # Flutter-based vendor clients often expose both fields as unnamed generic
    # edits. Their stable order is device code followed by password/captcha.
    if ($usableEdits.Count -eq 0 -and $visibleEdits.Count -gt 0 -and
        $vendor -match '(?i)sunlogin|todesk|向日葵') {
        $usableEdits = $visibleEdits
    }
    $passwordEdit = @($edits | Where-Object {
        $current = $_.Current
        $description = "{0} {1} {2}" -f $current.Name, $current.AutomationId, $current.ClassName
        -not $current.IsOffscreen -and $current.IsEnabled -and
            $description -match '(?i)password|passwd|pwd|验证码|密码'
    }) | Select-Object -First 1
    if ($null -eq $passwordEdit -and $usableEdits.Count -gt 1) {
        $passwordEdit = $usableEdits[1]
    }
    $passwordReady = [string]::IsNullOrWhiteSpace($password) -or $null -ne $passwordEdit
    if ($usableEdits.Count -gt 0 -and $passwordReady) {
        # Flutter's ValuePattern can report success without updating the
        # visible text field. Focus each control and paste through the
        # keyboard so the vendor's input state and change handlers run.
        try {
            $valuePattern = $usableEdits[0].GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
            $valuePattern.SetValue($code)
        } catch { }
        $usableEdits[0].SetFocus()
        [System.Windows.Forms.SendKeys]::SendWait('^a')
        Set-Clipboard -Value $code
        [System.Windows.Forms.SendKeys]::SendWait('^v')
        Start-Sleep -Milliseconds 220
        if (-not [string]::IsNullOrWhiteSpace($password) -and $null -ne $passwordEdit) {
            try {
                $passwordPattern = $passwordEdit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
                $passwordPattern.SetValue($password)
            } catch { }
            $passwordEdit.SetFocus()
            [System.Windows.Forms.SendKeys]::SendWait('^a')
            Set-Clipboard -Value $password
            [System.Windows.Forms.SendKeys]::SendWait('^v')
            Start-Sleep -Milliseconds 220
        }
        [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
        if ($vendor -match '(?i)sunlogin|向日葵|oray') {
            Start-Sleep -Milliseconds 900
            [void][Microsoft.VisualBasic.Interaction]::AppActivate($code)
        }
        exit 0
    }
} catch {
    # Fall through to the vendor-independent keyboard recipe below.
}

if (-not $activated) {
    [void][Microsoft.VisualBasic.Interaction]::AppActivate($pidValue)
    Start-Sleep -Milliseconds 200
}
[System.Windows.Forms.SendKeys]::SendWait('^a')
Start-Sleep -Milliseconds 120
Set-Clipboard -Value $code
[System.Windows.Forms.SendKeys]::SendWait('^v')
Start-Sleep -Milliseconds 180

if (-not [string]::IsNullOrWhiteSpace($password)) {
    [System.Windows.Forms.SendKeys]::SendWait('{TAB}')
    Start-Sleep -Milliseconds 150
    [System.Windows.Forms.SendKeys]::SendWait('^a')
    Start-Sleep -Milliseconds 120
    Set-Clipboard -Value $password
    [System.Windows.Forms.SendKeys]::SendWait('^v')
    Start-Sleep -Milliseconds 180
}

[System.Windows.Forms.SendKeys]::SendWait('{ENTER}')

# Sunlogin opens the remote session in a separate Flutter window. AppActivate
# on the original shell PID can leave that session behind the Dashboard or an
# older Sunlogin window, making a successful connection look like a no-op.
if ($vendor -match '(?i)sunlogin|向日葵|oray') {
    Start-Sleep -Milliseconds 900
    [void][Microsoft.VisualBasic.Interaction]::AppActivate($code)
}
exit 0
"#;

    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .env("HIMIND_REMOTE_TARGET_PID", process_id.to_string())
        .env("HIMIND_REMOTE_VENDOR", vendor)
        .env(
            "HIMIND_REMOTE_PROCESS_NAMES",
            remote_client_process_names(vendor)?.join(";"),
        )
        .env("HIMIND_REMOTE_CODE", code)
        .env("HIMIND_REMOTE_PASSWORD", password)
        .status()?;
    if status.success() {
        Ok(true)
    } else {
        Err(remote_connect_error(&format!(
            "remote UI automation exited with code {}",
            status.code().unwrap_or(-1)
        )))
    }
}

fn try_sunlogin_gui_connection(
    process_id: u32,
    code: &str,
    password: &str,
) -> Result<bool, Box<dyn Error>> {
    // AweSun's Flutter fields do not implement Windows ValuePattern and
    // ignore synthetic Ctrl+V. Its native context-menu paste path is stable.
    let script = r#"
Add-Type -AssemblyName Microsoft.VisualBasic
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class HimindSunloginInput {
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X; public int Y; }
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left; public int Top; public int Right; public int Bottom; }
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hWnd, out Rect rect);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hWnd, ref Point point);
    [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr hWnd, int command);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr SetActiveWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr SetFocus(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool attach);
    [DllImport("user32.dll")] public static extern void SwitchToThisWindow(IntPtr hWnd, bool fAltTab);
    public static bool IsFocusedForProcess(IntPtr hWnd) {
        if (hWnd == IntPtr.Zero || !IsWindowVisible(hWnd)) return false;
        var foreground = GetForegroundWindow();
        if (foreground == IntPtr.Zero) return false;
        uint targetProcessId;
        uint foregroundProcessId;
        GetWindowThreadProcessId(hWnd, out targetProcessId);
        GetWindowThreadProcessId(foreground, out foregroundProcessId);
        return targetProcessId != 0 && targetProcessId == foregroundProcessId;
    }
    public static readonly IntPtr HWND_TOPMOST = new IntPtr(-1);
    public static readonly IntPtr HWND_NOTOPMOST = new IntPtr(-2);
    public static bool Focus(IntPtr hWnd) {
        ShowWindowAsync(hWnd, 9);
        var foreground = GetForegroundWindow();
        uint ignoredTargetProcessId;
        uint ignoredForegroundProcessId;
        var targetThread = GetWindowThreadProcessId(hWnd, out ignoredTargetProcessId);
        var currentThread = GetCurrentThreadId();
        var foregroundThread = foreground == IntPtr.Zero ? 0 : GetWindowThreadProcessId(foreground, out ignoredForegroundProcessId);
        var attachedTarget = targetThread != 0 && targetThread != currentThread && AttachThreadInput(currentThread, targetThread, true);
        var attachedForeground = foregroundThread != 0 && foregroundThread != currentThread && AttachThreadInput(currentThread, foregroundThread, true);
        BringWindowToTop(hWnd);
        SetWindowPos(hWnd, HWND_TOPMOST, 0, 0, 0, 0, 0x0003);
        SetWindowPos(hWnd, HWND_NOTOPMOST, 0, 0, 0, 0, 0x0003);
        SwitchToThisWindow(hWnd, true);
        SetForegroundWindow(hWnd);
        SetActiveWindow(hWnd);
        SetFocus(hWnd);
        var focused = false;
        for (var attempt = 0; attempt < 4; attempt++) {
            System.Threading.Thread.Sleep(150);
            if (IsFocusedForProcess(hWnd)) { focused = true; break; }
            BringWindowToTop(hWnd);
            SwitchToThisWindow(hWnd, true);
            SetForegroundWindow(hWnd);
        }
        if (attachedForeground) AttachThreadInput(currentThread, foregroundThread, false);
        if (attachedTarget) AttachThreadInput(currentThread, targetThread, false);
        return focused;
    }
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
    public static void LeftClick(int x, int y) {
        SetCursorPos(x, y);
        mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero);
        mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
    }
    public static void RightClick(int x, int y) {
        SetCursorPos(x, y);
        mouse_event(0x0008, 0, 0, 0, UIntPtr.Zero);
        mouse_event(0x0010, 0, 0, 0, UIntPtr.Zero);
    }
}
'@

$pidValue = [int]$env:HIMIND_REMOTE_TARGET_PID
$code = $env:HIMIND_REMOTE_CODE
$password = $env:HIMIND_REMOTE_PASSWORD
# Wait until the Sunlogin main window is visible, restored, and has held a
# stable size for several samples. Flutter windows expose a handle long
# before the layout is ready, so sending input too early misses the fields.
$deadline = (Get-Date).AddSeconds(30)
$process = $null
$stableSamples = 0
$lastWidth = 0
$lastHeight = 0
do {
    $process = Get-Process -Id $pidValue -ErrorAction SilentlyContinue
    if ($null -eq $process -or $process.MainWindowHandle -eq 0) {
        $process = Get-Process -Name AweSun,SunloginClient -ErrorAction SilentlyContinue |
            Where-Object { $_.MainWindowHandle -ne 0 -and $_.MainWindowTitle -match '向日葵|AweSun|Sunlogin' } |
            Select-Object -First 1
        if ($null -ne $process) { $pidValue = $process.Id }
    }
    if ($null -ne $process -and $process.MainWindowHandle -ne 0) {
        $handle = $process.MainWindowHandle
        if ([HimindSunloginInput]::IsIconic($handle)) {
            [void][HimindSunloginInput]::ShowWindowAsync($handle, 9)
            $stableSamples = 0
            $lastWidth = 0
            $lastHeight = 0
        }
        elseif ([HimindSunloginInput]::IsWindowVisible($handle)) {
            $sample = New-Object HimindSunloginInput+Rect
            if ([HimindSunloginInput]::GetClientRect($handle, [ref]$sample)) {
                $sampleWidth = $sample.Right - $sample.Left
                $sampleHeight = $sample.Bottom - $sample.Top
                if ($sampleWidth -eq $lastWidth -and $sampleHeight -eq $lastHeight) { $stableSamples++ }
                else { $stableSamples = 0 }
                $lastWidth = $sampleWidth
                $lastHeight = $sampleHeight
                if ($stableSamples -ge 3 -and $sampleWidth -ge 700 -and $sampleHeight -ge 480) { break }
            }
        }
    }
    Start-Sleep -Milliseconds 300
} while ((Get-Date) -lt $deadline)
if ($null -eq $process -or $process.MainWindowHandle -eq 0) { exit 2 }

# Restore and activate the window, then wait until it is actually in the
# foreground and visible before sending any input.
$null = [Microsoft.VisualBasic.Interaction]::AppActivate($pidValue)
$focused = [HimindSunloginInput]::Focus($process.MainWindowHandle)
$foregroundDeadline = (Get-Date).AddSeconds(5)
do {
    Start-Sleep -Milliseconds 150
    $foreground = [HimindSunloginInput]::GetForegroundWindow()
    if ([HimindSunloginInput]::IsFocusedForProcess($process.MainWindowHandle)) { $focused = $true; break }
    [void][Microsoft.VisualBasic.Interaction]::AppActivate($pidValue)
    $focused = [HimindSunloginInput]::Focus($process.MainWindowHandle)
} while ((Get-Date) -lt $foregroundDeadline)
if (-not $focused) { exit 6 }
Start-Sleep -Milliseconds 600

# Re-measure the layout after restore; the Flutter canvas only reports its
# final size once the window has settled.
$client = New-Object HimindSunloginInput+Rect
$origin = New-Object HimindSunloginInput+Point
if (-not [HimindSunloginInput]::GetClientRect($process.MainWindowHandle, [ref]$client)) { exit 3 }
if (-not [HimindSunloginInput]::ClientToScreen($process.MainWindowHandle, [ref]$origin)) { exit 4 }
$width = $client.Right - $client.Left
$height = $client.Bottom - $client.Top
if ($width -lt 700 -or $height -lt 480) { exit 5 }

function Point-X([double]$ratio) { return $origin.X + [int]($width * $ratio) }
function Point-Y([double]$ratio) { return $origin.Y + [int]($height * $ratio) }
function Abort-Input([string]$reason) {
    Set-Clipboard -Value ''
    Write-Error $reason
    exit 6
}
function Ensure-Foreground() {
    if (-not [HimindSunloginInput]::Focus($process.MainWindowHandle)) { return $false }
    Start-Sleep -Milliseconds 80
    return [HimindSunloginInput]::IsFocusedForProcess($process.MainWindowHandle)
}
function Paste-WithContextMenu([double]$xRatio, [double]$yRatio, [string]$value) {
    if (-not (Ensure-Foreground)) { Abort-Input 'sunlogin window lost foreground before paste' }
    Set-Clipboard -Value $value
    $x = Point-X $xRatio
    $y = Point-Y $yRatio
    if (-not [HimindSunloginInput]::IsFocusedForProcess($process.MainWindowHandle)) {
        Abort-Input 'sunlogin window lost foreground before context menu'
    }
    [HimindSunloginInput]::RightClick($x, $y)
    Start-Sleep -Milliseconds 180
    # Opening the context menu can change the foreground HWND. Do not call
    # Focus here: reactivating the Flutter window would close the menu.
    if (-not [HimindSunloginInput]::IsFocusedForProcess($process.MainWindowHandle)) {
        Abort-Input 'sunlogin window lost foreground after context menu'
    }
    [HimindSunloginInput]::LeftClick($x + 32, $y + 20)
    Start-Sleep -Milliseconds 220
}

# Restore the stable main page before targeting its fixed Flutter layout.
if (-not (Ensure-Foreground)) { Abort-Input 'sunlogin window lost foreground before navigation' }
[HimindSunloginInput]::LeftClick((Point-X 0.08), (Point-Y 0.16))
Start-Sleep -Milliseconds 650

# Use the clear glyph so repeated requests replace the existing code.
if (-not (Ensure-Foreground)) { Abort-Input 'sunlogin window lost foreground before clearing code' }
[HimindSunloginInput]::LeftClick((Point-X 0.475), (Point-Y 0.50))
Start-Sleep -Milliseconds 100
Paste-WithContextMenu 0.34 0.50 $code
if (-not [string]::IsNullOrWhiteSpace($password)) {
    Paste-WithContextMenu 0.575 0.50 $password
}
if (-not (Ensure-Foreground)) { Abort-Input 'sunlogin window lost foreground before submit' }
[HimindSunloginInput]::LeftClick((Point-X 0.79), (Point-Y 0.50))
Start-Sleep -Milliseconds 200
Set-Clipboard -Value ''
exit 0
"#;

    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .env("HIMIND_REMOTE_TARGET_PID", process_id.to_string())
        .env("HIMIND_REMOTE_CODE", code)
        .env("HIMIND_REMOTE_PASSWORD", password)
        .status()?;
    if status.success() {
        Ok(true)
    } else {
        match status.code() {
            Some(6) => Err(remote_connect_error(
                "sunlogin window did not become the foreground window",
            )),
            Some(code) => Err(remote_connect_error(&format!(
                "sunlogin GUI automation exited with code {code}"
            ))),
            None => Err(remote_connect_error(
                "sunlogin GUI automation terminated unexpectedly",
            )),
        }
    }
}

fn remote_connect_error(message: &str) -> Box<dyn Error> {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::rand_core::OsRng;
    use rsa::RsaPrivateKey;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn remote_cli_template_preserves_quoted_arguments() {
        assert_eq!(
            split_command_template(r#"--device "{code}" --label "Main Controller""#),
            vec![
                "--device".to_string(),
                "{code}".to_string(),
                "--label".to_string(),
                "Main Controller".to_string(),
            ]
        );
    }

    #[test]
    fn remote_cli_prefix_accepts_quoted_windows_paths() {
        assert_eq!(
            split_command_template(r#""C:\Program Files\ToDesk\ToDesk.exe" --silent"#),
            vec![
                r#"C:\Program Files\ToDesk\ToDesk.exe"#.to_string(),
                "--silent".to_string(),
            ]
        );
    }

    #[test]
    fn remote_vendor_process_and_install_candidates_are_supported() {
        assert!(remote_client_process_names("todesk")
            .unwrap()
            .contains(&"ToDesk".to_string()));
        assert!(remote_client_process_names("sunlogin")
            .unwrap()
            .contains(&"SunloginClient".to_string()));
        assert!(remote_client_candidates("todesk")
            .unwrap()
            .iter()
            .any(|item| item.contains("Program Files")));
        assert!(remote_client_candidates("sunlogin")
            .unwrap()
            .iter()
            .any(|item| item.contains("Oray")));
    }

    #[test]
    fn detects_only_conventional_workspace_build_scripts() {
        let root = std::env::temp_dir().join(format!(
            "himind-workspace-build-script-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config_dir = root.join(".himind");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(root.join("build.ps1"), "exit 0").unwrap();
        assert!(find_workspace_build_script(&root).is_none());

        let expected = config_dir.join("build.cmd");
        std::fs::write(&expected, "@exit /b 0").unwrap();
        assert_eq!(find_workspace_build_script(&root), Some(expected));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_status_exposes_build_availability() {
        let root = std::env::temp_dir().join(format!(
            "himind-workspace-status-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".himind")).unwrap();
        let unavailable = inspect_project_workspace(root.to_str().unwrap(), None, None).unwrap();
        assert_eq!(unavailable["can_build"], false);

        std::fs::write(root.join(".himind").join("build.ps1"), "exit 0").unwrap();
        let available = inspect_project_workspace(root.to_str().unwrap(), None, None).unwrap();
        assert_eq!(available["can_build"], true);
        assert!(available["build_reason"].as_str().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn accepts_same_origin_update_url() {
        assert!(validate_update_download_url(
            "http://localhost:18081",
            "http://localhost:18081/api/distribution/artifacts/artifact-1/download"
        )
        .is_ok());
    }

    #[test]
    fn rejects_cross_origin_update_url() {
        assert!(validate_update_download_url(
            "http://localhost:18081",
            "https://downloads.example/himind-agent-update.zip"
        )
        .is_err());
    }

    #[test]
    fn rejects_non_http_update_url() {
        assert!(validate_update_download_url(
            "http://localhost:18081",
            "file:///C:/Temp/agent.exe"
        )
        .is_err());
    }

    #[test]
    fn validates_update_signature_metadata() {
        assert!(validate_signature_metadata("", "", "", false).is_ok());
        assert!(validate_signature_metadata("", "", "", true).is_err());
        assert!(
            validate_signature_metadata("c2ln", "release-2026", "rsa-pss-sha256", true).is_ok()
        );
        assert!(validate_signature_metadata("c2ln", "../escape", "rsa-pss-sha256", true).is_err());
        assert!(
            validate_signature_metadata("c2ln", "release-2026", "rsa-v1_5-sha256", true).is_err()
        );
    }

    #[test]
    fn embedded_update_key_enables_signed_update_policy() {
        if EMBEDDED_UPDATE_PUBLIC_KEY_PEM.trim().is_empty() {
            assert!(EMBEDDED_UPDATE_KEY_ID.trim().is_empty());
            return;
        }
        assert!(RsaPublicKey::from_public_key_pem(EMBEDDED_UPDATE_PUBLIC_KEY_PEM).is_ok());
        assert!(!EMBEDDED_UPDATE_KEY_ID.trim().is_empty());
        assert!(signed_agent_updates_required());
        assert_eq!(
            trusted_agent_update_key_ids(),
            vec![EMBEDDED_UPDATE_KEY_ID.trim().to_string()]
        );
    }

    #[test]
    fn update_restart_reopens_the_agent_with_a_final_safe_protocol_url() {
        let mut options = crate::Options::from_env();
        options.api_base = "https://himind.example".to_string();
        options.local_port = 18181;
        options.state_path = PathBuf::from(r"C:\HiMind\agent-state.json");

        let arguments = agent_restart_arguments(&options);

        assert_eq!(
            arguments,
            vec![
                "--api",
                "https://himind.example",
                "--local-app",
                "--local-port",
                "18181",
                "--state",
                r"C:\HiMind\agent-state.json",
                "--protocol-url",
                "himind-agent://open",
            ]
        );
    }

    #[test]
    fn production_artifact_signature_matches_embedded_key_when_requested() {
        let Ok(artifact_path) = std::env::var("HIMIND_TEST_SIGNED_ARTIFACT_PATH") else {
            return;
        };
        let metadata_path = std::env::var("HIMIND_TEST_SIGNATURE_METADATA_PATH")
            .expect("metadata path is required");
        let metadata: Value = serde_json::from_str(
            &std::fs::read_to_string(metadata_path).expect("signature metadata must be readable"),
        )
        .expect("signature metadata must be valid JSON");
        let key_id = metadata["signature_key_id"]
            .as_str()
            .expect("signature key id is required");
        assert_eq!(key_id, EMBEDDED_UPDATE_KEY_ID.trim());
        assert_eq!(metadata["signature_algorithm"], "rsa-pss-sha256");
        let public_key = trusted_agent_update_public_key(key_id).expect("key must be trusted");
        verify_rsa_pss_sha256(
            Path::new(&artifact_path),
            &public_key,
            metadata["signature"]
                .as_str()
                .expect("signature is required"),
        )
        .expect("artifact signature must verify");
    }

    #[test]
    fn verifies_rsa_pss_update_signature_and_rejects_tampering() {
        let root = std::env::temp_dir().join(format!(
            "project-dashboard-signature-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let artifact = root.join("agent.exe");
        std::fs::write(&artifact, b"signed agent artifact").unwrap();
        let mut rng = OsRng;
        let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public_key = RsaPublicKey::from(&private_key);
        let digest = Sha256::digest(std::fs::read(&artifact).unwrap());
        let signature = private_key
            .sign_with_rng(&mut rng, Pss::new::<Sha256>(), &digest)
            .unwrap();
        let public_pem = public_key.to_public_key_pem(LineEnding::LF).unwrap();
        assert!(
            verify_rsa_pss_sha256(&artifact, &public_pem, &BASE64_STANDARD.encode(&signature))
                .is_ok()
        );
        std::fs::write(&artifact, b"tampered agent artifact").unwrap();
        assert!(
            verify_rsa_pss_sha256(&artifact, &public_pem, &BASE64_STANDARD.encode(&signature))
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recognizes_minimal_unity_project_layout() {
        let root =
            std::env::temp_dir().join(format!("project-dashboard-unity-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Assets")).unwrap();
        std::fs::create_dir_all(root.join("Packages")).unwrap();
        std::fs::create_dir_all(root.join("ProjectSettings")).unwrap();
        assert!(is_unity_project(&root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_incomplete_unity_project_layout() {
        let root = std::env::temp_dir().join(format!(
            "project-dashboard-unity-incomplete-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("Assets")).unwrap();
        std::fs::create_dir_all(root.join("ProjectSettings")).unwrap();
        assert!(!is_unity_project(&root));
        std::fs::remove_dir_all(root).unwrap();
    }
}
