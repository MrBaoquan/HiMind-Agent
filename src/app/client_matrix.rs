//! 客户端能力矩阵的收口层。
//!
//! `skill::clients` 声明"每个客户端能做到什么"（静态、跨机器一致），本模块只把
//! "本机此刻能不能用"（运行期）叠加到同一份矩阵上，并保证叠加后的结果仍然满足
//! `contracts/agent-core/v1/client-capability-matrix.schema.json`。
//!
//! 两者分开的原因：能力是产品契约，可用性随安装情况变化。混在一起会让每个
//! 消费者各自猜测"这条能力是不是因为没装才没出现"。

use crate::skill::clients::{
    directory_client, ClientDefinition, DIRECTORY_CLIENTS, HOST_CLIENTS, HOST_CLIENT_HIMIND_AI,
};
use crate::Options;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};

/// MCP 侧的运行期状态，来自 `app::mcp_targets`。
#[derive(Clone, Debug, Default)]
pub(crate) struct McpClientState {
    pub state: String,
    pub detected: bool,
}

/// 叠加可用性所需的全部运行期输入。抽成结构体是为了让覆盖层可以在测试里
/// 用确定的输入验证，而不必真的去探测本机。
#[derive(Clone, Debug, Default)]
pub(crate) struct AvailabilityInput {
    pub detected_clients: BTreeSet<String>,
    pub mcp_clients: BTreeMap<String, McpClientState>,
    pub workspace_root: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub agent_skill_root: PathBuf,
}

/// 静态矩阵 + 运行期可用性覆盖层。
pub(crate) fn matrix_json(options: &Options) -> Value {
    apply_availability(static_matrix_json(), &collect_input(options))
}

/// 纯函数部分：给矩阵的每个客户端挂上 `availability`。
pub(crate) fn apply_availability(mut matrix: Value, input: &AvailabilityInput) -> Value {
    let Some(clients) = matrix.get_mut("clients").and_then(Value::as_array_mut) else {
        return matrix;
    };
    for client in clients.iter_mut() {
        let Some(id) = client.get("id").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        let Some(definition) = definition_for(&id) else {
            continue;
        };
        if let Some(object) = client.as_object_mut() {
            object.insert(
                "availability".to_string(),
                availability_for(definition, input),
            );
        }
    }
    matrix
}

fn definition_for(client_id: &str) -> Option<&'static ClientDefinition> {
    HOST_CLIENTS
        .iter()
        .chain(DIRECTORY_CLIENTS.iter())
        .find(|client| client.id == client_id)
        .or_else(|| directory_client(client_id))
}

fn collect_input(options: &Options) -> AvailabilityInput {
    let mut detected_clients = BTreeSet::new();
    for client_id in crate::skill::direct::active_client_ids() {
        detected_clients.insert(client_id.to_string());
    }
    if crate::skill::codex::is_detected() {
        detected_clients.insert(crate::skill::clients::HOST_CLIENT_CODEX.to_string());
    }
    detected_clients.insert(HOST_CLIENT_HIMIND_AI.to_string());
    for (client_id, detected, _) in crate::app::ai_clients::detected_clients() {
        if detected {
            detected_clients.insert(client_id);
        }
    }

    let mut mcp_clients = BTreeMap::new();
    if let Ok(targets) = crate::app::mcp_targets::list(options) {
        for target in targets {
            let client_id = if target.skill_client_id.trim().is_empty() {
                target.id.clone()
            } else {
                target.skill_client_id.clone()
            };
            // 一个客户端可能对应多个 MCP 目标（例如 VS Code 与 Insiders）。
            // 只要有一个已经配好，这个客户端就算配好了。
            let candidate = McpClientState {
                state: target.state,
                detected: target.detected,
            };
            mcp_clients
                .entry(client_id)
                .and_modify(|existing: &mut McpClientState| {
                    if !mcp_state_is_configured(&existing.state)
                        && mcp_state_is_configured(&candidate.state)
                    {
                        *existing = candidate.clone();
                    }
                })
                .or_insert(candidate);
        }
    }

    AvailabilityInput {
        detected_clients,
        mcp_clients,
        workspace_root: crate::skill::target::resolve_workspace_root(None)
            .ok()
            .flatten(),
        home: home_directory(),
        agent_skill_root: crate::skill::store::SkillStore::new().root().to_path_buf(),
    }
}

fn home_directory() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
}

fn mcp_state_is_configured(state: &str) -> bool {
    matches!(state, "configured" | "managed")
}

fn availability_for(client: &ClientDefinition, input: &AvailabilityInput) -> Value {
    let mcp = input.mcp_clients.get(client.id);
    let detected = input.detected_clients.contains(client.id) || mcp.is_some_and(|m| m.detected);
    let mcp_broken = mcp.is_some_and(|value| {
        !mcp_state_is_configured(&value.state)
            && !value.state.is_empty()
            && value.state != "not_configured"
    });

    let (state, detail) =
        if client.skills.is_none() && client.mcp.is_none() && client.plugins.is_none() {
            (
                "unsupported".to_string(),
                "该客户端没有可用的 HiMind 能力".to_string(),
            )
        } else if mcp_broken {
            ("not_configured".to_string(), "MCP 配置需要修复".to_string())
        } else if !detected {
            ("not_installed".to_string(), "未检测到本机安装".to_string())
        } else {
            ("ready".to_string(), ready_detail(client, mcp))
        };

    let (scope, resolved_target) = resolve_scope(client, input);
    let mut availability = Map::new();
    availability.insert("state".to_string(), json!(state));
    availability.insert("detail".to_string(), json!(detail));
    availability.insert("detected".to_string(), json!(detected));
    availability.insert(
        "configured".to_string(),
        json!(mcp.is_some_and(|value| mcp_state_is_configured(&value.state))),
    );
    availability.insert("scope".to_string(), json!(scope));
    if let Some(target) = resolved_target {
        availability.insert(
            "resolved_target".to_string(),
            json!(target.to_string_lossy()),
        );
    }
    Value::Object(availability)
}

fn ready_detail(client: &ClientDefinition, mcp: Option<&McpClientState>) -> String {
    match mcp {
        Some(value) if mcp_state_is_configured(&value.state) => {
            "已注册 MCP，可直接使用".to_string()
        }
        Some(_) => "已安装，尚未注册 MCP".to_string(),
        None if client.id == HOST_CLIENT_HIMIND_AI => "Agent 内置技能库，无需安装".to_string(),
        None => "技能目录可用".to_string(),
    }
}

fn resolve_scope(
    client: &ClientDefinition,
    input: &AvailabilityInput,
) -> (&'static str, Option<PathBuf>) {
    if client.id == HOST_CLIENT_HIMIND_AI {
        let target = if input.agent_skill_root.as_os_str().is_empty() {
            None
        } else {
            Some(input.agent_skill_root.clone())
        };
        return ("agent", target);
    }
    if client.skills.is_none() {
        return ("none", None);
    }
    if let (Some(root), Some(directory)) = (&input.workspace_root, client.skill_project_dir()) {
        return ("project", Some(join(root, directory)));
    }
    if let (Some(home), Some(directory)) = (&input.home, client.skill_user_dir()) {
        return ("user", Some(join(home, directory)));
    }
    ("none", None)
}

fn join(root: &Path, directory: &str) -> PathBuf {
    // 客户端目录常量使用 `/` 分隔，Windows 上直接 join 会把整串当成一个文件名。
    directory
        .split('/')
        .filter(|segment| !segment.is_empty())
        .fold(root.to_path_buf(), |path, segment| path.join(segment))
}

fn static_matrix_json() -> Value {
    crate::skill::clients::static_matrix_json()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::clients::{MATRIX_SCHEMA_VERSION, SKILL_STANDARD_AGENT_SKILLS};

    fn schema_validator() -> jsonschema::Validator {
        let schema: Value = serde_json::from_str(include_str!(
            "../../contracts/agent-core/v1/client-capability-matrix.schema.json"
        ))
        .unwrap();
        jsonschema::validator_for(&schema).unwrap()
    }

    fn empty_input() -> AvailabilityInput {
        // `collect_input` 永远把 HiMind AI 自己算作已就绪：Agent 就是它的宿主。
        let detected_clients = [HOST_CLIENT_HIMIND_AI.to_string()].into_iter().collect();
        AvailabilityInput {
            detected_clients,
            agent_skill_root: PathBuf::new(),
            ..AvailabilityInput::default()
        }
    }

    #[test]
    fn static_matrix_matches_contract_schema() {
        let matrix = static_matrix_json();
        assert_eq!(matrix["schema_version"], json!(MATRIX_SCHEMA_VERSION));
        assert!(
            schema_validator().is_valid(&matrix),
            "static matrix failed schema validation: {matrix:#?}"
        );
    }

    #[test]
    fn availability_layer_keeps_matrix_schema_valid() {
        let matrix = apply_availability(static_matrix_json(), &empty_input());
        let validator = schema_validator();
        assert!(
            validator.is_valid(&matrix),
            "matrix with availability failed schema validation: {matrix:#?}"
        );
        for client in matrix["clients"].as_array().unwrap() {
            let id = client["id"].as_str().unwrap();
            let state = client["availability"]["state"].as_str().unwrap();
            assert_eq!(
                state,
                if id == HOST_CLIENT_HIMIND_AI {
                    "ready"
                } else {
                    "not_installed"
                },
                "{id} has an unexpected state on an empty machine"
            );
        }
    }

    #[test]
    fn detected_client_with_broken_mcp_config_is_not_configured() {
        let mut input = empty_input();
        input.detected_clients.insert("qoder".to_string());
        assert_eq!(
            state_of(&apply_availability(static_matrix_json(), &input), "qoder"),
            "ready"
        );

        input.mcp_clients.insert(
            "qoder".to_string(),
            McpClientState {
                state: "invalid_config".to_string(),
                detected: true,
            },
        );
        let matrix = apply_availability(static_matrix_json(), &input);
        assert_eq!(state_of(&matrix, "qoder"), "not_configured");
        let qoder = client_of(&matrix, "qoder");
        assert_eq!(qoder["availability"]["configured"], json!(false));
        assert_eq!(qoder["availability"]["detected"], json!(true));
    }

    #[test]
    fn project_scope_resolves_to_the_workspace_skill_directory() {
        let mut input = empty_input();
        input.detected_clients.insert("qoder".to_string());
        input.workspace_root = Some(PathBuf::from("C:/repo"));
        let matrix = apply_availability(static_matrix_json(), &input);
        let qoder = client_of(&matrix, "qoder");
        assert_eq!(qoder["availability"]["scope"], json!("project"));
        assert_eq!(
            qoder["availability"]["resolved_target"].as_str(),
            Path::new("C:/repo").join(".qoder").join("skills").to_str()
        );
        assert_eq!(
            client_of(&matrix, "qoder")["capabilities"]["skills"]["standard"],
            json!(SKILL_STANDARD_AGENT_SKILLS)
        );
    }

    fn client_of<'a>(matrix: &'a Value, client_id: &'a str) -> &'a Value {
        matrix["clients"]
            .as_array()
            .unwrap()
            .iter()
            .find(|client| client["id"] == json!(client_id))
            .unwrap_or_else(|| panic!("client {client_id} missing from matrix"))
    }

    fn state_of(matrix: &Value, client_id: &str) -> String {
        client_of(matrix, client_id)["availability"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    }
}
