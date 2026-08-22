mod install_layout;

use reqwest::blocking::Client;
use serde::Deserialize;
use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Deserialize)]
struct UpdateArgs {
    current_executable: PathBuf,
    staged_executable: PathBuf,
    staged_package: PathBuf,
    staged_updater: PathBuf,
    staged_launcher: PathBuf,
    #[serde(default)]
    staged_vscode_extension: Option<PathBuf>,
    api_base: String,
    from_version: String,
    target_version: String,
    local_port: u16,
    state_path: PathBuf,
    #[serde(default)]
    old_pid: u32,
    #[serde(default)]
    wait_pid: u32,
    arguments: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct UpdaterReplacementArgs {
    operation: String,
    parent_pid: u32,
    staged_package: PathBuf,
    staged_directory: PathBuf,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("agent updater failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let raw = env::args().nth(1).ok_or("update arguments are required")?;
    let value = serde_json::from_str::<serde_json::Value>(&raw)?;
    if value.get("operation").and_then(|value| value.as_str()) == Some("replace_updater") {
        return replace_running_updater(serde_json::from_value(value)?);
    }
    let args = serde_json::from_value::<UpdateArgs>(value)?;
    let result = run_update(&args);
    if let Err(error) = &result {
        let _ = mark_install_failed_if_pending(&args, &error.to_string());
    }
    result
}

fn run_update(args: &UpdateArgs) -> Result<(), Box<dyn Error>> {
    log_update(
        args,
        &format!(
            "开始更新：{} -> {}，current={}，staged={}",
            args.from_version,
            args.target_version,
            args.current_executable.display(),
            args.staged_executable.display()
        ),
    );
    let current = plain_path(&fs::canonicalize(&args.current_executable)?);
    let staged = plain_path(&fs::canonicalize(&args.staged_executable)?);
    if !staged.is_file()
        || current == staged
        || current.file_name().and_then(|value| value.to_str()) != Some(install_layout::AGENT_FILE)
    {
        return Err("invalid staged Agent executable".into());
    }
    let root = install_layout::installation_root_from_executable(&current);
    if !staged_path_allowed(&staged, &root) {
        return Err("staged Agent executable is outside the installation root".into());
    }
    let staged_updater =
        canonical_staged_helper(&args.staged_updater, install_layout::UPDATER_FILE, &root)?;
    let staged_launcher =
        canonical_staged_helper(&args.staged_launcher, install_layout::LAUNCHER_FILE, &root)?;
    let staged_vscode_extension = args
        .staged_vscode_extension
        .as_ref()
        .map(|path| -> Result<PathBuf, Box<dyn Error>> {
            if path.file_name().and_then(|value| value.to_str()) != Some("himind-ai.vsix") {
                return Err("invalid staged VS Code extension".into());
            }
            let canonical = canonical_staged_path(path, &root)?;
            let size = fs::metadata(&canonical)?.len();
            if size == 0 || size > 100 * 1024 * 1024 {
                return Err("staged VS Code extension has an invalid size".into());
            }
            Ok(canonical)
        })
        .transpose()?;
    let staged_package = canonical_staged_path(&args.staged_package, &root)?;
    wait_for_old_agent_exit(args)?;
    let had_active_version = install_layout::read_active_version(&root)?.is_some();
    let target_dir =
        install_layout::prepare_version_directory(&root, &args.target_version, &staged)?;
    let target = target_dir.join(install_layout::AGENT_FILE);
    let _ = fs::remove_file(&staged);
    thread::sleep(Duration::from_millis(300));
    if launch_and_confirm(&args, &target, &args.target_version) {
        log_update(
            args,
            &format!("新 Agent 健康检查通过：{}", target.display()),
        );
        if let Some(extension) = staged_vscode_extension.as_deref() {
            if let Err(error) = install_vscode_extension(&root, extension) {
                let _ = terminate_process_for_path(&target);
                let _ = fs::remove_dir_all(&target_dir);
                let _ = launch_and_confirm(args, &current, &args.from_version);
                return Err(format!("更新 VS Code 扩展失败，已回滚 Agent：{error}").into());
            }
        }
        if !had_active_version {
            if let Err(error) = replace_helper(
                &root.join(install_layout::LAUNCHER_FILE),
                &root.join("himind-agent-launcher.previous.exe"),
                &staged_launcher,
            ) {
                let _ = terminate_process_for_path(&target);
                let _ = fs::remove_dir_all(&target_dir);
                let _ = launch_and_confirm(args, &current, &args.from_version);
                return Err(format!("首次迁移无法更新稳定 launcher：{error}").into());
            }
        }
        if let Err(error) = install_layout::write_active_version(&root, &args.target_version) {
            let _ = terminate_process_for_path(&target);
            let _ = fs::remove_dir_all(&target_dir);
            let _ = launch_and_confirm(args, &current, &args.from_version);
            return Err(format!("写入 Agent 活动版本指针失败：{error}").into());
        }
        let _ = fs::remove_file(&staged_launcher);
        if let Err(error) = schedule_updater_self_replace(&root, &staged_updater, &staged_package) {
            let detail = format!("Agent 已更新，但 updater 将在后续更新中重试：{error}");
            let _ = update_local_status(args, "idle", "");
            let _ = report_update_result(args, "update_success", &detail);
            return Ok(());
        }
        let _ = update_local_status(args, "idle", "");
        let _ = report_update_result(args, "update_success", "");
        return Ok(());
    }
    log_update(
        args,
        "新 Agent 健康检查失败，开始保留当前版本并恢复旧 Agent",
    );
    let _ = fs::remove_dir_all(&target_dir);
    let _ = launch_and_confirm(args, &current, &args.from_version);
    let _ = report_update_result(
        args,
        "rolled_back",
        "new Agent failed health check; previous version kept",
    );
    let _ = update_local_status(args, "rolled_back", "新版本启动检查失败，已保留当前版本");
    Err("new Agent failed health check; previous version was kept".into())
}

fn mark_install_failed_if_pending(args: &UpdateArgs, error: &str) -> Result<(), Box<dyn Error>> {
    let path = args.state_path.with_file_name("agent-update-state.json");
    if !path.is_file() {
        return Ok(());
    }
    let value = read_json_value(&path)?;
    if value.get("status").and_then(|value| value.as_str()) != Some("installing") {
        return Ok(());
    }
    update_local_status(args, "failed", error)
}

fn update_local_status(args: &UpdateArgs, status: &str, error: &str) -> Result<(), Box<dyn Error>> {
    let path = args.state_path.with_file_name("agent-update-state.json");
    if !path.is_file() {
        return Ok(());
    }
    let mut value = read_json_value(&path)?;
    value["status"] = serde_json::Value::String(status.to_string());
    value["last_error"] = serde_json::Value::String(error.to_string());
    value["current_version"] = serde_json::Value::String(if status == "idle" {
        args.target_version.clone()
    } else {
        args.from_version.clone()
    });
    if status == "idle" {
        for key in [
            "available_version",
            "release_id",
            "file_name",
            "sha256",
            "signature",
            "signature_key_id",
            "signature_algorithm",
            "download_url",
            "min_supported_version",
            "release_notes",
            "staged_package_path",
            "staged_agent_path",
            "staged_updater_path",
            "staged_launcher_path",
            "staged_vscode_extension_path",
        ] {
            value[key] = serde_json::Value::String(String::new());
        }
        value["size_bytes"] = serde_json::json!(0);
        value["downloaded_bytes"] = serde_json::json!(0);
        value["progress_percent"] = serde_json::json!(0);
        value["mandatory"] = serde_json::json!(false);
        value["package_type"] = serde_json::Value::String("directory-zip".to_string());
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn read_json_value(path: &Path) -> Result<serde_json::Value, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    let bytes = bytes
        .strip_prefix(&[0xEF, 0xBB, 0xBF])
        .unwrap_or(bytes.as_slice());
    Ok(serde_json::from_slice(bytes)?)
}

fn launch_and_confirm(args: &UpdateArgs, executable: &Path, expected_version: &str) -> bool {
    let root = install_layout::installation_root_from_executable(executable);
    let trusted_keys = root.join("trusted-keys");
    let mut command = Command::new(executable);
    command
        .args(&args.arguments)
        .current_dir(&root)
        .env("HIMIND_REQUIRE_SIGNED_UPDATES", "true");
    if trusted_keys.is_dir() {
        command.env("HIMIND_TRUSTED_SIGNING_KEYS_DIR", trusted_keys);
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            log_update(
                args,
                &format!("启动 Agent 失败：{}：{}", executable.display(), error),
            );
            return false;
        }
    };
    if wait_for_health(
        args.local_port,
        &args.api_base,
        &args.state_path,
        expected_version,
        executable,
        child.id(),
    ) {
        return true;
    }
    log_update(
        args,
        &format!(
            "Agent 未通过健康检查：path={}，expected_version={}，pid={}",
            executable.display(),
            expected_version,
            child.id()
        ),
    );
    let _ = terminate_single(child.id());
    false
}

fn wait_for_old_agent_exit(args: &UpdateArgs) -> Result<(), Box<dyn Error>> {
    let pid = if args.wait_pid != 0 {
        args.wait_pid
    } else {
        args.old_pid
    };
    if pid == 0 || pid == std::process::id() || !process_exists(pid) {
        return Ok(());
    }
    log_update(args, &format!("等待旧 Agent 退出：pid={pid}"));
    if wait_for_process_exit(pid, Duration::from_secs(10)) {
        return Ok(());
    }
    log_update(args, &format!("旧 Agent 未自行退出，终止主进程：pid={pid}"));
    terminate_single(pid)?;
    if wait_for_process_exit(pid, Duration::from_secs(5)) {
        Ok(())
    } else {
        Err(format!("旧 Agent 进程未能退出：pid={pid}").into())
    }
}

fn report_update_result(
    args: &UpdateArgs,
    report_type: &str,
    detail: &str,
) -> Result<(), Box<dyn Error>> {
    #[derive(Deserialize)]
    struct DistributionState {
        #[serde(default)]
        token: String,
        #[serde(default)]
        token_protected: String,
    }
    let state_path = args
        .state_path
        .with_file_name("agent-state.distribution.json");
    if !state_path.is_file() {
        return Ok(());
    }
    let mut state = serde_json::from_value::<DistributionState>(read_json_value(&state_path)?)?;
    if state.token.trim().is_empty() && !state.token_protected.trim().is_empty() {
        state.token = unprotect_secret_for_current_user(&state.token_protected)?;
    }
    if state.token.trim().is_empty() {
        return Ok(());
    }
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .post(format!(
            "{}/api/distribution/client/update-result",
            args.api_base.trim_end_matches('/')
        ))
        .bearer_auth(state.token)
        .json(&serde_json::json!({
            "report_type": report_type,
            "from_version": args.from_version,
            "to_version": args.target_version,
            "detail": detail,
        }))
        .send()?
        .error_for_status()?;
    Ok(())
}

fn unprotect_secret_for_current_user(secret: &str) -> Result<String, Box<dyn Error>> {
    let script = r#"$encrypted = [Console]::In.ReadToEnd(); $secure = ConvertTo-SecureString $encrypted; $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure); try { [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr) } finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }"#;
    let mut last_error = String::new();
    for shell in ["pwsh", "powershell"] {
        let mut child = match Command::new(shell)
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                last_error = error.to_string();
                continue;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(secret.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
        last_error = String::from_utf8_lossy(&output.stderr).trim().to_string();
    }
    Err(format!("failed to access local credential store: {last_error}").into())
}

fn canonical_staged_helper(
    path: &Path,
    expected_name: &str,
    installation_root: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    if path.file_name().and_then(|value| value.to_str()) != Some(expected_name) {
        return Err(format!("invalid staged helper executable: {expected_name}").into());
    }
    canonical_staged_path(path, installation_root)
}

fn canonical_staged_path(path: &Path, installation_root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let canonical = plain_path(&fs::canonicalize(path)?);
    if !canonical.is_file() || !staged_path_allowed(&canonical, installation_root) {
        return Err("staged update file is outside the installation root".into());
    }
    Ok(canonical)
}

fn install_vscode_extension(root: &Path, staged: &Path) -> Result<(), Box<dyn Error>> {
    let target_dir = root.join("resources").join("vscode");
    fs::create_dir_all(&target_dir)?;
    let temporary = target_dir.join(format!(".himind-ai-{}.vsix.installing", std::process::id()));
    let target = target_dir.join("himind-ai.vsix");
    let _ = fs::remove_file(&temporary);
    fs::copy(staged, &temporary)?;
    if let Err(error) = install_layout::replace_file(&temporary, &target) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("无法更新内置 HiMind VSIX：{error}").into());
    }
    Ok(())
}

// fs::canonicalize returns \\?\-prefixed verbatim paths, but Win32_Process
// ExecutablePath and PowerShell cmdlets use plain Win32 paths. Normalize
// canonicalized paths so process matching and helper scripts keep working.
fn plain_path(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_string());
    }
    path.to_path_buf()
}

fn replace_helper(target: &Path, backup: &Path, staged: &Path) -> Result<(), Box<dyn Error>> {
    let next = target.with_extension("next.exe");
    let _ = fs::remove_file(&next);
    fs::copy(staged, &next)?;
    if target.is_file() {
        let _ = fs::remove_file(backup);
        fs::copy(target, backup)?;
        if let Err(error) = fs::remove_file(target) {
            let _ = fs::remove_file(&next);
            return Err(error.into());
        }
    }
    if let Err(error) = fs::rename(&next, target) {
        if backup.is_file() {
            let _ = fs::copy(backup, target);
        }
        let _ = fs::remove_file(&next);
        return Err(error.into());
    }
    Ok(())
}

fn schedule_updater_self_replace(
    root: &Path,
    staged_updater: &Path,
    staged_package: &Path,
) -> Result<(), Box<dyn Error>> {
    let target = root.join("himind-agent-updater.exe");
    let next = root.join("himind-agent-updater.next.exe");
    let _ = fs::remove_file(&next);
    fs::copy(staged_updater, &next)?;
    let payload = serde_json::json!({
        "operation": "replace_updater",
        "parent_pid": std::process::id(),
        "staged_package": staged_package,
        "staged_directory": staged_updater.parent().unwrap_or(root),
    });
    Command::new(&next)
        .arg(payload.to_string())
        .current_dir(root)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    if !target.is_file() {
        return Err("installed Agent updater is missing".into());
    }
    Ok(())
}

fn replace_running_updater(args: UpdaterReplacementArgs) -> Result<(), Box<dyn Error>> {
    if args.operation != "replace_updater" || args.parent_pid == 0 {
        return Err("invalid updater replacement request".into());
    }
    let current = plain_path(&fs::canonicalize(env::current_exe()?)?);
    if current.file_name().and_then(|value| value.to_str()) != Some("himind-agent-updater.next.exe")
    {
        return Err("updater replacement must run from the staged helper".into());
    }
    let root = current
        .parent()
        .ok_or("updater installation root is unavailable")?;
    let target = root.join(install_layout::UPDATER_FILE);
    let backup = root.join("himind-agent-updater.previous.exe");
    if !wait_for_process_exit(args.parent_pid, Duration::from_secs(30)) {
        return Err("timed out waiting for the previous updater to exit".into());
    }
    let temporary = root.join("himind-agent-updater.replacing.exe");
    let _ = fs::remove_file(&temporary);
    fs::copy(&current, &temporary)?;
    if target.is_file() {
        let _ = fs::remove_file(&backup);
        fs::copy(&target, &backup)?;
    }
    install_layout::replace_file(&temporary, &target)?;
    let _ = fs::remove_file(&args.staged_package);
    if args.staged_directory.starts_with(root.join("staging")) {
        let _ = fs::remove_dir_all(&args.staged_directory);
    }
    Ok(())
}

fn powershell_escape_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

fn staged_path_allowed(staged: &Path, installation_root: &Path) -> bool {
    let canonical_root =
        fs::canonicalize(installation_root).unwrap_or_else(|_| installation_root.to_path_buf());
    let root = plain_path(&canonical_root);
    let staged = plain_path(staged);
    staged.starts_with(&root)
}

fn wait_for_health(
    port: u16,
    api_base: &str,
    state_path: &Path,
    target_version: &str,
    executable: &Path,
    pid: u32,
) -> bool {
    let client = match Client::builder().timeout(Duration::from_secs(2)).build() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last_observation = "尚未收到本地服务响应".to_string();
    while Instant::now() < deadline {
        match client.get(format!("http://127.0.0.1:{port}/health")).send() {
            Ok(response) => {
                if !response.status().is_success() {
                    last_observation = format!("/health 返回 HTTP {}", response.status());
                } else {
                    match response.json::<serde_json::Value>() {
                        Ok(payload) => {
                            last_observation = health_mismatch_reason(
                                &payload,
                                target_version,
                                executable,
                                state_path,
                            );
                            if last_observation == "ok" {
                                return true;
                            }
                        }
                        Err(error) => {
                            last_observation = format!("/health 响应不是合法 JSON：{error}");
                        }
                    }
                }
            }
            Err(error) => {
                last_observation = format!("连接本地 /health 失败：{error}");
            }
        }
        if !executable.exists() {
            append_update_log(state_path, "健康检查中止：目标 Agent 可执行文件已不存在");
            return false;
        }
        thread::sleep(Duration::from_millis(500));
    }
    append_update_log(
        state_path,
        &format!(
            "健康检查超时：port={}，expected_version={}，pid={}，最后观测={}",
            port, target_version, pid, last_observation
        ),
    );
    let _ = api_base;
    false
}

fn health_matches_update(
    payload: &serde_json::Value,
    target_version: &str,
    executable: &Path,
    state_path: &Path,
) -> bool {
    health_mismatch_reason(payload, target_version, executable, state_path) == "ok"
}

fn health_mismatch_reason(
    payload: &serde_json::Value,
    target_version: &str,
    executable: &Path,
    state_path: &Path,
) -> String {
    if target_version.trim().is_empty() {
        return "目标版本为空".to_string();
    }
    if payload.get("status").and_then(|value| value.as_str()) != Some("online") {
        return format!(
            "Agent 状态不是 online：{}",
            payload
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("缺失")
        );
    }
    if payload.get("version").and_then(|value| value.as_str()) != Some(target_version) {
        return format!(
            "Agent 版本不一致：实际={}，期望={}",
            payload
                .get("version")
                .and_then(|value| value.as_str())
                .unwrap_or("缺失"),
            target_version
        );
    }
    if !state_path.is_file() {
        return format!("Agent 状态文件不存在：{}", state_path.display());
    }
    let Some(reported_path) = payload
        .get("executable_path")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    else {
        return "健康信息缺少 executable_path".to_string();
    };
    match (
        fs::canonicalize(executable),
        fs::canonicalize(Path::new(reported_path)),
    ) {
        (Ok(expected), Ok(actual)) if plain_path(&expected) == plain_path(&actual) => {
            "ok".to_string()
        }
        (Ok(expected), Ok(actual)) => format!(
            "Agent 执行路径不一致：实际={}，期望={}",
            plain_path(&actual).display(),
            plain_path(&expected).display()
        ),
        (Err(error), _) => format!("无法解析目标 Agent 路径：{error}"),
        (_, Err(error)) => format!("无法解析健康信息中的执行路径：{error}"),
    }
}

fn log_update(context: &UpdateArgs, message: &str) {
    append_update_log(&context.state_path, message);
}

fn append_update_log(state_path: &Path, message: &str) {
    let Some(root) = state_path.parent().and_then(Path::parent) else {
        return;
    };
    let log_path = root.join("logs").join("agent-updater.log");
    let Some(parent) = log_path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default();
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path) {
        let _ = writeln!(file, "{timestamp} {message}");
    }
}

fn running_agent_pids(current_executable: &Path) -> Vec<u32> {
    let target = powershell_escape_single_quoted(&current_executable.to_string_lossy());
    let script = format!(
        "$target='{}'; Get-CimInstance Win32_Process -Filter \"Name='himind-agent.exe'\" | Where-Object {{ $_.ExecutablePath -and $_.ExecutablePath.Equals($target, [System.StringComparison]::OrdinalIgnoreCase) }} | ForEach-Object {{ $_.ProcessId }}",
        target
    );
    for shell in ["pwsh", "powershell"] {
        let Ok(output) = Command::new(shell)
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        return String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .filter(|pid| *pid != std::process::id())
            .collect();
    }
    Vec::new()
}

fn process_exists(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
        .unwrap_or(false)
}

fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(200));
    }
    !process_exists(pid)
}

fn terminate_single(pid: u32) -> Result<(), Box<dyn Error>> {
    Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output()?;
    Ok(())
}

fn terminate_process_for_path(executable: &Path) -> Result<(), Box<dyn Error>> {
    for pid in running_agent_pids(executable) {
        let _ = terminate_single(pid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::install_layout::{
        prepare_version_directory, read_active_version, write_active_version,
    };
    use super::{
        canonical_staged_helper, health_matches_update, plain_path, read_json_value,
        replace_helper, staged_path_allowed,
    };
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn installs_version_side_by_side_and_switches_pointer() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-test-{unique}"));
        fs::create_dir_all(root.join("current")).unwrap();
        fs::write(root.join("current/himind-agent.exe"), b"old").unwrap();
        let staged = root.join("staging-agent.exe");
        fs::write(&staged, b"new").unwrap();

        let version_dir = prepare_version_directory(&root, "0.4.0", &staged).unwrap();
        write_active_version(&root, "0.4.0").unwrap();
        assert_eq!(
            fs::read(version_dir.join("himind-agent.exe")).unwrap(),
            b"new"
        );
        assert_eq!(
            read_active_version(&root).unwrap().as_deref(),
            Some("0.4.0")
        );
        assert_eq!(
            fs::read(root.join("current/himind-agent.exe")).unwrap(),
            b"old"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_staging_path_outside_installation_root() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let staged_path =
            std::env::temp_dir().join(format!("himind-agent-update-path-test-{unique}.exe"));
        fs::write(&staged_path, b"agent").unwrap();
        let staged = fs::canonicalize(&staged_path).unwrap();
        let unrelated_root = staged
            .parent()
            .unwrap()
            .join(format!("himind-agent-installation-{unique}"));

        assert!(!staged_path_allowed(&staged, &unrelated_root));
        let _ = fs::remove_file(staged_path);
    }

    #[test]
    fn plain_path_strips_verbatim_prefixes() {
        use super::plain_path;
        assert_eq!(
            plain_path(Path::new(r"\\?\C:\Program Files\HiMind\agent.exe")),
            PathBuf::from(r"C:\Program Files\HiMind\agent.exe")
        );
        assert_eq!(
            plain_path(Path::new(r"\\?\UNC\server\share\agent.exe")),
            PathBuf::from(r"\\server\share\agent.exe")
        );
        assert_eq!(
            plain_path(Path::new(r"C:\Program Files\HiMind\agent.exe")),
            PathBuf::from(r"C:\Program Files\HiMind\agent.exe")
        );
    }

    #[test]
    fn reads_utf8_bom_json() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("himind-updater-bom-{unique}.json"));
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend(br#"{"status":"installing"}"#);
        fs::write(&path, bytes).unwrap();

        let value = read_json_value(&path).unwrap();
        assert_eq!(
            value.get("status").and_then(|v| v.as_str()),
            Some("installing")
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn health_confirmation_requires_target_version_and_executable() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-health-test-{unique}"));
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("himind-agent.exe");
        let state = root.join("agent-state.json");
        fs::write(&executable, b"agent").unwrap();
        fs::write(&state, b"{}").unwrap();
        let payload = json!({
            "status": "online",
            "dashboard_worker_online": false,
            "version": "0.3.0",
            "executable_path": executable,
        });
        assert!(health_matches_update(
            &payload,
            "0.3.0",
            &executable,
            &state
        ));
        assert!(!health_matches_update(
            &payload,
            "0.3.1",
            &executable,
            &state
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn validates_and_replaces_staged_helper() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-helper-test-{unique}"));
        let staged_dir = root.join("staging");
        fs::create_dir_all(&staged_dir).unwrap();
        let staged = staged_dir.join("himind-agent-launcher.exe");
        let wrong_name = staged_dir.join("launcher.exe");
        fs::write(&staged, b"new launcher").unwrap();
        fs::write(&wrong_name, b"wrong").unwrap();

        let validated =
            canonical_staged_helper(&staged, "himind-agent-launcher.exe", &root).unwrap();
        assert_eq!(validated, plain_path(&fs::canonicalize(&staged).unwrap()));
        assert!(canonical_staged_helper(&wrong_name, "himind-agent-launcher.exe", &root).is_err());

        let target = root.join("himind-agent-launcher.exe");
        let backup = root.join("himind-agent-launcher.previous.exe");
        fs::write(&target, b"old launcher").unwrap();
        replace_helper(&target, &backup, &staged).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new launcher");
        assert_eq!(fs::read(&backup).unwrap(), b"old launcher");
        let _ = fs::remove_dir_all(root);
    }
}
