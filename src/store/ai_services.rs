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
/// 视觉理解（看图说话/打标/OCR）。插件只表达“要打标”，由 Agent 决定用哪个服务。
pub(crate) const CAP_VISION: &str = "vision.describe";
pub(crate) const SUPPORTED_MEDIA_CAPABILITIES: &[&str] = &[
    CAP_IMAGE_GENERATE,
    CAP_IMAGE_EDIT,
    CAP_VIDEO_GENERATE,
    CAP_AUDIO_SPEECH,
    CAP_AUDIO_TRANSCRIBE,
    CAP_VISION,
];
pub(crate) const SUPPORTED_AI_CAPABILITIES: &[&str] = &[
    CAP_CHAT,
    CAP_IMAGE_GENERATE,
    CAP_IMAGE_EDIT,
    CAP_VIDEO_GENERATE,
    CAP_AUDIO_SPEECH,
    CAP_AUDIO_TRANSCRIBE,
    CAP_VISION,
];
/// 未显式声明适配器时的默认值（OpenAI Images 兼容协议）。
pub(crate) const DEFAULT_MEDIA_ADAPTER: &str = "openai-images";

/// 一个服务在某个媒体能力上使用的适配器与模型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct MediaModelBinding {
    pub adapter: String,
    /// 该能力的默认模型。路由项 `model` 为空时跟随这个值。
    pub model: String,
    /// 该能力下的其它可选模型。与 `model` 一起构成候选模型目录，去重后使用。
    /// 留空表示该能力只有 `model` 一个模型。
    #[serde(default)]
    pub models: Vec<String>,
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

/// 一条能力路由项：在某个服务上使用某个模型。
///
/// `model` 为空不是「未配置」，而是一个明确语义：**跟随该服务在这个能力上的
/// 默认模型**（对话是 `service.model`，媒体是 `media[capability].model`）。
/// 这样服务默认模型被修改时，未显式选择过模型的路由会跟着变。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CapabilityRouteEntry {
    pub service_id: String,
    pub model: String,
}

impl CapabilityRouteEntry {
    pub(crate) fn new(service_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            service_id: service_id.into(),
            model: model.into(),
        }
    }

    /// 该路由项是否为「跟随服务默认模型」。
    pub(crate) fn follows_service_default(&self) -> bool {
        self.model.trim().is_empty()
    }
}

/// 一个能力的完整路由：下标 0 是主选，其余按序是备用。
///
/// 这是能力路由唯一的持久化事实源。「主选」与「备用」不再用两套字段表达。
type CapabilityRouteTable = BTreeMap<String, Vec<CapabilityRouteEntry>>;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PersistedAIServiceSelection {
    /// 2 = 路由项携带模型。为了在原处保留 shape，版本字段由迁移函数维护。
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    capability_routes: CapabilityRouteTable,
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
                        models: Vec::new(),
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
    // 顺序只由创建时间决定（新的在前），编辑、改密钥、切换默认服务都不能让列表重排。
    // 之前直接用 BTreeMap 的键序（服务 ID），而中文名生成的是 `service-<hash>`，
    // 于是列表看起来是随机序，新增或改名的服务还会插到中间。
    let mut services = load_all()?.into_values().collect::<Vec<_>>();
    services.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(services)
}

pub(crate) fn public_snapshot() -> Result<serde_json::Value, Box<dyn Error>> {
    let services = list()?;
    let routes = load_selection()?.capability_routes;
    Ok(serde_json::json!({
        "services": services.iter().map(|item| item.public_json()).collect::<Vec<_>>(),
        "balances": crate::store::ai_service_balances::list(),
        // 唯一事实源：每个能力一条有序路由表（ADR 0023）。
        "capability_routes": routes,
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
                            "models": capability_models(service, capability),
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
        "schema_version": "himind.media-provider-catalog.v3",
        "adapters": adapters,
        "providers": providers,
        "capabilities": capabilities,
        "routes": route_plans,
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
                    models: binding.models.clone(),
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
        // `image_model` 是 `image.generate` 槽位的旧投影：它只描述默认模型，
        // 不能把槽位里已声明的其它可选模型抹掉。
        let models = media
            .get(CAP_IMAGE_GENERATE)
            .map(|binding| binding.models.clone())
            .unwrap_or_default();
        media.insert(
            CAP_IMAGE_GENERATE.to_string(),
            MediaModelBinding {
                adapter,
                model: input.image_model.trim().to_string(),
                models,
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

fn next_copy_id(source_id: &str, services: &BTreeMap<String, CustomAIService>) -> String {
    for index in 1..10_000 {
        let suffix = if index == 1 {
            "-copy".to_string()
        } else {
            format!("-copy-{index}")
        };
        let max_stem_len = 64usize.saturating_sub(suffix.len());
        let stem = source_id.chars().take(max_stem_len).collect::<String>();
        let candidate = format!("{stem}{suffix}");
        if !services.contains_key(&candidate) {
            return candidate;
        }
    }
    format!(
        "service-copy-{}",
        crate::app::ai_provider_import::unix_now_seconds()
    )
}

fn next_copy_name(source_name: &str, services: &BTreeMap<String, CustomAIService>) -> String {
    let source_name = source_name.trim();
    let first = format!("{source_name} 副本");
    if !services.values().any(|item| item.display_name == first) {
        return first;
    }
    for index in 2..10_000 {
        let candidate = format!("{source_name} 副本 {index}");
        if !services.values().any(|item| item.display_name == candidate) {
            return candidate;
        }
    }
    format!("{source_name} 副本 {first}")
}

/// 复制一个已保存服务。API Key 与余额令牌沿用原服务的 DPAPI 密文，
/// 不经过前端明文；路由、默认服务与客户端分发关系不复制。
pub(crate) fn duplicate(id: &str) -> Result<CustomAIService, Box<dyn Error>> {
    let source_id = normalize_id(id)?;
    let mut services = load_all()?;
    let source = services
        .get(&source_id)
        .cloned()
        .ok_or_else(|| format!("自定义 AI 服务不存在：{source_id}"))?;
    let copy_id = next_copy_id(&source_id, &services);
    let now = crate::app::ai_provider_import::unix_now_seconds().to_string();
    let mut copy = source.clone();
    copy.id = copy_id.clone();
    copy.display_name = next_copy_name(&source.display_name, &services);
    copy.created_at = now.clone();
    copy.updated_at = now;
    services.insert(copy_id, copy.clone());
    save_all(&services)?;
    Ok(copy)
}

pub(crate) fn remove(id: &str) -> Result<bool, Box<dyn Error>> {
    let id = id.trim();
    let mut services = load_all()?;
    let removed = services.remove(id).is_some();
    if removed {
        save_all(&services)?;
        let _ = crate::store::ai_service_balances::remove(id);
        // 硬迁移后路由表是唯一事实源：删服务必须同时清掉所有能力里的引用，
        // 否则会留下指向不存在服务的悬空路由项。
        let mut selection = load_selection()?;
        let referenced = selection
            .capability_routes
            .values()
            .any(|entries| entries.iter().any(|entry| entry.service_id == id));
        if referenced {
            for entries in selection.capability_routes.values_mut() {
                entries.retain(|entry| entry.service_id != id);
            }
            // 路由表是有序偏好列表：删掉一项后其余项保持相对顺序，
            // 原第一备用自然接替主选。
            selection
                .capability_routes
                .retain(|_, entries| !entries.is_empty());
            write_selection(&selection)?;
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
        // 视觉理解走服务自身的多模态对话协议，没有媒体适配器：
        // 给它一个「图像编辑」之类的默认值会写出一个永远用不到的假绑定。
        CAP_VISION => "",
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

/// `service.model` 是否其实是一个媒体模型（旧数据把生图模型写进了全局 model），
/// 且没有其它真正的对话模型。这类服务不应当被当作对话候选。
fn chat_model_is_media_only(service: &CustomAIService) -> bool {
    let primary = service.model.trim();
    if primary.is_empty() {
        return false;
    }
    let media_models = service
        .media
        .values()
        .map(|binding| binding.model.trim())
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();
    if !media_models.contains(&primary) {
        return false;
    }
    !service.models.iter().any(|model| {
        let model = model.trim();
        !model.is_empty() && model != primary && !media_models.contains(&model)
    })
}

/// 某服务在某能力下的模型目录：默认模型在前，其余按声明顺序去重。
///
/// 空列表意味着该服务在这个能力上没有可用模型。
pub(crate) fn capability_models(service: &CustomAIService, capability: &str) -> Vec<String> {
    let capability = capability.trim();
    let mut models = Vec::new();
    let mut push = |model: &str| {
        let model = model.trim();
        if !model.is_empty() && !models.iter().any(|item: &String| item == model) {
            models.push(model.to_string());
        }
    };
    if capability == CAP_CHAT {
        // 旧数据曾把媒体模型写进全局 `model`，且没有任何真正的对话模型。
        // 这种服务不按对话候选处理，否则会把打标模型当成聊天模型发出去。
        if chat_model_is_media_only(service) {
            return models;
        }
        push(&service.model);
        for model in &service.models {
            push(model);
        }
        return models;
    }
    if let Some(binding) = service.media.get(capability) {
        push(&binding.model);
        for model in &binding.models {
            push(model);
        }
        return models;
    }
    // `vision.describe` 没有显式媒体绑定时跟随对话模型（ADR 0023 第七条）。
    if capability == CAP_VISION {
        if chat_model_is_media_only(service) {
            return models;
        }
        push(&service.model);
        for model in &service.models {
            push(model);
        }
        return models;
    }
    // `image.generate` 的旧投影。
    if capability == CAP_IMAGE_GENERATE {
        push(&service.image_model);
    }
    models
}

/// 某能力某路由项真正会用的模型，以及这个模型是显式指定还是跟随服务默认。
///
/// 空 `model` 跟随服务默认模型；服务在该能力上一个模型都没有时返回 `None`
/// （ADR 0023）。显式 `model` 即使不在目录里也原样返回，由调用方按目录判定
/// 「已不可用」，避免悄悄换成默认模型。
pub(crate) fn route_entry_model(
    service: &CustomAIService,
    capability: &str,
    entry: &CapabilityRouteEntry,
) -> (Option<String>, &'static str) {
    if !entry.follows_service_default() {
        return (Some(entry.model.trim().to_string()), "explicit");
    }
    (
        capability_models(service, capability).into_iter().next(),
        "service_default",
    )
}

/// 校验并规范化一张能力路由表：服务必须存在、必须支持该能力、
/// 同一 (service, model) 不能重复。
fn normalize_capability_routes(
    capability: &str,
    entries: Vec<CapabilityRouteEntry>,
) -> Result<Vec<CapabilityRouteEntry>, Box<dyn Error>> {
    let services = load_all()?;
    let mut seen = std::collections::BTreeSet::new();
    let mut normalized = Vec::new();
    for entry in entries {
        let service_id = entry.service_id.trim().to_string();
        if service_id.is_empty() {
            continue;
        }
        let service = services
            .get(&service_id)
            .ok_or_else(|| format!("自定义 AI 服务不存在：{service_id}"))?;
        if capability_models(service, capability).is_empty() {
            return Err(format!(
                "服务「{}」在能力 {capability} 上没有配置模型",
                service.display_name
            )
            .into());
        }
        let model = entry.model.trim().to_string();
        if !model.is_empty() && !capability_models(service, capability).contains(&model) {
            return Err(format!(
                "服务「{}」没有模型 {model}（能力 {capability}）",
                service.display_name
            )
            .into());
        }
        if !seen.insert((service_id.clone(), model.clone())) {
            continue;
        }
        normalized.push(CapabilityRouteEntry { service_id, model });
    }
    Ok(normalized)
}

/// The default lifecycle for a media primitive. Providers may override it on the
/// binding, but callers must not force every media operation into an async job.
pub(crate) fn default_execution_mode(capability: &str) -> MediaExecutionMode {
    match capability.trim() {
        CAP_VIDEO_GENERATE => MediaExecutionMode::Async,
        _ => MediaExecutionMode::Sync,
    }
}

/// 解析某服务在某媒体能力上的绑定，`model_override` 非空时用它替换默认模型。
///
/// 适配器与执行模式始终来自能力槽位；模型可以被路由项的显式选择覆盖。
pub(crate) fn binding_details_for_model(
    service: &CustomAIService,
    capability: &str,
    model_override: Option<&str>,
) -> Option<ResolvedMediaBinding> {
    let capability = capability.trim();
    if !SUPPORTED_MEDIA_CAPABILITIES.contains(&capability) {
        return None;
    }
    let explicit = service.media.get(capability);
    let (adapter, default_model, execution_mode) = if let Some(binding) = explicit {
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
    let model = model_override
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .unwrap_or(default_model);
    Some(ResolvedMediaBinding {
        adapter,
        model,
        execution_mode,
    })
}

/// Resolve a capability binding without exposing provider credentials.
pub(crate) fn binding_details_for(
    service: &CustomAIService,
    capability: &str,
) -> Option<ResolvedMediaBinding> {
    binding_details_for_model(service, capability, None)
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

/// 一个路由项解析后的执行计划。
struct PlannedAttempt<'a> {
    service: &'a CustomAIService,
    /// 真正会用的模型。显式模型失效时为 `None`，该尝试不进入执行列表。
    model: Option<String>,
    /// 路由项声明的模型名（显式选择时），用于解释失效项。
    requested_model: Option<String>,
    model_source: &'static str,
    adapter: String,
    execution_mode: String,
    role: &'static str,
}

/// Explain how a capability resolves without decrypting any credential.
///
/// This is the read model used by the settings UI and by the capability bus.
/// It deliberately performs no provider I/O: eligibility here means "saved,
/// bound, and has a credential", not "the provider is healthy right now".
///
/// 候选的原子是 (服务, 模型)（ADR 0023）：候选集合按模型目录展开，
/// `attempts` 按路由表顺序给出真正会依次执行的项。
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
    let routes = capability_routes(capability)?;
    let supports = |service: &CustomAIService| !capability_models(service, capability).is_empty();
    let viable_count = services.iter().filter(|service| supports(service)).count();
    let requested = requested_service
        .trim()
        .strip_prefix("custom:")
        .unwrap_or(requested_service.trim());

    // 主选在前，其后是路由表里的备用顺序。
    let mut ordered: Vec<CapabilityRouteEntry> = Vec::new();
    let mut reason = "";
    if !requested.is_empty() {
        let entry = routes
            .iter()
            .find(|entry| entry.service_id == requested)
            .cloned()
            .unwrap_or_else(|| CapabilityRouteEntry::new(requested, ""));
        if !services
            .iter()
            .any(|service| service.id == entry.service_id && supports(service))
        {
            return Err(format!(
                "服务 {requested} 不存在或未配置能力 {capability}；请先在 AI 服务中绑定该能力"
            )
            .into());
        }
        ordered.push(entry);
        ordered.extend(
            routes
                .iter()
                .filter(|entry| entry.service_id != requested)
                .cloned(),
        );
        reason = "explicit_request";
    } else if !routes.is_empty() {
        ordered = routes.clone();
        reason = if capability == CAP_CHAT {
            "active_service"
        } else {
            "configured_default"
        };
    } else {
        // 没有显式路由时，唯一候选可以直接用。
        let viable = services
            .iter()
            .filter(|service| supports(service))
            .collect::<Vec<_>>();
        if let [only] = viable.as_slice() {
            ordered.push(CapabilityRouteEntry::new(&only.id, ""));
            reason = "only_candidate";
        }
    }

    let planned = ordered
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            plan_attempt(
                &services,
                capability,
                entry,
                if index == 0 { "primary" } else { "fallback" },
            )
        })
        .collect::<Vec<_>>();

    let credential_ready = |service: &CustomAIService| !service.encrypted_api_key.trim().is_empty();
    // `attempts` 只保留能真正执行的项：模型可用且有凭据。
    let attempts = planned
        .iter()
        .filter(|attempt| attempt.model.is_some() && credential_ready(attempt.service))
        .map(|attempt| {
            serde_json::json!({
                "service_id": attempt.service.id,
                "display_name": attempt.service.display_name,
                "model": attempt.model,
                "model_source": attempt.model_source,
                "adapter": attempt.adapter,
                "execution_mode": attempt.execution_mode,
                "role": attempt.role,
            })
        })
        .collect::<Vec<_>>();

    // 候选按 (服务, 模型) 展开：路由项在前，其余可用模型补在后面供 UI 选择。
    let mut candidates = planned
        .iter()
        .map(|attempt| {
            let eligible = attempt.model.is_some();
            let credential = credential_ready(attempt.service);
            // 显式模型失效时仍然显示用户当初选的那个名字，而不是空值。
            let displayed_model = attempt
                .model
                .clone()
                .or_else(|| attempt.requested_model.clone());
            candidate_json(
                attempt.service,
                displayed_model,
                attempt.model_source,
                attempt.adapter.clone(),
                attempt.execution_mode.clone(),
                attempt.role,
                if !eligible {
                    "model_unavailable"
                } else if !credential {
                    "missing_credential"
                } else if attempt.role == "primary" {
                    reason
                } else {
                    "configured_fallback"
                },
                eligible && credential,
            )
        })
        .collect::<Vec<_>>();
    for service in services.iter().filter(|service| supports(service)) {
        let (adapter, execution_mode) = binding_shape(service, capability);
        for model in capability_models(service, capability) {
            // 路由表里已经按 (服务, 模型) 展开过的项不重复。
            if planned.iter().any(|attempt| {
                attempt.service.id == service.id && attempt.model.as_deref() == Some(model.as_str())
            }) {
                continue;
            }
            let credential = credential_ready(service);
            candidates.push(candidate_json(
                service,
                Some(model),
                "catalog",
                adapter.clone(),
                execution_mode.clone(),
                "candidate",
                if credential {
                    "not_selected"
                } else {
                    "missing_credential"
                },
                credential,
            ));
        }
    }

    let primary_model_ready = planned
        .first()
        .is_some_and(|attempt| attempt.model.is_some());
    let state = if primary_model_ready {
        "resolved"
    } else if candidates.is_empty() {
        "missing"
    } else if planned.is_empty() {
        // 有多个候选但没有配置默认服务。
        "ambiguous"
    } else {
        // 配了默认服务，但它在该能力上的模型已不可用。
        "missing"
    };
    let plan_reason = if primary_model_ready {
        reason
    } else if candidates.is_empty() {
        "no_candidate"
    } else if planned.is_empty() {
        "multiple_candidates_without_default"
    } else {
        "model_unavailable"
    };
    let selected = planned.first().map(|attempt| {
        serde_json::json!({
            "service_id": attempt.service.id,
            "display_name": attempt.service.display_name,
            "adapter": attempt.adapter,
            "model": attempt.model,
            "model_source": attempt.model_source,
            "execution_mode": attempt.execution_mode,
        })
    });
    let fallbacks = planned
        .iter()
        .skip(1)
        .map(|attempt| {
            serde_json::json!({
                "service_id": attempt.service.id,
                "display_name": attempt.service.display_name,
                "model": attempt.model,
                "model_source": attempt.model_source,
            })
        })
        .collect::<Vec<_>>();

    let _ = viable_count;
    Ok(serde_json::json!({
        "schema_version": "himind.ai-capability-route.v2",
        "capability": capability,
        "state": state,
        "reason": plan_reason,
        "selected": selected,
        "fallbacks": fallbacks,
        "attempts": attempts,
        "candidates": candidates,
        "requires_cost_confirmation": capability != CAP_CHAT,
        "fallback_policy": "same_capability_only",
    }))
}

/// 一条候选的展示结构。资格判定只依据本机状态：模型在目录里、凭据已保存。
#[allow(clippy::too_many_arguments)]
fn candidate_json(
    service: &CustomAIService,
    model: Option<String>,
    model_source: &str,
    adapter: String,
    execution_mode: String,
    eligibility: &str,
    reason: &str,
    eligible: bool,
) -> serde_json::Value {
    serde_json::json!({
        "service_id": service.id,
        "display_name": service.display_name,
        "model": model,
        "model_source": model_source,
        "adapter": adapter,
        "execution_mode": execution_mode,
        "eligible": eligible,
        "eligibility": eligibility,
        "reason": reason,
        "credential_present": !service.encrypted_api_key.trim().is_empty(),
    })
}

/// 把一个路由项解析成执行计划。`model` 为空时跟随服务默认绑定；
/// 显式模型不在目录里时置 `None`，由调用方标记 `model_unavailable`。
fn plan_attempt<'a>(
    services: &'a [CustomAIService],
    capability: &str,
    entry: &CapabilityRouteEntry,
    role: &'static str,
) -> Option<PlannedAttempt<'a>> {
    let service = services
        .iter()
        .find(|service| service.id == entry.service_id.trim())?;
    let (resolved, model_source) = route_entry_model(service, capability, entry);
    let catalog = capability_models(service, capability);
    let (model, requested_model) = if model_source == "explicit" {
        // 显式模型失效时**不回落到服务默认**：用户以为在用 A 模型却实际用 B
        // 是不可观测的错误（ADR 0023 第六条）。
        let requested = resolved.unwrap_or_default();
        let usable = Some(requested.clone()).filter(|model| catalog.contains(model));
        (usable, Some(requested))
    } else {
        (catalog.first().cloned(), None)
    };
    let (adapter, execution_mode) = binding_shape(service, capability);
    Some(PlannedAttempt {
        service,
        model,
        requested_model,
        model_source,
        adapter,
        execution_mode,
        role,
    })
}

/// 某个服务在某能力上的「适配器 + 执行模式」展示值。
fn binding_shape(service: &CustomAIService, capability: &str) -> (String, String) {
    if capability == CAP_CHAT || capability == CAP_VISION {
        return (service.protocol.as_str().to_string(), "sync".to_string());
    }
    match binding_details_for(service, capability) {
        Some(binding) => {
            let mode = match binding.execution_mode {
                MediaExecutionMode::Sync => "sync",
                MediaExecutionMode::Stream => "stream",
                MediaExecutionMode::Async => "async",
            };
            (binding.adapter, mode.to_string())
        }
        None => (String::new(), "sync".to_string()),
    }
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

/// 旧版 `ai-service-selection.json` 的形状。只用于读取迁移，不再写回。
///
/// ADR 0023 选择硬迁移：三个平铺字段已经删除，但读取时必须认得出旧文件，
/// 否则已有用户的路由配置会在升级瞬间全部丢失。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LegacyAIServiceSelection {
    active_service_id: String,
    image_service_id: String,
    media_services: BTreeMap<String, String>,
    capability_fallbacks: BTreeMap<String, Vec<String>>,
}

impl LegacyAIServiceSelection {
    fn media_service_id(&self, capability: &str) -> Option<String> {
        if let Some(id) = self.media_services.get(capability) {
            let id = id.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
        if capability == CAP_IMAGE_GENERATE {
            let id = self.image_service_id.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
        None
    }

    /// 把三个平铺字段合成一张路由表。主选在前，备用在后，模型一律留空
    /// （跟随服务默认绑定），保证升级前后行为逐字一致。
    fn into_routes(self) -> CapabilityRouteTable {
        let mut routes = CapabilityRouteTable::new();
        let mut push = |capability: &str, primary: Option<String>, fallbacks: Vec<String>| {
            let mut entries = Vec::new();
            if let Some(service_id) = primary.filter(|id| !id.trim().is_empty()) {
                entries.push(CapabilityRouteEntry::new(service_id, ""));
            }
            for id in fallbacks {
                let id = id.trim();
                if id.is_empty() || entries.iter().any(|entry| entry.service_id == id) {
                    continue;
                }
                entries.push(CapabilityRouteEntry::new(id, ""));
            }
            if !entries.is_empty() {
                routes.insert(capability.to_string(), entries);
            }
        };
        for capability in SUPPORTED_AI_CAPABILITIES {
            let primary = if *capability == CAP_CHAT {
                Some(self.active_service_id.clone())
            } else {
                self.media_service_id(capability)
            };
            let fallbacks = self
                .capability_fallbacks
                .get(*capability)
                .cloned()
                .unwrap_or_default();
            push(capability, primary, fallbacks);
        }
        routes
    }
}

/// 当前路由表版本。1 是旧版平铺字段，2 起为「服务 + 模型」。
const SELECTION_SCHEMA_VERSION: u32 = 2;

fn load_selection() -> Result<PersistedAIServiceSelection, Box<dyn Error>> {
    let path = selection_path()?;
    if !path.is_file() {
        return Ok(PersistedAIServiceSelection {
            schema_version: SELECTION_SCHEMA_VERSION,
            capability_routes: CapabilityRouteTable::new(),
        });
    }
    let raw = fs::read(&path)?;
    let value: serde_json::Value = serde_json::from_slice(&raw)?;
    // 按「有没有 `capability_routes` 键」区分格式，而不是靠新结构解析失败。
    // `PersistedAIServiceSelection` 带 `deny_unknown_fields`，旧文件直接按新结构
    // 解析会先报「未知字段」中断，走不到迁移分支。
    let mut selection = if value.get("capability_routes").is_some() {
        serde_json::from_value(value)?
    } else {
        // 旧文件：三个平铺字段一次性转换成路由表，模型一律留空（跟随服务默认）。
        let legacy: LegacyAIServiceSelection = serde_json::from_value(value)?;
        PersistedAIServiceSelection {
            schema_version: SELECTION_SCHEMA_VERSION,
            capability_routes: legacy.into_routes(),
        }
    };
    selection.schema_version = SELECTION_SCHEMA_VERSION;
    Ok(selection)
}

fn write_selection(selection: &PersistedAIServiceSelection) -> Result<(), Box<dyn Error>> {
    let mut selection = PersistedAIServiceSelection {
        schema_version: SELECTION_SCHEMA_VERSION,
        capability_routes: selection.capability_routes.clone(),
    };
    // 空路由不留空数组，保持文件可读。
    selection
        .capability_routes
        .retain(|_, entries| !entries.is_empty());
    crate::store::atomic_file::atomic_write(
        &selection_path()?,
        &serde_json::to_vec_pretty(&selection)?,
    )?;
    Ok(())
}

/// 某能力的路由表。返回的 `Vec` 可能为空，表示该能力没有显式主选。
pub(crate) fn capability_routes(
    capability: &str,
) -> Result<Vec<CapabilityRouteEntry>, Box<dyn Error>> {
    Ok(load_selection()?
        .capability_routes
        .get(capability.trim())
        .cloned()
        .unwrap_or_default())
}

/// 覆盖写入某能力的完整路由表。空表等价于「清除该能力的主选与备用」。
pub(crate) fn set_capability_routes(
    capability: &str,
    entries: Vec<CapabilityRouteEntry>,
) -> Result<Vec<CapabilityRouteEntry>, Box<dyn Error>> {
    let capability = capability.trim();
    if !SUPPORTED_AI_CAPABILITIES.contains(&capability) {
        return Err(format!("不支持的能力：{capability}").into());
    }
    let normalized = normalize_capability_routes(capability, entries)?;
    let mut selection = load_selection()?;
    if normalized.is_empty() {
        selection.capability_routes.remove(capability);
    } else {
        selection
            .capability_routes
            .insert(capability.to_string(), normalized.clone());
    }
    write_selection(&selection)?;
    Ok(normalized)
}

pub(crate) fn active_id() -> Result<Option<String>, Box<dyn Error>> {
    Ok(capability_routes(CAP_CHAT)?
        .first()
        .map(|entry| entry.service_id.trim().to_string())
        .filter(|id| !id.is_empty()))
}

/// 某个媒体能力的默认服务；留空表示按「激活服务 → 唯一支持该能力的服务」自动解析。
pub(crate) fn media_service_id(capability: &str) -> Result<Option<String>, Box<dyn Error>> {
    Ok(capability_routes(capability)?
        .first()
        .map(|entry| entry.service_id.trim().to_string())
        .filter(|id| !id.is_empty()))
}

/// 只供内部测试使用的媒体主选写入辅助。生产代码走 [`set_capability_routes`]
/// （ADR 0023），理由同 [`set_active`]：这里表达不了模型。
#[cfg(test)]
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
    let value = selected.as_ref().map(|service| service.id.clone());
    set_primary_route(&mut selection, capability, value.as_deref());
    write_selection(&selection)?;
    Ok(selected)
}

/// 把某能力的主选换成 `service_id`，`None` 表示清除该能力的整条路由。
///
/// 换服务后主选的模型重置为空（跟随服务默认绑定），因为换服务后旧模型通常不再
/// 适用（ADR 0023 第九条）。原有备用项保持顺序，其中与新主选相同的服务被合并。
/// 清除主选时整条路由一并清除：没有主选的备用顺序没有意义。
///
/// 只有 [`set_active`] / [`set_media_service`] 这两个测试辅助会调用它；生产路径走
/// [`set_capability_routes`]。跟着一起标 `#[cfg(test)]`，非测试构建里不留死代码。
#[cfg(test)]
fn set_primary_route(
    selection: &mut PersistedAIServiceSelection,
    capability: &str,
    service_id: Option<&str>,
) {
    let Some(service_id) = service_id.map(str::trim).filter(|id| !id.is_empty()) else {
        selection.capability_routes.remove(capability);
        return;
    };
    let mut entries = vec![CapabilityRouteEntry::new(service_id, "")];
    if let Some(existing) = selection.capability_routes.get(capability) {
        entries.extend(
            existing
                .iter()
                .skip(1)
                .filter(|entry| entry.service_id != service_id)
                .cloned(),
        );
    }
    selection
        .capability_routes
        .insert(capability.to_string(), entries);
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

/// 只供内部测试使用的对话主选写入辅助。
///
/// 生产代码的唯一写入口是 [`set_capability_routes`]（ADR 0023）：这里不能表达
/// 模型，也无法带上 DSH 同步副作用，一旦被生产代码调用就会绕开那条路径。
/// 因此它在非测试构建里根本不存在。
#[cfg(test)]
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
    let value = selected.as_ref().map(|service| service.id.clone());
    set_primary_route(&mut selection, CAP_CHAT, value.as_deref());
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

/// 火山方舟 Agent Plan 是套餐推理入口，只提供 Messages 推理，不提供 `/models`
/// 发现接口。用 `x-api-key` 访问不存在的模型列表会先被鉴权层拒绝成 401，极易被
/// 误判为 Key 无效；因此这里单独识别，连接检查改走最小 Messages 探测。
pub(crate) fn is_volcengine_agent_plan(base_url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(base_url.trim()) else {
        return false;
    };
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let path = parsed.path().trim_end_matches('/');
    let path = path.strip_suffix("/v1").unwrap_or(path);
    host.ends_with(".volces.com") && path == "/api/plan"
}

/// Claude Desktop 启动时必须先拿到 `/v1/models`。目录型套餐若没有该端点，
/// 直连会表现为模型选择器为空，因此注册器应自动改走本机聚合网关。
pub(crate) fn supports_model_discovery(base_url: &str) -> bool {
    !is_volcengine_agent_plan(base_url)
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
    if is_volcengine_agent_plan(base_url) {
        return Err(
            "火山方舟 Agent Plan 不提供 /models 模型列表接口；请使用目录预设模型，连接检查会改为发起 1 Token 的 Messages 探测"
                .into(),
        );
    }
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

/// 对不支持 `/models` 的协议入口做最小 Messages 推理探测。
///
/// 只发送 `ping` 且限制 `max_tokens: 1`，用于验证端点、凭据和默认模型是否可用。
/// 不在错误里回显 Key。OpenAI 协议也保留实现，便于未来同类套餐复用。
pub(crate) fn probe_chat_connection(
    base_url: &str,
    api_key: &str,
    protocol: AIServiceProtocol,
    model: &str,
) -> Result<(), Box<dyn Error>> {
    validate_base_url(base_url)?;
    let model = model.trim();
    if model.is_empty() {
        return Err("连接探测需要先配置默认模型".into());
    }
    let mut client_builder =
        reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(30));
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
            .post(format!("{}/v1/messages", anthropic_api_root(base_url)))
            .header("x-api-key", api_key.trim())
            .header("anthropic-version", ANTHROPIC_API_VERSION)
            .json(&serde_json::json!({
                "model": model,
                "max_tokens": 1,
                "messages": [{ "role": "user", "content": "ping" }],
            })),
        AIServiceProtocol::OpenaiChat => client
            .post(format!(
                "{}/chat/completions",
                base_url.trim().trim_end_matches('/')
            ))
            .bearer_auth(api_key.trim())
            .json(&serde_json::json!({
                "model": model,
                "max_tokens": 1,
                "messages": [{ "role": "user", "content": "ping" }],
            })),
        AIServiceProtocol::OpenaiResponses => client
            .post(format!(
                "{}/responses",
                base_url.trim().trim_end_matches('/')
            ))
            .bearer_auth(api_key.trim())
            .json(&serde_json::json!({
                "model": model,
                "max_output_tokens": 1,
                "input": "ping",
            })),
    };
    let response = request
        .send()
        .map_err(|error| format!("连接探测失败：{error}"))?;
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let body = response
        .text()
        .unwrap_or_default()
        .chars()
        .take(1200)
        .collect::<String>();
    Err(format!("连接探测接口返回 {status}：{body}").into())
}

/// 读取已保存自定义服务并拉取其模型列表。
///
/// 常规供应商走 `GET /models`。火山方舟 Agent Plan 没有该接口，因此改为一次
/// 最小推理探测；探测成功后返回目录或用户保存的模型，而不是伪造一个列表响应。
pub(crate) fn list_models(id: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let (service, api_key) = load_secret(id)?;
    if is_volcengine_agent_plan(&service.base_url) {
        probe_chat_connection(
            &service.base_url,
            &api_key,
            service.protocol.clone(),
            &service.model,
        )?;
        if service.models.is_empty() {
            return Ok(vec![service.model.trim().to_string()]);
        }
        return Ok(service.models.clone());
    }
    fetch_models(&service.base_url, &api_key, service.protocol)
}

#[cfg(test)]
mod tests {
    use super::{
        binding_details_for, binding_for, default_execution_mode, validate_base_url,
        AIServiceProtocol, CapabilityRouteEntry, CustomAIServiceInput, MediaExecutionMode,
        MediaModelBinding, CAP_AUDIO_SPEECH, CAP_AUDIO_TRANSCRIBE, CAP_CHAT, CAP_IMAGE_EDIT,
        CAP_IMAGE_GENERATE, CAP_VIDEO_GENERATE,
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
    fn duplicate_service_keeps_credentials_and_allocates_unique_identity() {
        with_isolated_home(|| {
            let saved = super::upsert(CustomAIServiceInput {
                id: "clone-source".to_string(),
                display_name: "克隆源".to_string(),
                base_url: "https://ark.cn-beijing.volces.com/api/plan".to_string(),
                protocol: AIServiceProtocol::Anthropic,
                model: "doubao-seed-evolving".to_string(),
                models: vec!["doubao-seed-evolving".to_string(), "glm-5.3".to_string()],
                image_model: String::new(),
                image_adapter: String::new(),
                media: Default::default(),
                api_key: "ark-test-secret".to_string(),
            })
            .expect("save source service");

            let first = super::duplicate(&saved.id).expect("duplicate service");
            assert_eq!(first.id, "clone-source-copy");
            assert_eq!(first.display_name, "克隆源 副本");
            assert_eq!(first.base_url, saved.base_url);
            assert_eq!(first.models, saved.models);
            let (_, first_key) = super::load_secret(&first.id).expect("copy keeps key");
            assert_eq!(first_key, "ark-test-secret");

            let second = super::duplicate(&saved.id).expect("duplicate again");
            assert_eq!(second.id, "clone-source-copy-2");
            assert_eq!(second.display_name, "克隆源 副本 2");
            assert_eq!(super::list().expect("list services").len(), 3);
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
    fn removing_a_service_prunes_it_from_every_capability_route() {
        with_isolated_home(|| {
            for id in ["chat", "image", "backup"] {
                super::upsert(CustomAIServiceInput {
                    id: id.to_string(),
                    display_name: id.to_string(),
                    base_url: format!("https://{id}.example/v1"),
                    protocol: AIServiceProtocol::OpenaiChat,
                    model: "chat-model".to_string(),
                    models: vec!["chat-model".to_string()],
                    image_model: "image-model".to_string(),
                    image_adapter: "ark-images".to_string(),
                    media: Default::default(),
                    api_key: format!("sk-{id}"),
                })
                .expect("create service");
            }
            super::set_capability_routes(
                CAP_CHAT,
                vec![
                    CapabilityRouteEntry::new("chat", ""),
                    CapabilityRouteEntry::new("backup", ""),
                ],
            )
            .unwrap();
            super::set_capability_routes(
                CAP_IMAGE_GENERATE,
                vec![
                    CapabilityRouteEntry::new("image", ""),
                    CapabilityRouteEntry::new("backup", ""),
                ],
            )
            .unwrap();

            // 删掉同时被两个能力引用的备用服务：两处引用都要清掉，不留悬空项。
            assert!(super::remove("backup").unwrap());
            assert_eq!(
                super::capability_routes(CAP_CHAT).unwrap(),
                vec![CapabilityRouteEntry::new("chat", "")]
            );
            assert_eq!(
                super::capability_routes(CAP_IMAGE_GENERATE).unwrap(),
                vec![CapabilityRouteEntry::new("image", "")]
            );

            // 删掉主选：该能力整条路由清空，其它能力不受影响。
            assert!(super::remove("image").unwrap());
            assert!(super::capability_routes(CAP_IMAGE_GENERATE)
                .unwrap()
                .is_empty());
            assert_eq!(
                super::capability_routes(CAP_CHAT).unwrap(),
                vec![CapabilityRouteEntry::new("chat", "")]
            );
        });
    }

    #[test]
    fn legacy_selection_file_migrates_to_the_capability_route_table() {
        with_isolated_home(|| {
            for id in ["chat", "image", "backup"] {
                super::upsert(CustomAIServiceInput {
                    id: id.to_string(),
                    display_name: id.to_string(),
                    base_url: format!("https://{id}.example/v1"),
                    protocol: AIServiceProtocol::OpenaiChat,
                    model: "chat-model".to_string(),
                    models: vec!["chat-model".to_string()],
                    image_model: if id == "chat" {
                        String::new()
                    } else {
                        "doubao-seedream".to_string()
                    },
                    image_adapter: if id == "chat" {
                        String::new()
                    } else {
                        "ark-images".to_string()
                    },
                    media: Default::default(),
                    api_key: format!("sk-{id}"),
                })
                .expect("create service");
            }

            // 旧版 `ai-service-selection.json`，三个平铺字段。
            let legacy = serde_json::json!({
                "active_service_id": "chat",
                "image_service_id": "image",
                "media_services": {"image.generate": "image"},
                "capability_fallbacks": {
                    "image.generate": ["backup"],
                    "chat": ["chat"]
                }
            });
            std::fs::write(
                super::selection_path().expect("selection path"),
                serde_json::to_vec_pretty(&legacy).unwrap(),
            )
            .expect("write legacy selection");

            // 读取路径一次性迁移：主选在 `routes[0]`，备用顺序原样保留。
            assert_eq!(super::active_id().unwrap().as_deref(), Some("chat"));
            assert_eq!(
                super::media_service_id(CAP_IMAGE_GENERATE)
                    .unwrap()
                    .as_deref(),
                Some("image")
            );
            assert_eq!(
                super::capability_routes(CAP_IMAGE_GENERATE).unwrap(),
                vec![
                    CapabilityRouteEntry::new("image", ""),
                    CapabilityRouteEntry::new("backup", ""),
                ]
            );
            // 主选出现在旧备用列表里时应被丢弃，不能重复。
            assert_eq!(
                super::capability_routes(CAP_CHAT).unwrap(),
                vec![CapabilityRouteEntry::new("chat", "")]
            );
            // 迁移结果全部是「跟随服务默认模型」。
            let image_routes = super::capability_routes(CAP_IMAGE_GENERATE).unwrap();
            assert_eq!(image_routes.len(), 2);
            assert!(image_routes
                .iter()
                .all(|entry| entry.follows_service_default()));

            // 写入后文件变成新结构，旧字段不再出现。
            super::set_media_service(CAP_IMAGE_GENERATE, "backup").unwrap();
            let raw = std::fs::read(super::selection_path().expect("path")).unwrap();
            let text = String::from_utf8(raw).unwrap();
            assert!(text.contains("capability_routes"));
            assert!(!text.contains("active_service_id"));
            assert!(!text.contains("media_services"));
            assert!(!text.contains("capability_fallbacks"));
            // 换主选时原备用项保留顺序、模型重置为空；旧主选不会自动降级为备用，
            // 因为 `set_media_service` 的语义是「指定默认服务」而不是「插入备用」。
            assert_eq!(
                super::capability_routes(CAP_IMAGE_GENERATE).unwrap(),
                vec![CapabilityRouteEntry::new("backup", "")]
            );
        });
    }

    #[test]
    fn empty_route_model_follows_the_service_default_binding() {
        with_isolated_home(|| {
            super::upsert(CustomAIServiceInput {
                id: "multi".to_string(),
                display_name: "多模型服务".to_string(),
                base_url: "https://multi.example/v1".to_string(),
                protocol: AIServiceProtocol::OpenaiChat,
                model: "chat-a".to_string(),
                models: vec!["chat-a".to_string(), "chat-b".to_string()],
                image_model: "image-a".to_string(),
                image_adapter: "ark-images".to_string(),
                media: Default::default(),
                api_key: "sk-multi".to_string(),
            })
            .expect("create service");

            let service = super::load_all().unwrap().remove("multi").unwrap();
            assert_eq!(
                super::capability_models(&service, CAP_CHAT),
                vec!["chat-a".to_string(), "chat-b".to_string()]
            );
            assert_eq!(
                super::capability_models(&service, CAP_IMAGE_GENERATE),
                vec!["image-a".to_string()]
            );

            let follow = CapabilityRouteEntry::new("multi", "");
            assert_eq!(
                super::route_entry_model(&service, CAP_IMAGE_GENERATE, &follow),
                (Some("image-a".to_string()), "service_default")
            );
            let explicit = CapabilityRouteEntry::new("multi", "chat-b");
            assert_eq!(
                super::route_entry_model(&service, CAP_CHAT, &explicit),
                (Some("chat-b".to_string()), "explicit")
            );
            // 显式模型不在目录里也要原样返回，交给资格判定标记不可用。
            let stale = CapabilityRouteEntry::new("multi", "gone");
            assert_eq!(
                super::route_entry_model(&service, CAP_CHAT, &stale),
                (Some("gone".to_string()), "explicit")
            );
        });
    }

    #[test]
    fn capability_routes_reject_unknown_services_and_dedupe_entries() {
        with_isolated_home(|| {
            for id in ["one", "two"] {
                super::upsert(CustomAIServiceInput {
                    id: id.to_string(),
                    display_name: id.to_string(),
                    base_url: format!("https://{id}.example/v1"),
                    protocol: AIServiceProtocol::OpenaiChat,
                    model: "chat-a".to_string(),
                    models: vec!["chat-a".to_string(), "chat-b".to_string()],
                    image_model: String::new(),
                    image_adapter: String::new(),
                    media: Default::default(),
                    api_key: format!("sk-{id}"),
                })
                .expect("create service");
            }

            let saved = super::set_capability_routes(
                CAP_CHAT,
                vec![
                    CapabilityRouteEntry::new("one", "chat-a"),
                    CapabilityRouteEntry::new("one", "chat-a"),
                    CapabilityRouteEntry::new("two", ""),
                ],
            )
            .expect("save routes");
            assert_eq!(saved.len(), 2);
            assert_eq!(saved[0], CapabilityRouteEntry::new("one", "chat-a"));
            assert_eq!(saved[1], CapabilityRouteEntry::new("two", ""));

            let error = super::set_capability_routes(
                CAP_CHAT,
                vec![CapabilityRouteEntry::new("missing", "")],
            )
            .expect_err("unknown service must be rejected");
            assert!(error.to_string().contains("不存在"));

            let error = super::set_capability_routes(
                CAP_CHAT,
                vec![CapabilityRouteEntry::new("one", "image-only")],
            )
            .expect_err("unknown model must be rejected");
            assert!(error.to_string().contains("没有模型"));

            // 服务不支持该能力时不能写进路由表。
            let error = super::set_capability_routes(
                CAP_IMAGE_GENERATE,
                vec![CapabilityRouteEntry::new("one", "")],
            )
            .expect_err("unsupported capability must be rejected");
            assert!(error.to_string().contains("没有配置模型"));

            // 清空路由表等价于清除该能力的主选。
            super::set_capability_routes(CAP_CHAT, Vec::new()).unwrap();
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
            let (service, _) =
                super::resolve_media_service(CAP_IMAGE_GENERATE, "").expect("auto fallback");
            assert_eq!(service.id, "image-only");
            assert_eq!(service.image_adapter, "ark-images");

            super::set_media_service(CAP_IMAGE_GENERATE, "image-only").unwrap();
            assert_eq!(
                super::media_service_id(CAP_IMAGE_GENERATE)
                    .unwrap()
                    .as_deref(),
                Some("image-only")
            );
            // 切换对话服务不得清掉生图服务。
            super::set_active("chat-only").unwrap();
            assert_eq!(
                super::media_service_id(CAP_IMAGE_GENERATE)
                    .unwrap()
                    .as_deref(),
                Some("image-only")
            );
            let (explicit, _) =
                super::resolve_media_service(CAP_IMAGE_GENERATE, "custom:image-only")
                    .expect("explicit service");
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
                    models: Vec::new(),
                    execution_mode: None,
                },
            );
            media.insert(
                CAP_IMAGE_EDIT.to_string(),
                MediaModelBinding {
                    adapter: "ark-images".to_string(),
                    model: "doubao-seededit-4-0-i2i".to_string(),
                    models: Vec::new(),
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
                                models: Vec::new(),
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
            assert!(super::set_media_service("video.generate", "only-edit").is_ok());
            assert!(super::set_media_service("tts.generate", "only-edit").is_err());
        });
    }

    #[test]
    fn media_capability_keeps_its_extra_models_through_the_legacy_image_projection() {
        with_isolated_home(|| {
            // 表单保存「生图默认模型 + 其它可选模型」：额外模型必须活下来，
            // 因为 `image_model` 旧投影会在 upsert 时重写整个 `image.generate` 槽位。
            let mut media = std::collections::BTreeMap::new();
            media.insert(
                CAP_IMAGE_GENERATE.to_string(),
                MediaModelBinding {
                    adapter: "ark-images".to_string(),
                    model: "doubao-seedream-5-0-pro".to_string(),
                    models: vec!["doubao-seedream-4-0".to_string()],
                    execution_mode: None,
                },
            );
            super::upsert(CustomAIServiceInput {
                id: "multi-image".to_string(),
                display_name: "多生图模型".to_string(),
                base_url: "https://ark.example/api/v3".to_string(),
                protocol: AIServiceProtocol::OpenaiChat,
                model: String::new(),
                models: Vec::new(),
                image_model: "doubao-seedream-5-0-pro".to_string(),
                image_adapter: "ark-images".to_string(),
                media,
                api_key: "sk-image".to_string(),
            })
            .expect("upsert");

            let (saved, _) = super::load_secret("multi-image").expect("reload");
            assert_eq!(
                super::capability_models(&saved, CAP_IMAGE_GENERATE),
                vec![
                    "doubao-seedream-5-0-pro".to_string(),
                    "doubao-seedream-4-0".to_string()
                ]
            );
            // 额外模型可以作为路由项里显式指定的模型被接受。
            super::set_capability_routes(
                CAP_IMAGE_GENERATE,
                vec![CapabilityRouteEntry::new(
                    "multi-image",
                    "doubao-seedream-4-0",
                )],
            )
            .expect("pin the extra model");
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
                    models: Vec::new(),
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
                    models: Vec::new(),
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
                    models: Vec::new(),
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
                    models: Vec::new(),
                    execution_mode: Some(MediaExecutionMode::Stream),
                },
            );
            media.insert(
                CAP_AUDIO_TRANSCRIBE.to_string(),
                MediaModelBinding {
                    adapter: "openai-audio".to_string(),
                    model: "whisper-1".to_string(),
                    models: Vec::new(),
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
    fn identifies_volcengine_agent_plan_as_catalog_only() {
        assert!(super::is_volcengine_agent_plan(
            "https://ark.cn-beijing.volces.com/api/plan"
        ));
        assert!(super::is_volcengine_agent_plan(
            "https://ark.cn-beijing.volces.com/api/plan/"
        ));
        assert!(super::is_volcengine_agent_plan(
            "https://ark.cn-beijing.volces.com/api/plan/v1"
        ));
        assert!(!super::is_volcengine_agent_plan(
            "https://ark.cn-beijing.volces.com/api/v3"
        ));
        assert!(!super::is_volcengine_agent_plan(
            "https://example.com/api/plan"
        ));
    }

    #[test]
    fn agent_plan_probe_uses_minimal_messages_request() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let received = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let captured = received.clone();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 8192];
            let read = stream.read(&mut buffer).unwrap_or(0);
            *captured.lock().unwrap() = String::from_utf8_lossy(&buffer[..read]).to_string();
            let body = r#"{"type":"message","content":[{"type":"thinking","thinking":"p"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        });
        super::probe_chat_connection(
            &format!("http://{addr}/api/plan"),
            "ark-test",
            AIServiceProtocol::Anthropic,
            "deepseek-v4-pro",
        )
        .expect("probe succeeds");
        let request = received.lock().unwrap().clone();
        let normalized = request.to_ascii_lowercase();
        assert!(
            request.starts_with("POST /api/plan/v1/messages "),
            "unexpected request line: {request}"
        );
        assert!(normalized.contains("x-api-key: ark-test"));
        assert!(normalized.contains("anthropic-version: 2023-06-01"));
        assert!(normalized.contains("\"max_tokens\":1"));
        assert!(normalized.contains("\"content\":\"ping\""));
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
