use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use super::credentials::{protect_secret_for_current_user, unprotect_secret_for_current_user};

const STORE_FILE: &str = "ai-services.json";
const SELECTION_FILE: &str = "ai-service-selection.json";

/// Anthropic Messages 协议要求的版本头；`/v1/models` 与 `/v1/messages` 共用。
const ANTHROPIC_API_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AIServiceProtocol {
    #[serde(rename = "openai-chat", alias = "openai_chat")]
    OpenaiChat,
    #[serde(rename = "openai-responses", alias = "openai_responses")]
    OpenaiResponses,
    /// Anthropic Messages 协议。网关的 `/v1/messages` 端点、GCMP Anthropic 类模板
    /// 与 Claude 系客户端都使用该协议名。
    #[serde(
        rename = "anthropic",
        alias = "anthropic-messages",
        alias = "anthropic_messages"
    )]
    Anthropic,
}

impl AIServiceProtocol {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::OpenaiChat => "openai-chat",
            Self::OpenaiResponses => "openai-responses",
            Self::Anthropic => "anthropic",
        }
    }

    /// 该协议的密钥与端点写法是否走 Anthropic 原生约定（`x-api-key`、`/v1/messages`）。
    pub(crate) fn is_anthropic(&self) -> bool {
        matches!(self, Self::Anthropic)
    }

    /// 解析 Tauri/MCP 入参里的协议字符串；未知值一次性给出完整可选值，避免各处重复文案。
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "openai-chat" => Ok(Self::OpenaiChat),
            "openai-responses" => Ok(Self::OpenaiResponses),
            "anthropic" => Ok(Self::Anthropic),
            other => Err(format!(
                "protocol 只支持 openai-chat、openai-responses 或 anthropic，收到：{other}"
            )),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct CustomAIService {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    pub protocol: AIServiceProtocol,
    pub model: String,
    pub models: Vec<String>,
    /// DPAPI 加密后的 API Key，不落明文。
    encrypted_api_key: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedCustomAIService {
    id: String,
    display_name: String,
    base_url: String,
    protocol: AIServiceProtocol,
    model: String,
    models: Vec<String>,
    encrypted_api_key: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedAIServiceSelection {
    #[serde(default)]
    active_service_id: String,
}

impl From<PersistedCustomAIService> for CustomAIService {
    fn from(value: PersistedCustomAIService) -> Self {
        Self {
            id: value.id,
            display_name: value.display_name,
            base_url: value.base_url,
            protocol: value.protocol,
            model: value.model,
            models: value.models,
            encrypted_api_key: value.encrypted_api_key,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

impl From<&CustomAIService> for PersistedCustomAIService {
    fn from(value: &CustomAIService) -> Self {
        Self {
            id: value.id.clone(),
            display_name: value.display_name.clone(),
            base_url: value.base_url.clone(),
            protocol: value.protocol.clone(),
            model: value.model.clone(),
            models: value.models.clone(),
            encrypted_api_key: value.encrypted_api_key.clone(),
            created_at: value.created_at.clone(),
            updated_at: value.updated_at.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct CustomAIServiceInput {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    pub protocol: AIServiceProtocol,
    pub model: String,
    pub models: Vec<String>,
    /// 写入明文 Key；读取时返回的公开视图不含此字段。
    #[serde(default)]
    pub api_key: String,
}

impl CustomAIService {
    pub(crate) fn public_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "display_name": self.display_name,
            "base_url": self.base_url,
            "protocol": self.protocol.as_str(),
            "model": self.model,
            "models": self.models,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

pub(crate) fn store_path() -> Result<PathBuf, Box<dyn Error>> {
    let dir = crate::store::paths::agent_home();
    fs::create_dir_all(&dir)?;
    Ok(dir.join(STORE_FILE))
}

fn load_all() -> Result<BTreeMap<String, CustomAIService>, Box<dyn Error>> {
    let path = store_path()?;
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let persisted: BTreeMap<String, PersistedCustomAIService> =
        serde_json::from_slice(&fs::read(path)?)?;
    Ok(persisted
        .into_iter()
        .map(|(id, item)| (id, item.into()))
        .collect())
}

fn save_all(services: &BTreeMap<String, CustomAIService>) -> Result<(), Box<dyn Error>> {
    let path = store_path()?;
    let persisted = services
        .iter()
        .map(|(id, item)| (id.clone(), PersistedCustomAIService::from(item)))
        .collect::<BTreeMap<_, _>>();
    fs::write(path, serde_json::to_vec_pretty(&persisted)?)?;
    Ok(())
}

fn validate_base_url(value: &str) -> Result<(), Box<dyn Error>> {
    let value = value.trim();
    let url =
        url::Url::parse(value).map_err(|_| "base_url 必须是合法的 http/https URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("base_url 仅允许 http/https".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("base_url 不应包含用户名或密码".into());
    }
    Ok(())
}

fn normalize_id(id: &str) -> Result<String, Box<dyn Error>> {
    let id = id.trim().to_string();
    if id.is_empty() || id.len() > 64 {
        return Err("服务 ID 必须为 1-64 个字符".into());
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err("服务 ID 只允许字母、数字、下划线和连字符".into());
    }
    Ok(id)
}

pub(crate) fn list() -> Result<Vec<CustomAIService>, Box<dyn Error>> {
    Ok(load_all()?.into_values().collect())
}

pub(crate) fn public_snapshot() -> Result<serde_json::Value, Box<dyn Error>> {
    let services = list()?;
    Ok(serde_json::json!({
        "services": services.iter().map(|item| item.public_json()).collect::<Vec<_>>(),
        "active_service_id": active_id()?.unwrap_or_default(),
    }))
}

pub(crate) fn upsert(input: CustomAIServiceInput) -> Result<CustomAIService, Box<dyn Error>> {
    let id = normalize_id(&input.id)?;
    validate_base_url(&input.base_url)?;
    if input.display_name.trim().is_empty() {
        return Err("display_name 不能为空".into());
    }
    if input.model.trim().is_empty() {
        return Err("model 不能为空".into());
    }
    let now = crate::app::ai_provider_import::unix_now_seconds().to_string();
    let mut services = load_all()?;
    let existing = services.get(&id);
    let encrypted_api_key = if input.api_key.trim().is_empty() {
        existing
            .map(|item| item.encrypted_api_key.clone())
            .ok_or("新建服务时 api_key 不能为空")?
    } else {
        protect_secret_for_current_user(input.api_key.trim())?
    };
    let service = CustomAIService {
        id: id.clone(),
        display_name: input.display_name.trim().to_string(),
        base_url: input.base_url.trim().trim_end_matches('/').to_string(),
        protocol: input.protocol,
        model: input.model.trim().to_string(),
        models: input
            .models
            .iter()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect(),
        encrypted_api_key,
        created_at: existing
            .map(|item| item.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
    };
    services.insert(id, service.clone());
    save_all(&services)?;
    Ok(service)
}

pub(crate) fn remove(id: &str) -> Result<bool, Box<dyn Error>> {
    let id = id.trim();
    let mut services = load_all()?;
    let removed = services.remove(id).is_some();
    if removed {
        save_all(&services)?;
        if active_id()?.as_deref() == Some(id) {
            set_active("")?;
        }
    }
    Ok(removed)
}

pub(crate) fn load_secret(id: &str) -> Result<(CustomAIService, String), Box<dyn Error>> {
    let services = load_all()?;
    let service = services
        .get(id.trim())
        .ok_or_else(|| format!("自定义 AI 服务不存在：{id}"))?;
    let api_key = unprotect_secret_for_current_user(&service.encrypted_api_key)?;
    Ok((service.clone(), api_key))
}

pub(crate) fn active_id() -> Result<Option<String>, Box<dyn Error>> {
    let path = selection_path()?;
    if !path.is_file() {
        return Ok(None);
    }
    let selection: PersistedAIServiceSelection = serde_json::from_slice(&fs::read(path)?)?;
    let id = selection.active_service_id.trim();
    Ok((!id.is_empty()).then(|| id.to_string()))
}

pub(crate) fn active_service() -> Result<Option<CustomAIService>, Box<dyn Error>> {
    let Some(id) = active_id()? else {
        return Ok(None);
    };
    Ok(load_all()?.remove(&id))
}

pub(crate) fn active_service_with_secret(
) -> Result<Option<(CustomAIService, String)>, Box<dyn Error>> {
    let Some(id) = active_id()? else {
        return Ok(None);
    };
    load_secret(&id).map(Some)
}

pub(crate) fn set_active(id: &str) -> Result<Option<CustomAIService>, Box<dyn Error>> {
    let id = id.trim();
    let selected = if id.is_empty() {
        None
    } else {
        let id = normalize_id(id)?;
        Some(
            load_all()?
                .remove(&id)
                .ok_or_else(|| format!("自定义 AI 服务不存在：{id}"))?,
        )
    };
    let selection = PersistedAIServiceSelection {
        active_service_id: selected
            .as_ref()
            .map(|service| service.id.clone())
            .unwrap_or_default(),
    };
    crate::store::atomic_file::atomic_write(
        &selection_path()?,
        &serde_json::to_vec_pretty(&selection)?,
    )?;
    Ok(selected)
}

fn selection_path() -> Result<PathBuf, Box<dyn Error>> {
    let dir = crate::store::paths::agent_home();
    fs::create_dir_all(&dir)?;
    Ok(dir.join(SELECTION_FILE))
}

/// Anthropic 协议里的 API 根地址：去掉末尾斜杠，并剥掉可选的末尾 `/v1`。
///
/// GCMP Anthropic 类模板给出的是 API 根地址（如 `https://api.moonshot.cn/anthropic`），
/// 手工填写时用户常按 OpenAI 习惯带上 `/v1`，两种写法指向同一根。Anthropic SDK
/// 系客户端（Claude Code、DSH）会在根地址后自行追加 `/v1/messages`，因此写入这些
/// 客户端前必须回到根地址；只有需要显式带 `/v1` 的客户端（OpenCode/AI SDK）才补回去。
pub(crate) fn anthropic_api_root(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    base.strip_suffix("/v1").unwrap_or(base).to_string()
}

/// Anthropic 原生模型列表地址 `{api_root}/v1/models`。
fn anthropic_models_url(base_url: &str) -> String {
    format!("{}/v1/models", anthropic_api_root(base_url))
}

/// 拉取服务可用模型列表。
///
/// `openai-chat`/`openai-responses` 走 OpenAI 兼容 `GET {base_url}/models`（Bearer 认证）；
/// `anthropic` 走 Anthropic 原生 `GET {base_url}/v1/models`（`x-api-key` + `anthropic-version`）。
/// 两者响应都是 `data[].id`，因此共用同一份解析逻辑。
pub(crate) fn fetch_models(
    base_url: &str,
    api_key: &str,
    protocol: AIServiceProtocol,
) -> Result<Vec<String>, Box<dyn Error>> {
    validate_base_url(base_url)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let request = match protocol {
        AIServiceProtocol::Anthropic => client
            .get(anthropic_models_url(base_url))
            .header("x-api-key", api_key.trim())
            .header("anthropic-version", ANTHROPIC_API_VERSION),
        AIServiceProtocol::OpenaiChat | AIServiceProtocol::OpenaiResponses => {
            let base = base_url.trim().trim_end_matches('/');
            client
                .get(format!("{base}/models"))
                .bearer_auth(api_key.trim())
        }
    };
    let response = request
        .send()
        .map_err(|error| format!("拉取模型列表失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "模型列表接口返回 {}：{}",
            response.status(),
            response.text().unwrap_or_default()
        )
        .into());
    }
    let payload: serde_json::Value = response
        .json()
        .map_err(|error| format!("模型列表响应解析失败：{error}"))?;
    let models = payload
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or("模型列表响应缺少 data 数组")?
        .iter()
        .filter_map(|item| item.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err("模型列表接口未返回任何模型".into());
    }
    Ok(models)
}

/// 读取已保存自定义服务并拉取其 `/models` 模型列表。
pub(crate) fn list_models(id: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let (service, api_key) = load_secret(id)?;
    fetch_models(&service.base_url, &api_key, service.protocol)
}

#[cfg(test)]
mod tests {
    use super::{validate_base_url, AIServiceProtocol, CustomAIServiceInput};
    use std::io::{Read, Write};
    use std::path::PathBuf;

    fn with_isolated_home(run: impl FnOnce()) {
        // `HIMIND_AGENT_HOME` 是进程级环境变量，切换它会影响所有并行测试。
        // 与其它会改它的用例共用一把全局锁，避免测试之间互相污染。
        let _guard = crate::store::paths::test_env_lock();
        let previous = std::env::var("HIMIND_AGENT_HOME").ok();
        let root = std::env::temp_dir().join(format!(
            "himind-ai-services-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        run();
        let _ = std::fs::remove_dir_all(&root);
        match previous {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
    }

    #[test]
    fn custom_service_roundtrips_with_encrypted_key() {
        with_isolated_home(|| {
            let input = CustomAIServiceInput {
                id: "my-gateway".to_string(),
                display_name: "我的网关".to_string(),
                base_url: "https://ai.example.com/v1".to_string(),
                protocol: AIServiceProtocol::OpenaiResponses,
                model: "gpt-test".to_string(),
                models: vec!["gpt-test".to_string(), "gpt-test-2".to_string()],
                api_key: "sk-test-secret-123".to_string(),
            };
            let saved = super::upsert(input).expect("upsert custom service");
            assert_eq!(saved.id, "my-gateway");
            assert!(!saved.encrypted_api_key.contains("sk-test"));

            let snapshot = super::public_snapshot().expect("snapshot");
            let snapshot_text = snapshot.to_string();
            assert!(snapshot_text.contains("my-gateway"));
            assert!(snapshot_text.contains("openai-responses"));
            assert!(!snapshot_text.contains("sk-test-secret"));

            let (loaded, api_key) = super::load_secret("my-gateway").expect("load secret");
            assert_eq!(api_key, "sk-test-secret-123");
            assert_eq!(loaded.base_url, "https://ai.example.com/v1");

            assert!(super::remove("my-gateway").expect("remove"));
            assert!(
                super::load_secret("my-gateway").is_err(),
                "removed service must not resolve"
            );
        });
    }

    #[test]
    fn updating_service_without_key_preserves_existing_secret() {
        with_isolated_home(|| {
            super::upsert(CustomAIServiceInput {
                id: "editable".to_string(),
                display_name: "初始服务".to_string(),
                base_url: "https://ai.example.com/v1".to_string(),
                protocol: AIServiceProtocol::OpenaiChat,
                model: "model-a".to_string(),
                models: vec!["model-a".to_string()],
                api_key: "sk-original".to_string(),
            })
            .expect("create service");
            super::upsert(CustomAIServiceInput {
                id: "editable".to_string(),
                display_name: "更新后的服务".to_string(),
                base_url: "https://ai.example.com/v2".to_string(),
                protocol: AIServiceProtocol::OpenaiResponses,
                model: "model-b".to_string(),
                models: vec!["model-b".to_string()],
                api_key: String::new(),
            })
            .expect("update service without rotating key");
            let (service, api_key) = super::load_secret("editable").expect("load service");
            assert_eq!(service.display_name, "更新后的服务");
            assert_eq!(service.protocol, AIServiceProtocol::OpenaiResponses);
            assert_eq!(api_key, "sk-original");
        });
    }

    #[test]
    fn active_service_selection_is_explicit_and_clears_on_remove() {
        with_isolated_home(|| {
            for id in ["first", "second"] {
                super::upsert(CustomAIServiceInput {
                    id: id.to_string(),
                    display_name: id.to_string(),
                    base_url: format!("https://{id}.example/v1"),
                    protocol: AIServiceProtocol::OpenaiChat,
                    model: "model-a".to_string(),
                    models: vec!["model-a".to_string()],
                    api_key: format!("sk-{id}"),
                })
                .expect("create service");
            }

            assert_eq!(super::active_id().unwrap(), None);
            let selected = super::set_active("first").unwrap().expect("selected");
            assert_eq!(selected.id, "first");
            assert_eq!(super::active_id().unwrap().as_deref(), Some("first"));

            let (active, api_key) = super::active_service_with_secret()
                .unwrap()
                .expect("active service");
            assert_eq!(active.id, "first");
            assert_eq!(api_key, "sk-first");

            assert!(super::remove("first").unwrap());
            assert_eq!(super::active_id().unwrap(), None);

            super::set_active("second").unwrap();
            super::set_active("").unwrap();
            assert_eq!(super::active_id().unwrap(), None);
        });
    }

    #[test]
    fn rejects_invalid_service_input() {
        assert!(validate_base_url("file:///etc/passwd").is_err());
        assert!(validate_base_url("https://user:pass@host/v1").is_err());
        assert!(validate_base_url("https://ok.example/v1").is_ok());
        let invalid = CustomAIServiceInput {
            id: "bad id/with slash".to_string(),
            display_name: "x".to_string(),
            base_url: "https://ok.example/v1".to_string(),
            protocol: AIServiceProtocol::OpenaiChat,
            model: "m".to_string(),
            models: Vec::new(),
            api_key: "k".to_string(),
        };
        let err = super::upsert(invalid).err().expect("must reject");
        assert!(err.to_string().contains("只允许"));
    }

    #[test]
    fn accepts_public_hyphenated_protocol_values() {
        let input: CustomAIServiceInput = serde_json::from_value(serde_json::json!({
            "id": "gateway",
            "display_name": "Gateway",
            "base_url": "https://ai.example.com/v1",
            "protocol": "openai-chat",
            "model": "model-a",
            "models": ["model-a"],
            "api_key": "secret"
        }))
        .expect("public capability payload should parse");
        assert_eq!(input.protocol, AIServiceProtocol::OpenaiChat);

        let anthropic: CustomAIServiceInput = serde_json::from_value(serde_json::json!({
            "id": "anthropic-gateway",
            "display_name": "Anthropic Gateway",
            "base_url": "https://api.anthropic.com",
            "protocol": "anthropic",
            "model": "claude-sonnet-4-5",
            "models": ["claude-sonnet-4-5"],
            "api_key": "secret"
        }))
        .expect("anthropic protocol payload should parse");
        assert_eq!(anthropic.protocol, AIServiceProtocol::Anthropic);
        assert_eq!(anthropic.protocol.as_str(), "anthropic");
    }

    #[test]
    fn parses_known_protocols_and_rejects_unknown_ones() {
        assert_eq!(
            AIServiceProtocol::parse("openai-chat").unwrap(),
            AIServiceProtocol::OpenaiChat
        );
        assert_eq!(
            AIServiceProtocol::parse(" openai-responses ").unwrap(),
            AIServiceProtocol::OpenaiResponses
        );
        assert_eq!(
            AIServiceProtocol::parse("anthropic").unwrap(),
            AIServiceProtocol::Anthropic
        );
        let error = AIServiceProtocol::parse("gemini").unwrap_err();
        assert!(error.contains("anthropic"));
    }

    #[test]
    fn store_path_is_under_agent_home() {
        with_isolated_home(|| {
            let path: PathBuf = super::store_path().expect("store path");
            assert!(path.ends_with("ai-services.json"));
        });
    }

    #[test]
    fn fetch_models_parses_openai_models_payload() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let body = r#"{"object":"list","data":[{"id":"model-a"},{"id":"model-b"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        });
        let models = super::fetch_models(
            &format!("http://{addr}/v1"),
            "sk-test",
            AIServiceProtocol::OpenaiResponses,
        )
        .expect("fetch models");
        assert_eq!(models, vec!["model-a".to_string(), "model-b".to_string()]);
    }

    #[test]
    fn fetch_models_uses_anthropic_headers_and_v1_path() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let received = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let captured = received.clone();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            *captured.lock().unwrap() = String::from_utf8_lossy(&buffer[..read]).to_string();
            let body = r#"{"data":[{"id":"claude-sonnet-4-5"}],"has_more":false}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        });
        // 不带 `/v1` 的 API 根地址也要落到同一端点。
        let models = super::fetch_models(
            &format!("http://{addr}/anthropic"),
            "sk-anthropic",
            AIServiceProtocol::Anthropic,
        )
        .expect("fetch anthropic models");
        assert_eq!(models, vec!["claude-sonnet-4-5".to_string()]);
        let request = received.lock().unwrap().clone();
        // 头字段名大小写由 HTTP 客户端决定（reqwest 发 Title-Case），断言只看语义。
        let normalized = request.to_ascii_lowercase();
        assert!(
            request.starts_with("GET /anthropic/v1/models "),
            "unexpected request line: {request}"
        );
        assert!(
            normalized.contains("x-api-key: sk-anthropic"),
            "unexpected headers: {request}"
        );
        assert!(
            normalized.contains("anthropic-version: 2023-06-01"),
            "unexpected headers: {request}"
        );
        assert!(
            !normalized.contains("authorization:"),
            "anthropic 端点不应带 bearer 鉴权: {request}"
        );
    }

    #[test]
    fn fetch_models_rejects_non_success_status() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let body = r#"{"error":{"message":"bad key"}}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        });
        let err = super::fetch_models(
            &format!("http://{addr}/v1"),
            "sk-bad",
            AIServiceProtocol::OpenaiChat,
        )
        .err()
        .expect("must reject");
        assert!(err.to_string().contains("401"));
    }
}
