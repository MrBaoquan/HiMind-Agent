use reqwest::blocking::Client;
use reqwest::Url;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::app::types::{LocalAgentUpdateRequest, RemoteConnectRequest};
use crate::store::credentials::{configured_unity_editor_path, unity_editor_environment_path};
use crate::Options;

const CREATE_NO_WINDOW: u32 = 0x08000000;
const AUTO_START_REG_PATH: &str = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Run";
const AUTO_START_VALUE: &str = "ProjectDashboardAgent";

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
    let staged_file = download_agent_package(&download_url, expected_sha256)?;
    schedule_agent_replace_and_restart(&staged_file, &exe, options)?;

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
) -> Result<PathBuf, Box<dyn Error>> {
    let mut response = Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?
        .get(download_url)
        .send()?;
    response.error_for_status_ref()?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default();
    let staged_path =
        env::temp_dir().join(format!("project-dashboard-agent-update-{timestamp}.exe"));
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

fn schedule_agent_replace_and_restart(
    staged_executable: &Path,
    current_executable: &Path,
    options: &Options,
) -> Result<(), Box<dyn Error>> {
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
        "Start-Sleep -Milliseconds 1200; Copy-Item -Force '{}' '{}'; Start-Process -FilePath '{}' -ArgumentList @({}) -WorkingDirectory '{}' -WindowStyle Hidden; Remove-Item -Force '{}' -ErrorAction SilentlyContinue",
        powershell_escape_single_quoted(&staged_executable.to_string_lossy()),
        powershell_escape_single_quoted(&current_executable.to_string_lossy()),
        powershell_escape_single_quoted(&current_executable.to_string_lossy()),
        arg_list.join(", "),
        powershell_escape_single_quoted(&working_dir.to_string_lossy()),
        powershell_escape_single_quoted(&staged_executable.to_string_lossy()),
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
        return Err("无法调度 Agent 静默更新，请确认 pwsh 或 powershell 可用。".into());
    }
    Ok(())
}

fn powershell_escape_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

pub(crate) fn local_agent_executable_metadata() -> Value {
    match env::current_exe() {
        Ok(path) => json!({
            "name": path.file_name().map(|item| item.to_string_lossy().to_string()).unwrap_or_else(|| "project-dashboard-agent.exe".to_string()),
            "path": path.to_string_lossy().to_string(),
        }),
        Err(_) => json!({
            "name": "project-dashboard-agent.exe",
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
    let description = format!("项目看板插件: {title}");
    let script = r#"
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($env:PROJECT_DASHBOARD_SHORTCUT_PATH)
$shortcut.TargetPath = $env:PROJECT_DASHBOARD_SHORTCUT_TARGET
$shortcut.Arguments = $env:PROJECT_DASHBOARD_SHORTCUT_ARGUMENTS
$shortcut.WorkingDirectory = $env:PROJECT_DASHBOARD_SHORTCUT_WORKING_DIRECTORY
$shortcut.Description = $env:PROJECT_DASHBOARD_SHORTCUT_DESCRIPTION
$shortcut.Save()
"#;
    let output = run_hidden_powershell_with_env(
        script,
        &[
            ("PROJECT_DASHBOARD_SHORTCUT_PATH", &shortcut_path),
            ("PROJECT_DASHBOARD_SHORTCUT_TARGET", &target_path),
            ("PROJECT_DASHBOARD_SHORTCUT_ARGUMENTS", &arguments),
            (
                "PROJECT_DASHBOARD_SHORTCUT_WORKING_DIRECTORY",
                &working_directory,
            ),
            ("PROJECT_DASHBOARD_SHORTCUT_DESCRIPTION", &description),
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
        "项目看板插件".to_string()
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
$path = $env:PROJECT_DASHBOARD_AUTO_START_REG_PATH
$name = $env:PROJECT_DASHBOARD_AUTO_START_VALUE_NAME
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
$path = $env:PROJECT_DASHBOARD_AUTO_START_REG_PATH
$name = $env:PROJECT_DASHBOARD_AUTO_START_VALUE_NAME
$command = $env:PROJECT_DASHBOARD_AUTO_START_COMMAND
New-Item -Path $path -Force | Out-Null
New-ItemProperty -Path $path -Name $name -Value $command -PropertyType String -Force | Out-Null
"#,
        &[("PROJECT_DASHBOARD_AUTO_START_COMMAND", launch_command)],
    )?;

    if !output.status.success() {
        return Err(powershell_failure("设置 Agent 开机自启失败", &output).into());
    }

    Ok(())
}

fn remove_auto_start_command() -> Result<(), Box<dyn Error>> {
    let output = run_hidden_powershell(
        r#"
$path = $env:PROJECT_DASHBOARD_AUTO_START_REG_PATH
$name = $env:PROJECT_DASHBOARD_AUTO_START_VALUE_NAME
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
        .env("PROJECT_DASHBOARD_AUTO_START_REG_PATH", AUTO_START_REG_PATH)
        .env("PROJECT_DASHBOARD_AUTO_START_VALUE_NAME", AUTO_START_VALUE);

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

$url = $env:PROJECT_DASHBOARD_CAPTURE_URL
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
        .env("PROJECT_DASHBOARD_CAPTURE_URL", source_url)
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
        "can_open_folder": path_exists,
        "can_open_project": open_project_reason.is_empty(),
        "open_folder_reason": if path_exists { "" } else { "本机工程目录不存在" },
        "open_project_reason": open_project_reason,
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
        let launcher = std::env::var("PROJECT_DASHBOARD_UNREAL_EDITOR")
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
        "remote client not found; configure PROJECT_DASHBOARD_{}_CLI or install the client ({last_error})",
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
    let cli_var = format!("PROJECT_DASHBOARD_{prefix}_CLI");
    let args_var = format!("PROJECT_DASHBOARD_{prefix}_ARGS");
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

$pidValue = [int]$env:PROJECT_DASHBOARD_REMOTE_TARGET_PID
$code = $env:PROJECT_DASHBOARD_REMOTE_CODE
$password = $env:PROJECT_DASHBOARD_REMOTE_PASSWORD
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
        .env(
            "PROJECT_DASHBOARD_REMOTE_TARGET_PID",
            process_id.to_string(),
        )
        .env("PROJECT_DASHBOARD_REMOTE_VENDOR", vendor)
        .env("PROJECT_DASHBOARD_REMOTE_CODE", code)
        .env("PROJECT_DASHBOARD_REMOTE_PASSWORD", password)
        .status()?;
    Ok(status.success())
}

fn remote_connect_error(message: &str) -> Box<dyn Error> {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message).into()
}

#[cfg(test)]
mod tests {
    use super::*;

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
