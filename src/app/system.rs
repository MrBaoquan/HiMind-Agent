use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use reqwest::blocking::Client;
use reqwest::Url;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pss, RsaPublicKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::app::types::{LocalAgentUpdateRequest, RemoteConnectRequest};
use crate::store::credentials::{configured_unity_editor_path, unity_editor_environment_path};
use crate::Options;

const CREATE_NO_WINDOW: u32 = 0x08000000;
const AUTO_START_REG_PATH: &str = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Run";
const AUTO_START_VALUE: &str = "ProjectDashboardAgent";
const EMBEDDED_UPDATE_PUBLIC_KEY_PEM: &str =
    include_str!(concat!(env!("OUT_DIR"), "/embedded-update-public-key.pem"));
const EMBEDDED_UPDATE_KEY_ID: &str =
    include_str!(concat!(env!("OUT_DIR"), "/embedded-update-key-id.txt"));

pub(crate) fn local_agent_update_supported() -> bool {
    env::current_exe().is_ok()
}

pub(crate) fn trigger_local_agent_update(
    options: &Options,
    payload: &LocalAgentUpdateRequest,
) -> Result<String, Box<dyn Error>> {
    let exe = env::current_exe()?;
    let download_url = payload
        .download_url
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    if download_url.is_empty() {
        schedule_agent_restart(&exe, options)?;

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            std::process::exit(0);
        });
        return Ok(
            "已开始重新加载本机 Agent 可执行文件，托盘和 127.0.0.1:18181 服务会短暂重启。"
                .to_string(),
        );
    }

    validate_update_download_url(&options.api_base, &download_url)?;
    let expected_sha256 = payload.sha256.as_deref().unwrap_or_default().trim();
    validate_sha256(expected_sha256)?;
    let target_version = payload.version.as_deref().unwrap_or_default().trim();
    if target_version.is_empty()
        || crate::skill::resolver::compare_versions(target_version, crate::VERSION)
            != std::cmp::Ordering::Greater
    {
        return Err("Agent 更新目标版本必须高于当前版本".into());
    }
    let staged_file = download_agent_package(&download_url, expected_sha256, &options.state_path)?;
    if let Err(error) = verify_agent_package_signature(&staged_file, payload) {
        let _ = std::fs::remove_file(&staged_file);
        return Err(error);
    }
    schedule_agent_replace_and_restart(&staged_file, &exe, options, target_version)?;

    thread::spawn(move || {
        thread::sleep(Duration::from_millis(500));
        std::process::exit(0);
    });
    let version_label = payload.version.as_deref().unwrap_or_default().trim();
    let checksum_label = payload.sha256.as_deref().unwrap_or_default().trim();
    let mut message = if version_label.is_empty() {
        "已开始下载并安装最新 Agent，可执行文件会在本机静默替换并自动重启。".to_string()
    } else {
        format!(
            "已开始下载并安装 Agent {}，可执行文件会在本机静默替换并自动重启。",
            version_label
        )
    };
    if !checksum_label.is_empty() {
        message.push_str(&format!(
            " 校验摘要：{}...",
            &checksum_label.chars().take(12).collect::<String>()
        ));
    }
    Ok(message)
}

fn schedule_agent_restart(executable: &Path, options: &Options) -> Result<(), Box<dyn Error>> {
    let working_dir = env::current_dir()?;
    let mut arg_list = vec![
        "'--api'".to_string(),
        format!("'{}'", powershell_escape_single_quoted(&options.api_base)),
        "'--local-app'".to_string(),
        "'--local-port'".to_string(),
        format!("'{}'", options.local_port),
    ];
    if !options.state_path.as_os_str().is_empty() {
        arg_list.push("'--state'".to_string());
        arg_list.push(format!(
            "'{}'",
            powershell_escape_single_quoted(&options.state_path.to_string_lossy())
        ));
    }
    let script = format!(
        "Start-Sleep -Milliseconds 900; Start-Process -FilePath '{}' -ArgumentList @({}) -WorkingDirectory '{}' -WindowStyle Hidden",
        powershell_escape_single_quoted(&executable.to_string_lossy()),
        arg_list.join(", "),
        powershell_escape_single_quoted(&working_dir.to_string_lossy())
    );
    let mut started = false;
    for shell in ["pwsh", "powershell"] {
        if Command::new(shell)
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .is_ok()
        {
            started = true;
            break;
        }
    }
    if !started {
        return Err("无法调度 Agent 可执行文件重载，请确认 pwsh 或 powershell 可用。".into());
    }
    Ok(())
}

fn download_agent_package(
    download_url: &str,
    expected_sha256: &str,
    agent_state_path: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?;
    let mut request = client.get(download_url);
    if download_url.contains("/api/distribution/artifacts/") {
        let state_path = crate::api::distribution::distribution_state_path(agent_state_path);
        if let Ok(content) = std::fs::read_to_string(state_path) {
            if let Ok(state) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(token) = state
                    .get("token")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    request = request.bearer_auth(token);
                }
            }
        }
    } else if download_url.contains("/api/agent-package/latest/update") {
        if let Ok(state) = crate::api::client::load_agent_state(agent_state_path) {
            if !state.agent_id.trim().is_empty() && !state.credential.trim().is_empty() {
                request = request.header(
                    "Authorization",
                    format!("Agent {}:{}", state.agent_id, state.credential),
                );
            }
        }
    }
    let mut response = request.send()?;
    response.error_for_status_ref()?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default();
    let staged_path = env::temp_dir().join(format!("himind-agent-update-{timestamp}.exe"));
    let mut file = File::create(&staged_path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
    }
    file.flush()?;
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        let _ = std::fs::remove_file(&staged_path);
        return Err(format!(
            "Agent 更新包 SHA-256 校验失败，期望 {expected_sha256}，实际 {actual_sha256}"
        )
        .into());
    }
    Ok(staged_path)
}

fn validate_update_download_url(api_base: &str, download_url: &str) -> Result<(), Box<dyn Error>> {
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

fn validate_sha256(value: &str) -> Result<(), Box<dyn Error>> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Agent 更新必须提供合法的 SHA-256 摘要".into());
    }
    Ok(())
}

fn verify_agent_package_signature(
    staged_executable: &Path,
    payload: &LocalAgentUpdateRequest,
) -> Result<(), Box<dyn Error>> {
    let signature = payload.signature.as_deref().unwrap_or_default().trim();
    let key_id = payload
        .signature_key_id
        .as_deref()
        .unwrap_or_default()
        .trim();
    let algorithm = payload
        .signature_algorithm
        .as_deref()
        .unwrap_or_default()
        .trim();
    let require_signed = signed_agent_updates_required();
    validate_signature_metadata(signature, key_id, algorithm, require_signed)?;
    if signature.is_empty() && key_id.is_empty() && algorithm.is_empty() {
        return Ok(());
    }
    let public_key = trusted_agent_update_public_key(key_id)?;
    verify_rsa_pss_sha256(staged_executable, &public_key, signature)
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

fn schedule_agent_replace_and_restart(
    staged_executable: &Path,
    current_executable: &Path,
    options: &Options,
    target_version: &str,
) -> Result<(), Box<dyn Error>> {
    let installed_layout = current_executable
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        == Some("current");
    if !installed_layout {
        return schedule_legacy_agent_replace_and_restart(
            staged_executable,
            current_executable,
            options,
        );
    }
    let updater = current_executable
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("himind-agent-updater.exe"))
        .filter(|path| path.is_file())
        .or_else(|| {
            current_executable
                .parent()
                .map(|path| path.join("himind-agent-updater.exe"))
        })
        .ok_or("无法定位 Agent updater")?;
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
    let payload = serde_json::json!({
        "current_executable": current_executable,
        "staged_executable": staged_executable,
        "api_base": options.api_base,
        "target_version": target_version,
        "local_port": options.local_port,
        "state_path": options.state_path,
        "old_pid": std::process::id(),
        "arguments": arguments,
    });
    Command::new(updater)
        .arg(payload.to_string())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

fn schedule_legacy_agent_replace_and_restart(
    staged_executable: &Path,
    current_executable: &Path,
    options: &Options,
) -> Result<(), Box<dyn Error>> {
    let working_dir = env::current_dir()?;
    let args = format!(
        "'--api','{}','--local-app','--local-port','{}','--state','{}'",
        powershell_escape_single_quoted(&options.api_base),
        options.local_port,
        powershell_escape_single_quoted(&options.state_path.to_string_lossy())
    );
    let script = format!(
        "Start-Sleep -Milliseconds 900; Copy-Item -Force '{}' '{}'; Start-Process -FilePath '{}' -ArgumentList @({}) -WorkingDirectory '{}' -WindowStyle Hidden; Remove-Item -Force '{}' -ErrorAction SilentlyContinue",
        powershell_escape_single_quoted(&staged_executable.to_string_lossy()),
        powershell_escape_single_quoted(&current_executable.to_string_lossy()),
        powershell_escape_single_quoted(&current_executable.to_string_lossy()),
        args,
        powershell_escape_single_quoted(&working_dir.to_string_lossy()),
        powershell_escape_single_quoted(&staged_executable.to_string_lossy()),
    );
    for shell in ["pwsh", "powershell"] {
        if Command::new(shell)
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
    }
    Err("无法调度 Agent 更新，请确认 PowerShell 可用。".into())
}

fn powershell_escape_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
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
    let executable = env::current_exe()?;
    let folder = executable
        .parent()
        .ok_or_else(|| std::io::Error::other("agent executable directory is missing"))?;
    open_folder(&folder.to_string_lossy())
}

pub(crate) fn create_plugin_view_shortcut(
    plugin_id: &str,
    view_id: &str,
    title: &str,
) -> Result<(), Box<dyn Error>> {
    let executable = env::current_exe()?;
    let desktop = env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|path| path.join("Desktop"))
        .filter(|path| path.is_dir())
        .ok_or_else(|| "Windows 桌面目录不可用")?;
    let shortcut_name = format!("{}.lnk", sanitize_shortcut_name(title));
    let shortcut = desktop.join(shortcut_name);
    let shortcut_path = shortcut.to_string_lossy().to_string();
    let target_path = executable.to_string_lossy().to_string();
    let arguments = format!(
        "--local-app --plugin-id \"{}\" --view-id \"{}\"",
        plugin_id, view_id
    );
    let working_directory = executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_string_lossy()
        .to_string();
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
    let executable = env::current_exe()?;
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
        let launcher = configured_unity_editor_path()
            .or_else(unity_editor_environment_path)
            .filter(|value| Path::new(value).is_file())
            .or_else(find_unity_editor);
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

fn find_unity_editor() -> Option<String> {
    let mut candidates = Vec::new();
    collect_unity_editors(
        Path::new(r"C:\Program Files\Unity\Hub\Editor"),
        &mut candidates,
    );
    collect_unity_editors(Path::new(r"C:\Program Files\Unity"), &mut candidates);
    if let Ok(entries) = std::fs::read_dir(r"C:\Program Files") {
        candidates.extend(entries.filter_map(Result::ok).filter_map(|item| {
            let name = item.file_name().to_string_lossy().to_string();
            name.starts_with("Unity ")
                .then(|| item.path().join(r"Editor\Unity.exe"))
                .filter(|path| path.is_file())
        }));
    }
    candidates.sort();
    candidates
        .pop()
        .map(|path| path.to_string_lossy().to_string())
}

fn collect_unity_editors(root: &Path, candidates: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    candidates.extend(
        entries
            .filter_map(Result::ok)
            .map(|item| item.path().join(r"Editor\Unity.exe"))
            .filter(|path| path.is_file()),
    );
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
    let clipboard_ready = set_clipboard_text(&code).is_ok();
    if let Some(value) = launch_remote_connection_from_env(&vendor, &code, password, label)? {
        return Ok(value);
    }

    let clients = remote_client_candidates(&vendor)?;
    let mut last_error = String::new();
    for client in clients {
        match Command::new(&client).spawn() {
            Ok(child) => {
                let automation_error =
                    match try_auto_fill_remote_connection(child.id(), &vendor, &code, password) {
                        Ok(true) => {
                            return Ok(json!({
                                "ok": true,
                                "vendor": vendor,
                                "mode": "ui_automation",
                                "client": client,
                                "code_copied": clipboard_ready,
                                "label": label,
                            }));
                        }
                        Ok(false) => None,
                        Err(error) => Some(error.to_string()),
                    };
                return Ok(json!({
                    "ok": true,
                    "vendor": vendor,
                    "mode": "client_opened",
                    "client": client,
                    "code_copied": clipboard_ready,
                    "label": label,
                    "automation_error": automation_error,
                }));
            }
            Err(error) => last_error = format!("{client}: {error}"),
        }
    }

    Err(remote_connect_error(&format!(
        "remote client not found; configure HIMIND_{}_CLI or install the client ({last_error})",
        remote_env_prefix(&vendor)?
    )))
}

fn launch_remote_connection_from_env(
    vendor: &str,
    code: &str,
    password: &str,
    label: &str,
) -> Result<Option<Value>, Box<dyn Error>> {
    let prefix = remote_env_prefix(vendor)?;
    let cli_var = format!("HIMIND_{prefix}_CLI");
    let args_var = format!("HIMIND_{prefix}_ARGS");
    let cli = match env::var(&cli_var) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => return Ok(None),
    };
    let args_template = env::var(&args_var).unwrap_or_default();
    let args = render_remote_args(&args_template, code, password, label);
    Command::new(cli.trim()).args(&args).spawn()?;
    Ok(Some(json!({
        "ok": true,
        "vendor": vendor,
        "mode": "cli_template",
        "cli_var": cli_var,
        "args_var": args_var,
        "label": label,
    })))
}

fn render_remote_args(template: &str, code: &str, password: &str, label: &str) -> Vec<String> {
    template
        .split_whitespace()
        .map(|item| {
            item.replace("{code}", code)
                .replace("{password}", password)
                .replace("{label}", label)
        })
        .filter(|item| !item.is_empty())
        .collect()
}

fn remote_client_candidates(vendor: &str) -> Result<Vec<String>, Box<dyn Error>> {
    if vendor.contains("todesk") {
        return Ok(vec![
            "ToDesk.exe".to_string(),
            "C:\\Program Files\\ToDesk\\ToDesk.exe".to_string(),
            "C:\\Program Files (x86)\\ToDesk\\ToDesk.exe".to_string(),
        ]);
    }
    if vendor.contains("sunlogin") || vendor.contains("向日葵") || vendor.contains("oray") {
        return Ok(vec![
            "SunloginClient.exe".to_string(),
            "C:\\Program Files\\Oray\\SunLogin\\SunloginClient\\SunloginClient.exe".to_string(),
            "C:\\Program Files (x86)\\Oray\\SunLogin\\SunloginClient\\SunloginClient.exe"
                .to_string(),
        ]);
    }
    Err(remote_connect_error("unsupported remote vendor"))
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
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    Ok(())
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

    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName Microsoft.VisualBasic

$pidValue = [int]$env:HIMIND_REMOTE_TARGET_PID
$code = $env:HIMIND_REMOTE_CODE
$password = $env:HIMIND_REMOTE_PASSWORD
$deadline = (Get-Date).AddSeconds(12)
$process = $null

do {
    $process = Get-Process -Id $pidValue -ErrorAction SilentlyContinue
    if ($null -ne $process -and $process.MainWindowHandle -ne 0) { break }
    Start-Sleep -Milliseconds 250
} while ((Get-Date) -lt $deadline)

if ($null -eq $process -or $process.MainWindowHandle -eq 0) {
    exit 2
}

if (-not [Microsoft.VisualBasic.Interaction]::AppActivate($pidValue)) {
    exit 3
}

Start-Sleep -Milliseconds 500
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
        .env("HIMIND_REMOTE_CODE", code)
        .env("HIMIND_REMOTE_PASSWORD", password)
        .status()?;
    Ok(status.success())
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
            "http://localhost:18081/api/agent-package/latest/download"
        )
        .is_ok());
    }

    #[test]
    fn rejects_cross_origin_update_url() {
        assert!(validate_update_download_url(
            "http://localhost:18081",
            "https://downloads.example/agent.exe"
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
    fn validates_sha256_format() {
        assert!(validate_sha256(&"a".repeat(64)).is_ok());
        assert!(validate_sha256("").is_err());
        assert!(validate_sha256(&"z".repeat(64)).is_err());
        assert!(validate_sha256(&"a".repeat(63)).is_err());
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
