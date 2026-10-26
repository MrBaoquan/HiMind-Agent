use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use super::credentials::{protect_secret_for_current_user, unprotect_secret_for_current_user};

const STORE_FILE: &str = "ai-services.json";
const SELECTION_FILE: &str = "ai-service-selection.json";
pub(crate) const SUPPORTED_BALANCE_PROFILES: &[&str] = &[
    "deepseek",
    "kimi",
    "kimi-coding",
    "zhipu-plan",
    "minimax-plan-cn",
    "minimax-plan-en",
    "stepfun",
    "siliconflow",
    "siliconflow-en",
    "openrouter",
    "novita",
    "relay-balance",
    "new-api",
    "sub2api",
    "opencode-go",
];

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

/// Stable AI capability IDs used by the capability bus and settings UI.
pub(crate) const CAP_CHAT: &str = "chat";
pub(crate) const CAP_IMAGE_GENERATE: &str = "image.generate";
pub(crate) const CAP_IMAGE_EDIT: &str = "image.edit";
pub(crate) const CAP_VIDEO_GENERATE: &str = "video.generate";
pub(crate) const CAP_AUDIO_SPEECH: &str = "audio.speech";
pub(crate) const CAP_AUDIO_TRANSCRIBE: &str = "audio.transcribe";
pub(crate) const SUPPORTED_MEDIA_CAPABILITIES: &[&str] = &[
    CAP_IMAGE_GENERATE,
    CAP_IMAGE_EDIT,
    CAP_VIDEO_GENERATE,
    CAP_AUDIO_SPEECH,
    CAP_AUDIO_TRANSCRIBE,
];
pub(crate) const SUPPORTED_AI_CAPABILITIES: &[&str] = &[
    CAP_CHAT,
    CAP_IMAGE_GENERATE,
    CAP_IMAGE_EDIT,
    CAP_VIDEO_GENERATE,
    CAP_AUDIO_SPEECH,
    CAP_AUDIO_TRANSCRIBE,
];
/// 未显式声明适配器时的默认值（OpenAI Images 兼容协议）。
pub(crate) const DEFAULT_MEDIA_ADAPTER: &str = "openai-images";

/// 一个服务在某个媒体能力上使用的适配器与模型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct MediaModelBinding {
    pub adapter: String,
    pub model: String,
    #[serde(default)]
    pub execution_mode: Option<MediaExecutionMode>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MediaExecutionMode {
    Sync,
    Stream,
    Async,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedMediaBinding {
    pub adapter: String,
    pub model: String,
    pub execution_mode: MediaExecutionMode,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct CustomAIService {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    pub protocol: AIServiceProtocol,
    pub model: String,
    pub models: Vec<String>,
    /// 生图模型。留空表示该服务不用于生图。
    pub image_model: String,
    /// 生图适配器：`openai-images`（默认）或 `ark-images`（火山方舟/豆包）。
    pub image_adapter: String,
    /// 媒体能力槽位：`image.generate` / `image.edit` / `video.generate` / `audio.speech` / `audio.transcribe` → 适配器 + 模型。
    /// `image_model` / `image_adapter` 是 `image.generate` 槽位的旧投影，二者保持一致。
    #[serde(default)]
    pub media: std::collections::BTreeMap<String, MediaModelBinding>,
    /// Provider-specific account-balance endpoint profile, independent of the chat protocol.
    #[serde(default)]
    pub balance_profile_id: Option<String>,
    #[serde(default)]
    pub balance_user_id: Option<String>,
    /// Optional New API access token, separately DPAPI-protected.
    #[serde(default)]
    encrypted_balance_access_token: Option<String>,
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
    #[serde(default)]
    image_model: String,
    #[serde(default)]
    image_adapter: String,
    #[serde(default)]
    media: std::collections::BTreeMap<String, MediaModelBinding>,
    #[serde(default)]
    balance_profile_id: Option<String>,
    #[serde(default)]
    balance_user_id: Option<String>,
    #[serde(default)]
    encrypted_balance_access_token: Option<String>,
    encrypted_api_key: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedAIServiceSelection {
    #[serde(default)]
    active_service_id: String,
    /// 生图默认使用的本机服务（`media.image.generate`）。留空表示自动解析。
    /// 这是 `media_services["image.generate"]` 的旧投影。
    #[serde(default)]
    image_service_id: String,
    /// 各媒体能力的默认服务：`image.generate` / `image.edit` / `video.generate`。
    #[serde(default)]
    media_services: std::collections::BTreeMap<String, String>,
    /// Explicit same-capability fallback order. No other capability can be
    /// substituted because doing so would change the meaning of the request.
    #[serde(default)]
    capability_fallbacks: std::collections::BTreeMap<String, Vec<String>>,
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
            image_model: value.image_model,
            image_adapter: value.image_adapter,
            media: value.media,
            balance_profile_id: value.balance_profile_id,
            balance_user_id: value.balance_user_id,
            encrypted_balance_access_token: value.encrypted_balance_access_token,
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
            image_model: value.image_model.clone(),
            image_adapter: value.image_adapter.clone(),
            media: value.media.clone(),
            balance_profile_id: value.balance_profile_id.clone(),
            balance_user_id: value.balance_user_id.clone(),
            encrypted_balance_access_token: value.encrypted_balance_access_token.clone(),
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
    /// 生图模型。可留空。
    #[serde(default)]
    pub image_model: String,
    /// 生图适配器（`openai-images` / `ark-images`）。可留空，缺省 `openai-images`。
    #[serde(default)]
    pub image_adapter: String,
    /// 媒体能力槽位（`image.generate` / `image.edit` / `video.generate` 等）。
    #[serde(default)]
    pub media: std::collections::BTreeMap<String, MediaModelBinding>,
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
            "image_model": self.image_model,
            "image_adapter": self.image_adapter,
            "media": self.media,
            "balance_profile_id": self.balance_profile_id,
            "balance_user_id": self.balance_user_id,
            "suggested_balance_profile_id":
                crate::app::ai_service_balance::suggest_balance_profile(&self.base_url),
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
        .map(|(id, item)| {
            let mut service: CustomAIService = item.into();
            // 旧配置只有 `image_model` / `image_adapter`：读时补能力槽位。
            if service.media.is_empty() && !service.image_model.trim().is_empty() {
                let adapter = if service.image_adapter.trim().is_empty() {
                    DEFAULT_MEDIA_ADAPTER.to_string()
                } else {
                    service.image_adapter.trim().to_string()
                };
                service.media.insert(
                    CAP_IMAGE_GENERATE.to_string(),
                    MediaModelBinding {
                        adapter,
                        model: service.image_model.trim().to_string(),
                        execution_mode: None,
                    },
                );
            }
            (id, service)
        })
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
        "balances": crate::store::ai_service_balances::list(),
        "active_service_id": active_id()?.unwrap_or_default(),
        "image_service_id": image_service_id()?.unwrap_or_default(),
        "media_services": load_selection()?.media_services,
        "capability_fallbacks": load_selection()?.capability_fallbacks,
    }))
}

/// Built-in adapter metadata. Implementations remain in their domain modules;
/// this registry is the shared discovery surface for capability routing and UI.
pub(crate) fn builtin_media_adapters() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "id": "openai-images",
            "capabilities": [CAP_IMAGE_GENERATE],
            "execution_modes": [MediaExecutionMode::Sync],
            "features": ["text_to_image", "b64_response", "url_response"],
            "limits": {"prompt_chars": 8000, "n": [1, 4], "input_images": 0},
            "pricing": {"status": "unknown", "source": "provider_catalog"},
            "usage": {"status": "unknown", "source": "provider_response"},
        }),
        serde_json::json!({
            "id": "openai-images-edit",
            "capabilities": [CAP_IMAGE_EDIT],
            "execution_modes": [MediaExecutionMode::Sync],
            "features": ["image_edit", "single_reference", "mask"],
            "limits": {"prompt_chars": 8000, "n": [1, 4], "input_images": 1},
            "pricing": {"status": "unknown", "source": "provider_catalog"},
            "usage": {"status": "unknown", "source": "provider_response"},
        }),
        serde_json::json!({
            "id": "ark-images",
            "capabilities": [CAP_IMAGE_GENERATE, CAP_IMAGE_EDIT],
            "execution_modes": [MediaExecutionMode::Sync],
            "features": ["text_to_image", "image_edit", "multi_reference", "watermark_control"],
            "limits": {"prompt_chars": 8000, "n": [1, 4], "input_images": 4},
            "pricing": {"status": "unknown", "source": "provider_catalog"},
            "usage": {"status": "unknown", "source": "provider_response"},
        }),
        serde_json::json!({
            "id": "ark-video",
            "capabilities": [CAP_VIDEO_GENERATE],
            "execution_modes": [MediaExecutionMode::Async],
            "features": ["text_to_video", "image_to_video", "first_last_frame", "async_job", "cancel"],
            "limits": {"prompt_chars": 8000, "input_images": 2, "duration_seconds": [1, 60]},
            "pricing": {"status": "unknown", "source": "provider_catalog"},
            "usage": {"status": "unknown", "source": "provider_response"},
        }),
        serde_json::json!({
            "id": "wan-video",
            "capabilities": [CAP_VIDEO_GENERATE],
            "execution_modes": [MediaExecutionMode::Async],
            "features": ["text_to_video", "image_to_video", "first_frame", "async_job", "cancel", "prompt_extend"],
            "limits": {"prompt_chars": 5000, "input_images": 1, "duration_seconds": [2, 15]},
            "pricing": {"status": "provider_catalog", "source": "provider_catalog"},
            "usage": {"status": "provider_reported", "source": "provider_response"},
        }),
        serde_json::json!({
            "id": "openai-speech",
            "capabilities": [CAP_AUDIO_SPEECH],
            "execution_modes": [MediaExecutionMode::Sync],
            "features": ["text_to_speech", "voice", "speed"],
            "limits": {"text_chars": 4096, "speed": [0.25, 4.0]},
            "pricing": {"status": "unknown", "source": "provider_catalog"},
            "usage": {"status": "unknown", "source": "provider_response"},
        }),
        serde_json::json!({
            "id": "fish-audio-tts",
            "capabilities": [CAP_AUDIO_SPEECH],
            "execution_modes": [MediaExecutionMode::Sync],
            "features": ["text_to_speech", "voice_reference", "voice_clone_reference", "emotion_tags", "multiple_formats"],
            "limits": {"text_chars": 4096, "speed": [0.5, 2.0], "formats": ["mp3", "wav", "pcm", "opus"]},
            "pricing": {"status": "published", "source": "provider_catalog", "unit": "utf8_bytes"},
            "usage": {"status": "reported_in_dashboard", "source": "provider_dashboard"},
        }),
        serde_json::json!({
            "id": "openai-audio",
            "capabilities": [CAP_AUDIO_TRANSCRIBE],
            "execution_modes": [MediaExecutionMode::Sync],
            "features": ["transcription", "language_hint", "timestamps"],
            "limits": {"prompt_chars": 2000},
            "pricing": {"status": "unknown", "source": "provider_catalog"},
            "usage": {"status": "unknown", "source": "provider_response"},
        }),
        serde_json::json!({
            "id": "fish-audio-asr",
            "capabilities": [CAP_AUDIO_TRANSCRIBE],
            "execution_modes": [MediaExecutionMode::Sync],
            "features": ["transcription", "language_hint", "timestamps", "speaker_turns", "emotion_events", "long_audio"],
            "limits": {"duration_seconds": 3600, "audio_files": 1},
            "pricing": {"status": "published", "source": "provider_catalog", "unit": "audio_hour"},
            "usage": {"status": "reported_in_dashboard", "source": "provider_dashboard"},
        }),
    ]
}

fn adapter_for<'a>(adapters: &'a [serde_json::Value], id: &str) -> Option<&'a serde_json::Value> {
    adapters.iter().find(|adapter| adapter["id"] == id)
}

fn adapter_field(
    adapter: Option<&serde_json::Value>,
    field: &str,
    fallback: serde_json::Value,
) -> serde_json::Value {
    adapter
        .and_then(|value| value.get(field))
        .cloned()
        .unwrap_or(fallback)
}

fn adapter_execution_modes(
    adapters: &[serde_json::Value],
    capability: &str,
) -> Vec<serde_json::Value> {
    let mut seen = std::collections::BTreeSet::new();
    for adapter in adapters {
        let supports = adapter["capabilities"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item == capability));
        if !supports {
            continue;
        }
        if let Some(modes) = adapter["execution_modes"].as_array() {
            for mode in modes {
                if let Some(value) = mode.as_str() {
                    seen.insert(value.to_string());
                }
            }
        }
    }
    if seen.is_empty() {
        let fallback = match default_execution_mode(capability) {
            MediaExecutionMode::Sync => "sync",
            MediaExecutionMode::Stream => "stream",
            MediaExecutionMode::Async => "async",
        };
        seen.insert(fallback.to_string());
    }
    seen.into_iter().map(serde_json::Value::String).collect()
}

/// A machine-readable snapshot of configured media providers. It never includes
/// API keys or provider-specific request payloads.
pub(crate) fn media_provider_catalog() -> Result<serde_json::Value, Box<dyn Error>> {
    let services = list()?;
    let selection = load_selection()?;
    let adapters = builtin_media_adapters();
    let providers = services
        .iter()
        .map(|service| {
            let has_api_key = !service.encrypted_api_key.trim().is_empty();
            let health = if has_api_key {
                serde_json::json!({"state": "ready", "reason": "credential_present"})
            } else {
                serde_json::json!({"state": "unconfigured", "reason": "missing_credential"})
            };
            let media_capabilities = SUPPORTED_MEDIA_CAPABILITIES
                .iter()
                .filter_map(|capability| {
                    binding_details_for(service, capability).map(|binding| {
                        let adapter = adapter_for(&adapters, &binding.adapter);
                        serde_json::json!({
                            "capability": capability,
                            "adapter": binding.adapter,
                            "model": binding.model,
                            "execution_mode": binding.execution_mode,
                            "features": adapter_field(adapter, "features", serde_json::json!([])),
                            "limits": adapter_field(adapter, "limits", serde_json::json!({})),
                            "health": health,
                            "pricing": adapter_field(adapter, "pricing", serde_json::json!({"status": "unknown", "source": "provider_catalog"})),
                            "usage": adapter_field(adapter, "usage", serde_json::json!({"status": "unknown", "source": "provider_response"})),
                        })
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({
                "id": service.id,
                "display_name": service.display_name,
                "base_url": service.base_url,
                "protocol": service.protocol.as_str(),
                "has_api_key": has_api_key,
                "health": health,
                "models": service.models,
                "media_capabilities": media_capabilities,
            })
        })
        .collect::<Vec<_>>();
    let capabilities = SUPPORTED_MEDIA_CAPABILITIES
        .iter()
        .map(|capability| {
            let configured_by = providers
                .iter()
                .filter(|provider| {
                    provider
                        .get("media_capabilities")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|items| {
                            items.iter().any(|item| {
                                item.get("capability").and_then(serde_json::Value::as_str)
                                    == Some(*capability)
                            })
                        })
                })
                .filter_map(|provider| provider.get("id").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>();
            serde_json::json!({
                "id": capability,
                "default_execution_mode": default_execution_mode(capability),
                "execution_modes": adapter_execution_modes(&adapters, capability),
                "adapter_count": adapters.iter().filter(|adapter| {
                    adapter["capabilities"].as_array().is_some_and(|items| items.iter().any(|item| item == capability))
                }).count(),
                "configured_by": configured_by,
                "configured": !configured_by.is_empty(),
            })
        })
        .collect::<Vec<_>>();
    let route_plans = SUPPORTED_AI_CAPABILITIES
        .iter()
        .map(|capability| capability_route_plan(capability, ""))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(serde_json::json!({
        "schema_version": "himind.media-provider-catalog.v2",
        "adapters": adapters,
        "providers": providers,
        "capabilities": capabilities,
        "routes": route_plans,
        "defaults": selection.media_services,
        "fallbacks": selection.capability_fallbacks,
        "image_service_id": selection.image_service_id,
    }))
}
pub(crate) fn upsert(input: CustomAIServiceInput) -> Result<CustomAIService, Box<dyn Error>> {
    upsert_internal(input, None, None, None)
}

/// Upserts a service while explicitly selecting or clearing its balance-query profile.
/// The legacy `upsert` entry point preserves an existing profile for non-UI callers.
pub(crate) fn upsert_with_balance_profile(
    input: CustomAIServiceInput,
    balance_profile_id: Option<&str>,
    balance_access_token: Option<&str>,
    balance_user_id: Option<&str>,
) -> Result<CustomAIService, Box<dyn Error>> {
    let profile = match balance_profile_id.map(str::trim) {
        Some("") | None => None,
        Some(value) => Some(normalize_balance_profile_id(value)?),
    };
    let access_token = balance_access_token
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let user_id = balance_user_id
        .map(str::trim)
        .filter(|value| !value.is_empty());
    upsert_internal(
        input,
        Some(profile),
        access_token,
        user_id.map(str::to_string),
    )
}

fn upsert_internal(
    input: CustomAIServiceInput,
    profile_override: Option<Option<String>>,
    balance_access_token: Option<&str>,
    balance_user_id: Option<String>,
) -> Result<CustomAIService, Box<dyn Error>> {
    let id = normalize_id(&input.id)?;
    validate_base_url(&input.base_url)?;
    if input.display_name.trim().is_empty() {
        return Err("display_name 不能为空".into());
    }
    let has_media_model = !input.image_model.trim().is_empty()
        || input
            .media
            .values()
            .any(|binding| !binding.model.trim().is_empty());
    if input.model.trim().is_empty() && !has_media_model {
        return Err("至少配置一个对话或媒体模型".into());
    }
    let now = crate::app::ai_provider_import::unix_now_seconds().to_string();
    let mut services = load_all()?;
    let existing = services.get(&id);
    let normalized_base_url = input.base_url.trim().trim_end_matches('/').to_string();
    let balance_profile_id = profile_override
        .unwrap_or_else(|| existing.and_then(|item| item.balance_profile_id.clone()));
    let api_key_replaced = !input.api_key.trim().is_empty();
    let balance_user_id = if balance_profile_id.as_deref() == Some("new-api") {
        balance_user_id.or_else(|| existing.and_then(|item| item.balance_user_id.clone()))
    } else {
        None
    };
    if balance_profile_id.as_deref() == Some("new-api") && balance_user_id.is_none() {
        return Err("New API 余额查询需要填写 User ID".into());
    }
    let encrypted_balance_access_token = if balance_profile_id.as_deref() == Some("new-api") {
        match balance_access_token {
            Some(token) => Some(protect_secret_for_current_user(token)?),
            None => existing.and_then(|item| item.encrypted_balance_access_token.clone()),
        }
    } else {
        None
    };
    let invalidate_balance = existing.is_some_and(|item| {
        item.base_url != normalized_base_url
            || item.balance_profile_id != balance_profile_id
            || item.balance_user_id != balance_user_id
            || item.encrypted_balance_access_token != encrypted_balance_access_token
            || api_key_replaced
    });
    let unsupported = input
        .media
        .keys()
        .map(|capability| capability.trim())
        .filter(|capability| !SUPPORTED_MEDIA_CAPABILITIES.contains(capability))
        .collect::<Vec<_>>();
    if !unsupported.is_empty() {
        return Err(format!(
            "不支持的媒体能力槽位：{}；可选 {}",
            unsupported.join("、"),
            SUPPORTED_MEDIA_CAPABILITIES.join("、")
        )
        .into());
    } // 能力槽位是事实源；`image_model` / `image_adapter` 作为 `image.generate`
      // 槽位的旧投影一并写回，保证旧版本 Agent 仍能读到生图配置。
    let mut media = input
        .media
        .iter()
        .filter_map(|(capability, binding)| {
            let model = binding.model.trim();
            if model.is_empty() {
                return None;
            }
            let adapter = if binding.adapter.trim().is_empty() {
                default_media_adapter(capability).to_string()
            } else {
                binding.adapter.trim().to_string()
            };
            Some((
                capability.trim().to_string(),
                MediaModelBinding {
                    adapter,
                    model: model.to_string(),
                    execution_mode: binding.execution_mode,
                },
            ))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    if !input.image_model.trim().is_empty() {
        let adapter = if input.image_adapter.trim().is_empty() {
            DEFAULT_MEDIA_ADAPTER.to_string()
        } else {
            input.image_adapter.trim().to_string()
        };
        media.insert(
            CAP_IMAGE_GENERATE.to_string(),
            MediaModelBinding {
                adapter,
                model: input.image_model.trim().to_string(),
                execution_mode: None,
            },
        );
    }
    let legacy = media.get(CAP_IMAGE_GENERATE).cloned().unwrap_or_default();
    let legacy_image_model = legacy.model.clone();
    let legacy_image_adapter = if legacy.model.is_empty() {
        String::new()
    } else {
        legacy.adapter.clone()
    };
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
        base_url: normalized_base_url,
        protocol: input.protocol,
        model: input.model.trim().to_string(),
        models: input
            .models
            .iter()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect(),
        image_model: legacy_image_model.clone(),
        image_adapter: legacy_image_adapter.clone(),
        media,
        balance_profile_id,
        balance_user_id,
        encrypted_balance_access_token,
        encrypted_api_key,
        created_at: existing
            .map(|item| item.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
    };
    services.insert(id.clone(), service.clone());
    save_all(&services)?;
    if invalidate_balance {
        let _ = crate::store::ai_service_balances::remove(&id);
    }
    Ok(service)
}

pub(crate) fn remove(id: &str) -> Result<bool, Box<dyn Error>> {
    let id = id.trim();
    let mut services = load_all()?;
    let removed = services.remove(id).is_some();
    if removed {
        save_all(&services)?;
        let _ = crate::store::ai_service_balances::remove(id);
        if active_id()?.as_deref() == Some(id) {
            set_active("")?;
        }
    }
    Ok(removed)
}

fn normalize_balance_profile_id(value: &str) -> Result<String, Box<dyn Error>> {
    let normalized = value.trim().to_ascii_lowercase();
    if SUPPORTED_BALANCE_PROFILES.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(format!("不支持的余额查询档案：{value}").into())
    }
}

pub(crate) fn load_balance_credentials(
    id: &str,
) -> Result<(CustomAIService, String, Option<String>, Option<String>), Box<dyn Error>> {
    let (service, api_key) = load_secret(id)?;
    let access_token = service
        .encrypted_balance_access_token
        .as_deref()
        .map(unprotect_secret_for_current_user)
        .transpose()?;
    Ok((
        service.clone(),
        api_key,
        access_token,
        service.balance_user_id.clone(),
    ))
}

pub(crate) fn load_secret(id: &str) -> Result<(CustomAIService, String), Box<dyn Error>> {
    let services = load_all()?;
    let service = services
        .get(id.trim())
        .ok_or_else(|| format!("自定义 AI 服务不存在：{id}"))?;
    let api_key = unprotect_secret_for_current_user(&service.encrypted_api_key)?;
    Ok((service.clone(), api_key))
}

pub(crate) fn default_media_adapter(capability: &str) -> &'static str {
    match capability.trim() {
        CAP_VIDEO_GENERATE => "ark-video",
        CAP_AUDIO_SPEECH => "openai-speech",
        CAP_AUDIO_TRANSCRIBE => "openai-audio",
        _ => DEFAULT_MEDIA_ADAPTER,
    }
}

/// The strongest persisted routing choice for a capability. Chat keeps using
/// the active service, while media uses the per-capability default.
pub(crate) fn configured_capability_service(
    capability: &str,
) -> Result<Option<String>, Box<dyn Error>> {
    if capability.trim() == CAP_CHAT {
        return active_id();
    }
    media_service_id(capability)
}

/// Explicit fallback order for a capability. An empty list means "do not
/// silently substitute another provider".
pub(crate) fn capability_fallbacks(capability: &str) -> Result<Vec<String>, Box<dyn Error>> {
    Ok(load_selection()?
        .capability_fallbacks
        .get(capability.trim())
        .cloned()
        .unwrap_or_default())
}

/// Persist a same-capability fallback order. Every id must exist and actually
/// support that capability; duplicates and the primary are ignored in the
/// returned normalized list.
pub(crate) fn set_capability_fallbacks(
    capability: &str,
    ids: Vec<String>,
) -> Result<Vec<String>, Box<dyn Error>> {
    let capability = capability.trim();
    if !SUPPORTED_AI_CAPABILITIES.contains(&capability) {
        return Err(format!("不支持的能力：{capability}").into());
    }
    let services = load_all()?;
    let primary = configured_capability_service(capability)?.unwrap_or_default();
    let mut seen = std::collections::BTreeSet::new();
    let mut normalized = Vec::new();
    for id in ids {
        let id = id.trim();
        if id.is_empty() || id == primary || !seen.insert(id.to_string()) {
            continue;
        }
        let service = services
            .get(id)
            .ok_or_else(|| format!("自定义 AI 服务不存在：{id}"))?;
        if !service_supports_capability(service, capability) {
            return Err(format!("服务「{}」不支持能力 {capability}", service.display_name).into());
        }
        normalized.push(id.to_string());
    }
    let mut selection = load_selection()?;
    if normalized.is_empty() {
        selection.capability_fallbacks.remove(capability);
    } else {
        selection
            .capability_fallbacks
            .insert(capability.to_string(), normalized.clone());
    }
    write_selection(&selection)?;
    Ok(normalized)
}

pub(crate) fn service_supports_capability(service: &CustomAIService, capability: &str) -> bool {
    if capability == CAP_CHAT {
        return !service.model.trim().is_empty();
    }
    binding_details_for(service, capability).is_some()
}

/// The default lifecycle for a media primitive. Providers may override it on the
/// binding, but callers must not force every media operation into an async job.
pub(crate) fn default_execution_mode(capability: &str) -> MediaExecutionMode {
    match capability.trim() {
        CAP_VIDEO_GENERATE => MediaExecutionMode::Async,
        _ => MediaExecutionMode::Sync,
    }
}

/// Resolve a capability binding without exposing provider credentials.
pub(crate) fn binding_details_for(
    service: &CustomAIService,
    capability: &str,
) -> Option<ResolvedMediaBinding> {
    let capability = capability.trim();
    if !SUPPORTED_MEDIA_CAPABILITIES.contains(&capability) {
        return None;
    }
    let explicit = service.media.get(capability);
    let (adapter, model, execution_mode) = if let Some(binding) = explicit {
        let model = binding.model.trim();
        if model.is_empty() {
            return None;
        }
        let adapter = if binding.adapter.trim().is_empty() {
            default_media_adapter(capability).to_string()
        } else {
            binding.adapter.trim().to_string()
        };
        (
            adapter,
            model.to_string(),
            binding
                .execution_mode
                .unwrap_or_else(|| default_execution_mode(capability)),
        )
    } else if capability == CAP_IMAGE_GENERATE && !service.image_model.trim().is_empty() {
        let adapter = if service.image_adapter.trim().is_empty() {
            default_media_adapter(capability).to_string()
        } else {
            service.image_adapter.trim().to_string()
        };
        (
            adapter,
            service.image_model.trim().to_string(),
            default_execution_mode(capability),
        )
    } else {
        return None;
    };
    Some(ResolvedMediaBinding {
        adapter,
        model,
        execution_mode,
    })
}

/// 取某个服务在指定媒体能力上的「适配器 + 模型」；未配置该能力时返回 `None`。
///
/// `image.generate` 会回落到 `image_model` / `image_adapter` 这两个旧字段，
/// 保证升级前后行为一致。这是保留给旧调用方的兼容入口；新代码应使用
/// [`binding_details_for`] 读取执行模式。
pub(crate) fn binding_for(service: &CustomAIService, capability: &str) -> Option<(String, String)> {
    binding_details_for(service, capability).map(|binding| (binding.adapter, binding.model))
}

/// Whether at least one saved service can satisfy a media capability.
pub(crate) fn media_capability_configured(capability: &str) -> Result<bool, Box<dyn Error>> {
    Ok(list()?
        .iter()
        .any(|service| binding_details_for(service, capability).is_some()))
}

/// Explain how a capability resolves without decrypting any credential.
///
/// This is the read model used by the settings UI and by the capability bus.
/// It deliberately performs no provider I/O: eligibility here means "saved,
/// bound, and has a credential", not "the provider is healthy right now".
pub(crate) fn capability_route_plan(
    capability: &str,
    requested_service: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let capability = capability.trim();
    if !SUPPORTED_AI_CAPABILITIES.contains(&capability) {
        return Err(format!(
            "不支持的能力：{capability}；可选 {}",
            SUPPORTED_AI_CAPABILITIES.join("、")
        )
        .into());
    }
    let services = list()?;
    let candidates = services
        .iter()
        .filter(|service| service_supports_capability(service, capability))
        .collect::<Vec<_>>();
    let requested = requested_service
        .trim()
        .strip_prefix("custom:")
        .unwrap_or(requested_service.trim());
    let mut reason = "";
    let mut selected: Option<&CustomAIService> = None;
    if !requested.is_empty() {
        let service = candidates
            .iter()
            .copied()
            .find(|service| service.id == requested)
            .ok_or_else(|| {
                format!(
                    "服务 {requested} 不存在或未配置能力 {capability}；请先在 AI 服务中绑定该能力"
                )
            })?;
        selected = Some(service);
        reason = "explicit_request";
    } else if let Some(id) = configured_capability_service(capability)? {
        selected = candidates.iter().copied().find(|service| service.id == id);
        if selected.is_some() {
            reason = if capability == CAP_CHAT {
                "active_service"
            } else {
                "configured_default"
            };
        }
    }
    if selected.is_none() && candidates.len() == 1 {
        selected = Some(candidates[0]);
        reason = "only_candidate";
    }
    let fallbacks = capability_fallbacks(capability)?
        .into_iter()
        .filter(|id| selected.is_none_or(|service| service.id != *id))
        .filter_map(|id| {
            candidates
                .iter()
                .copied()
                .find(|service| service.id == id)
                .map(|service| (id, service))
        })
        .collect::<Vec<_>>();

    let candidate_json = candidates
        .iter()
        .map(|service| {
            let credential_ready = !service.encrypted_api_key.trim().is_empty();
            let (adapter, model, execution_mode) = if capability == CAP_CHAT {
                (
                    service.protocol.as_str().to_string(),
                    service.model.clone(),
                    "sync".to_string(),
                )
            } else {
                let binding = binding_details_for(service, capability)
                    .ok_or_else(|| format!("服务 {} 的能力绑定已失效", service.id))?;
                (
                    binding.adapter,
                    binding.model,
                    match binding.execution_mode {
                        MediaExecutionMode::Sync => "sync",
                        MediaExecutionMode::Stream => "stream",
                        MediaExecutionMode::Async => "async",
                    }
                    .to_string(),
                )
            };
            let is_selected = selected.is_some_and(|selected| selected.id == service.id);
            let fallback_index = fallbacks.iter().position(|(id, _)| id == &service.id);
            let eligibility = if is_selected {
                "selected"
            } else if fallback_index.is_some() {
                "fallback"
            } else {
                "candidate"
            };
            Ok(serde_json::json!({
                "service_id": service.id,
                "display_name": service.display_name,
                "eligible": credential_ready,
                "eligibility": eligibility,
                "reason": if !credential_ready {
                    "missing_credential"
                } else if is_selected {
                    reason
                } else if fallback_index.is_some() {
                    "configured_fallback"
                } else {
                    "not_selected"
                },
                "adapter": adapter,
                "model": model,
                "execution_mode": execution_mode,
                "credential_present": credential_ready,
            }))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;

    let attempts = selected
        .into_iter()
        .map(|service| (service, "primary"))
        .chain(fallbacks.iter().map(|(_, service)| (*service, "fallback")))
        .filter(|(service, _)| !service.encrypted_api_key.trim().is_empty())
        .map(|(service, role)| {
            serde_json::json!({
                "service_id": service.id,
                "display_name": service.display_name,
                "role": role,
            })
        })
        .collect::<Vec<_>>();
    let state = if selected.is_some() {
        "resolved"
    } else if candidates.is_empty() {
        "missing"
    } else {
        "ambiguous"
    };
    let selected_binding = selected.and_then(|service| {
        if capability == CAP_CHAT {
            Some((
                service.protocol.as_str().to_string(),
                service.model.clone(),
                "sync".to_string(),
            ))
        } else {
            binding_details_for(service, capability).map(|binding| {
                (
                    binding.adapter,
                    binding.model,
                    match binding.execution_mode {
                        MediaExecutionMode::Sync => "sync",
                        MediaExecutionMode::Stream => "stream",
                        MediaExecutionMode::Async => "async",
                    }
                    .to_string(),
                )
            })
        }
    });
    Ok(serde_json::json!({
        "schema_version": "himind.ai-capability-route.v1",
        "capability": capability,
        "state": state,
        "reason": if selected.is_some() {
            reason
        } else if candidates.is_empty() {
            "no_candidate"
        } else {
            "multiple_candidates_without_default"
        },
        "selected": selected.map(|service| serde_json::json!({
            "service_id": service.id,
            "display_name": service.display_name,
            "adapter": selected_binding.as_ref().map(|binding| binding.0.clone()),
            "model": selected_binding.as_ref().map(|binding| binding.1.clone()),
            "execution_mode": selected_binding.as_ref().map(|binding| binding.2.clone()),
        })),
        "fallbacks": fallbacks.iter().map(|(_, service)| serde_json::json!({
            "service_id": service.id,
            "display_name": service.display_name,
        })).collect::<Vec<_>>(),
        "attempts": attempts,
        "candidates": candidate_json,
        "requires_cost_confirmation": capability != CAP_CHAT,
        "fallback_policy": "same_capability_only",
    }))
}

/// 解析某个媒体能力要用的服务与明文 Key。
///
/// `service` 为空时按「该能力的默认服务 → 激活服务（且已配该能力）→ 唯一支持该
/// 能力的服务」解析；否则必须是 `custom:<id>`。只读本机状态库（DPAPI 解密），
/// 不触碰 AI 工作台。
pub(crate) fn resolve_media_service(
    capability: &str,
    service: &str,
) -> Result<(CustomAIService, String), Box<dyn Error>> {
    let capability = capability.trim();
    let service = service.trim();
    if !service.is_empty() {
        let id = service.strip_prefix("custom:").ok_or_else(|| {
            format!("不支持的服务源：{service}；本机媒体能力只接受 custom:<id>，留空表示自动选择")
        })?;
        return load_secret(id);
    }
    if let Some(id) = media_service_id(capability)? {
        if let Ok(found) = load_secret(&id) {
            return Ok(found);
        }
    }
    if let Some((active, api_key)) = active_service_with_secret()? {
        if binding_for(&active, capability).is_some() {
            return Ok((active, api_key));
        }
    }
    let candidates = list()?
        .into_iter()
        .filter(|item| binding_for(item, capability).is_some())
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [only] => load_secret(&only.id),
        [] => Err(format!(
            "没有为本机 AI 服务配置「{capability}」能力；请先在「AI 服务」里为该能力填写模型"
        )
        .into()),
        many => Err(format!(
            "有多个服务支持 {capability}，请显式指定 service 或设置该能力的默认服务：{}",
            many.iter()
                .map(|item| format!("custom:{}", item.id))
                .collect::<Vec<_>>()
                .join("、")
        )
        .into()),
    }
}

/// 解析本机生图（`media.image.generate`）要用的服务与明文 Key。
pub(crate) fn resolve_image_service(
    service: &str,
) -> Result<(CustomAIService, String), Box<dyn Error>> {
    resolve_media_service(CAP_IMAGE_GENERATE, service)
}

fn load_selection() -> Result<PersistedAIServiceSelection, Box<dyn Error>> {
    let path = selection_path()?;
    if !path.is_file() {
        return Ok(PersistedAIServiceSelection::default());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_selection(selection: &PersistedAIServiceSelection) -> Result<(), Box<dyn Error>> {
    crate::store::atomic_file::atomic_write(
        &selection_path()?,
        &serde_json::to_vec_pretty(selection)?,
    )?;
    Ok(())
}

pub(crate) fn active_id() -> Result<Option<String>, Box<dyn Error>> {
    let id = load_selection()?.active_service_id.trim().to_string();
    Ok((!id.is_empty()).then_some(id))
}

/// 某个媒体能力的默认服务；留空表示按「激活服务 → 唯一支持该能力的服务」自动解析。
pub(crate) fn media_service_id(capability: &str) -> Result<Option<String>, Box<dyn Error>> {
    let capability = capability.trim();
    let selection = load_selection()?;
    if let Some(id) = selection.media_services.get(capability) {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Ok(Some(id));
        }
    }
    if capability == CAP_IMAGE_GENERATE {
        let id = selection.image_service_id.trim().to_string();
        if !id.is_empty() {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// 生图默认使用的本机服务（`media.image.generate`）。
pub(crate) fn image_service_id() -> Result<Option<String>, Box<dyn Error>> {
    media_service_id(CAP_IMAGE_GENERATE)
}

/// 指定（或清除）某个媒体能力的默认服务；不影响对话使用的激活服务。
pub(crate) fn set_media_service(
    capability: &str,
    id: &str,
) -> Result<Option<CustomAIService>, Box<dyn Error>> {
    let capability = capability.trim();
    if !SUPPORTED_MEDIA_CAPABILITIES.contains(&capability) {
        return Err(format!(
            "不支持的本机媒体能力：{capability}；可选 {}",
            SUPPORTED_MEDIA_CAPABILITIES.join("、")
        )
        .into());
    }
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
    let mut selection = load_selection()?;
    let value = selected
        .as_ref()
        .map(|service| service.id.clone())
        .unwrap_or_default();
    if value.is_empty() {
        selection.media_services.remove(capability);
    } else {
        selection
            .media_services
            .insert(capability.to_string(), value.clone());
    }
    if capability == CAP_IMAGE_GENERATE {
        selection.image_service_id = value;
    }
    write_selection(&selection)?;
    Ok(selected)
}

/// 指定（或清除）生图默认使用的本机服务；是 [`set_media_service`] 的旧入口。
pub(crate) fn set_image_service(id: &str) -> Result<Option<CustomAIService>, Box<dyn Error>> {
    set_media_service(CAP_IMAGE_GENERATE, id)
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
    let mut selection = load_selection()?;
    selection.active_service_id = selected
        .as_ref()
        .map(|service| service.id.clone())
        .unwrap_or_default();
    write_selection(&selection)?;
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
    let mut client_builder =
        reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(15));
    if url::Url::parse(base_url.trim())
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]"))
    {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder.build()?;
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
    use super::{
        binding_details_for, binding_for, default_execution_mode, validate_base_url,
        AIServiceProtocol, CustomAIServiceInput, MediaExecutionMode, MediaModelBinding,
        CAP_AUDIO_SPEECH, CAP_AUDIO_TRANSCRIBE, CAP_IMAGE_EDIT, CAP_IMAGE_GENERATE,
        CAP_VIDEO_GENERATE,
    };
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
                image_model: String::new(),
                image_adapter: String::new(),
                media: Default::default(),
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

    fn balance_test_service(api_key: &str) -> CustomAIServiceInput {
        CustomAIServiceInput {
            id: "deepseek-account".to_string(),
            display_name: "DeepSeek 余额测试".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            protocol: AIServiceProtocol::OpenaiResponses,
            model: "deepseek-chat".to_string(),
            models: vec!["deepseek-chat".to_string()],
            image_model: String::new(),
            image_adapter: String::new(),
            media: Default::default(),
            api_key: api_key.to_string(),
        }
    }

    #[test]
    fn balance_profile_is_persisted_preserved_and_cache_cleared_on_key_rotation() {
        with_isolated_home(|| {
            let service = super::upsert_with_balance_profile(
                balance_test_service("sk-first"),
                Some(" DeepSeek "),
                None,
                None,
            )
            .expect("save profile");
            assert_eq!(service.balance_profile_id.as_deref(), Some("deepseek"));

            let (reloaded, _) = super::load_secret("deepseek-account").expect("reload service");
            assert_eq!(reloaded.balance_profile_id.as_deref(), Some("deepseek"));
            assert!(super::upsert_with_balance_profile(
                balance_test_service(""),
                Some("unknown"),
                None,
                None,
            )
            .is_err());

            let cached = crate::store::ai_service_balances::AISourceBalanceSnapshot {
                provider_profile_id: "deepseek".to_string(),
                kind: "balance".to_string(),
                balances: vec![crate::store::ai_service_balances::BalanceCurrency {
                    currency: "USD".to_string(),
                    available: 1.25,
                    total: Some(1.25),
                    components: Default::default(),
                }],
                plan_name: None,
                quota_windows: Vec::new(),
                is_available: Some(true),
                observed_at: "1".to_string(),
            };
            crate::store::ai_service_balances::save("deepseek-account", cached)
                .expect("cache balance");

            let preserved =
                super::upsert(balance_test_service("")).expect("legacy update preserves profile");
            assert_eq!(preserved.balance_profile_id.as_deref(), Some("deepseek"));
            assert!(crate::store::ai_service_balances::list().contains_key("deepseek-account"));

            super::upsert(balance_test_service("sk-rotated")).expect("rotate API key");
            assert!(!crate::store::ai_service_balances::list().contains_key("deepseek-account"));
        });
    }

    #[test]
    fn new_api_balance_token_is_encrypted_and_not_returned_in_public_service() {
        with_isolated_home(|| {
            let mut input = balance_test_service("sk-inference");
            let saved = super::upsert_with_balance_profile(
                input.clone(),
                Some("new-api"),
                Some("access-token-secret"),
                Some("42"),
            )
            .expect("save New API credentials");
            assert_eq!(saved.balance_profile_id.as_deref(), Some("new-api"));
            assert_eq!(saved.balance_user_id.as_deref(), Some("42"));
            let public = saved.public_json().to_string();
            assert!(!public.contains("access-token-secret"));
            assert!(!public.contains("encrypted_balance_access_token"));

            let (loaded, api_key, access_token, user_id) =
                super::load_balance_credentials("deepseek-account")
                    .expect("load balance credentials");
            assert_eq!(api_key, "sk-inference");
            assert_eq!(access_token.as_deref(), Some("access-token-secret"));
            assert_eq!(user_id.as_deref(), Some("42"));

            input.display_name = "updated".to_string();
            let preserved =
                super::upsert_with_balance_profile(input, Some("new-api"), None, Some("43"))
                    .expect("preserve saved access token when editing without re-entering it");
            assert_eq!(preserved.balance_user_id.as_deref(), Some("43"));
            let (_, _, access_token, _) = super::load_balance_credentials("deepseek-account")
                .expect("reload preserved access token");
            assert_eq!(access_token.as_deref(), Some("access-token-secret"));
        });
    }

    #[test]
    fn public_service_suggests_balance_template_from_exact_base_url_only() {
        with_isolated_home(|| {
            let mut input = balance_test_service("sk-suggest");
            input.base_url = "https://api.openrouter.ai/api/v1".to_string();
            let saved = super::upsert(input).expect("save matching service");
            let public = saved.public_json();
            assert_eq!(public["suggested_balance_profile_id"], "openrouter");
            assert!(public.get("encrypted_balance_access_token").is_none());

            let mut custom = balance_test_service("sk-custom");
            custom.id = "custom-relay".to_string();
            custom.base_url = "https://openrouter.ai.attacker.example/v1".to_string();
            let public = super::upsert(custom)
                .expect("save custom host")
                .public_json();
            assert!(public["suggested_balance_profile_id"].is_null());
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
                image_model: String::new(),
                image_adapter: String::new(),
                media: Default::default(),
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
                image_model: String::new(),
                image_adapter: String::new(),
                media: Default::default(),
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
                    image_model: String::new(),
                    image_adapter: String::new(),
                    media: Default::default(),
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
    fn image_service_selection_is_independent_from_the_chat_service() {
        with_isolated_home(|| {
            super::upsert(CustomAIServiceInput {
                id: "chat-only".to_string(),
                display_name: "纯对话".to_string(),
                base_url: "https://chat.example/v1".to_string(),
                protocol: AIServiceProtocol::OpenaiChat,
                model: "chat-model".to_string(),
                models: vec!["chat-model".to_string()],
                image_model: String::new(),
                image_adapter: String::new(),
                media: Default::default(),
                api_key: "sk-chat".to_string(),
            })
            .expect("chat service");
            super::upsert(CustomAIServiceInput {
                id: "image-only".to_string(),
                display_name: "豆包生图".to_string(),
                base_url: "https://ark.example/api/v3".to_string(),
                protocol: AIServiceProtocol::OpenaiResponses,
                model: "doubao-seed".to_string(),
                models: vec!["doubao-seed".to_string()],
                image_model: "doubao-seedream-5-0-pro".to_string(),
                image_adapter: "ark-images".to_string(),
                media: Default::default(),
                api_key: "sk-image".to_string(),
            })
            .expect("image service");

            super::set_active("chat-only").unwrap();
            // 激活服务没有生图模型时，唯一配了生图模型的服务会自动兜底。
            let (service, _) = super::resolve_image_service("").expect("auto fallback");
            assert_eq!(service.id, "image-only");
            assert_eq!(service.image_adapter, "ark-images");

            super::set_image_service("image-only").unwrap();
            assert_eq!(
                super::image_service_id().unwrap().as_deref(),
                Some("image-only")
            );
            // 切换对话服务不得清掉生图服务。
            super::set_active("chat-only").unwrap();
            assert_eq!(
                super::image_service_id().unwrap().as_deref(),
                Some("image-only")
            );
            let (explicit, _) =
                super::resolve_image_service("custom:image-only").expect("explicit service");
            assert_eq!(explicit.id, "image-only");
        });
    }

    #[test]
    fn media_slots_cover_edit_and_mirror_the_legacy_image_projection() {
        with_isolated_home(|| {
            let mut media = std::collections::BTreeMap::new();
            media.insert(
                CAP_IMAGE_GENERATE.to_string(),
                MediaModelBinding {
                    adapter: "ark-images".to_string(),
                    model: "doubao-seedream-5-0-pro".to_string(),
                    execution_mode: None,
                },
            );
            media.insert(
                CAP_IMAGE_EDIT.to_string(),
                MediaModelBinding {
                    adapter: "ark-images".to_string(),
                    model: "doubao-seededit-4-0-i2i".to_string(),
                    execution_mode: None,
                },
            );
            let saved = super::upsert(CustomAIServiceInput {
                id: "multimodal".to_string(),
                display_name: "多能力服务".to_string(),
                base_url: "https://ark.example/api/v3".to_string(),
                protocol: AIServiceProtocol::OpenaiChat,
                model: "chat".to_string(),
                models: vec!["chat".to_string()],
                image_model: String::new(),
                image_adapter: String::new(),
                media,
                api_key: "sk-x".to_string(),
            })
            .expect("upsert");

            // 能力槽位是事实源，`image_model` / `image_adapter` 是 image.generate 的旧投影。
            assert_eq!(saved.media.len(), 2);
            assert_eq!(saved.image_model, "doubao-seedream-5-0-pro");
            assert_eq!(saved.image_adapter, "ark-images");
            assert_eq!(
                binding_for(&saved, CAP_IMAGE_EDIT),
                Some((
                    "ark-images".to_string(),
                    "doubao-seededit-4-0-i2i".to_string()
                ))
            );
            assert!(binding_for(&saved, CAP_VIDEO_GENERATE).is_none());

            // 重新读盘后槽位仍在（持久化 round-trip）。
            let (reloaded, _) = super::load_secret("multimodal").expect("reload");
            assert_eq!(reloaded.media.len(), 2);
        });
    }

    #[test]
    fn legacy_image_fields_are_backfilled_into_the_generate_slot() {
        with_isolated_home(|| {
            super::upsert(CustomAIServiceInput {
                id: "legacy".to_string(),
                display_name: "旧配置".to_string(),
                base_url: "https://api.example.com/v1".to_string(),
                protocol: AIServiceProtocol::OpenaiResponses,
                model: "chat".to_string(),
                models: vec!["chat".to_string()],
                image_model: "gpt-image-1".to_string(),
                image_adapter: "openai-images".to_string(),
                media: std::collections::BTreeMap::new(),
                api_key: "sk-legacy".to_string(),
            })
            .expect("upsert legacy");
            let (service, _) = super::load_secret("legacy").expect("load");
            assert_eq!(
                binding_for(&service, CAP_IMAGE_GENERATE),
                Some(("openai-images".to_string(), "gpt-image-1".to_string()))
            );
        });
    }

    #[test]
    fn each_media_capability_can_pin_its_own_service() {
        with_isolated_home(|| {
            for (id, image_model) in [("only-generate", "img-a"), ("only-edit", "")] {
                super::upsert(CustomAIServiceInput {
                    id: id.to_string(),
                    display_name: id.to_string(),
                    base_url: format!("https://{id}.example/v1"),
                    protocol: AIServiceProtocol::OpenaiResponses,
                    model: "chat".to_string(),
                    models: vec!["chat".to_string()],
                    image_model: image_model.to_string(),
                    image_adapter: String::new(),
                    media: if id == "only-edit" {
                        let mut media = std::collections::BTreeMap::new();
                        media.insert(
                            CAP_IMAGE_EDIT.to_string(),
                            MediaModelBinding {
                                adapter: "openai-images-edit".to_string(),
                                model: "edit-model".to_string(),
                                execution_mode: None,
                            },
                        );
                        media
                    } else {
                        std::collections::BTreeMap::new()
                    },
                    api_key: "sk".to_string(),
                })
                .expect("upsert");
            }

            super::set_media_service(CAP_IMAGE_GENERATE, "only-generate").unwrap();
            super::set_media_service(CAP_IMAGE_EDIT, "only-edit").unwrap();

            assert_eq!(
                super::media_service_id(CAP_IMAGE_GENERATE)
                    .unwrap()
                    .as_deref(),
                Some("only-generate")
            );
            assert_eq!(
                super::media_service_id(CAP_IMAGE_EDIT).unwrap().as_deref(),
                Some("only-edit")
            );
            // image.generate 的旧入口读的是同一个槽位
            assert_eq!(
                super::image_service_id().unwrap().as_deref(),
                Some("only-generate")
            );
            assert!(super::set_media_service("video.generate", "only-edit").is_ok());
            assert!(super::set_media_service("tts.generate", "only-edit").is_err());
        });
    }

    #[test]
    fn builtin_adapter_registry_covers_all_media_capabilities() {
        let adapters = super::builtin_media_adapters();
        let serialized = serde_json::to_string(&adapters).unwrap();
        assert!(serialized.contains("openai-images"));
        assert!(serialized.contains("ark-video"));
        assert!(serialized.contains("openai-speech"));
        assert!(serialized.contains("openai-audio"));
        assert!(serialized.contains("audio.speech"));
        assert!(serialized.contains("audio.transcribe"));
        for adapter in adapters {
            assert!(adapter["features"]
                .as_array()
                .is_some_and(|items| !items.is_empty()));
            assert!(adapter["limits"].is_object());
            assert!(adapter["pricing"].is_object());
            assert!(adapter["usage"].is_object());
        }
    }

    #[test]
    fn provider_catalog_reports_media_without_exposing_secrets() {
        with_isolated_home(|| {
            let mut media = std::collections::BTreeMap::new();
            media.insert(
                CAP_AUDIO_SPEECH.to_string(),
                MediaModelBinding {
                    adapter: "openai-speech".to_string(),
                    model: "tts-1".to_string(),
                    execution_mode: Some(MediaExecutionMode::Stream),
                },
            );
            super::upsert(CustomAIServiceInput {
                id: "voice".to_string(),
                display_name: "Voice".to_string(),
                base_url: "https://voice.example/v1".to_string(),
                protocol: AIServiceProtocol::OpenaiResponses,
                model: "chat".to_string(),
                models: vec!["chat".to_string()],
                image_model: String::new(),
                image_adapter: String::new(),
                media,
                api_key: "sk-catalog-secret".to_string(),
            })
            .expect("upsert voice");

            let catalog = super::media_provider_catalog().expect("provider catalog");
            let serialized = serde_json::to_string(&catalog).expect("serialize catalog");
            assert!(serialized.contains("audio.speech"));
            assert!(serialized.contains("openai-speech"));
            assert!(!serialized.contains("sk-catalog-secret"));
            let speech_capability = catalog["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == "audio.speech")
                .unwrap();
            assert_eq!(speech_capability["configured"], true);
            assert!(speech_capability["execution_modes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|mode| mode == "sync"));
            assert_eq!(
                speech_capability["execution_modes"],
                serde_json::json!(["sync"])
            );
            assert_eq!(speech_capability["adapter_count"], 2);
            let voice = catalog["providers"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == "voice")
                .unwrap();
            assert_eq!(voice["health"]["state"], "ready");
            let binding = &voice["media_capabilities"][0];
            assert!(binding["features"]
                .as_array()
                .unwrap()
                .iter()
                .any(|feature| feature == "text_to_speech"));
            assert_eq!(binding["limits"]["text_chars"], 4096);
            assert_eq!(binding["pricing"]["status"], "unknown");
            assert_eq!(binding["usage"]["status"], "unknown");
        });
    }
    #[test]
    fn media_only_service_can_save_without_chat_model() {
        with_isolated_home(|| {
            let mut media = std::collections::BTreeMap::new();
            media.insert(
                CAP_IMAGE_GENERATE.to_string(),
                MediaModelBinding {
                    adapter: "openai-images".to_string(),
                    model: "gpt-image-1".to_string(),
                    execution_mode: None,
                },
            );
            let saved = super::upsert(CustomAIServiceInput {
                id: "media-only".to_string(),
                display_name: "Media Only".to_string(),
                base_url: "https://media.example/v1".to_string(),
                protocol: AIServiceProtocol::OpenaiResponses,
                model: String::new(),
                models: Vec::new(),
                image_model: String::new(),
                image_adapter: String::new(),
                media,
                api_key: "sk-media-only".to_string(),
            })
            .expect("media-only service");
            assert!(saved.model.is_empty());
            assert_eq!(
                super::binding_for(&saved, CAP_IMAGE_GENERATE),
                Some(("openai-images".to_string(), "gpt-image-1".to_string()))
            );
        });
    }

    #[test]
    fn service_without_chat_or_media_model_is_rejected() {
        with_isolated_home(|| {
            let error = super::upsert(CustomAIServiceInput {
                id: "empty-models".to_string(),
                display_name: "Empty Models".to_string(),
                base_url: "https://empty.example/v1".to_string(),
                protocol: AIServiceProtocol::OpenaiResponses,
                model: String::new(),
                models: Vec::new(),
                image_model: String::new(),
                image_adapter: String::new(),
                media: std::collections::BTreeMap::new(),
                api_key: "sk-empty".to_string(),
            })
            .expect_err("empty model service must fail");
            assert!(error.to_string().contains("至少配置一个对话或媒体模型"));
        });
    }
    #[test]
    fn media_bindings_reject_unknown_capabilities_and_fields() {
        let with_business_field: Result<MediaModelBinding, _> =
            serde_json::from_value(serde_json::json!({
                "adapter": "openai-speech",
                "model": "tts-1",
                "voice_license": "cc0"
            }));
        assert!(with_business_field.is_err());

        with_isolated_home(|| {
            let mut media = std::collections::BTreeMap::new();
            media.insert(
                "audio.mix".to_string(),
                MediaModelBinding {
                    adapter: "ffmpeg".to_string(),
                    model: "mix".to_string(),
                    execution_mode: None,
                },
            );
            let error = super::upsert(CustomAIServiceInput {
                id: "bad-media".to_string(),
                display_name: "Bad Media".to_string(),
                base_url: "https://bad.example/v1".to_string(),
                protocol: AIServiceProtocol::OpenaiResponses,
                model: "chat".to_string(),
                models: vec!["chat".to_string()],
                image_model: String::new(),
                image_adapter: String::new(),
                media,
                api_key: "sk-bad".to_string(),
            })
            .expect_err("business binding must be rejected");
            assert!(error.to_string().contains("不支持的媒体能力槽位"));
        });
    }
    #[test]
    fn audio_provider_slots_round_trip_without_business_fields() {
        with_isolated_home(|| {
            let mut media = std::collections::BTreeMap::new();
            media.insert(
                CAP_AUDIO_SPEECH.to_string(),
                MediaModelBinding {
                    adapter: "openai-speech".to_string(),
                    model: "tts-1".to_string(),
                    execution_mode: Some(MediaExecutionMode::Stream),
                },
            );
            media.insert(
                CAP_AUDIO_TRANSCRIBE.to_string(),
                MediaModelBinding {
                    adapter: "openai-audio".to_string(),
                    model: "whisper-1".to_string(),
                    execution_mode: None,
                },
            );
            super::upsert(CustomAIServiceInput {
                id: "voice".to_string(),
                display_name: "Voice".to_string(),
                base_url: "https://voice.example/v1".to_string(),
                protocol: AIServiceProtocol::OpenaiResponses,
                model: "chat".to_string(),
                models: vec!["chat".to_string()],
                image_model: String::new(),
                image_adapter: String::new(),
                media,
                api_key: "sk-voice".to_string(),
            })
            .expect("upsert audio slots");

            let (service, _) = super::load_secret("voice").expect("load voice service");
            assert!(super::media_capability_configured(CAP_AUDIO_SPEECH).unwrap());
            assert!(super::media_capability_configured(CAP_AUDIO_TRANSCRIBE).unwrap());
            assert_eq!(
                binding_for(&service, CAP_AUDIO_SPEECH),
                Some(("openai-speech".to_string(), "tts-1".to_string()))
            );
            assert_eq!(
                binding_for(&service, CAP_AUDIO_TRANSCRIBE),
                Some(("openai-audio".to_string(), "whisper-1".to_string()))
            );
            let speech = binding_details_for(&service, CAP_AUDIO_SPEECH).unwrap();
            assert_eq!(speech.execution_mode, MediaExecutionMode::Stream);
            let transcribe = binding_details_for(&service, CAP_AUDIO_TRANSCRIBE).unwrap();
            assert_eq!(transcribe.execution_mode, MediaExecutionMode::Sync);
            assert_eq!(
                default_execution_mode(CAP_VIDEO_GENERATE),
                MediaExecutionMode::Async
            );
            super::set_media_service(CAP_AUDIO_SPEECH, "voice").expect("pin speech");
            assert_eq!(
                super::media_service_id(CAP_AUDIO_SPEECH)
                    .unwrap()
                    .as_deref(),
                Some("voice")
            );
            assert!(super::set_media_service("audio.mix", "voice").is_err());
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
            image_model: String::new(),
            image_adapter: String::new(),
            media: Default::default(),
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
