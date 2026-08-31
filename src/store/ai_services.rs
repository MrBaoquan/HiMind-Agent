use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use super::credentials::{protect_secret_for_current_user, unprotect_secret_for_current_user};

const STORE_FILE: &str = "ai-services.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AIServiceProtocol {
    #[serde(rename = "openai-chat", alias = "openai_chat")]
    OpenaiChat,
    #[serde(rename = "openai-responses", alias = "openai_responses")]
    OpenaiResponses,
}

impl AIServiceProtocol {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::OpenaiChat => "openai-chat",
            Self::OpenaiResponses => "openai-responses",
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

/// 请求 OpenAI 兼容的 `GET {base_url}/models` 接口，返回可用模型 ID 列表。
pub(crate) fn fetch_models(base_url: &str, api_key: &str) -> Result<Vec<String>, Box<dyn Error>> {
    validate_base_url(base_url)?;
    let base = base_url.trim().trim_end_matches('/');
    let url = format!("{base}/models");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let response = client
        .get(&url)
        .bearer_auth(api_key.trim())
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
    fetch_models(&service.base_url, &api_key)
}

#[cfg(test)]
mod tests {
    use super::{validate_base_url, AIServiceProtocol, CustomAIServiceInput};
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    /// `HIMIND_AGENT_HOME` 是进程级环境变量，切换它会影响所有并行测试。
    /// 串行化所有依赖它的用例，避免测试之间互相污染。
    fn home_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_isolated_home(run: impl FnOnce()) {
        let _guard = home_test_lock().lock().unwrap();
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
        let models =
            super::fetch_models(&format!("http://{addr}/v1"), "sk-test").expect("fetch models");
        assert_eq!(models, vec!["model-a".to_string(), "model-b".to_string()]);
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
        let err = super::fetch_models(&format!("http://{addr}/v1"), "sk-bad")
            .err()
            .expect("must reject");
        assert!(err.to_string().contains("401"));
    }
}
