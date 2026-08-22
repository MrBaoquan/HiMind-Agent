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
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use toml_edit::{value, DocumentMut, Item, Table};
use url::Url;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::api::ai::{fetch_client_credential, AIClientCredential};
use crate::app::ai_clients::{backup_and_write, workbuddy_executable_exists};
use crate::Options;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
const MANAGED_VENDOR: &str = "HiMind";
const CC_SWITCH_PROVIDER_ID: &str = "himind-codex";
const CODEX_HIMIND_MODELS_FILE: &str = "himind-models.json";
const CODEX_PROVIDER_ID: &str = "himind";
const VSCODE_EXTENSION_ID: &str = "himind.himind-ai";
const VSCODE_CHAT_PROVIDER_PROPOSAL: &str = "chatProvider";
const VSCODE_ENROLLMENT_TTL_SECONDS: u64 = 60;
const VSCODE_ENROLLMENT_HANDOFF_FILE: &str = "vscode-enrollment-v2.json";
const VSCODE_IMPORT_STATUS_FILE: &str = "vscode-import-status.json";

#[derive(Debug, Serialize)]
pub(crate) struct VSCodeEnrollmentCredential {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub models: Vec<String>,
    pub expires_at: u64,
    pub import_status_path: String,
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
static VSCODE_EXTENSION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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

#[derive(Debug, Serialize)]
pub(crate) struct AIProviderImportStatus {
    pub target: String,
    pub state: String,
    pub client_detected: bool,
    pub detail: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub config_path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub synced_at: String,
}

#[derive(Debug, Default, Deserialize)]
struct VSCodeImportStatusFile {
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    synced_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct AIProviderImportStatusOverview {
    pub targets: Vec<AIProviderImportStatus>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AIProviderImportCancelResult {
    pub ok: bool,
    pub target: String,
    pub status: String,
    pub changed: bool,
    pub client_detected: bool,
    pub detail: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub backup_path: String,
}

pub(crate) fn import(
    options: &Options,
    expected_user_id: &str,
    request: &AIProviderImportRequest,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    match request.target.trim() {
        "cc-switch" => import_cc_switch(options, expected_user_id),
        "codex" => import_codex(options, expected_user_id),
        "workbuddy" => import_workbuddy(options, expected_user_id),
        "vscode" => import_vscode(options, expected_user_id),
        _ => Err("不支持的 AI 客户端，请选择 VS Code、CC Switch、Codex 或 WorkBuddy".into()),
    }
}

pub(crate) fn status(options: &Options) -> AIProviderImportStatusOverview {
    AIProviderImportStatusOverview {
        targets: vec![
            vscode_import_status(options),
            cc_switch_import_status(),
            codex_import_status(options),
            workbuddy_import_status(),
        ],
    }
}

pub(crate) fn cancel(
    options: &Options,
    target: &str,
) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    match target.trim() {
        "vscode" => cancel_vscode(options),
        "cc-switch" => cancel_cc_switch(),
        "codex" => cancel_codex(options),
        "workbuddy" => cancel_workbuddy(),
        _ => Err("不支持的 AI 客户端，请选择 VS Code、CC Switch、Codex 或 WorkBuddy".into()),
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
    let code = create_vscode_enrollment(
        credential,
        preferred.clone(),
        models.clone(),
        vscode_import_status_path(options)
            .to_string_lossy()
            .to_string(),
    )?;
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
    import_status_path: String,
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
            import_status_path,
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
    let path = cc_switch_database_path();
    let client_detected =
        cc_switch_protocol_registered() || running_cc_switch_executable().is_some();
    if !path.is_file() {
        return Err(if client_detected {
            "CC Switch 尚未初始化数据库，请先打开一次 CC Switch 再导入".into()
        } else {
            "未检测到 CC Switch，请先安装并启动一次 CC Switch".into()
        });
    }
    let credential = fetch_client_credential(options, expected_user_id, "cc-switch-import")?;
    let models = available_models(&credential)?;
    let preferred = preferred_model(&credential)?;
    let existing = read_cc_switch_managed_settings(&path)?;
    let settings =
        build_cc_switch_provider_settings(&credential, &models, &preferred, existing.as_ref())?;
    let website = Url::parse(&credential.access.base_url)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_default();
    let backup = write_cc_switch_provider(&path, &settings, &website)?;
    Ok(AIProviderImportResult {
        ok: true,
        target: "cc-switch".to_string(),
        status: "configured".to_string(),
        model_count: models.len(),
        model: preferred,
        config_path: path.to_string_lossy().to_string(),
        backup_path: backup.to_string_lossy().to_string(),
        client_detected,
    })
}

fn codex_config_path() -> PathBuf {
    let home = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(user_home);
    if env::var_os("CODEX_HOME").is_none() {
        home.join(".codex")
    } else {
        home
    }
}

fn codex_himind_models_path() -> PathBuf {
    codex_config_path().join(CODEX_HIMIND_MODELS_FILE)
}

// Codex 直连采用 DeepSeek 官方接入范式：model_catalog_json 指向独立模型目录，
// Codex 重启后 /model 即可列出 HiMind 全量模型；key 按官方做法明文写入
// experimental_bearer_token（仅本机 config.toml）。
fn import_codex(
    options: &Options,
    expected_user_id: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let config_path = codex_config_path();
    let models_path = codex_himind_models_path();
    let client_detected = config_path.join("config.toml").is_file()
        || config_path.join(CODEX_HIMIND_MODELS_FILE).is_file();
    let credential = fetch_client_credential(options, expected_user_id, "codex-import")?;
    let models = available_models(&credential)?;
    let preferred = preferred_model(&credential)?;
    let catalog = build_codex_models_json(&models)?;
    let config_file = config_path.join("config.toml");
    let original_config = if config_file.is_file() {
        fs::read_to_string(&config_file)?
    } else {
        String::new()
    };
    let config = build_codex_config_toml(&original_config, &credential, &models_path, &preferred)?;
    let catalog_backup = backup_and_write(&models_path, catalog.as_bytes())?;
    let config_backup = backup_and_write(&config_file, config.as_bytes())?;
    Ok(AIProviderImportResult {
        ok: true,
        target: "codex".to_string(),
        status: "configured".to_string(),
        model_count: models.len(),
        model: preferred,
        config_path: config_path
            .join("config.toml")
            .to_string_lossy()
            .to_string(),
        backup_path: config_backup
            .or(catalog_backup)
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
        client_detected,
    })
}

fn build_codex_models_json(models: &[String]) -> Result<String, Box<dyn Error>> {
    let mut catalog = Vec::new();
    for (index, model) in models.iter().enumerate() {
        let display = model
            .split('-')
            .map(capitalize)
            .collect::<Vec<_>>()
            .join(" ");
        catalog.push(json!({
            "slug": model,
            "display_name": display,
            "description": "HiMind 网关模型",
            "context_window": 1048576,
            "max_context_window": 1048576,
            "effective_context_window_percent": 95,
            "input_modalities": ["text"],
            "supports_parallel_tool_calls": true,
            "apply_patch_tool_type": "freeform",
            "web_search_tool_type": "text",
            "supports_search_tool": true,
            "default_reasoning_level": "high",
            "supported_reasoning_levels": [
                {"effort": "low", "description": "Fast responses with lighter reasoning"},
                {"effort": "high", "description": "Extra high reasoning depth for complex problems"},
                {"effort": "max", "description": "Maximum reasoning depth for the hardest problems"}
            ],
            "default_verbosity": "low",
            "support_verbosity": true,
            "priority": (index + 1) as i64,
            "visibility": "list",
            "minimal_client_version": "0.144.0",
            "supported_in_api": true,
            "truncation_policy": {"mode": "tokens", "limit": 10000},
            "comp_hash": 3000,
            "multi_agent_version": "v2",
            "use_responses_lite": false,
            "supports_reasoning_summaries": true,
            "reasoning_summary_format": "experimental",
            "default_reasoning_summary": "none",
            "shell_type": "shell_command"
        }));
    }
    Ok(serde_json::to_string_pretty(&json!({ "models": catalog }))?)
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

// config.toml 保留式合并：只接管 Codex 直连必需字段与 [model_providers.himind]，
// mcp_servers、notify、marketplaces、plugins、[projects] 等用户配置原样保留。
fn build_codex_config_toml(
    original: &str,
    credential: &AIClientCredential,
    models_path: &Path,
    preferred: &str,
) -> Result<String, Box<dyn Error>> {
    let endpoint = normalized_base_url(&credential.access.base_url)?;
    let catalog_value = models_path.to_string_lossy().replace('\\', "/");
    let mut document = original
        .parse::<DocumentMut>()
        .map_err(|error| format!("Codex config.toml 格式无效：{error}"))?;
    document["model"] = value(preferred);
    document["model_provider"] = value(CODEX_PROVIDER_ID);
    document["preferred_auth_method"] = value("apikey");
    document["forced_login_method"] = value("api");
    document["model_reasoning_effort"] = value("high");
    document["model_catalog_json"] = value(catalog_value.as_str());
    let providers = document
        .as_table_mut()
        .entry("model_providers")
        .or_insert_with(|| {
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_mut()
        .ok_or("既有 config.toml 的 model_providers 不是表")?;
    let provider = providers
        .entry(CODEX_PROVIDER_ID)
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or("既有 config.toml 的 model_providers.himind 不是表")?;
    provider["name"] = value(MANAGED_VENDOR);
    provider["base_url"] = value(endpoint.as_str());
    provider["wire_api"] = value("responses");
    provider["experimental_bearer_token"] = value(credential.api_key.as_str());
    Ok(document.to_string())
}

fn codex_import_status(_options: &Options) -> AIProviderImportStatus {
    let config_path = codex_config_path();
    let config_file = config_path.join("config.toml");
    let models_path = codex_himind_models_path();
    let client_detected = config_file.is_file();
    let models = read_codex_managed_models(&config_file, &models_path)
        .ok()
        .unwrap_or_default();
    let imported = !models.is_empty() || codex_managed_provider_present(&config_file);
    AIProviderImportStatus {
        target: "codex".to_string(),
        state: if imported { "imported" } else { "not_imported" }.to_string(),
        client_detected,
        detail: if imported && models.is_empty() {
            "检测到 Codex 已配置 HiMind 供应商，但缺少模型目录，请重新导入".to_string()
        } else if imported {
            format!(
                "已写入 {} 个 HiMind 模型；重启 Codex 后可在 /model 选择",
                models.len()
            )
        } else if client_detected {
            "已检测到 Codex，尚未导入 HiMind AI".to_string()
        } else {
            "未检测到 Codex 配置目录，请先运行一次 Codex".to_string()
        },
        config_path: config_file.to_string_lossy().to_string(),
        models,
        synced_at: String::new(),
    }
}

fn codex_managed_provider_present(config_file: &Path) -> bool {
    fs::read_to_string(config_file)
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .is_some_and(|document| {
            document
                .get("model_provider")
                .and_then(|item| item.as_str())
                == Some(CODEX_PROVIDER_ID)
                || document
                    .get("model_providers")
                    .and_then(|item| item.as_table())
                    .is_some_and(|table| table.contains_key(CODEX_PROVIDER_ID))
        })
}

fn read_codex_model_catalog(models_path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let content = fs::read_to_string(models_path)?;
    let root: Value = serde_json::from_str(&content)?;
    Ok(root
        .get("models")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("slug").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

fn read_codex_managed_models(
    config_file: &Path,
    models_path: &Path,
) -> Result<Vec<String>, Box<dyn Error>> {
    if !codex_managed_provider_present(config_file) || !models_path.is_file() {
        return Ok(Vec::new());
    }
    read_codex_model_catalog(models_path)
}

fn cancel_codex(_options: &Options) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let config_path = codex_config_path();
    let config_file = config_path.join("config.toml");
    let models_path = codex_himind_models_path();
    let client_detected = config_file.is_file();
    let original_config = if config_file.is_file() {
        fs::read_to_string(&config_file)?
    } else {
        String::new()
    };
    let models_present = models_path.is_file();
    let removed_models = if models_present {
        read_codex_model_catalog(&models_path)
            .unwrap_or_default()
            .len()
    } else {
        0
    };
    let (updated, changed) = strip_codex_himind(&original_config, &models_path)?;
    if changed {
        backup_and_write(&config_file, updated.as_bytes())?;
    }
    if models_present {
        fs::remove_file(&models_path)?;
    }
    let removed = changed || models_present;
    Ok(AIProviderImportCancelResult {
        ok: true,
        target: "codex".to_string(),
        status: if removed { "cancelled" } else { "not_imported" }.to_string(),
        changed: removed,
        client_detected,
        detail: if removed {
            format!(
                "已从 Codex 移除 HiMind 供应商{}",
                if removed_models > 0 {
                    format!("及 {removed_models} 个模型")
                } else {
                    String::new()
                }
            )
        } else {
            "Codex 当前没有 HiMind 导入记录".to_string()
        },
        backup_path: String::new(),
    })
}

// 只移除 HiMind 明确写入的字段，保留用户其他配置；无法判定归属的字段不动。
fn strip_codex_himind(config: &str, models_path: &Path) -> Result<(String, bool), Box<dyn Error>> {
    let mut document = config.parse::<DocumentMut>()?;
    let mut changed = false;
    let catalog_target = models_path.to_string_lossy().replace('\\', "/");
    if document
        .get("model_catalog_json")
        .and_then(|item| item.as_str())
        .is_some_and(|value| value.replace('\\', "/") == catalog_target)
    {
        document.remove("model_catalog_json");
        changed = true;
    }
    if let Some(providers) = document
        .get_mut("model_providers")
        .and_then(|item| item.as_table_mut())
    {
        if providers.remove(CODEX_PROVIDER_ID).is_some() {
            changed = true;
        }
    }
    if document
        .get("model_provider")
        .and_then(|item| item.as_str())
        == Some(CODEX_PROVIDER_ID)
    {
        document.remove("model_provider");
        changed = true;
    }
    Ok((document.to_string(), changed))
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

fn vscode_import_status(options: &Options) -> AIProviderImportStatus {
    let path = vscode_import_status_path(options);
    let cli = locate_vscode_cli();
    let client_detected = cli.is_some();
    let extension_installed = cli
        .and_then(|cli| installed_vscode_extension_version(&cli).ok().flatten())
        .is_some();
    let imported = path.is_file();
    let status = fs::read_to_string(&path)
        .ok()
        .and_then(|content| parse_vscode_import_status(&content).ok())
        .unwrap_or_default();
    AIProviderImportStatus {
        target: "vscode".to_string(),
        state: if imported { "imported" } else { "not_imported" }.to_string(),
        client_detected,
        detail: if imported && !status.models.is_empty() {
            format!("VS Code 已同步 {} 个 HiMind 模型", status.models.len())
        } else if imported {
            "VS Code 已保存 HiMind AI 凭据，等待扩展同步模型状态".to_string()
        } else if extension_installed {
            "已安装 HiMind AI 扩展，尚未检测到导入记录".to_string()
        } else if client_detected {
            "已检测到 VS Code，尚未安装 HiMind AI 扩展".to_string()
        } else {
            "未检测到 VS Code，请先安装；便携版可将 HIMIND_VSCODE_CLI 配置为 bin\\code.cmd"
                .to_string()
        },
        config_path: path.to_string_lossy().to_string(),
        models: status.models,
        synced_at: status.synced_at,
    }
}

fn cc_switch_import_status() -> AIProviderImportStatus {
    let path = cc_switch_database_path();
    let client_detected =
        cc_switch_protocol_registered() || running_cc_switch_executable().is_some();
    let models = if path.is_file() {
        read_cc_switch_managed_models(&path)
            .ok()
            .flatten()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let imported = path.is_file() && cc_switch_managed_provider_count(&path).unwrap_or(0) > 0;
    AIProviderImportStatus {
        target: "cc-switch".to_string(),
        state: if imported { "imported" } else { "not_imported" }.to_string(),
        client_detected,
        detail: if imported && models.is_empty() {
            "检测到 CC Switch 中的 HiMind 供应商，但缺少模型映射，请重新导入".to_string()
        } else if imported {
            format!(
                "已写入 {} 个 HiMind 模型；在 CC Switch 启用 HiMind 并重启 Codex 后可在 /model 选择",
                models.len()
            )
        } else if client_detected || path.is_file() {
            "已检测到 CC Switch，尚未导入 HiMind AI".to_string()
        } else {
            "未检测到 CC Switch".to_string()
        },
        config_path: path.to_string_lossy().to_string(),
        models,
        synced_at: String::new(),
    }
}

fn workbuddy_import_status() -> AIProviderImportStatus {
    let path = workbuddy_models_path();
    let models = fs::read_to_string(&path)
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .map(|root| managed_workbuddy_model_ids(&root))
        .unwrap_or_default();
    let imported = !models.is_empty();
    let client_detected = workbuddy_executable_exists();
    AIProviderImportStatus {
        target: "workbuddy".to_string(),
        state: if imported { "imported" } else { "not_imported" }.to_string(),
        client_detected,
        detail: if imported {
            format!("检测到 WorkBuddy 中的 {} 个 HiMind 模型", models.len())
        } else if client_detected {
            "已检测到 WorkBuddy，尚未导入 HiMind AI".to_string()
        } else {
            "未检测到 WorkBuddy".to_string()
        },
        config_path: path.to_string_lossy().to_string(),
        models,
        synced_at: String::new(),
    }
}

fn parse_vscode_import_status(content: &str) -> Result<VSCodeImportStatusFile, serde_json::Error> {
    serde_json::from_str(content)
}

fn managed_workbuddy_model_ids(root: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    root.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| is_managed_workbuddy_model(item))
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .filter(|id| seen.insert((*id).to_string()))
        .map(str::to_string)
        .collect()
}

fn cancel_vscode(options: &Options) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let path = vscode_import_status_path(options);
    let client_detected = locate_vscode_cli().is_some();
    if !path.is_file() {
        return Ok(AIProviderImportCancelResult {
            ok: true,
            target: "vscode".to_string(),
            status: "not_imported".to_string(),
            changed: false,
            client_detected,
            detail: "VS Code 当前没有 HiMind 导入记录".to_string(),
            backup_path: String::new(),
        });
    }
    let cli = locate_vscode_cli().ok_or("未检测到 VS Code，无法取消导入")?;
    launch_vscode(&cli, "vscode://himind.himind-ai/disconnect")?;
    Ok(AIProviderImportCancelResult {
        ok: true,
        target: "vscode".to_string(),
        status: "cancellation_opened".to_string(),
        changed: true,
        client_detected: true,
        detail: "已通知 VS Code 扩展清除 HiMind 凭据".to_string(),
        backup_path: String::new(),
    })
}

fn cancel_workbuddy() -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let path = workbuddy_models_path();
    let original = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let (updated, removed) = remove_workbuddy_models(&original)?;
    let backup = if removed > 0 {
        backup_and_write(&path, updated.as_bytes())?
    } else {
        None
    };
    Ok(AIProviderImportCancelResult {
        ok: true,
        target: "workbuddy".to_string(),
        status: if removed > 0 {
            "cancelled"
        } else {
            "not_imported"
        }
        .to_string(),
        changed: removed > 0,
        client_detected: workbuddy_executable_exists(),
        detail: if removed > 0 {
            format!("已移除 {removed} 个 HiMind 模型")
        } else {
            "WorkBuddy 当前没有 HiMind 模型".to_string()
        },
        backup_path: backup
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
    })
}

fn remove_workbuddy_models(content: &str) -> Result<(String, usize), Box<dyn Error>> {
    if content.trim().is_empty() {
        return Ok((String::new(), 0));
    }
    let mut root = serde_json::from_str::<Value>(content)
        .map_err(|_| "WorkBuddy models.json 格式无效，已停止取消导入且未覆盖原文件")?;
    let object = root
        .as_object_mut()
        .ok_or("WorkBuddy models.json 根节点必须是 JSON 对象")?;
    let Some(models_value) = object.get_mut("models") else {
        return Ok((content.to_string(), 0));
    };
    let models = models_value
        .as_array_mut()
        .ok_or_else(|| "WorkBuddy models.json 的 models 必须是数组".to_string())?;
    let mut removed_ids = HashSet::new();
    let mut removed_count = 0usize;
    models.retain(|item| {
        if is_managed_workbuddy_model(item) {
            removed_count += 1;
            if let Some(id) = item.get("id").and_then(Value::as_str) {
                removed_ids.insert(id.to_string());
            }
            false
        } else {
            true
        }
    });
    if let Some(available) = object
        .get_mut("availableModels")
        .and_then(Value::as_array_mut)
    {
        available.retain(|item| {
            item.as_str()
                .map(|id| !removed_ids.contains(id))
                .unwrap_or(true)
        });
    }
    let removed = removed_count;
    if removed == 0 {
        return Ok((content.to_string(), 0));
    }
    Ok((
        format!("{}\n", serde_json::to_string_pretty(&root)?),
        removed,
    ))
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

// cc-switch v3.16+ 以供应商 settings_config.modelCatalog 为模型列表唯一事实源：
// 启用供应商时生成 ~/.codex/cc-switch-model-catalog.json 并注入 model_catalog_json，
// Codex 重启后 /model 才能列出第三方模型；官方 deep link 协议无法携带该字段。
// config.toml 采用保留式合并：HiMind 只接管 auth、默认模型、provider 端点与模型目录，
// notify、mcp_servers 等用户在 cc-switch 中回填的自定义段原样保留。
fn build_cc_switch_provider_settings(
    credential: &AIClientCredential,
    models: &[String],
    preferred: &str,
    existing: Option<&Value>,
) -> Result<String, Box<dyn Error>> {
    let endpoint = normalized_base_url(&credential.access.base_url)?;
    let mut document =
        match existing.and_then(|settings| settings.get("config").and_then(Value::as_str)) {
            Some(text) => text
                .parse::<DocumentMut>()
                .map_err(|error| format!("CC Switch 既有 config.toml 格式无效：{error}"))?,
            None => DocumentMut::default(),
        };
    document["model_provider"] = value("custom");
    document["model"] = value(preferred);
    if document.get("model_reasoning_effort").is_none() {
        document["model_reasoning_effort"] = value("high");
    }
    if document.get("disable_response_storage").is_none() {
        document["disable_response_storage"] = value(true);
    }
    let providers = document
        .as_table_mut()
        .entry("model_providers")
        .or_insert_with(|| {
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_mut()
        .ok_or("CC Switch 既有 config 的 model_providers 不是表")?;
    let provider = providers
        .entry("custom")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or("CC Switch 既有 config 的 model_providers.custom 不是表")?;
    provider["name"] = value(MANAGED_VENDOR);
    provider["base_url"] = value(endpoint.as_str());
    provider["wire_api"] = value("responses");
    provider["requires_openai_auth"] = value(true);

    let previous_entries = existing
        .and_then(|settings| settings.pointer("/modelCatalog/models"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let catalog = models
        .iter()
        .map(|model| {
            previous_entries
                .iter()
                .find(|entry| entry.get("model").and_then(Value::as_str) == Some(model.as_str()))
                .cloned()
                .unwrap_or_else(|| json!({ "model": model, "displayName": model }))
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "auth": { "OPENAI_API_KEY": credential.api_key },
        "config": document.to_string(),
        "modelCatalog": { "models": catalog },
    })
    .to_string())
}

// CC Switch 是长驻 GUI 且持有数据库连接，外部写库需等待其释放锁，否则 SQLITE_BUSY 立即失败。
fn open_cc_switch_database(path: &Path) -> Result<Connection, Box<dyn Error>> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(15))?;
    Ok(connection)
}

fn write_cc_switch_provider(
    path: &Path,
    settings: &str,
    website: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let connection = open_cc_switch_database(path)?;
    let has_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='providers')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Err("CC Switch 数据库结构未就绪，请先打开一次 CC Switch".into());
    }
    let backup = backup_sqlite_database(&connection, path)?;
    let transaction = connection.unchecked_transaction()?;
    let was_current: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM providers WHERE app_type = 'codex' AND id LIKE 'himind-%' AND is_current = 1)",
        [],
        |row| row.get(0),
    )?;
    // CC Switch 是长驻 GUI，其内存供应商列表仍引用既有 HiMind 行的 id；复用该 id
    // （优先 current 行）可避免“供应商不存在”的悬空引用，仅当没有历史行时才新建。
    let target_id: String = transaction
        .query_row(
            "SELECT id FROM providers WHERE app_type = 'codex' AND id LIKE 'himind-%'
             ORDER BY CASE WHEN is_current = 1 THEN 0 ELSE 1 END,
                      CASE WHEN id = ?1 THEN 0 ELSE 1 END LIMIT 1",
            params![CC_SWITCH_PROVIDER_ID],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| CC_SWITCH_PROVIDER_ID.to_string());
    transaction.execute(
        "DELETE FROM provider_endpoints WHERE app_type = 'codex' AND provider_id IN (SELECT id FROM providers WHERE app_type = 'codex' AND id LIKE 'himind-%' AND id <> ?1)",
        params![target_id],
    )?;
    transaction.execute(
        "DELETE FROM providers WHERE app_type = 'codex' AND id LIKE 'himind-%' AND id <> ?1",
        params![target_id],
    )?;
    transaction.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, website_url, created_at, is_current)
         VALUES (?1, 'codex', ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id, app_type) DO UPDATE SET
           name = excluded.name,
           settings_config = excluded.settings_config,
           website_url = excluded.website_url",
        params![
            target_id,
            MANAGED_VENDOR,
            settings,
            website,
            unix_now_millis() as i64,
            was_current
        ],
    )?;
    transaction.commit()?;
    Ok(backup)
}

fn read_cc_switch_managed_settings(path: &Path) -> Result<Option<Value>, Box<dyn Error>> {
    let connection = open_cc_switch_database(path)?;
    let has_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='providers')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(None);
    }
    let settings: Option<String> = connection
        .query_row(
            "SELECT settings_config FROM providers WHERE app_type = 'codex' AND id LIKE 'himind-%' ORDER BY CASE WHEN id = 'himind-codex' THEN 0 ELSE 1 END LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();
    match settings {
        Some(text) => Ok(Some(
            serde_json::from_str(&text).map_err(|_| "CC Switch 中的 HiMind 供应商配置无法解析")?,
        )),
        None => Ok(None),
    }
}

fn read_cc_switch_managed_models(path: &Path) -> Result<Option<Vec<String>>, Box<dyn Error>> {
    let settings = read_cc_switch_managed_settings(path)?;
    Ok(Some(
        settings
            .as_ref()
            .and_then(|value| value.pointer("/modelCatalog/models"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("model").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    ))
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

fn vscode_import_status_path(options: &Options) -> PathBuf {
    options
        .state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(VSCODE_IMPORT_STATUS_FILE)
}

fn cc_switch_database_path() -> PathBuf {
    if let Some(path) = env::var_os("HIMIND_CC_SWITCH_DATABASE") {
        return PathBuf::from(path);
    }
    user_home().join(".cc-switch").join("cc-switch.db")
}

fn cc_switch_managed_provider_count(path: &Path) -> Result<usize, Box<dyn Error>> {
    let connection = open_cc_switch_database(path)?;
    let has_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='providers')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(0);
    }
    let count: usize = connection.query_row(
        "SELECT COUNT(*) FROM providers WHERE app_type = 'codex' AND name = 'HiMind' AND id LIKE 'himind-%'",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

fn cancel_cc_switch() -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let path = cc_switch_database_path();
    let client_detected =
        cc_switch_protocol_registered() || running_cc_switch_executable().is_some();
    if !path.is_file() {
        return Ok(AIProviderImportCancelResult {
            ok: true,
            target: "cc-switch".to_string(),
            status: "not_imported".to_string(),
            changed: false,
            client_detected,
            detail: "CC Switch 当前没有 HiMind 导入记录".to_string(),
            backup_path: String::new(),
        });
    }
    let count = cc_switch_managed_provider_count(&path)?;
    if count == 0 {
        return Ok(AIProviderImportCancelResult {
            ok: true,
            target: "cc-switch".to_string(),
            status: "not_imported".to_string(),
            changed: false,
            client_detected,
            detail: "CC Switch 当前没有 HiMind 导入记录".to_string(),
            backup_path: String::new(),
        });
    }
    let connection = open_cc_switch_database(&path)?;
    let backup = backup_sqlite_database(&connection, &path)?;
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "DELETE FROM provider_endpoints WHERE app_type = 'codex' AND provider_id IN (SELECT id FROM providers WHERE app_type = 'codex' AND name = 'HiMind' AND id LIKE 'himind-%')",
        [],
    )?;
    let removed = transaction.execute(
        "DELETE FROM providers WHERE app_type = 'codex' AND name = 'HiMind' AND id LIKE 'himind-%'",
        [],
    )?;
    transaction.commit()?;
    Ok(AIProviderImportCancelResult {
        ok: true,
        target: "cc-switch".to_string(),
        status: "cancelled".to_string(),
        changed: removed > 0,
        client_detected,
        detail: format!("已从 CC Switch 移除 {removed} 个 HiMind 供应商"),
        backup_path: backup.to_string_lossy().to_string(),
    })
}

fn backup_sqlite_database(
    connection: &Connection,
    database_path: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    let file_name = database_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("cc-switch.db");
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
    let _lock = VSCODE_EXTENSION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "VS Code 导入锁不可用")?;
    let cli = locate_vscode_cli()
        .ok_or("未检测到 VS Code，请先安装；便携版可配置 HIMIND_VSCODE_CLI 指向 bin\\code.cmd")?;
    let vsix = bundled_vscode_vsix_path()?;
    let bundled_version = read_vscode_vsix_version(&vsix)?;
    let installed_version = installed_vscode_extension_version(&cli)?;
    let install_required =
        vscode_extension_install_required(installed_version.as_deref(), &bundled_version)?;
    if install_required {
        install_vscode_extension(&cli, &vsix)?;
        let installed = wait_for_vscode_extension_version(&cli)?
            .ok_or("VS Code CLI 已返回安装成功，但未检测到 HiMind AI 扩展")?;
        if compare_extension_versions(&installed, &bundled_version)? == Ordering::Less {
            return Err(format!(
                "HiMind AI 扩展安装校验失败：当前版本 {installed}，内置版本 {bundled_version}"
            )
            .into());
        }
    }
    ensure_vscode_chat_provider_allowlist(&cli)?;
    Ok(cli)
}

/// Reconcile a previously imported VS Code installation after Agent startup.
/// VS Code updates install a new version directory and replace product.json;
/// repairing here keeps the provider available after ordinary upgrades without
/// requiring the user to repeat the import flow.
pub(crate) fn reconcile_vscode_import(options: &Options) {
    if !vscode_import_status_path(options).is_file() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("himind-vscode-reconcile".to_string())
        .spawn(|| match ensure_vscode_extension() {
            Ok(_) => {}
            Err(error) => eprintln!("VS Code HiMind import reconciliation skipped: {error}"),
        });
}

/// The Language Model Chat Provider API is still a VS Code proposal. Unlike a
/// launch flag, the product allowlist survives ordinary desktop launches and
/// window restarts. Keep the change local to the installed VS Code version and
/// retain a timestamped backup so an update or uninstall can restore the file.
fn ensure_vscode_chat_provider_allowlist(cli: &Path) -> Result<(), Box<dyn Error>> {
    let install_root = cli
        .parent()
        .and_then(Path::parent)
        .ok_or("无法定位 VS Code 安装目录")?;
    let mut product_paths = Vec::new();
    let direct = install_root.join("resources/app/product.json");
    if direct.is_file() {
        product_paths.push(direct);
    }
    if let Ok(entries) = fs::read_dir(install_root) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("resources/app/product.json");
            if candidate.is_file() {
                product_paths.push(candidate);
            }
        }
    }
    product_paths.sort();
    product_paths.dedup();
    if product_paths.is_empty() {
        return Err("无法找到 VS Code product.json，无法持久启用 HiMind 模型 Provider".into());
    }
    for product_path in product_paths {
        let original = fs::read(&product_path)?;
        let mut product: Value = serde_json::from_slice(&original)
            .map_err(|error| format!("VS Code product.json 格式无效：{error}"))?;
        if !product
            .get("extensionEnabledApiProposals")
            .is_some_and(Value::is_object)
        {
            product["extensionEnabledApiProposals"] = json!({});
        }
        let proposals = product
            .get_mut("extensionEnabledApiProposals")
            .and_then(Value::as_object_mut)
            .ok_or("VS Code product.json 的 extensionEnabledApiProposals 格式无效")?;
        let entry = proposals
            .entry(VSCODE_EXTENSION_ID.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let list = entry
            .as_array_mut()
            .ok_or("VS Code product.json 的 HiMind API 白名单格式无效")?;
        if list
            .iter()
            .any(|item| item.as_str() == Some(VSCODE_CHAT_PROVIDER_PROPOSAL))
        {
            continue;
        }
        list.push(Value::String(VSCODE_CHAT_PROVIDER_PROPOSAL.to_string()));

        let backup = product_path.with_file_name(format!(
            "product.json.himind-backup-{}.json",
            unix_now_millis()
        ));
        fs::copy(&product_path, &backup)?;
        let temporary = product_path.with_file_name("product.json.himind.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&product)?)?;
        if let Err(error) =
            fs::remove_file(&product_path).and_then(|_| fs::rename(&temporary, &product_path))
        {
            let _ = fs::remove_file(&temporary);
            let _ = fs::copy(&backup, &product_path);
            return Err(format!(
                "无法更新 VS Code product.json（备份位于 {}）：{error}",
                backup.display()
            )
            .into());
        }
    }
    Ok(())
}

fn locate_vscode_cli() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(value) = env::var_os("HIMIND_VSCODE_CLI") {
        candidates.push(PathBuf::from(value));
    }
    candidates.extend(vscode_running_process_candidates());
    candidates.extend(vscode_registry_candidates());
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        let root = PathBuf::from(local_app_data).join("Programs");
        candidates.push(root.join("Microsoft VS Code/bin/code.cmd"));
        candidates.push(root.join("Microsoft VS Code Insiders/bin/code-insiders.cmd"));
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(program_files) = env::var_os(variable) {
            let root = PathBuf::from(program_files);
            candidates.push(root.join("Microsoft VS Code/bin/code.cmd"));
            candidates.push(root.join("Microsoft VS Code Insiders/bin/code-insiders.cmd"));
        }
    }
    if cfg!(windows) {
        candidates.push(PathBuf::from(r"C:\Programs\Microsoft VS Code\bin\code.cmd"));
        candidates.push(PathBuf::from(
            r"C:\Programs\Microsoft VS Code Insiders\bin\code-insiders.cmd",
        ));
    }
    candidates.extend(vscode_path_candidates());
    candidates.push(PathBuf::from("code"));
    candidates.push(PathBuf::from("code-insiders"));

    let mut seen = HashSet::new();
    candidates.into_iter().find_map(|candidate| {
        let key = candidate.to_string_lossy().to_ascii_lowercase();
        if !seen.insert(key) {
            return None;
        }
        resolve_vscode_cli_candidate(&candidate)
    })
}

fn resolve_vscode_cli_candidate(candidate: &Path) -> Option<PathBuf> {
    if candidate.components().count() == 1 {
        return vscode_path_command(candidate);
    }
    vscode_cli_available(candidate).then(|| candidate.to_path_buf())
}

#[cfg(windows)]
fn vscode_path_command(command: &Path) -> Option<PathBuf> {
    let output = Command::new("where.exe")
        .arg(command)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .find(|path| vscode_cli_available(path))
}

#[cfg(not(windows))]
fn vscode_path_command(command: &Path) -> Option<PathBuf> {
    vscode_cli_available(command).then(|| command.to_path_buf())
}

fn vscode_path_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for root in [
        "LOCALAPPDATA",
        "USERPROFILE",
        "ProgramFiles",
        "ProgramFiles(x86)",
    ] {
        let Some(root) = env::var_os(root).map(PathBuf::from) else {
            continue;
        };
        for relative in [
            "Microsoft VS Code/bin/code.cmd",
            "Microsoft VS Code Insiders/bin/code-insiders.cmd",
            "scoop/apps/vscode/current/bin/code.cmd",
            "scoop/apps/vscode-insiders/current/bin/code-insiders.cmd",
        ] {
            candidates.push(root.join(relative));
        }
    }
    candidates
}

#[cfg(windows)]
fn vscode_running_process_candidates() -> Vec<PathBuf> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('Code.exe','Code - Insiders.exe') -and $_.ExecutablePath } | Select-Object -ExpandProperty ExecutablePath",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter_map(|value| vscode_cli_from_executable(Path::new(value)))
        .collect()
}

#[cfg(not(windows))]
fn vscode_running_process_candidates() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(windows)]
fn vscode_registry_candidates() -> Vec<PathBuf> {
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    use winreg::RegKey;

    let mut candidates = Vec::new();
    for (root, key_path) in [
        (
            RegKey::predef(HKEY_CURRENT_USER),
            r"Software\Microsoft\Windows\CurrentVersion\App Paths",
        ),
        (
            RegKey::predef(HKEY_LOCAL_MACHINE),
            r"Software\Microsoft\Windows\CurrentVersion\App Paths",
        ),
    ] {
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            for executable in ["Code.exe", "code-insiders.exe"] {
                if let Ok(key) = root
                    .open_subkey_with_flags(format!(r"{key_path}\{executable}"), KEY_READ | view)
                {
                    if let Ok(value) = key.get_value::<String, _>("") {
                        push_vscode_registry_value(&mut candidates, &value);
                    }
                }
            }
        }
    }
    for (root, key_path) in [
        (
            RegKey::predef(HKEY_CURRENT_USER),
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            RegKey::predef(HKEY_LOCAL_MACHINE),
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
    ] {
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let Ok(uninstall) = root.open_subkey_with_flags(key_path, KEY_READ | view) else {
                continue;
            };
            for child_name in uninstall.enum_keys().flatten() {
                let Ok(child) = uninstall.open_subkey_with_flags(&child_name, KEY_READ | view)
                else {
                    continue;
                };
                let display_name = child
                    .get_value::<String, _>("DisplayName")
                    .unwrap_or_default();
                if !display_name
                    .to_ascii_lowercase()
                    .contains("visual studio code")
                {
                    continue;
                }
                for value_name in ["InstallLocation", "DisplayIcon", "UninstallString"] {
                    if let Ok(value) = child.get_value::<String, _>(value_name) {
                        push_vscode_registry_value(&mut candidates, &value);
                    }
                }
            }
        }
    }
    candidates
}

#[cfg(not(windows))]
fn vscode_registry_candidates() -> Vec<PathBuf> {
    Vec::new()
}

fn push_vscode_registry_value(candidates: &mut Vec<PathBuf>, value: &str) {
    let trimmed = value.trim().trim_matches('"');
    let path = if let Some(end) = trimmed.to_ascii_lowercase().find(".exe") {
        PathBuf::from(trimmed[..end + 4].trim_matches('"'))
    } else {
        PathBuf::from(trimmed)
    };
    let looks_like_executable = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("exe"));
    if path.is_dir() || !looks_like_executable {
        candidates.push(path.join("bin/code.cmd"));
        candidates.push(path.join("bin/code-insiders.cmd"));
    } else if let Some(cli) = vscode_cli_from_executable(&path) {
        candidates.push(cli);
    }
}

fn vscode_cli_from_executable(path: &Path) -> Option<PathBuf> {
    let name = path.file_name().and_then(|value| value.to_str())?;
    let cli = if name.eq_ignore_ascii_case("code.exe") {
        "code.cmd"
    } else if name.eq_ignore_ascii_case("code-insiders.exe")
        || name.eq_ignore_ascii_case("code - insiders.exe")
    {
        "code-insiders.cmd"
    } else {
        return None;
    };
    Some(path.parent()?.join("bin").join(cli))
}

fn vscode_cli_available(cli: &Path) -> bool {
    run_vscode_command(vscode_command(cli).arg("--version"), Duration::from_secs(3))
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn installed_vscode_extension_version(cli: &Path) -> Result<Option<String>, Box<dyn Error>> {
    let output = run_vscode_command(
        vscode_command(cli).args(["--list-extensions", "--show-versions"]),
        Duration::from_secs(8),
    )?;
    if !output.status.success() {
        return Err(format!(
            "无法检查 VS Code 扩展：{}",
            command_error_detail(&output.stdout, &output.stderr)
        )
        .into());
    }
    parse_vscode_extension_version(&String::from_utf8_lossy(&output.stdout)).map_err(Into::into)
}

fn wait_for_vscode_extension_version(cli: &Path) -> Result<Option<String>, Box<dyn Error>> {
    for attempt in 0..5 {
        if let Some(version) = installed_vscode_extension_version(cli)? {
            return Ok(Some(version));
        }
        if attempt < 4 {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    Ok(None)
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
        if directory
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("versions"))
        {
            if let Some(install_root) = directory.parent().and_then(Path::parent) {
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
    let output = run_vscode_command(
        vscode_command(cli)
            .arg("--install-extension")
            .arg(vsix)
            .arg("--force"),
        Duration::from_secs(30),
    )?;
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

fn run_vscode_command(command: &mut Command, timeout: Duration) -> Result<Output, Box<dyn Error>> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("VS Code CLI 执行超时（{} 秒）", timeout.as_secs()).into());
        }
        std::thread::sleep(Duration::from_millis(40));
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
    let output = run_vscode_command(
        vscode_command(cli).args(["--reuse-window", "--open-url", enrollment_url]),
        Duration::from_secs(15),
    )?;
    if !output.status.success() {
        return Err(format!(
            "无法唤起 VS Code 完成 HiMind 授权：{}",
            command_error_detail(&output.stdout, &output.stderr)
        )
        .into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn launch_vscode(cli: &Path, enrollment_url: &str) -> Result<(), Box<dyn Error>> {
    let output = run_vscode_command(
        vscode_command(cli).args(["--reuse-window", "--open-url", enrollment_url]),
        Duration::from_secs(15),
    )?;
    if !output.status.success() {
        return Err(format!(
            "无法唤起 VS Code 完成 HiMind 授权：{}",
            command_error_detail(&output.stdout, &output.stderr)
        )
        .into());
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

#[cfg(test)]
mod tests {
    use super::{
        build_cc_switch_provider_settings, build_codex_config_toml, build_codex_models_json,
        build_vscode_enrollment_url, bundled_vscode_vsix_candidates, chat_completions_url,
        compare_extension_versions, consume_vscode_enrollment, create_vscode_enrollment,
        ensure_vscode_chat_provider_allowlist, legacy_workbuddy_model_id,
        managed_workbuddy_model_ids, merge_workbuddy_models, migrate_workbuddy_sessions,
        parse_vscode_extension_version, parse_vscode_import_status, push_vscode_registry_value,
        read_cc_switch_managed_models, read_cc_switch_managed_settings, read_codex_model_catalog,
        remove_workbuddy_models, strip_codex_himind, vscode_extension_install_required,
        workbuddy_model_id, workbuddy_models_path_in, write_cc_switch_provider, AIClientCredential,
        VSCODE_CHAT_PROVIDER_PROPOSAL, VSCODE_EXTENSION_ID,
    };
    use crate::api::ai::AIUserCredential;
    use serde_json::{json, Value};
    use std::cmp::Ordering;
    use std::path::{Path, PathBuf};

    fn credential(models: &[&str]) -> AIClientCredential {
        AIClientCredential {
            access: AIUserCredential {
                active_entitlement_id: "ent-1".to_string(),
                active_personal_connection_id: String::new(),
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
    fn builds_codex_models_catalog_with_himind_models() {
        let catalog = build_codex_models_json(&[
            "deepseek-v4-flash".to_string(),
            "deepseek-v4-pro".to_string(),
        ])
        .unwrap();
        let root: Value = serde_json::from_str(&catalog).unwrap();
        let models = root.get("models").and_then(Value::as_array).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(
            models[0].get("slug").and_then(Value::as_str),
            Some("deepseek-v4-flash")
        );
        assert_eq!(models[0].get("priority").and_then(Value::as_i64), Some(1));
        assert_eq!(models[1].get("priority").and_then(Value::as_i64), Some(2));
        assert_eq!(
            models[0].get("context_window").and_then(Value::as_i64),
            Some(1048576)
        );
        assert_eq!(
            models[0].get("visibility").and_then(Value::as_str),
            Some("list")
        );
    }

    #[test]
    fn codex_config_merge_preserves_user_sections() {
        let original = r#"model_reasoning_effort = "medium"

notify = ["codex-notify.exe", "turn-ended"]

[mcp_servers.unityMCP]
type = "stdio"
command = "uvx.exe"

[model_providers.deepseek]
name = "deepseek"
base_url = "https://api.deepseek.com/"
wire_api = "responses"
"#;
        let config = build_codex_config_toml(
            original,
            &credential(&["deepseek-v4-flash"]),
            Path::new(r"C:\Users\Admin\.codex\himind-models.json"),
            "deepseek-v4-flash",
        )
        .unwrap();
        assert!(config.contains("model_provider = \"himind\""));
        assert!(config.contains("model = \"deepseek-v4-flash\""));
        assert!(config.contains("preferred_auth_method = \"apikey\""));
        assert!(config.contains("forced_login_method = \"api\""));
        assert!(
            config.contains("model_catalog_json = \"C:/Users/Admin/.codex/himind-models.json\"")
        );
        assert!(config.contains("[model_providers.himind]"));
        assert!(config.contains("experimental_bearer_token = \"test-secret-key\""));
        assert!(config.contains("base_url = \"https://ai.example.com/v1\""));
        assert!(config.contains("model_reasoning_effort = \"high\""));
        assert!(config.contains("notify = [\"codex-notify.exe\", \"turn-ended\"]"));
        assert!(config.contains("[mcp_servers.unityMCP]"));
        assert!(config.contains("[model_providers.deepseek]"));
        assert!(config.contains("base_url = \"https://api.deepseek.com/\""));
    }

    #[test]
    fn codex_config_merge_rejects_invalid_existing_toml() {
        let error = build_codex_config_toml(
            "model = [",
            &credential(&["deepseek-v4-flash"]),
            Path::new(r"C:\Users\Admin\.codex\himind-models.json"),
            "deepseek-v4-flash",
        )
        .unwrap_err();
        assert!(error.to_string().contains("Codex config.toml 格式无效"));
    }

    #[test]
    fn codex_strip_only_removes_himind_owned_fields() {
        let original = r#"model = "deepseek-v4-flash"
model_provider = "himind"
preferred_auth_method = "apikey"
forced_login_method = "api"
model_catalog_json = "C:/Users/Admin/.codex/himind-models.json"

notify = ["codex-notify.exe"]

[model_providers.himind]
name = "HiMind"
base_url = "https://himind.andcrane.com/gateway/v1"
wire_api = "responses"
experimental_bearer_token = "sk-x"

[mcp_servers.unityMCP]
command = "uvx.exe"
"#;
        let (updated, changed) = strip_codex_himind(
            original,
            Path::new(r"C:\Users\Admin\.codex\himind-models.json"),
        )
        .unwrap();
        assert!(changed);
        assert!(!updated.contains("model_provider"));
        assert!(!updated.contains("model_catalog_json"));
        assert!(!updated.contains("[model_providers.himind]"));
        assert!(!updated.contains("experimental_bearer_token"));
        assert!(updated.contains("model = \"deepseek-v4-flash\""));
        assert!(updated.contains("preferred_auth_method = \"apikey\""));
        assert!(updated.contains("notify = [\"codex-notify.exe\"]"));
        assert!(updated.contains("[mcp_servers.unityMCP]"));
    }

    #[test]
    fn builds_cc_switch_settings_with_full_model_catalog() {
        let value = build_cc_switch_provider_settings(
            &credential(&["deepseek-v4-flash", "deepseek-v4-pro"]),
            &[
                "deepseek-v4-flash".to_string(),
                "deepseek-v4-pro".to_string(),
            ],
            "deepseek-v4-flash",
            None,
        )
        .unwrap();
        let settings: Value = serde_json::from_str(&value).unwrap();
        assert_eq!(
            settings
                .pointer("/auth/OPENAI_API_KEY")
                .and_then(Value::as_str),
            Some("test-secret-key")
        );
        let config = settings.get("config").and_then(Value::as_str).unwrap();
        assert!(config.contains("model_provider = \"custom\""));
        assert!(config.contains("model = \"deepseek-v4-flash\""));
        assert!(config.contains("base_url = \"https://ai.example.com/v1\""));
        assert!(config.contains("wire_api = \"responses\""));
        let models = settings
            .pointer("/modelCatalog/models")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(
            models[1].get("model").and_then(Value::as_str),
            Some("deepseek-v4-pro")
        );
    }

    #[test]
    fn cc_switch_merge_rejects_invalid_existing_toml() {
        let existing = json!({"config": "model = ["});
        let error = build_cc_switch_provider_settings(
            &credential(&["deepseek-v4-flash"]),
            &["deepseek-v4-flash".to_string()],
            "deepseek-v4-flash",
            Some(&existing),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("CC Switch 既有 config.toml 格式无效"));
    }

    #[test]
    fn codex_model_catalog_can_be_read_without_config() {
        let path = std::env::temp_dir().join(format!(
            "himind-codex-models-test-{}-{}.json",
            std::process::id(),
            super::unix_now_millis()
        ));
        std::fs::write(
            &path,
            r#"{"models":[{"slug":"deepseek-v4-flash"},{"slug":"deepseek-v4-pro"}]}"#,
        )
        .unwrap();
        let models = read_codex_model_catalog(&path).unwrap();
        assert_eq!(
            models,
            vec![
                "deepseek-v4-flash".to_string(),
                "deepseek-v4-pro".to_string()
            ]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn cc_switch_merge_preserves_user_config_and_catalog_metadata() {
        let existing = json!({
            "auth": { "OPENAI_API_KEY": "stale-key" },
            "config": "model_provider = \"custom\"\nmodel = \"old-model\"\nmodel_reasoning_effort = \"medium\"\ndisable_response_storage = true\n\nnotify = [\"codex-notify.exe\", \"turn-ended\"]\n\n[model_providers.custom]\nname = \"HiMind\"\nbase_url = \"https://old.example/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true\n\n[mcp_servers.unityMCP]\ntype = \"stdio\"\ncommand = \"uvx.exe\"\n",
            "modelCatalog": {
                "models": [
                    { "model": "deepseek-v4-flash", "displayName": "Flash 自定义名", "contextWindow": 131072 }
                ]
            }
        });
        let value = build_cc_switch_provider_settings(
            &credential(&["deepseek-v4-flash", "deepseek-v4-pro"]),
            &[
                "deepseek-v4-flash".to_string(),
                "deepseek-v4-pro".to_string(),
            ],
            "deepseek-v4-flash",
            Some(&existing),
        )
        .unwrap();
        let settings: Value = serde_json::from_str(&value).unwrap();
        assert_eq!(
            settings
                .pointer("/auth/OPENAI_API_KEY")
                .and_then(Value::as_str),
            Some("test-secret-key")
        );
        let config = settings.get("config").and_then(Value::as_str).unwrap();
        assert!(config.contains("model = \"deepseek-v4-flash\""));
        assert!(config.contains("base_url = \"https://ai.example.com/v1\""));
        assert!(config.contains("model_reasoning_effort = \"medium\""));
        assert!(config.contains("notify = [\"codex-notify.exe\", \"turn-ended\"]"));
        assert!(config.contains("[mcp_servers.unityMCP]"));
        let models = settings
            .pointer("/modelCatalog/models")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(
            models[0].get("displayName").and_then(Value::as_str),
            Some("Flash 自定义名")
        );
        assert_eq!(
            models[0].get("contextWindow").and_then(Value::as_i64),
            Some(131072)
        );
        assert_eq!(
            models[1].get("displayName").and_then(Value::as_str),
            Some("deepseek-v4-pro")
        );
    }

    #[test]
    fn cc_switch_upsert_reuses_legacy_id_and_keeps_current_flag() {
        let directory = std::env::temp_dir().join(format!(
            "himind-cc-switch-test-{}-{}",
            std::process::id(),
            super::unix_now_millis()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("cc-switch.db");
        {
            let connection = super::Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE providers (id TEXT NOT NULL, app_type TEXT NOT NULL, name TEXT NOT NULL, settings_config TEXT NOT NULL, website_url TEXT, category TEXT, created_at INTEGER, sort_index INTEGER, notes TEXT, icon TEXT, icon_color TEXT, meta TEXT NOT NULL DEFAULT '{}', is_current BOOLEAN NOT NULL DEFAULT 0, in_failover_queue BOOLEAN NOT NULL DEFAULT 0, cost_multiplier TEXT NOT NULL DEFAULT '1.0', limit_daily_usd TEXT, limit_monthly_usd TEXT, provider_type TEXT, PRIMARY KEY (id, app_type));
                     CREATE TABLE provider_endpoints (app_type TEXT NOT NULL, provider_id TEXT NOT NULL, url TEXT NOT NULL);",
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO providers (id, app_type, name, settings_config, is_current) VALUES ('himind-legacy', 'codex', 'HiMind', '{}', 1)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO provider_endpoints (app_type, provider_id, url) VALUES ('codex', 'himind-legacy', 'https://legacy.example')",
                    [],
                )
                .unwrap();
        }
        let settings = build_cc_switch_provider_settings(
            &credential(&["deepseek-v4-flash"]),
            &["deepseek-v4-flash".to_string()],
            "deepseek-v4-flash",
            None,
        )
        .unwrap();
        let backup = write_cc_switch_provider(&path, &settings, "https://ai.example.com").unwrap();
        assert!(backup.is_file());

        let models = read_cc_switch_managed_models(&path).unwrap().unwrap();
        assert_eq!(models, vec!["deepseek-v4-flash".to_string()]);
        assert!(read_cc_switch_managed_settings(&path).unwrap().is_some());

        let connection = super::Connection::open(&path).unwrap();
        // 复用既有 himind-% 行的 id，避免 CC Switch 内存引用悬空
        let (id, name, website, is_current): (String, String, String, i64) = connection
            .query_row(
                "SELECT id, name, website_url, is_current FROM providers WHERE app_type = 'codex' AND id LIKE 'himind-%'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(id, "himind-legacy");
        assert_eq!(name, "HiMind");
        assert_eq!(website, "https://ai.example.com");
        assert_eq!(is_current, 1);

        write_cc_switch_provider(&path, &settings, "https://ai.example.com").unwrap();
        let (managed_count, still_current): (i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), MAX(is_current) FROM providers WHERE app_type = 'codex' AND id LIKE 'himind-%'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(managed_count, 1);
        assert_eq!(still_current, 1);

        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn vscode_enrollment_is_single_use_and_keeps_key_out_of_uri() {
        let code = create_vscode_enrollment(
            credential(&["glm-5.1", "deepseek-v4-flash"]),
            "glm-5.1".to_string(),
            vec!["glm-5.1".to_string(), "deepseek-v4-flash".to_string()],
            r"C:\HiMindAgent\profiles\development\data\vscode-import-status.json".to_string(),
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
        assert_eq!(
            exchanged.import_status_path,
            r"C:\HiMindAgent\profiles\development\data\vscode-import-status.json"
        );
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
    fn persists_chat_provider_allowlist_for_an_installed_vscode_version() {
        let root = std::env::temp_dir().join(format!(
            "himind-vscode-product-{}-{}",
            std::process::id(),
            super::unix_now_millis()
        ));
        let product_path = root.join("version/resources/app/product.json");
        let cli = root.join("bin/code.cmd");
        std::fs::create_dir_all(product_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(
            &product_path,
            serde_json::to_vec(&serde_json::json!({
                "extensionEnabledApiProposals": {"GitHub.copilot-chat": ["chatProvider"]}
            }))
            .unwrap(),
        )
        .unwrap();

        ensure_vscode_chat_provider_allowlist(&cli).unwrap();
        let product: Value =
            serde_json::from_slice(&std::fs::read(&product_path).unwrap()).unwrap();
        assert_eq!(
            product["extensionEnabledApiProposals"][VSCODE_EXTENSION_ID],
            serde_json::json!([VSCODE_CHAT_PROVIDER_PROPOSAL])
        );
        assert!(std::fs::read_dir(product_path.parent().unwrap())
            .unwrap()
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("product.json.himind-backup-")));
        ensure_vscode_chat_provider_allowlist(&cli).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn creates_chat_provider_allowlist_when_product_field_is_missing() {
        let root = std::env::temp_dir().join(format!(
            "himind-vscode-product-missing-{}-{}",
            std::process::id(),
            super::unix_now_millis()
        ));
        let product_path = root.join("resources/app/product.json");
        let cli = root.join("bin/code.cmd");
        std::fs::create_dir_all(product_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(&product_path, br#"{"quality":"stable"}"#).unwrap();

        ensure_vscode_chat_provider_allowlist(&cli).unwrap();
        let product: Value =
            serde_json::from_slice(&std::fs::read(&product_path).unwrap()).unwrap();
        assert_eq!(
            product["extensionEnabledApiProposals"][VSCODE_EXTENSION_ID],
            serde_json::json!([VSCODE_CHAT_PROVIDER_PROPOSAL])
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn derives_cli_candidates_from_registry_install_values() {
        let mut candidates = Vec::new();
        push_vscode_registry_value(
            &mut candidates,
            r#"C:\Users\example\AppData\Local\Programs\Microsoft VS Code\Code.exe"#,
        );
        assert!(candidates
            .iter()
            .any(|path| path.ends_with(r"bin\code.cmd")));

        candidates.clear();
        push_vscode_registry_value(
            &mut candidates,
            r#"C:\Users\example\AppData\Local\Programs\Microsoft VS Code"#,
        );
        assert!(candidates
            .iter()
            .any(|path| path.ends_with(r"bin\code.cmd")));
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
    fn reads_vscode_synced_model_status() {
        let status = parse_vscode_import_status(
            r#"{"imported_at":"2026-08-17T01:00:00Z","synced_at":"2026-08-17T02:00:00Z","models":["glm-5.2","deepseek-v4"]}"#,
        )
        .unwrap();
        assert_eq!(status.models, vec!["glm-5.2", "deepseek-v4"]);
        assert_eq!(status.synced_at, "2026-08-17T02:00:00Z");

        let legacy =
            parse_vscode_import_status(r#"{"imported_at":"2026-08-16T01:00:00Z"}"#).unwrap();
        assert!(legacy.models.is_empty());
        assert!(legacy.synced_at.is_empty());
    }

    #[test]
    fn extracts_only_himind_workbuddy_models() {
        let root = serde_json::json!({
            "models": [
                {"id": "personal", "vendor": "Other"},
                {"id": "glm-5.2", "vendor": "HiMind"},
                {"id": " deepseek-v4 ", "vendor": "HiMind"},
                {"id": "glm-5.2", "vendor": "HiMind"}
            ]
        });
        assert_eq!(
            managed_workbuddy_model_ids(&root),
            vec!["glm-5.2", "deepseek-v4"]
        );
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
    fn removes_only_himind_workbuddy_models_and_available_ids() {
        let source = r#"{
          "models": [
            {"id":"personal","vendor":"Other","apiKey":"keep"},
            {"id":"gpt-4.1","vendor":"HiMind","apiKey":"remove"}
          ],
          "availableModels": ["personal", "gpt-4.1"],
          "theme": "dark"
        }"#;
        let (updated, removed) = remove_workbuddy_models(source).unwrap();
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(root["theme"], "dark");
        assert_eq!(root["models"].as_array().unwrap().len(), 1);
        assert_eq!(root["models"][0]["vendor"], "Other");
        assert_eq!(root["availableModels"], serde_json::json!(["personal"]));
    }

    #[test]
    fn rejects_invalid_json_without_rebuilding_it() {
        let error = merge_workbuddy_models("{broken", &credential(&["gpt-4.1"]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("未覆盖原文件"));
    }
}
