//! One host-owned recovery surface for UI, MCP and Mission dispatch.
//! Permission inspection never grants access. ACL mutations bind to a fresh
//! fingerprint; all Mission permission choices bind to the current plan.
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceResolution {
    pub workspace: String,
    #[serde(default)]
    pub expected_fingerprint: String,
    pub action: String,
    #[serde(default)]
    pub mission_id: String,
    #[serde(default)]
    pub expected_plan_sha256: String,
}

pub(crate) fn check(workspace: &str) -> Result<Value, Box<dyn Error>> {
    let workspace = permission_workspace(workspace)?;
    #[cfg(not(windows))]
    return Ok(json!({"workspace": workspace, "status": "not_required", "repairable": false,
        "fingerprint": "", "message": "当前平台不需要 Windows 工作区授权", "shell_verified": false}));
    #[cfg(windows)]
    {
        let result = run_permission_tool(&workspace, false)?;
        let observation = &result["observation"];
        let repairable = observation["write_dac"] == true
            && observation["write_owner_win32"] == 5
            && observation["owner_sid"] == observation["current_user_sid"]
            && protected_reason(&workspace).is_none();
        let ready = result["permission_prerequisites_met"] == true;
        let reason = protected_reason(&workspace);
        Ok(json!({
            "workspace": workspace,
            "status": if ready { "permissions_ready" } else { "permission_missing" },
            "fingerprint": fingerprint(observation),
            "repairable": repairable,
            "observation": observation,
            "message": reason.unwrap_or_else(|| if ready {
                "已具备配置沙箱的权限；命令运行能力将在准备时验证".to_string()
            } else if repairable {
                "当前用户缺少配置沙箱标签的权限，可仅为此工作区补充授权".to_string()
            } else {
                "当前目录无法自动完成最小授权，请选择其他工作区或由目录所有者处理".to_string()
            }),
            "shell_verified": false,
        }))
    }
}

pub(crate) fn fingerprint(observation: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(observation).unwrap_or_default()))
}

fn canonical_workspace(workspace: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = Path::new(workspace);
    if !path.is_absolute() || !path.is_dir() {
        return Err("请选择已存在的绝对工作区目录".into());
    }
    let canonical = path.canonicalize()?;
    // Keep the script's ordinary local spelling. Its mutation guards reject
    // UNC, volume roots and reparse traversal before any write.
    #[cfg(windows)]
    let canonical = {
        let text = canonical.to_string_lossy();
        PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text.as_ref()))
    };
    Ok(canonical)
}

fn permission_workspace(workspace: &str) -> Result<PathBuf, Box<dyn Error>> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        for ancestor in Path::new(workspace).ancestors() {
            if !ancestor.as_os_str().is_empty() && fs::symlink_metadata(ancestor)?.file_attributes() & 0x400 != 0 {
                return Err("重解析路径不能用于工作区权限修复，请直接选择真实目录".into());
            }
        }
    }
    canonical_workspace(workspace)
}

fn protected_reason(workspace: &Path) -> Option<String> {
    if workspace.parent().is_none() || workspace.to_string_lossy().starts_with(r"\\") {
        return Some("请选择本地工作区子目录，不能对磁盘根目录或网络共享执行沙箱授权".into());
    }
    crate::runtime::deepseek_harness::interactive_executable().ok()
        .and_then(|executable| super::sandbox_warmup::protected_workspace_reason(&executable, workspace))
}

/// The official grant is authoritative: an already materialized exact grant
/// can work without WRITE_OWNER. Never reject it from the probe alone.
pub(crate) fn prepare(workspace: &str) -> Result<(), Box<dyn Error>> {
    let path = permission_workspace(workspace)?;
    if let Some(reason) = protected_reason(&path) { return Err(reason.into()); }
    let executable = crate::runtime::deepseek_harness::interactive_executable()?;
    match super::sandbox_warmup::warm(&executable, &path) {
        super::sandbox_warmup::WarmupOutcome::Ready(_) => verify_commands(&executable, &path).map(|_| ()),
        super::sandbox_warmup::WarmupOutcome::Skipped(reason) if !cfg!(windows) => { let _ = reason; Ok(()) },
        super::sandbox_warmup::WarmupOutcome::Skipped(reason)
        | super::sandbox_warmup::WarmupOutcome::Failed(reason) => {
            let diagnosis = check(workspace).unwrap_or_else(|error| json!({"workspace": path, "message": error.to_string(), "repairable": false}));
            Err(json!({"code": "runtime_workspace_blocked", "message": "命令环境尚未准备好，专家尚未开始执行", "cause": reason, "diagnosis": diagnosis,
                "choices": ["full_access", "repair", "retry", "files_only", "cancel"], "retryable": false}).to_string().into())
        }
    }
}

pub(crate) fn resolve(input: &WorkspaceResolution) -> Result<Value, Box<dyn Error>> {
    if !matches!(input.action.as_str(), "repair" | "retry" | "files_only" | "full_access") {
        return Err("不支持的命令环境恢复操作".into());
    }
    if matches!(input.action.as_str(), "files_only" | "full_access") && input.mission_id.is_empty() {
        return Err("执行权限选择必须绑定到待恢复的协作任务".into());
    }
    let workspace = canonical_workspace(&input.workspace)?;
    if input.action != "full_access" {
        permission_workspace(&input.workspace)?;
        if let Some(reason) = protected_reason(&workspace) { return Err(reason.into()); }
    }
    let store = crate::mission::MissionStore::open_default()?;
    if !input.mission_id.is_empty() {
        let mission = store.get(&input.mission_id)?;
        if canonical_workspace(&mission.workspace.root)? != workspace || mission.plan_sha256 != input.expected_plan_sha256 {
            return Err("工作区或任务计划已变化，请重新检查后确认".into());
        }
        if mission.status != crate::mission::MissionStatus::Waiting || !store.runtime_blocked(&input.mission_id)? {
            return Err("此任务当前没有待恢复的命令环境阻塞".into());
        }
        if input.action == "full_access" && mission.tasks.iter().any(|task| !crate::workflow::supports_full_access(&task.spec.execution.executor)) {
            return Err("完全访问模式需要所有任务选择 HiMind AI 或 ACP 执行器".into());
        }
    }
    let lock_root = crate::store::paths::agent_home().join("runtime-workspace-locks");
    fs::create_dir_all(&lock_root)?;
    let key = format!("{:x}", Sha256::digest(workspace.to_string_lossy().to_lowercase().as_bytes()));
    let lock = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(lock_root.join(format!("{key}.lock")))?;
    if !fs4::fs_std::FileExt::try_lock_exclusive(&lock)? { return Err("该工作区正在恢复，请稍后重新检查".into()); }
    let diagnosis = if input.action == "full_access" { json!({"workspace":workspace,"permission_mode":"full_access"}) } else { check(&input.workspace)? };
    if input.action != "full_access" && diagnosis["fingerprint"].as_str() != Some(input.expected_fingerprint.as_str()) {
        return Err(json!({"code": "runtime_workspace_changed", "message": "目录权限已变化，请重新检查并确认", "diagnosis": diagnosis}).to_string().into());
    }
    let mut repair = Value::Null;
    if input.action == "repair" {
        if diagnosis["repairable"] != true { return Err("当前状态不适用最小权限修复，请重新检查或选择其他操作".into()); }
        repair = run_permission_tool(&workspace, true)?;
    }
    if !matches!(input.action.as_str(), "files_only" | "full_access") {
        if let Err(error) = prepare(&input.workspace) {
            let failure = json!({"action":input.action,"repair":repair,"error":error.to_string(),"plan_sha256":input.expected_plan_sha256});
            if !input.mission_id.is_empty() {
                store.append_event(&input.mission_id,"runtime_workspace_recovery_failed","",
                    "命令环境验证未通过，任务继续暂停；已完成的权限操作保留恢复记录",failure.clone())?;
            }
            let mut envelope = serde_json::from_str::<Value>(&error.to_string()).unwrap_or_else(|_| json!({"message":error.to_string()}));
            envelope["recovery"] = failure;
            return Err(envelope.to_string().into());
        }
    }
    let mission = if input.mission_id.is_empty() { Value::Null } else {
        serde_json::to_value(store.resolve_runtime_block(&input.mission_id, &input.expected_plan_sha256,
            if input.action == "files_only" { "files_only" } else if input.action == "full_access" { "full_access" } else { "default" }, &json!({"diagnosis": diagnosis, "repair": repair, "action": input.action}))?)?
    };
    crate::approval::manager::append_event_log("info", &format!("工作区命令环境恢复：{}，操作 {}", workspace.display(), input.action));
    Ok(json!({"resolved": true, "workspace": workspace, "mode": if input.action == "files_only" { "files_only" } else if input.action == "full_access" { "full_access" } else { "default" },
        "permission_prepared": matches!(input.action.as_str(), "repair" | "retry"), "shell_verified": matches!(input.action.as_str(), "repair" | "retry"), "repair": repair, "mission": mission}))
}

/// Exercise the real restricted token and Runtime-selected PowerShell,
/// including a piped grandchild, without
/// invoking a model or changing project files. A successful ACL grant alone
/// cannot prove that command execution works on this host.
#[cfg(windows)]
fn verify_commands(executable: &Path, workspace: &Path) -> Result<Value, Box<dyn Error>> {
    let runtime = executable.parent().and_then(Path::parent).ok_or("Runtime 目录不可用")?;
    let node = runtime.join("node.exe");
    let module = runtime.join("node_modules/@deepseek-ai/dsh-sandbox-windows-acl/lib/index.js");
    const PROBE: &str = r#"
const {pathToFileURL} = require('node:url');
(async () => {
  const [module, root, node] = process.argv.slice(1);
  const {join, dirname} = require('node:path');
  const acl = await import(pathToFileURL(module).href);
  const {resolvePwshPath} = await import(pathToFileURL(join(dirname(node),'node_modules/@deepseek-ai/dsh-pwsh-local/lib/index.js')).href);
  const shell = resolvePwshPath();
  const box = new acl.AclSandbox({writableDirs:[root],tempDir:null,writeSid:acl.workspaceWriteSid(root),mode:'workspace-write'});
  await box.init();
  try {
    const result = await box.spawn({command:node,args:['-e',
      `const r=require('node:child_process').spawnSync(process.execPath,['-e','process.stdout.write("himind-pipe-ok")'],{encoding:'utf8'});if(r.error)throw r.error;if(r.status!==0||r.stdout!=="himind-pipe-ok")process.exit(1);console.log("himind-command-probe-ok")`],cwd:root}).wait();
    if(result.exitCode!==0)throw new Error(result.stderr.toString() || 'restricted process exit '+result.exitCode);
    if(!result.stdout.toString().includes('himind-command-probe-ok'))throw new Error('missing command probe evidence');
    const script = `const r=require('node:child_process').spawnSync(process.execPath,['-e','process.stdout.write("himind-shell-pipe-ok")'],{encoding:'utf8'});if(r.error)throw r.error;if(r.status!==0||r.stdout!=="himind-shell-pipe-ok")process.exit(1);console.log("himind-shell-probe-ok")`;
    const literal = value => "'"+value.replaceAll("'","''")+"'";
    const shellResult = await box.spawn({command:shell,args:['-NoLogo','-NoProfile','-NonInteractive','-Command',
      "$ErrorActionPreference='Stop'; node -e "+literal(script)+"; exit $LASTEXITCODE"],cwd:root}).wait();
    if(shellResult.exitCode!==0)throw new Error(shellResult.stderr.toString() || 'restricted shell exit '+shellResult.exitCode);
    if(!shellResult.stdout.toString().includes('himind-shell-probe-ok'))throw new Error('missing shell probe evidence');
    console.log(JSON.stringify({shell_verified:true,piped_children:true,shell}));
  } finally {box.dispose();}
})().catch(e=>{console.error(e.message);process.exitCode=1});
"#;
    let mut command = crate::runtime::process::hidden_command(node);
    crate::runtime::deepseek_harness::configure_runtime_environment(&mut command, executable);
    command.arg("-e").arg(PROBE).arg(module).arg(workspace).arg(runtime.join("node.exe"))
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::runtime::process::remove_himind_secret_environment(&mut command);
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().map(crate::runtime::process::capture_output);
    let stderr = child.stderr.take().map(crate::runtime::process::capture_output);
    let status = crate::runtime::process::wait_for_child_with_timeout_and_cancel(&mut child,
        "HIMIND_RUNTIME_PROBE_TIMEOUT_SECONDS", 30, "沙箱命令自检", || Ok(false))?;
    let stdout = crate::runtime::process::join_output(stdout);
    let stderr = crate::runtime::process::join_output(stderr);
    if status.success() { return Ok(serde_json::from_str(stdout.trim())?); }
    Err(json!({"code":"runtime_workspace_blocked", "kind":"runtime_command_incompatible",
        "message":"沙箱命令自检未通过，任务已暂停；可授予本任务完全访问或仅用文件工具继续",
        "cause":stderr.trim(), "diagnosis":check(&workspace.to_string_lossy())?,
        "choices":["full_access","retry","files_only","cancel"],"retryable":false}).to_string().into())
}

#[cfg(not(windows))]
fn verify_commands(_: &Path, _: &Path) -> Result<Value, Box<dyn Error>> { Ok(json!({"shell_verified":false})) }

#[cfg(windows)]
fn run_permission_tool(workspace: &Path, repair: bool) -> Result<Value, Box<dyn Error>> {
    const SCRIPT: &str = include_str!("../../scripts/runtime-workspace-permissions.ps1");
    let root = crate::store::paths::agent_home().join("runtime-support");
    fs::create_dir_all(&root)?;
    let script = root.join(format!("workspace-permissions-{:x}.ps1", Sha256::digest(SCRIPT.as_bytes())));
    if fs::read(&script).ok().as_deref() != Some(SCRIPT.as_bytes()) { fs::write(&script, SCRIPT)?; }
    let powershell = PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot 不可用")?)
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut command = crate::runtime::process::hidden_command(powershell);
    command.args(["-NoProfile", "-NonInteractive", "-File"]).arg(&script).arg("-Workspace").arg(workspace);
    if repair { command.arg("-Repair").arg("-ReportDirectory").arg(root.join("permission-backups")); }
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::runtime::process::remove_himind_secret_environment(&mut command);
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().map(crate::runtime::process::capture_output);
    let stderr = child.stderr.take().map(crate::runtime::process::capture_output);
    let status = crate::runtime::process::wait_for_child_with_timeout_and_cancel(&mut child,
        "HIMIND_WORKSPACE_PERMISSION_TIMEOUT_SECONDS", 45, "工作区权限检查", || Ok(false))?;
    let stdout = crate::runtime::process::join_output(stdout);
    let stderr = crate::runtime::process::join_output(stderr);
    if !status.success() { return Err(format!("工作区权限操作失败：{}", stderr.trim()).into()); }
    Ok(serde_json::from_str(stdout.trim_start_matches('\u{feff}').trim())?)
}

#[cfg(not(windows))]
fn run_permission_tool(_: &Path, _: bool) -> Result<Value, Box<dyn Error>> { Err("当前平台不支持 Windows 权限修复".into()) }

#[tauri::command]
pub(crate) async fn set_mission_permission_mode(mission_id: String, expected_plan_sha256: String, permission_mode: String) -> Result<crate::mission::MissionView, String> {
    tauri::async_runtime::spawn_blocking(move || crate::mission::MissionStore::open_default()
        .and_then(|store| store.set_permission_mode(&mission_id,&expected_plan_sha256,&permission_mode)).map_err(|error|error.to_string()))
        .await.map_err(|error|error.to_string())?
}

#[tauri::command]
pub(crate) async fn check_runtime_workspace(workspace: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || check(&workspace).map_err(|error| error.to_string()))
        .await.map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) async fn resolve_runtime_workspace(input: WorkspaceResolution) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || resolve(&input).map_err(|error| error.to_string()))
        .await.map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    #[ignore = "requires an explicitly selected isolated workspace and installed signed Runtime"]
    fn installed_runtime_prepares_and_verifies_real_shell() {
        let workspace = std::env::var("HIMIND_RUNTIME_PROBE_WORKSPACE").expect("set HIMIND_RUNTIME_PROBE_WORKSPACE to an isolated, authorized workspace");
        let executable = crate::runtime::deepseek_harness::interactive_executable().unwrap();
        eprintln!("runtime={}",executable.display());
        let identity=crate::runtime::process::hidden_command("whoami.exe").arg("/user").output().unwrap();
        eprintln!("probe identity={}",String::from_utf8_lossy(&identity.stdout));
        prepare(&workspace).unwrap();
    }
    #[test]
    fn permission_fingerprint_binds_owner_user_path_and_dacl() {
        let initial = json!({"workspace": "F:\\work", "owner_sid": "owner", "current_user_sid": "user", "dacl": "original"});
        for (key, value) in [("workspace", "F:\\other"), ("owner_sid", "other"), ("current_user_sid", "other"), ("dacl", "changed")] {
            let mut changed = initial.clone(); changed[key] = json!(value);
            assert_ne!(fingerprint(&initial), fingerprint(&changed));
        }
        assert_eq!(fingerprint(&initial), fingerprint(&initial));
    }
    #[test]
    fn resolution_has_no_client_asserted_approval_override() {
        assert!(serde_json::from_value::<WorkspaceResolution>(json!({"workspace":"F:\\work", "expected_fingerprint":"x", "action":"repair", "user_approved":true})).is_err());
    }
}
