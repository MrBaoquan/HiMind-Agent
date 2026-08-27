use serde::Serialize;
use serde_json::{json, Map, Value};
use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use toml_edit::{value, Array, DocumentMut, Item, Table};

use crate::runtime::process::configure_hidden_process;
use crate::Options;

const SERVER_ID: &str = "himind-agent";

#[derive(Debug, Clone)]
pub(crate) struct AgentMcpLaunchSpec {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct McpConnectionTestResult {
    pub ok: bool,
    pub server_name: String,
    pub server_version: String,
    pub protocol_version: String,
    pub capability_count: usize,
    pub duration_ms: u128,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigKind {
    CodexToml,
    McpJson,
    WorkBuddyJson,
}

struct ClientDefinition {
    id: &'static str,
    name: &'static str,
    path: PathBuf,
    detected: bool,
    detection_message: String,
    kind: ConfigKind,
}

pub(crate) fn launch_spec(options: &Options) -> Result<AgentMcpLaunchSpec, Box<dyn Error>> {
    let executable = mcp_executable()?;
    let arguments = mcp_arguments(options);
    Ok(AgentMcpLaunchSpec {
        command: executable.to_string_lossy().to_string(),
        args: arguments,
    })
}

pub(crate) fn targets(
    options: &Options,
) -> Result<Vec<super::mcp_target_types::McpTargetDescriptor>, Box<dyn Error>> {
    let executable = mcp_executable()?;
    let arguments = mcp_arguments(options);
    Ok(client_definitions()
        .into_iter()
        .map(|definition| integration_status(&definition, &executable, &arguments))
        .collect())
}

pub(crate) fn configure(
    options: &Options,
    client_id: &str,
    reset_invalid: bool,
) -> Result<super::mcp_target_types::McpTargetOperationResult, Box<dyn Error>> {
    let executable = mcp_executable()?;
    let arguments = mcp_arguments(options);
    let definition = find_client_definition(client_id)?;
    let original = if definition.path.exists() {
        fs::read_to_string(&definition.path)?
    } else {
        String::new()
    };
    let (updated, reset) = merge_client_config(
        definition.kind,
        &original,
        &executable,
        &arguments,
        definition.id,
        reset_invalid,
    )?;
    let changed = normalized_text(&original) != normalized_text(&updated);
    let backup_path = if changed || reset {
        backup_and_write(&definition.path, updated.as_bytes())?
    } else {
        None
    };
    Ok(super::mcp_target_types::McpTargetOperationResult {
        target: integration_status(&definition, &executable, &arguments),
        changed,
        backup_path: backup_path
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default(),
        message: "目标客户端 MCP 配置已应用".to_string(),
    })
}

pub(crate) fn remove_configuration(
    options: &Options,
    client_id: &str,
) -> Result<super::mcp_target_types::McpTargetOperationResult, Box<dyn Error>> {
    let executable = mcp_executable()?;
    let arguments = mcp_arguments(options);
    let definition = find_client_definition(client_id)?;
    if !definition.path.exists() {
        return Ok(super::mcp_target_types::McpTargetOperationResult {
            target: integration_status(&definition, &executable, &arguments),
            changed: false,
            backup_path: String::new(),
            message: "目标客户端没有配置文件".to_string(),
        });
    }
    let original = fs::read_to_string(&definition.path)?;
    let updated = match definition.kind {
        ConfigKind::CodexToml => remove_codex_config(&original)?,
        ConfigKind::McpJson | ConfigKind::WorkBuddyJson => remove_json_config(&original)?,
    };
    let changed = normalized_text(&original) != normalized_text(&updated);
    let backup_path = if changed {
        backup_and_write(&definition.path, updated.as_bytes())?
    } else {
        None
    };
    Ok(super::mcp_target_types::McpTargetOperationResult {
        target: integration_status(&definition, &executable, &arguments),
        changed,
        backup_path: backup_path
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default(),
        message: "目标客户端 MCP 配置已移除".to_string(),
    })
}

pub(crate) fn test_connection(
    options: &Options,
) -> Result<McpConnectionTestResult, Box<dyn Error>> {
    let executable = mcp_executable()?;
    let env = std::collections::BTreeMap::from([
        (
            "HIMIND_AI_CLIENT_ID".to_string(),
            "agent-self-test".to_string(),
        ),
        (
            "HIMIND_AGENT_PROFILE".to_string(),
            crate::store::paths::profile_name(),
        ),
    ]);
    let probe = crate::app::mcp_probe::probe_stdio_command(
        &executable.to_string_lossy(),
        &mcp_arguments(options),
        &env,
        None,
        Duration::from_secs(10),
    )?;
    Ok(McpConnectionTestResult {
        ok: probe.ok,
        server_name: probe.server_name,
        server_version: probe.server_version,
        protocol_version: probe.protocol_version,
        capability_count: probe.tool_count,
        duration_ms: probe.duration_ms,
    })
}

/// Move previously registered HiMind MCP entries from a GUI Agent executable
/// to the correct console entry. Installed layouts use the stable launcher;
/// development layouts use the sibling MCP companion. This is intentionally
/// narrow: only the managed `himind-agent` command changes.
pub(crate) fn migrate_legacy_agent_commands() -> Result<usize, Box<dyn Error>> {
    let current = env::current_exe()?;
    let root = crate::install_layout::installation_root_from_executable(&current);
    let mcp_entry = mcp_executable()?;
    if !mcp_entry.is_file() {
        return Ok(0);
    }
    let mut migrated = 0;
    for definition in client_definitions() {
        if !definition.path.is_file() {
            continue;
        }
        let original = fs::read_to_string(&definition.path)?;
        let updated = match definition.kind {
            ConfigKind::CodexToml => {
                let mut document = original.parse::<DocumentMut>()?;
                let Some(server) = document
                    .get_mut("mcp_servers")
                    .and_then(Item::as_table_mut)
                    .and_then(|servers| servers.get_mut(SERVER_ID))
                    .and_then(Item::as_table_mut)
                else {
                    continue;
                };
                let command = server
                    .get("command")
                    .and_then(Item::as_str)
                    .unwrap_or_default();
                if !is_legacy_agent_command(command, &root, &mcp_entry) {
                    continue;
                }
                server.insert("command", value(mcp_entry.to_string_lossy().to_string()));
                document.to_string()
            }
            ConfigKind::McpJson | ConfigKind::WorkBuddyJson => {
                let mut root_value = serde_json::from_str::<Value>(&original)?;
                let Some(server) = root_value
                    .get_mut("mcpServers")
                    .and_then(Value::as_object_mut)
                    .and_then(|servers| servers.get_mut(SERVER_ID))
                    .and_then(Value::as_object_mut)
                else {
                    continue;
                };
                let command = server
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !is_legacy_agent_command(command, &root, &mcp_entry) {
                    continue;
                }
                server.insert(
                    "command".to_string(),
                    Value::String(mcp_entry.to_string_lossy().to_string()),
                );
                format!("{}\n", serde_json::to_string_pretty(&root_value)?)
            }
        };
        if normalized_text(&original) != normalized_text(&updated) {
            backup_and_write(&definition.path, updated.as_bytes())?;
            migrated += 1;
        }
    }
    Ok(migrated)
}

fn is_legacy_agent_command(command: &str, installation_root: &Path, expected_entry: &Path) -> bool {
    let candidate = Path::new(command);
    candidate
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("himind-agent.exe"))
        && crate::install_layout::installation_root_from_executable(candidate) == installation_root
        && !paths_equal(command, expected_entry)
}

fn client_definitions() -> Vec<ClientDefinition> {
    let home = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let codex_root = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let copilot_path = env::var_os("HIMIND_COPILOT_MCP_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".copilot").join("mcp-config.json"));
    let workbuddy_path = env::var_os("HIMIND_WORKBUDDY_MCP_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_workbuddy_mcp_config_path(&home));
    let codex_detected = executable_on_path("codex");
    let copilot_detected = executable_responds("copilot", &["--version"]);
    let workbuddy_detected = executable_on_path("workbuddy") || workbuddy_executable_exists();
    vec![
        ClientDefinition {
            id: "codex",
            name: "Codex",
            detected: codex_detected,
            detection_message: detection_message(codex_detected),
            path: codex_root.join("config.toml"),
            kind: ConfigKind::CodexToml,
        },
        ClientDefinition {
            id: "github-copilot",
            name: "GitHub Copilot",
            detected: copilot_detected,
            detection_message: detection_message(copilot_detected),
            path: copilot_path,
            kind: ConfigKind::McpJson,
        },
        ClientDefinition {
            id: "workbuddy",
            name: "WorkBuddy",
            detected: workbuddy_detected,
            detection_message: detection_message(workbuddy_detected),
            path: workbuddy_path,
            kind: ConfigKind::WorkBuddyJson,
        },
    ]
}

fn default_workbuddy_mcp_config_path(home: &Path) -> PathBuf {
    home.join(".workbuddy").join("mcp.json")
}

fn find_client_definition(client_id: &str) -> Result<ClientDefinition, Box<dyn Error>> {
    client_definitions()
        .into_iter()
        .find(|client| client.id == client_id)
        .ok_or_else(|| format!("unsupported AI client: {client_id}").into())
}

fn integration_status(
    definition: &ClientDefinition,
    executable: &Path,
    arguments: &[String],
) -> super::mcp_target_types::McpTargetDescriptor {
    let skill_client = crate::skill::clients::client_for_mcp_target(definition.id);
    let preview = match definition.kind {
        ConfigKind::CodexToml => merge_codex_config("", executable, arguments, definition.id),
        ConfigKind::McpJson | ConfigKind::WorkBuddyJson => merge_json_config(
            "",
            executable,
            arguments,
            definition.id,
            definition.kind == ConfigKind::WorkBuddyJson,
        ),
    };
    let (state, error) = if !definition.path.exists() {
        ("not_configured".to_string(), String::new())
    } else {
        match configuration_matches(definition, executable, arguments) {
            Ok(true) => ("configured".to_string(), String::new()),
            Ok(false) => ("needs_repair".to_string(), String::new()),
            Err(error) => ("invalid_config".to_string(), error.to_string()),
        }
    };
    super::mcp_target_types::McpTargetDescriptor {
        id: definition.id.to_string(),
        name: definition.name.to_string(),
        detected: definition.detected,
        detection_message: if definition.detected {
            definition.detection_message.clone()
        } else if definition.path.exists() {
            "未找到客户端，已有配置将继续保留".to_string()
        } else {
            definition.detection_message.clone()
        },
        state,
        config_path: definition.path.to_string_lossy().to_string(),
        config_directory: definition
            .path
            .parent()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default(),
        config_format: if definition.kind == ConfigKind::CodexToml {
            "TOML"
        } else {
            "JSON"
        }
        .to_string(),
        kind: "ai_client".to_string(),
        supported_transports: vec!["stdio".to_string()],
        supports_auto_configure: true,
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
        manual_snippet: super::mcp_target_types::manual_snippet(
            &executable.to_string_lossy(),
            arguments,
            definition.id,
        ),
        config_preview: preview.unwrap_or_else(|error| format!("无法生成配置: {error}")),
        error,
    }
}

fn configuration_matches(
    definition: &ClientDefinition,
    executable: &Path,
    arguments: &[String],
) -> Result<bool, Box<dyn Error>> {
    let content = fs::read_to_string(&definition.path)?;
    match definition.kind {
        ConfigKind::CodexToml => {
            let document = content.parse::<DocumentMut>()?;
            let Some(server) = document
                .get("mcp_servers")
                .and_then(Item::as_table_like)
                .and_then(|servers| servers.get(SERVER_ID))
                .and_then(Item::as_table_like)
            else {
                return Ok(false);
            };
            let command_matches = server
                .get("command")
                .and_then(Item::as_str)
                .is_some_and(|value| paths_equal(value, executable));
            let args_match = server
                .get("args")
                .and_then(Item::as_array)
                .is_some_and(|items| {
                    items.len() == arguments.len()
                        && items
                            .iter()
                            .zip(arguments)
                            .all(|(item, expected)| item.as_str() == Some(expected.as_str()))
                });
            let client_matches = server
                .get("env")
                .and_then(Item::as_table_like)
                .and_then(|table| table.get("HIMIND_AI_CLIENT_ID"))
                .and_then(Item::as_str)
                == Some(definition.id);
            let profile_matches = server
                .get("env")
                .and_then(Item::as_table_like)
                .and_then(|table| table.get("HIMIND_AGENT_PROFILE"))
                .and_then(Item::as_str)
                == Some(crate::store::paths::profile_name().as_str());
            Ok(command_matches && args_match && client_matches && profile_matches)
        }
        ConfigKind::McpJson | ConfigKind::WorkBuddyJson => {
            let root: Value = serde_json::from_str(&content)?;
            let Some(server) = root
                .get("mcpServers")
                .and_then(Value::as_object)
                .and_then(|servers| servers.get(SERVER_ID))
            else {
                return Ok(false);
            };
            let command_matches = server
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|value| paths_equal(value, executable));
            let args_match = server
                .get("args")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.len() == arguments.len()
                        && items
                            .iter()
                            .zip(arguments)
                            .all(|(item, expected)| item.as_str() == Some(expected.as_str()))
                });
            let client_matches = server
                .pointer("/env/HIMIND_AI_CLIENT_ID")
                .and_then(Value::as_str)
                == Some(definition.id);
            let profile_matches = server
                .pointer("/env/HIMIND_AGENT_PROFILE")
                .and_then(Value::as_str)
                == Some(crate::store::paths::profile_name().as_str());
            Ok(command_matches && args_match && client_matches && profile_matches)
        }
    }
}

fn merge_codex_config(
    content: &str,
    executable: &Path,
    arguments: &[String],
    client_id: &str,
) -> Result<String, Box<dyn Error>> {
    let mut document = if content.trim().is_empty() {
        DocumentMut::new()
    } else {
        content.parse::<DocumentMut>()?
    };
    if !document.as_table().contains_key("mcp_servers") {
        document["mcp_servers"] = Item::Table(Table::new());
    }
    let servers = document["mcp_servers"]
        .as_table_mut()
        .ok_or("Codex mcp_servers must be a TOML table")?;
    let mut server = Table::new();
    server.insert("command", value(executable.to_string_lossy().to_string()));
    let mut args = Array::new();
    for argument in arguments {
        args.push(argument.as_str());
    }
    server.insert("args", value(args));
    let mut environment = Table::new();
    environment.insert("HIMIND_AI_CLIENT_ID", value(client_id));
    environment.insert(
        "HIMIND_AGENT_PROFILE",
        value(crate::store::paths::profile_name()),
    );
    server.insert("env", Item::Table(environment));
    servers.insert(SERVER_ID, Item::Table(server));
    Ok(document.to_string())
}

fn merge_client_config(
    kind: ConfigKind,
    content: &str,
    executable: &Path,
    arguments: &[String],
    client_id: &str,
    reset_invalid: bool,
) -> Result<(String, bool), Box<dyn Error>> {
    let merge = |source: &str| match kind {
        ConfigKind::CodexToml => merge_codex_config(source, executable, arguments, client_id),
        ConfigKind::McpJson | ConfigKind::WorkBuddyJson => merge_json_config(
            source,
            executable,
            arguments,
            client_id,
            kind == ConfigKind::WorkBuddyJson,
        ),
    };
    match merge(content) {
        Ok(updated) => Ok((updated, false)),
        Err(_) if reset_invalid => Ok((merge("")?, true)),
        Err(_) => Err("配置文件格式有误。请使用“备份并重建”恢复连接。".into()),
    }
}

fn remove_codex_config(content: &str) -> Result<String, Box<dyn Error>> {
    let mut document = content.parse::<DocumentMut>()?;
    if let Some(servers) = document.get_mut("mcp_servers").and_then(Item::as_table_mut) {
        servers.remove(SERVER_ID);
        if servers.is_empty() {
            document.as_table_mut().remove("mcp_servers");
        }
    }
    Ok(document.to_string())
}

fn merge_json_config(
    content: &str,
    executable: &Path,
    arguments: &[String],
    client_id: &str,
    workbuddy: bool,
) -> Result<String, Box<dyn Error>> {
    let mut root = if content.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str::<Value>(content)?
    };
    let object = root
        .as_object_mut()
        .ok_or("MCP configuration root must be a JSON object")?;
    if !object.contains_key("mcpServers") {
        object.insert("mcpServers".to_string(), Value::Object(Map::new()));
    }
    let servers = object
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .ok_or("mcpServers must be a JSON object")?;
    let mut server = json!({
        "type": "stdio",
        "command": executable.to_string_lossy(),
        "args": arguments,
        "env": {
            "HIMIND_AI_CLIENT_ID": client_id,
            "HIMIND_AGENT_PROFILE": crate::store::paths::profile_name()
        }
    });
    if workbuddy {
        server["timeout"] = json!(60);
        server["disabled"] = json!(false);
    }
    servers.insert(SERVER_ID.to_string(), server);
    Ok(format!("{}\n", serde_json::to_string_pretty(&root)?))
}

fn remove_json_config(content: &str) -> Result<String, Box<dyn Error>> {
    let mut root = serde_json::from_str::<Value>(content)?;
    if let Some(servers) = root.get_mut("mcpServers").and_then(Value::as_object_mut) {
        servers.remove(SERVER_ID);
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&root)?))
}

fn mcp_arguments(options: &Options) -> Vec<String> {
    vec![
        "--mcp".to_string(),
        "--api".to_string(),
        options.api_base.clone(),
        "--state".to_string(),
        options.state_path.to_string_lossy().to_string(),
    ]
}

fn stable_launcher_executable() -> Result<PathBuf, Box<dyn Error>> {
    Ok(crate::install_layout::stable_launcher_for_executable(
        &env::current_exe()?,
    ))
}

fn mcp_executable() -> Result<PathBuf, Box<dyn Error>> {
    let current = env::current_exe()?;
    let launcher = crate::install_layout::stable_launcher_for_executable(&current);
    if launcher.is_file() && launcher != current {
        return Ok(launcher);
    }
    let companion = crate::install_layout::companion_mcp_path(&current);
    if companion.is_file() {
        return Ok(companion);
    }
    if cfg!(debug_assertions) {
        // Test harnesses do not spawn a child process; retaining the current
        // executable keeps pure config merge tests independent of build files.
        return Ok(current);
    }
    Err("HiMind Agent MCP console companion is missing; rebuild or reinstall the same Agent version".into())
}

pub(crate) fn backup_and_write(
    path: &Path,
    content: &[u8],
) -> Result<Option<PathBuf>, Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let backup = if path.exists() {
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("mcp-config");
        let backup = path.with_file_name(format!(
            "{file_name}.himind-backup-{}.bak",
            unix_now_millis()
        ));
        fs::copy(path, &backup)?;
        Some(backup)
    } else {
        None
    };
    atomic_write(path, content)?;
    Ok(backup)
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<(), Box<dyn Error>> {
    let temporary = path.with_file_name(format!(
        ".{}.himind-tmp-{}-{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("config"),
        std::process::id(),
        unix_now_millis()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);
    replace_file(&temporary, path)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    use std::os::windows::ffi::OsStrExt;
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }
    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        let _ = fs::remove_file(source);
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    fs::rename(source, destination)?;
    Ok(())
}

fn executable_on_path(name: &str) -> bool {
    find_executable(name).is_some()
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let candidates = if cfg!(target_os = "windows") {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
        ]
    } else {
        vec![name.to_string()]
    };
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path).find_map(|directory| {
            candidates
                .iter()
                .map(|file| directory.join(file))
                .find(|candidate| candidate.is_file())
        })
    })
}

fn executable_responds(name: &str, arguments: &[&str]) -> bool {
    let Some(executable) = find_executable(name) else {
        return false;
    };
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_hidden_process(&mut command);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let Ok(output) = child.wait_with_output() else {
                    return false;
                };
                let message = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
                .to_ascii_lowercase();
                return status.success() && command_output_indicates_client(&message);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn command_output_indicates_client(message: &str) -> bool {
    ![
        "cannot find github copilot cli",
        "not installed",
        "command not found",
        "is not recognized as",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

pub(crate) fn workbuddy_executable_exists() -> bool {
    let local_install = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("WorkBuddy"))
        .filter(|path| path.is_dir())
        .is_some_and(|root| {
            walkdir::WalkDir::new(root)
                .max_depth(3)
                .into_iter()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.file_type().is_file()
                        && entry.file_name().to_str().is_some_and(|name| {
                            name.to_ascii_lowercase().contains("workbuddy")
                                && name.to_ascii_lowercase().ends_with(".exe")
                        })
                })
        });
    local_install || workbuddy_start_menu_link_exists() || workbuddy_registry_entry_exists()
}

fn workbuddy_start_menu_link_exists() -> bool {
    [
        env::var_os("APPDATA").map(PathBuf::from),
        env::var_os("ProgramData").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    .map(|root| {
        root.join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
    })
    .filter(|root| root.is_dir())
    .any(|root| {
        walkdir::WalkDir::new(root)
            .max_depth(4)
            .into_iter()
            .filter_map(Result::ok)
            .any(|entry| {
                entry.file_type().is_file()
                    && entry.file_name().to_str().is_some_and(|name| {
                        name.to_ascii_lowercase().contains("workbuddy")
                            && name.to_ascii_lowercase().ends_with(".lnk")
                    })
            })
    })
}

#[cfg(target_os = "windows")]
fn workbuddy_registry_entry_exists() -> bool {
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    use winreg::RegKey;

    [
        (
            RegKey::predef(HKEY_CURRENT_USER),
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            RegKey::predef(HKEY_LOCAL_MACHINE),
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
    ]
    .iter()
    .any(|(root, path)| {
        [KEY_WOW64_64KEY, KEY_WOW64_32KEY].iter().any(|view| {
            let Ok(uninstall) = root.open_subkey_with_flags(path, KEY_READ | *view) else {
                return false;
            };
            uninstall.enum_keys().flatten().any(|child_name| {
                let Ok(child) = uninstall.open_subkey_with_flags(&child_name, KEY_READ | *view)
                else {
                    return false;
                };
                let mut values = vec![child_name];
                for value_name in [
                    "DisplayName",
                    "Publisher",
                    "InstallLocation",
                    "DisplayIcon",
                    "UninstallString",
                ] {
                    if let Ok(value) = child.get_value::<String, _>(value_name) {
                        values.push(value);
                    }
                }
                values
                    .iter()
                    .any(|value| value.to_ascii_lowercase().contains("workbuddy"))
            })
        })
    })
}

#[cfg(not(target_os = "windows"))]
fn workbuddy_registry_entry_exists() -> bool {
    false
}

fn detection_message(detected: bool) -> String {
    if detected {
        "已找到客户端".to_string()
    } else {
        "未在这台电脑上找到客户端".to_string()
    }
}

fn paths_equal(configured: &str, expected: &Path) -> bool {
    let configured = PathBuf::from(configured);
    configured
        .canonicalize()
        .ok()
        .zip(expected.canonicalize().ok())
        .map(|(left, right)| left == right)
        .unwrap_or_else(|| configured == expected)
}

fn normalized_text(value: &str) -> &str {
    value.trim_end_matches(['\r', '\n'])
}

fn unix_now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        command_output_indicates_client, default_workbuddy_mcp_config_path, merge_client_config,
        merge_codex_config, merge_json_config, remove_codex_config, remove_json_config, ConfigKind,
        SERVER_ID,
    };
    use serde_json::Value;
    use std::env;
    use std::fs;
    use std::path::Path;
    use toml_edit::DocumentMut;

    #[test]
    fn workbuddy_uses_mcp_json_as_its_default_config_file() {
        let home = Path::new("test-home");
        assert_eq!(
            default_workbuddy_mcp_config_path(home),
            home.join(".workbuddy").join("mcp.json")
        );
    }

    #[test]
    fn merges_codex_server_without_removing_existing_configuration() {
        let source = "model = \"gpt-test\"\n[mcp_servers.existing]\ncommand = \"node\"\n";
        let updated = merge_codex_config(
            source,
            Path::new(r"C:\HiMind\himind-agent.exe"),
            &["--mcp".to_string()],
            "codex",
        )
        .unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        assert_eq!(document["model"].as_str(), Some("gpt-test"));
        assert!(document["mcp_servers"]["existing"].is_table());
        assert_eq!(
            document["mcp_servers"][SERVER_ID]["env"]["HIMIND_AI_CLIENT_ID"].as_str(),
            Some("codex")
        );
        let removed = remove_codex_config(&updated).unwrap();
        let document = removed.parse::<DocumentMut>().unwrap();
        assert!(document["mcp_servers"]["existing"].is_table());
        assert!(document["mcp_servers"]
            .as_table()
            .unwrap()
            .get(SERVER_ID)
            .is_none());
    }

    #[test]
    fn merges_json_server_without_removing_existing_configuration() {
        let source =
            r#"{"mcpServers":{"existing":{"type":"stdio","command":"node"}},"setting":true}"#;
        let updated = merge_json_config(
            source,
            Path::new(r"C:\HiMind\himind-agent.exe"),
            &["--mcp".to_string()],
            "workbuddy",
            true,
        )
        .unwrap();
        let value: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(value["setting"], true);
        assert_eq!(value["mcpServers"]["existing"]["command"], "node");
        assert_eq!(
            value["mcpServers"][SERVER_ID]["env"]["HIMIND_AI_CLIENT_ID"],
            "workbuddy"
        );
        assert_eq!(value["mcpServers"][SERVER_ID]["disabled"], false);
        let removed = remove_json_config(&updated).unwrap();
        let value: Value = serde_json::from_str(&removed).unwrap();
        assert!(value["mcpServers"].get(SERVER_ID).is_none());
        assert!(value["mcpServers"].get("existing").is_some());
    }

    #[test]
    fn configuration_write_keeps_a_backup_and_replaces_atomically() {
        let root = env::temp_dir().join(format!(
            "himind-ai-config-test-{}-{}",
            std::process::id(),
            super::unix_now_millis()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("mcp.json");
        fs::write(&path, b"old").unwrap();
        let backup = super::backup_and_write(&path, b"new").unwrap().unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "new");
        assert_eq!(fs::read_to_string(backup).unwrap(), "old");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_configuration_requires_explicit_reset() {
        let executable = Path::new(r"C:\HiMind\himind-agent.exe");
        let error = merge_client_config(
            ConfigKind::McpJson,
            "{invalid",
            executable,
            &["--mcp".to_string()],
            "github-copilot",
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("备份并重建"));

        let (updated, reset) = merge_client_config(
            ConfigKind::McpJson,
            "{invalid",
            executable,
            &["--mcp".to_string()],
            "github-copilot",
            true,
        )
        .unwrap();
        assert!(reset);
        let value: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(
            value["mcpServers"][SERVER_ID]["env"]["HIMIND_AI_CLIENT_ID"],
            "github-copilot"
        );
    }

    #[test]
    fn client_probe_rejects_placeholder_commands() {
        assert!(!command_output_indicates_client(
            "cannot find github copilot cli"
        ));
        assert!(command_output_indicates_client("github copilot 1.2.3"));
    }
}
