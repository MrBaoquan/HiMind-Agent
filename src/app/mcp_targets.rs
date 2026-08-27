//! MCP registration targets.
//!
//! Adapters deliberately keep the existing client-specific writers behind a
//! small common surface. Adding a client therefore extends this module rather
//! than adding another UI/CLI-specific configuration path.

use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use super::mcp_registry::{McpRegistrationAction, McpRegistrationPlan};
pub(crate) use super::mcp_target_types::{
    McpTargetBatchFailure, McpTargetBatchResult, McpTargetDescriptor, McpTargetOperationResult,
};
use crate::Options;

pub(crate) const DSH_TARGET_ID: &str = "himind-ai";

#[derive(Clone, Debug)]
struct JsonTargetDefinition {
    id: &'static str,
    name: &'static str,
    executable: &'static str,
    path: PathBuf,
    servers_key: &'static str,
    config_format: &'static str,
    layout: JsonTargetLayout,
    detect_paths: Vec<PathBuf>,
    supports_auto_configure: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JsonTargetLayout {
    Standard,
    VsCode,
    OpenCode,
    NestedMcp,
}

pub(crate) fn list(options: &Options) -> Result<Vec<McpTargetDescriptor>, Box<dyn Error>> {
    let launch = super::ai_clients::launch_spec(options)?;
    let mut targets = super::ai_clients::targets(options)?;
    let home = home_directory();
    for definition in json_target_definitions(&home) {
        targets.push(json_target_status(
            &definition,
            &launch.command,
            &launch.args,
        ));
    }
    targets.push(dsh_target(options));
    targets.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(targets)
}

pub(crate) fn inspect(options: &Options, target_id: &str) -> Result<Value, Box<dyn Error>> {
    let target = find_target(options, target_id)?;
    Ok(serde_json::to_value(target)?)
}

pub(crate) fn plan(
    options: &Options,
    target_id: &str,
) -> Result<McpRegistrationPlan, Box<dyn Error>> {
    let target = find_target(options, target_id)?;
    if target.id == DSH_TARGET_ID {
        return Ok(McpRegistrationPlan {
            target_id: target.id,
            action: McpRegistrationAction::Noop,
            write_required: false,
            backup_required: false,
            restart_required: true,
            configured_server_id: super::mcp_registry::AGENT_SERVER_ID.to_string(),
            warnings: vec![
                "HiMind AI 使用 Agent 管理的会话覆盖层，保存 MCP 设置后会在下一次会话生效。"
                    .to_string(),
            ],
        });
    }
    let action = if !target.supports_auto_configure {
        McpRegistrationAction::Unsupported
    } else {
        match target.state.as_str() {
            "configured" => McpRegistrationAction::Noop,
            "needs_repair" | "invalid_config" => McpRegistrationAction::Update,
            "not_configured" => McpRegistrationAction::Create,
            _ => McpRegistrationAction::Unsupported,
        }
    };
    let mut warnings = Vec::new();
    if !target.detected {
        warnings.push("未检测到客户端程序，仍可写入配置，客户端安装后即可加载。".to_string());
    }
    if target.state == "invalid_config" {
        warnings.push("配置文件无法解析；应用前需要显式允许重置无效配置。".to_string());
    }
    let write_required = !matches!(action, McpRegistrationAction::Noop);
    Ok(McpRegistrationPlan {
        target_id: target.id,
        action,
        write_required,
        backup_required: write_required,
        restart_required: true,
        configured_server_id: super::mcp_registry::AGENT_SERVER_ID.to_string(),
        warnings,
    })
}

pub(crate) fn apply(
    options: &Options,
    target_id: &str,
    reset_invalid: bool,
) -> Result<McpTargetOperationResult, Box<dyn Error>> {
    let target = find_target(options, target_id)?;
    if target.id == DSH_TARGET_ID {
        super::ui::stop_builtin_ai_process();
        return Ok(McpTargetOperationResult {
            target,
            changed: false,
            backup_path: String::new(),
            message: "HiMind AI 会话将在下次启动时重新加载 Agent MCP 配置".to_string(),
        });
    }
    let result = match target.id.as_str() {
        "codex" | "github-copilot" | "workbuddy" => {
            super::ai_clients::configure(options, &target.id, reset_invalid)?
        }
        _ => apply_json_target(options, &target, reset_invalid)?,
    };
    Ok(result)
}

pub(crate) fn apply_all(
    options: &Options,
    detected_only: bool,
    reset_invalid: bool,
) -> Result<McpTargetBatchResult, Box<dyn Error>> {
    let targets = list(options)?;
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut skipped_target_ids = Vec::new();
    for target in targets {
        if target.id == DSH_TARGET_ID
            || target.state == "configured"
            || !target.supports_auto_configure
            || (detected_only && !target.detected)
        {
            skipped_target_ids.push(target.id);
            continue;
        }
        match apply(options, &target.id, reset_invalid) {
            Ok(result) => results.push(result),
            Err(error) => failures.push(McpTargetBatchFailure {
                target_id: target.id,
                target_name: target.name,
                error: error.to_string(),
            }),
        }
    }
    Ok(McpTargetBatchResult {
        results,
        failures,
        skipped_target_ids,
    })
}

pub(crate) fn remove(
    options: &Options,
    target_id: &str,
) -> Result<McpTargetOperationResult, Box<dyn Error>> {
    let target = find_target(options, target_id)?;
    if target.id == DSH_TARGET_ID {
        return Err("HiMind AI 的 Agent MCP 桥接由会话管理，不能从 DSH 目标中移除".into());
    }
    if matches!(target.id.as_str(), "codex" | "github-copilot" | "workbuddy") {
        return super::ai_clients::remove_configuration(options, &target.id);
    }
    remove_json_target(options, &target)
}

pub(crate) fn remove_all(
    options: &Options,
    detected_only: bool,
) -> Result<McpTargetBatchResult, Box<dyn Error>> {
    let targets = list(options)?;
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut skipped_target_ids = Vec::new();
    for target in targets {
        if target.id == DSH_TARGET_ID
            || (detected_only && !target.detected)
            || !matches!(
                target.state.as_str(),
                "configured" | "needs_repair" | "invalid_config"
            )
        {
            skipped_target_ids.push(target.id);
            continue;
        }
        match remove(options, &target.id) {
            Ok(result) => results.push(result),
            Err(error) => failures.push(McpTargetBatchFailure {
                target_id: target.id,
                target_name: target.name,
                error: error.to_string(),
            }),
        }
    }
    Ok(McpTargetBatchResult {
        results,
        failures,
        skipped_target_ids,
    })
}

fn find_target(options: &Options, target_id: &str) -> Result<McpTargetDescriptor, Box<dyn Error>> {
    list(options)?
        .into_iter()
        .find(|target| target.id == target_id.trim())
        .ok_or_else(|| format!("unsupported MCP target: {target_id}").into())
}

fn dsh_target(options: &Options) -> McpTargetDescriptor {
    let path = super::mcp_registry::settings_path(&options.state_path);
    let state = if path.is_file() { "managed" } else { "ready" };
    McpTargetDescriptor {
        id: DSH_TARGET_ID.to_string(),
        name: "HiMind AI".to_string(),
        kind: "dsh".to_string(),
        detected: true,
        detection_message: "由 HiMind AI 会话覆盖层管理".to_string(),
        config_path: path.to_string_lossy().to_string(),
        config_directory: path
            .parent()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
        config_format: "DSH overlay + JSON".to_string(),
        state: state.to_string(),
        supported_transports: vec!["stdio".to_string(), "streamable-http".to_string()],
        supports_auto_configure: true,
        supports_skills: true,
        skill_client_id: DSH_TARGET_ID.to_string(),
        skill_client_name: "HiMind AI".to_string(),
        restart_required: true,
        manual_snippet: String::new(),
        config_preview: String::new(),
        error: String::new(),
    }
}

fn home_directory() -> PathBuf {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn json_target_definitions(home: &Path) -> Vec<JsonTargetDefinition> {
    let app_data = env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData").join("Roaming"));
    let local_app_data = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData").join("Local"));
    let standard = JsonTargetLayout::Standard;
    let vscode = JsonTargetLayout::VsCode;
    let opencode = JsonTargetLayout::OpenCode;
    let nested_mcp = JsonTargetLayout::NestedMcp;
    let target = |id: &'static str,
                  name: &'static str,
                  executable: &'static str,
                  path: PathBuf,
                  servers_key: &'static str,
                  config_format: &'static str,
                  layout: JsonTargetLayout,
                  detect_paths: Vec<PathBuf>,
                  supports_auto_configure: bool| JsonTargetDefinition {
        id,
        name,
        executable,
        path,
        servers_key,
        config_format,
        layout,
        detect_paths,
        supports_auto_configure,
    };
    vec![
        target(
            "claude-code",
            "Claude Code",
            "claude",
            home.join(".claude.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![home.join(".claude")],
            true,
        ),
        target(
            "claude-desktop",
            "Claude Desktop",
            "claude",
            app_data.join("Claude").join("claude_desktop_config.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![app_data.join("Claude")],
            true,
        ),
        target(
            "vscode",
            "VS Code GitHub Copilot",
            "code",
            app_data.join("Code").join("User").join("mcp.json"),
            "servers",
            "JSON",
            vscode,
            vec![app_data.join("Code")],
            true,
        ),
        target(
            "vscode-insiders",
            "VS Code Insiders GitHub Copilot",
            "code-insiders",
            app_data
                .join("Code - Insiders")
                .join("User")
                .join("mcp.json"),
            "servers",
            "JSON",
            vscode,
            vec![app_data.join("Code - Insiders")],
            true,
        ),
        target(
            "cursor",
            "Cursor",
            "cursor",
            home.join(".cursor").join("mcp.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![home.join(".cursor")],
            true,
        ),
        target(
            "windsurf",
            "Windsurf",
            "windsurf",
            home.join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![home.join(".codeium").join("windsurf")],
            true,
        ),
        target(
            "cline",
            "Cline",
            "code",
            app_data
                .join("Code")
                .join("User")
                .join("globalStorage")
                .join("saoudrizwan.claude-dev")
                .join("settings")
                .join("cline_mcp_settings.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![app_data
                .join("Code")
                .join("User")
                .join("globalStorage")
                .join("saoudrizwan.claude-dev")],
            true,
        ),
        target(
            "codebuddy-cli",
            "CodeBuddy CLI",
            "codebuddy",
            home.join(".codebuddy.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![],
            true,
        ),
        target(
            "qoder",
            "Qoder",
            "qoder",
            home.join(".qoder").join("settings.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![home.join(".qoder")],
            true,
        ),
        target(
            "zcode",
            "ZCode",
            "zcode",
            home.join(".zcode").join("cli").join("config.json"),
            "servers",
            "JSON",
            nested_mcp,
            vec![home.join(".zcode")],
            true,
        ),
        target(
            "gemini-cli",
            "Gemini CLI",
            "gemini",
            home.join(".gemini").join("settings.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![home.join(".gemini")],
            true,
        ),
        target(
            "kimi-code",
            "Kimi Code",
            "kimi",
            home.join(".kimi").join("mcp.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![home.join(".kimi")],
            true,
        ),
        target(
            "kiro",
            "Kiro",
            "kiro",
            home.join(".kiro").join("settings").join("mcp.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![home.join(".kiro")],
            true,
        ),
        target(
            "qwen-code",
            "Qwen Code",
            "qwen",
            home.join(".qwen").join("settings.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![home.join(".qwen")],
            true,
        ),
        target(
            "trae",
            "Trae",
            "trae",
            app_data.join("Trae").join("mcp.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![app_data.join("Trae")],
            true,
        ),
        target(
            "rider",
            "Rider GitHub Copilot",
            "rider",
            local_app_data
                .join("github-copilot")
                .join("intellij")
                .join("mcp.json"),
            "servers",
            "JSON",
            vscode,
            vec![local_app_data.join("github-copilot")],
            true,
        ),
        target(
            "antigravity",
            "Antigravity 2.0",
            "antigravity",
            home.join(".gemini").join("config").join("mcp_config.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![
                home.join(".antigravity"),
                home.join(".gemini").join("config"),
            ],
            true,
        ),
        target(
            "antigravity-ide",
            "Antigravity IDE",
            "antigravity-ide",
            home.join(".gemini")
                .join("antigravity-ide")
                .join("mcp_config.json"),
            "mcpServers",
            "JSON",
            standard,
            vec![home.join(".gemini").join("antigravity-ide")],
            true,
        ),
        target(
            "opencode",
            "OpenCode",
            "opencode",
            home.join(".config").join("opencode").join("opencode.json"),
            "mcp",
            "JSON",
            opencode,
            vec![home.join(".config").join("opencode")],
            true,
        ),
        target(
            "kilo-code",
            "Kilo Code",
            "kilo",
            home.join(".config").join("kilo").join("kilo.jsonc"),
            "mcp",
            "JSONC",
            standard,
            vec![home.join(".config").join("kilo")],
            false,
        ),
        target(
            "cherry-studio",
            "Cherry Studio",
            "cherry-studio",
            app_data
                .join("Cherry Studio")
                .join("config")
                .join("mcp.json"),
            "mcpServers",
            "manual",
            standard,
            vec![app_data.join("Cherry Studio").join("config")],
            false,
        ),
    ]
}

fn json_target_status(
    definition: &JsonTargetDefinition,
    command: &str,
    args: &[String],
) -> McpTargetDescriptor {
    let detected = executable_on_path(definition.executable)
        || definition.detect_paths.iter().any(|path| path.exists());
    let (state, error) = if !definition.path.exists() {
        ("not_configured".to_string(), String::new())
    } else {
        match fs::read_to_string(&definition.path)
            .map_err(|error| error.to_string())
            .and_then(|content| {
                let root = parse_json_document(&content, definition.config_format)
                    .map_err(|error| format!("invalid_config: {error}"))?;
                Ok(json_target_state(&root, definition, command, args))
            }) {
            Ok(Some(true)) => ("configured".to_string(), String::new()),
            Ok(Some(false)) => ("needs_repair".to_string(), String::new()),
            Ok(None) => ("not_configured".to_string(), String::new()),
            Err(error) => ("invalid_config".to_string(), error),
        }
    };
    let skill_client = crate::skill::clients::client_for_mcp_target(definition.id);
    McpTargetDescriptor {
        id: definition.id.to_string(),
        name: definition.name.to_string(),
        kind: "ai_client".to_string(),
        detected,
        detection_message: if detected {
            "已检测到客户端".to_string()
        } else if definition.path.is_file() {
            "未检测到客户端，已有配置将继续保留".to_string()
        } else {
            "未检测到客户端".to_string()
        },
        config_path: definition.path.to_string_lossy().to_string(),
        config_directory: definition
            .path
            .parent()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default(),
        config_format: definition.config_format.to_string(),
        state,
        supported_transports: vec!["stdio".to_string()],
        supports_auto_configure: definition.supports_auto_configure,
        supports_skills: skill_client.is_some(),
        skill_client_id: skill_client
            .map(|value| value.0)
            .unwrap_or_default()
            .to_string(),
        skill_client_name: skill_client
            .map(|value| value.1)
            .unwrap_or_default()
            .to_string(),
        restart_required: true,
        manual_snippet: manual_snippet_for_definition(definition, command, args),
        // Never expose the complete client configuration: it may contain
        // unrelated user secrets. The preview is the managed HiMind entry.
        config_preview: manual_snippet_for_definition(definition, command, args),
        error,
    }
}

fn json_target_state(
    root: &Value,
    definition: &JsonTargetDefinition,
    command: &str,
    args: &[String],
) -> Option<bool> {
    let target_id = definition.id;
    if definition.layout == JsonTargetLayout::OpenCode {
        let Some(server) = server_collection(root, definition)
            .and_then(|servers| servers.get(super::mcp_registry::AGENT_SERVER_ID))
        else {
            return None;
        };
        let Some(command_line) = server.get("command").and_then(Value::as_array) else {
            return Some(false);
        };
        let expected = std::iter::once(command)
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>();
        let command_matches = command_line.len() == expected.len()
            && command_line
                .iter()
                .zip(expected)
                .all(|(item, expected)| item.as_str() == Some(expected));
        let type_matches = server.get("type").and_then(Value::as_str) == Some("local");
        let enabled_matches = server.get("enabled").and_then(Value::as_bool) != Some(false);
        return Some(command_matches && type_matches && enabled_matches);
    }
    let Some(server) = server_collection(root, definition)
        .and_then(|servers| servers.get(super::mcp_registry::AGENT_SERVER_ID))
    else {
        return None;
    };
    let command_matches = server
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|value| paths_equal(value, command));
    let args_match = server
        .get("args")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.len() == args.len()
                && items
                    .iter()
                    .zip(args)
                    .all(|(item, expected)| item.as_str() == Some(expected.as_str()))
        });
    let client_matches = server
        .pointer("/env/HIMIND_AI_CLIENT_ID")
        .and_then(Value::as_str)
        == Some(target_id);
    let profile_matches = server
        .pointer("/env/HIMIND_AGENT_PROFILE")
        .and_then(Value::as_str)
        == Some(crate::store::paths::profile_name().as_str());
    let type_matches = if matches!(
        definition.layout,
        JsonTargetLayout::VsCode | JsonTargetLayout::NestedMcp
    ) {
        server.get("type").and_then(Value::as_str) == Some("stdio")
    } else if definition.id == "kilo-code" {
        server.get("type").and_then(Value::as_str) == Some("local")
    } else {
        true
    };
    Some(command_matches && args_match && client_matches && profile_matches && type_matches)
}

fn apply_json_target(
    options: &Options,
    target: &McpTargetDescriptor,
    reset_invalid: bool,
) -> Result<McpTargetOperationResult, Box<dyn Error>> {
    let definition = json_target_definitions(&home_directory())
        .into_iter()
        .find(|item| item.id == target.id)
        .ok_or_else(|| format!("unsupported MCP target: {}", target.id))?;
    if !definition.supports_auto_configure {
        return Err(format!("{} 仅支持手动配置", definition.name).into());
    }
    let original = if definition.path.is_file() {
        fs::read_to_string(&definition.path)?
    } else {
        String::new()
    };
    let mut root = if original.trim().is_empty() {
        json!({})
    } else {
        match parse_json_document(&original, definition.config_format) {
            Ok(value) => value,
            Err(_error) if reset_invalid => json!({}),
            Err(error) => {
                return Err(
                    format!("invalid_config: {error}; pass reset_invalid to rebuild").into(),
                )
            }
        }
    };
    let object = root
        .as_object_mut()
        .ok_or("MCP client JSON root must be an object")?;
    let launch = super::ai_clients::launch_spec(options)?;
    let entry = json_entry(&definition, &launch.command, &launch.args);
    if definition.layout == JsonTargetLayout::OpenCode {
        let servers = object
            .entry("mcp")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("OpenCode mcp collection must be an object")?;
        servers.insert(super::mcp_registry::AGENT_SERVER_ID.to_string(), entry);
    } else if definition.layout == JsonTargetLayout::NestedMcp {
        let mcp = object
            .entry("mcp")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("ZCode mcp configuration must be an object")?;
        let servers = mcp
            .entry("servers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("ZCode mcp.servers collection must be an object")?;
        servers.insert(super::mcp_registry::AGENT_SERVER_ID.to_string(), entry);
    } else {
        let servers = object
            .entry(definition.servers_key)
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("MCP client MCP server collection must be an object")?;
        servers.insert(super::mcp_registry::AGENT_SERVER_ID.to_string(), entry);
    }
    let updated = format!("{}\n", serde_json::to_string_pretty(&root)?);
    let changed = normalized_text(&original) != normalized_text(&updated);
    let backup_path = if changed {
        super::ai_clients::backup_and_write(&definition.path, updated.as_bytes())?
    } else {
        None
    };
    Ok(McpTargetOperationResult {
        target: json_target_status(&definition, &launch.command, &launch.args),
        changed,
        backup_path: backup_path
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default(),
        message: "目标客户端 MCP 配置已应用".to_string(),
    })
}

fn remove_json_target(
    options: &Options,
    target: &McpTargetDescriptor,
) -> Result<McpTargetOperationResult, Box<dyn Error>> {
    let definition = json_target_definitions(&home_directory())
        .into_iter()
        .find(|item| item.id == target.id)
        .ok_or_else(|| format!("unsupported MCP target: {}", target.id))?;
    if !definition.path.is_file() {
        return Ok(McpTargetOperationResult {
            target: target.clone(),
            changed: false,
            backup_path: String::new(),
            message: "目标客户端没有配置文件".to_string(),
        });
    }
    let original = fs::read_to_string(&definition.path)?;
    let launch = super::ai_clients::launch_spec(options)?;
    let mut root = parse_json_document(&original, definition.config_format)?;
    let removed = server_collection_mut(&mut root, &definition)
        .and_then(|servers| servers.remove(super::mcp_registry::AGENT_SERVER_ID))
        .is_some();
    if !removed {
        return Ok(McpTargetOperationResult {
            target: target.clone(),
            changed: false,
            backup_path: String::new(),
            message: "目标客户端没有 HiMind MCP 配置".to_string(),
        });
    }
    let updated = format!("{}\n", serde_json::to_string_pretty(&root)?);
    let backup = super::ai_clients::backup_and_write(&definition.path, updated.as_bytes())?;
    Ok(McpTargetOperationResult {
        target: json_target_status(&definition, &launch.command, &launch.args),
        changed: true,
        backup_path: backup
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default(),
        message: "目标客户端 MCP 配置已移除".to_string(),
    })
}

fn json_entry(definition: &JsonTargetDefinition, command: &str, args: &[String]) -> Value {
    if definition.layout == JsonTargetLayout::OpenCode {
        let mut command_line = vec![Value::String(command.to_string())];
        command_line.extend(args.iter().cloned().map(Value::String));
        return json!({
            "type": "local",
            "command": command_line,
            "enabled": true,
            "environment": {
                "HIMIND_AI_CLIENT_ID": definition.id,
                "HIMIND_AGENT_PROFILE": crate::store::paths::profile_name()
            }
        });
    }
    let mut entry = json!({
        "command": command,
        "args": args,
        "env": {
            "HIMIND_AI_CLIENT_ID": definition.id,
            "HIMIND_AGENT_PROFILE": crate::store::paths::profile_name()
        }
    });
    if matches!(
        definition.layout,
        JsonTargetLayout::VsCode | JsonTargetLayout::NestedMcp
    ) {
        entry["type"] = json!("stdio");
    }
    if definition.id == "kilo-code" {
        entry["type"] = json!("local");
        entry["enabled"] = json!(true);
    }
    entry
}

fn manual_snippet_for_definition(
    definition: &JsonTargetDefinition,
    command: &str,
    args: &[String],
) -> String {
    let entry = json_entry(definition, command, args);
    let payload = match definition.layout {
        JsonTargetLayout::OpenCode => json!({
            "mcp": { super::mcp_registry::AGENT_SERVER_ID: entry }
        }),
        JsonTargetLayout::NestedMcp => json!({
            "mcp": { "servers": { super::mcp_registry::AGENT_SERVER_ID: entry } }
        }),
        _ => json!({
            definition.servers_key: { super::mcp_registry::AGENT_SERVER_ID: entry }
        }),
    };
    serde_json::to_string_pretty(&payload).unwrap_or_default()
}

fn server_collection<'a>(
    root: &'a Value,
    definition: &JsonTargetDefinition,
) -> Option<&'a serde_json::Map<String, Value>> {
    match definition.layout {
        JsonTargetLayout::OpenCode => root.get("mcp").and_then(Value::as_object),
        JsonTargetLayout::NestedMcp => root
            .get("mcp")
            .and_then(Value::as_object)
            .and_then(|mcp| mcp.get("servers"))
            .and_then(Value::as_object),
        _ => root.get(definition.servers_key).and_then(Value::as_object),
    }
}

fn server_collection_mut<'a>(
    root: &'a mut Value,
    definition: &JsonTargetDefinition,
) -> Option<&'a mut serde_json::Map<String, Value>> {
    match definition.layout {
        JsonTargetLayout::OpenCode => root.get_mut("mcp").and_then(Value::as_object_mut),
        JsonTargetLayout::NestedMcp => root
            .get_mut("mcp")
            .and_then(Value::as_object_mut)
            .and_then(|mcp| mcp.get_mut("servers"))
            .and_then(Value::as_object_mut),
        _ => root
            .get_mut(definition.servers_key)
            .and_then(Value::as_object_mut),
    }
}

fn parse_json_document(content: &str, format: &str) -> Result<Value, serde_json::Error> {
    if format.eq_ignore_ascii_case("JSONC") {
        serde_json::from_str(&strip_jsonc_comments(content))
    } else {
        serde_json::from_str(content)
    }
}

fn strip_jsonc_comments(content: &str) -> String {
    let mut output = String::with_capacity(content.len());
    let bytes = content.as_bytes();
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            output.push(byte as char);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            output.push('"');
            index += 1;
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                index += 1;
            }
            index = (index + 2).min(bytes.len());
        } else {
            output.push(byte as char);
            index += 1;
        }
    }
    strip_jsonc_trailing_commas(&output)
}

fn strip_jsonc_trailing_commas(content: &str) -> String {
    let mut output = String::with_capacity(content.len());
    let bytes = content.as_bytes();
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            output.push(byte as char);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            output.push('"');
            index += 1;
            continue;
        }
        if byte == b',' {
            let mut lookahead = index + 1;
            while lookahead < bytes.len() && bytes[lookahead].is_ascii_whitespace() {
                lookahead += 1;
            }
            if matches!(bytes.get(lookahead), Some(b'}' | b']')) {
                index += 1;
                continue;
            }
        }
        output.push(byte as char);
        index += 1;
    }
    output
}

fn normalized_text(value: &str) -> String {
    value.replace("\r\n", "\n").trim().to_string()
}

fn executable_on_path(name: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    let extensions = if cfg!(windows) {
        vec!["", ".exe", ".cmd", ".bat"]
    } else {
        vec![""]
    };
    env::split_paths(&path).any(|directory| {
        extensions.iter().any(|extension| {
            let candidate = directory.join(format!("{name}{extension}"));
            candidate.is_file()
        })
    })
}

fn paths_equal(left: &str, right: &str) -> bool {
    let left = Path::new(left);
    let right = Path::new(right);
    if cfg!(windows) {
        left.to_string_lossy()
            .replace('/', "\\")
            .eq_ignore_ascii_case(&right.to_string_lossy().replace('/', "\\"))
    } else {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dsh_target_is_explicitly_managed() {
        let mut options = Options::from_env();
        options.state_path = std::env::temp_dir().join("himind-mcp-target-test-state.json");
        let target = dsh_target(&options);
        assert_eq!(target.id, DSH_TARGET_ID);
        assert!(target.supported_transports.contains(&"stdio".to_string()));
    }

    #[test]
    fn json_adapter_distinguishes_missing_and_stale_registration() {
        let missing = json!({ "mcpServers": {} });
        let definition = json_target_definitions(Path::new("C:\\Users\\test"))
            .into_iter()
            .find(|item| item.id == "cursor")
            .unwrap();
        assert_eq!(
            json_target_state(&missing, &definition, "agent.exe", &[]),
            None
        );
        let stale = json!({ "mcpServers": { "himind-agent": { "command": "other", "args": [] } } });
        assert_eq!(
            json_target_state(&stale, &definition, "agent.exe", &[]),
            Some(false)
        );
        let ready = json!({
            "mcpServers": {
                "himind-agent": {
                    "command": "agent.exe",
                    "args": [],
                    "env": {
                        "HIMIND_AI_CLIENT_ID": "cursor",
                        "HIMIND_AGENT_PROFILE": crate::store::paths::profile_name()
                    }
                }
            }
        });
        assert_eq!(
            json_target_state(&ready, &definition, "agent.exe", &[]),
            Some(true)
        );
    }

    #[test]
    fn json_adapter_rejects_wrong_identity_and_vscode_layout() {
        let wrong_identity = json!({
            "mcpServers": {
                "himind-agent": {
                    "command": "agent.exe",
                    "args": [],
                    "env": {
                        "HIMIND_AI_CLIENT_ID": "other",
                        "HIMIND_AGENT_PROFILE": crate::store::paths::profile_name()
                    }
                }
            }
        });
        let cursor = json_target_definitions(Path::new("C:\\Users\\test"))
            .into_iter()
            .find(|item| item.id == "cursor")
            .unwrap();
        assert_eq!(
            json_target_state(&wrong_identity, &cursor, "agent.exe", &[]),
            Some(false)
        );

        let missing_type = json!({
            "servers": {
                "himind-agent": {
                    "command": "agent.exe",
                    "args": [],
                    "env": {
                        "HIMIND_AI_CLIENT_ID": "vscode",
                        "HIMIND_AGENT_PROFILE": crate::store::paths::profile_name()
                    }
                }
            }
        });
        let vscode = json_target_definitions(Path::new("C:\\Users\\test"))
            .into_iter()
            .find(|item| item.id == "vscode")
            .unwrap();
        assert_eq!(
            json_target_state(&missing_type, &vscode, "agent.exe", &[]),
            Some(false)
        );
    }

    #[test]
    fn qoder_uses_standard_mcp_servers_and_exposes_skills() {
        let definition = json_target_definitions(Path::new("C:\\Users\\test"))
            .into_iter()
            .find(|item| item.id == "qoder")
            .unwrap();
        let ready = json!({
            "mcpServers": {
                "himind-agent": {
                    "command": "agent.exe",
                    "args": ["mcp"],
                    "env": {
                        "HIMIND_AI_CLIENT_ID": "qoder",
                        "HIMIND_AGENT_PROFILE": crate::store::paths::profile_name()
                    }
                }
            }
        });
        assert_eq!(
            json_target_state(&ready, &definition, "agent.exe", &["mcp".to_string()]),
            Some(true)
        );
        assert!(manual_snippet_for_definition(&definition, "agent.exe", &[]).contains("mcpServers"));
        assert_eq!(
            crate::skill::clients::client_for_mcp_target("qoder"),
            Some(("qoder", "Qoder"))
        );
    }

    #[test]
    fn skill_support_is_independent_from_mcp_support() {
        assert_eq!(
            crate::skill::clients::client_for_mcp_target("cline"),
            Some(("cline", "Cline"))
        );
        assert_eq!(crate::skill::clients::client_for_mcp_target("rider"), None);
        let rider = json_target_definitions(Path::new("C:\\Users\\test"))
            .into_iter()
            .find(|item| item.id == "rider")
            .unwrap();
        let descriptor = json_target_status(&rider, "agent.exe", &[]);
        assert!(!descriptor.supports_skills);
    }

    #[test]
    fn zcode_uses_nested_mcp_servers_layout() {
        let definition = json_target_definitions(Path::new("C:\\Users\\test"))
            .into_iter()
            .find(|item| item.id == "zcode")
            .unwrap();
        let ready = json!({
            "mcp": { "servers": {
                "himind-agent": {
                    "type": "stdio",
                    "command": "agent.exe",
                    "args": [],
                    "env": {
                        "HIMIND_AI_CLIENT_ID": "zcode",
                        "HIMIND_AGENT_PROFILE": crate::store::paths::profile_name()
                    }
                }
            }}
        });
        assert_eq!(
            json_target_state(&ready, &definition, "agent.exe", &[]),
            Some(true)
        );
        let snippet: Value = serde_json::from_str(&manual_snippet_for_definition(
            &definition,
            "agent.exe",
            &[],
        ))
        .unwrap();
        assert!(snippet.pointer("/mcp/servers/himind-agent").is_some());
    }

    #[test]
    fn opencode_command_array_and_jsonc_are_parsed() {
        let opencode = json_target_definitions(Path::new("C:\\Users\\test"))
            .into_iter()
            .find(|item| item.id == "opencode")
            .unwrap();
        let entry = json_entry(&opencode, "agent.exe", &["mcp".to_string()]);
        assert_eq!(entry["command"], json!(["agent.exe", "mcp"]));
        assert_eq!(entry["type"], "local");

        let parsed =
            parse_json_document("{ // note\n \"mcp\": {\"enabled\": true,},\n}", "JSONC").unwrap();
        assert_eq!(
            parsed.pointer("/mcp/enabled").and_then(Value::as_bool),
            Some(true)
        );
    }
}
