use reqwest::blocking::Client;
use serde::Deserialize;
use std::env;
use std::error::Error;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Deserialize)]
struct UpdateArgs {
    current_executable: PathBuf,
    staged_executable: PathBuf,
    staged_package: PathBuf,
    staged_updater: PathBuf,
    staged_launcher: PathBuf,
    api_base: String,
    from_version: String,
    target_version: String,
    local_port: u16,
    state_path: PathBuf,
    old_pid: u32,
    arguments: Vec<String>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("agent updater failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let raw = env::args().nth(1).ok_or("update arguments are required")?;
    let args = serde_json::from_str::<UpdateArgs>(&raw)?;
    let result = run_update(&args);
    if let Err(error) = &result {
        let _ = mark_install_failed_if_pending(&args, &error.to_string());
    }
    result
}

fn run_update(args: &UpdateArgs) -> Result<(), Box<dyn Error>> {
    let current = fs::canonicalize(&args.current_executable)?;
    let staged = fs::canonicalize(&args.staged_executable)?;
    if !staged.is_file()
        || current == staged
        || current.file_name().and_then(|value| value.to_str()) != Some("himind-agent.exe")
        || current
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            != Some("current")
    {
        return Err("invalid staged Agent executable".into());
    }
    let root = installation_root(&current);
    if !staged_path_allowed(&staged, &root) {
        return Err("staged Agent executable is outside the installation root".into());
    }
    let staged_updater =
        canonical_staged_helper(&args.staged_updater, "himind-agent-updater.exe", &root)?;
    let staged_launcher =
        canonical_staged_helper(&args.staged_launcher, "himind-agent-launcher.exe", &root)?;
    let staged_package = canonical_staged_path(&args.staged_package, &root)?;
    if !stop_running_agents(&current, args.old_pid) {
        return Err("安装目录下仍有 Agent 进程运行，更新超时".into());
    }
    let current_dir = root.join("current");
    let previous_dir = root.join("previous");
    fs::create_dir_all(&current_dir)?;
    fs::create_dir_all(&previous_dir)?;
    let current_target = current_dir.join("himind-agent.exe");
    let previous_target = previous_dir.join("himind-agent.exe");
    rotate_version(&current_target, &previous_target, &staged)?;
    let _ = fs::remove_file(&staged);
    thread::sleep(Duration::from_millis(1200));
    if launch_and_confirm(&args, &current_target, &args.target_version) {
        if let Err(error) =
            update_helpers_after_health(&root, &staged_updater, &staged_launcher, &staged_package)
        {
            let detail = format!("Agent 已更新，但辅助程序将在后续更新中重试：{error}");
            let _ = update_local_status(args, "idle", "");
            let _ = report_update_result(args, "update_success", &detail);
            return Ok(());
        }
        let _ = update_local_status(args, "idle", "");
        let _ = report_update_result(args, "update_success", "");
        return Ok(());
    }
    if previous_target.exists() {
        restore_previous(&current_target, &previous_target)?;
        if launch_and_confirm(&args, &current_target, &args.from_version) {
            let _ = report_update_result(
                &args,
                "rolled_back",
                "new Agent failed health check; previous restored",
            );
            let _ = update_local_status(&args, "rolled_back", "新版本启动检查失败，已恢复上一版本");
            return Err("new Agent failed health check and was rolled back".into());
        }
    }
    let _ = report_update_result(
        &args,
        "update_failed",
        "new Agent failed health check and rollback health check failed",
    );
    let _ = update_local_status(&args, "failed", "新版本与回滚版本均未通过启动检查");
    Err("Agent update failed and previous version could not be confirmed".into())
}

fn mark_install_failed_if_pending(args: &UpdateArgs, error: &str) -> Result<(), Box<dyn Error>> {
    let path = args.state_path.with_file_name("agent-update-state.json");
    if !path.is_file() {
        return Ok(());
    }
    let value = serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&path)?)?;
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
    let mut value = serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&path)?)?;
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

fn rotate_version(current: &Path, previous: &Path, staged: &Path) -> Result<(), Box<dyn Error>> {
    let backup = previous.with_extension("staging");
    if current.exists() {
        let _ = fs::remove_file(&backup);
        let _ = fs::remove_file(previous);
        fs::copy(current, &backup)?;
        fs::rename(&backup, previous)?;
    }
    fs::copy(staged, current)?;
    Ok(())
}

fn restore_previous(current: &Path, previous: &Path) -> Result<(), Box<dyn Error>> {
    fs::copy(previous, current)?;
    Ok(())
}

fn launch_and_confirm(args: &UpdateArgs, executable: &Path, expected_version: &str) -> bool {
    let child = Command::new(executable)
        .args(&args.arguments)
        .current_dir(executable.parent().unwrap_or(Path::new(".")))
        .spawn();
    let Ok(child) = child else {
        return false;
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
    let _ = terminate(child.id());
    false
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
    let mut state = serde_json::from_str::<DistributionState>(&fs::read_to_string(state_path)?)?;
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
    let canonical = fs::canonicalize(path)?;
    if !canonical.is_file() || !staged_path_allowed(&canonical, installation_root) {
        return Err("staged update file is outside the installation root".into());
    }
    Ok(canonical)
}

fn update_helpers_after_health(
    root: &Path,
    staged_updater: &Path,
    staged_launcher: &Path,
    staged_package: &Path,
) -> Result<(), Box<dyn Error>> {
    let launcher = root.join("himind-agent-launcher.exe");
    let launcher_backup = root.join("himind-agent-launcher.previous.exe");
    let launcher_existed = launcher.is_file();
    let mut errors = Vec::new();
    match replace_helper(&launcher, &launcher_backup, staged_launcher) {
        Ok(()) => {
            let _ = fs::remove_file(staged_launcher);
        }
        Err(error) => errors.push(format!("launcher update failed: {error}")),
    }

    if let Err(error) = schedule_updater_self_replace(root, staged_updater, staged_package) {
        errors.push(format!("updater update failed: {error}"));
    }
    if errors.is_empty() {
        return Ok(());
    }
    if errors
        .iter()
        .any(|error| error.contains("launcher update failed"))
    {
        if launcher_existed && launcher_backup.is_file() {
            let _ = fs::copy(&launcher_backup, &launcher);
        } else {
            let _ = fs::remove_file(&launcher);
        }
    }
    Err(errors.join("; ").into())
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
    let backup = root.join("himind-agent-updater.previous.exe");
    let next = root.join("himind-agent-updater.next.exe");
    let updater_pid = std::process::id();
    let cleanup_artifact = format!(
        "; Remove-Item -LiteralPath '{}' -Force -ErrorAction SilentlyContinue",
        powershell_escape_single_quoted(&staged_package.to_string_lossy())
    );
    let cleanup_directory = staged_updater
        .parent()
        .map(|path| {
            format!(
                "; Remove-Item -LiteralPath '{}' -Recurse -Force -ErrorAction SilentlyContinue",
                powershell_escape_single_quoted(&path.to_string_lossy())
            )
        })
        .unwrap_or_default();
    let script = format!(
        "$ErrorActionPreference='Stop'; Copy-Item -LiteralPath '{}' -Destination '{}' -Force; if (Test-Path -LiteralPath '{}') {{ Copy-Item -LiteralPath '{}' -Destination '{}' -Force }}; $running = Get-Process -Id {} -ErrorAction SilentlyContinue; if ($running) {{ Wait-Process -Id {} -Timeout 30 }}; try {{ Remove-Item -LiteralPath '{}' -Force -ErrorAction SilentlyContinue; Move-Item -LiteralPath '{}' -Destination '{}' -Force{}{} }} catch {{ if (Test-Path -LiteralPath '{}') {{ Copy-Item -LiteralPath '{}' -Destination '{}' -Force }}; throw }}",
        powershell_escape_single_quoted(&staged_updater.to_string_lossy()),
        powershell_escape_single_quoted(&next.to_string_lossy()),
        powershell_escape_single_quoted(&target.to_string_lossy()),
        powershell_escape_single_quoted(&target.to_string_lossy()),
        powershell_escape_single_quoted(&backup.to_string_lossy()),
        updater_pid,
        updater_pid,
        powershell_escape_single_quoted(&target.to_string_lossy()),
        powershell_escape_single_quoted(&next.to_string_lossy()),
        powershell_escape_single_quoted(&target.to_string_lossy()),
        cleanup_artifact,
        cleanup_directory,
        powershell_escape_single_quoted(&backup.to_string_lossy()),
        powershell_escape_single_quoted(&backup.to_string_lossy()),
        powershell_escape_single_quoted(&target.to_string_lossy()),
    );
    for shell in ["pwsh", "powershell"] {
        if Command::new(shell)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &script,
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
    }
    Err("failed to schedule Agent updater self replacement".into())
}

fn powershell_escape_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

fn installation_root(executable: &Path) -> PathBuf {
    executable
        .parent()
        .and_then(Path::parent)
        .unwrap_or(executable)
        .to_path_buf()
}

fn staged_path_allowed(staged: &Path, installation_root: &Path) -> bool {
    let canonical_root =
        fs::canonicalize(installation_root).unwrap_or_else(|_| installation_root.to_path_buf());
    staged.starts_with(&canonical_root)
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
    while Instant::now() < deadline {
        if let Ok(response) = client.get(format!("http://127.0.0.1:{port}/health")).send() {
            if response.status().is_success() {
                if let Ok(payload) = response.json::<serde_json::Value>() {
                    if health_matches_update(&payload, target_version, executable, state_path) {
                        return true;
                    }
                }
            }
        }
        if !executable.exists() {
            return false;
        }
        thread::sleep(Duration::from_millis(500));
    }
    let _ = api_base;
    let _ = pid;
    false
}

fn health_matches_update(
    payload: &serde_json::Value,
    target_version: &str,
    executable: &Path,
    state_path: &Path,
) -> bool {
    if target_version.trim().is_empty()
        || payload.get("status").and_then(|value| value.as_str()) != Some("online")
        || payload.get("version").and_then(|value| value.as_str()) != Some(target_version)
        || !state_path.is_file()
    {
        return false;
    }
    let Some(reported_path) = payload
        .get("executable_path")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    else {
        return false;
    };
    match (
        fs::canonicalize(executable),
        fs::canonicalize(Path::new(reported_path)),
    ) {
        (Ok(expected), Ok(actual)) => expected == actual,
        _ => false,
    }
}

fn stop_running_agents(current_executable: &Path, old_pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let mut pids = running_agent_pids(current_executable);
        if old_pid != 0 && process_exists(old_pid) && !pids.contains(&old_pid) {
            pids.push(old_pid);
        }
        if pids.is_empty() {
            return true;
        }
        for pid in pids {
            let _ = terminate(pid);
        }
        thread::sleep(Duration::from_millis(200));
    }
    false
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

fn terminate(pid: u32) -> Result<(), Box<dyn Error>> {
    Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_staged_helper, health_matches_update, replace_helper, restore_previous,
        rotate_version, staged_path_allowed,
    };
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn rotates_and_restores_agent_versions() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("himind-updater-test-{unique}"));
        fs::create_dir_all(&root).unwrap();
        let current = root.join("current.exe");
        let previous = root.join("previous.exe");
        let staged = root.join("staged.exe");
        fs::write(&current, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();

        rotate_version(&current, &previous, &staged).unwrap();
        assert_eq!(fs::read(&current).unwrap(), b"new");
        assert_eq!(fs::read(&previous).unwrap(), b"old");

        restore_previous(&current, &previous).unwrap();
        assert_eq!(fs::read(&current).unwrap(), b"old");
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
        assert_eq!(validated, fs::canonicalize(&staged).unwrap());
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
