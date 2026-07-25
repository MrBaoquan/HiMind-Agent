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

use crate::Options;

const SERVER_ID: &str = "himind-agent";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AiClientIntegration {
    pub id: String,
    pub name: String,
    pub detected: bool,
    pub detection_message: String,
    pub state: String,
    pub config_path: String,
    pub config_directory: String,
    pub config_format: String,
    pub config_preview: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AiIntegrationOverview {
    pub protocol: String,
    pub server_id: String,
    pub command: String,
    pub args: Vec<String>,
    pub clients: Vec<AiClientIntegration>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AiClientConfigurationResult {
    pub client: AiClientIntegration,
    pub changed: bool,
    pub backup_path: String,
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

pub(crate) fn overview(options: &Options) -> Result<AiIntegrationOverview, Box<dyn Error>> {
    let executable = env::current_exe()?;
    let arguments = mcp_arguments(options);
    let clients = client_definitions()
        .into_iter()
        .map(|definition| integration_status(&definition, &executable, &arguments))
        .collect();
    Ok(AiIntegrationOverview {
        protocol: "MCP stdio".to_string(),
        server_id: SERVER_ID.to_string(),
        command: executable.to_string_lossy().to_string(),
        args: arguments,
        clients,
    })
}

pub(crate) fn configure(
    options: &Options,
    client_id: &str,
    reset_invalid: bool,
) -> Result<AiClientConfigurationResult, Box<dyn Error>> {
    let executable = env::current_exe()?;
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
    Ok(AiClientConfigurationResult {
        client: integration_status(&definition, &executable, &arguments),
        changed,
        backup_path: backup_path
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default(),
    })
}

pub(crate) fn remove_configuration(
    options: &Options,
    client_id: &str,
) -> Result<AiClientConfigurationResult, Box<dyn Error>> {
    let executable = env::current_exe()?;
    let arguments = mcp_arguments(options);
    let definition = find_client_definition(client_id)?;
    if !definition.path.exists() {
        return Ok(AiClientConfigurationResult {
            client: integration_status(&definition, &executable, &arguments),
            changed: false,
            backup_path: String::new(),
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
    Ok(AiClientConfigurationResult {
        client: integration_status(&definition, &executable, &arguments),
        changed,
        backup_path: backup_path
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default(),
    })
}

pub(crate) fn test_connection(
    options: &Options,
) -> Result<McpConnectionTestResult, Box<dyn Error>> {
    let started = Instant::now();
    let executable = env::current_exe()?;
    let mut child = Command::new(executable)
        .args(mcp_arguments(options))
        .env("HIMIND_AI_CLIENT_ID", "agent-self-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or("MCP self-test stdin is unavailable")?;
        stdin.write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"himind-agent-self-test","version":"1"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
"#,
        )?;
    }
    drop(child.stdin.take());
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "MCP self-test failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let stdout = String::from_utf8(output.stdout)?;
    let mut initialize = None;
    let mut tools = None;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let response: Value = serde_json::from_str(line)?;
        match response.get("id").and_then(Value::as_i64) {
            Some(1) => initialize = response.get("result").cloned(),
            Some(2) => tools = response.get("result").cloned(),
            _ => {}
        }
    }
    let initialize = initialize.ok_or("MCP initialize response is missing")?;
    let tools = tools.ok_or("MCP tools/list response is missing")?;
    Ok(McpConnectionTestResult {
        ok: true,
        server_name: initialize
            .pointer("/serverInfo/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        server_version: initialize
            .pointer("/serverInfo/version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        protocol_version: initialize
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        capability_count: tools
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default(),
        duration_ms: started.elapsed().as_millis(),
    })
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
        .unwrap_or_else(|| home.join(".workbuddy").join(".mcp.json"));
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
) -> AiClientIntegration {
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
    AiClientIntegration {
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
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .eq(arguments.iter().map(String::as_str))
                })
                .unwrap_or(false);
            let client_matches = server
                .get("env")
                .and_then(Item::as_table_like)
                .and_then(|table| table.get("HIMIND_AI_CLIENT_ID"))
                .and_then(Item::as_str)
                == Some(definition.id);
            Ok(command_matches && args_match && client_matches)
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
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .eq(arguments.iter().map(String::as_str))
                })
                .unwrap_or(false);
            let client_matches = server
                .pointer("/env/HIMIND_AI_CLIENT_ID")
                .and_then(Value::as_str)
                == Some(definition.id);
            Ok(command_matches && args_match && client_matches)
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
        "env": { "HIMIND_AI_CLIENT_ID": client_id }
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

fn backup_and_write(path: &Path, content: &[u8]) -> Result<Option<PathBuf>, Box<dyn Error>> {
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
    let Ok(mut child) = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
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

fn workbuddy_executable_exists() -> bool {
    let Some(root) = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("WorkBuddy"))
        .filter(|path| path.is_dir())
    else {
        return false;
    };
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
        command_output_indicates_client, merge_client_config, merge_codex_config,
        merge_json_config, remove_codex_config, remove_json_config, ConfigKind, SERVER_ID,
    };
    use serde_json::Value;
    use std::env;
    use std::fs;
    use std::path::Path;
    use toml_edit::DocumentMut;

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
