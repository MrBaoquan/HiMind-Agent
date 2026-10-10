use rand::{distributions::Alphanumeric, Rng, RngCore};
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
// OpenCode 通过 provider.<id>.npm 指定 AI SDK 适配包：OpenAI Chat 兼容用
// @ai-sdk/openai-compatible，/v1/responses 用 @ai-sdk/openai，Anthropic Messages
// 用 @ai-sdk/anthropic（官方文档点名的包）。
const OPENCODE_PROVIDER_ID: &str = "himind";
const OPENCODE_NPM_OPENAI_COMPATIBLE: &str = "@ai-sdk/openai-compatible";
const OPENCODE_NPM_OPENAI_RESPONSES: &str = "@ai-sdk/openai";
const OPENCODE_NPM_ANTHROPIC: &str = "@ai-sdk/anthropic";
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
    /// 目标客户端已注册其它来源时，先撤销旧注册再写入新来源。
    ///
    /// 默认 `false`：一个客户端同时只属于一个来源，冲突时先返回错误，
    /// 让调用方明确表达"切换"意图，而不是被动覆盖用户已有的注册。
    #[serde(default)]
    pub replace: bool,
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

    /// 导入前，对「本次会被覆盖、且属于用户既有配置」的键做快照，随簿记一起落盘。
    ///
    /// 取消导入时按同一份快照精确还原用户原值；只做新增式合并、不会覆盖用户既有值
    /// 的客户端返回 `None`（默认）。快照是适配器私有的不透明 JSON，其他层不解释。
    fn owned_snapshot(&self, _options: &Options) -> Option<Value> {
        None
    }

    /// 取消导入。`restore` 为导入时记录的快照，`None` 表示旧簿记或无快照，
    /// 此时只移除 HiMind 自己写入、且仍带 HiMind 标记的键。
    fn cancel(
        &self,
        options: &Options,
        restore: Option<&Value>,
    ) -> Result<AIProviderImportCancelResult, Box<dyn Error>>;
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
    fn cancel(
        &self,
        options: &Options,
        _restore: Option<&Value>,
    ) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
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
    fn cancel(
        &self,
        _options: &Options,
        _restore: Option<&Value>,
    ) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
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
    fn owned_snapshot(&self, _options: &Options) -> Option<Value> {
        codex_owned_snapshot()
    }
    fn cancel(
        &self,
        options: &Options,
        restore: Option<&Value>,
    ) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_codex(options, restore)
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
    fn cancel(
        &self,
        _options: &Options,
        _restore: Option<&Value>,
    ) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
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
    fn owned_snapshot(&self, _options: &Options) -> Option<Value> {
        kimi_code_owned_snapshot()
    }
    fn cancel(
        &self,
        _options: &Options,
        restore: Option<&Value>,
    ) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_kimi_code(restore)
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
    fn owned_snapshot(&self, _options: &Options) -> Option<Value> {
        qwen_code_owned_snapshot()
    }
    fn cancel(
        &self,
        _options: &Options,
        restore: Option<&Value>,
    ) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_qwen_code(restore)
    }
}

pub(crate) struct ClaudeCodeAdapter;
pub(crate) struct ClaudeDesktopAdapter;
pub(crate) struct OpenCodeAdapter;

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
    fn owned_snapshot(&self, _options: &Options) -> Option<Value> {
        claude_owned_snapshot(&claude_code_settings_path())
    }
    fn cancel(
        &self,
        _options: &Options,
        restore: Option<&Value>,
    ) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_claude_code(restore)
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
    fn owned_snapshot(&self, _options: &Options) -> Option<Value> {
        claude_desktop_owned_snapshot()
    }
    fn cancel(
        &self,
        _options: &Options,
        restore: Option<&Value>,
    ) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_claude_desktop(restore)
    }
}

impl AIClientAdapter for OpenCodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }
    fn display_name(&self) -> &'static str {
        "OpenCode"
    }
    fn status(&self, _options: &Options) -> AIProviderImportStatus {
        opencode_import_status()
    }
    fn plan(&self, action: &str, status: &AIProviderImportStatus) -> AIProviderImportPlan {
        plan_for("opencode", action, status)
    }
    fn import(
        &self,
        options: &Options,
        user_id: &str,
        service: &str,
    ) -> Result<AIProviderImportResult, Box<dyn Error>> {
        import_opencode(options, user_id, service)
    }
    fn cancel(
        &self,
        _options: &Options,
        _restore: Option<&Value>,
    ) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_opencode()
    }
}

pub(crate) fn adapter_for(target: &str) -> Option<&'static dyn AIClientAdapter> {
    let target = target.trim();
    match target {
        "vscode" => Some(&VSCodeAdapter),
        "cc-switch" => Some(&CCSwitchAdapter),
        "codex" => Some(&CodexAdapter),
        "workbuddy" => Some(&WorkBuddyAdapter),
        "kimi-code" => Some(&KimiCodeAdapter),
        "qwen-code" => Some(&QwenCodeAdapter),
        "claude-code" => Some(&ClaudeCodeAdapter),
        "claude-desktop" => Some(&ClaudeDesktopAdapter),
        "opencode" => Some(&OpenCodeAdapter),
        // 声明式适配表：新增同类客户端只加一行数据。
        _ => declarative_provider_adapters()
            .into_iter()
            .find(|adapter| adapter.id() == target),
    }
}

pub(crate) fn known_adapters() -> Vec<&'static dyn AIClientAdapter> {
    let mut adapters: Vec<&'static dyn AIClientAdapter> = vec![
        &VSCodeAdapter,
        &CCSwitchAdapter,
        &CodexAdapter,
        &WorkBuddyAdapter,
        &KimiCodeAdapter,
        &QwenCodeAdapter,
        &ClaudeCodeAdapter,
        &ClaudeDesktopAdapter,
        &OpenCodeAdapter,
    ];
    adapters.extend(declarative_provider_adapters());
    adapters
}

pub(crate) fn known_adapter_ids() -> Vec<&'static str> {
    known_adapters()
        .into_iter()
        .map(AIClientAdapter::id)
        .collect()
}

#[derive(Debug, Serialize, Default)]
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
    /// ADR 0114 生命周期：写入落盘后的同步状态。
    /// `file_written` = 已写入并复核；`conflict` = 写入后复核未通过。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sync_status: String,
    /// ADR 0114 生命周期：验证等级。
    /// `file_verified` = 回读目标文件确认 HiMind 标记在位；
    /// `client_load_unverified` = 写入本身正确，但客户端是否加载无法证明；
    /// `client_load_verified` = 有真实探针（本机网关可达）证明客户端能加载。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub verification_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AIProviderImportBinding {
    service: String,
    /// 注入模式：空串 = 直连（旧格式与默认）；`gateway` = 走本机推理网关。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    mode: String,
    /// 网关模式的上游与令牌事实；直连模式为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gateway: Option<AIProviderGatewayBinding>,
    /// 最近一次写入该客户端配置的时间（RFC 3339）。旧簿记没有这个字段，
    /// 因此默认空串，UI 在没有时间可显示时就不显示这一行。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    updated_at: String,
    /// 导入前被覆盖字段的原值快照（适配器私有格式）。取消导入时按它还原用户
    /// 原有配置；旧簿记没有这个字段，退化为「只移除自己写入的键」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restore: Option<Value>,
    /// ADR 0114：最近一次写入后复核得到的验证等级，供状态检测回显。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    verification_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AIProviderImportBindings {
    #[serde(default)]
    clients: HashMap<String, AIProviderImportBinding>,
}

#[derive(Debug, Serialize, Default)]
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
    /// ADR 0114：该客户端最近一次写入后的验证等级（来自簿记）。客户端自身状态探测
    /// 无法证明「已加载」时，UI 用它区分「文件已写入」与「客户端已加载」。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub verification_status: String,
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

/// 计划里要展示的写入/备份文件：声明式客户端可能有多个文件（ZCode 是两份），
/// 其余客户端沿用状态里的单一配置路径。
fn planned_config_paths(target: &str, status: &AIProviderImportStatus) -> Vec<String> {
    if let Some(definition) = provider_target_definition(target) {
        return provider_config_file_paths(definition)
            .into_iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect();
    }
    if status.config_path.is_empty() {
        Vec::new()
    } else {
        vec![status.config_path.clone()]
    }
}

fn plan_import(target: &str, status: &AIProviderImportStatus) -> (Vec<String>, Vec<String>) {
    let mut will_write = Vec::new();
    let mut will_backup = Vec::new();
    let paths = planned_config_paths(target, status);
    for path in paths {
        will_backup.push(path.clone());
        will_write.push(path);
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
                "在 Claude Desktop 第三方档案（Claude-3p）的 configLibrary 写入 HiMind 网关条目"
                    .to_string(),
            );
            will_write.push(
                "把第三方档案设为当前档案（claude_desktop_config.json 的 deploymentMode）"
                    .to_string(),
            );
            will_write.push("模型由网关 /v1/models 自动发现，无需在档案里声明".to_string());
        }
        "opencode" => {
            will_write
                .push("写入 OpenCode opencode.json 的 provider.himind 与模型目录".to_string());
        }
        _ => {}
    }
    if let Some(definition) = provider_target_definition(target) {
        will_write.push(definition.import_summary.to_string());
    }
    (will_write, will_backup)
}

fn plan_remove(target: &str, status: &AIProviderImportStatus) -> (Vec<String>, Vec<String>) {
    let mut will_write = Vec::new();
    let mut will_backup = Vec::new();
    let paths = planned_config_paths(target, status);
    for path in paths {
        will_backup.push(path.clone());
        will_write.push(path);
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
                "从 Claude Desktop 第三方档案的 configLibrary 移除 HiMind 网关条目".to_string(),
            );
            will_write
                .push("保留 deploymentMode 与用户的其它档案条目、mcpServers 不变".to_string());
        }
        "opencode" => {
            will_write.push("移除 OpenCode opencode.json 中的 provider.himind".to_string());
        }
        _ => {}
    }
    if let Some(definition) = provider_target_definition(target) {
        will_write.push(definition.remove_summary.to_string());
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
    let target = request.target.trim();
    // 一个客户端只能绑定一个来源：相同来源再次执行即为同步；不同来源需要
    // 显式请求切换（`replace`），否则先返回错误，由调用方决定是否切换。
    let service_source = request.service_source();
    let current = status(options)
        .targets
        .into_iter()
        .find(|item| item.target == target);
    // `Some(prior_restore)` 表示本次是「同一来源的重复导入」：沿用首次导入时保存的
    // 原始快照，不要重新采集——此时目标文件已经含 HiMind 内容，重新采集会把原始
    // 快照覆盖成 HiMind 状态，导致取消时无法还原用户在导入前的配置。
    let mut prior_binding_restore: Option<Option<Value>> = None;
    if current
        .as_ref()
        .is_some_and(|item| item.state == "imported")
    {
        let bindings = load_import_bindings(options);
        let existing_binding = bindings.clients.get(target);
        let existing = existing_binding.map(|binding| binding.service.clone());
        if existing.as_deref() != Some(service_source) {
            if !request.replace {
                return Err(match existing {
                    Some(_) => {
                        format!("客户端 {target} 已注册其他模型服务，请先取消分发后再切换").into()
                    }
                    None => {
                        format!("客户端 {target} 的注册来源未知，请先取消注册后再重新注册").into()
                    }
                });
            }
            // 切换来源：先按旧快照撤销旧注册（会写回客户端的原始配置并留下备份），
            // 再按新来源写入。撤销后立刻落盘簿记，避免中途失败留下错误的归属。
            let previous_restore = existing_binding.and_then(|binding| binding.restore.clone());
            adapter.cancel(options, previous_restore.as_ref())?;
            let mut bindings = load_import_bindings(options);
            bindings.clients.remove(target);
            save_import_bindings(options, &bindings)?;
        } else {
            prior_binding_restore =
                Some(existing_binding.and_then(|binding| binding.restore.clone()));
        }
    }
    // 快照在写入前采集，且排在「切换来源」的还原之后：此时客户端已是用户原始配置。
    // 同一来源重复导入时沿用首次快照（见上），否则重新采集。
    let snapshot = match prior_binding_restore {
        Some(restore) => restore,
        None => adapter.owned_snapshot(options),
    };
    let mut result = adapter.import(options, expected_user_id, service_source)?;
    // ADR 0114 §2：写入后按内容判定 `file_written` / `file_unchanged`（重复导入同一
    // 来源时客户端配置逐字节不变，应报「已是最新」而不是又写了一次）。
    result.sync_status = if import_was_noop(&result) {
        "file_unchanged".to_string()
    } else {
        "file_written".to_string()
    };
    // ADR 0114：写入不是完成，回读目标确认 HiMind 标记实际落盘才算 `file_verified`。
    // 客户端是否「已加载」外部无法可靠证明（有些客户端是异步完成注册的），
    // 缺省只能到 `client_load_unverified`。
    let verification_status = if client_config_verified(options, target) {
        FILE_VERIFIED.to_string()
    } else {
        CLIENT_LOAD_UNVERIFIED.to_string()
    };
    result.verification_status = verification_status.clone();
    let mut bindings = load_import_bindings(options);
    bindings.clients.insert(
        target.to_string(),
        AIProviderImportBinding {
            service: service_source.to_string(),
            // 走现有导入路径就是直连：真实凭据写进客户端，用量不计入本机口径。
            mode: String::new(),
            gateway: None,
            updated_at: now_rfc3339(),
            restore: snapshot,
            verification_status,
        },
    );
    save_import_bindings(options, &bindings)?;
    Ok(result)
}

/// ADR 0114 验证等级取值。
const FILE_VERIFIED: &str = "file_verified";
const CLIENT_LOAD_UNVERIFIED: &str = "client_load_unverified";
const CLIENT_LOAD_VERIFIED: &str = "client_load_verified";

/// 回读客户端状态，确认 HiMind 标记真实落盘（ADR 0114 `file_verified`）。
fn client_config_verified(options: &Options, target: &str) -> bool {
    status(options)
        .targets
        .into_iter()
        .any(|item| item.target == target && item.state == "imported")
}

/// 判断本次导入是否实际改动了目标配置（ADR 0114 `file_unchanged`）。
///
/// `backup_and_write` 在写入前把原文件复制成 `.himind-backup-*.bak`，所以备份内容就是
/// 写前内容；若它与写后内容逐字节相同，说明这次导入是空操作。目标文件原本不存在
/// （无备份）或适配器没有主配置文件时无法判断，保守地按 `file_written`。
fn import_was_noop(result: &AIProviderImportResult) -> bool {
    if result.backup_path.trim().is_empty() || result.config_path.trim().is_empty() {
        return false;
    }
    match (fs::read(&result.backup_path), fs::read(&result.config_path)) {
        (Ok(backup), Ok(current)) => backup == current,
        _ => false,
    }
}

/// 簿记时间戳。用 UTC 秒级 RFC 3339，跨时区显示交给 UI。
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
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

/// 网关模式的上游事实：真实凭据只在 Agent 内保存（DPAPI 保护），客户端只拿到本机令牌。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AIProviderGatewayBinding {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    token_protected: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    base_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    api_key_protected: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    protocol: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    models: Vec<String>,
    /// 翻译请求时的模型名兜底。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    default_model: String,
}

const BINDING_MODE_GATEWAY: &str = "gateway";

fn new_binding_token() -> String {
    use base64::Engine as _;
    let mut bytes = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 切换到本机网关模式（ADR 0113）。P1 只落地 Codex：其余客户端要等各自的写入面
/// 补齐，宁可不做，也不写半套配置。
pub(crate) fn enable_gateway_binding(
    options: &Options,
    expected_user_id: &str,
    target: &str,
    service: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let target = target.trim();
    let adapter = adapter_for(target).ok_or_else(|| format!("不支持的 AI 客户端：{target}"))?;
    let gateway_url =
        crate::app::inference_gateway::url().ok_or("本机推理网关未启动，无法切换到网关模式")?;
    let credential = resolve_credential(options, expected_user_id, "gateway-binding", service)?;
    let models = available_models(&credential)?;
    let preferred = preferred_model(&credential)?;
    let token = new_binding_token();
    // 客户端看到的是网关，网关再按上游真实协议转发或互译。
    let proxy_protocol = gateway_proxy_protocol(adapter.id(), &credential.access.protocol)?;
    let mut client_access = credential.access.clone();
    client_access.base_url = gateway_url.clone();
    client_access.protocol = proxy_protocol.to_string();
    let client_credential = AIClientCredential {
        access: client_access,
        api_key: token.clone(),
    };
    let service_source = if service.trim().is_empty() {
        "managed".to_string()
    } else {
        service.trim().to_string()
    };
    // 走客户端自己的写入面：把「客户端看到的凭据」临时替换成网关凭据，
    // 各适配器的协议校验、备份、合并与还原逻辑全部原样复用。
    let mut result = {
        let _guard = set_gateway_override(&service_source, client_credential);
        import(
            options,
            expected_user_id,
            &AIProviderImportRequest {
                target: target.to_string(),
                service: if service_source == "managed" {
                    String::new()
                } else {
                    service_source.clone()
                },
                replace: true,
            },
        )?
    };
    // 网关模式的意义就是请求真的经过本机网关：探一次绑定令牌能否命中，
    // 能命中才能升级为 `client_load_verified`（ADR 0114）。
    let verification_status = if gateway_binding_reachable(&gateway_url, &token) {
        result.sync_status = "file_written".to_string();
        result.verification_status = CLIENT_LOAD_VERIFIED.to_string();
        CLIENT_LOAD_VERIFIED.to_string()
    } else {
        result.verification_status.clone()
    };

    // 客户端配置已写好，再把绑定事实改成网关模式：真实凭据只留在 Agent。
    let mut bindings = load_import_bindings(options);
    bindings.clients.insert(
        target.to_string(),
        AIProviderImportBinding {
            service: service_source.clone(),
            mode: BINDING_MODE_GATEWAY.to_string(),
            gateway: Some(AIProviderGatewayBinding {
                token_protected: crate::store::credentials::protect_secret_for_current_user(
                    &token,
                )?,
                base_url: credential.access.base_url.clone(),
                api_key_protected: crate::store::credentials::protect_secret_for_current_user(
                    &credential.api_key,
                )?,
                protocol: credential.access.protocol.clone(),
                models: models.clone(),
                default_model: preferred.clone(),
            }),
            updated_at: now_rfc3339(),
            // 保留 import 刚写下的「用户原始配置」快照，切回直连时用它还原。
            restore: bindings
                .clients
                .get(target)
                .and_then(|binding| binding.restore.clone()),
            verification_status,
        },
    );
    save_import_bindings(options, &bindings)?;
    Ok(result)
}

/// 探本机网关是否认可这枚绑定令牌（`GET /v1/models`）。
/// 命中即可证明「客户端走网关」这条链路真实可用（ADR 0114 `client_load_verified`）。
/// 探测失败只降级验证等级，不影响已经写好的客户端配置。
fn gateway_binding_reachable(gateway_url: &str, token: &str) -> bool {
    let url = format!("{}/v1/models", gateway_url.trim_end_matches('/'));
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return false;
    };
    client
        .get(url)
        .bearer_auth(token)
        .header("x-api-key", token)
        .send()
        .map(|response| response.status().is_success())
        .unwrap_or(false)
}

/// 客户端要求的入口协议与上游真实协议的组合是否有互译实现。
///
/// 只做本轮用得到的组合：Anthropic 入口 ↔ OpenAI Chat 上游。其它组合必须
/// 明确报错，而不是让客户端拿到一个它读不懂的响应。
fn gateway_proxy_protocol(
    adapter_id: &str,
    upstream_protocol: &str,
) -> Result<&'static str, Box<dyn Error>> {
    // Codex 0.150 起只讲 Responses（`wire_api = "chat"` 已被移除）：客户端看到
    // Responses，Chat 类上游由网关互译，否则会写出一个 CLI 直接拒绝加载的配置。
    if adapter_id == "codex" {
        return match upstream_protocol {
            "openai-responses" | "openai-chat" => Ok("openai-responses"),
            other => Err(format!("Codex 暂不支持 {other} 上游").into()),
        };
    }
    let anthropic_client = matches!(adapter_id, "claude-code" | "claude-desktop" | "zcode");
    if anthropic_client {
        if upstream_protocol == "openai-chat" {
            return Ok("anthropic");
        }
        return Err(
            format!("该客户端只讲 Anthropic 协议，暂不支持接入 {upstream_protocol} 上游").into(),
        );
    }
    match upstream_protocol {
        "openai-chat" => Ok("openai-chat"),
        "openai-responses" => Ok("openai-responses"),
        other => Err(format!("暂不支持把 {other} 上游接入这类客户端").into()),
    }
}

/// 客户端配置里写入的凭据在导入期间被临时替换成网关凭据。
///
/// 单用户桌面应用，导入操作由界面串行化，所以这个窗口足够小；用锁而不是
/// 全局变量是为了让并发调用下也不会读到半套状态。
struct GatewayCredentialOverride {
    service: String,
    credential: AIClientCredential,
}

static GATEWAY_CREDENTIAL_OVERRIDE: OnceLock<Mutex<Option<GatewayCredentialOverride>>> =
    OnceLock::new();

fn gateway_override_slot() -> &'static Mutex<Option<GatewayCredentialOverride>> {
    GATEWAY_CREDENTIAL_OVERRIDE.get_or_init(|| Mutex::new(None))
}

fn set_gateway_override(service: &str, credential: AIClientCredential) -> GatewayOverrideGuard {
    if let Ok(mut slot) = gateway_override_slot().lock() {
        *slot = Some(GatewayCredentialOverride {
            service: service.trim().to_string(),
            credential,
        });
    }
    GatewayOverrideGuard
}

struct GatewayOverrideGuard;

impl Drop for GatewayOverrideGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = gateway_override_slot().lock() {
            *slot = None;
        }
    }
}

fn gateway_override_for(service: &str) -> Option<AIClientCredential> {
    let slot = gateway_override_slot().lock().ok()?;
    let current = slot.as_ref()?;
    if current.service != service.trim() {
        return None;
    }
    Some(current.credential.clone())
}

/// 切回直连：丢弃本机令牌，把真实凭据按现有导入路径写回客户端。
pub(crate) fn disable_gateway_binding(
    options: &Options,
    expected_user_id: &str,
    target: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let target = target.trim();
    let bindings = load_import_bindings(options);
    let service = bindings
        .clients
        .get(target)
        .map(|binding| binding.service.clone())
        .unwrap_or_else(|| "managed".to_string());
    let result = import(
        options,
        expected_user_id,
        &AIProviderImportRequest {
            target: target.to_string(),
            service: if service == "managed" {
                String::new()
            } else {
                service
            },
            replace: true,
        },
    )?;
    let mut bindings = load_import_bindings(options);
    if let Some(binding) = bindings.clients.get_mut(target) {
        binding.mode.clear();
        binding.gateway = None;
    }
    save_import_bindings(options, &bindings)?;
    Ok(result)
}

/// 网关启动时解析所有网关模式绑定。真实凭据在此解密，随后只留在内存。
pub(crate) fn gateway_bindings(
    options: &Options,
) -> Vec<crate::app::inference_gateway::GatewayBinding> {
    load_import_bindings(options)
        .clients
        .iter()
        .filter_map(|(client, binding)| {
            if binding.mode != BINDING_MODE_GATEWAY {
                return None;
            }
            let gateway = binding.gateway.as_ref()?;
            let token = crate::store::credentials::unprotect_secret_for_current_user(
                &gateway.token_protected,
            )
            .ok()?;
            let api_key = crate::store::credentials::unprotect_secret_for_current_user(
                &gateway.api_key_protected,
            )
            .ok()?;
            Some(crate::app::inference_gateway::GatewayBinding {
                id: format!("{client}:{}", binding.service),
                client: client.clone(),
                service: binding.service.clone(),
                models: gateway.models.clone(),
                default_model: if gateway.default_model.trim().is_empty() {
                    gateway.models.first().cloned().unwrap_or_default()
                } else {
                    gateway.default_model.clone()
                },
                protocol: if gateway.protocol.trim().is_empty() {
                    "openai-responses".to_string()
                } else {
                    gateway.protocol.clone()
                },
                base_url: gateway.base_url.clone(),
                api_key,
                token,
                platform_metered: binding.service == "managed",
            })
        })
        .collect()
}

/// 直连注入的客户端清单：其用量不计入本机口径，面板据此点名。
pub(crate) fn direct_bound_clients(options: &Options) -> Vec<String> {
    let mut clients = load_import_bindings(options)
        .clients
        .into_iter()
        .filter(|(_, binding)| binding.mode != BINDING_MODE_GATEWAY)
        .map(|(client, _)| client)
        .collect::<Vec<_>>();
    clients.sort();
    clients
}

/// 某客户端当前绑定的服务源；未绑定时为 `None`。
pub(crate) fn binding_service(options: &Options, target: &str) -> Option<String> {
    load_import_bindings(options)
        .clients
        .get(target.trim())
        .map(|binding| binding.service.clone())
        .filter(|service| !service.trim().is_empty())
}

/// 删除自定义 AI 服务前的占用检查。
///
/// 只有**真正绑定了该服务**的客户端会阻止删除：簿记里 `service == custom:<id>` 时删掉
/// 服务会留下悬空的归属，必须先断开。
///
/// **来源不明**的注册（旧版本或人工写入的 `provider.himind`，簿记里查不到归属）不阻止
/// 删除，也不该阻止：它没有指向任何具体服务 id，删服务既不会改写客户端配置，也不会让
/// 已有绑定悬空。这类注册在 UI 上单独标注、可单独断开，而不是把整个服务列表锁死。
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
    if clients.is_empty() {
        return Ok(());
    }
    Err(format!(
        "请先断开正在使用该服务的 AI 工具（{}），再删除此服务",
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
                        status.verification_status = binding.verification_status.clone();
                        // 客户端自己不记时间时，用簿记时间兜底，UI 才能显示「最近同步」。
                        if status.synced_at.is_empty() {
                            status.synced_at = binding.updated_at.clone();
                        }
                    }
                }
                status
            })
            .collect(),
    }
}

pub(crate) fn cancel(
    options: &Options,
    target: &str,
) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let adapter = adapter_for(target).ok_or_else(|| format!("不支持的 AI 客户端：{target}"))?;
    let bindings = load_import_bindings(options);
    let restore = bindings
        .clients
        .get(target.trim())
        .and_then(|binding| binding.restore.clone());
    let result = adapter.cancel(options, restore.as_ref())?;
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
    // 网关模式下，写进客户端的是「网关地址 + 本机令牌」；真实凭据留在 Agent。
    if let Some(credential) = gateway_override_for(service) {
        return Ok(credential);
    }
    if service.is_empty() || service == "managed" {
        if !options.mode().dashboard_enabled() {
            return Err(
                "HiMind 分发服务需要先对接 AI 工作台；未对接时请从本机自定义模型服务导入".into(),
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
    ensure_openai_compatible(&credential, "VS Code 扩展")?;
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
        ..Default::default()
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
    ensure_openai_compatible(&credential, "CC Switch")?;
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
        ..Default::default()
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
    ensure_openai_compatible(&credential, "Codex")?;
    // Codex 0.150 起 `wire_api = "chat"` 已不再被接受；直接分发 chat 类服务
    // 会写出一个 CLI 拒绝加载的 config.toml，必须在写入前拦住。
    if openai_protocol_is_chat(&credential) {
        return Err(
            "Codex 只接受 Responses 协议的服务，当前服务是 chat 协议；请改用 Responses 类服务"
                .into(),
        );
    }
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
        ..Default::default()
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
            // Codex 0.150 的模型目录把 comp_hash 当字符串读；写成整数会让整个
            // 目录解析失败，客户端直接拒绝启动。
            "comp_hash": "3000",
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
            "检测到 Codex 已配置 HiMind 供应商，但缺少模型目录（himind-models.json）；重新导入可自动重建".to_string()
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
        ..Default::default()
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

/// Codex 的导入会接管 `model`、`model_provider`、登录方式、推理档位、模型目录与
/// `[model_providers.himind]`。其中前几项多半是用户自己的选择，取消导入必须还原，
/// 而不是删掉 —— 否则用户自选的模型与推理档位会被永久改成 HiMind 的默认值。
fn codex_owned_snapshot() -> Option<Value> {
    let config_file = codex_config_path().join("config.toml");
    let text = fs::read_to_string(&config_file).ok()?;
    Some(json!({ "config": text }))
}

fn cancel_codex(
    _options: &Options,
    restore: Option<&Value>,
) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
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
    let previous = restore
        .and_then(|value| value.get("config"))
        .and_then(Value::as_str);
    let (updated, changed) = strip_codex_himind(&original_config, &models_path, previous)?;
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

/// Codex 导入会覆盖的顶层键。
const CODEX_OWNED_KEYS: [&str; 6] = [
    "model",
    "model_provider",
    "preferred_auth_method",
    "forced_login_method",
    "model_reasoning_effort",
    "model_catalog_json",
];

/// 移除 HiMind 写入的内容。
///
/// 有导入快照时按快照还原：导入前存在该键就写回原值，导入前不存在才删除；
/// 没有快照（旧簿记）时退化为保守策略 —— 只移除仍带 HiMind 标记的字段，
/// 用户其他配置一律不动。
fn strip_codex_himind(
    config: &str,
    models_path: &Path,
    previous: Option<&str>,
) -> Result<(String, bool), Box<dyn Error>> {
    let mut document = config.parse::<DocumentMut>()?;
    let previous_document = previous
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .and_then(|text| text.parse::<DocumentMut>().ok());
    let Some(previous_document) = previous_document else {
        return strip_codex_himind_conservative(document, models_path);
    };
    let mut changed = false;
    for key in CODEX_OWNED_KEYS {
        changed |= restore_document_key(document.as_table_mut(), key, &previous_document);
    }
    if let Some(providers) = document
        .as_table_mut()
        .get_mut("model_providers")
        .and_then(|item| item.as_table_mut())
    {
        let previous_provider = previous_document
            .as_table()
            .get("model_providers")
            .and_then(|item| item.as_table())
            .and_then(|table| table.get(CODEX_PROVIDER_ID))
            .cloned();
        match previous_provider {
            Some(entry) => {
                providers.insert(CODEX_PROVIDER_ID, entry);
                changed = true;
            }
            None => {
                if providers.remove(CODEX_PROVIDER_ID).is_some() {
                    changed = true;
                }
            }
        }
    }
    Ok((document.to_string(), changed))
}

/// 把 `key` 还原成快照里的值；快照没有该键时删除。返回是否有改动。
fn restore_document_key(table: &mut Table, key: &str, previous: &DocumentMut) -> bool {
    match previous.as_table().get(key) {
        Some(item) => {
            if table.get(key).map(|current| current.to_string()) == Some(item.to_string()) {
                return false;
            }
            table.insert(key, item.clone());
            true
        }
        None => table.remove(key).is_some(),
    }
}

/// 无快照时的保守清理：只移除 HiMind 明确写入、且仍带 HiMind 标记的字段。
fn strip_codex_himind_conservative(
    mut document: DocumentMut,
    models_path: &Path,
) -> Result<(String, bool), Box<dyn Error>> {
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
    ensure_openai_compatible(&credential, "WorkBuddy")?;
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
    }
}

fn parse_vscode_import_status(content: &str) -> Result<VSCodeImportStatusFile, serde_json::Error> {
    serde_json::from_str(content)
}

fn managed_workbuddy_model_ids(root: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    root.get("models")
        .and_then(Value::as_array)
        // WorkBuddy 自己写的顶层数组形态：数组本身就是模型列表。
        .or_else(|| root.as_array())
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
    let (mut root, array_root) = workbuddy_root_value(content, "取消导入")?;
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
    Ok((workbuddy_serialize(&root, array_root)?, removed))
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
    ensure_openai_compatible(&credential, "Kimi Code")?;
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
        ..Default::default()
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
        ..Default::default()
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

fn cancel_kimi_code(restore: Option<&Value>) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let path = kimi_code_config_path();
    let client_detected = path.is_file();
    let original = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let previous = restore.and_then(|value| value.get("settings")).and_then(Value::as_str);
    let (updated, removed) = strip_kimi_code_himind(&original, previous)?;
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

// 移除 HiMind 明确写入的字段（providers.himind、himind/* 别名、default_model），
// 用户其他配置原样保留。`default_model` 若被导入覆盖，按快照还原用户原值，
// 而不是一律删除（否则用户在导入前设的默认模型会永久丢失）。
fn strip_kimi_code_himind(
    original: &str,
    previous: Option<&str>,
) -> Result<(String, bool), Box<dyn Error>> {
    let mut document = original.parse::<DocumentMut>()?;
    let previous_default = previous
        .filter(|text| !text.trim().is_empty())
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .and_then(|doc| {
            doc.get("default_model")
                .and_then(|item| item.as_str())
                .map(str::to_string)
        });
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
    let points_at_himind = document
        .get("default_model")
        .and_then(|item| item.as_str())
        .is_some_and(|value| value.starts_with(KIMI_CODE_HIMIND_PREFIX));
    if points_at_himind {
        // 快照里的原值若也是 himind/*（用户此前就导入过），取消时仍应清空，避免
        // 留下指向已删除别名的悬空默认值。
        match previous_default.filter(|value| !value.starts_with(KIMI_CODE_HIMIND_PREFIX)) {
            Some(previous) => {
                document["default_model"] = value(previous.as_str());
                changed = true;
            }
            None => {
                document.remove("default_model");
                changed = true;
            }
        }
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
    ensure_openai_compatible(&credential, "Qwen Code")?;
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
        ..Default::default()
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
        ..Default::default()
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

/// 读取配置文件全文（`{"settings": "<原文>"}`）作为导入前快照，用于取消时还原被
/// 导入覆盖的、属于用户的键（如默认模型）。Qwen Code / Kimi Code 共用同一形状。
fn settings_file_snapshot(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    Some(json!({ "settings": text }))
}

/// Qwen Code 的导入会覆盖用户既有的默认模型 `model`，取消时必须还原。
fn qwen_code_owned_snapshot() -> Option<Value> {
    settings_file_snapshot(&qwen_code_settings_path())
}

/// Kimi Code 的导入会覆盖用户既有的 `default_model`，取消时必须还原。
fn kimi_code_owned_snapshot() -> Option<Value> {
    settings_file_snapshot(&kimi_code_config_path())
}

fn cancel_qwen_code(restore: Option<&Value>) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let path = qwen_code_settings_path();
    let client_detected = path.is_file();
    let original = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let previous = restore.and_then(|value| value.get("settings")).and_then(Value::as_str);
    let (updated, removed) = strip_qwen_code_himind(&original, previous)?;
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
fn strip_qwen_code_himind(
    original: &str,
    previous: Option<&str>,
) -> Result<(String, bool), Box<dyn Error>> {
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
    // 导入时我们写入了 modelProviders.himind；它的存在即代表默认模型 `model`
    // 也是被我们覆盖的，取消时按快照还原，而不是按模型名去猜（写入的是裸模型名，
    // 不带 himind 前缀，按前缀判定永远命中不了，会留下悬空默认模型）。
    let had_himind_provider = object
        .get_mut("modelProviders")
        .and_then(Value::as_object_mut)
        .is_some_and(|providers| providers.remove(QWEN_CODE_PROVIDER_ID).is_some());
    if had_himind_provider {
        changed = true;
    }
    if let Some(protocols) = object
        .get_mut("providerProtocol")
        .and_then(Value::as_object_mut)
    {
        if protocols.remove(QWEN_CODE_PROVIDER_ID).is_some() {
            changed = true;
        }
    }
    if had_himind_provider {
        let previous_model = previous
            .filter(|text| !text.trim().is_empty())
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .and_then(|restore| restore.get("model").cloned());
        match previous_model {
            Some(model) if object.get("model") != Some(&model) => {
                object.insert("model".to_string(), model);
                changed = true;
            }
            Some(_) => {}
            None => {
                if object.remove("model").is_some() {
                    changed = true;
                }
            }
        }
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
    Ok(crate::store::ai_services::anthropic_api_root(&base))
}

fn claude_code_settings_path() -> PathBuf {
    if let Some(dir) = env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(dir).join("settings.json");
    }
    user_home().join(".claude").join("settings.json")
}

// Claude Desktop 的推理接入走「第三方档案」（3P）配置库，而不是 1P 时代的 env 块：
//
// * 档案目录：`CLAUDE_USER_DATA_DIR` 优先；Windows 为 `%LOCALAPPDATA%\Claude-3p`
//   （Electron 的 userData 在 Windows 上取自 LOCALAPPDATA；`%APPDATA%\Claude-3p`
//   只是旧版迁移源，写在那里不会被读取）；macOS/Linux 是 1P 目录名追加 `-3p`。
// * 目录里的 `claude_desktop_config.json` 用顶层 `deploymentMode: "3p"` 标记当前档案；
// * `configLibrary/_meta.json` 记录 `appliedId` 与条目列表，真正生效的配置是
//   `configLibrary/<appliedId>.json`；条目用扁平键（`inferenceProvider`、
//   `inferenceGatewayBaseUrl`、`inferenceGatewayApiKey`、`inferenceGatewayAuthScheme`、
//   `inferenceCredentialKind`）。
// * 客户端启动判据：生效条目带 `inference`/`bootstrap`/`selfHosted` 且 `deploymentMode`
//   不为 `1p` 时切到 3P（见 `claude_desktop_third_party_enabled`）。
//
// 3P 模式会把 Claude Desktop 的 userData 指向 3P 目录，`mcpServers` 也随之落在这个
// 目录的 `claude_desktop_config.json`（见 `claude_desktop_app_config_path`），
// 因此 MCP 注册不能只写 1P 的 `%APPDATA%\Claude`。
const CLAUDE_DESKTOP_CONFIG_FILE: &str = "claude_desktop_config.json";
const CLAUDE_DESKTOP_CONFIG_LIBRARY_DIR: &str = "configLibrary";
const CLAUDE_DESKTOP_LIBRARY_META_FILE: &str = "_meta.json";
const CLAUDE_DESKTOP_ENTRY_NAME: &str = "HiMind";
// `note` 是 Claude Desktop 配置库条目的自由字段，用它标记归属，避免误删用户条目。
const CLAUDE_DESKTOP_ENTRY_NOTE: &str = "himind-agent";
const CLAUDE_DESKTOP_DEPLOYMENT_MODE_KEY: &str = "deploymentMode";
const CLAUDE_DESKTOP_FIRST_PARTY_MODE: &str = "1p";
const CLAUDE_DESKTOP_THIRD_PARTY_MODE: &str = "3p";
const CLAUDE_DESKTOP_THIRD_PARTY_SUFFIX: &str = "-3p";
// 3P 条目带的来源标记头（`inferenceCustomHeaders`）。网关据此只对 Claude Desktop
// 表面返回 Anthropic 形态的路由名，其它客户端继续拿到规范模型名。
const CLAUDE_DESKTOP_SURFACE_HEADER: &str = "X-Himind-Surface";
const CLAUDE_DESKTOP_SURFACE_VALUE: &str = "claude-desktop";

/// Electron 的 `appData` 根：Windows 是 `%APPDATA%`，macOS/Linux 是 1P 档案所在的根。
fn claude_desktop_app_data_root() -> PathBuf {
    if cfg!(windows) {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home().join("AppData").join("Roaming"))
    } else if cfg!(target_os = "macos") {
        user_home().join("Library").join("Application Support")
    } else {
        user_home().join(".config")
    }
}

/// Electron 的 `appData`/本地根：Windows 上 3P 档案落在 `%LOCALAPPDATA%`，其余平台同根。
fn claude_desktop_local_root() -> PathBuf {
    if cfg!(windows) {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home().join("AppData").join("Local"))
    } else {
        claude_desktop_app_data_root()
    }
}

fn claude_desktop_first_party_dir() -> PathBuf {
    claude_desktop_app_data_root().join("Claude")
}

/// 3P 档案目录。导入本身就是「启用 3P」的动作，所以这里不判断当前是否已启用。
///
/// 路径必须与客户端 `Tu()` 一致：Windows 用 `%LOCALAPPDATA%\Claude-3p`（Electron 的
/// `userData` 在 Windows 上取自 LOCALAPPDATA）；`%APPDATA%\Claude-3p` 只是旧版迁移源，
/// 写在那里不会被读取。
fn claude_desktop_third_party_dir() -> PathBuf {
    if let Some(dir) = env::var_os("CLAUDE_USER_DATA_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return dir;
        }
    }
    if cfg!(windows) {
        return claude_desktop_local_root()
            .join(format!("Claude{CLAUDE_DESKTOP_THIRD_PARTY_SUFFIX}"));
    }
    claude_desktop_first_party_dir()
        .with_file_name(format!("Claude{CLAUDE_DESKTOP_THIRD_PARTY_SUFFIX}"))
}

/// Claude Desktop 当前真正使用的档案目录：3P 已启用时是 3P 目录，否则是 1P 默认目录。
fn claude_desktop_active_dir() -> PathBuf {
    let third_party = claude_desktop_third_party_dir();
    if env::var_os("CLAUDE_USER_DATA_DIR").map_or(false, |dir| !dir.is_empty()) {
        // 环境变量覆盖时，Electron 两种模式都用这个目录。
        return third_party;
    }
    if claude_desktop_third_party_enabled() {
        return third_party;
    }
    claude_desktop_first_party_dir()
}

/// 客户端启动时把「生效配置里带 `inference`/`bootstrap`/`selfHosted`」且
/// 「已持久化 deploymentMode 不为 `1p`」判定为 3P（`claude_desktop_third_party_dir`
/// 下的 `claude_desktop_config.json` 与 `configLibrary/`）。这里复刻同一条判据，
/// 以免 MCP 注册写到客户端不会读取的目录。
fn claude_desktop_third_party_enabled() -> bool {
    let env_override = env::var_os("CLAUDE_USER_DATA_DIR").map_or(false, |dir| !dir.is_empty());
    claude_desktop_third_party_enabled_in(&claude_desktop_third_party_dir(), env_override)
}

/// 判据本体。参数化目录与「是否被 `CLAUDE_USER_DATA_DIR` 覆盖」，便于单测直接构造档案，
/// 不必改动进程级环境变量（`CLAUDE_USER_DATA_DIR` 一旦存在，Electron 两种模式都用它，
/// 因此它本身就等价于「不是 1P」）。
fn claude_desktop_third_party_enabled_in(dir: &Path, env_override: bool) -> bool {
    let persisted = env_override
        || !claude_desktop_persisted_deployment_mode(dir).is_some_and(|mode| {
            mode.trim()
                .eq_ignore_ascii_case(CLAUDE_DESKTOP_FIRST_PARTY_MODE)
        });
    if !persisted {
        return false;
    }
    claude_desktop_applied_entry(dir)
        .ok()
        .flatten()
        .is_some_and(|entry| claude_desktop_entry_enables_third_party(&entry))
}

fn claude_desktop_persisted_deployment_mode(dir: &Path) -> Option<String> {
    read_json_object(&dir.join(CLAUDE_DESKTOP_CONFIG_FILE))
        .ok()
        .flatten()
        .and_then(|root| {
            root.get(CLAUDE_DESKTOP_DEPLOYMENT_MODE_KEY)
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

/// 3P 目录里当前生效的配置库条目（`_meta.json` 的 `appliedId` 指向的文件）。
fn claude_desktop_applied_entry(
    dir: &Path,
) -> Result<Option<serde_json::Map<String, Value>>, Box<dyn Error>> {
    let meta_path = dir
        .join(CLAUDE_DESKTOP_CONFIG_LIBRARY_DIR)
        .join(CLAUDE_DESKTOP_LIBRARY_META_FILE);
    let Some(meta) = read_json_object(&meta_path)? else {
        return Ok(None);
    };
    // `hybridPointer` 等价于一条只带 `bootstrapUrl` 的条目，同样会启用 3P。
    if meta
        .get("hybridPointer")
        .and_then(Value::as_str)
        .is_some_and(|url| !url.trim().is_empty())
    {
        let mut entry = serde_json::Map::new();
        entry.insert("bootstrapUrl".to_string(), json!("hybrid"));
        return Ok(Some(entry));
    }
    let Some(applied_id) = meta.get("appliedId").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !is_claude_config_library_id(applied_id) {
        return Ok(None);
    }
    read_json_object(
        &dir.join(CLAUDE_DESKTOP_CONFIG_LIBRARY_DIR)
            .join(format!("{applied_id}.json")),
    )
}

/// 客户端只认 `inference`（网关/厂商自带）、`bootstrapUrl`、`selfHosted` 三类开关。
fn claude_desktop_entry_enables_third_party(entry: &serde_json::Map<String, Value>) -> bool {
    ["inferenceProvider", "bootstrapUrl", "selfHosted"]
        .iter()
        .any(|key| entry.contains_key(*key))
}

fn claude_desktop_config_path() -> PathBuf {
    claude_desktop_third_party_dir().join(CLAUDE_DESKTOP_CONFIG_FILE)
}

fn claude_desktop_library_dir() -> PathBuf {
    claude_desktop_third_party_dir().join(CLAUDE_DESKTOP_CONFIG_LIBRARY_DIR)
}

fn claude_desktop_library_meta_path() -> PathBuf {
    claude_desktop_library_dir().join(CLAUDE_DESKTOP_LIBRARY_META_FILE)
}

/// Claude Desktop 应用配置（含 `mcpServers`）的真实路径，供 MCP 注册复用。
pub(crate) fn claude_desktop_app_config_path() -> PathBuf {
    claude_desktop_active_dir().join(CLAUDE_DESKTOP_CONFIG_FILE)
}

/// MCP 目标的探测目录：1P 与 3P 都要认，避免只装了其中一种档案时漏检。
pub(crate) fn claude_desktop_detect_dirs() -> Vec<PathBuf> {
    vec![
        claude_desktop_first_party_dir(),
        claude_desktop_third_party_dir(),
    ]
}

fn read_json_object(path: &Path) -> Result<Option<serde_json::Map<String, Value>>, Box<dyn Error>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if text.trim().is_empty() {
        return Ok(Some(serde_json::Map::new()));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| format!("{} 格式无效：{error}", path.display()))?;
    match value.as_object().cloned() {
        Some(object) => Ok(Some(object)),
        None => Err(format!("{} 顶层必须是 JSON 对象", path.display()).into()),
    }
}

/// Claude Desktop 配置库要求条目 id 是 uuid（`/^[a-f0-9-]{36}$/`）。
fn is_claude_config_library_id(value: &str) -> bool {
    value.len() == 36
        && value
            .chars()
            .all(|character| character.is_ascii_hexdigit() || character == '-')
}

fn new_claude_config_library_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut id = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        id.push_str(&format!("{byte:02x}"));
    }
    id
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
    ensure_anthropic_compatible(&credential, "Claude Code")?;
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
        ..Default::default()
    })
}

fn import_claude_desktop(
    options: &Options,
    expected_user_id: &str,
    service: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let credential =
        resolve_credential(options, expected_user_id, "claude-desktop-import", service)?;
    ensure_anthropic_compatible(&credential, "Claude Desktop")?;
    let models = available_models(&credential)?;
    let preferred = preferred_model(&credential)?;
    let base = anthropic_base_url(&credential.access.base_url)?;
    let user_data = claude_desktop_third_party_dir();
    let config_path = claude_desktop_config_path();
    let meta_path = claude_desktop_library_meta_path();
    let client_detected =
        config_path.is_file() || claude_desktop_first_party_dir().is_dir() || user_data.is_dir();

    // 先写条目文件，再让 `_meta.json` 引用它：任何一步失败都不会留下指向空条目的档案。
    let mut meta = claude_desktop_library_meta(&meta_path)?;
    let entry_id =
        claude_desktop_owned_entry_id(&meta).unwrap_or_else(new_claude_config_library_id);
    let entry_path = claude_desktop_library_dir().join(format!("{entry_id}.json"));
    let entry_backup = backup_and_write(
        &entry_path,
        claude_desktop_gateway_entry(&base, &credential, &models)?.as_bytes(),
    )?;

    // 保留用户已有条目，只替换/新增 HiMind 那一条，并让它成为当前生效档案。
    upsert_claude_desktop_entry(&mut meta, &entry_id);
    let meta_backup = backup_and_write(
        &meta_path,
        format!("{}\n", serde_json::to_string_pretty(&Value::Object(meta))?).as_bytes(),
    )?;

    let config_backup = write_claude_desktop_deployment_mode()?;
    Ok(AIProviderImportResult {
        ok: true,
        target: "claude-desktop".to_string(),
        status: "configured".to_string(),
        model_count: models.len(),
        model: preferred,
        config_path: config_path.to_string_lossy().to_string(),
        backup_path: config_backup
            .or(meta_backup)
            .or(entry_backup)
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
        client_detected,
        ..Default::default()
    })
}

/// 读取（或初始化）Claude Desktop 配置库的 `_meta.json`。
///
/// 结构由 Claude Desktop 自己校验：`appliedId` 必须是已存在的条目 id，`entries`
/// 里的每条都要有字符串 `id`/`name`。这里宁可报错也不写坏——写坏了 Claude Desktop
/// 会认为整个本地配置不可用，用户此前的档案也会打不开。
fn claude_desktop_library_meta(
    meta_path: &Path,
) -> Result<serde_json::Map<String, Value>, Box<dyn Error>> {
    let mut meta = read_json_object(meta_path)?.unwrap_or_else(serde_json::Map::new);
    match meta.get("entries") {
        None => {
            meta.insert("entries".to_string(), Value::Array(Vec::new()));
        }
        Some(Value::Array(entries)) => {
            let valid = entries.iter().all(|entry| {
                entry.as_object().is_some_and(|object| {
                    object.get("id").and_then(Value::as_str).is_some()
                        && object.get("name").and_then(Value::as_str).is_some()
                })
            });
            if !valid {
                return Err(format!(
                    "{} 的 entries 结构异常，已停止写入；请在 Claude Desktop 中确认该档案正常后重试",
                    meta_path.display()
                )
                .into());
            }
        }
        Some(_) => {
            return Err(format!(
                "{} 的 entries 不是数组，已停止写入；请在 Claude Desktop 中确认该档案正常后重试",
                meta_path.display()
            )
            .into())
        }
    }
    if !meta.get("appliedId").is_some_and(Value::is_string) {
        meta.insert("appliedId".to_string(), json!(""));
    }
    if !meta.get("isManaged").is_some_and(Value::is_boolean) {
        meta.insert("isManaged".to_string(), json!(false));
    }
    let platform = meta
        .get("platform")
        .and_then(Value::as_str)
        .map(str::to_string);
    if !platform.is_some_and(|value| matches!(value.as_str(), "win32" | "darwin" | "linux")) {
        meta.insert("platform".to_string(), json!(claude_desktop_platform()));
    }
    Ok(meta)
}

fn claude_desktop_platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

fn claude_desktop_owned_entry_id(meta: &serde_json::Map<String, Value>) -> Option<String> {
    meta.get("entries")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|entry| {
            let object = entry.as_object()?;
            if object.get("note").and_then(Value::as_str) != Some(CLAUDE_DESKTOP_ENTRY_NOTE) {
                return None;
            }
            let id = object.get("id").and_then(Value::as_str)?.trim();
            is_claude_config_library_id(id).then(|| id.to_string())
        })
}

fn upsert_claude_desktop_entry(meta: &mut serde_json::Map<String, Value>, entry_id: &str) {
    let mut entries = meta
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let ours = json!({
        "id": entry_id,
        "name": CLAUDE_DESKTOP_ENTRY_NAME,
        "provider": "gateway",
        "note": CLAUDE_DESKTOP_ENTRY_NOTE,
    });
    match entries
        .iter_mut()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(entry_id))
    {
        Some(slot) => *slot = ours,
        None => entries.push(ours),
    }
    meta.insert("entries".to_string(), Value::Array(entries));
    meta.insert("appliedId".to_string(), json!(entry_id));
}

/// 3P 条目的内容。模型列表交给 Claude Desktop 的自动发现（`GET /v1/models`）：
/// 条目里的 `inferenceModels` 只接受 Anthropic 命名的条目，网关侧的
/// `deepseek-*` / `glm-*` 之类会被丢弃，写进去等于声明了一堆用不了的模型。
///
/// `inferenceCustomHeaders` 带一个来源标记，网关据此只对 Claude Desktop 表面返回
/// Anthropic 形态的路由名（`claude-<档位>-himind-<序号>`，展示名仍是真实模型名）：
/// 客户端的选择器只保留「看起来像 Anthropic」的模型名，其它厂商名会被直接丢弃，
/// 表现为「能发现但选择器为空」。标记让其它客户端继续拿到规范模型名。
fn claude_desktop_gateway_entry(
    base_url: &str,
    credential: &AIClientCredential,
    _models: &[String],
) -> Result<String, Box<dyn Error>> {
    let entry = json!({
        "inferenceProvider": "gateway",
        "inferenceGatewayBaseUrl": base_url,
        "inferenceGatewayApiKey": credential.api_key,
        "inferenceGatewayAuthScheme": "bearer",
        "inferenceCredentialKind": "static",
        "inferenceCustomHeaders": { CLAUDE_DESKTOP_SURFACE_HEADER: CLAUDE_DESKTOP_SURFACE_VALUE },
    });
    Ok(format!("{}\n", serde_json::to_string_pretty(&entry)?))
}

/// 把 `deploymentMode` 标记为 3p，并顺手把 1P 档案里的 `mcpServers` 带过去。
///
/// 切换 3P 会让 Claude Desktop 换用 3P 档案目录，用户原有的 MCP 注册如果留在
/// `%APPDATA%\Claude` 就会静默失效，所以这里做一次「只补不覆盖」的搬运。
fn write_claude_desktop_deployment_mode() -> Result<Option<PathBuf>, Box<dyn Error>> {
    let config_path = claude_desktop_config_path();
    let mut root = read_json_object(&config_path)?.unwrap_or_else(serde_json::Map::new);
    root.insert(
        CLAUDE_DESKTOP_DEPLOYMENT_MODE_KEY.to_string(),
        json!(CLAUDE_DESKTOP_THIRD_PARTY_MODE),
    );
    let first_party_dir = claude_desktop_first_party_dir();
    if first_party_dir != claude_desktop_third_party_dir() {
        let first_party = read_json_object(&first_party_dir.join(CLAUDE_DESKTOP_CONFIG_FILE))?
            .and_then(|object| object.get("mcpServers").cloned());
        if let Some(mcp_servers) = first_party {
            let third_party = root
                .entry("mcpServers".to_string())
                .or_insert_with(|| json!({}));
            if let Some(target) = third_party.as_object_mut() {
                if let Some(source) = mcp_servers.as_object() {
                    for (name, value) in source {
                        target.entry(name.clone()).or_insert_with(|| value.clone());
                    }
                }
            }
        }
    }
    backup_and_write(
        &config_path,
        format!("{}\n", serde_json::to_string_pretty(&Value::Object(root))?).as_bytes(),
    )
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
        ..Default::default()
    }
}

fn claude_code_import_status() -> AIProviderImportStatus {
    claude_import_status(&claude_code_settings_path(), "claude-code", "Claude Code")
}

fn claude_desktop_import_status() -> AIProviderImportStatus {
    claude_desktop_status_in(
        &claude_desktop_third_party_dir(),
        &claude_desktop_first_party_dir(),
        claude_desktop_third_party_enabled(),
    )
}

/// Claude Desktop 的导入状态以 3P 配置库为准：条目里有网关地址与凭据即视为已导入，
/// 不再读 1P 时代的 `env.ANTHROPIC_*`（3P 档案下那些键根本不会被读取）。
fn claude_desktop_status_in(
    third_dir: &Path,
    first_dir: &Path,
    active: bool,
) -> AIProviderImportStatus {
    let config_path = third_dir.join(CLAUDE_DESKTOP_CONFIG_FILE);
    let client_detected = first_dir.is_dir() || third_dir.is_dir() || config_path.is_file();
    let library_dir = claude_desktop_library_dir_in(third_dir);
    let entry = read_json_object(&library_dir.join(CLAUDE_DESKTOP_LIBRARY_META_FILE))
        .ok()
        .flatten()
        .and_then(|meta| claude_desktop_owned_entry_id(&meta))
        .and_then(|id| {
            read_json_object(&library_dir.join(format!("{id}.json")))
                .ok()
                .flatten()
        });
    let imported = entry.as_ref().is_some_and(|entry| {
        claude_desktop_entry_text(entry, "inferenceGatewayBaseUrl").is_some()
            && claude_desktop_entry_text(entry, "inferenceGatewayApiKey").is_some()
    });
    let models = entry
        .as_ref()
        .map(claude_desktop_entry_models)
        .unwrap_or_default();
    AIProviderImportStatus {
        target: "claude-desktop".to_string(),
        state: if imported { "imported" } else { "not_imported" }.to_string(),
        client_detected,
        detail: if imported && !active {
            "已写入 HiMind 网关档案，但 Claude Desktop 尚未切换到第三方档案；\
             请完全退出（含托盘）后重新启动 Claude Desktop"
                .to_string()
        } else if imported {
            "已写入 HiMind 网关档案；模型由网关自动发现，改动需完全退出（含托盘）\
             后重新启动 Claude Desktop 才生效"
                .to_string()
        } else if client_detected {
            "已检测到 Claude Desktop，尚未导入 HiMind AI".to_string()
        } else {
            "未检测到 Claude Desktop 配置，请先安装并启动一次".to_string()
        },
        config_path: config_path.to_string_lossy().to_string(),
        models,
        synced_at: String::new(),
        service: String::new(),
        ..Default::default()
    }
}

fn claude_desktop_library_dir_in(third_dir: &Path) -> PathBuf {
    third_dir.join(CLAUDE_DESKTOP_CONFIG_LIBRARY_DIR)
}

fn claude_desktop_entry_text(entry: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// 条目里的 `inferenceModels` 只接受 Anthropic 命名的条目，因此 HiMind 条目通常不带它，
/// 模型交给网关的 `GET /v1/models` 自动发现；这里只做只读回显。
fn claude_desktop_entry_models(entry: &serde_json::Map<String, Value>) -> Vec<String> {
    entry
        .get("inferenceModels")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(name) => Some(name.trim().to_string()),
                    Value::Object(object) => object
                        .get("name")
                        .and_then(Value::as_str)
                        .map(|name| name.trim().to_string()),
                    _ => None,
                })
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default()
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

/// Claude Code 的导入会接管 env 下的 4 个 ANTHROPIC_* 键。用户原本就配过这些键时，
/// 取消导入必须还原原值，而不是直接删除。
fn claude_owned_snapshot(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    Some(json!({ "settings": text }))
}

/// Claude Desktop 的导入只改配置库里的 HiMind 条目与 `_meta.json`，快照留给取消时
/// 原样还原；`deploymentMode` 与用户的其它条目、`mcpServers` 都不在快照范围内，
/// 因为它们本就不该被导入动作改写。
fn claude_desktop_owned_snapshot() -> Option<Value> {
    let meta_path = claude_desktop_library_meta_path();
    let meta_text = fs::read_to_string(&meta_path).ok()?;
    let entry_id = read_json_object(&meta_path)
        .ok()
        .flatten()
        .and_then(|meta| claude_desktop_owned_entry_id(&meta));
    let entry_text = entry_id.as_ref().and_then(|id| {
        fs::read_to_string(claude_desktop_library_dir().join(format!("{id}.json"))).ok()
    });
    Some(json!({
        "meta": meta_text,
        "entry_id": entry_id,
        "entry": entry_text,
    }))
}

fn cancel_claude_code(
    restore: Option<&Value>,
) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    cancel_claude_settings(
        &claude_code_settings_path(),
        "claude-code",
        "Claude Code",
        restore,
    )
}

/// 取消 Claude Desktop 导入：只做减法，移除 HiMind 自己的配置库条目并修正 `appliedId`，
/// 其余条目、`deploymentMode` 与 `mcpServers` 保持用户原样。
fn cancel_claude_desktop(
    restore: Option<&Value>,
) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    cancel_claude_desktop_in(
        &claude_desktop_third_party_dir(),
        &claude_desktop_first_party_dir(),
        restore,
    )
}

fn cancel_claude_desktop_in(
    third_dir: &Path,
    first_dir: &Path,
    restore: Option<&Value>,
) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let meta_path = third_dir
        .join(CLAUDE_DESKTOP_CONFIG_LIBRARY_DIR)
        .join(CLAUDE_DESKTOP_LIBRARY_META_FILE);
    let library_dir = claude_desktop_library_dir_in(third_dir);
    let config_path = third_dir.join(CLAUDE_DESKTOP_CONFIG_FILE);
    let client_detected = config_path.is_file() || third_dir.is_dir() || first_dir.is_dir();
    let not_imported = |client_detected: bool| AIProviderImportCancelResult {
        ok: true,
        target: "claude-desktop".to_string(),
        status: "not_imported".to_string(),
        changed: false,
        client_detected,
        detail: "Claude Desktop 当前没有 HiMind 导入记录".to_string(),
        backup_path: String::new(),
    };

    let Some(mut meta) = read_json_object(&meta_path)? else {
        return Ok(not_imported(client_detected));
    };
    let owned_id = claude_desktop_owned_entry_id(&meta);
    let snapshot_meta = restore
        .and_then(|value| value.get("meta"))
        .and_then(Value::as_str);
    let mut backup: Option<PathBuf> = None;
    let mut changed = false;

    if let Some(original) = snapshot_meta {
        // 有快照：还原 `_meta.json`，并还原（或删除）快照记录的那条条目文件。
        let restored_id = restore
            .and_then(|value| value.get("entry_id"))
            .and_then(Value::as_str)
            .filter(|id| is_claude_config_library_id(id));
        if let Some(id) = restored_id {
            let entry_path = library_dir.join(format!("{id}.json"));
            match restore
                .and_then(|value| value.get("entry"))
                .and_then(Value::as_str)
            {
                Some(text) => {
                    backup = backup_and_write(&entry_path, text.as_bytes())?;
                    changed = true;
                }
                None if entry_path.is_file() => {
                    fs::remove_file(&entry_path)?;
                    changed = true;
                }
                None => {}
            }
        }
        if fs::read_to_string(&meta_path).ok().as_deref() != Some(original) {
            backup = backup_and_write(&meta_path, original.as_bytes())?;
            changed = true;
        }
    } else if let Some(id) = owned_id {
        let entries = meta
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let remaining: Vec<Value> = entries
            .iter()
            .filter(|entry| {
                entry.get("note").and_then(Value::as_str) != Some(CLAUDE_DESKTOP_ENTRY_NOTE)
            })
            .cloned()
            .collect();
        let applied_points_to_us =
            meta.get("appliedId").and_then(Value::as_str) == Some(id.as_str());
        if remaining.len() != entries.len() || applied_points_to_us {
            if applied_points_to_us {
                let next = remaining
                    .first()
                    .and_then(|entry| entry.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                meta.insert("appliedId".to_string(), json!(next));
            }
            meta.insert("entries".to_string(), Value::Array(remaining));
            backup = backup_and_write(
                &meta_path,
                format!("{}\n", serde_json::to_string_pretty(&Value::Object(meta))?).as_bytes(),
            )?;
            changed = true;
        }
        let entry_path = library_dir.join(format!("{id}.json"));
        if entry_path.is_file() {
            fs::remove_file(&entry_path)?;
            changed = true;
        }
    } else {
        return Ok(not_imported(client_detected));
    }

    Ok(AIProviderImportCancelResult {
        ok: true,
        target: "claude-desktop".to_string(),
        status: if changed { "cancelled" } else { "not_imported" }.to_string(),
        changed,
        client_detected,
        detail: if changed {
            "已从 Claude Desktop 移除 HiMind 网关档案；完全退出（含托盘）后重新启动生效".to_string()
        } else {
            "Claude Desktop 当前没有 HiMind 导入记录".to_string()
        },
        backup_path: backup
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
    })
}

fn cancel_claude_settings(
    path: &Path,
    target: &str,
    client_name: &str,
    restore: Option<&Value>,
) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let client_detected = path.is_file();
    let original = if path.is_file() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    let previous = restore
        .and_then(|value| value.get("settings"))
        .and_then(Value::as_str);
    let (updated, removed) = strip_claude_himind(&original, client_name, previous)?;
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

// 移除 HiMind 写入的 ANTHROPIC_* 键：有快照时还原用户原值，没有快照（旧簿记）
// 时按原行为删除；用户其他配置原样保留。
fn strip_claude_himind(
    original: &str,
    client_name: &str,
    previous: Option<&str>,
) -> Result<(String, bool), Box<dyn Error>> {
    if original.trim().is_empty() {
        return Ok((String::new(), false));
    }
    let mut root = serde_json::from_str::<Value>(original).map_err(|_| {
        format!("{client_name} settings.json 格式无效，已停止取消导入且未覆盖原文件")
    })?;
    let previous_root = previous
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let object = root
        .as_object_mut()
        .ok_or(format!("{client_name} settings.json 顶层必须是 JSON 对象"))?;
    let mut changed = false;
    if let Some(env) = object.get_mut("env").and_then(Value::as_object_mut) {
        let previous_env = previous_root
            .as_ref()
            .and_then(|root| root.get("env"))
            .and_then(Value::as_object);
        for key in claude_env_keys() {
            match previous_env.and_then(|env| env.get(key)) {
                Some(original_value) => {
                    if env.get(key) == Some(original_value) {
                        continue;
                    }
                    env.insert(key.to_string(), original_value.clone());
                    changed = true;
                }
                None => {
                    if env.remove(key).is_some() {
                        changed = true;
                    }
                }
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

// ---- OpenCode ----
// OpenCode 全局配置为 ~/.config/opencode/opencode.json（OPENCODE_CONFIG 可指定自定义
// 配置文件）。自定义供应商写在 provider.<id>：npm 指定 AI SDK 适配包、options 携带
// apiKey/baseURL、models 声明模型目录。配置分层合并（全局 + 项目），因此这里只接管
// provider.himind，mcp、permission、theme 等用户配置原样保留。OpenCode 官方支持
// JSON 与 JSONC，读取时容忍注释与尾随逗号，写回为等价的标准 JSON（写入前必有备份）。
fn opencode_config_path() -> PathBuf {
    if let Some(path) = env::var_os("OPENCODE_CONFIG") {
        return PathBuf::from(path);
    }
    opencode_config_dir().join("opencode.json")
}

fn opencode_config_dir() -> PathBuf {
    user_home().join(".config").join("opencode")
}

fn opencode_client_detected(path: &Path) -> bool {
    path.is_file() || opencode_config_dir().is_dir() || env::var_os("OPENCODE_CONFIG").is_some()
}

fn opencode_npm_for_protocol(protocol: &str) -> Result<&'static str, Box<dyn Error>> {
    match protocol.trim() {
        "openai-chat" => Ok(OPENCODE_NPM_OPENAI_COMPATIBLE),
        "anthropic" => Ok(OPENCODE_NPM_ANTHROPIC),
        // 与 resolve_credential 的默认协议保持一致：空值与未知值按 Responses 处理。
        _ => Ok(OPENCODE_NPM_OPENAI_RESPONSES),
    }
}

/// AI SDK 的 Anthropic Provider 默认 `baseURL` 是 `https://api.anthropic.com/v1`，
/// 请求路径为 `{baseURL}/messages`；而 Claude Code / DSH 用的是 Anthropic SDK 约定
/// （`baseURL` 后自动追加 `/v1/messages`）。同一份服务地址写入 OpenCode 前必须补上
/// `/v1`，否则请求会落到 `/messages`。
fn opencode_anthropic_base_url(value: &str) -> Result<String, Box<dyn Error>> {
    let base = normalized_base_url(value)?;
    let base = base
        .strip_suffix("/messages")
        .map(str::to_string)
        .unwrap_or(base);
    let root = crate::store::ai_services::anthropic_api_root(&base);
    Ok(format!("{root}/v1"))
}

fn import_opencode(
    options: &Options,
    expected_user_id: &str,
    service: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let path = opencode_config_path();
    let client_detected = opencode_client_detected(&path);
    let credential = resolve_credential(options, expected_user_id, "opencode-import", service)?;
    let models = available_models(&credential)?;
    let preferred = preferred_model(&credential)?;
    let original = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let updated = build_opencode_config(&original, &credential, &models)?;
    let backup = backup_and_write(&path, updated.as_bytes())?;
    Ok(AIProviderImportResult {
        ok: true,
        target: "opencode".to_string(),
        status: "configured".to_string(),
        model_count: models.len(),
        model: preferred,
        config_path: path.to_string_lossy().to_string(),
        backup_path: backup
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
        client_detected,
        ..Default::default()
    })
}

fn build_opencode_config(
    original: &str,
    credential: &AIClientCredential,
    models: &[String],
) -> Result<String, Box<dyn Error>> {
    let protocol = credential.access.protocol.trim();
    let npm = opencode_npm_for_protocol(protocol)?;
    let endpoint = if protocol == "anthropic" {
        opencode_anthropic_base_url(&credential.access.base_url)?
    } else {
        normalized_base_url(&credential.access.base_url)?
    };
    let mut root = if original.trim().is_empty() {
        serde_json::Map::new()
    } else {
        serde_json::from_str::<Value>(&crate::app::mcp_targets::strip_jsonc_comments(original))
            .map_err(|error| format!("OpenCode opencode.json 格式无效：{error}"))?
            .as_object()
            .cloned()
            .ok_or("OpenCode opencode.json 顶层必须是 JSON 对象")?
    };
    let mut providers = match root.remove("provider") {
        Some(value) => value
            .as_object()
            .cloned()
            .ok_or("OpenCode opencode.json 的 provider 必须是对象")?,
        None => serde_json::Map::new(),
    };
    let catalog = models
        .iter()
        .map(|model| (model.clone(), json!({ "name": model })))
        .collect::<serde_json::Map<String, Value>>();
    providers.insert(
        OPENCODE_PROVIDER_ID.to_string(),
        json!({
            "name": MANAGED_VENDOR,
            "npm": npm,
            "options": {
                "baseURL": endpoint,
                "apiKey": credential.api_key,
            },
            "models": Value::Object(catalog),
        }),
    );
    root.insert("provider".to_string(), Value::Object(providers));
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(root))?
    ))
}

fn read_opencode_config(path: &Path) -> Option<Value> {
    let content = fs::read_to_string(path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(&crate::app::mcp_targets::strip_jsonc_comments(&content)).ok()
}

fn opencode_himind_provider_present(path: &Path) -> bool {
    read_opencode_config(path).is_some_and(|root| {
        root.get("provider")
            .and_then(|value| value.get(OPENCODE_PROVIDER_ID))
            .is_some()
    })
}

fn read_opencode_himind_models(path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let root = read_opencode_config(path).ok_or("OpenCode opencode.json 无法解析")?;
    Ok(root
        .get("provider")
        .and_then(|value| value.get(OPENCODE_PROVIDER_ID))
        .and_then(|value| value.get("models"))
        .and_then(Value::as_object)
        .map(|models| models.keys().cloned().collect())
        .unwrap_or_default())
}

fn opencode_import_status() -> AIProviderImportStatus {
    let path = opencode_config_path();
    let client_detected = opencode_client_detected(&path);
    let models = read_opencode_himind_models(&path).unwrap_or_default();
    let imported = !models.is_empty() || opencode_himind_provider_present(&path);
    AIProviderImportStatus {
        target: "opencode".to_string(),
        state: if imported { "imported" } else { "not_imported" }.to_string(),
        client_detected,
        detail: if imported && models.is_empty() {
            "检测到 OpenCode 已配置 HiMind 供应商，但缺少模型条目，请重新导入".to_string()
        } else if imported {
            format!(
                "已写入 {} 个 HiMind 模型；重启 OpenCode 后可在 /models 选择",
                models.len()
            )
        } else if client_detected {
            "已检测到 OpenCode，尚未导入 HiMind AI".to_string()
        } else {
            "未检测到 OpenCode 配置目录，请先运行一次 opencode".to_string()
        },
        config_path: path.to_string_lossy().to_string(),
        models,
        synced_at: String::new(),
        service: String::new(),
        ..Default::default()
    }
}

fn cancel_opencode() -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let path = opencode_config_path();
    let client_detected = opencode_client_detected(&path);
    let original = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let (updated, removed) = strip_opencode_himind(&original)?;
    let backup = if removed {
        backup_and_write(&path, updated.as_bytes())?
    } else {
        None
    };
    Ok(AIProviderImportCancelResult {
        ok: true,
        target: "opencode".to_string(),
        status: if removed { "cancelled" } else { "not_imported" }.to_string(),
        changed: removed,
        client_detected,
        detail: if removed {
            "已从 OpenCode 移除 HiMind 供应商配置".to_string()
        } else {
            "OpenCode 当前没有 HiMind 导入记录".to_string()
        },
        backup_path: backup
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
    })
}

// 只移除 HiMind 明确写入的 provider.himind，其他供应商与用户配置原样保留。
fn strip_opencode_himind(original: &str) -> Result<(String, bool), Box<dyn Error>> {
    if original.trim().is_empty() {
        return Ok((String::new(), false));
    }
    let mut root =
        serde_json::from_str::<Value>(&crate::app::mcp_targets::strip_jsonc_comments(original))
            .map_err(|_| "OpenCode opencode.json 格式无效，已停止取消导入且未覆盖原文件")?;
    let object = root
        .as_object_mut()
        .ok_or("OpenCode opencode.json 顶层必须是 JSON 对象")?;
    let mut changed = false;
    if let Some(providers) = object.get_mut("provider").and_then(Value::as_object_mut) {
        if providers.remove(OPENCODE_PROVIDER_ID).is_some() {
            changed = true;
        }
        if providers.is_empty() {
            object.remove("provider");
        }
    }
    Ok((
        format!("{}\n", serde_json::to_string_pretty(&root)?),
        changed,
    ))
}

// ---- 声明式适配表：JSON/YAML 文本配置文件 ----
//
// 与 MCP 侧的 mcp_targets::json_target_definitions() 同构：只要客户端把
// 「端点 + 令牌 + 模型列表」放在自己的文本配置里，接入就只是数据行，
// 检测、保留式合并、备份、状态与卸载复用同一套实现。
//
// 边界（不在本表内，继续保留手写适配器）：
// - 配置不是 JSON/YAML 文本（Codex 的 TOML、CC Switch 的 SQLite）；
// - 密钥不进配置文件，只能走系统钥匙串或环境变量：Goose 的 config.yaml
//   明确忽略 provider key（官方文档：a key placed there is ignored），声明式
//   provider 也只有 api_key_env / auth.command 两种取钥方式；Zed 的
//   language_models 里没有密钥字段；
// - 客户端把 provider / model / 密钥存在自己的状态快照里，且优先级高于配置文件
//   （Cline 的 VS Code 扩展读 globalState.json 与 secrets.json，配置文件只在
//   CLI 与首次启动时被读）。
const CONTINUE_GLOBAL_DIR_ENV: &str = "CONTINUE_GLOBAL_DIR";
const CONTINUE_CONFIG_OVERRIDE_ENV: &str = "HIMIND_CONTINUE_CONFIG";
const CONTINUE_HIMIND_PREFIX: &str = "himind/";
const AIDER_CONFIG_OVERRIDE_ENV: &str = "HIMIND_AIDER_CONFIG";
const AIDER_HIMIND_PREFIX: &str = "himind/";
const AIDER_OPENAI_BASE_KEY: &str = "openai-api-base";
const AIDER_OPENAI_KEY_KEY: &str = "openai-api-key";
const AIDER_MODEL_KEY: &str = "model";
const AIDER_ALIAS_KEY: &str = "alias";
const CRUSH_CONFIG_OVERRIDE_ENV: &str = "HIMIND_CRUSH_CONFIG";
const QODER_CONFIG_OVERRIDE_ENV: &str = "HIMIND_QODER_CONFIG";
const QODER_CN_CONFIG_OVERRIDE_ENV: &str = "HIMIND_QODERCN_CONFIG";
const ZCODE_CONFIG_OVERRIDE_ENV: &str = "HIMIND_ZCODE_CONFIG";
const ZCODE_PROVIDER_CONFIG_OVERRIDE_ENV: &str = "HIMIND_ZCODE_PROVIDER_CONFIG";
/// 声明式客户端统一以 `himind` 作为 provider id / 目录名，卸载按它判定归属。
const DECLARATIVE_PROVIDER_ID: &str = "himind";
/// Qoder 选择自定义模型用 `<provider>/<model>` 形式，与 Continue / Aider 共用前缀。
const DECLARATIVE_MODEL_PREFIX: &str = "himind/";
/// Crush 的模型条目要填上下文窗口，HiMind 凭据只下发模型 id，没有元数据可查；
/// 缺省值取 200000，与 magpie 对未标注模型的兜底一致。窗口偏大只会推迟客户端
/// 的自动压缩，偏小会提前截断上下文，因此宁可取客户端常见上限。
const DECLARATIVE_MODEL_CONTEXT_WINDOW: u64 = 200_000;
/// Crush 的 `default_max_tokens` 是每次回复的默认上限，magpie 固定写 16384。
const CRUSH_DEFAULT_MAX_TOKENS: u64 = 16_384;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderConfigFormat {
    Json,
    Yaml,
}

/// 一个目标要写的配置文件。
///
/// 列表顺序即写入顺序，**第 0 项是主配置**：状态、模型列表与卸载判定只认它，
/// 其余文件随主配置同步写入与清理（例如 ZCode 的 provider 规则文件）。
#[derive(Clone)]
struct ProviderConfigFile {
    path: PathBuf,
    layout: ProviderConfigLayout,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderConfigLayout {
    /// Continue：models[] 数组，条目为 {name, provider, model, apiBase, apiKey, roles}。
    ContinueModels,
    /// Aider：顶层 openai-api-base / openai-api-key / model 标量，加 alias[] 列表。
    AiderConf,
    /// Crush：providers.<id> 的 provider 对象，加 models.large / models.small 槽位。
    CrushConfig,
    /// Qoder：providers.<id> 的 provider 对象，加 model.name 的 `<id>/<model>` 选择。
    QoderSettings,
    /// ZCode 主配置：provider.<id> 的 provider 对象（kind 为 anthropic）。
    ZCodeConfig,
    /// ZCode 规则文件：providerConfigRules / modelConfigRules 里的按 id 归组规则。
    ZCodeProviderRules,
}

/// 客户端配置面能吃下的线格式。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderProtocolSupport {
    /// 只有 chat / responses 两种线格式，导入 Anthropic 服务前必须拦下。
    OpenAiOnly,
    /// 只有 Anthropic Messages（ZCode 的 provider kind 固定为 anthropic）。
    AnthropicOnly,
    /// 两种都吃得下（Continue 按协议切 provider）。
    Both,
}

struct AiProviderTargetDefinition {
    id: &'static str,
    display_name: &'static str,
    /// 该目标要写的全部配置文件；第 0 项是主配置，不存在时返回将要创建的路径。
    config_files: fn() -> Vec<ProviderConfigFile>,
    /// 客户端自身的安装/初始化探测，用于 client_detected。
    detected: fn() -> bool,
    /// 该客户端配置面支持的线格式。
    protocol_support: ProviderProtocolSupport,
    not_detected_hint: &'static str,
    import_summary: &'static str,
    remove_summary: &'static str,
}

static CONTINUE_TARGET: AiProviderTargetDefinition = AiProviderTargetDefinition {
    id: "continue",
    display_name: "Continue",
    config_files: continue_config_files,
    detected: continue_client_detected,
    protocol_support: ProviderProtocolSupport::Both,
    not_detected_hint: "未检测到 Continue 配置目录，请先安装 Continue 扩展并运行一次",
    import_summary: "写入 Continue config 的 models 中 himind/* 模型",
    remove_summary: "移除 Continue config 中的 himind/* 模型",
};

static AIDER_TARGET: AiProviderTargetDefinition = AiProviderTargetDefinition {
    id: "aider",
    display_name: "Aider",
    config_files: aider_config_files,
    detected: aider_client_detected,
    protocol_support: ProviderProtocolSupport::OpenAiOnly,
    // Aider 的 Anthropic 端点只能走环境变量，配置文件里没有对应的基址键。
    not_detected_hint: "未检测到 Aider，请先安装 aider 并运行一次",
    import_summary: "写入 Aider .aider.conf.yml 的端点、密钥与 himind/* 别名",
    remove_summary: "移除 Aider .aider.conf.yml 的端点、密钥与 himind/* 别名",
};

static CRUSH_TARGET: AiProviderTargetDefinition = AiProviderTargetDefinition {
    id: "crush",
    display_name: "Crush",
    config_files: crush_config_files,
    detected: crush_client_detected,
    protocol_support: ProviderProtocolSupport::OpenAiOnly,
    not_detected_hint: "未检测到 Crush 配置目录，请先安装 Crush 并运行一次",
    import_summary: "写入 Crush crush.json 的 providers.himind 与 large/small 模型槽位",
    remove_summary: "移除 Crush crush.json 的 providers.himind 与 large/small 槽位",
};

static QODER_TARGET: AiProviderTargetDefinition = AiProviderTargetDefinition {
    id: "qoder",
    display_name: "Qoder",
    config_files: qoder_config_files,
    detected: qoder_client_detected,
    protocol_support: ProviderProtocolSupport::OpenAiOnly,
    not_detected_hint: "未检测到 Qoder 配置目录，请先安装 Qoder CLI 并运行一次",
    import_summary: "写入 Qoder settings.json 的 providers.himind 与默认模型",
    remove_summary: "移除 Qoder settings.json 的 providers.himind 与 himind/* 默认模型",
};

static QODER_CN_TARGET: AiProviderTargetDefinition = AiProviderTargetDefinition {
    id: "qoder-cn",
    display_name: "Qoder CN",
    config_files: qoder_cn_config_files,
    detected: qoder_cn_client_detected,
    protocol_support: ProviderProtocolSupport::OpenAiOnly,
    not_detected_hint: "未检测到 Qoder CN 配置目录，请先安装 Qoder CN CLI 并运行一次",
    import_summary: "写入 Qoder CN settings.json 的 providers.himind 与默认模型",
    remove_summary: "移除 Qoder CN settings.json 的 providers.himind 与 himind/* 默认模型",
};

static ZCODE_TARGET: AiProviderTargetDefinition = AiProviderTargetDefinition {
    id: "zcode",
    display_name: "ZCode",
    config_files: zcode_config_files,
    detected: zcode_client_detected,
    // ZCode 自定义 provider 的 kind 只有 anthropic 一种，端点按 /v1/messages 请求。
    protocol_support: ProviderProtocolSupport::AnthropicOnly,
    not_detected_hint: "未检测到 ZCode 配置目录，请先安装 ZCode 并运行一次",
    import_summary: "写入 ZCode config.json 与 provider_config.json 的 himind provider",
    remove_summary: "移除 ZCode config.json 与 provider_config.json 中的 himind provider",
};

static PROVIDER_TARGETS: &[&AiProviderTargetDefinition] = &[
    &CONTINUE_TARGET,
    &AIDER_TARGET,
    &CRUSH_TARGET,
    &QODER_TARGET,
    &QODER_CN_TARGET,
    &ZCODE_TARGET,
];

static CONTINUE_ADAPTER: ProviderConfigAdapter = ProviderConfigAdapter(&CONTINUE_TARGET);
static AIDER_ADAPTER: ProviderConfigAdapter = ProviderConfigAdapter(&AIDER_TARGET);
static CRUSH_ADAPTER: ProviderConfigAdapter = ProviderConfigAdapter(&CRUSH_TARGET);
static QODER_ADAPTER: ProviderConfigAdapter = ProviderConfigAdapter(&QODER_TARGET);
static QODER_CN_ADAPTER: ProviderConfigAdapter = ProviderConfigAdapter(&QODER_CN_TARGET);
static ZCODE_ADAPTER: ProviderConfigAdapter = ProviderConfigAdapter(&ZCODE_TARGET);

/// 声明式适配表的适配器：定义即行为，不承载客户端专有逻辑。
struct ProviderConfigAdapter(&'static AiProviderTargetDefinition);

fn declarative_provider_adapters() -> Vec<&'static dyn AIClientAdapter> {
    vec![
        &CONTINUE_ADAPTER,
        &AIDER_ADAPTER,
        &CRUSH_ADAPTER,
        &QODER_ADAPTER,
        &QODER_CN_ADAPTER,
        &ZCODE_ADAPTER,
    ]
}

fn provider_target_definition(id: &str) -> Option<&'static AiProviderTargetDefinition> {
    PROVIDER_TARGETS
        .iter()
        .copied()
        .find(|definition| definition.id == id.trim())
}

// Continue 1.0 以 ~/.continue/config.yaml 为主配置；config.json 仍在读取范围内，
// 只有 config.yaml 不存在时才会回落到它，因此两份都在时写 config.yaml 会顶掉旧配置。
fn continue_config_dir() -> PathBuf {
    if let Some(dir) = env::var_os(CONTINUE_GLOBAL_DIR_ENV) {
        return PathBuf::from(dir);
    }
    user_home().join(".continue")
}

fn continue_config_path() -> PathBuf {
    if let Some(path) = env::var_os(CONTINUE_CONFIG_OVERRIDE_ENV) {
        return PathBuf::from(path);
    }
    let dir = continue_config_dir();
    let yaml = dir.join("config.yaml");
    if yaml.is_file() {
        return yaml;
    }
    let json = dir.join("config.json");
    if json.is_file() {
        return json;
    }
    yaml
}

fn continue_client_detected() -> bool {
    continue_config_path().is_file()
        || continue_config_dir().is_dir()
        || env::var_os(CONTINUE_CONFIG_OVERRIDE_ENV).is_some()
}

fn continue_config_files() -> Vec<ProviderConfigFile> {
    vec![ProviderConfigFile {
        path: continue_config_path(),
        layout: ProviderConfigLayout::ContinueModels,
    }]
}

fn aider_config_path() -> PathBuf {
    if let Some(path) = env::var_os(AIDER_CONFIG_OVERRIDE_ENV) {
        return PathBuf::from(path);
    }
    user_home().join(".aider.conf.yml")
}

fn aider_config_files() -> Vec<ProviderConfigFile> {
    vec![ProviderConfigFile {
        path: aider_config_path(),
        layout: ProviderConfigLayout::AiderConf,
    }]
}

// Aider 不会自动生成配置文件，只装了 CLI、还没导入过的机器上只能靠 PATH 判断。
fn aider_client_detected() -> bool {
    aider_config_path().is_file() || aider_executable_on_path()
}

fn aider_executable_on_path() -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|dir| {
        ["aider", "aider.exe", "aider.cmd", "aider.bat"]
            .iter()
            .any(|name| dir.join(name).is_file())
    })
}

// ---- Crush ----
// Charm 的 Crush 在 Windows 用 %LOCALAPPDATA%\crush\crush.json，其它平台是
// $XDG_CONFIG_HOME/crush/crush.json（缺省 ~/.config）。provider 是 OpenCode
// 系的形状，多出 models.large / models.small 两个用途槽位。
fn crush_config_path() -> PathBuf {
    if let Some(path) = env::var_os(CRUSH_CONFIG_OVERRIDE_ENV) {
        return PathBuf::from(path);
    }
    if cfg!(windows) {
        if let Some(app_data) = env::var_os("LOCALAPPDATA") {
            return PathBuf::from(app_data).join("crush").join("crush.json");
        }
    }
    user_home().join(".config").join("crush").join("crush.json")
}

fn crush_config_files() -> Vec<ProviderConfigFile> {
    vec![ProviderConfigFile {
        path: crush_config_path(),
        layout: ProviderConfigLayout::CrushConfig,
    }]
}

fn crush_client_detected() -> bool {
    let path = crush_config_path();
    path.is_file() || path.parent().is_some_and(|dir| dir.is_dir())
}

// ---- Qoder / Qoder CN ----
// Qoder CLI 的 settings.json 在 $QODER_CONFIG_DIR（缺省 ~/.qoder）；Qoder CN 是
// 同一份 CLI 的中国站点版本，目录 $QODERCN_CONFIG_DIR（缺省 ~/.qoder-cn）。
fn qoder_settings_dir(env_name: &str, home_dir: &str) -> PathBuf {
    if let Some(dir) = env::var_os(env_name) {
        return PathBuf::from(dir);
    }
    user_home().join(home_dir)
}

fn qoder_config_path() -> PathBuf {
    if let Some(path) = env::var_os(QODER_CONFIG_OVERRIDE_ENV) {
        return PathBuf::from(path);
    }
    qoder_settings_dir("QODER_CONFIG_DIR", ".qoder").join("settings.json")
}

fn qoder_cn_config_path() -> PathBuf {
    if let Some(path) = env::var_os(QODER_CN_CONFIG_OVERRIDE_ENV) {
        return PathBuf::from(path);
    }
    qoder_settings_dir("QODERCN_CONFIG_DIR", ".qoder-cn").join("settings.json")
}

fn qoder_config_files() -> Vec<ProviderConfigFile> {
    vec![ProviderConfigFile {
        path: qoder_config_path(),
        layout: ProviderConfigLayout::QoderSettings,
    }]
}

fn qoder_cn_config_files() -> Vec<ProviderConfigFile> {
    vec![ProviderConfigFile {
        path: qoder_cn_config_path(),
        layout: ProviderConfigLayout::QoderSettings,
    }]
}

fn qoder_client_detected() -> bool {
    qoder_config_path().is_file() || qoder_settings_dir("QODER_CONFIG_DIR", ".qoder").is_dir()
}

fn qoder_cn_client_detected() -> bool {
    qoder_cn_config_path().is_file()
        || qoder_settings_dir("QODERCN_CONFIG_DIR", ".qoder-cn").is_dir()
}

// ---- ZCode ----
// ZCode 3.14 起 provider 的事实源是 ~/.zcode/v2/provider_config.json，config.json
// 只在启动时被读一次做导入，因此两份都写：旧版 ZCode 读 config.json，新版读规则。
fn zcode_dir() -> PathBuf {
    user_home().join(".zcode").join("v2")
}

fn zcode_config_path() -> PathBuf {
    if let Some(path) = env::var_os(ZCODE_CONFIG_OVERRIDE_ENV) {
        return PathBuf::from(path);
    }
    zcode_dir().join("config.json")
}

fn zcode_provider_config_path() -> PathBuf {
    if let Some(path) = env::var_os(ZCODE_PROVIDER_CONFIG_OVERRIDE_ENV) {
        return PathBuf::from(path);
    }
    // 两份配置同目录，主配置被重定位时规则文件跟着走，避免写进真实用户目录。
    if let Some(main) = env::var_os(ZCODE_CONFIG_OVERRIDE_ENV) {
        return PathBuf::from(main).with_file_name("provider_config.json");
    }
    zcode_dir().join("provider_config.json")
}

fn zcode_config_files() -> Vec<ProviderConfigFile> {
    vec![
        ProviderConfigFile {
            path: zcode_config_path(),
            layout: ProviderConfigLayout::ZCodeConfig,
        },
        ProviderConfigFile {
            path: zcode_provider_config_path(),
            layout: ProviderConfigLayout::ZCodeProviderRules,
        },
    ]
}

fn zcode_client_detected() -> bool {
    zcode_config_path().is_file() || zcode_dir().is_dir()
}

fn provider_config_file_primary(definition: &AiProviderTargetDefinition) -> ProviderConfigFile {
    let mut files = (definition.config_files)();
    files.remove(0)
}

/// 目标要写的文件路径列表；will_write / will_backup 与写入编排共用一份来源。
fn provider_config_file_paths(definition: &AiProviderTargetDefinition) -> Vec<PathBuf> {
    (definition.config_files)()
        .into_iter()
        .map(|file| file.path)
        .collect()
}

fn provider_config_format(path: &Path) -> ProviderConfigFormat {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("json") => ProviderConfigFormat::Json,
        // Continue 与 Aider 主配置都是 YAML，按扩展名兜底即可。
        _ => ProviderConfigFormat::Yaml,
    }
}

fn parse_provider_config(
    original: &str,
    format: ProviderConfigFormat,
    label: &str,
) -> Result<Option<Value>, Box<dyn Error>> {
    if original.trim().is_empty() {
        return Ok(None);
    }
    let value = match format {
        ProviderConfigFormat::Json => {
            serde_json::from_str::<Value>(&crate::app::mcp_targets::strip_jsonc_comments(original))
                .map_err(|error| format!("{label} 格式无效：{error}"))?
        }
        ProviderConfigFormat::Yaml => serde_yaml::from_str::<Value>(original)
            .map_err(|error| format!("{label} 格式无效：{error}"))?,
    };
    Ok(Some(value))
}

fn provider_config_object(
    original: &str,
    format: ProviderConfigFormat,
    label: &str,
) -> Result<serde_json::Map<String, Value>, Box<dyn Error>> {
    match parse_provider_config(original, format, label)? {
        None => Ok(serde_json::Map::new()),
        Some(Value::Object(map)) => Ok(map),
        Some(_) => Err(format!("{label} 顶层必须是键值映射").into()),
    }
}

/// 取（必要时建）`key` 下的子对象；客户端配置里这些段必须是对象，
/// 是标量或数组时说明文件已被改成别的形状，报错比覆盖更安全。
fn json_object_at<'a>(
    owner: &'a mut serde_json::Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<&'a mut serde_json::Map<String, Value>, Box<dyn Error>> {
    owner
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| format!("{label} 的 {key} 必须是对象").into())
}

fn render_provider_config(
    root: &Value,
    format: ProviderConfigFormat,
    label: &str,
) -> Result<String, Box<dyn Error>> {
    match format {
        ProviderConfigFormat::Json => Ok(format!("{}\n", serde_json::to_string_pretty(root)?)),
        ProviderConfigFormat::Yaml => serde_yaml::to_string(root)
            .map_err(|error| Box::<dyn Error>::from(format!("{label} 序列化失败：{error}"))),
    }
}

fn continue_himind_entry_model(entry: &Value) -> Option<String> {
    let name = entry.get("name")?.as_str()?;
    name.strip_prefix(CONTINUE_HIMIND_PREFIX)?;
    Some(
        entry
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(name)
            .to_string(),
    )
}

fn continue_himind_models(root: &Value) -> Vec<String> {
    root.get("models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(continue_himind_entry_model)
                .collect()
        })
        .unwrap_or_default()
}

/// Continue 条目结构对齐 config-yaml 的 modelSchema：name/provider/model/apiBase/apiKey。
/// apiBase 直接写服务地址，openai provider 会自行追加 /chat/completions 或 /responses；
/// Anthropic 协议改用 anthropic provider，基址需要不带 /v1（Anthropic SDK 自己追加）。
fn build_continue_config(
    original: &str,
    format: ProviderConfigFormat,
    credential: &AIClientCredential,
    models: &[String],
    preferred: &str,
) -> Result<String, Box<dyn Error>> {
    let label = "Continue config";
    let anthropic = credential.access.protocol.trim() == "anthropic";
    let endpoint = if anthropic {
        anthropic_base_url(&credential.access.base_url)?
    } else {
        normalized_base_url(&credential.access.base_url)?
    };
    let mut root = provider_config_object(original, format, label)?;
    if original.trim().is_empty() {
        // name/version 是 config.yaml 的必填字段，缺省会让 Continue 判定配置无效。
        root.insert("name".to_string(), json!("Main Config"));
        root.insert("version".to_string(), json!("0.0.1"));
        root.insert("schema".to_string(), json!("v1"));
    }
    let mut entries = match root.remove("models") {
        None => Vec::new(),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(format!("{label} 的 models 必须是列表").into()),
    };
    entries.retain(|entry| continue_himind_entry_model(entry).is_none());
    let default_index = models
        .iter()
        .position(|model| model == preferred)
        .unwrap_or(0);
    let use_responses_api = !anthropic && credential.access.protocol.trim() != "openai-chat";
    for (index, model) in models.iter().enumerate() {
        let mut entry = json!({
            "name": format!("{CONTINUE_HIMIND_PREFIX}{model}"),
            "provider": if anthropic { "anthropic" } else { "openai" },
            "model": model,
            "apiBase": endpoint,
            "apiKey": credential.api_key,
            "capabilities": ["tool_use"],
            "roles": if index == default_index {
                json!(["chat", "edit", "apply"])
            } else {
                json!(["chat"])
            },
        });
        if use_responses_api {
            entry["useResponsesApi"] = json!(true);
        }
        entries.push(entry);
    }
    root.insert("models".to_string(), Value::Array(entries));
    render_provider_config(&Value::Object(root), format, label)
}

/// Aider 只认顶层标量：端点、密钥、默认模型与 himind/<model> 别名。
/// 别名用 `name:model` 形式（aider --alias 的格式），卸载时按 himind/ 前缀判定归属。
fn build_aider_config(
    original: &str,
    format: ProviderConfigFormat,
    credential: &AIClientCredential,
    models: &[String],
    preferred: &str,
) -> Result<String, Box<dyn Error>> {
    let label = "Aider .aider.conf.yml";
    let endpoint = normalized_base_url(&credential.access.base_url)?;
    let mut root = provider_config_object(original, format, label)?;
    let mut aliases = match root.remove(AIDER_ALIAS_KEY) {
        None => Vec::new(),
        // aider 允许单个标量，也允许列表。
        Some(Value::String(single)) => vec![Value::String(single)],
        Some(Value::Array(items)) => items,
        Some(_) => return Err(format!("{label} 的 alias 必须是列表").into()),
    };
    aliases.retain(|alias| {
        !alias
            .as_str()
            .is_some_and(|value| value.starts_with(AIDER_HIMIND_PREFIX))
    });
    for model in models {
        aliases.push(json!(format!(
            "{AIDER_HIMIND_PREFIX}{model}:openai/{model}"
        )));
    }
    root.insert(AIDER_OPENAI_BASE_KEY.to_string(), json!(endpoint));
    root.insert(AIDER_OPENAI_KEY_KEY.to_string(), json!(credential.api_key));
    root.insert(
        AIDER_MODEL_KEY.to_string(),
        json!(format!("{AIDER_HIMIND_PREFIX}{preferred}")),
    );
    root.insert(AIDER_ALIAS_KEY.to_string(), Value::Array(aliases));
    render_provider_config(&Value::Object(root), format, label)
}

// ---- Crush ----
// providers.<id> 是 OpenCode 系的 provider 形状（type/name/base_url/api_key/models），
// 外加 models.large / models.small 两个用途槽位。导入即让 Crush 用上 HiMind：
// 两个槽位都指向 himind/<首选模型>，与 Aider 写默认模型的取舍一致。
// 推理档位（models.large.reasoning_effort）留在用户侧，导入不覆盖也不清理。
fn build_crush_config(
    original: &str,
    format: ProviderConfigFormat,
    credential: &AIClientCredential,
    models: &[String],
    preferred: &str,
) -> Result<String, Box<dyn Error>> {
    let label = "Crush crush.json";
    let endpoint = normalized_base_url(&credential.access.base_url)?;
    let mut root = provider_config_object(original, format, label)?;
    let entries = models
        .iter()
        .map(|model| {
            json!({
                "id": model,
                "name": model,
                "context_window": DECLARATIVE_MODEL_CONTEXT_WINDOW,
                "default_max_tokens": CRUSH_DEFAULT_MAX_TOKENS,
            })
        })
        .collect::<Vec<_>>();
    json_object_at(&mut root, "providers", label)?.insert(
        DECLARATIVE_PROVIDER_ID.to_string(),
        json!({
            "type": "openai",
            "name": DECLARATIVE_PROVIDER_ID,
            "base_url": endpoint,
            "api_key": credential.api_key,
            "models": entries,
        }),
    );
    // 只在槽位对象上覆写 provider / model，保留用户自己设的推理档位等字段。
    // 取消导入时按快照把槽位整体还原（见 strip_crush_himind），所以这里即使
    // 覆写也必须在导入前留快照，不能把用户的 reasoning_effort 冲掉。
    let slots = json_object_at(&mut root, "models", label)?;
    for slot in ["large", "small"] {
        let mut entry = slots
            .get(slot)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        entry.insert(
            "provider".to_string(),
            json!(DECLARATIVE_PROVIDER_ID),
        );
        entry.insert("model".to_string(), json!(preferred));
        slots.insert(slot.to_string(), Value::Object(entry));
    }
    render_provider_config(&Value::Object(root), format, label)
}

// ---- Qoder / Qoder CN ----
// providers.<id> 写端点、密钥与模型清单，model.name 用 `<id>/<model>` 选中它。
// Qoder 的模型推理档位是每个模型自己的 model.preferences，导入不触碰用户既有档位，
// 只把默认模型切到 himind。
fn build_qoder_config(
    original: &str,
    format: ProviderConfigFormat,
    credential: &AIClientCredential,
    models: &[String],
    preferred: &str,
) -> Result<String, Box<dyn Error>> {
    let label = "Qoder settings.json";
    let endpoint = normalized_base_url(&credential.access.base_url)?;
    let mut root = provider_config_object(original, format, label)?;
    let entries = models
        .iter()
        .map(|model| {
            json!({
                "model": model,
                "displayName": model,
                "capabilities": { "tools": true },
            })
        })
        .collect::<Vec<_>>();
    json_object_at(&mut root, "providers", label)?.insert(
        DECLARATIVE_PROVIDER_ID.to_string(),
        json!({
            "displayName": DECLARATIVE_PROVIDER_ID,
            "protocol": "openai",
            "baseUrl": endpoint,
            "apiKey": credential.api_key,
            "model": preferred,
            "models": entries,
        }),
    );
    json_object_at(&mut root, "model", label)?.insert(
        "name".to_string(),
        json!(format!("{DECLARATIVE_MODEL_PREFIX}{preferred}")),
    );
    render_provider_config(&Value::Object(root), format, label)
}

// ---- ZCode ----
// config.json 的 provider.<id> 是 OpenCode 形状加自定义 kind：kind 固定 anthropic，
// 端点按 baseURL + /v1/messages 请求，故 baseURL 不带 /v1。
// 模型条目的 limit.output 留空：HiMind 凭据没有各模型的输出上限，写死一个上限
// （ZCode 自身缺省是 32000）一旦超过上游限制会让请求直接报错，宁可交给客户端缺省。
// 用户在 ZCode 里关掉的 provider 保持关闭，不因重新导入被打开。
fn build_zcode_config(
    original: &str,
    format: ProviderConfigFormat,
    credential: &AIClientCredential,
    models: &[String],
) -> Result<String, Box<dyn Error>> {
    let label = "ZCode config.json";
    let endpoint = anthropic_base_url(&credential.access.base_url)?;
    let mut root = provider_config_object(original, format, label)?;
    let enabled = zcode_provider_enabled(&root, DECLARATIVE_PROVIDER_ID).unwrap_or(true);
    let mut entries = serde_json::Map::new();
    for model in models {
        entries.insert(
            model.clone(),
            json!({
                "name": model,
                "limit": { "context": DECLARATIVE_MODEL_CONTEXT_WINDOW },
                "modalities": { "input": ["text"], "output": ["text"] },
            }),
        );
    }
    json_object_at(&mut root, "provider", label)?.insert(
        DECLARATIVE_PROVIDER_ID.to_string(),
        json!({
            "name": DECLARATIVE_PROVIDER_ID,
            "kind": "anthropic",
            "enabled": enabled,
            "source": "custom",
            "options": { "apiKey": credential.api_key, "baseURL": endpoint },
            "models": Value::Object(entries),
        }),
    );
    render_provider_config(&Value::Object(root), format, label)
}

// ZCode 3.14 起 provider 的事实源是这份规则文件：providerRule 声明 provider 与
// 访问方式，providerModelRules 声明每个模型的窗口与输入形态。旧版 ZCode 读的
// config.json 由 build_zcode_config 一并写。这里按 providerId 归组，其他规则与
// 用户的 manualProviderModelRules 原样保留；用户手工设过的模型（manual 规则）
// 不覆盖，也不因重新导入被重置。
fn build_zcode_provider_rules(
    original: &str,
    format: ProviderConfigFormat,
    credential: &AIClientCredential,
    models: &[String],
) -> Result<String, Box<dyn Error>> {
    let label = "ZCode provider_config.json";
    let endpoint = anthropic_base_url(&credential.access.base_url)?;
    let mut root = provider_config_object(original, format, label)?;
    root.entry("schemaVersion".to_string()).or_insert(json!(1));
    let config = json_object_at(&mut root, "config", label)?;
    let provider_rules = json_object_at(config, "providerConfigRules", label)?;
    let mut rule = json!({
        "providerId": DECLARATIVE_PROVIDER_ID,
        "providerName": DECLARATIVE_PROVIDER_ID,
        "enabled": true,
        "config": {
            "group": "standard-personal",
            "access": { "type": "api-key", "apiKey": credential.api_key },
            "api": { "type": "anthropic-messages", "baseUrl": endpoint },
            "personalModelIds": models,
            "modelOrder": models,
        },
    });
    if let Some(enabled) = provider_rules
        .get("providerRules")
        .and_then(Value::as_array)
        .and_then(|rules| zcode_rule_enabled(rules, DECLARATIVE_PROVIDER_ID))
    {
        rule["enabled"] = json!(enabled);
    }
    zcode_rules_replace(
        provider_rules,
        "providerRules",
        DECLARATIVE_PROVIDER_ID,
        vec![rule],
    );
    let model_rules = json_object_at(config, "modelConfigRules", label)?;
    let entries = models
        .iter()
        .map(|model| {
            json!({
                "providerId": DECLARATIVE_PROVIDER_ID,
                "modelId": model,
                "config": {
                    "properties": {
                        "contextWindow": DECLARATIVE_MODEL_CONTEXT_WINDOW,
                        "inputFormat": { "supportsImage": false },
                    },
                },
            })
        })
        .collect::<Vec<_>>();
    zcode_rules_replace(
        model_rules,
        "providerModelRules",
        DECLARATIVE_PROVIDER_ID,
        entries,
    );
    // 用户手工设过的模型规则不覆盖，但 HiMind 自己的残留要清掉。
    zcode_rules_replace(
        model_rules,
        "manualProviderModelRules",
        DECLARATIVE_PROVIDER_ID,
        Vec::new(),
    );
    render_provider_config(&Value::Object(root), format, label)
}

/// 读 config.json 里 HiMind provider 的启用开关；没有记录时返回 None（按启用处理）。
fn zcode_provider_enabled(
    root: &serde_json::Map<String, Value>,
    provider_id: &str,
) -> Option<bool> {
    root.get("provider")?
        .get(provider_id)?
        .get("enabled")?
        .as_bool()
}

/// 读 providerRules 里 HiMind 那条规则的 enabled；没有记录时返回 None。
fn zcode_rule_enabled(rules: &[Value], provider_id: &str) -> Option<bool> {
    rules
        .iter()
        .find(|rule| rule.get("providerId").and_then(Value::as_str) == Some(provider_id))?
        .get("enabled")?
        .as_bool()
}

/// 把规则数组里的 HiMind 条目换成 `entries`，其他条目按原顺序保留。
fn zcode_rules_replace(
    owner: &mut serde_json::Map<String, Value>,
    key: &str,
    provider_id: &str,
    entries: Vec<Value>,
) {
    let existing = match owner.get_mut(key) {
        Some(Value::Array(items)) => std::mem::take(items),
        _ => Vec::new(),
    };
    let mut merged = existing
        .into_iter()
        .filter(|rule| rule.get("providerId").and_then(Value::as_str) != Some(provider_id))
        .collect::<Vec<_>>();
    merged.extend(entries);
    owner.insert(key.to_string(), Value::Array(merged));
}

/// 按文件自己的布局渲染写入内容；文件列表由目标定义给出，主配置与附属文件各写各的形状。
fn build_provider_config_file(
    file: &ProviderConfigFile,
    original: &str,
    format: ProviderConfigFormat,
    credential: &AIClientCredential,
    models: &[String],
    preferred: &str,
) -> Result<String, Box<dyn Error>> {
    match file.layout {
        ProviderConfigLayout::ContinueModels => {
            build_continue_config(original, format, credential, models, preferred)
        }
        ProviderConfigLayout::AiderConf => {
            build_aider_config(original, format, credential, models, preferred)
        }
        ProviderConfigLayout::CrushConfig => {
            build_crush_config(original, format, credential, models, preferred)
        }
        ProviderConfigLayout::QoderSettings => {
            build_qoder_config(original, format, credential, models, preferred)
        }
        ProviderConfigLayout::ZCodeConfig => {
            build_zcode_config(original, format, credential, models)
        }
        ProviderConfigLayout::ZCodeProviderRules => {
            build_zcode_provider_rules(original, format, credential, models)
        }
    }
}

/// aider 的 alias 既可能是单标量，也可能是列表；这里只取 HiMind 写进去的部分。
fn himind_aliases(value: Option<&Value>) -> Vec<String> {
    let mut aliases = match value {
        Some(Value::String(single)) => vec![single.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    aliases.retain(|alias| alias.starts_with(AIDER_HIMIND_PREFIX));
    aliases
}

fn aider_himind_models(root: &Value) -> Vec<String> {
    let mut models = Vec::new();
    if let Some(model) = root
        .get(AIDER_MODEL_KEY)
        .and_then(Value::as_str)
        .and_then(|value| value.strip_prefix(AIDER_HIMIND_PREFIX))
    {
        models.push(model.to_string());
    }
    for alias in himind_aliases(root.get(AIDER_ALIAS_KEY)) {
        let Some((name, _)) = alias.split_once(':') else {
            continue;
        };
        let Some(model) = name.strip_prefix(AIDER_HIMIND_PREFIX) else {
            continue;
        };
        if !models.iter().any(|existing| existing == model) {
            models.push(model.to_string());
        }
    }
    models
}

fn read_provider_config_models(
    definition: &AiProviderTargetDefinition,
    path: &Path,
    format: ProviderConfigFormat,
) -> Result<Vec<String>, Box<dyn Error>> {
    let Ok(content) = fs::read_to_string(path) else {
        return Ok(Vec::new());
    };
    let Some(root) = parse_provider_config(&content, format, definition.id)? else {
        return Ok(Vec::new());
    };
    Ok(himind_models_in(
        provider_config_file_primary(definition).layout,
        &root,
    ))
}

/// 按主配置布局读出 HiMind 写进去的模型列表；列表非空即视为已导入。
fn himind_models_in(layout: ProviderConfigLayout, root: &Value) -> Vec<String> {
    match layout {
        ProviderConfigLayout::ContinueModels => continue_himind_models(root),
        ProviderConfigLayout::AiderConf => aider_himind_models(root),
        ProviderConfigLayout::CrushConfig => crush_himind_models(root),
        ProviderConfigLayout::QoderSettings => qoder_himind_models(root),
        ProviderConfigLayout::ZCodeConfig => zcode_himind_models(root),
        ProviderConfigLayout::ZCodeProviderRules => zcode_rule_himind_models(root),
    }
}

/// Crush：providers.himind.models[].id。
fn crush_himind_models(root: &Value) -> Vec<String> {
    root.get("providers")
        .and_then(|providers| providers.get(DECLARATIVE_PROVIDER_ID))
        .and_then(|provider| provider.get("models"))
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| model.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Qoder：providers.himind.models[].model；模型清单缺失时回落到默认模型。
fn qoder_himind_models(root: &Value) -> Vec<String> {
    let listed = root
        .get("providers")
        .and_then(|providers| providers.get(DECLARATIVE_PROVIDER_ID))
        .and_then(|provider| provider.get("models"))
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| model.get("model").and_then(Value::as_str))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !listed.is_empty() {
        return listed;
    }
    qoder_selected_model(root).into_iter().collect()
}

/// Qoder 当前选中的 HiMind 模型（model.name 去掉 `<id>/` 前缀）。
fn qoder_selected_model(root: &Value) -> Option<String> {
    root.get("model")?
        .get("name")?
        .as_str()?
        .strip_prefix(DECLARATIVE_MODEL_PREFIX)
        .map(str::to_string)
}

/// ZCode config.json：provider.himind.models 的键就是模型 id。
fn zcode_himind_models(root: &Value) -> Vec<String> {
    root.get("provider")
        .and_then(|provider| provider.get(DECLARATIVE_PROVIDER_ID))
        .and_then(|entry| entry.get("models"))
        .and_then(Value::as_object)
        .map(|models| models.keys().cloned().collect())
        .unwrap_or_default()
}

/// ZCode provider_config.json：模型来自 HiMind 的 providerModelRules。
fn zcode_rule_himind_models(root: &Value) -> Vec<String> {
    root.get("config")
        .and_then(|config| config.get("modelConfigRules"))
        .and_then(|rules| rules.get("providerModelRules"))
        .and_then(Value::as_array)
        .map(|rules| {
            rules
                .iter()
                .filter(|rule| {
                    rule.get("providerId").and_then(Value::as_str) == Some(DECLARATIVE_PROVIDER_ID)
                })
                .filter_map(|rule| rule.get("modelId").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn strip_continue_himind(
    original: &str,
    format: ProviderConfigFormat,
) -> Result<(String, bool), Box<dyn Error>> {
    let label = "Continue config";
    let mut root = provider_config_object(original, format, label)?;
    let Some(Value::Array(entries)) = root.get_mut("models") else {
        return Ok((original.to_string(), false));
    };
    let before = entries.len();
    entries.retain(|entry| continue_himind_entry_model(entry).is_none());
    if entries.len() == before {
        return Ok((original.to_string(), false));
    }
    Ok((
        render_provider_config(&Value::Object(root), format, label)?,
        true,
    ))
}

// 该文件不支持多来源并存：只要还存在 himind/* 标记，端点、密钥与默认模型就是本次写入的。
// 有导入快照时以快照为准：导入前存在的键写回原值，导入前不存在才删除。
fn strip_aider_himind(
    original: &str,
    format: ProviderConfigFormat,
    previous: Option<&str>,
) -> Result<(String, bool), Box<dyn Error>> {
    let label = "Aider .aider.conf.yml";
    let mut root = provider_config_object(original, format, label)?;
    let previous_root = previous
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| provider_config_object(text, format, label))
        .transpose()
        .ok()
        .flatten();
    let has_marker = root
        .get(AIDER_MODEL_KEY)
        .and_then(Value::as_str)
        .is_some_and(|value| value.starts_with(AIDER_HIMIND_PREFIX))
        || !himind_aliases(root.get(AIDER_ALIAS_KEY)).is_empty();
    if previous_root.is_none() && !has_marker {
        return Ok((original.to_string(), false));
    }
    let mut changed = false;
    for key in [AIDER_OPENAI_BASE_KEY, AIDER_OPENAI_KEY_KEY] {
        match previous_root
            .as_ref()
            .and_then(|root| root.get(key))
            .cloned()
        {
            Some(original_value) => {
                if root.get(key) != Some(&original_value) {
                    root.insert(key.to_string(), original_value);
                    changed = true;
                }
            }
            None => {
                if root.remove(key).is_some() {
                    changed = true;
                }
            }
        }
    }
    let had_himind_model = root
        .get(AIDER_MODEL_KEY)
        .and_then(Value::as_str)
        .is_some_and(|value| value.starts_with(AIDER_HIMIND_PREFIX));
    match previous_root
        .as_ref()
        .and_then(|root| root.get(AIDER_MODEL_KEY))
        .cloned()
    {
        Some(original_value) => {
            if root.get(AIDER_MODEL_KEY) != Some(&original_value) {
                root.insert(AIDER_MODEL_KEY.to_string(), original_value);
                changed = true;
            }
        }
        None => {
            if had_himind_model {
                root.remove(AIDER_MODEL_KEY);
                changed = true;
            }
        }
    }
    let mut aliases = match root.remove(AIDER_ALIAS_KEY) {
        Some(Value::String(single)) => vec![Value::String(single)],
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    };
    let aliases_before = aliases.len();
    aliases.retain(|alias| {
        !alias
            .as_str()
            .is_some_and(|value| value.starts_with(AIDER_HIMIND_PREFIX))
    });
    changed |= aliases.len() != aliases_before;
    if !aliases.is_empty() {
        root.insert(AIDER_ALIAS_KEY.to_string(), Value::Array(aliases));
    }
    if !changed {
        return Ok((original.to_string(), false));
    }
    Ok((
        render_provider_config(&Value::Object(root), format, label)?,
        true,
    ))
}

/// 按文件布局剥离 HiMind 写入的内容；返回 (内容, 是否有改动)。
/// `previous` 为该文件的导入前快照，仅有需要的布局会用到（目前是 Aider）。
fn strip_provider_config_file(
    layout: ProviderConfigLayout,
    original: &str,
    format: ProviderConfigFormat,
    previous: Option<&str>,
) -> Result<(String, bool), Box<dyn Error>> {
    if original.trim().is_empty() {
        return Ok((String::new(), false));
    }
    match layout {
        ProviderConfigLayout::ContinueModels => strip_continue_himind(original, format),
        ProviderConfigLayout::AiderConf => strip_aider_himind(original, format, previous),
        ProviderConfigLayout::CrushConfig => strip_crush_himind(original, format, previous),
        ProviderConfigLayout::QoderSettings => strip_qoder_himind(original, format, previous),
        ProviderConfigLayout::ZCodeConfig => strip_zcode_himind(original, format),
        ProviderConfigLayout::ZCodeProviderRules => strip_zcode_rules_himind(original, format),
    }
}

/// Crush：移除 providers.himind，并把 large/small 槽位还原到导入前的原值；
/// 没有快照（旧簿记）时才退化为「只在仍指向 HiMind 时清掉槽位」。
fn strip_crush_himind(
    original: &str,
    format: ProviderConfigFormat,
    previous: Option<&str>,
) -> Result<(String, bool), Box<dyn Error>> {
    let label = "Crush crush.json";
    let mut root = provider_config_object(original, format, label)?;
    let mut changed = root
        .get_mut("providers")
        .and_then(Value::as_object_mut)
        .is_some_and(|providers| providers.remove(DECLARATIVE_PROVIDER_ID).is_some());
    let restore_root = previous
        .filter(|text| !text.trim().is_empty())
        .and_then(|text| parse_provider_config(text, format, label).ok().flatten());
    let mut drop_slots = false;
    if let Some(slots) = root.get_mut("models").and_then(Value::as_object_mut) {
        for slot in ["large", "small"] {
            let points_at_himind = slots
                .get(slot)
                .is_some_and(is_crush_himind_slot);
            if !points_at_himind {
                continue;
            }
            match restore_root
                .as_ref()
                .and_then(|restore| restore.get("models"))
                .and_then(Value::as_object)
                .and_then(|models| models.get(slot))
                .cloned()
            {
                Some(original_slot) => {
                    if slots.get(slot) != Some(&original_slot) {
                        slots.insert(slot.to_string(), original_slot);
                        changed = true;
                    }
                }
                None => {
                    slots.remove(slot);
                    changed = true;
                }
            }
        }
        drop_slots = slots.is_empty();
    }
    if drop_slots {
        root.remove("models");
    }
    if !changed {
        return Ok((original.to_string(), false));
    }
    Ok((
        render_provider_config(&Value::Object(root), format, label)?,
        true,
    ))
}

fn is_crush_himind_slot(entry: &Value) -> bool {
    entry.get("provider").and_then(Value::as_str) == Some(DECLARATIVE_PROVIDER_ID)
}

/// Qoder：移除 providers.himind，并把 model.name 还原到导入前的原值；
/// 没有快照时才退化为「只在仍指向 HiMind 时清掉 model.name」。
fn strip_qoder_himind(
    original: &str,
    format: ProviderConfigFormat,
    previous: Option<&str>,
) -> Result<(String, bool), Box<dyn Error>> {
    let label = "Qoder settings.json";
    let mut root = provider_config_object(original, format, label)?;
    let mut changed = root
        .get_mut("providers")
        .and_then(Value::as_object_mut)
        .is_some_and(|providers| providers.remove(DECLARATIVE_PROVIDER_ID).is_some());
    let previous_name = previous
        .filter(|text| !text.trim().is_empty())
        .and_then(|text| parse_provider_config(text, format, label).ok().flatten())
        .and_then(|restore| {
            restore
                .get("model")
                .and_then(|model| model.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let mut drop_model = false;
    if let Some(model) = root.get_mut("model").and_then(Value::as_object_mut) {
        let points_at_himind = model
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| name.starts_with(DECLARATIVE_MODEL_PREFIX));
        if points_at_himind {
            match previous_name {
                Some(name) if model.get("name").and_then(Value::as_str) != Some(name.as_str()) => {
                    model.insert("name".to_string(), json!(name));
                    changed = true;
                }
                None => {
                    model.remove("name");
                    changed = true;
                }
                // 快照原值就是当前值：无需改动。
                Some(_) => {}
            }
        }
        drop_model = model.is_empty();
    }
    if drop_model {
        root.remove("model");
    }
    if !changed {
        return Ok((original.to_string(), false));
    }
    Ok((
        render_provider_config(&Value::Object(root), format, label)?,
        true,
    ))
}

/// ZCode config.json：移除 provider.himind。
fn strip_zcode_himind(
    original: &str,
    format: ProviderConfigFormat,
) -> Result<(String, bool), Box<dyn Error>> {
    let label = "ZCode config.json";
    let mut root = provider_config_object(original, format, label)?;
    let changed = root
        .get_mut("provider")
        .and_then(Value::as_object_mut)
        .is_some_and(|providers| providers.remove(DECLARATIVE_PROVIDER_ID).is_some());
    if changed && root.get("provider").is_some_and(Value::is_object) {
        // 只剩空对象时一并移除，避免留下一个空壳段。
        if root
            .get("provider")
            .and_then(Value::as_object)
            .is_some_and(serde_json::Map::is_empty)
        {
            root.remove("provider");
        }
    }
    if !changed {
        return Ok((original.to_string(), false));
    }
    Ok((
        render_provider_config(&Value::Object(root), format, label)?,
        true,
    ))
}

/// ZCode provider_config.json：按 providerId 过滤掉 HiMind 的 provider 规则与模型规则，
/// 其他 provider 的规则与用户手工规则原样保留。
fn strip_zcode_rules_himind(
    original: &str,
    format: ProviderConfigFormat,
) -> Result<(String, bool), Box<dyn Error>> {
    let label = "ZCode provider_config.json";
    let mut root = provider_config_object(original, format, label)?;
    let mut changed = false;
    if let Some(config) = root.get_mut("config").and_then(Value::as_object_mut) {
        for (section, list) in [
            ("providerConfigRules", "providerRules"),
            ("modelConfigRules", "providerModelRules"),
            ("modelConfigRules", "manualProviderModelRules"),
        ] {
            let Some(entries) = config
                .get_mut(section)
                .and_then(Value::as_object_mut)
                .and_then(|owner| owner.get_mut(list))
                .and_then(Value::as_array_mut)
            else {
                continue;
            };
            let before = entries.len();
            entries.retain(|rule| {
                rule.get("providerId").and_then(Value::as_str) != Some(DECLARATIVE_PROVIDER_ID)
            });
            changed |= entries.len() != before;
        }
    }
    if !changed {
        return Ok((original.to_string(), false));
    }
    Ok((
        render_provider_config(&Value::Object(root), format, label)?,
        true,
    ))
}

fn aider_imported_detail(path: &Path, format: ProviderConfigFormat, models: &[String]) -> String {
    let default_model = fs::read_to_string(path)
        .ok()
        .and_then(|content| {
            parse_provider_config(&content, format, "Aider .aider.conf.yml")
                .ok()
                .flatten()
        })
        .and_then(|root| {
            root.get(AIDER_MODEL_KEY)
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    match default_model {
        Some(model) if model.starts_with(AIDER_HIMIND_PREFIX) => {
            format!(
                "已写入 {} 个 HiMind 模型；aider 默认使用 {model}",
                models.len()
            )
        }
        _ => format!(
            "已写入 {} 个 HiMind 模型；用 aider --model himind/{} 调用",
            models.len(),
            models.first().cloned().unwrap_or_default()
        ),
    }
}

fn provider_config_status(definition: &AiProviderTargetDefinition) -> AIProviderImportStatus {
    let path = provider_config_file_primary(definition).path;
    let format = provider_config_format(&path);
    let client_detected = (definition.detected)();
    let models = read_provider_config_models(definition, &path, format).unwrap_or_default();
    let imported = !models.is_empty();
    AIProviderImportStatus {
        target: definition.id.to_string(),
        state: if imported { "imported" } else { "not_imported" }.to_string(),
        client_detected,
        detail: if imported {
            match provider_config_file_primary(definition).layout {
                ProviderConfigLayout::ContinueModels => format!(
                    "已写入 {} 个 HiMind 模型；在 Continue 模型列表中选择",
                    models.len()
                ),
                ProviderConfigLayout::AiderConf => aider_imported_detail(&path, format, &models),
                ProviderConfigLayout::CrushConfig => format!(
                    "已写入 {} 个 HiMind 模型；Crush 的 large/small 已切到 himind",
                    models.len()
                ),
                ProviderConfigLayout::QoderSettings => format!(
                    "已写入 {} 个 HiMind 模型；Qoder 默认模型为 himind/{}",
                    models.len(),
                    models.first().cloned().unwrap_or_default()
                ),
                ProviderConfigLayout::ZCodeConfig => format!(
                    "已写入 {} 个 HiMind 模型；重启 ZCode 后可在模型选择器中看到",
                    models.len()
                ),
                ProviderConfigLayout::ZCodeProviderRules => {
                    format!("已写入 {} 个 HiMind 模型规则", models.len())
                }
            }
        } else if client_detected {
            format!("已检测到 {}，尚未导入 HiMind AI", definition.display_name)
        } else {
            definition.not_detected_hint.to_string()
        },
        config_path: path.to_string_lossy().to_string(),
        models,
        synced_at: String::new(),
        service: String::new(),
        ..Default::default()
    }
}

fn import_provider_config(
    definition: &AiProviderTargetDefinition,
    options: &Options,
    expected_user_id: &str,
    service: &str,
) -> Result<AIProviderImportResult, Box<dyn Error>> {
    let files = (definition.config_files)();
    let primary = provider_config_file_primary(definition);
    let client_detected = (definition.detected)();
    let client_id = format!("{}-import", definition.id);
    let credential = resolve_credential(options, expected_user_id, &client_id, service)?;
    match definition.protocol_support {
        ProviderProtocolSupport::OpenAiOnly => {
            ensure_openai_compatible(&credential, definition.display_name)?
        }
        ProviderProtocolSupport::AnthropicOnly => {
            ensure_anthropic_compatible(&credential, definition.display_name)?
        }
        ProviderProtocolSupport::Both => {}
    }
    let models = available_models(&credential)?;
    let preferred = preferred_model(&credential)?;
    // 主配置与附属文件逐个读写：为每个文件单独写备份，避免一个文件失败后
    // 其他文件停在半写状态（失败会直接向上抛出，已写的文件保持可回滚）。
    let mut backup_path = String::new();
    for file in &files {
        let format = provider_config_format(&file.path);
        let original = if file.path.is_file() {
            fs::read_to_string(&file.path)?
        } else {
            String::new()
        };
        let updated =
            build_provider_config_file(file, &original, format, &credential, &models, &preferred)?;
        let backup = backup_and_write(&file.path, updated.as_bytes())?;
        if file.path == primary.path {
            backup_path = backup
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default();
        }
    }
    Ok(AIProviderImportResult {
        ok: true,
        target: definition.id.to_string(),
        status: "configured".to_string(),
        model_count: models.len(),
        model: preferred,
        config_path: primary.path.to_string_lossy().to_string(),
        backup_path,
        client_detected,
        ..Default::default()
    })
}

fn cancel_provider_config(
    definition: &AiProviderTargetDefinition,
    restore: Option<&Value>,
) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
    let files = (definition.config_files)();
    let primary = provider_config_file_primary(definition);
    let client_detected = (definition.detected)();
    let mut removed = false;
    let mut backup_path = String::new();
    let previous_files = restore
        .and_then(|value| value.get("files"))
        .and_then(Value::as_object);
    for file in &files {
        let format = provider_config_format(&file.path);
        let original = if file.path.is_file() {
            fs::read_to_string(&file.path)?
        } else {
            String::new()
        };
        let previous = previous_files
            .and_then(|files| files.get(&file.path.to_string_lossy().to_string()))
            .and_then(Value::as_str);
        let (updated, changed) =
            strip_provider_config_file(file.layout, &original, format, previous)?;
        if !changed {
            continue;
        }
        removed = true;
        let backup = backup_and_write(&file.path, updated.as_bytes())?;
        if file.path == primary.path {
            backup_path = backup
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default();
        }
    }
    Ok(AIProviderImportCancelResult {
        ok: true,
        target: definition.id.to_string(),
        status: if removed { "cancelled" } else { "not_imported" }.to_string(),
        changed: removed,
        client_detected,
        detail: if removed {
            format!("已移除 {} 中的 HiMind 配置", definition.display_name)
        } else {
            format!("{} 当前没有 HiMind 导入记录", definition.display_name)
        },
        backup_path,
    })
}

impl AIClientAdapter for ProviderConfigAdapter {
    fn id(&self) -> &'static str {
        self.0.id
    }
    fn display_name(&self) -> &'static str {
        self.0.display_name
    }
    fn status(&self, _options: &Options) -> AIProviderImportStatus {
        provider_config_status(self.0)
    }
    fn plan(&self, action: &str, status: &AIProviderImportStatus) -> AIProviderImportPlan {
        plan_for(self.0.id, action, status)
    }
    fn import(
        &self,
        options: &Options,
        user_id: &str,
        service: &str,
    ) -> Result<AIProviderImportResult, Box<dyn Error>> {
        import_provider_config(self.0, options, user_id, service)
    }
    fn owned_snapshot(&self, _options: &Options) -> Option<Value> {
        provider_config_owned_snapshot(self.0)
    }
    fn cancel(
        &self,
        _options: &Options,
        restore: Option<&Value>,
    ) -> Result<AIProviderImportCancelResult, Box<dyn Error>> {
        cancel_provider_config(self.0, restore)
    }
}

/// 布局在导入时会覆盖用户既有键时（Aider、Crush 的槽位、Qoder 的默认模型），
/// 导入前留存原文件用于还原；只做新增式合并的布局也会留存，取消逻辑忽略即可。
fn provider_config_owned_snapshot(definition: &AiProviderTargetDefinition) -> Option<Value> {
    let files = (definition.config_files)();
    let mut snapshot = serde_json::Map::new();
    for file in files {
        if let Ok(text) = fs::read_to_string(&file.path) {
            snapshot.insert(file.path.to_string_lossy().to_string(), Value::String(text));
        }
    }
    if snapshot.is_empty() {
        None
    } else {
        Some(json!({ "files": Value::Object(snapshot) }))
    }
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

/// 解析 WorkBuddy models.json，并把两种根形态归一到对象。
///
/// WorkBuddy 自己会把空配置写成顶层数组 `[]`（更早还写过 0 字节文件），而 HiMind
/// 写入的是对象 `{ models, availableModels }`。两者都必须能读：顶层数组按「模型列表」
/// 包成对象，并返回 `true`，让调用方按原形态写回——不擅自替 WorkBuddy 改变表示法。
/// 其它根类型继续报错。
fn workbuddy_root_value(content: &str, action: &str) -> Result<(Value, bool), Box<dyn Error>> {
    if content.trim().is_empty() {
        return Ok((json!({}), false));
    }
    let root = serde_json::from_str::<Value>(content)
        .map_err(|_| format!("WorkBuddy models.json 格式无效，已停止{action}且未覆盖原文件"))?;
    if root.is_array() {
        return Ok((json!({ "models": root }), true));
    }
    if !root.is_object() {
        return Err("WorkBuddy models.json 根节点必须是 JSON 对象或数组".into());
    }
    Ok((root, false))
}

/// 按 root 的原形态写回：顶层数组根重新序列化成数组，对象根保持对象。
fn workbuddy_serialize(root: &Value, array_root: bool) -> Result<String, Box<dyn Error>> {
    let value = if array_root {
        root.get("models").cloned().unwrap_or_else(|| json!([]))
    } else {
        root.clone()
    };
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}

fn merge_workbuddy_models(
    content: &str,
    credential: &AIClientCredential,
) -> Result<(String, usize), Box<dyn Error>> {
    let (mut root, array_root) = workbuddy_root_value(content, "导入")?;
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
    Ok((workbuddy_serialize(&root, array_root)?, aliases.len()))
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

/// 入口协议兼容性判定：先看服务的 `protocol` 标签，标签不匹配时**再探测端点**
/// 是否真的支持目标协议（能力判定），而不是只凭标签拒绝。
///
/// 起因：HiMind 云端网关同时讲 Anthropic 与 OpenAI，但服务记录只标了
/// `openai-responses`；标签判定会误拦这类「标签单一、端点多协议」的服务，
/// 例如把可用的网关服务导入 Claude Desktop。探测确认可达即放行，探测不了
/// 或明确不支持，才回落到原有的清晰报错。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RequiredProtocol {
    Anthropic,
    OpenAi,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ProbeVerdict {
    Supported,
    Unsupported,
    Unavailable,
}

#[cfg(not(test))]
fn protocol_probe(credential: &AIClientCredential, required: RequiredProtocol) -> ProbeVerdict {
    probe_endpoint_protocol(credential, required)
}

#[cfg(test)]
fn protocol_probe(_credential: &AIClientCredential, _required: RequiredProtocol) -> ProbeVerdict {
    // 单测不打网络：回落到标签判定，行为与历史一致。
    ProbeVerdict::Unavailable
}

/// 标签是否已经满足目标协议。`openai-chat` / `openai-responses` 都算 OpenAI 兼容。
fn label_satisfies_protocol(credential: &AIClientCredential, required: RequiredProtocol) -> bool {
    match required {
        RequiredProtocol::Anthropic => credential.access.protocol.trim() == "anthropic",
        RequiredProtocol::OpenAi => credential.access.protocol.trim() != "anthropic",
    }
}

/// 纯判定：标签满足即放行；否则看探测结论。
fn protocol_gate(label_ok: bool, verdict: ProbeVerdict) -> bool {
    label_ok || verdict == ProbeVerdict::Supported
}

#[cfg(not(test))]
fn probe_endpoint_protocol(
    credential: &AIClientCredential,
    required: RequiredProtocol,
) -> ProbeVerdict {
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
    else {
        return ProbeVerdict::Unavailable;
    };
    match required {
        RequiredProtocol::Anthropic => {
            let Ok(root) = anthropic_api_root_checked(&credential.access.base_url) else {
                return ProbeVerdict::Unavailable;
            };
            let model = preferred_model(credential).unwrap_or_default();
            let body = json!({
                "model": model,
                "max_tokens": 1,
                "messages": [{ "role": "user", "content": "ping" }],
            });
            let Ok(response) = client
                .post(format!("{root}/v1/messages"))
                .header("x-api-key", &credential.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
            else {
                return ProbeVerdict::Unavailable;
            };
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .keys()
                .map(|name| name.as_str().to_string())
                .collect::<Vec<_>>();
            let body = response.text().unwrap_or_default();
            classify_anthropic_probe(status, &headers, &body)
        }
        RequiredProtocol::OpenAi => {
            let Ok(base) = normalized_base_url(&credential.access.base_url) else {
                return ProbeVerdict::Unavailable;
            };
            let Ok(response) = client
                .get(format!("{base}/models"))
                .bearer_auth(&credential.api_key)
                .send()
            else {
                return ProbeVerdict::Unavailable;
            };
            let status = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            classify_openai_probe(status, &body)
        }
    }
}

/// Anthropic 探针判读：2xx 直接成立；4xx 里 Anthropic 的错误信封
/// （`{"type":"error","error":{...}}`）或 `anthropic-*` 响应头也足以证明端点讲该协议；
/// 404 说明没有该路由，判为不支持。
fn classify_anthropic_probe(status: u16, header_names: &[String], body: &str) -> ProbeVerdict {
    if (200..300).contains(&status) {
        return ProbeVerdict::Supported;
    }
    if status == 404 {
        return ProbeVerdict::Unsupported;
    }
    if header_names
        .iter()
        .any(|name| name.to_ascii_lowercase().starts_with("anthropic-"))
    {
        return ProbeVerdict::Supported;
    }
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if value.get("type").and_then(Value::as_str) == Some("error")
            && value.get("error").is_some_and(Value::is_object)
        {
            return ProbeVerdict::Supported;
        }
    }
    ProbeVerdict::Unsupported
}

/// OpenAI 探针判读：`/models` 2xx 且带 `data`/`object:list` 才成立；
/// 其余（含 401/404）一律判为不支持，避免把不可用端点误放行。
fn classify_openai_probe(status: u16, body: &str) -> ProbeVerdict {
    if !(200..300).contains(&status) {
        return ProbeVerdict::Unsupported;
    }
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if value.get("data").is_some_and(Value::is_array)
            || value.get("object").and_then(Value::as_str) == Some("list")
        {
            return ProbeVerdict::Supported;
        }
    }
    ProbeVerdict::Unsupported
}

/// 只支持 OpenAI 兼容协议的客户端必须显式拒绝 Anthropic 服务。
///
/// 这些客户端的配置里只有 `chat`/`responses` 两种线格式，把 Anthropic 端点按
/// Responses 写进去会得到一个能保存、但一发请求就失败的配置。这里提前报错并给出
/// 可用去处，比让用户在客户端里排查一个 404 更清楚。
///
/// 标签为 Anthropic 时，先探测端点是否其实也讲 OpenAI（标签单一、端点多协议），
/// 探测确认为不支持才拒绝。
fn ensure_openai_compatible(
    credential: &AIClientCredential,
    client_label: &str,
) -> Result<(), Box<dyn Error>> {
    if !label_satisfies_protocol(credential, RequiredProtocol::OpenAi) {
        let verdict = protocol_probe(credential, RequiredProtocol::OpenAi);
        if !protocol_gate(false, verdict) {
            return Err(format!(
                "{client_label} 只支持 OpenAI 兼容的 AI 服务，当前服务使用 Anthropic 协议。\
                 请改用 HiMind AI 或选择 OpenAI Chat / OpenAI Responses 的服务；\
                 Anthropic 服务可以导入到 Claude Code、OpenCode 等支持该协议的客户端。"
            )
            .into());
        }
    }
    Ok(())
}

/// 只吃 Anthropic Messages 的客户端（ZCode 的自定义 provider 固定 kind=anthropic）
/// 遇到 OpenAI 兼容服务时同样提前拦下：写进去只能保存、一发请求就失败。
///
/// 标签不是 Anthropic 时，先探测端点是否其实也讲 Anthropic（同一端点两种协议的服务
/// 很常见，例如 HiMind 网关），探测确认支持即放行。
fn ensure_anthropic_compatible(
    credential: &AIClientCredential,
    client_label: &str,
) -> Result<(), Box<dyn Error>> {
    if !label_satisfies_protocol(credential, RequiredProtocol::Anthropic) {
        let verdict = protocol_probe(credential, RequiredProtocol::Anthropic);
        if !protocol_gate(false, verdict) {
            return Err(format!(
                "{client_label} 的自定义 AI 服务只支持 Anthropic 协议，当前服务使用 OpenAI 兼容协议。\
                 请改用 Anthropic 协议的服务；OpenAI 兼容服务可以导入到 Continue、Crush、Qoder 等客户端。"
            )
            .into());
        }
    }
    Ok(())
}

/// 探针用的 Anthropic 根地址（剥掉末尾 `/v1`），失败时不 panic。
#[cfg(not(test))]
fn anthropic_api_root_checked(value: &str) -> Result<String, Box<dyn Error>> {
    anthropic_base_url(value)
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
        adapter_for, aider_himind_models, anthropic_base_url, build_aider_config,
        build_cc_switch_provider_settings, build_claude_settings, build_codex_config_toml,
        build_codex_models_json, build_continue_config, build_crush_config, build_kimi_code_config,
        build_opencode_config, build_qoder_config, build_qwen_code_settings,
        build_vscode_enrollment_url, build_zcode_config, build_zcode_provider_rules,
        bundled_vscode_vsix_candidates, chat_completions_url, classify_anthropic_probe,
        classify_openai_probe, compare_extension_versions,
        consume_vscode_enrollment, continue_himind_models, create_vscode_enrollment,
        crush_himind_models, ensure_anthropic_compatible, ensure_openai_compatible,
        ensure_vscode_chat_provider_allowlist, find_vscode_extension_version, known_adapters,
        legacy_workbuddy_model_id, managed_workbuddy_model_ids, merge_workbuddy_models,
        migrate_workbuddy_sessions, opencode_anthropic_base_url, opencode_npm_for_protocol,
        label_satisfies_protocol, protocol_gate,
        parse_vscode_cli_version, parse_vscode_extension_version, parse_vscode_import_status,
        push_vscode_registry_value, qoder_himind_models, read_cc_switch_managed_models,
        read_cc_switch_managed_settings, read_codex_model_catalog, remove_workbuddy_models,
        strip_aider_himind, strip_claude_himind, strip_codex_himind, strip_continue_himind,
        strip_crush_himind, strip_kimi_code_himind, strip_opencode_himind, strip_qoder_himind,
        strip_qwen_code_himind, strip_zcode_himind, strip_zcode_rules_himind,
        vscode_extension_install_required, workbuddy_model_id, workbuddy_models_path_in,
        write_cc_switch_provider, zcode_himind_models, AIClientCredential, AIProviderImportRequest,
        ProbeVerdict, ProviderConfigFormat, CLAUDE_BASE_URL_ENV, CLAUDE_CUSTOM_MODEL_OPTION,
        PROVIDER_TARGETS, VSCODE_CHAT_PROVIDER_PROPOSAL, VSCODE_EXTENSION_ID,
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
        assert_eq!(adapters.len(), 15);
        for target in [
            "vscode",
            "cc-switch",
            "codex",
            "workbuddy",
            "kimi-code",
            "qwen-code",
            "claude-code",
            "claude-desktop",
            "opencode",
            "continue",
            "aider",
            "crush",
            "qoder",
            "qoder-cn",
            "zcode",
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

    /// 删除守卫只拦「簿记里绑定到该服务」的客户端。
    ///
    /// 回归用例：曾经只要机器上存在任意一个来源不明的注册，所有自定义服务都删不掉；
    /// 来源不明的注册没有归属，不该阻止删除无关服务。
    #[test]
    fn service_removal_guard_only_blocks_bound_services() {
        let _guard = crate::store::paths::test_env_lock();
        let previous_home = std::env::var("HIMIND_AGENT_HOME").ok();
        let root = std::env::temp_dir().join(format!(
            "himind-ai-binding-guard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("HIMIND_AGENT_HOME", &root);

        let options = crate::Options::from_env();
        assert!(super::ensure_service_not_in_use(&options, "free").is_ok());

        let mut bindings = super::AIProviderImportBindings::default();
        bindings.clients.insert(
            "opencode".to_string(),
            super::AIProviderImportBinding {
                service: "custom:taken".to_string(),
                mode: String::new(),
                gateway: None,
                updated_at: String::new(),
                restore: None,
                verification_status: String::new(),
            },
        );
        super::save_import_bindings(&options, &bindings).unwrap();

        let error = super::ensure_service_not_in_use(&options, "taken")
            .expect_err("bound service must not be removable");
        assert!(
            error.to_string().contains("opencode"),
            "blocking reason must name the bound client: {error}"
        );
        // 注册来源不明的客户端没有指向这个服务，不阻止删除。
        assert!(super::ensure_service_not_in_use(&options, "free").is_ok());

        let _ = std::fs::remove_dir_all(&root);
        match previous_home {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
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

    /// 3P 档案目录里 `_meta.json` 指向的 HiMind 条目就是导入状态的唯一来源。
    /// 模型列表默认留空（交给网关 `/v1/models` 自动发现），因此这里断言为空。
    #[test]
    fn claude_desktop_status_reads_third_party_profile_entry() {
        let root = claude_temp_root("status");
        let third = root.join("Claude-3p");
        let first = root.join("Claude");
        std::fs::create_dir_all(third.join("configLibrary")).unwrap();
        let entry_id = "11111111-2222-3333-4444-555555555555";
        std::fs::write(
            third.join("configLibrary/_meta.json"),
            format!(
                r#"{{"appliedId":"{entry_id}","entries":[{{"id":"{entry_id}","name":"HiMind","provider":"gateway","note":"himind-agent"}}],"isManaged":false,"platform":"win32"}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            third.join("configLibrary").join(format!("{entry_id}.json")),
            r#"{"inferenceProvider":"gateway","inferenceGatewayBaseUrl":"https://ai.internal","inferenceGatewayApiKey":"secret","inferenceGatewayAuthScheme":"bearer","inferenceCredentialKind":"static"}"#,
        )
        .unwrap();

        let active = super::claude_desktop_status_in(&third, &first, true);
        assert_eq!(active.state, "imported");
        assert!(active.client_detected);
        assert!(
            active.models.is_empty(),
            "models come from /v1/models discovery"
        );
        assert!(active.detail.contains("自动发现"), "{}", active.detail);
        assert!(active.config_path.ends_with("claude_desktop_config.json"));

        // 已写入但客户端未切到 3P：仍算已导入，只是提示需要完整重启。
        let inactive = super::claude_desktop_status_in(&third, &first, false);
        assert_eq!(inactive.state, "imported");
        assert!(inactive.detail.contains("尚未切换"), "{}", inactive.detail);

        // 没有归属条目时不是「已导入」，但目录存在仍算检测到客户端。
        std::fs::write(
            third.join("configLibrary/_meta.json"),
            r#"{"appliedId":"","entries":[],"platform":"win32"}"#,
        )
        .unwrap();
        let empty = super::claude_desktop_status_in(&third, &first, false);
        assert_eq!(empty.state, "not_imported");
        assert!(empty.client_detected);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 3P 条目必须带来源标记头。客户端的选择器只保留「看起来像 Anthropic」的模型名，
    /// 网关靠这个头才把规范模型名换成 Anthropic 形态的路由名；漏掉就会「能发现但选择器为空」。
    #[test]
    fn claude_desktop_gateway_entry_marks_its_surface() {
        let body = super::claude_desktop_gateway_entry(
            "http://127.0.0.1:18090/gateway",
            &credential(&["deepseek-v4-pro"]),
            &["deepseek-v4-pro".to_string()],
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["inferenceProvider"], "gateway");
        assert_eq!(parsed["inferenceGatewayAuthScheme"], "bearer");
        assert_eq!(parsed["inferenceCredentialKind"], "static");
        assert_eq!(
            parsed["inferenceCustomHeaders"][super::CLAUDE_DESKTOP_SURFACE_HEADER],
            super::CLAUDE_DESKTOP_SURFACE_VALUE
        );
        assert!(
            parsed.get("inferenceModels").is_none(),
            "模型列表交给 /v1/models 自动发现"
        );
    }

    /// 复刻客户端判据：生效条目要带 `inference`/`bootstrap`/`selfHosted` 开关，
    /// 且持久化的 `deploymentMode` 不是 `1p`。
    #[test]
    fn claude_desktop_third_party_enabled_requires_gateway_entry_outside_1p_mode() {
        let root = claude_temp_root("enabled");
        let third = root.join("Claude-3p");
        std::fs::create_dir_all(third.join("configLibrary")).unwrap();
        let entry_id = "aaaaaaaa-1111-2222-3333-444444444444";
        std::fs::write(
            third.join("configLibrary/_meta.json"),
            format!(
                r#"{{"appliedId":"{entry_id}","entries":[{{"id":"{entry_id}","name":"HiMind","note":"himind-agent"}}]}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            third.join("configLibrary").join(format!("{entry_id}.json")),
            r#"{"inferenceProvider":"gateway","inferenceGatewayBaseUrl":"https://ai.internal"}"#,
        )
        .unwrap();

        // 没有 config / 没有 deploymentMode 等价于 `!== "1p"`，3P 生效。
        assert!(super::claude_desktop_third_party_enabled_in(&third, false));

        // 用户把手动档案切回 1P：客户端按 1P 启动，判据为否。
        std::fs::write(
            third.join("claude_desktop_config.json"),
            r#"{"deploymentMode":"1p"}"#,
        )
        .unwrap();
        assert!(!super::claude_desktop_third_party_enabled_in(&third, false));

        // `CLAUDE_USER_DATA_DIR` 覆盖时 Electron 两个模式都用该目录，等价于非 1P。
        assert!(super::claude_desktop_third_party_enabled_in(&third, true));

        // 条目里没有三类开关时不算 3P，即使档案目录与条目都存在。
        std::fs::write(third.join("claude_desktop_config.json"), "{}").unwrap();
        std::fs::write(
            third.join("configLibrary").join(format!("{entry_id}.json")),
            r#"{"inferenceGatewayBaseUrl":"https://ai.internal"}"#,
        )
        .unwrap();
        assert!(!super::claude_desktop_third_party_enabled_in(&third, false));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 无快照的取消只做减法：删掉自己的条目、`appliedId` 回退到剩余条目，
    /// 用户自己的档案、`deploymentMode` 与 `mcpServers` 一律不动。
    #[test]
    fn cancelling_claude_desktop_without_snapshot_keeps_user_entries() {
        let root = claude_temp_root("cancel");
        let third = root.join("Claude-3p");
        let first = root.join("Claude");
        std::fs::create_dir_all(third.join("configLibrary")).unwrap();
        let ours = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let theirs = "99999999-8888-7777-6666-555555555555";
        std::fs::write(
            third.join("configLibrary/_meta.json"),
            format!(
                r#"{{"appliedId":"{ours}","entries":[{{"id":"{ours}","name":"HiMind","provider":"gateway","note":"himind-agent"}},{{"id":"{theirs}","name":"用户自有","provider":"bedrock"}}],"platform":"win32"}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            third.join("configLibrary").join(format!("{ours}.json")),
            r#"{"inferenceProvider":"gateway","inferenceGatewayBaseUrl":"https://ai.internal","inferenceGatewayApiKey":"secret"}"#,
        )
        .unwrap();
        std::fs::write(
            third.join("configLibrary").join(format!("{theirs}.json")),
            r#"{"bootstrapUrl":"https://user.example"}"#,
        )
        .unwrap();
        std::fs::write(
            third.join("claude_desktop_config.json"),
            r#"{"deploymentMode":"3p","mcpServers":{"himind-agent":{"command":"himind-agent"}}}"#,
        )
        .unwrap();

        let result = super::cancel_claude_desktop_in(&third, &first, None).unwrap();
        assert!(result.changed);
        assert_eq!(result.status, "cancelled");

        let meta: Value = serde_json::from_str(
            &std::fs::read_to_string(third.join("configLibrary/_meta.json")).unwrap(),
        )
        .unwrap();
        let entries = meta.get("entries").and_then(Value::as_array).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get("id").and_then(Value::as_str), Some(theirs));
        assert_eq!(
            meta.get("appliedId").and_then(Value::as_str),
            Some(theirs),
            "appliedId 必须回退到剩余条目"
        );
        assert!(!third
            .join("configLibrary")
            .join(format!("{ours}.json"))
            .exists());
        assert!(third
            .join("configLibrary")
            .join(format!("{theirs}.json"))
            .exists());

        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(third.join("claude_desktop_config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            config.get("deploymentMode").and_then(Value::as_str),
            Some("3p")
        );
        assert!(config
            .get("mcpServers")
            .and_then(|value| value.get("himind-agent"))
            .is_some());

        // 再取消一次：已无归属条目，应报 not_imported 且不报错。
        let again = super::cancel_claude_desktop_in(&third, &first, None).unwrap();
        assert!(!again.changed);
        assert_eq!(again.status, "not_imported");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 测试用临时目录：名字带进程 id 与毫秒，避免并行用例互相踩。
    fn claude_temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "himind-claude3p-{label}-{}-{}",
            std::process::id(),
            super::unix_now_millis()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
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
            None,
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

    /// 有导入快照时，取消导入必须把用户原有的模型与推理档位写回，并补回被覆盖的
    /// model_provider（这里是 cc-switch 的 custom），而不是留下 HiMind 的默认值。
    #[test]
    fn codex_strip_restores_user_values_from_snapshot() {
        let previous = r#"model_provider = "custom"
model = "deepseek-v4.1-flash"
model_catalog_json = "cc-switch-model-catalog.json"
model_reasoning_effort = "medium"

notify = ["codex-notify.exe"]

[mcp_servers.unityMCP]
command = "uvx.exe"
"#;
        let imported = r#"model = "himind-model"
model_provider = "himind"
preferred_auth_method = "apikey"
forced_login_method = "api"
model_reasoning_effort = "high"
model_catalog_json = "C:/Users/Admin/.codex/himind-models.json"

notify = ["codex-notify.exe"]

[model_providers.himind]
name = "HiMind"
base_url = "https://himind.andcrane.com/gateway/v1"
wire_api = "responses"

[mcp_servers.unityMCP]
command = "uvx.exe"
"#;
        let (updated, changed) = strip_codex_himind(
            imported,
            Path::new(r"C:\Users\Admin\.codex\himind-models.json"),
            Some(previous),
        )
        .unwrap();
        assert!(changed);
        assert!(updated.contains("model = \"deepseek-v4.1-flash\""));
        assert!(updated.contains("model_provider = \"custom\""));
        assert!(updated.contains("model_reasoning_effort = \"medium\""));
        assert!(updated.contains("model_catalog_json = \"cc-switch-model-catalog.json\""));
        assert!(!updated.contains("preferred_auth_method"));
        assert!(!updated.contains("forced_login_method"));
        assert!(!updated.contains("[model_providers.himind]"));
        assert!(updated.contains("[mcp_servers.unityMCP]"));
        // 还原后再次清理应幂等：没有任何改动。
        let (_, again) = strip_codex_himind(
            &updated,
            Path::new(r"C:\Users\Admin\.codex\himind-models.json"),
            Some(previous),
        )
        .unwrap();
        assert!(!again);
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

    /// WorkBuddy 自己会把空配置写成顶层数组 `[]`；导入不能因为根是数组就失败。
    #[test]
    fn merge_accepts_top_level_array_and_writes_it_back() {
        let (updated, count) = merge_workbuddy_models("[]", &credential(&["gpt-4.1"])).unwrap();
        assert_eq!(count, 1);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert!(root.is_array(), "数组根要按数组写回，不擅自改成对象");
        assert_eq!(root.as_array().unwrap().len(), 1);
        assert_eq!(root[0]["id"], "gpt-4.1");
    }

    #[test]
    fn merge_keeps_existing_entries_in_array_root() {
        let source = r#"[{"id":"personal","vendor":"Other","apiKey":"keep"}]"#;
        let (updated, _) = merge_workbuddy_models(source, &credential(&["gpt-4.1"])).unwrap();
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert!(root.is_array());
        assert!(root.as_array().unwrap().iter().any(|item| item["id"] == "personal"));
        assert!(root.as_array().unwrap().iter().any(|item| item["id"] == "gpt-4.1"));
    }

    #[test]
    fn remove_accepts_top_level_array_and_prunes_managed_entries() {
        let source = r#"[{"id":"personal","vendor":"Other"},{"id":"gpt-4.1","vendor":"HiMind"}]"#;
        let (updated, removed) = remove_workbuddy_models(source).unwrap();
        assert_eq!(removed, 1);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert!(root.is_array());
        assert_eq!(root.as_array().unwrap().len(), 1);
        assert_eq!(root[0]["id"], "personal");
    }

    #[test]
    fn managed_ids_read_top_level_array_root() {
        let empty: Value = serde_json::from_str("[]").unwrap();
        assert!(managed_workbuddy_model_ids(&empty).is_empty());
        let wired: Value = serde_json::from_str(r#"[{"id":"gpt-4.1","vendor":"HiMind"}]"#).unwrap();
        assert_eq!(managed_workbuddy_model_ids(&wired), vec!["gpt-4.1".to_string()]);
    }

    #[test]
    fn resolves_service_source_from_import_request() {
        let managed = AIProviderImportRequest {
            target: "codex".to_string(),
            service: String::new(),
            replace: false,
        };
        assert_eq!(managed.service_source(), "managed");

        let explicit_managed = AIProviderImportRequest {
            target: "codex".to_string(),
            service: "managed".to_string(),
            replace: false,
        };
        assert_eq!(explicit_managed.service_source(), "managed");

        let custom = AIProviderImportRequest {
            target: "codex".to_string(),
            service: "custom:my-gateway".to_string(),
            replace: false,
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
        let (updated, changed) = strip_kimi_code_himind(original, None).unwrap();
        assert!(changed);
        assert!(!updated.contains("himind"));
        assert!(updated.contains("[providers.other]"));
        assert!(updated.contains("[models.\"local/m1\"]"));
        assert!(!updated.contains("default_model"));
    }

    // Kimi Code 的取消：有导入前快照时，用户的 default_model 应还原，而不是被清空。
    #[test]
    fn strip_kimi_code_restores_previous_default_model() {
        let original = "default_model = \"existing/model\"\n[hooks]\nenabled = true\n";
        let updated =
            build_kimi_code_config(original, &credential(&["kimi-k3"]), &["kimi-k3".to_string()], "kimi-k3")
                .unwrap();
        assert!(updated.contains("default_model = \"himind/kimi-k3\""));
        let (restored, changed) = strip_kimi_code_himind(&updated, Some(original)).unwrap();
        assert!(changed);
        assert!(restored.contains("default_model = \"existing/model\""));
        assert!(!restored.contains("himind"));
        assert!(restored.contains("[hooks]"));
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
        let (updated, changed) = strip_qwen_code_himind(original, None).unwrap();
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

    // Qwen Code：导入写了裸模型名，取消时按快照还原默认模型，而不是留下悬空 model。
    #[test]
    fn strip_qwen_code_restores_previous_default_model() {
        let original = r#"{"model":{"name":"qwen3-max"},"mcpServers":{"s":{"command":"x"}}}"#;
        let updated = build_qwen_code_settings(
            original,
            &credential(&["qwen3-coder-plus"]),
            &["qwen3-coder-plus".to_string()],
            "qwen3-coder-plus",
        )
        .unwrap();
        let imported: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(imported["model"]["name"], "qwen3-coder-plus");
        let (restored, changed) = strip_qwen_code_himind(&updated, Some(original)).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&restored).unwrap();
        assert!(root["modelProviders"].get("himind").is_none());
        assert!(root["env"].get("HIMIND_API_KEY").is_none());
        assert!(root["providerProtocol"].get("himind").is_none());
        assert_eq!(root["model"]["name"], "qwen3-max");
        assert_eq!(root["mcpServers"]["s"]["command"], "x");
    }

    // 没有快照（旧簿记）时，取消仍要清掉我们写入的默认模型，避免悬空引用。
    #[test]
    fn strip_qwen_code_removes_default_model_without_snapshot() {
        let updated = build_qwen_code_settings(
            "",
            &credential(&["qwen3-coder-plus"]),
            &["qwen3-coder-plus".to_string()],
            "qwen3-coder-plus",
        )
        .unwrap();
        let (restored, changed) = strip_qwen_code_himind(&updated, None).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&restored).unwrap();
        assert!(root.get("model").is_none());
        assert!(root["modelProviders"].get("himind").is_none());
    }

    #[test]
    fn builds_opencode_config_with_provider_endpoint_and_models() {
        let credential = credential(&["deepseek-v4-flash", "deepseek-v4-pro"]);
        let models = [
            "deepseek-v4-flash".to_string(),
            "deepseek-v4-pro".to_string(),
        ];
        let updated = build_opencode_config("", &credential, &models).unwrap();
        let root: Value = serde_json::from_str(&updated).unwrap();
        let provider = &root["provider"]["himind"];
        assert_eq!(provider["name"], "HiMind");
        assert_eq!(provider["npm"], "@ai-sdk/openai");
        assert_eq!(provider["options"]["baseURL"], "https://ai.example.com/v1");
        assert_eq!(provider["options"]["apiKey"], "test-secret-key");
        assert_eq!(
            provider["models"]["deepseek-v4-flash"]["name"],
            "deepseek-v4-flash"
        );
        assert_eq!(
            provider["models"]["deepseek-v4-pro"]["name"],
            "deepseek-v4-pro"
        );
    }

    #[test]
    fn opencode_config_merge_preserves_existing_providers_and_mcp() {
        let original = r#"{
  // 用户自己的 OpenCode 配置
  "mcp": { "pencil": { "type": "local", "command": ["pencil", "mcp"] } },
  "provider": {
    "ark-codingplan": {
      "npm": "@ai-sdk/openai-compatible",
      "options": { "apiKey": "ark-key", "baseURL": "https://ark.example/api/coding" },
      "models": { "glm-5.2": { "name": "glm-5.2" } }
    }
  },
}"#;
        let credential = credential(&["himind/kimi-k3"]);
        let models = ["himind/kimi-k3".to_string()];
        let updated = build_opencode_config(original, &credential, &models).unwrap();
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(
            root["provider"]["ark-codingplan"]["options"]["apiKey"],
            "ark-key"
        );
        assert_eq!(root["mcp"]["pencil"]["type"], "local");
        assert_eq!(
            root["provider"]["himind"]["models"]["himind/kimi-k3"]["name"],
            "himind/kimi-k3"
        );
    }

    #[test]
    fn opencode_config_rejects_invalid_json_and_non_object_root() {
        let credential = credential(&["deepseek-v4-flash"]);
        let models = ["deepseek-v4-flash".to_string()];
        let error = build_opencode_config("{ not json", &credential, &models).unwrap_err();
        assert!(error
            .to_string()
            .contains("OpenCode opencode.json 格式无效"));
        let error = build_opencode_config("[1,2,3]", &credential, &models).unwrap_err();
        assert!(error.to_string().contains("顶层必须是 JSON 对象"));
        // 既有 provider 的同级供应商配置可被正常合并。
        assert!(
            build_opencode_config(r#"{"provider":{"ark":{"npm":"x"}}}"#, &credential, &models)
                .is_ok()
        );
    }

    #[test]
    fn opencode_npm_follows_protocols() {
        assert_eq!(
            opencode_npm_for_protocol("openai-chat").unwrap(),
            "@ai-sdk/openai-compatible"
        );
        assert_eq!(
            opencode_npm_for_protocol("openai-responses").unwrap(),
            "@ai-sdk/openai"
        );
        assert_eq!(
            opencode_npm_for_protocol("anthropic").unwrap(),
            "@ai-sdk/anthropic"
        );
        assert_eq!(opencode_npm_for_protocol("").unwrap(), "@ai-sdk/openai");
    }

    #[test]
    fn opencode_anthropic_provider_keeps_v1_suffix() {
        let mut credential = credential(&["claude-sonnet-4-5"]);
        credential.access.protocol = "anthropic".to_string();
        credential.access.base_url = "https://api.moonshot.cn/anthropic".to_string();
        let models = ["claude-sonnet-4-5".to_string()];
        let updated = build_opencode_config("", &credential, &models).unwrap();
        let root: Value = serde_json::from_str(&updated).unwrap();
        let provider = &root["provider"]["himind"];
        assert_eq!(provider["npm"], "@ai-sdk/anthropic");
        assert_eq!(
            provider["options"]["baseURL"],
            "https://api.moonshot.cn/anthropic/v1"
        );

        // 网关已给出带 /v1 的地址时不再叠加；带 /messages 的地址回到根地址。
        assert_eq!(
            opencode_anthropic_base_url("https://gateway.example/anthropic/v1").unwrap(),
            "https://gateway.example/anthropic/v1"
        );
        assert_eq!(
            opencode_anthropic_base_url("https://gateway.example/anthropic/v1/messages").unwrap(),
            "https://gateway.example/anthropic/v1"
        );
    }

    #[test]
    fn strip_opencode_only_removes_himind_provider() {
        let original =
            r#"{"provider":{"himind":{"npm":"@ai-sdk/openai"},"ark":{"npm":"x"}},"mcp":{"s":{}}}"#;
        let (updated, changed) = strip_opencode_himind(original).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert!(root["provider"].get("himind").is_none());
        assert_eq!(root["provider"]["ark"]["npm"], "x");
        assert!(root["mcp"]["s"].is_object());
        // 二次调用无 HiMind 条目，保持幂等且不再改写文件。
        let (_, changed_again) = strip_opencode_himind(&updated).unwrap();
        assert!(!changed_again);
        // provider 只含 HiMind 时整体移除空对象。
        let (emptied, removed) =
            strip_opencode_himind(r#"{"provider":{"himind":{"npm":"@ai-sdk/openai"}}}"#).unwrap();
        assert!(removed);
        assert!(serde_json::from_str::<Value>(&emptied)
            .unwrap()
            .get("provider")
            .is_none());
    }

    #[test]
    fn opencode_status_reads_himind_model_ids() {
        let path = std::env::temp_dir().join(format!(
            "himind-opencode-status-test-{}-{}.json",
            std::process::id(),
            super::unix_now_millis()
        ));
        std::fs::write(
            &path,
            r#"{"provider":{"himind":{"npm":"@ai-sdk/openai","models":{"deepseek-v4-flash":{"name":"deepseek-v4-flash"},"kimi-k3":{"name":"kimi-k3"}}}}}"#,
        )
        .unwrap();
        let mut models = super::read_opencode_himind_models(&path).unwrap();
        models.sort();
        assert_eq!(
            models.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["deepseek-v4-flash", "kimi-k3"]
        );
        assert!(super::opencode_himind_provider_present(&path));
        std::fs::remove_file(&path).ok();
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
        let (updated, changed) = strip_claude_himind(original, "Claude Code", None).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert!(root["env"].get(CLAUDE_BASE_URL_ENV).is_none());
        assert!(root["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
        assert!(root["env"].get("ANTHROPIC_MODEL").is_none());
        assert_eq!(root["env"]["OTHER"], "v");
        assert_eq!(root["permissions"]["allow"][0], "Bash(npm test *)");
    }

    /// 用户本来就配过 ANTHROPIC_* 时，取消导入必须还原原值，未配过的键才删除。
    #[test]
    fn strip_claude_restores_user_env_from_snapshot() {
        let previous = r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"user-token","OTHER":"v"}}"#;
        let imported = r#"{"env":{"ANTHROPIC_BASE_URL":"https://himind.example.com/gateway","ANTHROPIC_AUTH_TOKEN":"sk-1","ANTHROPIC_MODEL":"claude-sonnet-5","ANTHROPIC_CUSTOM_MODEL_OPTION":"claude-sonnet-5","OTHER":"v"}}"#;
        let (updated, changed) =
            strip_claude_himind(imported, "Claude Code", Some(previous)).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(root["env"]["ANTHROPIC_AUTH_TOKEN"], "user-token");
        assert_eq!(root["env"]["OTHER"], "v");
        assert!(root["env"].get(CLAUDE_BASE_URL_ENV).is_none());
        assert!(root["env"].get("ANTHROPIC_MODEL").is_none());
        let (_, again) = strip_claude_himind(&updated, "Claude Code", Some(previous)).unwrap();
        assert!(!again);
    }

    // Continue：首次在空文件上生成 config.yaml 时，name/version/schema 是必填字段，
    // 且 models 条目要对齐 config-yaml 的 modelSchema 形状。
    #[test]
    fn builds_continue_config_from_empty_file() {
        let cred = credential(&["model-a", "model-b"]);
        let content = build_continue_config(
            "",
            ProviderConfigFormat::Yaml,
            &cred,
            &["model-a".to_string(), "model-b".to_string()],
            "model-a",
        )
        .unwrap();
        let root: Value = serde_yaml::from_str(&content).unwrap();
        assert_eq!(root["name"], "Main Config");
        assert_eq!(root["schema"], "v1");
        let models = root["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["name"], "himind/model-a");
        assert_eq!(models[0]["provider"], "openai");
        assert_eq!(models[0]["model"], "model-a");
        assert_eq!(models[0]["apiBase"], "https://ai.example.com/v1");
        assert_eq!(models[0]["apiKey"], "test-secret-key");
        assert_eq!(models[0]["useResponsesApi"], true);
        assert_eq!(models[0]["roles"], json!(["chat", "edit", "apply"]));
        // 非首选模型只挂 chat 角色，避免多个模型同时被选为默认编辑器。
        assert_eq!(models[1]["roles"], json!(["chat"]));
    }

    // anthropic 协议走 anthropic provider，基址剥掉 /v1 且不带 responses 开关。
    #[test]
    fn continue_anthropic_protocol_uses_anthropic_provider() {
        let mut cred = credential(&["claude-x"]);
        cred.access.protocol = "anthropic".to_string();
        let content = build_continue_config(
            "",
            ProviderConfigFormat::Yaml,
            &cred,
            &["claude-x".to_string()],
            "claude-x",
        )
        .unwrap();
        let root: Value = serde_yaml::from_str(&content).unwrap();
        let entry = &root["models"][0];
        assert_eq!(entry["provider"], "anthropic");
        assert_eq!(entry["apiBase"], "https://ai.example.com");
        assert!(entry.get("useResponsesApi").is_none());
    }

    // 保留式合并：既有非 himind 模型与其它顶层键必须原样保留，重复导入不产生重复条目。
    #[test]
    fn continue_build_preserves_existing_models_and_is_idempotent() {
        let original = "name: Main Config\nversion: 0.0.1\nschema: v1\nextras:\n  keep: true\nmodels:\n  - name: gpt-4o\n    provider: openai\n    model: gpt-4o\n";
        let cred = credential(&["model-a"]);
        let updated = build_continue_config(
            original,
            ProviderConfigFormat::Yaml,
            &cred,
            &["model-a".to_string()],
            "model-a",
        )
        .unwrap();
        let root: Value = serde_yaml::from_str(&updated).unwrap();
        assert_eq!(root["extras"]["keep"], true);
        let models = root["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert!(models.iter().any(|entry| entry["name"] == "gpt-4o"));
        assert!(models.iter().any(|entry| entry["name"] == "himind/model-a"));

        let rerun = build_continue_config(
            &updated,
            ProviderConfigFormat::Yaml,
            &cred,
            &["model-a".to_string()],
            "model-a",
        )
        .unwrap();
        let again: Value = serde_yaml::from_str(&rerun).unwrap();
        assert_eq!(again["models"].as_array().unwrap().len(), 2);
        assert_eq!(continue_himind_models(&again), vec!["model-a".to_string()]);
    }

    #[test]
    fn strips_only_himind_entries_from_continue_config() {
        let original = "name: Main Config\nversion: 0.0.1\nschema: v1\nmodels:\n  - name: gpt-4o\n    provider: openai\n    model: gpt-4o\n  - name: himind/model-a\n    provider: openai\n    model: model-a\n";
        let (updated, changed) =
            strip_continue_himind(original, ProviderConfigFormat::Yaml).unwrap();
        assert!(changed);
        let root: Value = serde_yaml::from_str(&updated).unwrap();
        let models = root["models"].as_array().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["name"], "gpt-4o");
        // 幂等：没有 himind 条目时不改写文件。
        let (_, again) = strip_continue_himind(&updated, ProviderConfigFormat::Yaml).unwrap();
        assert!(!again);
    }

    // Aider：顶层标量端点/密钥/默认模型，加 himind/<model>:openai/<model> 别名。
    #[test]
    fn builds_aider_conf_with_endpoint_and_alias() {
        let cred = credential(&["model-a", "model-b"]);
        let content = build_aider_config(
            "",
            ProviderConfigFormat::Yaml,
            &cred,
            &["model-a".to_string(), "model-b".to_string()],
            "model-b",
        )
        .unwrap();
        let root: Value = serde_yaml::from_str(&content).unwrap();
        assert_eq!(root["openai-api-base"], "https://ai.example.com/v1");
        assert_eq!(root["openai-api-key"], "test-secret-key");
        assert_eq!(root["model"], "himind/model-b");
        let aliases = root["alias"].as_array().unwrap();
        assert!(aliases
            .iter()
            .any(|item| item.as_str() == Some("himind/model-a:openai/model-a")));
        assert!(aliases
            .iter()
            .any(|item| item.as_str() == Some("himind/model-b:openai/model-b")));
        // 默认模型来自 model 标量，别名补齐其余模型。
        assert_eq!(
            aider_himind_models(&root),
            vec!["model-b".to_string(), "model-a".to_string()]
        );
    }

    #[test]
    fn strips_aider_himind_entries_and_preserves_user_keys() {
        let original = "openai-api-base: https://ai.example.com/v1\nopenai-api-key: test-secret-key\nmodel: himind/model-a\nalias:\n  - himind/model-a:openai/model-a\n  - fast:gpt-4o\nweak-model: gpt-4o\n";
        let (updated, changed) =
            strip_aider_himind(original, ProviderConfigFormat::Yaml, None).unwrap();
        assert!(changed);
        let root: Value = serde_yaml::from_str(&updated).unwrap();
        assert!(root.get("openai-api-base").is_none());
        assert!(root.get("openai-api-key").is_none());
        assert!(root.get("model").is_none());
        assert_eq!(root["weak-model"], "gpt-4o");
        let aliases = root["alias"].as_array().unwrap();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0], "fast:gpt-4o");
        let (_, again) = strip_aider_himind(&updated, ProviderConfigFormat::Yaml, None).unwrap();
        assert!(!again);
    }

    /// Aider 只支持单来源：有快照时取消导入应把用户原来的端点、密钥与默认模型
    /// 写回，而不是删除。
    #[test]
    fn strips_aider_restores_user_values_from_snapshot() {
        let previous =
            "openai-api-base: https://user.example/v1\nopenai-api-key: user-key\nmodel: gpt-4o\n";
        let imported = "openai-api-base: https://ai.example.com/v1\nopenai-api-key: test-secret-key\nmodel: himind/model-a\nalias:\n  - himind/model-a:openai/model-a\nweak-model: gpt-4o\n";
        let (updated, changed) =
            strip_aider_himind(imported, ProviderConfigFormat::Yaml, Some(previous)).unwrap();
        assert!(changed);
        let root: Value = serde_yaml::from_str(&updated).unwrap();
        assert_eq!(root["openai-api-base"], "https://user.example/v1");
        assert_eq!(root["openai-api-key"], "user-key");
        assert_eq!(root["model"], "gpt-4o");
        assert_eq!(root["weak-model"], "gpt-4o");
        assert!(root.get("alias").is_none());
        let (_, again) =
            strip_aider_himind(&updated, ProviderConfigFormat::Yaml, Some(previous)).unwrap();
        assert!(!again);
    }

    // 每个声明式目标都必须给出主配置，状态与卸载判定都按第 0 项走。
    #[test]
    fn declarative_targets_declare_a_primary_config_file() {
        for definition in PROVIDER_TARGETS {
            assert!(
                !(definition.config_files)().is_empty(),
                "{} 未声明配置文件",
                definition.id
            );
        }
        // ZCode 是唯一写两份配置的目标：config.json 给旧版，规则文件给 3.14+。
        assert_eq!((PROVIDER_TARGETS[5].config_files)().len(), 2);
    }

    // Crush：providers.himind 带端点与密钥，模型清单填 id 与窗口兜底，
    // large/small 两个用途槽位都指向 himind 的首选模型。
    #[test]
    fn builds_crush_config_with_provider_and_model_slots() {
        let cred = credential(&["model-a", "model-b"]);
        let models = vec!["model-a".to_string(), "model-b".to_string()];
        let content =
            build_crush_config("", ProviderConfigFormat::Json, &cred, &models, "model-b").unwrap();
        let root: Value = serde_json::from_str(&content).unwrap();
        let provider = &root["providers"]["himind"];
        assert_eq!(provider["type"], "openai");
        assert_eq!(provider["base_url"], "https://ai.example.com/v1");
        assert_eq!(provider["api_key"], "test-secret-key");
        assert_eq!(provider["models"][0]["id"], "model-a");
        assert_eq!(provider["models"][0]["name"], "model-a");
        assert_eq!(provider["models"][0]["context_window"], 200000);
        assert_eq!(provider["models"][0]["default_max_tokens"], 16384);
        assert_eq!(root["models"]["large"]["provider"], "himind");
        assert_eq!(root["models"]["large"]["model"], "model-b");
        assert_eq!(root["models"]["small"]["provider"], "himind");
        assert_eq!(
            crush_himind_models(&root),
            vec!["model-a".to_string(), "model-b".to_string()]
        );
    }

    // Crush 的取消：只清 HiMind 的 provider 与仍指向它的槽位，别人的模型不动。
    // 导入前被槽位占用的模型不还原，回到导入前的状态要看备份文件。
    #[test]
    fn strips_crush_himind_and_keeps_other_providers() {
        let original = r#"{"providers":{"anthropic":{"type":"anthropic","name":"anthropic"}},"models":{"large":{"provider":"anthropic","model":"claude"}}}"#;
        let cred = credential(&["model-a"]);
        let content = build_crush_config(
            original,
            ProviderConfigFormat::Json,
            &cred,
            &["model-a".to_string()],
            "model-a",
        )
        .unwrap();
        let root: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(root["providers"]["anthropic"]["name"], "anthropic");
        let (updated, changed) =
            strip_crush_himind(&content, ProviderConfigFormat::Json, None).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert!(root["providers"].get("himind").is_none());
        assert_eq!(root["providers"]["anthropic"]["name"], "anthropic");
        assert!(root["models"].get("large").is_none());
    }

    #[test]
    fn strips_crush_himind_keeps_slots_pointing_elsewhere() {
        let original = r#"{"providers":{"himind":{"type":"openai"}},"models":{"large":{"provider":"anthropic","model":"claude"},"small":{"provider":"himind","model":"model-a"}}}"#;
        let (updated, changed) = strip_crush_himind(original, ProviderConfigFormat::Json, None).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(root["models"]["large"]["provider"], "anthropic");
        assert!(root["models"].get("small").is_none());
        let (_, again) = strip_crush_himind(&updated, ProviderConfigFormat::Json, None).unwrap();
        assert!(!again);
    }

    // Crush 的取消：有导入前快照时，large/small 槽位连同用户自己的推理档位一起还原，
    // 不再把用户原有槽位永久丢失。
    #[test]
    fn strips_crush_himind_restores_previous_slot_snapshot() {
        let original = r#"{"providers":{"anthropic":{"type":"anthropic"}},"models":{"large":{"provider":"anthropic","model":"claude-sonnet","reasoning_effort":"high"},"small":{"provider":"anthropic","model":"claude-haiku"}}}"#;
        let content = build_crush_config(
            original,
            ProviderConfigFormat::Json,
            &credential(&["model-a"]),
            &["model-a".to_string()],
            "model-a",
        )
        .unwrap();
        let imported: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(imported["models"]["large"]["provider"], "himind");
        assert_eq!(imported["models"]["large"]["reasoning_effort"], "high");
        let (updated, changed) =
            strip_crush_himind(&content, ProviderConfigFormat::Json, Some(original)).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert!(root["providers"].get("himind").is_none());
        assert_eq!(root["models"]["large"]["provider"], "anthropic");
        assert_eq!(root["models"]["large"]["model"], "claude-sonnet");
        assert_eq!(root["models"]["large"]["reasoning_effort"], "high");
        assert_eq!(root["models"]["small"]["model"], "claude-haiku");
    }

    // Qoder：providers.himind 走 openai 协议，默认模型写成 `<id>/<model>`。
    #[test]
    fn builds_qoder_config_with_default_model() {
        let cred = credential(&["model-a", "model-b"]);
        let models = vec!["model-a".to_string(), "model-b".to_string()];
        let content =
            build_qoder_config("", ProviderConfigFormat::Json, &cred, &models, "model-b").unwrap();
        let root: Value = serde_json::from_str(&content).unwrap();
        let provider = &root["providers"]["himind"];
        assert_eq!(provider["protocol"], "openai");
        assert_eq!(provider["baseUrl"], "https://ai.example.com/v1");
        assert_eq!(provider["apiKey"], "test-secret-key");
        assert_eq!(provider["model"], "model-b");
        assert_eq!(provider["models"][0]["model"], "model-a");
        assert_eq!(provider["models"][0]["capabilities"]["tools"], true);
        assert_eq!(root["model"]["name"], "himind/model-b");
        assert_eq!(
            qoder_himind_models(&root),
            vec!["model-a".to_string(), "model-b".to_string()]
        );
    }

    // Qoder 的取消：清 HiMind 的 provider 与默认模型，用户自己的推理档位保持原样。
    #[test]
    fn strips_qoder_himind_and_keeps_reasoning_effort() {
        let original = r#"{"model":{"name":"himind/model-a","reasoningEffort":"high"},"providers":{"himind":{"protocol":"openai"},"other":{"protocol":"openai"}}}"#;
        let (updated, changed) = strip_qoder_himind(original, ProviderConfigFormat::Json, None).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert!(root["providers"].get("himind").is_none());
        assert_eq!(root["providers"]["other"]["protocol"], "openai");
        assert!(root["model"].get("name").is_none());
        assert_eq!(root["model"]["reasoningEffort"], "high");
    }

    #[test]
    fn strips_qoder_himind_keeps_foreign_default_model() {
        let original = r#"{"model":{"name":"other/model"},"providers":{"himind":{}}}"#;
        let (updated, changed) = strip_qoder_himind(original, ProviderConfigFormat::Json, None).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(root["model"]["name"], "other/model");
        assert!(root["providers"].get("himind").is_none());
    }

    // Qoder 的取消：有导入前快照时，默认模型还原到用户原值，而不是清成空。
    #[test]
    fn strips_qoder_himind_restores_previous_default_model() {
        let original =
            r#"{"model":{"name":"other/model","reasoningEffort":"high"},"providers":{"other":{"protocol":"openai"}}}"#;
        let content = build_qoder_config(
            original,
            ProviderConfigFormat::Json,
            &credential(&["model-a"]),
            &["model-a".to_string()],
            "model-a",
        )
        .unwrap();
        let (updated, changed) =
            strip_qoder_himind(&content, ProviderConfigFormat::Json, Some(original)).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert!(root["providers"].get("himind").is_none());
        assert_eq!(root["model"]["name"], "other/model");
        assert_eq!(root["model"]["reasoningEffort"], "high");
    }

    // ZCode：config.json 的 provider kind 固定 anthropic，baseURL 不带 /v1；
    // provider_config.json 里的 provider 规则与模型规则按 providerId 归组。
    #[test]
    fn builds_zcode_provider_and_rules() {
        let mut cred = credential(&["claude-x"]);
        cred.access.protocol = "anthropic".to_string();
        let models = vec!["claude-x".to_string()];
        let config = build_zcode_config("", ProviderConfigFormat::Json, &cred, &models).unwrap();
        let root: Value = serde_json::from_str(&config).unwrap();
        let provider = &root["provider"]["himind"];
        assert_eq!(provider["kind"], "anthropic");
        assert_eq!(provider["enabled"], true);
        assert_eq!(provider["source"], "custom");
        assert_eq!(provider["options"]["baseURL"], "https://ai.example.com");
        assert_eq!(provider["options"]["apiKey"], "test-secret-key");
        assert_eq!(provider["models"]["claude-x"]["limit"]["context"], 200000);
        // 输出上限没有可信来源，留空由 ZCode 按缺省处理。
        assert!(provider["models"]["claude-x"]["limit"]
            .get("output")
            .is_none());
        assert_eq!(zcode_himind_models(&root), models);

        let rules =
            build_zcode_provider_rules("", ProviderConfigFormat::Json, &cred, &models).unwrap();
        let root: Value = serde_json::from_str(&rules).unwrap();
        assert_eq!(root["schemaVersion"], 1);
        let rule = &root["config"]["providerConfigRules"]["providerRules"][0];
        assert_eq!(rule["providerId"], "himind");
        assert_eq!(rule["providerName"], "himind");
        assert_eq!(rule["enabled"], true);
        assert_eq!(rule["config"]["group"], "standard-personal");
        assert_eq!(rule["config"]["access"]["type"], "api-key");
        assert_eq!(rule["config"]["access"]["apiKey"], "test-secret-key");
        assert_eq!(rule["config"]["api"]["type"], "anthropic-messages");
        assert_eq!(rule["config"]["api"]["baseUrl"], "https://ai.example.com");
        assert_eq!(rule["config"]["personalModelIds"], json!(["claude-x"]));
        assert_eq!(rule["config"]["modelOrder"], json!(["claude-x"]));
        let model_rule = &root["config"]["modelConfigRules"]["providerModelRules"][0];
        assert_eq!(model_rule["providerId"], "himind");
        assert_eq!(model_rule["modelId"], "claude-x");
        assert_eq!(model_rule["config"]["properties"]["contextWindow"], 200000);
        assert_eq!(
            model_rule["config"]["properties"]["inputFormat"]["supportsImage"],
            false
        );
    }

    // 重新导入要保留其他 provider 的规则，也不覆盖用户在 ZCode 里手工设过的模型；
    // 用户在 ZCode 里关掉的 provider 不被重新打开。
    #[test]
    fn zcode_rules_preserve_other_providers_and_manual_models() {
        let original = r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[{"providerId":"other"},{"providerId":"himind","enabled":false}]},"modelConfigRules":{"providerModelRules":[{"providerId":"other","modelId":"x"}],"manualProviderModelRules":[{"providerId":"other","modelId":"y"}]}}}"#;
        let mut cred = credential(&["claude-x"]);
        cred.access.protocol = "anthropic".to_string();
        let content = build_zcode_provider_rules(
            original,
            ProviderConfigFormat::Json,
            &cred,
            &["claude-x".to_string()],
        )
        .unwrap();
        let root: Value = serde_json::from_str(&content).unwrap();
        let rules = root["config"]["providerConfigRules"]["providerRules"]
            .as_array()
            .unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0]["providerId"], "other");
        assert_eq!(rules[1]["providerId"], "himind");
        assert_eq!(rules[1]["enabled"], false);
        let model_rules = root["config"]["modelConfigRules"]["providerModelRules"]
            .as_array()
            .unwrap();
        assert_eq!(model_rules.len(), 2);
        assert_eq!(model_rules[0]["providerId"], "other");
        assert_eq!(model_rules[1]["modelId"], "claude-x");
        let manual = root["config"]["modelConfigRules"]["manualProviderModelRules"]
            .as_array()
            .unwrap();
        assert_eq!(manual.len(), 1);
        assert_eq!(manual[0]["providerId"], "other");
    }

    #[test]
    fn strips_zcode_rules_and_keeps_other_providers() {
        let original = r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[{"providerId":"himind"},{"providerId":"other"}]},"modelConfigRules":{"providerModelRules":[{"providerId":"himind","modelId":"claude-x"},{"providerId":"other","modelId":"x"}],"manualProviderModelRules":[]}}}"#;
        let (updated, changed) =
            strip_zcode_rules_himind(original, ProviderConfigFormat::Json).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        let rules = root["config"]["providerConfigRules"]["providerRules"]
            .as_array()
            .unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["providerId"], "other");
        let model_rules = root["config"]["modelConfigRules"]["providerModelRules"]
            .as_array()
            .unwrap();
        assert_eq!(model_rules.len(), 1);
        assert_eq!(model_rules[0]["providerId"], "other");
        let (_, again) = strip_zcode_rules_himind(&updated, ProviderConfigFormat::Json).unwrap();
        assert!(!again);
    }

    #[test]
    fn strips_zcode_provider_from_config_json() {
        let original = r#"{"provider":{"himind":{"kind":"anthropic"},"other":{"kind":"openai"}},"theme":"dark"}"#;
        let (updated, changed) = strip_zcode_himind(original, ProviderConfigFormat::Json).unwrap();
        assert!(changed);
        let root: Value = serde_json::from_str(&updated).unwrap();
        assert!(root["provider"].get("himind").is_none());
        assert_eq!(root["provider"]["other"]["kind"], "openai");
        assert_eq!(root["theme"], "dark");
        let (_, again) = strip_zcode_himind(&updated, ProviderConfigFormat::Json).unwrap();
        assert!(!again);
    }

    // 重复导入同一来源时配置逐字节不变，应判定为 file_unchanged（备份=写前内容）。
    #[test]
    fn import_noop_detected_when_backup_matches_written_file() {
        let root = std::env::temp_dir().join(format!(
            "himind-noop-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("config.json");
        let backup = root.join("config.json.himind-backup-1.bak");
        std::fs::write(&config, b"{\"a\":1}\n").unwrap();
        std::fs::write(&backup, b"{\"a\":1}\n").unwrap();
        let result = super::AIProviderImportResult {
            config_path: config.to_string_lossy().to_string(),
            backup_path: backup.to_string_lossy().to_string(),
            ..Default::default()
        };
        assert!(super::import_was_noop(&result));
        std::fs::write(&config, b"{\"a\":2}\n").unwrap();
        assert!(!super::import_was_noop(&result));
        let _ = std::fs::remove_dir_all(&root);
    }

    // 线格式守卫：ZCode 只吃 Anthropic，Continue/Aider/Crush/Qoder 只吃 OpenAI 兼容。
    #[test]
    fn protocol_guards_match_each_client_wire_format() {
        let openai = credential(&["model-a"]);
        let mut anthropic = credential(&["claude-x"]);
        anthropic.access.protocol = "anthropic".to_string();
        assert!(ensure_anthropic_compatible(&openai, "ZCode").is_err());
        assert!(ensure_anthropic_compatible(&anthropic, "ZCode").is_ok());
        assert!(ensure_openai_compatible(&anthropic, "Crush").is_err());
        assert!(ensure_openai_compatible(&openai, "Crush").is_ok());
    }

    // 能力判定：标签不匹配时，探针确认端点支持目标协议即放行。
    #[test]
    fn protocol_gate_allows_when_probe_confirms_capability() {
        assert!(protocol_gate(true, ProbeVerdict::Unavailable));
        assert!(protocol_gate(true, ProbeVerdict::Unsupported));
        assert!(protocol_gate(false, ProbeVerdict::Supported));
        assert!(!protocol_gate(false, ProbeVerdict::Unsupported));
        assert!(!protocol_gate(false, ProbeVerdict::Unavailable));
    }

    #[test]
    fn label_satisfies_protocol_treats_both_openai_wire_formats_as_openai() {
        let mut credential = credential(&["model-a"]);
        credential.access.protocol = "openai-chat".to_string();
        assert!(label_satisfies_protocol(&credential, super::RequiredProtocol::OpenAi));
        assert!(!label_satisfies_protocol(&credential, super::RequiredProtocol::Anthropic));
        credential.access.protocol = "openai-responses".to_string();
        assert!(label_satisfies_protocol(&credential, super::RequiredProtocol::OpenAi));
        credential.access.protocol = "anthropic".to_string();
        assert!(label_satisfies_protocol(&credential, super::RequiredProtocol::Anthropic));
        assert!(!label_satisfies_protocol(&credential, super::RequiredProtocol::OpenAi));
    }

    // Anthropic 探针判读：2xx 成立；404 不成立；错误信封与 anthropic-* 头也算讲该协议。
    #[test]
    fn classify_anthropic_probe_recognizes_protocol_signals() {
        assert_eq!(
            classify_anthropic_probe(200, &[], "{}"),
            ProbeVerdict::Supported
        );
        assert_eq!(
            classify_anthropic_probe(404, &[], "not found"),
            ProbeVerdict::Unsupported
        );
        assert_eq!(
            classify_anthropic_probe(
                400,
                &[],
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#
            ),
            ProbeVerdict::Supported
        );
        assert_eq!(
            classify_anthropic_probe(400, &["anthropic-ratelimit-requests-limit".to_string()], "{}"),
            ProbeVerdict::Supported
        );
        assert_eq!(
            classify_anthropic_probe(500, &[], "oops"),
            ProbeVerdict::Unsupported
        );
    }

    // OpenAI 探针判读：只有 /models 2xx 且带 data/list 才成立。
    #[test]
    fn classify_openai_probe_requires_model_list_shape() {
        assert_eq!(
            classify_openai_probe(200, r#"{"object":"list","data":[{"id":"m1"}]}"#),
            ProbeVerdict::Supported
        );
        assert_eq!(
            classify_openai_probe(200, r#"{"data":[]}"#),
            ProbeVerdict::Supported
        );
        assert_eq!(
            classify_openai_probe(200, "<html>ok</html>"),
            ProbeVerdict::Unsupported
        );
        assert_eq!(classify_openai_probe(401, "{}"), ProbeVerdict::Unsupported);
        assert_eq!(classify_openai_probe(404, "not found"), ProbeVerdict::Unsupported);
    }

    #[test]
    fn provider_config_format_follows_extension() {
        assert!(matches!(
            super::provider_config_format(Path::new("/tmp/config.json")),
            ProviderConfigFormat::Json
        ));
        assert!(matches!(
            super::provider_config_format(Path::new("/tmp/config.yaml")),
            ProviderConfigFormat::Yaml
        ));
        assert!(matches!(
            super::provider_config_format(Path::new("/tmp/.aider.conf.yml")),
            ProviderConfigFormat::Yaml
        ));
    }
}
