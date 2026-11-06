//! Stable loopback router used by the built-in DSH surface.
//!
//! DSH receives one process-lifetime URL and one process-lifetime local token.
//! The upstream service is resolved for every request, so selecting another AI
//! service changes the next request without restarting DSH.

use crate::app::inference_gateway::{GatewayBinding, InferenceGateway};
use crate::store::ai_services::{self, AIServiceProtocol};
use base64::Engine as _;
use rand::RngCore;
use std::sync::{Mutex, OnceLock};

pub(crate) const CLIENT_ID: &str = "himind-ai";
pub(crate) const PROVIDER_ID: &str = "himind-local-ai";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelRoute {
    pub alias: String,
    pub display_name: String,
    pub service_id: String,
    pub model: String,
}

static ROUTER: OnceLock<Mutex<Option<InferenceGateway>>> = OnceLock::new();
static TOKEN: OnceLock<String> = OnceLock::new();

fn router_state() -> &'static Mutex<Option<InferenceGateway>> {
    ROUTER.get_or_init(|| Mutex::new(None))
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn token() -> &'static str {
    TOKEN.get_or_init(random_token).as_str()
}

pub(crate) fn ensure_started() -> Result<(), String> {
    let mut state = router_state()
        .lock()
        .map_err(|_| "DSH 本机 AI 路由状态不可用".to_string())?;
    if state.is_some() {
        return Ok(());
    }
    let gateway = InferenceGateway::start(Some(0), Box::new(gateway_bindings), false)?;
    *state = Some(gateway);
    Ok(())
}

pub(crate) fn api_base_url() -> Result<String, String> {
    ensure_started()?;
    let state = router_state()
        .lock()
        .map_err(|_| "DSH 本机 AI 路由状态不可用".to_string())?;
    let gateway = state
        .as_ref()
        .ok_or_else(|| "DSH 本机 AI 路由尚未启动".to_string())?;
    Ok(format!("{}/v1", gateway.url().trim_end_matches('/')))
}

pub(crate) fn gateway_bindings() -> Vec<GatewayBinding> {
    let mut bindings = Vec::new();
    let Ok(services) = ai_services::list() else {
        return bindings;
    };
    for service in services {
        let Ok((service, api_key)) = ai_services::load_secret(&service.id) else {
            continue;
        };
        let mut models = Vec::with_capacity(service.models.len() + 1);
        models.push(service.model.trim().to_string());
        models.extend(service.models.iter().map(|model| model.trim().to_string()));
        models.retain(|model| !model.is_empty());
        let mut seen = std::collections::HashSet::new();
        models.retain(|model| seen.insert(model.clone()));
        for model in models {
            bindings.push(binding_from_parts(
                &service.id,
                &model,
                &service.protocol,
                &service.base_url,
                &api_key,
                model == service.model,
            ));
        }
    }
    bindings
}

pub(crate) fn model_alias(service_id: &str, model: &str) -> String {
    format!("himind:{}:{}", service_id.trim(), model.trim())
}

/// DSH 会话默认使用的模型别名。取自对话路由的第一项，而不是 `service.model`：
/// 用户在能力路由里给对话钉了具体模型时，DSH 必须拿到同一个模型（ADR 0023）。
pub(crate) fn active_model_alias() -> Result<String, String> {
    let service = ai_services::active_service()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "尚未选择本机 AI 服务".to_string())?;
    let entry = ai_services::capability_routes(ai_services::CAP_CHAT)
        .map_err(|error| error.to_string())?
        .into_iter()
        .next()
        .unwrap_or_else(|| ai_services::CapabilityRouteEntry::new(&service.id, ""));
    let (model, _) = ai_services::route_entry_model(&service, ai_services::CAP_CHAT, &entry);
    let model = model.ok_or_else(|| "尚未选择本机 AI 服务".to_string())?;
    Ok(model_alias(&service.id, &model))
}

pub(crate) fn model_catalog() -> Result<Vec<ModelRoute>, String> {
    let mut routes = Vec::new();
    for service in ai_services::list().map_err(|error| error.to_string())? {
        let mut models = Vec::with_capacity(service.models.len() + 1);
        models.push(service.model.trim().to_string());
        models.extend(service.models.iter().map(|model| model.trim().to_string()));
        models.retain(|model| !model.is_empty());
        let mut seen = std::collections::HashSet::new();
        models.retain(|model| seen.insert(model.clone()));
        for model in models {
            routes.push(ModelRoute {
                alias: model_alias(&service.id, &model),
                display_name: format!("{} · {}", service.display_name.trim(), model),
                service_id: service.id.clone(),
                model,
            });
        }
    }
    Ok(routes)
}

fn binding_from_parts(
    service_id: &str,
    model: &str,
    protocol: &AIServiceProtocol,
    base_url: &str,
    api_key: &str,
    is_default: bool,
) -> GatewayBinding {
    let model = model.trim().to_string();
    GatewayBinding {
        id: format!("{CLIENT_ID}:active:{}", service_id.trim()),
        client: CLIENT_ID.to_string(),
        service: format!("custom:{}", service_id.trim()),
        profile_id: "himind-dsh".to_string(),
        entry_protocol: "openai-chat".to_string(),
        exposed_model: model_alias(service_id, &model),
        models: vec![model_alias(service_id, &model)],
        default_model: model_alias(service_id, &model),
        upstream_model: model,
        is_default,
        protocol: protocol.as_str().to_string(),
        base_url: base_url.trim().to_string(),
        api_key: api_key.to_string(),
        token: token().to_string(),
        platform_metered: false,
    }
}

#[cfg(test)]
mod tests {
    use super::binding_from_parts;
    use crate::store::ai_services::AIServiceProtocol;
    use serde_json::Value;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn active_alias_follows_the_chat_route_model_not_the_service_default() {
        let _env_guard = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-dsh-router-alias-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let previous_home = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        let result = (|| {
            crate::store::ai_services::upsert(crate::store::ai_services::CustomAIServiceInput {
                id: "multi".to_string(),
                display_name: "多模型".to_string(),
                base_url: "https://multi.example/v1".to_string(),
                protocol: AIServiceProtocol::OpenaiChat,
                model: "chat-default".to_string(),
                models: vec!["chat-default".to_string(), "chat-pinned".to_string()],
                image_model: String::new(),
                image_adapter: String::new(),
                media: Default::default(),
                api_key: "secret-multi".to_string(),
            })
            .unwrap();

            // 没配路由时跟着服务默认模型。
            crate::store::ai_services::set_active("multi").unwrap();
            assert_eq!(
                super::active_model_alias().unwrap(),
                "himind:multi:chat-default"
            );

            // 路由里钉了具体模型时，DSH 必须拿到同一个模型（ADR 0023）。
            crate::store::ai_services::set_capability_routes(
                crate::store::ai_services::CAP_CHAT,
                vec![crate::store::ai_services::CapabilityRouteEntry::new(
                    "multi",
                    "chat-pinned",
                )],
            )
            .unwrap();
            assert_eq!(
                super::active_model_alias().unwrap(),
                "himind:multi:chat-pinned"
            );
        })();
        match previous_home {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
        result
    }

    #[test]
    fn binding_tracks_the_active_service_behind_a_stable_client_token() {
        let binding = binding_from_parts(
            "vector-tide",
            "deepseek-v4.1-flash-1017",
            &AIServiceProtocol::OpenaiResponses,
            "https://vectide.cn/v1",
            "upstream-secret",
            true,
        );
        assert_eq!(binding.client, "himind-ai");
        assert_eq!(binding.entry_protocol, "openai-chat");
        assert_eq!(binding.protocol, "openai-responses");
        assert_eq!(
            binding.exposed_model,
            "himind:vector-tide:deepseek-v4.1-flash-1017"
        );
        assert_eq!(
            binding.default_model,
            "himind:vector-tide:deepseek-v4.1-flash-1017"
        );
        assert_eq!(binding.upstream_model, "deepseek-v4.1-flash-1017");
        assert_eq!(
            binding.models,
            vec!["himind:vector-tide:deepseek-v4.1-flash-1017".to_string()]
        );
        assert_eq!(binding.base_url, "https://vectide.cn/v1");
        assert_eq!(binding.api_key, "upstream-secret");
        assert_eq!(binding.token, super::token());
    }

    fn spawn_upstream() -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let body_start = loop {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0, "gateway closed before sending a request");
                request.extend_from_slice(&buffer[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let head = String::from_utf8_lossy(&request[..body_start]).to_string();
            let content_length = head
                .lines()
                .find_map(|line| {
                    line.split_once(':').and_then(|(name, value)| {
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                })
                .unwrap_or(0);
            while request.len() < body_start + content_length {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0, "gateway closed before sending the full body");
                request.extend_from_slice(&buffer[..read]);
            }
            let body = String::from_utf8(request[body_start..body_start + content_length].to_vec())
                .unwrap();
            let model = serde_json::from_str::<Value>(&body).unwrap()["model"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let response = format!(
                "{{\"id\":\"resp-upstream\",\"object\":\"chat.completion\",\"model\":\"{model}\",\"choices\":[{{\"index\":0,\"message\":{{\"role\":\"assistant\",\"content\":\"ok\"}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}}}"
            );
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
            body
        });
        (format!("http://{address}/v1"), worker)
    }

    fn send_chat(base_url: &str, model: &str) -> String {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let response = client
            .post(format!("{base_url}/chat/completions"))
            .bearer_auth(super::token())
            .json(&serde_json::json!({
                "model": model,
                "messages": [{ "role": "user", "content": "hello" }],
                "max_tokens": 1
            }))
            .send()
            .unwrap();
        assert!(response.status().is_success());
        response.text().unwrap()
    }

    #[test]
    fn active_service_switch_reroutes_live_requests_without_restarting_router() {
        let _env_guard = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-dsh-router-switch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let previous_home = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        let result = (|| {
            let (base_a, upstream_a) = spawn_upstream();
            let (base_b, upstream_b) = spawn_upstream();
            for (id, base_url, model) in [
                ("switch-a", base_a.as_str(), "model-a"),
                ("switch-b", base_b.as_str(), "model-b"),
            ] {
                crate::store::ai_services::upsert(
                    crate::store::ai_services::CustomAIServiceInput {
                        id: id.to_string(),
                        display_name: id.to_string(),
                        base_url: base_url.to_string(),
                        protocol: AIServiceProtocol::OpenaiChat,
                        model: model.to_string(),
                        models: vec![model.to_string()],
                        image_model: String::new(),
                        image_adapter: String::new(),
                        media: Default::default(),
                        api_key: format!("secret-{id}"),
                    },
                )
                .unwrap();
            }
            crate::store::ai_services::set_active("switch-a").unwrap();
            let gateway_base = super::api_base_url().unwrap();
            assert!(send_chat(&gateway_base, "himind:switch-a:model-a")
                .contains("himind:switch-a:model-a"));
            assert!(upstream_a.join().unwrap().contains("\"model\":\"model-a\""));

            // The provider catalog can gain services while DSH is already
            // running. The resolver is invoked per request, so the new alias
            // must work immediately without rebuilding the listener.
            let (base_c, upstream_c) = spawn_upstream();
            crate::store::ai_services::upsert(crate::store::ai_services::CustomAIServiceInput {
                id: "switch-c".to_string(),
                display_name: "Switch C".to_string(),
                base_url: base_c,
                protocol: AIServiceProtocol::OpenaiChat,
                model: "model-c".to_string(),
                models: vec!["model-c".to_string(), "model-c-fast".to_string()],
                image_model: String::new(),
                image_adapter: String::new(),
                media: Default::default(),
                api_key: "secret-switch-c".to_string(),
            })
            .unwrap();
            assert!(send_chat(&gateway_base, "himind:switch-c:model-c-fast")
                .contains("himind:switch-c:model-c-fast"));
            assert!(upstream_c
                .join()
                .unwrap()
                .contains("\"model\":\"model-c-fast\""));

            crate::store::ai_services::set_active("switch-b").unwrap();
            assert!(send_chat(&gateway_base, "himind:switch-b:model-b")
                .contains("himind:switch-b:model-b"));
            assert!(upstream_b.join().unwrap().contains("\"model\":\"model-b\""));
        })();
        match previous_home {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
        result
    }
}
