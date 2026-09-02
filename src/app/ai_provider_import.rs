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
const KIMI_CODE_PROVIDER_ID: &str = "himind";
const KIMI_CODE_HIMIND_PREFIX: &str = "himind/";
const KIMI_CODE_DEFAULT_CONTEXT: u64 = 1_048_576;
const QWEN_CODE_PROVIDER_ID: &str = "himind";
const QWEN_CODE_ENV_KEY: &str = "HIMIND_API_KEY";
// Claude Code / Claude Desktop 通过 settings env 块注入 Anthropic 协议端点。
// Anthropic SDK 会在 base_url 后追加 /v1/messages，故 base_url 需剥掉网关路径末尾的 /v1。
const CLAUDE_BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";
const CLAUDE_AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
const CLAUDE_MODEL_ENV: &str = "ANTHROPIC_MODEL";
const CLAUDE_CUSTOM_MODEL_OPTION: &str = "ANTHROPIC_CUSTOM_MODEL_OPTION";
const VSCODE_EXTENSION_ID: &str = "himind.himind-ai";
const VSCODE_CHAT_PROVIDER_PROPOSAL: &str = "chatProvider";
// Keep the handoff short-lived, but long enough for a cold VS Code process,
// extension host startup and antivirus scanning on a first-use machine.
const VSCODE_ENROLLMENT_TTL_SECONDS: u64 = 180;
const MIN_SUPPORTED_VSCODE_VERSION: &str = "1.120.0";
const VSCODE_ENROLLMENT_HANDOFF_FILE: &str = "vscode-enrollment-v2.json";
const VSCODE_IMPORT_STATUS_FILE: &str = "vscode-import-status.json";
const IMPORT_BINDINGS_FILE: &str = "ai-provider-import-bindings.json";

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
    /// 服务源：`managed`（默认，HiMind Dashboard 分发）或 `custom:<id>`（本机自定义服务）。
    #[serde(default)]
    pub service: String,
}

impl AIProviderImportRequest {
    pub(crate) fn service_source(&self) -> &str {
        if self.service.trim().is_empty() {
            "managed"
        } else {
            self.service.trim()
        }
    }
}

/// 每种 AI 客户端的独立 Adapter 契约。
///
/// 各实现负责该客户端的检测、状态、接入计划、写配置、备份与移除；
/// 不允许把客户端特定逻辑复制到 HTTP、Tauri 或 MCP 适配层。
pub(crate) trait AIClientAdapter {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn status(&self, options: &Options) -> AIProviderImportStatus;
    fn plan(&self, action: &str, status: &AIProviderImportStatus) -> AIProviderImportPlan;
    fn import(
        &self,
        options: &Options,
        user_id: &str,
        service: &str,
    ) -> Result<AIProviderImportResult, Box<dyn Error>>;
    fn cancel(&self, options: &Options) -> Result<AIProviderImportCancelResult, Box<dyn Error>>;
}

pub(crate) struct VSCodeAdapter;
pub(crate) struct CCSwitchAdapter;
pub(crate) struct CodexAdapter;
pub(crate) struct WorkBuddyAdapter;

impl AIClientAdapter for VSCodeAdapter {
    fn id(&self) -> &'static str {
        "vscode"
    }
    fn display_name(&self) -> &'static str {
        "VS Code"
    }
    fn status(&self, options: &Options) -> AIProviderImportStatus {
        vscode_import_status(options)
    }
    fn plan(&self, action: &str, status: &AIProviderImportStatus) -> AIProviderImportPlan {
        plan_for("vscode", action, status)
    }
    fn import(
        &self,
        options: &Options,
        user_id: &str,
        service: &str,
    ) -> Result<AIProviderImportResult, Box<dyn Error>> {
        import_vscode(options, user_id, service)
    }
    fn cancel(&self, options: &Options) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_vscode(options)
    }
}

impl AIClientAdapter for CCSwitchAdapter {
    fn id(&self) -> &'static str {
        "cc-switch"
    }
    fn display_name(&self) -> &'static str {
        "CC Switch"
    }
    fn status(&self, _options: &Options) -> AIProviderImportStatus {
        cc_switch_import_status()
    }
    fn plan(&self, action: &str, status: &AIProviderImportStatus) -> AIProviderImportPlan {
        plan_for("cc-switch", action, status)
    }
    fn import(
        &self,
        options: &Options,
        user_id: &str,
        service: &str,
    ) -> Result<AIProviderImportResult, Box<dyn Error>> {
        import_cc_switch(options, user_id, service)
    }
    fn cancel(&self, _options: &Options) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_cc_switch()
    }
}

impl AIClientAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn display_name(&self) -> &'static str {
        "Codex"
    }
    fn status(&self, options: &Options) -> AIProviderImportStatus {
        codex_import_status(options)
    }
    fn plan(&self, action: &str, status: &AIProviderImportStatus) -> AIProviderImportPlan {
        plan_for("codex", action, status)
    }
    fn import(
        &self,
        options: &Options,
        user_id: &str,
        service: &str,
    ) -> Result<AIProviderImportResult, Box<dyn Error>> {
        import_codex(options, user_id, service)
    }
    fn cancel(&self, options: &Options) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_codex(options)
    }
}

impl AIClientAdapter for WorkBuddyAdapter {
    fn id(&self) -> &'static str {
        "workbuddy"
    }
    fn display_name(&self) -> &'static str {
        "WorkBuddy"
    }
    fn status(&self, _options: &Options) -> AIProviderImportStatus {
        workbuddy_import_status()
    }
    fn plan(&self, action: &str, status: &AIProviderImportStatus) -> AIProviderImportPlan {
        plan_for("workbuddy", action, status)
    }
    fn import(
        &self,
        options: &Options,
        user_id: &str,
        service: &str,
    ) -> Result<AIProviderImportResult, Box<dyn Error>> {
        import_workbuddy(options, user_id, service)
    }
    fn cancel(&self, _options: &Options) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_workbuddy()
    }
}

pub(crate) struct KimiCodeAdapter;
pub(crate) struct QwenCodeAdapter;

impl AIClientAdapter for KimiCodeAdapter {
    fn id(&self) -> &'static str {
        "kimi-code"
    }
    fn display_name(&self) -> &'static str {
        "Kimi Code"
    }
    fn status(&self, _options: &Options) -> AIProviderImportStatus {
        kimi_code_import_status()
    }
    fn plan(&self, action: &str, status: &AIProviderImportStatus) -> AIProviderImportPlan {
        plan_for("kimi-code", action, status)
    }
    fn import(
        &self,
        options: &Options,
        user_id: &str,
        service: &str,
    ) -> Result<AIProviderImportResult, Box<dyn Error>> {
        import_kimi_code(options, user_id, service)
    }
    fn cancel(&self, _options: &Options) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_kimi_code()
    }
}

impl AIClientAdapter for QwenCodeAdapter {
    fn id(&self) -> &'static str {
        "qwen-code"
    }
    fn display_name(&self) -> &'static str {
        "Qwen Code"
    }
    fn status(&self, _options: &Options) -> AIProviderImportStatus {
        qwen_code_import_status()
    }
    fn plan(&self, action: &str, status: &AIProviderImportStatus) -> AIProviderImportPlan {
        plan_for("qwen-code", action, status)
    }
    fn import(
        &self,
        options: &Options,
        user_id: &str,
        service: &str,
    ) -> Result<AIProviderImportResult, Box<dyn Error>> {
        import_qwen_code(options, user_id, service)
    }
    fn cancel(&self, _options: &Options) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_qwen_code()
    }
}

pub(crate) struct ClaudeCodeAdapter;
pub(crate) struct ClaudeDesktopAdapter;

impl AIClientAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        "claude-code"
    }
    fn display_name(&self) -> &'static str {
        "Claude Code"
    }
    fn status(&self, _options: &Options) -> AIProviderImportStatus {
        claude_code_import_status()
    }
    fn plan(&self, action: &str, status: &AIProviderImportStatus) -> AIProviderImportPlan {
        plan_for("claude-code", action, status)
    }
    fn import(
        &self,
        options: &Options,
        user_id: &str,
        service: &str,
    ) -> Result<AIProviderImportResult, Box<dyn Error>> {
        import_claude_code(options, user_id, service)
    }
    fn cancel(&self, _options: &Options) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_claude_code()
    }
}

impl AIClientAdapter for ClaudeDesktopAdapter {
    fn id(&self) -> &'static str {
        "claude-desktop"
    }
    fn display_name(&self) -> &'static str {
        "Claude Desktop"
    }
    fn status(&self, _options: &Options) -> AIProviderImportStatus {
        claude_desktop_import_status()
    }
    fn plan(&self, action: &str, status: &AIProviderImportStatus) -> AIProviderImportPlan {
        plan_for("claude-desktop", action, status)
    }
    fn import(
        &self,
        options: &Options,
        user_id: &str,
        service: &str,
    ) -> Result<AIProviderImportResult, Box<dyn Error>> {
        import_claude_desktop(options, user_id, service)
    }
    fn cancel(&self, _options: &Options) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_claude_desktop()
    }
}

pub(crate) fn adapter_for(target: &str) -> Option<&'static dyn AIClientAdapter> {
    match target.trim() {
        "vscode" => Some(&VSCodeAdapter),
        "cc-switch" => Some(&CCSwitchAdapter),
        "codex" => Some(&CodexAdapter),
        "workbuddy" => Some(&WorkBuddyAdapter),
        "kimi-code" => Some(&KimiCodeAdapter),
        "qwen-code" => Some(&QwenCodeAdapter),
        "claude-code" => Some(&ClaudeCodeAdapter),
        "claude-desktop" => Some(&ClaudeDesktopAdapter),
        _ => None,
    }
}

pub(crate) fn known_adapters() -> Vec<&'static dyn AIClientAdapter> {
    vec![
        &VSCodeAdapter,
        &CCSwitchAdapter,
        &CodexAdapter,
        &WorkBuddyAdapter,
        &KimiCodeAdapter,
        &QwenCodeAdapter,
        &ClaudeCodeAdapter,
        &ClaudeDesktopAdapter,
    ]
}

pub(crate) fn known_adapter_ids() -> Vec<&'static str> {
    known_adapters()
        .into_iter()
        .map(AIClientAdapter::id)
        .collect()
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AIProviderImportBinding {
    service: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AIProviderImportBindings {
    #[serde(default)]
    clients: HashMap<String, AIProviderImportBinding>,
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
    #[serde(skip_serializing_if = "String::is_empty")]
    pub service: String,
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

#[derive(Debug, Serialize)]
pub(crate) struct AIProviderImportPlan {
    pub target: String,
    pub action: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub service: String,
    pub client_detected: bool,
    pub already_imported: bool,
    pub will_write: Vec<String>,
    pub will_backup: Vec<String>,
    pub detail: String,
}

pub(crate) fn plan(
    options: &Options,
    target: &str,
    action: &str,
) -> Result<AIProviderImportPlan, Box<dyn Error>> {
    plan_with_service(options, target, action, "")
}

pub(crate) fn plan_with_service(
    options: &Options,
    target: &str,
    action: &str,
    service: &str,
) -> Result<AIProviderImportPlan, Box<dyn Error>> {
    let adapter = adapter_for(target).ok_or_else(|| format!("不支持的 AI 客户端：{target}"))?;
    let status = status(options)
        .targets
        .into_iter()
        .find(|item| item.target == target.trim())
        .ok_or_else(|| format!("不支持的 AI 客户端：{target}"))?;
    let mut result = adapter.plan(action, &status);
    result.service = service.trim().to_string();
    if action == "import" && service.trim().starts_with("custom:") && result.already_imported {
        result.detail = format!(
            "{}；切换到自定义服务前请先移除客户端当前接入",
            result.detail
        );
    }
    Ok(result)
}

fn plan_for(target: &str, action: &str, status: &AIProviderImportStatus) -> AIProviderImportPlan {
    let (will_write, will_backup) = match action {
        "import" => plan_import(target, status),
        "remove" => plan_remove(target, status),
        _ => (Vec::new(), Vec::new()),
    };
    AIProviderImportPlan {
        target: target.to_string(),
        action: action.to_string(),
        service: String::new(),
        client_detected: status.client_detected,
        already_imported: status.state == "imported",
        will_write,
        will_backup,
        detail: status.detail.clone(),
    }
}

fn plan_import(target: &str, status: &AIProviderImportStatus) -> (Vec<String>, Vec<String>) {
    let mut will_write = Vec::new();
    let mut will_backup = Vec::new();
    if !status.config_path.is_empty() {
        will_backup.push(status.config_path.clone());
        will_write.push(status.config_path.clone());
    }
    match target {
        "codex" => {
            will_write
                .push("写入 Codex config.toml 的 [model_providers.himind] 与模型目录".to_string());
        }
        "cc-switch" => {
            will_write.push("向 CC Switch 数据库写入 HiMind 供应商配置".to_string());
        }
        "workbuddy" => {
            will_write.push("向 WorkBuddy models 配置写入 HiMind 模型".to_string());
        }
        "vscode" => {
            will_write.push("安装/更新 HiMind VS Code 扩展并打开授权页".to_string());
        }
        "kimi-code" => {
            will_write.push(
                "写入 Kimi Code config.toml 的 [providers.himind] 与 [models] 配置".to_string(),
            );
        }
        "qwen-code" => {
            will_write.push("写入 Qwen Code settings.json 的 modelProviders 与 env".to_string());
        }
        "claude-code" => {
            will_write.push(
                "写入 Claude Code settings.json env 的 ANTHROPIC_BASE_URL/AUTH_TOKEN/MODEL"
                    .to_string(),
            );
        }
        "claude-desktop" => {
            will_write.push(
                "写入 Claude Desktop claude_desktop_config.json env 的 ANTHROPIC_* 配置"
                    .to_string(),
            );
        }
        _ => {}
    }
    (will_write, will_backup)
}

fn plan_remove(target: &str, status: &AIProviderImportStatus) -> (Vec<String>, Vec<String>) {
    let mut will_write = Vec::new();
    let mut will_backup = Vec::new();
    if !status.config_path.is_empty() {
        will_backup.push(status.config_path.clone());
        will_write.push(status.config_path.clone());
    }
    match target {
        "codex" => {
            will_write.push(
                "移除 Codex config.toml 的 [model_providers.himind] 配置与模型目录".to_string(),
            );
        }
        "cc-switch" => {
            will_write.push("移除 CC Switch 数据库中的 HiMind 供应商配置".to_string());
        }
        "workbuddy" => {
            will_write.push("移除 WorkBuddy models 配置中的 HiMind 模型".to_string());
        }
        "vscode" => {
            will_write.push("移除 HiMind VS Code 扩展中的 HiMind 服务配置".to_string());
        }
        "kimi-code" => {
            will_write
                .push("移除 Kimi Code config.toml 中的 HiMind provider 与相关模型配置".to_string());
        }
        "qwen-code" => {
            will_write.push(
                "移除 Qwen Code settings.json 中的 HiMind modelProviders 条目与 env key"
                    .to_string(),
            );
        }
        "claude-code" => {
            will_write.push(
                "移除 Claude Code settings.json env 中的 HiMind ANTHROPIC_* 配置".to_string(),
            );
        }
        "claude-desktop" => {
            will_write.push(
                "移除 Claude Desktop claude_desktop_config.json env 中的 HiMind ANTHROPIC_* 配置"
                    .to_string(),
            );
        }
        _ => {}
    }
    (will_write, will_backup)
}

pub(crate) fn import(
    options: &Options,
    expected_user_id: &str,
    request: &AIProviderImportRequest,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let adapter = adapter_for(&request.target)
        .ok_or_else(|| format!("不支持的 AI 客户端：{}", request.target))?;
    // 一个客户端只能绑定一个来源。相同来源再次执行即为同步，切换来源必须先取消注册。
    let service_source = request.service_source();
    let current = status(options)
        .targets
        .into_iter()
        .find(|item| item.target == request.target.trim());
    if current
        .as_ref()
        .is_some_and(|item| item.state == "imported")
    {
        let bindings = load_import_bindings(options);
        match bindings.clients.get(request.target.trim()) {
            Some(binding) if binding.service == service_source => {}
            Some(_) => {
                return Err(format!(
                    "客户端 {} 已注册其他 AI 服务，请先取消注册后再切换",
                    request.target
                )
                .into())
            }
            None => {
                return Err(format!(
                    "客户端 {} 的注册来源未知，请先取消注册后再重新注册",
                    request.target
                )
                .into())
            }
        }
    }
    let result = adapter.import(options, expected_user_id, service_source)?;
    let mut bindings = load_import_bindings(options);
    bindings.clients.insert(
        request.target.trim().to_string(),
        AIProviderImportBinding {
            service: service_source.to_string(),
        },
    );
    save_import_bindings(options, &bindings)?;
    Ok(result)
}

fn import_bindings_path(options: &Options) -> PathBuf {
    options
        .state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(IMPORT_BINDINGS_FILE)
}

fn load_import_bindings(options: &Options) -> AIProviderImportBindings {
    fs::read(import_bindings_path(options))
        .ok()
        .and_then(|content| serde_json::from_slice(&content).ok())
        .unwrap_or_default()
}

fn save_import_bindings(
    options: &Options,
    bindings: &AIProviderImportBindings,
) -> Result<(), Box<dyn Error>> {
    let path = import_bindings_path(options);
    let _lock = crate::store::atomic_file::lock(&path)?;
    crate::store::atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(bindings)?)?;
    Ok(())
}

pub(crate) fn ensure_service_not_in_use(
    options: &Options,
    service_id: &str,
) -> Result<(), Box<dyn Error>> {
    let source = format!("custom:{}", service_id.trim());
    let bindings = load_import_bindings(options);
    let clients = bindings
        .clients
        .iter()
        .filter(|(_, binding)| binding.service == source)
        .map(|(target, _)| target.clone())
        .collect::<Vec<_>>();
    let unknown_imported = status(options)
        .targets
        .into_iter()
        .filter(|item| item.state == "imported")
        .filter(|item| !bindings.clients.contains_key(&item.target))
        .map(|item| item.target)
        .collect::<Vec<_>>();
    if !unknown_imported.is_empty() {
        return Err(format!(
            "检测到来源未知的客户端注册（{}），请先取消注册后再删除 AI 服务",
            unknown_imported.join("、")
        )
        .into());
    }
    if clients.is_empty() {
        return Ok(());
    }
    Err(format!(
        "请先取消客户端注册（{}），再删除此 AI 服务",
        clients.join("、")
    )
    .into())
}

pub(crate) fn status(options: &Options) -> AIProviderImportStatusOverview {
    let bindings = load_import_bindings(options);
    AIProviderImportStatusOverview {
        targets: known_adapters()
            .into_iter()
            .map(|adapter| {
                let mut status = adapter.status(options);
                if status.state == "imported" {
                    if let Some(binding) = bindings.clients.get(status.target.as_str()) {
                        status.service = binding.service.clone();
                    }
                }
                status
            })
            .collect(),
    }
}

/// 删除自定义服务前必须先撤销客户端接入，避免 API Key 继续留在外部客户端配置中。
/// 当前客户端配置格式不携带可靠的服务源 ID，因此采用保守阻断策略；
/// UI 会提供逐个移除入口，完成后再允许删除服务。
pub(crate) fn ensure_no_imported_clients(options: &Options) -> Result<(), Box<dyn Error>> {
    let imported = status(options)
        .targets
        .into_iter()
        .filter(|item| item.state == "imported")
        .map(|item| item.target)
        .collect::<Vec<_>>();
    if imported.is_empty() {
        return Ok(());
    }
    Err(format!(
        "请先移除已接入客户端（{}），再删除 AI 服务；这样可以避免客户端继续保留旧凭据",
        imported.join("、")
    )
    .into())
}

pub(crate) fn cancel(
    options: &Options,
    target: &str,
) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let adapter = adapter_for(target).ok_or_else(|| format!("不支持的 AI 客户端：{target}"))?;
    let result = adapter.cancel(options)?;
    let mut bindings = load_import_bindings(options);
    bindings.clients.remove(target.trim());
    save_import_bindings(options, &bindings)?;
    Ok(result)
}

/// 解析服务源对应的 AI 凭据。
///
/// `service` 为 `managed`（默认）时走 HiMind Dashboard 分发；
/// 为 `custom:<id>` 时从本机自定义服务读取（API Key 经 DPAPI 解密）。
fn resolve_credential(
    options: &Options,
    expected_user_id: &str,
    client_id: &str,
    service: &str,
) -> Result<AIClientCredential, Box<dyn Error>> {
    let service = service.trim();
    if service.is_empty() || service == "managed" {
        if !options.mode().dashboard_enabled() {
            return Err(
                "HiMind 分发服务需要 Connected 模式；独立模式请从本机自定义 AI 服务导入".into(),
            );
        }
        return fetch_client_credential(options, expected_user_id, client_id);
    }
    let custom_id = service
        .strip_prefix("custom:")
        .ok_or_else(|| format!("不支持的服务源：{service}，应为 managed 或 custom:<id>"))?;
    let (custom, api_key) = crate::store::ai_services::load_secret(custom_id)?;
    let access = crate::api::ai::AIUserCredential {
        active_entitlement_id: String::new(),
        active_personal_connection_id: String::new(),
        status: "active".to_string(),
        created_at: custom.created_at,
        updated_at: custom.updated_at,
        rotated_at: String::new(),
        base_url: custom.base_url,
        model: custom.model,
        models: custom.models,
        protocol: custom.protocol.as_str().to_string(),
    };
    Ok(AIClientCredential { access, api_key })
}

fn import_vscode(
    options: &Options,
    expected_user_id: &str,
    service: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let vscode_cli = ensure_vscode_extension()?;
    let credential = resolve_credential(options, expected_user_id, "vscode-import", service)?;
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
    service: &str,
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
    let credential = resolve_credential(options, expected_user_id, "cc-switch-import", service)?;
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
    service: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let config_path = codex_config_path();
    let models_path = codex_himind_models_path();
    let client_detected = config_path.join("config.toml").is_file()
        || config_path.join(CODEX_HIMIND_MODELS_FILE).is_file();
    let credential = resolve_credential(options, expected_user_id, "codex-import", service)?;
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
    provider["wire_api"] = value(openai_wire_api(credential));
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
        service: String::new(),
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
    service: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let credential = resolve_credential(options, expected_user_id, "workbuddy-import", service)?;
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
    // Status reads must stay side-effect free. The import path performs the
    // CLI version and extension checks when the user explicitly imports; a
    // periodic dashboard refresh only inspects known paths on disk.
    let cli = locate_vscode_cli_for_status();
    let client_detected = cli.is_some();
    let extension_roots = vscode_extension_roots_for_status(cli.as_deref());
    let extension_installed = find_vscode_extension_version(&extension_roots)
        .ok()
        .flatten()
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
        service: String::new(),
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
        service: String::new(),
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
        service: String::new(),
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

// ---- Kimi Code ----
// Kimi Code CLI 使用 ~/.kimi-code/config.toml（KIMI_CODE_HOME 可重定位）。Provider
// type 支持 openai / openai_responses，与 HiMind 网关的 OpenAI Chat/Responses 协议
// 对齐；模型以 [models."himind/<model>"] 别名表形式暴露，default_model 指向首选别名。
// 采用保留式合并：只接管 providers.himind、himind/* 模型别名与 default_model，
// hooks、permission、services 等用户配置原样保留。
fn kimi_code_config_path() -> PathBuf {
    if let Some(path) = env::var_os("KIMI_CODE_HOME") {
        return PathBuf::from(path).join("config.toml");
    }
    user_home().join(".kimi-code").join("config.toml")
}

fn kimi_code_himind_alias(model: &str) -> String {
    format!("{KIMI_CODE_HIMIND_PREFIX}{model}")
}

fn import_kimi_code(
    options: &Options,
    expected_user_id: &str,
    service: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let path = kimi_code_config_path();
    let client_detected = path.is_file()
        || user_home().join(".kimi-code").is_dir()
        || env::var_os("KIMI_CODE_HOME").is_some();
    let credential = resolve_credential(options, expected_user_id, "kimi-code-import", service)?;
    let models = available_models(&credential)?;
    let preferred = preferred_model(&credential)?;
    let original = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let updated = build_kimi_code_config(&original, &credential, &models, &preferred)?;
    let backup = backup_and_write(&path, updated.as_bytes())?;
    Ok(AIProviderImportResult {
        ok: true,
        target: "kimi-code".to_string(),
        status: "configured".to_string(),
        model_count: models.len(),
        model: preferred,
        config_path: path.to_string_lossy().to_string(),
        backup_path: backup
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
        client_detected,
    })
}

fn build_kimi_code_config(
    original: &str,
    credential: &AIClientCredential,
    models: &[String],
    preferred: &str,
) -> Result<String, Box<dyn Error>> {
    let endpoint = normalized_base_url(&credential.access.base_url)?;
    let mut document = original
        .parse::<DocumentMut>()
        .map_err(|error| format!("Kimi Code config.toml 格式无效：{error}"))?;
    let providers = document
        .as_table_mut()
        .entry("providers")
        .or_insert_with(|| {
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_mut()
        .ok_or("既有 config.toml 的 providers 不是表")?;
    let provider = providers
        .entry(KIMI_CODE_PROVIDER_ID)
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or("既有 config.toml 的 providers.himind 不是表")?;
    provider["type"] = value(openai_provider_type(credential));
    provider["api_key"] = value(credential.api_key.as_str());
    provider["base_url"] = value(endpoint.as_str());
    let models_table = document
        .as_table_mut()
        .entry("models")
        .or_insert_with(|| {
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_mut()
        .ok_or("既有 config.toml 的 models 不是表")?;
    for model in models {
        let alias = kimi_code_himind_alias(model);
        let entry = models_table
            .entry(&alias)
            .or_insert(Item::Table(Table::new()))
            .as_table_mut()
            .ok_or("既有 config.toml 的 models 条目不是表")?;
        entry["provider"] = value(KIMI_CODE_PROVIDER_ID);
        entry["model"] = value(model.as_str());
        entry["max_context_size"] = value(KIMI_CODE_DEFAULT_CONTEXT as i64);
        let mut capabilities = toml_edit::Array::new();
        capabilities.push("tool_use");
        entry["capabilities"] = toml_edit::Item::Value(toml_edit::Value::Array(capabilities));
    }
    document["default_model"] = value(kimi_code_himind_alias(preferred).as_str());
    Ok(document.to_string())
}

fn kimi_code_import_status() -> AIProviderImportStatus {
    let path = kimi_code_config_path();
    let client_detected = path.is_file()
        || user_home().join(".kimi-code").is_dir()
        || env::var_os("KIMI_CODE_HOME").is_some();
    let models = read_kimi_code_himind_models(&path).unwrap_or_default();
    let imported = !models.is_empty() || kimi_code_himind_provider_present(&path);
    AIProviderImportStatus {
        target: "kimi-code".to_string(),
        state: if imported { "imported" } else { "not_imported" }.to_string(),
        client_detected,
        detail: if imported && models.is_empty() {
            "检测到 Kimi Code 已配置 HiMind 供应商，但缺少模型别名，请重新导入".to_string()
        } else if imported {
            format!(
                "已写入 {} 个 HiMind 模型；重启 Kimi Code 后可在模型选择器中使用",
                models.len()
            )
        } else if client_detected {
            "已检测到 Kimi Code，尚未导入 HiMind AI".to_string()
        } else {
            "未检测到 Kimi Code 配置目录，请先运行一次 kimi".to_string()
        },
        config_path: path.to_string_lossy().to_string(),
        models,
        synced_at: String::new(),
        service: String::new(),
    }
}

fn kimi_code_himind_provider_present(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .is_some_and(|document| {
            document
                .get("providers")
                .and_then(|item| item.as_table())
                .is_some_and(|table| table.contains_key(KIMI_CODE_PROVIDER_ID))
        })
}

fn read_kimi_code_himind_models(path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let content = fs::read_to_string(path)?;
    let document = content.parse::<DocumentMut>()?;
    let Some(models) = document.get("models").and_then(|item| item.as_table()) else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    for (alias, item) in models.iter() {
        let Some(model) = alias.strip_prefix(KIMI_CODE_HIMIND_PREFIX) else {
            continue;
        };
        let provider = item
            .get("provider")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if provider == KIMI_CODE_PROVIDER_ID && !model.trim().is_empty() {
            result.push(model.to_string());
        }
    }
    Ok(result)
}

fn cancel_kimi_code() -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let path = kimi_code_config_path();
    let client_detected = path.is_file();
    let original = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let (updated, removed) = strip_kimi_code_himind(&original)?;
    let backup = if removed {
        backup_and_write(&path, updated.as_bytes())?
    } else {
        None
    };
    Ok(AIProviderImportCancelResult {
        ok: true,
        target: "kimi-code".to_string(),
        status: if removed { "cancelled" } else { "not_imported" }.to_string(),
        changed: removed,
        client_detected,
        detail: if removed {
            "已从 Kimi Code 移除 HiMind 供应商与模型别名".to_string()
        } else {
            "Kimi Code 当前没有 HiMind 导入记录".to_string()
        },
        backup_path: backup
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
    })
}

// 只移除 HiMind 明确写入的字段（providers.himind、himind/* 别名、default_model），
// 用户其他配置原样保留；无法判定归属的字段不动。
fn strip_kimi_code_himind(original: &str) -> Result<(String, bool), Box<dyn Error>> {
    let mut document = original.parse::<DocumentMut>()?;
    let mut changed = false;
    if let Some(providers) = document
        .get_mut("providers")
        .and_then(|item| item.as_table_mut())
    {
        if providers.remove(KIMI_CODE_PROVIDER_ID).is_some() {
            changed = true;
        }
        if providers.is_empty() {
            document.remove("providers");
        }
    }
    if let Some(models) = document
        .get_mut("models")
        .and_then(|item| item.as_table_mut())
    {
        let himind_aliases: Vec<String> = models
            .iter()
            .filter_map(|(alias, _)| {
                alias
                    .strip_prefix(KIMI_CODE_HIMIND_PREFIX)
                    .map(|_| alias.to_string())
            })
            .collect();
        for alias in himind_aliases {
            models.remove(&alias);
            changed = true;
        }
        if models.is_empty() {
            document.remove("models");
        }
    }
    if document
        .get("default_model")
        .and_then(|item| item.as_str())
        .is_some_and(|value| value.starts_with(KIMI_CODE_HIMIND_PREFIX))
    {
        document.remove("default_model");
        changed = true;
    }
    Ok((document.to_string(), changed))
}

// ---- Qwen Code ----
// Qwen Code 使用 ~/.qwen/settings.json；凭据经顶层 env 存放（envKey 引用），
// modelProviders 声明模型目录，自定义 provider id 经 providerProtocol 映射到
// openai 协议，与 HiMind 网关 OpenAI Chat/Responses 协议对齐。采用保留式合并：
// 只接管 env.HIMIND_API_KEY、modelProviders.himind、providerProtocol.himind 与
// model.name；mcpServers、ui 等用户配置原样保留。
fn qwen_code_settings_path() -> PathBuf {
    if let Some(path) = env::var_os("QWEN_CODE_HOME") {
        return PathBuf::from(path).join("settings.json");
    }
    user_home().join(".qwen").join("settings.json")
}

fn import_qwen_code(
    options: &Options,
    expected_user_id: &str,
    service: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let path = qwen_code_settings_path();
    let client_detected = path.is_file()
        || user_home().join(".qwen").is_dir()
        || env::var_os("QWEN_CODE_HOME").is_some();
    let credential = resolve_credential(options, expected_user_id, "qwen-code-import", service)?;
    let models = available_models(&credential)?;
    let preferred = preferred_model(&credential)?;
    let original = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let updated = build_qwen_code_settings(&original, &credential, &models, &preferred)?;
    let backup = backup_and_write(&path, updated.as_bytes())?;
    Ok(AIProviderImportResult {
        ok: true,
        target: "qwen-code".to_string(),
        status: "configured".to_string(),
        model_count: models.len(),
        model: preferred,
        config_path: path.to_string_lossy().to_string(),
        backup_path: backup
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
        client_detected,
    })
}

fn build_qwen_code_settings(
    original: &str,
    credential: &AIClientCredential,
    models: &[String],
    preferred: &str,
) -> Result<String, Box<dyn Error>> {
    let endpoint = normalized_base_url(&credential.access.base_url)?;
    let mut root = if original.trim().is_empty() {
        serde_json::Map::new()
    } else {
        serde_json::from_str::<Value>(original)
            .map_err(|error| format!("Qwen Code settings.json 格式无效：{error}"))?
            .as_object()
            .cloned()
            .ok_or("Qwen Code settings.json 顶层必须是 JSON 对象")?
    };
    let env = root
        .entry("env")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("Qwen Code settings.json 的 env 必须是对象")?
        .clone();
    let mut env = env;
    env.insert(QWEN_CODE_ENV_KEY.to_string(), json!(credential.api_key));
    root.insert("env".to_string(), Value::Object(env));
    let provider_models = models
        .iter()
        .map(|model| {
            json!({
                "id": model,
                "name": model,
                "envKey": QWEN_CODE_ENV_KEY,
                "baseUrl": endpoint,
            })
        })
        .collect::<Vec<_>>();
    let mut providers = root
        .entry("modelProviders")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("Qwen Code settings.json 的 modelProviders 必须是对象")?
        .clone();
    providers.insert(QWEN_CODE_PROVIDER_ID.to_string(), json!(provider_models));
    root.insert("modelProviders".to_string(), Value::Object(providers));
    let mut protocols = root
        .entry("providerProtocol")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("Qwen Code settings.json 的 providerProtocol 必须是对象")?
        .clone();
    // Qwen Code currently exposes the OpenAI Chat provider name only. Its
    // `providerProtocol` value is a client capability, not the wire protocol
    // selector used by other adapters.
    protocols.insert(QWEN_CODE_PROVIDER_ID.to_string(), json!("openai"));
    root.insert("providerProtocol".to_string(), Value::Object(protocols));
    root.insert("model".to_string(), json!({ "name": preferred }));
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(root))?
    ))
}

fn qwen_code_import_status() -> AIProviderImportStatus {
    let path = qwen_code_settings_path();
    let client_detected = path.is_file()
        || user_home().join(".qwen").is_dir()
        || env::var_os("QWEN_CODE_HOME").is_some();
    let models = read_qwen_code_himind_models(&path).unwrap_or_default();
    let imported = !models.is_empty() || qwen_code_himind_provider_present(&path);
    AIProviderImportStatus {
        target: "qwen-code".to_string(),
        state: if imported { "imported" } else { "not_imported" }.to_string(),
        client_detected,
        detail: if imported && models.is_empty() {
            "检测到 Qwen Code 已配置 HiMind 供应商，但缺少模型条目，请重新导入".to_string()
        } else if imported {
            format!(
                "已写入 {} 个 HiMind 模型；重启 Qwen Code 后可在 /model 选择",
                models.len()
            )
        } else if client_detected {
            "已检测到 Qwen Code，尚未导入 HiMind AI".to_string()
        } else {
            "未检测到 Qwen Code 配置目录，请先运行一次 qwen".to_string()
        },
        config_path: path.to_string_lossy().to_string(),
        models,
        synced_at: String::new(),
        service: String::new(),
    }
}

fn qwen_code_himind_provider_present(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .is_some_and(|root| {
            root.get("modelProviders")
                .and_then(|value| value.get(QWEN_CODE_PROVIDER_ID))
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty())
        })
}

fn read_qwen_code_himind_models(path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let content = fs::read_to_string(path)?;
    let root: Value = serde_json::from_str(&content)?;
    Ok(root
        .get("modelProviders")
        .and_then(|value| value.get(QWEN_CODE_PROVIDER_ID))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect())
}

fn cancel_qwen_code() -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let path = qwen_code_settings_path();
    let client_detected = path.is_file();
    let original = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let (updated, removed) = strip_qwen_code_himind(&original)?;
    let backup = if removed {
        backup_and_write(&path, updated.as_bytes())?
    } else {
        None
    };
    Ok(AIProviderImportCancelResult {
        ok: true,
        target: "qwen-code".to_string(),
        status: if removed { "cancelled" } else { "not_imported" }.to_string(),
        changed: removed,
        client_detected,
        detail: if removed {
            "已从 Qwen Code 移除 HiMind 模型供应商与凭据".to_string()
        } else {
            "Qwen Code 当前没有 HiMind 导入记录".to_string()
        },
        backup_path: backup
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
    })
}

// 只移除 HiMind 明确写入的字段（env.HIMIND_API_KEY、modelProviders.himind、
// providerProtocol.himind、model.name 若指向 HiMind 模型），用户其他配置原样保留。
fn strip_qwen_code_himind(original: &str) -> Result<(String, bool), Box<dyn Error>> {
    if original.trim().is_empty() {
        return Ok((String::new(), false));
    }
    let mut root = serde_json::from_str::<Value>(original)
        .map_err(|_| "Qwen Code settings.json 格式无效，已停止取消导入且未覆盖原文件")?;
    let object = root
        .as_object_mut()
        .ok_or("Qwen Code settings.json 顶层必须是 JSON 对象")?;
    let mut changed = false;
    if let Some(env) = object.get_mut("env").and_then(Value::as_object_mut) {
        if env.remove(QWEN_CODE_ENV_KEY).is_some() {
            changed = true;
        }
    }
    if let Some(providers) = object
        .get_mut("modelProviders")
        .and_then(Value::as_object_mut)
    {
        if providers.remove(QWEN_CODE_PROVIDER_ID).is_some() {
            changed = true;
        }
    }
    if let Some(protocols) = object
        .get_mut("providerProtocol")
        .and_then(Value::as_object_mut)
    {
        if protocols.remove(QWEN_CODE_PROVIDER_ID).is_some() {
            changed = true;
        }
    }
    let model_points_at_himind = object
        .get("model")
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .is_some_and(|name| {
            object
                .get("modelProviders")
                .and_then(|value| value.get(QWEN_CODE_PROVIDER_ID))
                .and_then(Value::as_array)
                .is_none()
                && name.starts_with(QWEN_CODE_PROVIDER_ID)
        });
    if model_points_at_himind {
        object.remove("model");
        changed = true;
    }
    Ok((
        format!("{}\n", serde_json::to_string_pretty(&root)?),
        changed,
    ))
}

// ---- Claude Code / Claude Desktop ----
// 两者共用 Anthropic 协议 env 注入：settings.json 的 env 块写入
// ANTHROPIC_BASE_URL（网关 base，SDK 自动追加 /v1/messages）、
// ANTHROPIC_AUTH_TOKEN（网关 Bearer 认证）、ANTHROPIC_MODEL 与
// ANTHROPIC_CUSTOM_MODEL_OPTION。Anthropic SDK 会在 base_url 后追加
// /v1/messages，因此这里把网关 URL 末尾的 /v1 剥掉再写入。
// 采用保留式合并，取消时只剥离 HiMind 写入的 ANTHROPIC_* 键。
fn anthropic_base_url(value: &str) -> Result<String, Box<dyn Error>> {
    let base = normalized_base_url(value)?;
    let stripped = base.strip_suffix("/v1").map(str::to_string).unwrap_or(base);
    Ok(stripped)
}

fn claude_code_settings_path() -> PathBuf {
    if let Some(dir) = env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(dir).join("settings.json");
    }
    user_home().join(".claude").join("settings.json")
}

fn claude_desktop_config_path() -> PathBuf {
    let app_data = env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join("AppData").join("Roaming"));
    app_data.join("Claude").join("claude_desktop_config.json")
}

fn claude_env_keys() -> [&'static str; 4] {
    [
        CLAUDE_BASE_URL_ENV,
        CLAUDE_AUTH_TOKEN_ENV,
        CLAUDE_MODEL_ENV,
        CLAUDE_CUSTOM_MODEL_OPTION,
    ]
}

fn import_claude_code(
    options: &Options,
    expected_user_id: &str,
    service: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let path = claude_code_settings_path();
    let client_detected = path.is_file() || user_home().join(".claude").is_dir();
    let credential = resolve_credential(options, expected_user_id, "claude-code-import", service)?;
    let models = available_models(&credential)?;
    let preferred = preferred_model(&credential)?;
    let original = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let updated =
        build_claude_settings(&original, &credential, &models, &preferred, "Claude Code")?;
    let backup = backup_and_write(&path, updated.as_bytes())?;
    Ok(AIProviderImportResult {
        ok: true,
        target: "claude-code".to_string(),
        status: "configured".to_string(),
        model_count: models.len(),
        model: preferred,
        config_path: path.to_string_lossy().to_string(),
        backup_path: backup
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
        client_detected,
    })
}

fn import_claude_desktop(
    options: &Options,
    expected_user_id: &str,
    service: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let path = claude_desktop_config_path();
    let client_detected = path.is_file()
        || env::var_os("APPDATA")
            .map(|dir| PathBuf::from(dir).join("Claude").is_dir())
            .unwrap_or(false);
    let credential =
        resolve_credential(options, expected_user_id, "claude-desktop-import", service)?;
    let models = available_models(&credential)?;
    let preferred = preferred_model(&credential)?;
    let original = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let updated = build_claude_settings(
        &original,
        &credential,
        &models,
        &preferred,
        "Claude Desktop",
    )?;
    let backup = backup_and_write(&path, updated.as_bytes())?;
    Ok(AIProviderImportResult {
        ok: true,
        target: "claude-desktop".to_string(),
        status: "configured".to_string(),
        model_count: models.len(),
        model: preferred,
        config_path: path.to_string_lossy().to_string(),
        backup_path: backup
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
        client_detected,
    })
}

fn build_claude_settings(
    original: &str,
    credential: &AIClientCredential,
    _models: &[String],
    preferred: &str,
    client_name: &str,
) -> Result<String, Box<dyn Error>> {
    let base = anthropic_base_url(&credential.access.base_url)?;
    let mut root = if original.trim().is_empty() {
        serde_json::Map::new()
    } else {
        serde_json::from_str::<Value>(original)
            .map_err(|error| format!("{client_name} settings.json 格式无效：{error}"))?
            .as_object()
            .cloned()
            .ok_or(format!("{client_name} settings.json 顶层必须是 JSON 对象"))?
    };
    let mut env = root
        .entry("env")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(format!("{client_name} settings.json 的 env 必须是对象"))?
        .clone();
    env.insert(CLAUDE_BASE_URL_ENV.to_string(), json!(base));
    env.insert(CLAUDE_AUTH_TOKEN_ENV.to_string(), json!(credential.api_key));
    env.insert(CLAUDE_MODEL_ENV.to_string(), json!(preferred));
    env.insert(CLAUDE_CUSTOM_MODEL_OPTION.to_string(), json!(preferred));
    root.insert("env".to_string(), Value::Object(env));
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(root))?
    ))
}

fn claude_import_status(path: &Path, target: &str, client_name: &str) -> AIProviderImportStatus {
    let client_detected = path.is_file();
    let models = read_claude_himind_models(path, target).unwrap_or_default();
    let imported = claude_himind_env_present(path);
    AIProviderImportStatus {
        target: target.to_string(),
        state: if imported { "imported" } else { "not_imported" }.to_string(),
        client_detected,
        detail: if imported && models.is_empty() {
            format!("检测到 {client_name} 已配置 HiMind 网关，但缺少模型声明，请重新导入")
        } else if imported {
            format!(
                "已写入 HiMind 网关端点与模型；重启 {client_name} 后生效（模型：{}）",
                models.join(", ")
            )
        } else if client_detected {
            format!("已检测到 {client_name}，尚未导入 HiMind AI")
        } else {
            format!("未检测到 {client_name} 配置，请先安装并启动一次")
        },
        config_path: path.to_string_lossy().to_string(),
        models,
        synced_at: String::new(),
        service: String::new(),
    }
}

fn claude_code_import_status() -> AIProviderImportStatus {
    claude_import_status(&claude_code_settings_path(), "claude-code", "Claude Code")
}

fn claude_desktop_import_status() -> AIProviderImportStatus {
    claude_import_status(
        &claude_desktop_config_path(),
        "claude-desktop",
        "Claude Desktop",
    )
}

fn claude_himind_env_present(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .is_some_and(|root| {
            root.get("env")
                .and_then(|value| value.get(CLAUDE_BASE_URL_ENV))
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
                && root
                    .get("env")
                    .and_then(|value| value.get(CLAUDE_AUTH_TOKEN_ENV))
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.trim().is_empty())
        })
}

fn read_claude_himind_models(path: &Path, _target: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let content = fs::read_to_string(path)?;
    let root: Value = serde_json::from_str(&content)?;
    let mut models = Vec::new();
    if let Some(model) = root
        .get("env")
        .and_then(|value| value.get(CLAUDE_MODEL_ENV))
        .and_then(Value::as_str)
    {
        if !model.trim().is_empty() {
            models.push(model.trim().to_string());
        }
    }
    Ok(models)
}

fn cancel_claude_code() -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    cancel_claude_settings(&claude_code_settings_path(), "claude-code", "Claude Code")
}

fn cancel_claude_desktop() -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    cancel_claude_settings(
        &claude_desktop_config_path(),
        "claude-desktop",
        "Claude Desktop",
    )
}

fn cancel_claude_settings(
    path: &Path,
    target: &str,
    client_name: &str,
) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let client_detected = path.is_file();
    let original = if path.is_file() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    let (updated, removed) = strip_claude_himind(&original, client_name)?;
    let backup = if removed {
        backup_and_write(path, updated.as_bytes())?
    } else {
        None
    };
    Ok(AIProviderImportCancelResult {
        ok: true,
        target: target.to_string(),
        status: if removed { "cancelled" } else { "not_imported" }.to_string(),
        changed: removed,
        client_detected,
        detail: if removed {
            format!("已从 {client_name} 移除 HiMind 网关端点与凭据")
        } else {
            format!("{client_name} 当前没有 HiMind 导入记录")
        },
        backup_path: backup
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
    })
}

// 只移除 HiMind 写入的 ANTHROPIC_* 键，用户其他配置原样保留。
fn strip_claude_himind(
    original: &str,
    client_name: &str,
) -> Result<(String, bool), Box<dyn Error>> {
    if original.trim().is_empty() {
        return Ok((String::new(), false));
    }
    let mut root = serde_json::from_str::<Value>(original).map_err(|_| {
        format!("{client_name} settings.json 格式无效，已停止取消导入且未覆盖原文件")
    })?;
    let object = root
        .as_object_mut()
        .ok_or(format!("{client_name} settings.json 顶层必须是 JSON 对象"))?;
    let mut changed = false;
    if let Some(env) = object.get_mut("env").and_then(Value::as_object_mut) {
        for key in claude_env_keys() {
            if env.remove(key).is_some() {
                changed = true;
            }
        }
        if env.is_empty() {
            object.remove("env");
        }
    }
    Ok((
        format!("{}\n", serde_json::to_string_pretty(&root)?),
        changed,
    ))
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
    provider["wire_api"] = value(openai_wire_api(credential));
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
    // WorkBuddy's models.json contract is fixed to Chat Completions; retain
    // that client-specific capability even when the service supports Responses.
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

pub(crate) fn unix_now_seconds() -> u64 {
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

fn openai_protocol_is_chat(credential: &AIClientCredential) -> bool {
    credential.access.protocol.trim() == "openai-chat"
}

fn openai_wire_api(credential: &AIClientCredential) -> &'static str {
    if openai_protocol_is_chat(credential) {
        "chat"
    } else {
        "responses"
    }
}

fn openai_provider_type(credential: &AIClientCredential) -> &'static str {
    if openai_protocol_is_chat(credential) {
        "openai"
    } else {
        "openai_responses"
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
    ensure_supported_vscode_version(&cli)?;
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
    // The stable @himind participant remains usable when a system-wide VS
    // Code install prevents a normal user from editing product.json. The
    // proposed model picker is an enhancement, not a reason to fail the
    // complete installation and enrollment flow.
    if let Err(error) = ensure_vscode_chat_provider_allowlist(&cli) {
        eprintln!("VS Code chatProvider allowlist skipped: {error}");
    }
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

fn locate_vscode_cli_for_status() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(value) = env::var_os("HIMIND_VSCODE_CLI") {
        candidates.push(PathBuf::from(value));
    }
    candidates.extend(vscode_registry_candidates());
    candidates.extend(vscode_path_candidates());
    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path) {
            candidates.push(directory.join("code.cmd"));
            candidates.push(directory.join("code-insiders.cmd"));
            candidates.push(directory.join("code"));
            candidates.push(directory.join("code-insiders"));
        }
    }
    let mut seen = HashSet::new();
    candidates.into_iter().find(|candidate| {
        let key = candidate.to_string_lossy().to_ascii_lowercase();
        seen.insert(key) && candidate.is_file()
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

fn ensure_supported_vscode_version(cli: &Path) -> Result<(), Box<dyn Error>> {
    let output = run_vscode_command(vscode_command(cli).arg("--version"), Duration::from_secs(5))?;
    if !output.status.success() {
        return Err(format!(
            "无法读取 VS Code 版本：{}",
            command_error_detail(&output.stdout, &output.stderr)
        )
        .into());
    }
    let version_text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let version = parse_vscode_cli_version(&version_text)?;
    let minimum = Version::parse(MIN_SUPPORTED_VSCODE_VERSION)?;
    if version < minimum {
        return Err(format!(
            "当前 VS Code 版本为 {version}，HiMind AI 扩展要求 VS Code >= {minimum}"
        )
        .into());
    }
    Ok(())
}

fn parse_vscode_cli_version(output: &str) -> Result<Version, Box<dyn Error>> {
    output
        .lines()
        .map(str::trim)
        .find_map(|line| Version::parse(line).ok())
        .ok_or_else(|| "无法解析 VS Code 版本，请升级到支持的稳定版本".into())
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
    // Depending on the VS Code build and locale, extension listing output can
    // be written to stdout or stderr. Parse both streams before falling back
    // to the on-disk extension directory (portable VS Code does not always
    // refresh the CLI index immediately after installation).
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if let Some(version) = parse_vscode_extension_version(&combined)? {
        return Ok(Some(version));
    }
    find_vscode_extension_version(&vscode_extension_roots(cli))
}

fn vscode_extension_roots(cli: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let insiders = cli
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.to_ascii_lowercase().contains("insiders"));
    let profile_dir = if insiders {
        ".vscode-insiders"
    } else {
        ".vscode"
    };
    roots.push(user_home().join(profile_dir).join("extensions"));

    // Portable VS Code keeps extensions below the product root rather than
    // under the user's profile. The CLI path is <root>/bin/code(.cmd).
    if let Some(root) = cli.parent().and_then(Path::parent) {
        roots.push(root.join("data/extensions"));
    }
    roots
}

fn vscode_extension_roots_for_status(cli: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![
        user_home().join(".vscode").join("extensions"),
        user_home().join(".vscode-insiders").join("extensions"),
    ];
    if let Some(cli) = cli {
        roots.extend(vscode_extension_roots(cli));
    }
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter(|root| seen.insert(root.to_string_lossy().to_ascii_lowercase()))
        .collect()
}

fn find_vscode_extension_version(roots: &[PathBuf]) -> Result<Option<String>, Box<dyn Error>> {
    let mut best: Option<String> = None;
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("package.json");
            let Ok(content) = fs::read_to_string(manifest_path) else {
                continue;
            };
            let Ok(manifest) = serde_json::from_str::<VSCodeExtensionManifest>(&content) else {
                continue;
            };
            if !format!("{}.{}", manifest.publisher, manifest.name)
                .eq_ignore_ascii_case(VSCODE_EXTENSION_ID)
            {
                continue;
            }
            if Version::parse(manifest.version.trim()).is_err() {
                continue;
            }
            let replace = best.as_deref().is_none_or(|current| {
                compare_extension_versions(&manifest.version, current)
                    .map(|ordering| ordering == Ordering::Greater)
                    .unwrap_or(false)
            });
            if replace {
                best = Some(manifest.version.trim().to_string());
            }
        }
    }
    Ok(best)
}

fn wait_for_vscode_extension_version(cli: &Path) -> Result<Option<String>, Box<dyn Error>> {
    // VS Code returns from --install-extension before its extension index is
    // immediately visible to a second CLI invocation on slower machines.
    // Keep polling long enough for portable and first-run installations.
    for attempt in 0..20 {
        if let Some(version) = installed_vscode_extension_version(cli)? {
            return Ok(Some(version));
        }
        if attempt < 19 {
            std::thread::sleep(Duration::from_millis(500));
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
        // The monorepo keeps the extension under official-extensions; the
        // standalone Agent repository keeps the legacy integrations path.
        candidates.push(root.join("official-extensions/vscode-himind-ai/dist/himind-ai.vsix"));
        candidates.push(root.join("integrations/vscode-himind-ai/dist/himind-ai.vsix"));
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
        adapter_for, anthropic_base_url, build_cc_switch_provider_settings, build_claude_settings,
        build_codex_config_toml, build_codex_models_json, build_kimi_code_config,
        build_qwen_code_settings, build_vscode_enrollment_url, bundled_vscode_vsix_candidates,
        chat_completions_url, compare_extension_versions, consume_vscode_enrollment,
        create_vscode_enrollment, ensure_vscode_chat_provider_allowlist,
        find_vscode_extension_version, known_adapters, legacy_workbuddy_model_id,
        managed_workbuddy_model_ids, merge_workbuddy_models, migrate_workbuddy_sessions,
        parse_vscode_cli_version, parse_vscode_extension_version, parse_vscode_import_status,
        push_vscode_registry_value, read_cc_switch_managed_models, read_cc_switch_managed_settings,
        read_codex_model_catalog, remove_workbuddy_models, strip_claude_himind, strip_codex_himind,
        strip_kimi_code_himind, strip_qwen_code_himind, vscode_extension_install_required,
        workbuddy_model_id, workbuddy_models_path_in, write_cc_switch_provider, AIClientCredential,
        AIProviderImportRequest, CLAUDE_BASE_URL_ENV, CLAUDE_CUSTOM_MODEL_OPTION,
        VSCODE_CHAT_PROVIDER_PROPOSAL, VSCODE_EXTENSION_ID,
    };
    use crate::api::ai::AIUserCredential;
    use semver::Version;
    use serde_json::{json, Value};
    use std::cmp::Ordering;
    use std::path::{Path, PathBuf};

    #[test]
    fn adapters_register_unique_ids_with_display_names() {
        let adapters = known_adapters();
        let mut ids = std::collections::HashSet::new();
        for adapter in &adapters {
            let id = adapter.id();
            assert!(!id.trim().is_empty());
            assert!(ids.insert(id), "duplicate adapter id: {id}");
            assert!(!adapter.display_name().trim().is_empty());
        }
        assert_eq!(adapters.len(), 8);
        for target in [
            "vscode",
            "cc-switch",
            "codex",
            "workbuddy",
            "kimi-code",
            "qwen-code",
            "claude-code",
            "claude-desktop",
        ] {
            assert!(adapter_for(target).is_some(), "missing adapter: {target}");
        }
        assert!(adapter_for("gemini-cli").is_none());
    }

    #[test]
    fn plan_returns_read_only_preview_without_writing() {
        for adapter in known_adapters() {
            let status = adapter.status(&crate::Options::from_env());
            for action in ["import", "remove"] {
                let plan = adapter.plan(action, &status);
                assert_eq!(plan.target, adapter.id());
                assert_eq!(plan.action, action);
                assert!(
                    !plan.will_write.is_empty() || !plan.will_backup.is_empty(),
                    "plan for {} ({action}) must describe at least one change",
                    adapter.id()
                );
            }
        }
    }

    fn credential(models: &[&str]) -> AIClientCredential {
        AIClientCredential {
            access: AIUserCredential {
                active_entitlement_id: "ent-1".to_string(),
                active_personal_connection_id: String::new(),
                status: "active".to_string(),
                created_at: String::new(),
                updated_at: String::new(),
                rotated_at: String::new(),
                base_url: "https://ai.example.com/v1/".to_string(),
                model: models.first().copied().unwrap_or("default").to_string(),
                models: models.iter().map(|value| value.to_string()).collect(),
                protocol: "openai-responses".to_string(),
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
    fn chat_protocol_is_forwarded_to_openai_compatible_adapters() {
        let mut chat = credential(&["model-a"]);
        chat.access.protocol = "openai-chat".to_string();
        let codex = build_codex_config_toml(
            "",
            &chat,
            Path::new(r"C:\Users\Admin\.codex\himind-models.json"),
            "model-a",
        )
        .unwrap();
        assert!(codex.contains("wire_api = \"chat\""));
        let kimi = build_kimi_code_config("", &chat, &["model-a".to_string()], "model-a").unwrap();
        assert!(kimi.contains("type = \"openai\""));
        let cc_switch =
            build_cc_switch_provider_settings(&chat, &["model-a".to_string()], "model-a", None)
                .unwrap();
        let cc_switch_config: Value = serde_json::from_str(&cc_switch).unwrap();
        assert!(cc_switch_config["config"]
            .as_str()
            .is_some_and(|value| value.contains("wire_api = \"chat\"")));
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
    fn parses_vscode_cli_version_from_multiline_output() {
        assert_eq!(
            parse_vscode_cli_version("1.120.2\ncommit-hash\nx64").unwrap(),
            Version::parse("1.120.2").unwrap()
        );
        assert!(parse_vscode_cli_version("commit-hash\nx64").is_err());
    }

    #[test]
    fn finds_vscode_extension_from_portable_extension_directory() {
        let root = std::env::temp_dir().join(format!(
            "himind-vscode-extensions-{}-{}",
            std::process::id(),
            super::unix_now_millis()
        ));
        let extension = root.join("himind.himind-ai-0.1.15");
        std::fs::create_dir_all(&extension).unwrap();
        std::fs::write(
            extension.join("package.json"),
            br#"{"name":"himind-ai","publisher":"himind","version":"0.1.15"}"#,
        )
        .unwrap();
        assert_eq!(
            find_vscode_extension_version(&[root.clone()]).unwrap(),
            Some("0.1.15".to_string())
        );
        std::fs::remove_dir_all(root).ok();
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
                r"F:\workspace\himind\integrations\vscode-himind-ai\dist\himind-ai.vsix"
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

    #[test]
    fn resolves_service_source_from_import_request() {
        let managed = AIProviderImportRequest {
            target: "codex".to_string(),
            service: String::new(),
        };
        assert_eq!(managed.service_source(), "managed");

        let explicit_managed = AIProviderImportRequest {
            target: "codex".to_string(),
            service: "managed".to_string(),
        };
        assert_eq!(explicit_managed.service_source(), "managed");

        let custom = AIProviderImportRequest {
            target: "codex".to_string(),
            service: "custom:my-gateway".to_string(),
        };
        assert_eq!(custom.service_source(), "custom:my-gateway");
    }

    #[test]
    fn builds_kimi_code_config_with_himind_provider_and_models() {
        let credential = credential(&["kimi-k3", "kimi-for-coding"]);
        let models = ["kimi-k3".to_string(), "kimi-for-coding".to_string()];
        let updated = build_kimi_code_config("", &credential, &models, "kimi-k3").unwrap();
        assert!(updated.contains("[providers.himind]"));
        assert!(updated.contains("type = \"openai_responses\""));
        assert!(updated.contains("api_key = \"test-secret-key\""));
        assert!(updated.contains("base_url = \"https://ai.example.com/v1\""));
        assert!(updated.contains("[models.\"himind/kimi-k3\"]"));
        assert!(updated.contains("[models.\"himind/kimi-for-coding\"]"));
        assert!(updated.contains("default_model = \"himind/kimi-k3\""));
    }

    #[test]
    fn kimi_code_config_merge_preserves_existing_tables() {
        let original = "default_model = \"existing/model\"\n[hooks]\nenabled = true\n";
        let models = ["kimi-k3".to_string()];
        let updated =
            build_kimi_code_config(original, &credential(&["kimi-k3"]), &models, "kimi-k3")
                .unwrap();
        assert!(updated.contains("[hooks]"));
        assert!(updated.contains("enabled = true"));
        assert!(updated.contains("[providers.himind]"));
        assert!(updated.contains("[models.\"himind/kimi-k3\"]"));
        assert!(updated.contains("default_model = \"himind/kimi-k3\""));
    }

    #[test]
    fn strip_kimi_code_only_removes_himind_fields() {
        let original = "default_model = \"himind/kimi-k3\"\n[providers.himind]\ntype = \"openai_responses\"\n[providers.other]\ntype = \"openai\"\n[models.\"himind/kimi-k3\"]\nprovider = \"himind\"\n[models.\"local/m1\"]\nprovider = \"other\"\n";
        let (updated, changed) = strip_kimi_code_himind(original).unwrap();
        assert!(changed);
        assert!(!updated.contains("himind"));
        assert!(updated.contains("[providers.other]"));
        assert!(updated.contains("[models.\"local/m1\"]"));
        assert!(!updated.contains("default_model"));
    }

    #[test]
    fn builds_qwen_code_settings_with_provider_catalog_and_env() {
        let credential = credential(&["qwen3-coder-plus"]);
        let models = ["qwen3-coder-plus".to_string()];
        let updated =
            build_qwen_code_settings("", &credential, &models, "qwen3-coder-plus").unwrap();
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(root["env"]["HIMIND_API_KEY"], "test-secret-key");
        assert_eq!(root["providerProtocol"]["himind"], "openai");
        assert_eq!(
            root["modelProviders"]["himind"][0]["id"],
            "qwen3-coder-plus"
        );
        assert_eq!(
            root["modelProviders"]["himind"][0]["envKey"],
            "HIMIND_API_KEY"
        );
        assert_eq!(root["model"]["name"], "qwen3-coder-plus");
    }

    #[test]
    fn qwen_code_settings_merge_preserves_existing_keys() {
        let original = r#"{"mcpServers":{"github":{"command":"npx"}},"ui":{"theme":"dark"}}"#;
        let models = ["qwen3-coder-plus".to_string()];
        let updated = build_qwen_code_settings(
            original,
            &credential(&["qwen3-coder-plus"]),
            &models,
            "qwen3-coder-plus",
        )
        .unwrap();
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(root["mcpServers"]["github"]["command"], "npx");
        assert_eq!(root["ui"]["theme"], "dark");
        assert!(root["modelProviders"]["himind"].is_array());
        assert!(root["providerProtocol"]["himind"].is_string());
    }

    #[test]
    fn strip_qwen_code_only_removes_himind_fields() {
        let original = r#"{"env":{"HIMIND_API_KEY":"sk-1","OTHER":"v"},"modelProviders":{"himind":[{"id":"qwen3-coder-plus"}],"local":[{"id":"m1"}]},"providerProtocol":{"himind":"openai","local":"openai"},"mcpServers":{"s":{"command":"x"}}}"#;
        let (updated, changed) = strip_qwen_code_himind(original).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert!(root["env"].get("HIMIND_API_KEY").is_none());
        assert_eq!(root["env"]["OTHER"], "v");
        assert!(root["modelProviders"].get("himind").is_none());
        assert!(root["modelProviders"]["local"].is_array());
        assert!(root["providerProtocol"].get("himind").is_none());
        assert_eq!(root["providerProtocol"]["local"], "openai");
        assert!(root["mcpServers"]["s"].is_object());
    }

    #[test]
    fn strips_gateway_v1_suffix_for_anthropic_base_url() {
        assert_eq!(
            anthropic_base_url("https://himind.example.com/gateway/v1").unwrap(),
            "https://himind.example.com/gateway"
        );
        assert_eq!(
            anthropic_base_url("https://himind.example.com/gateway").unwrap(),
            "https://himind.example.com/gateway"
        );
        assert_eq!(
            anthropic_base_url("http://127.0.0.1:18090/gateway/v1/").unwrap(),
            "http://127.0.0.1:18090/gateway"
        );
    }

    #[test]
    fn builds_claude_settings_env_with_anthropic_gateway() {
        let credential = credential(&["claude-sonnet-5"]);
        let models = ["claude-sonnet-5".to_string()];
        let updated =
            build_claude_settings("", &credential, &models, "claude-sonnet-5", "Claude Code")
                .unwrap();
        let root: Value = serde_json::from_str(&updated).unwrap();
        // credential(helpers) 使用 base_url "https://ai.example.com/v1/"；anthropic 剥掉 /v1
        assert_eq!(root["env"][CLAUDE_BASE_URL_ENV], "https://ai.example.com");
        assert_eq!(root["env"][CLAUDE_CUSTOM_MODEL_OPTION], "claude-sonnet-5");
    }

    #[test]
    fn claude_settings_merge_preserves_existing_keys() {
        let original =
            r#"{"env":{"OTHER_VAR":"keep"},"permissions":{"allow":["Bash(npm test *)"]}}"#;
        let credential = credential(&["claude-sonnet-5"]);
        let models = ["claude-sonnet-5".to_string()];
        let updated = build_claude_settings(
            original,
            &credential,
            &models,
            "claude-sonnet-5",
            "Claude Code",
        )
        .unwrap();
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(root["env"]["OTHER_VAR"], "keep");
        assert_eq!(root["permissions"]["allow"][0], "Bash(npm test *)");
        assert!(root["env"][CLAUDE_BASE_URL_ENV].is_string());
    }

    #[test]
    fn strip_claude_only_removes_himind_env_keys() {
        let original = r#"{"env":{"ANTHROPIC_BASE_URL":"https://himind.example.com/gateway","ANTHROPIC_AUTH_TOKEN":"sk-1","ANTHROPIC_MODEL":"claude-sonnet-5","OTHER":"v"},"permissions":{"allow":["Bash(npm test *)"]}}"#;
        let (updated, changed) = strip_claude_himind(original, "Claude Code").unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert!(root["env"].get(CLAUDE_BASE_URL_ENV).is_none());
        assert!(root["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
        assert!(root["env"].get("ANTHROPIC_MODEL").is_none());
        assert_eq!(root["env"]["OTHER"], "v");
        assert_eq!(root["permissions"]["allow"][0], "Bash(npm test *)");
    }
}
