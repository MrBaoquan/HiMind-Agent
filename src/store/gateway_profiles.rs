//! 本机 AI 网关的聚合 Profile。
//!
//! Profile 只保存“哪些服务的哪些模型对客户端暴露”以及稳定别名，不保存 API Key。
//! 网关启动或绑定解析时再通过 `ai_services` 读取真实上游配置和 DPAPI 凭据。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::store::ai_services::{self, AIServiceProtocol, CustomAIService};

const STORE_FILE: &str = "ai-gateway-profiles.json";
const SCHEMA_VERSION: u32 = 1;

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct GatewayModelRoute {
    pub exposed_model: String,
    pub service_id: String,
    pub upstream_model: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct GatewayProfile {
    pub id: String,
    pub name: String,
    pub entry_protocol: AIServiceProtocol,
    /// 同一 Profile 可同时服务的客户端入口协议。
    #[serde(default)]
    pub entry_protocols: Vec<AIServiceProtocol>,
    pub default_model: String,
    pub enabled: bool,
    pub routes: Vec<GatewayModelRoute>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct GatewayModelRouteInput {
    #[serde(default)]
    pub exposed_model: String,
    pub service_id: String,
    pub upstream_model: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct GatewayProfileInput {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub entry_protocol: String,
    #[serde(default)]
    pub entry_protocols: Vec<String>,
    #[serde(default)]
    pub default_model: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub routes: Vec<GatewayModelRouteInput>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedGatewayProfiles {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    profiles: BTreeMap<String, GatewayProfile>,
}

pub(crate) fn store_path() -> Result<PathBuf, Box<dyn Error>> {
    let dir = crate::store::paths::agent_home();
    fs::create_dir_all(&dir)?;
    Ok(dir.join(STORE_FILE))
}

fn load_store() -> Result<PersistedGatewayProfiles, Box<dyn Error>> {
    let path = store_path()?;
    if !path.exists() {
        return Ok(PersistedGatewayProfiles::default());
    }
    let mut store: PersistedGatewayProfiles = serde_json::from_slice(&fs::read(path)?)?;
    if store.schema_version > SCHEMA_VERSION {
        return Err(format!(
            "网关聚合配置版本 {} 高于当前 Agent 支持的版本 {}",
            store.schema_version, SCHEMA_VERSION
        )
        .into());
    }
    store.schema_version = SCHEMA_VERSION;
    for profile in store.profiles.values_mut() {
        if profile.entry_protocols.is_empty() {
            profile.entry_protocols.push(profile.entry_protocol.clone());
        }
    }
    Ok(store)
}

fn save_store(store: &PersistedGatewayProfiles) -> Result<(), Box<dyn Error>> {
    let path = store_path()?;
    crate::store::atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(store)?)?;
    Ok(())
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

fn normalize_profile_id(value: &str) -> Result<String, Box<dyn Error>> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() || value.len() > 64 {
        return Err("聚合入口 ID 必须为 1-64 个字符".into());
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err("聚合入口 ID 只允许字母、数字、下划线和连字符".into());
    }
    Ok(value)
}

fn normalize_exposed_model(value: &str) -> Result<String, Box<dyn Error>> {
    let value = value.trim();
    if value.is_empty() || value.len() > 200 {
        return Err("对外模型 ID 必须为 1-200 个字符".into());
    }
    if value
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(format!("对外模型 ID 不能包含空白或控制字符：{value}").into());
    }
    Ok(value.to_string())
}

/// 默认别名：`service_id/upstream_model`。调用方仍可传入更短的自定义别名。
pub(crate) fn suggested_alias(service_id: &str, upstream_model: &str) -> String {
    format!(
        "{}/{}",
        service_id.trim().trim_matches('/'),
        upstream_model.trim().trim_matches('/')
    )
}

fn service_has_model(service: &CustomAIService, model: &str) -> bool {
    service.model.trim() == model
        || service
            .models
            .iter()
            .any(|candidate| candidate.trim() == model)
}

pub(crate) fn protocol_supported(entry: &AIServiceProtocol, upstream: &AIServiceProtocol) -> bool {
    matches!(
        (entry, upstream),
        (AIServiceProtocol::OpenaiChat, AIServiceProtocol::OpenaiChat)
            | (
                AIServiceProtocol::OpenaiResponses,
                AIServiceProtocol::OpenaiResponses | AIServiceProtocol::OpenaiChat
            )
            | (
                AIServiceProtocol::Anthropic,
                AIServiceProtocol::Anthropic
                    | AIServiceProtocol::OpenaiChat
                    | AIServiceProtocol::OpenaiResponses
            )
    )
}

fn validate_routes(
    inputs: Vec<GatewayModelRouteInput>,
    entry_protocols: &[AIServiceProtocol],
    services: &BTreeMap<String, CustomAIService>,
) -> Result<Vec<GatewayModelRoute>, Box<dyn Error>> {
    if inputs.is_empty() {
        return Err("聚合入口至少需要一个模型路由".into());
    }
    let mut exposed = HashSet::new();
    let mut routes = Vec::with_capacity(inputs.len());
    for (index, input) in inputs.into_iter().enumerate() {
        let service_id = input.service_id.trim().to_string();
        let service = services
            .get(&service_id)
            .ok_or_else(|| format!("第 {} 条路由引用的 AI 服务不存在：{service_id}", index + 1))?;
        let upstream_model = input.upstream_model.trim().to_string();
        if upstream_model.is_empty() {
            return Err(format!("第 {} 条路由缺少上游模型", index + 1).into());
        }
        if !entry_protocols
            .iter()
            .any(|entry| protocol_supported(entry, &service.protocol))
        {
            return Err(format!(
                "模型 {upstream_model} 不兼容当前任何客户端入口协议（服务 {service_id} 使用 {}）",
                service.protocol.as_str()
            )
            .into());
        }
        if !service_has_model(service, &upstream_model) {
            return Err(format!(
                "服务 {service_id} 未声明模型 {upstream_model}，请先刷新服务模型列表"
            )
            .into());
        }
        let exposed_model = if input.exposed_model.trim().is_empty() {
            suggested_alias(&service_id, &upstream_model)
        } else {
            input.exposed_model
        };
        let exposed_model = normalize_exposed_model(&exposed_model)?;
        if !exposed.insert(exposed_model.clone()) {
            return Err(format!("对外模型 ID 重复：{exposed_model}").into());
        }
        routes.push(GatewayModelRoute {
            exposed_model,
            service_id,
            upstream_model,
            enabled: input.enabled,
        });
    }
    Ok(routes)
}

pub(crate) fn list() -> Result<Vec<GatewayProfile>, Box<dyn Error>> {
    let mut profiles = load_store()?.profiles.into_values().collect::<Vec<_>>();
    profiles.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(profiles)
}

pub(crate) fn get(id: &str) -> Result<GatewayProfile, Box<dyn Error>> {
    let id = normalize_profile_id(id)?;
    load_store()?
        .profiles
        .remove(&id)
        .ok_or_else(|| format!("聚合入口不存在：{id}").into())
}

pub(crate) fn save(input: GatewayProfileInput) -> Result<GatewayProfile, Box<dyn Error>> {
    let id = normalize_profile_id(&input.id)?;
    let name = input.name.trim().to_string();
    if name.is_empty() || name.len() > 80 {
        return Err("聚合入口名称必须为 1-80 个字符".into());
    }
    let mut entry_protocols = Vec::new();
    for value in &input.entry_protocols {
        let protocol = AIServiceProtocol::parse(value)?;
        if !entry_protocols.contains(&protocol) {
            entry_protocols.push(protocol);
        }
    }
    if entry_protocols.is_empty() && !input.entry_protocol.trim().is_empty() {
        entry_protocols.push(AIServiceProtocol::parse(&input.entry_protocol)?);
    }
    if entry_protocols.is_empty() {
        return Err("聚合入口至少需要一个客户端协议".into());
    }
    let entry_protocol = entry_protocols[0].clone();
    let services = ai_services::list()?
        .into_iter()
        .map(|service| (service.id.clone(), service))
        .collect::<BTreeMap<_, _>>();
    let routes = validate_routes(input.routes, &entry_protocols, &services)?;
    if input.enabled && !routes.iter().any(|route| route.enabled) {
        return Err("启用的聚合入口至少需要一个启用的模型路由".into());
    }
    for entry in &entry_protocols {
        let compatible = routes
            .iter()
            .filter(|route| {
                services
                    .get(&route.service_id)
                    .is_some_and(|service| protocol_supported(entry, &service.protocol))
            })
            .collect::<Vec<_>>();
        if compatible.is_empty() {
            return Err(format!(
                "客户端协议 {} 没有兼容的模型，请添加对应上游协议的服务",
                entry.as_str()
            )
            .into());
        }
        if input.enabled && !compatible.iter().any(|route| route.enabled) {
            return Err(format!(
                "客户端协议 {} 没有启用中的兼容模型，客户端绑定后会看不到模型",
                entry.as_str()
            )
            .into());
        }
    }
    let default_model = input.default_model.trim().to_string();
    let default_model = if default_model.is_empty() {
        routes
            .iter()
            .find(|route| route.enabled)
            .or_else(|| routes.first())
            .map(|route| route.exposed_model.clone())
            .unwrap_or_default()
    } else {
        normalize_exposed_model(&default_model)?
    };
    if !routes
        .iter()
        .any(|route| route.enabled && route.exposed_model == default_model)
    {
        return Err("默认模型必须是已启用的对外模型 ID".into());
    }

    let now = now_stamp();
    let mut store = load_store()?;
    let created_at = store
        .profiles
        .get(&id)
        .map(|profile| profile.created_at.clone())
        .unwrap_or_else(|| now.clone());
    let profile = GatewayProfile {
        id: id.clone(),
        name,
        entry_protocol,
        entry_protocols,
        default_model,
        enabled: input.enabled,
        routes,
        created_at,
        updated_at: now,
    };
    store.schema_version = SCHEMA_VERSION;
    store.profiles.insert(id, profile.clone());
    save_store(&store)?;
    Ok(profile)
}

pub(crate) fn remove(id: &str) -> Result<bool, Box<dyn Error>> {
    let id = normalize_profile_id(id)?;
    let mut store = load_store()?;
    let removed = store.profiles.remove(&id).is_some();
    if removed {
        save_store(&store)?;
    }
    Ok(removed)
}

/// 某个服务被哪些 Profile 引用。删除 AI Service 前必须调用。
pub(crate) fn profiles_referencing_service(
    service_id: &str,
) -> Result<Vec<String>, Box<dyn Error>> {
    let service_id = service_id.trim();
    let mut ids = load_store()?
        .profiles
        .into_values()
        .filter(|profile| {
            profile
                .routes
                .iter()
                .any(|route| route.service_id == service_id)
        })
        .map(|profile| profile.id)
        .collect::<Vec<_>>();
    ids.sort();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ai_services::{CustomAIServiceInput, MediaModelBinding};

    fn with_isolated_home(run: impl FnOnce()) {
        let _guard = crate::store::paths::test_env_lock();
        let previous = std::env::var("HIMIND_AGENT_HOME").ok();
        let root = std::env::temp_dir().join(format!(
            "himind-gateway-profiles-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        run();
        let _ = fs::remove_dir_all(&root);
        match previous {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
    }

    fn add_service(id: &str, protocol: AIServiceProtocol, models: &[&str]) {
        ai_services::upsert(CustomAIServiceInput {
            id: id.to_string(),
            display_name: format!("{id} 服务"),
            base_url: "https://example.com/v1".to_string(),
            protocol,
            model: models.first().copied().unwrap_or_default().to_string(),
            models: models.iter().map(|model| model.to_string()).collect(),
            image_model: String::new(),
            image_adapter: String::new(),
            media: BTreeMap::<String, MediaModelBinding>::new(),
            api_key: format!("sk-{id}"),
        })
        .expect("add service");
    }

    fn profile_input(id: &str, routes: Vec<GatewayModelRouteInput>) -> GatewayProfileInput {
        GatewayProfileInput {
            id: id.to_string(),
            name: "编码模型聚合".to_string(),
            entry_protocol: "openai-responses".to_string(),
            entry_protocols: Vec::new(),
            default_model: String::new(),
            enabled: true,
            routes,
        }
    }

    #[test]
    fn profile_round_trips_and_alias_is_generated() {
        with_isolated_home(|| {
            add_service(
                "deepseek",
                AIServiceProtocol::OpenaiResponses,
                &["deepseek-v4-pro", "deepseek-v4-flash"],
            );
            let saved = save(profile_input(
                "coding",
                vec![GatewayModelRouteInput {
                    exposed_model: String::new(),
                    service_id: "deepseek".to_string(),
                    upstream_model: "deepseek-v4-pro".to_string(),
                    enabled: true,
                }],
            ))
            .expect("save profile");
            assert_eq!(saved.routes[0].exposed_model, "deepseek/deepseek-v4-pro");
            assert_eq!(saved.default_model, "deepseek/deepseek-v4-pro");
            assert_eq!(get("coding").unwrap(), saved);
            assert_eq!(list().unwrap().len(), 1);
        });
    }

    #[test]
    fn rejects_duplicate_exposed_model() {
        with_isolated_home(|| {
            add_service("a", AIServiceProtocol::OpenaiChat, &["m1", "m2"]);
            let error = save(profile_input(
                "duplicate",
                vec![
                    GatewayModelRouteInput {
                        exposed_model: "same".to_string(),
                        service_id: "a".to_string(),
                        upstream_model: "m1".to_string(),
                        enabled: true,
                    },
                    GatewayModelRouteInput {
                        exposed_model: "same".to_string(),
                        service_id: "a".to_string(),
                        upstream_model: "m2".to_string(),
                        enabled: true,
                    },
                ],
            ))
            .unwrap_err();
            assert!(error.to_string().contains("重复"));
        });
    }

    #[test]
    fn rejects_unknown_service_or_model() {
        with_isolated_home(|| {
            add_service("a", AIServiceProtocol::OpenaiChat, &["m1"]);
            let missing_service = save(profile_input(
                "missing-service",
                vec![GatewayModelRouteInput {
                    exposed_model: "x".to_string(),
                    service_id: "missing".to_string(),
                    upstream_model: "m1".to_string(),
                    enabled: true,
                }],
            ))
            .unwrap_err();
            assert!(missing_service.to_string().contains("不存在"));

            let missing_model = save(profile_input(
                "missing-model",
                vec![GatewayModelRouteInput {
                    exposed_model: "x".to_string(),
                    service_id: "a".to_string(),
                    upstream_model: "m2".to_string(),
                    enabled: true,
                }],
            ))
            .unwrap_err();
            assert!(missing_model.to_string().contains("未声明模型"));
        });
    }

    #[test]
    fn rejects_protocol_pair_without_translation() {
        with_isolated_home(|| {
            add_service("claude", AIServiceProtocol::Anthropic, &["claude"]);
            let error = save(profile_input(
                "responses-with-anthropic",
                vec![GatewayModelRouteInput {
                    exposed_model: "claude/claude".to_string(),
                    service_id: "claude".to_string(),
                    upstream_model: "claude".to_string(),
                    enabled: true,
                }],
            ))
            .unwrap_err();
            assert!(error.to_string().contains("不兼容当前任何客户端入口协议"));
        });
    }
    #[test]
    fn multi_protocol_profile_accepts_shared_openai_chat_upstream() {
        with_isolated_home(|| {
            add_service("shared", AIServiceProtocol::OpenaiChat, &["shared-model"]);
            let mut input = profile_input(
                "multi-client",
                vec![GatewayModelRouteInput {
                    exposed_model: "shared/shared-model".to_string(),
                    service_id: "shared".to_string(),
                    upstream_model: "shared-model".to_string(),
                    enabled: true,
                }],
            );
            input.entry_protocol = "openai-responses".to_string();
            input.entry_protocols = vec![
                "openai-responses".to_string(),
                "anthropic".to_string(),
                "openai-chat".to_string(),
            ];
            let saved = save(input).expect("multi-protocol profile should save");
            assert_eq!(saved.entry_protocol, AIServiceProtocol::OpenaiResponses);
            assert_eq!(
                saved.entry_protocols,
                vec![
                    AIServiceProtocol::OpenaiResponses,
                    AIServiceProtocol::Anthropic,
                    AIServiceProtocol::OpenaiChat,
                ]
            );
        });
    }

    #[test]
    fn multi_protocol_profile_requires_a_route_for_each_entry() {
        with_isolated_home(|| {
            add_service("claude", AIServiceProtocol::Anthropic, &["claude-model"]);
            let mut input = profile_input(
                "responses-and-anthropic",
                vec![GatewayModelRouteInput {
                    exposed_model: "claude/claude-model".to_string(),
                    service_id: "claude".to_string(),
                    upstream_model: "claude-model".to_string(),
                    enabled: true,
                }],
            );
            input.entry_protocols = vec!["openai-responses".to_string(), "anthropic".to_string()];
            let error = save(input).unwrap_err();
            assert!(error.to_string().contains("客户端协议 openai-responses"));
        });
    }

    #[test]
    fn anthropic_entry_supports_responses_upstream() {
        assert!(protocol_supported(
            &AIServiceProtocol::Anthropic,
            &AIServiceProtocol::OpenaiResponses
        ));
        assert!(protocol_supported(
            &AIServiceProtocol::Anthropic,
            &AIServiceProtocol::OpenaiChat
        ));
        assert!(!protocol_supported(
            &AIServiceProtocol::OpenaiChat,
            &AIServiceProtocol::OpenaiResponses
        ));
    }

    #[test]
    fn mixed_client_protocols_accept_distinct_upstream_routes() {
        with_isolated_home(|| {
            add_service("claude", AIServiceProtocol::Anthropic, &["claude-model"]);
            add_service("gpt", AIServiceProtocol::OpenaiResponses, &["gpt-model"]);
            let mut input = profile_input(
                "mixed",
                vec![
                    GatewayModelRouteInput {
                        exposed_model: "claude/claude-model".to_string(),
                        service_id: "claude".to_string(),
                        upstream_model: "claude-model".to_string(),
                        enabled: true,
                    },
                    GatewayModelRouteInput {
                        exposed_model: "gpt/gpt-model".to_string(),
                        service_id: "gpt".to_string(),
                        upstream_model: "gpt-model".to_string(),
                        enabled: true,
                    },
                ],
            );
            input.entry_protocols = vec!["openai-responses".to_string(), "anthropic".to_string()];
            let saved = save(input).expect("mixed protocol routes should save");
            assert_eq!(saved.routes.len(), 2);
            assert_eq!(saved.entry_protocols.len(), 2);
        });
    }

    #[test]
    fn legacy_profile_is_backfilled_with_its_entry_protocol() {
        with_isolated_home(|| {
            let legacy = serde_json::json!({
                "schema_version": 1,
                "profiles": {
                    "legacy": {
                        "id": "legacy",
                        "name": "旧版入口",
                        "entry_protocol": "openai-responses",
                        "default_model": "old/model",
                        "enabled": true,
                        "routes": [{
                            "exposed_model": "old/model",
                            "service_id": "old",
                            "upstream_model": "model",
                            "enabled": true
                        }],
                        "created_at": "1",
                        "updated_at": "2"
                    }
                }
            });
            fs::write(
                store_path().unwrap(),
                serde_json::to_vec_pretty(&legacy).unwrap(),
            )
            .unwrap();
            let profiles = list().expect("legacy profile should load");
            assert_eq!(profiles.len(), 1);
            assert_eq!(
                profiles[0].entry_protocols,
                vec![AIServiceProtocol::OpenaiResponses]
            );
        });
    }

    #[test]
    fn service_references_are_discoverable_and_remove_is_idempotent() {
        with_isolated_home(|| {
            add_service("a", AIServiceProtocol::OpenaiResponses, &["claude"]);
            save(profile_input(
                "p1",
                vec![GatewayModelRouteInput {
                    exposed_model: "a/claude".to_string(),
                    service_id: "a".to_string(),
                    upstream_model: "claude".to_string(),
                    enabled: true,
                }],
            ))
            .unwrap();
            assert_eq!(profiles_referencing_service("a").unwrap(), vec!["p1"]);
            assert!(remove("p1").unwrap());
            assert!(!remove("p1").unwrap());
            assert!(profiles_referencing_service("a").unwrap().is_empty());
        });
    }
}
