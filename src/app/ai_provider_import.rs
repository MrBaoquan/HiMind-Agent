use rand::{distributions::Alphanumeric, Rng};
use rusqlite::{backup::Backup, params, Connection, TransactionBehavior};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::api::ai::{fetch_client_credential, AIClientCredential};
use crate::app::ai_clients::{backup_and_write, workbuddy_executable_exists};
use crate::Options;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
const MANAGED_VENDOR: &str = "HiMind";
const VSCODE_EXTENSION_ID: &str = "himind.himind-ai";
const VSCODE_ENROLLMENT_TTL_SECONDS: u64 = 60;
const VSCODE_ENROLLMENT_HANDOFF_FILE: &str = "vscode-enrollment-v2.json";

#[derive(Debug, Serialize)]
pub(crate) struct VSCodeEnrollmentCredential {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub models: Vec<String>,
    pub expires_at: u64,
}

struct PendingVSCodeEnrollment {
    credential: VSCodeEnrollmentCredential,
}

#[derive(Serialize)]
struct VSCodeEnrollmentHandoff<'a> {
    port: u16,
    code: &'a str,
    expires_at: u64,
}

static VSCODE_ENROLLMENTS: OnceLock<Mutex<HashMap<String, PendingVSCodeEnrollment>>> =
    OnceLock::new();

#[derive(Debug, Deserialize)]
pub(crate) struct AIProviderImportRequest {
    pub target: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct AIProviderImportResult {
    pub ok: bool,
    pub target: String,
    pub status: String,
    pub model_count: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub config_path: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub backup_path: String,
    pub client_detected: bool,
}

pub(crate) fn import(
    options: &Options,
    expected_user_id: &str,
    request: &AIProviderImportRequest,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    match request.target.trim() {
        "cc-switch" => import_cc_switch(options, expected_user_id),
        "workbuddy" => import_workbuddy(options, expected_user_id),
        "vscode" => import_vscode(options, expected_user_id),
        _ => Err("不支持的 AI 客户端，请选择 VS Code、CC Switch 或 WorkBuddy".into()),
    }
}

fn import_vscode(
    options: &Options,
    expected_user_id: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let vscode_cli = ensure_vscode_extension()?;
    let credential = fetch_client_credential(options, expected_user_id, "vscode-import")?;
    let models = available_models(&credential)?;
    let preferred = preferred_model(&credential)?;
    let code = create_vscode_enrollment(credential, preferred.clone(), models.clone())?;
    let enrollment_url = build_vscode_enrollment_url(options.local_port, &code)?;
    write_vscode_enrollment_handoff(options, &code)?;
    launch_vscode(&vscode_cli, &enrollment_url)?;
    Ok(AIProviderImportResult {
        ok: true,
        target: "vscode".to_string(),
        status: "authorization_opened".to_string(),
        model_count: models.len(),
        model: preferred,
        config_path: String::new(),
        backup_path: String::new(),
        client_detected: true,
    })
}

fn write_vscode_enrollment_handoff(options: &Options, code: &str) -> Result<(), Box<dyn Error>> {
    let directory = options
        .state_path
        .parent()
        .ok_or("HiMind Agent state directory is unavailable")?;
    fs::create_dir_all(directory)?;
    let path = directory.join(VSCODE_ENROLLMENT_HANDOFF_FILE);
    let temporary = directory.join("vscode-enrollment-v2.tmp");
    let handoff = VSCodeEnrollmentHandoff {
        port: options.local_port,
        code,
        expires_at: unix_now_seconds().saturating_add(VSCODE_ENROLLMENT_TTL_SECONDS),
    };
    fs::write(&temporary, serde_json::to_vec(&handoff)?)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn create_vscode_enrollment(
    credential: AIClientCredential,
    preferred: String,
    models: Vec<String>,
) -> Result<String, Box<dyn Error>> {
    let code: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect();
    let now = unix_now_seconds();
    let pending = PendingVSCodeEnrollment {
        credential: VSCodeEnrollmentCredential {
            base_url: normalized_base_url(&credential.access.base_url)?,
            api_key: credential.api_key,
            model: preferred,
            models,
            expires_at: now.saturating_add(VSCODE_ENROLLMENT_TTL_SECONDS),
        },
    };
    let enrollments = VSCODE_ENROLLMENTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut enrollments = enrollments
        .lock()
        .map_err(|_| "VS Code 授权状态暂时不可用")?;
    enrollments.retain(|_, item| item.credential.expires_at > now);
    enrollments.insert(code.clone(), pending);
    Ok(code)
}

pub(crate) fn consume_vscode_enrollment(
    code: &str,
) -> Result<VSCodeEnrollmentCredential, Box<dyn Error>> {
    if code.len() < 32
        || !code
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err("VS Code 授权码无效".into());
    }
    let enrollments = VSCODE_ENROLLMENTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut enrollments = enrollments
        .lock()
        .map_err(|_| "VS Code 授权状态暂时不可用")?;
    let pending = enrollments
        .remove(code)
        .ok_or("VS Code 授权码无效或已使用")?;
    if pending.credential.expires_at <= unix_now_seconds() {
        return Err("VS Code 授权码已过期，请从 Dashboard 重新导入".into());
    }
    Ok(pending.credential)
}

fn build_vscode_enrollment_url(port: u16, code: &str) -> Result<String, Box<dyn Error>> {
    if code.len() < 32
        || !code
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err("VS Code enrollment code is invalid".into());
    }
    Ok(Url::parse(&format!("vscode://himind.himind-ai/enroll/{port}/{code}"))?.into())
}

fn import_cc_switch(
    options: &Options,
    expected_user_id: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let protocol_registered = cc_switch_protocol_registered();
    let portable_executable = if protocol_registered {
        None
    } else {
        running_cc_switch_executable()
    };
    if !protocol_registered && portable_executable.is_none() {
        return Err("未检测到 CC Switch；便携版需先启动，安装版需注册 ccswitch:// 协议".into());
    }
    let credential = fetch_client_credential(options, expected_user_id, "cc-switch-import")?;
    let model = preferred_model(&credential)?;
    let deep_link = build_cc_switch_deep_link(&credential, &model)?;
    launch_sensitive_url(&deep_link, portable_executable.as_deref())?;
    Ok(AIProviderImportResult {
        ok: true,
        target: "cc-switch".to_string(),
        status: "confirmation_opened".to_string(),
        model_count: 1,
        model,
        config_path: String::new(),
        backup_path: String::new(),
        client_detected: true,
    })
}

fn import_workbuddy(
    options: &Options,
    expected_user_id: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let credential = fetch_client_credential(options, expected_user_id, "workbuddy-import")?;
    let path = workbuddy_models_path();
    let original = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let (updated, count) = merge_workbuddy_models(&original, &credential)?;
    let backup = backup_and_write(&path, updated.as_bytes())?;
    migrate_workbuddy_sessions(&path, &available_models(&credential)?)?;
    Ok(AIProviderImportResult {
        ok: true,
        target: "workbuddy".to_string(),
        status: "configured".to_string(),
        model_count: count,
        model: String::new(),
        config_path: path.to_string_lossy().to_string(),
        backup_path: backup
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
        client_detected: workbuddy_executable_exists(),
    })
}

fn preferred_model(credential: &AIClientCredential) -> Result<String, Box<dyn Error>> {
    let model = credential.access.model.trim();
    if !model.is_empty() {
        return Ok(model.to_string());
    }
    credential
        .access
        .models
        .iter()
        .find(|item| !item.trim().is_empty())
        .map(|item| item.trim().to_string())
        .ok_or_else(|| "当前 AI 接入没有可导入的模型".into())
}

fn build_cc_switch_deep_link(
    credential: &AIClientCredential,
    model: &str,
) -> Result<String, Box<dyn Error>> {
    let endpoint = normalized_base_url(&credential.access.base_url)?;
    let mut url = Url::parse("ccswitch://v1/import")?;
    url.query_pairs_mut()
        .append_pair("resource", "provider")
        .append_pair("app", "codex")
        .append_pair("name", "HiMind")
        .append_pair("endpoint", &endpoint)
        .append_pair("apiKey", &credential.api_key)
        .append_pair("model", model)
        .append_pair("enabled", "true");
    Ok(url.into())
}

fn merge_workbuddy_models(
    content: &str,
    credential: &AIClientCredential,
) -> Result<(String, usize), Box<dyn Error>> {
    let mut root = if content.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str::<Value>(content)
            .map_err(|_| "WorkBuddy models.json 格式无效，已停止导入且未覆盖原文件")?
    };
    let object = root
        .as_object_mut()
        .ok_or("WorkBuddy models.json 根节点必须是 JSON 对象")?;
    let models = object
        .entry("models")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or("WorkBuddy models.json 的 models 必须是数组")?;

    let previous_managed_ids = models
        .iter()
        .filter(|item| is_managed_workbuddy_model(item))
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
        .collect::<HashSet<_>>();
    models.retain(|item| !is_managed_workbuddy_model(item));

    let aliases = available_models(credential)?;
    let endpoint = chat_completions_url(&credential.access.base_url)?;
    let mut generated_id_set = HashSet::new();
    let mut generated_ids = Vec::new();
    for alias in &aliases {
        let mut id = workbuddy_model_id(alias);
        if !generated_id_set.insert(id.clone()) {
            id = format!("{}-{}", id, generated_id_set.len() + 1);
            generated_id_set.insert(id.clone());
        }
        generated_ids.push(id.clone());
        models.push(json!({
            "id": id,
            // WorkBuddy renders custom models as `<name>: <id>`, so keep the
            // configured name brand-only to avoid repeating the model alias.
            "name": MANAGED_VENDOR,
            "vendor": MANAGED_VENDOR,
            "apiKey": credential.api_key,
            "url": endpoint,
            "supportsToolCall": true,
            "supportsImages": false
        }));
    }

    let available = object
        .entry("availableModels")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or("WorkBuddy models.json 的 availableModels 必须是数组")?;
    available.retain(|item| {
        item.as_str()
            .map(|id| !previous_managed_ids.contains(id))
            .unwrap_or(true)
    });
    let mut existing = available
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<HashSet<_>>();
    for id in generated_ids {
        if existing.insert(id.clone()) {
            available.push(Value::String(id));
        }
    }
    Ok((
        format!("{}\n", serde_json::to_string_pretty(&root)?),
        aliases.len(),
    ))
}

fn available_models(credential: &AIClientCredential) -> Result<Vec<String>, Box<dyn Error>> {
    let mut seen = HashSet::new();
    let mut models = credential
        .access
        .models
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .filter(|item| seen.insert((*item).to_string()))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if models.is_empty() {
        models.push(preferred_model(credential)?);
    }
    Ok(models)
}

fn is_managed_workbuddy_model(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.get("vendor").and_then(Value::as_str) == Some(MANAGED_VENDOR)
}

fn workbuddy_model_id(alias: &str) -> String {
    // WorkBuddy adds its own `custom-local:` namespace in the UI and removes it
    // before sending a request. The remaining ID must therefore stay equal to
    // the model alias authorized by the HiMind gateway.
    alias.trim().to_string()
}

fn legacy_workbuddy_model_id(alias: &str) -> String {
    let mut normalized = String::new();
    let mut pending_separator = false;
    for character in alias.trim().chars() {
        if character.is_ascii_alphanumeric() {
            if pending_separator && !normalized.is_empty() {
                normalized.push('-');
            }
            normalized.push(character.to_ascii_lowercase());
            pending_separator = false;
        } else {
            pending_separator = !normalized.is_empty();
        }
    }
    format!("himind-{normalized}")
}

fn legacy_workbuddy_model_mappings(aliases: &[String]) -> Vec<(String, String)> {
    let mut mappings = HashMap::new();
    for alias in aliases {
        let current = workbuddy_model_id(alias);
        let legacy = legacy_workbuddy_model_id(alias);
        if !current.is_empty() && legacy != current {
            mappings.insert(legacy, current);
        }
    }
    mappings.into_iter().collect()
}

fn migrate_workbuddy_sessions(
    models_path: &Path,
    aliases: &[String],
) -> Result<usize, Box<dyn Error>> {
    let mappings = legacy_workbuddy_model_mappings(aliases);
    if mappings.is_empty() {
        return Ok(0);
    }
    let Some(config_directory) = models_path.parent() else {
        return Ok(0);
    };
    let database_path = config_directory.join("workbuddy.db");
    if !database_path.is_file() {
        return Ok(0);
    }

    let mut connection = Connection::open(&database_path)?;
    let has_sessions_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='sessions')",
        [],
        |row| row.get(0),
    )?;
    if !has_sessions_table {
        return Ok(0);
    }

    let stale_count = mappings.iter().try_fold(0usize, |total, (legacy, _)| {
        let namespaced = format!("custom-local:{legacy}");
        let count: usize = connection.query_row(
            "SELECT COUNT(*) FROM sessions WHERE model = ?1 OR model = ?2",
            params![legacy, namespaced],
            |row| row.get(0),
        )?;
        Ok::<usize, rusqlite::Error>(total + count)
    })?;
    if stale_count == 0 {
        return Ok(0);
    }

    backup_workbuddy_database(&connection, &database_path)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut migrated = 0usize;
    for (legacy, current) in mappings {
        migrated += transaction.execute(
            "UPDATE sessions SET model = CASE WHEN model = ?1 THEN ?2 ELSE ?3 END WHERE model = ?1 OR model = ?4",
            params![
                legacy,
                current,
                format!("custom-local:{current}"),
                format!("custom-local:{legacy}")
            ],
        )?;
    }
    transaction.commit()?;
    Ok(migrated)
}

fn backup_workbuddy_database(
    connection: &Connection,
    database_path: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    let file_name = database_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("workbuddy.db");
    let backup_path = database_path.with_file_name(format!(
        "{file_name}.himind-backup-{}.bak",
        unix_now_millis()
    ));
    let mut destination = Connection::open(&backup_path)?;
    let backup = Backup::new(connection, &mut destination)?;
    backup.run_to_completion(8, Duration::from_millis(25), None)?;
    drop(backup);
    destination.close().map_err(|(_, error)| error)?;
    Ok(backup_path)
}

fn unix_now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn unix_now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn normalized_base_url(value: &str) -> Result<String, Box<dyn Error>> {
    let mut url = Url::parse(value.trim()).map_err(|_| "AI Base URL 无效")?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("AI Base URL 仅支持 http 或 https".into());
    }
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(if path.is_empty() { "/" } else { &path });
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn chat_completions_url(value: &str) -> Result<String, Box<dyn Error>> {
    let base = normalized_base_url(value)?;
    if base.ends_with("/chat/completions") {
        Ok(base)
    } else {
        Ok(format!("{base}/chat/completions"))
    }
}

fn workbuddy_models_path() -> PathBuf {
    if let Some(path) = env::var_os("HIMIND_WORKBUDDY_MODELS_CONFIG") {
        return PathBuf::from(path);
    }
    workbuddy_models_path_in(&user_home())
}

fn workbuddy_models_path_in(home: &Path) -> PathBuf {
    // WorkBuddy Desktop uses its own runtime directory. `.codebuddy` belongs to
    // the standalone CodeBuddy CLI and is not observed by the desktop client.
    home.join(".workbuddy").join("models.json")
}

fn user_home() -> PathBuf {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[derive(Deserialize)]
struct VSCodeExtensionManifest {
    name: String,
    publisher: String,
    version: String,
}

fn ensure_vscode_extension() -> Result<PathBuf, Box<dyn Error>> {
    let cli = locate_vscode_cli().ok_or("未检测到 VS Code，请先安装 VS Code 后再导入 HiMind AI")?;
    let vsix = bundled_vscode_vsix_path()?;
    let bundled_version = read_vscode_vsix_version(&vsix)?;
    let installed_version = installed_vscode_extension_version(&cli)?;
    let install_required =
        vscode_extension_install_required(installed_version.as_deref(), &bundled_version)?;
    if install_required {
        install_vscode_extension(&cli, &vsix)?;
        let installed = installed_vscode_extension_version(&cli)?
            .ok_or("VS Code CLI 已返回安装成功，但未检测到 HiMind AI 扩展")?;
        if compare_extension_versions(&installed, &bundled_version)? == Ordering::Less {
            return Err(format!(
                "HiMind AI 扩展安装校验失败：当前版本 {installed}，内置版本 {bundled_version}"
            )
            .into());
        }
    }
    Ok(cli)
}

fn locate_vscode_cli() -> Option<PathBuf> {
    let mut stable_candidates = Vec::new();
    let mut insiders_candidates = Vec::new();
    if let Some(value) = env::var_os("HIMIND_VSCODE_CLI") {
        stable_candidates.push(PathBuf::from(value));
    }
    stable_candidates.push(PathBuf::from("code"));
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        let root = PathBuf::from(local_app_data).join("Programs");
        stable_candidates.push(root.join("Microsoft VS Code/bin/code.cmd"));
        insiders_candidates.push(root.join("Microsoft VS Code Insiders/bin/code-insiders.cmd"));
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(program_files) = env::var_os(variable) {
            let root = PathBuf::from(program_files);
            stable_candidates.push(root.join("Microsoft VS Code/bin/code.cmd"));
            insiders_candidates.push(root.join("Microsoft VS Code Insiders/bin/code-insiders.cmd"));
        }
    }
    if cfg!(windows) {
        stable_candidates.push(PathBuf::from(r"C:\Programs\Microsoft VS Code\bin\code.cmd"));
        insiders_candidates.push(PathBuf::from(
            r"C:\Programs\Microsoft VS Code Insiders\bin\code-insiders.cmd",
        ));
    }
    insiders_candidates.push(PathBuf::from("code-insiders"));
    stable_candidates.extend(insiders_candidates);

    let mut seen = HashSet::new();
    stable_candidates.into_iter().find(|candidate| {
        let key = candidate.to_string_lossy().to_ascii_lowercase();
        seen.insert(key) && vscode_cli_available(candidate)
    })
}

fn vscode_cli_available(cli: &Path) -> bool {
    vscode_command(cli)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn installed_vscode_extension_version(cli: &Path) -> Result<Option<String>, Box<dyn Error>> {
    let output = vscode_command(cli)
        .args(["--list-extensions", "--show-versions"])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "无法检查 VS Code 扩展：{}",
            command_error_detail(&output.stdout, &output.stderr)
        )
        .into());
    }
    parse_vscode_extension_version(&String::from_utf8_lossy(&output.stdout)).map_err(Into::into)
}

fn parse_vscode_extension_version(output: &str) -> Result<Option<String>, String> {
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some((extension_id, version)) = line.rsplit_once('@') {
            if extension_id.eq_ignore_ascii_case(VSCODE_EXTENSION_ID) {
                if version.trim().is_empty() {
                    return Err("VS Code 返回了空的 HiMind AI 扩展版本".to_string());
                }
                return Ok(Some(version.trim().to_string()));
            }
        } else if line.eq_ignore_ascii_case(VSCODE_EXTENSION_ID) {
            return Err("VS Code 未返回 HiMind AI 扩展版本".to_string());
        }
    }
    Ok(None)
}

fn compare_extension_versions(left: &str, right: &str) -> Result<Ordering, Box<dyn Error>> {
    let left_version =
        Version::parse(left.trim()).map_err(|_| format!("HiMind AI 扩展版本格式无效：{left}"))?;
    let right_version = Version::parse(right.trim())
        .map_err(|_| format!("内置 HiMind AI 扩展版本格式无效：{right}"))?;
    Ok(left_version.cmp(&right_version))
}

fn vscode_extension_install_required(
    installed: Option<&str>,
    bundled: &str,
) -> Result<bool, Box<dyn Error>> {
    match installed {
        None => Ok(true),
        Some(version) => Ok(compare_extension_versions(version, bundled)? == Ordering::Less),
    }
}

fn bundled_vscode_vsix_path() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(value) = env::var_os("HIMIND_VSCODE_EXTENSION_VSIX") {
        let path = PathBuf::from(value);
        return path
            .is_file()
            .then_some(path)
            .ok_or_else(|| "HIMIND_VSCODE_EXTENSION_VSIX 指向的 VSIX 文件不存在".into());
    }
    let executable = env::current_exe()?;
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent();
    bundled_vscode_vsix_candidates(&executable, repository_root)
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "HiMind Agent 安装资源不完整：缺少内置 HiMind AI VSIX".into())
}

fn bundled_vscode_vsix_candidates(
    executable: &Path,
    repository_root: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(directory) = executable.parent() {
        if directory
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("current"))
        {
            if let Some(install_root) = directory.parent() {
                candidates.push(install_root.join("resources/vscode/himind-ai.vsix"));
            }
        }
        candidates.push(directory.join("resources/vscode/himind-ai.vsix"));
    }
    if let Some(root) = repository_root {
        candidates.push(root.join("official-extensions/vscode-himind-ai/dist/himind-ai.vsix"));
    }
    candidates
}

fn read_vscode_vsix_version(path: &Path) -> Result<String, Box<dyn Error>> {
    let file =
        fs::File::open(path).map_err(|error| format!("无法读取内置 HiMind AI VSIX：{error}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| format!("内置 HiMind AI VSIX 已损坏：{error}"))?;
    let mut manifest_file = archive
        .by_name("extension/package.json")
        .map_err(|_| "内置 HiMind AI VSIX 缺少 extension/package.json")?;
    let mut content = String::new();
    manifest_file.read_to_string(&mut content)?;
    let manifest: VSCodeExtensionManifest = serde_json::from_str(&content)
        .map_err(|error| format!("内置 HiMind AI VSIX 清单无效：{error}"))?;
    let extension_id = format!("{}.{}", manifest.publisher, manifest.name);
    if !extension_id.eq_ignore_ascii_case(VSCODE_EXTENSION_ID) {
        return Err(
            format!("内置 VSIX 身份无效：预期 {VSCODE_EXTENSION_ID}，实际 {extension_id}").into(),
        );
    }
    Version::parse(manifest.version.trim())
        .map_err(|_| format!("内置 HiMind AI 扩展版本格式无效：{}", manifest.version))?;
    Ok(manifest.version)
}

fn install_vscode_extension(cli: &Path, vsix: &Path) -> Result<(), Box<dyn Error>> {
    let output = vscode_command(cli)
        .arg("--install-extension")
        .arg(vsix)
        .arg("--force")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "HiMind AI 扩展安装失败：{}",
            command_error_detail(&output.stdout, &output.stderr)
        )
        .into());
    }
    Ok(())
}

fn command_error_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let detail = format!(
        "{} {}",
        String::from_utf8_lossy(stderr).trim(),
        String::from_utf8_lossy(stdout).trim()
    );
    let detail = detail.trim();
    if detail.is_empty() {
        "VS Code CLI 未返回错误详情".to_string()
    } else {
        detail.chars().take(500).collect()
    }
}

fn vscode_command(cli: &Path) -> Command {
    let mut command = Command::new(cli);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(windows)]
fn launch_vscode(cli: &Path, enrollment_url: &str) -> Result<(), Box<dyn Error>> {
    let status = vscode_command(cli)
        .args(["--reuse-window", "--open-url", enrollment_url])
        .status()?;
    if !status.success() {
        return Err("无法唤起 VS Code 完成 HiMind 授权".into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn launch_vscode(cli: &Path, enrollment_url: &str) -> Result<(), Box<dyn Error>> {
    let status = vscode_command(cli)
        .args(["--reuse-window", "--open-url", enrollment_url])
        .status()?;
    if !status.success() {
        return Err("无法唤起 VS Code 完成 HiMind 授权".into());
    }
    Ok(())
}

#[cfg(windows)]
fn cc_switch_protocol_registered() -> bool {
    Command::new("reg.exe")
        .args(["query", r"HKCR\ccswitch", "/ve"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn running_cc_switch_executable() -> Option<PathBuf> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-Process -Name 'cc-switch' -ErrorAction SilentlyContinue | Where-Object { $_.Path } | Select-Object -First 1 -ExpandProperty Path",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let valid_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("cc-switch.exe"));
    (valid_name && path.is_file()).then_some(path)
}

#[cfg(not(windows))]
fn running_cc_switch_executable() -> Option<PathBuf> {
    None
}

#[cfg(not(windows))]
fn cc_switch_protocol_registered() -> bool {
    false
}

#[cfg(windows)]
fn launch_sensitive_url(url: &str, executable: Option<&Path>) -> Result<(), Box<dyn Error>> {
    let script = if executable.is_some() {
        "Start-Process -FilePath $env:HIMIND_CC_SWITCH_EXE -ArgumentList $env:HIMIND_EXTERNAL_URL"
    } else {
        "Start-Process -FilePath $env:HIMIND_EXTERNAL_URL"
    };
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
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .env("HIMIND_EXTERNAL_URL", url);
    if let Some(path) = executable {
        command.env("HIMIND_CC_SWITCH_EXE", path);
    }
    let status = command.status()?;
    if !status.success() {
        return Err("无法打开 CC Switch 导入确认窗口".into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn launch_sensitive_url(_url: &str, _executable: Option<&Path>) -> Result<(), Box<dyn Error>> {
    Err("CC Switch 一键导入目前仅支持 Windows".into())
}

#[cfg(test)]
mod tests {
    use super::{
        build_cc_switch_deep_link, build_vscode_enrollment_url, bundled_vscode_vsix_candidates,
        chat_completions_url, compare_extension_versions, consume_vscode_enrollment,
        create_vscode_enrollment, legacy_workbuddy_model_id, merge_workbuddy_models,
        migrate_workbuddy_sessions, parse_vscode_extension_version,
        vscode_extension_install_required, workbuddy_model_id, workbuddy_models_path_in,
        AIClientCredential,
    };
    use crate::api::ai::AIUserCredential;
    use serde_json::Value;
    use std::cmp::Ordering;
    use std::path::{Path, PathBuf};

    fn credential(models: &[&str]) -> AIClientCredential {
        AIClientCredential {
            access: AIUserCredential {
                active_entitlement_id: "ent-1".to_string(),
                status: "active".to_string(),
                base_url: "https://ai.example.com/v1/".to_string(),
                model: models.first().copied().unwrap_or("default").to_string(),
                models: models.iter().map(|value| value.to_string()).collect(),
            },
            api_key: "test-secret-key".to_string(),
        }
    }

    #[test]
    fn normalizes_chat_completions_url() {
        assert_eq!(
            chat_completions_url("https://ai.example.com/v1/").unwrap(),
            "https://ai.example.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://ai.example.com/v1/chat/completions").unwrap(),
            "https://ai.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn encodes_cc_switch_provider_parameters() {
        let value = build_cc_switch_deep_link(&credential(&["gpt-4.1"]), "gpt-4.1").unwrap();
        let url = url::Url::parse(&value).unwrap();
        let parameters = url
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(url.scheme(), "ccswitch");
        assert_eq!(
            parameters.get("resource").map(String::as_str),
            Some("provider")
        );
        assert_eq!(parameters.get("app").map(String::as_str), Some("codex"));
        assert_eq!(
            parameters.get("endpoint").map(String::as_str),
            Some("https://ai.example.com/v1")
        );
        assert_eq!(
            parameters.get("apiKey").map(String::as_str),
            Some("test-secret-key")
        );
        assert_eq!(parameters.get("model").map(String::as_str), Some("gpt-4.1"));
    }

    #[test]
    fn vscode_enrollment_is_single_use_and_keeps_key_out_of_uri() {
        let code = create_vscode_enrollment(
            credential(&["glm-5.1", "deepseek-v4-flash"]),
            "glm-5.1".to_string(),
            vec!["glm-5.1".to_string(), "deepseek-v4-flash".to_string()],
        )
        .unwrap();
        let enrollment_url = build_vscode_enrollment_url(18181, &code).unwrap();
        assert!(enrollment_url.starts_with("vscode://himind.himind-ai/enroll/18181/"));
        assert!(enrollment_url.contains(&code));
        assert!(!enrollment_url.contains('?'));
        assert!(!enrollment_url.contains('&'));
        assert!(!enrollment_url.contains("test-secret-key"));

        let exchanged = consume_vscode_enrollment(&code).unwrap();
        assert_eq!(exchanged.api_key, "test-secret-key");
        assert_eq!(exchanged.model, "glm-5.1");
        assert_eq!(exchanged.models.len(), 2);
        assert!(consume_vscode_enrollment(&code).is_err());
    }

    #[test]
    fn parses_vscode_extension_versions() {
        let output = "other.publisher@2.0.0\nhimind.himind-ai@0.1.8\n";
        assert_eq!(
            parse_vscode_extension_version(output).unwrap().as_deref(),
            Some("0.1.8")
        );
        assert_eq!(
            parse_vscode_extension_version("other.publisher@2.0.0").unwrap(),
            None
        );
        assert!(parse_vscode_extension_version("himind.himind-ai").is_err());
    }

    #[test]
    fn compares_vscode_extension_versions_without_downgrading() {
        assert_eq!(
            compare_extension_versions("0.1.7", "0.1.8").unwrap(),
            Ordering::Less
        );
        assert!(vscode_extension_install_required(Some("0.1.7"), "0.1.8").unwrap());
        assert!(!vscode_extension_install_required(Some("0.1.8"), "0.1.8").unwrap());
        assert!(!vscode_extension_install_required(Some("0.2.0"), "0.1.8").unwrap());
        assert!(vscode_extension_install_required(None, "0.1.8").unwrap());
    }

    #[test]
    fn resolves_installed_and_development_vscode_vsix_candidates() {
        let executable =
            Path::new(r"C:\Users\example\AppData\Local\HiMindAgent\current\himind-agent.exe");
        let repository = Path::new(r"F:\workspace\himind");
        let candidates = bundled_vscode_vsix_candidates(executable, Some(repository));
        assert_eq!(
            candidates[0],
            PathBuf::from(
                r"C:\Users\example\AppData\Local\HiMindAgent\resources\vscode\himind-ai.vsix"
            )
        );
        assert_eq!(
            candidates.last().unwrap(),
            &PathBuf::from(
                r"F:\workspace\himind\official-extensions\vscode-himind-ai\dist\himind-ai.vsix"
            )
        );
    }

    #[test]
    fn preserves_gateway_model_aliases_for_workbuddy_ids() {
        assert_eq!(workbuddy_model_id(" glm-5.2 "), "glm-5.2");
        assert_eq!(workbuddy_model_id("qwen-3.5-35b-a3b"), "qwen-3.5-35b-a3b");
        assert_eq!(legacy_workbuddy_model_id(" glm-5.2 "), "himind-glm-5-2");
    }

    #[test]
    fn migrates_legacy_workbuddy_session_models() {
        let root = std::env::temp_dir().join(format!(
            "himind-workbuddy-session-migration-{}-{}",
            std::process::id(),
            super::unix_now_millis()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let database_path = root.join("workbuddy.db");
        let connection = rusqlite::Connection::open(&database_path).unwrap();
        connection
            .execute(
                "CREATE TABLE sessions (id TEXT PRIMARY KEY, model TEXT)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions (id, model) VALUES ('legacy', 'custom-local:himind-glm-5-2'), ('personal', 'custom-local:personal')",
                [],
            )
            .unwrap();
        drop(connection);

        let migrated =
            migrate_workbuddy_sessions(&root.join("models.json"), &["glm-5.2".into()]).unwrap();
        assert_eq!(migrated, 1);
        let connection = rusqlite::Connection::open(&database_path).unwrap();
        let legacy_model: String = connection
            .query_row("SELECT model FROM sessions WHERE id='legacy'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let personal_model: String = connection
            .query_row(
                "SELECT model FROM sessions WHERE id='personal'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_model, "custom-local:glm-5.2");
        assert_eq!(personal_model, "custom-local:personal");
        assert!(std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains("himind-backup")));
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uses_workbuddy_desktop_models_path_by_default() {
        assert_eq!(
            workbuddy_models_path_in(Path::new(r"C:\\Users\\example")),
            PathBuf::from(r"C:\\Users\\example\\.workbuddy\\models.json")
        );
    }

    #[test]
    fn merges_models_without_removing_user_configuration() {
        let source = r#"{
          "models": [
            {"id":"personal","vendor":"Other","apiKey":"keep"},
            {"id":"himind-old","vendor":"HiMind","apiKey":"replace"}
          ],
          "availableModels": ["personal", "himind-old"],
          "theme": "dark"
        }"#;
        let (updated, count) =
            merge_workbuddy_models(source, &credential(&["gpt-4.1", "o3"])).unwrap();
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(count, 2);
        assert_eq!(root["theme"], "dark");
        assert!(root["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "personal"));
        assert!(!root["availableModels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "himind-old"));
        assert!(root["availableModels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "gpt-4.1"));
        assert!(root["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "gpt-4.1" && item["name"] == "HiMind"));
    }

    #[test]
    fn rejects_invalid_json_without_rebuilding_it() {
        let error = merge_workbuddy_models("{broken", &credential(&["gpt-4.1"]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("未覆盖原文件"));
    }
}
