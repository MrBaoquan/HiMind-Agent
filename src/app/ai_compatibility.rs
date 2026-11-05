//! AI 客户端 / Provider 行为兼容矩阵。
//!
//! 协议名只描述线格式，不描述工具结果媒体、工具历史、namespace、web_search、
//! tool_choice、采样参数等行为差异。本模块是这些行为能力的唯一代码事实源；
//! 网关、导入器和 UI 只能查询这里的矩阵，不再各自按 base_url 或客户端 ID 推断。
//!
//! 三态能力值是刻意设计：unknown 永远不能按支持处理。未知 Provider 先采用
//! 最大兼容子集，并由网关记录结构化降级原因。

use crate::store::ai_services::AIServiceProtocol;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub(crate) const AI_COMPATIBILITY_SCHEMA_VERSION: u32 = 1;
pub(crate) const UNKNOWN_PROVIDER_ID: &str = "unknown";
pub(crate) const DEFAULT_GATEWAY_CLIENT_IDS: &[&str] = &[
    "vscode",
    "cc-switch",
    "codex",
    "workbuddy",
    "kimi-code",
    "qwen-code",
    "claude-code",
    "claude-desktop",
    "opencode",
    "continue",
    "aider",
    "crush",
    "qoder",
    "qoder-cn",
    "zcode",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CapabilitySupport {
    Supported,
    Unsupported,
    Unknown,
}

impl CapabilitySupport {
    pub(crate) fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }

    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Supported, Self::Supported) => Self::Supported,
            (Self::Unsupported, _) | (_, Self::Unsupported) => Self::Unsupported,
            _ => Self::Unknown,
        }
    }
}

impl Default for CapabilitySupport {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModelDiscovery {
    Native,
    LocalCatalog,
    None,
    Unknown,
}

impl ModelDiscovery {
    pub(crate) fn is_available(&self) -> bool {
        matches!(self, Self::Native | Self::LocalCatalog)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DirectModePolicy {
    Allow,
    Warn,
    GatewayRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NormalizationAction {
    Native,
    Translate,
    Degrade,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FailureMode {
    StructuredDiagnostic,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ToolResultCapabilities {
    pub text: CapabilitySupport,
    pub image: CapabilitySupport,
    pub file: CapabilitySupport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SamplingCapabilities {
    pub temperature: CapabilitySupport,
    pub top_p: CapabilitySupport,
    pub max_tokens: CapabilitySupport,
    pub max_output_tokens: CapabilitySupport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ClientCapability {
    pub id: String,
    pub display_name: String,
    pub aliases: Vec<String>,
    pub entry_protocols: Vec<AIServiceProtocol>,
    pub requires_model_discovery: bool,
    pub tool_result: ToolResultCapabilities,
    pub function_call_history: CapabilitySupport,
    pub namespace_tools: CapabilitySupport,
    pub web_search_tool: CapabilitySupport,
    pub tool_choice: CapabilitySupport,
    pub user_input_image: CapabilitySupport,
    pub requires_1m_variant: CapabilitySupport,
    pub sampling: SamplingCapabilities,
    pub rich_tool_client: bool,
    pub default_gateway: bool,
    pub direct_policy: DirectModePolicy,
}

impl Default for ClientCapability {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            aliases: Vec::new(),
            entry_protocols: Vec::new(),
            requires_model_discovery: false,
            tool_result: ToolResultCapabilities::default(),
            function_call_history: CapabilitySupport::Unknown,
            namespace_tools: CapabilitySupport::Unknown,
            web_search_tool: CapabilitySupport::Unknown,
            tool_choice: CapabilitySupport::Unknown,
            user_input_image: CapabilitySupport::Unknown,
            requires_1m_variant: CapabilitySupport::Unknown,
            sampling: SamplingCapabilities::default(),
            rich_tool_client: false,
            default_gateway: false,
            direct_policy: DirectModePolicy::Warn,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProviderFingerprint {
    pub host_suffixes: Vec<String>,
    pub path_prefixes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProviderCapability {
    pub id: String,
    pub display_name: String,
    pub protocols: Vec<AIServiceProtocol>,
    pub fingerprint: ProviderFingerprint,
    pub model_discovery: ModelDiscovery,
    pub tool_result: ToolResultCapabilities,
    pub function_call_history: CapabilitySupport,
    pub namespace_tools: CapabilitySupport,
    pub web_search_tool: CapabilitySupport,
    pub tool_choice: CapabilitySupport,
    pub user_input_image: CapabilitySupport,
    pub system_messages: CapabilitySupport,
    pub sampling: SamplingCapabilities,
    pub max_output_tokens: CapabilitySupport,
    pub context_window: CapabilitySupport,
    pub supports_1m: CapabilitySupport,
    pub unknown_policy: FailureMode,
}

impl Default for ProviderCapability {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            protocols: Vec::new(),
            fingerprint: ProviderFingerprint::default(),
            model_discovery: ModelDiscovery::Unknown,
            tool_result: ToolResultCapabilities::default(),
            function_call_history: CapabilitySupport::Unknown,
            namespace_tools: CapabilitySupport::Unknown,
            web_search_tool: CapabilitySupport::Unknown,
            tool_choice: CapabilitySupport::Unknown,
            user_input_image: CapabilitySupport::Unknown,
            system_messages: CapabilitySupport::Unknown,
            sampling: SamplingCapabilities::default(),
            max_output_tokens: CapabilitySupport::Unknown,
            context_window: CapabilitySupport::Unknown,
            supports_1m: CapabilitySupport::Unknown,
            unknown_policy: FailureMode::StructuredDiagnostic,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModelMatch {
    Exact,
    Prefix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ModelCapabilityRule {
    pub provider_id: String,
    pub matcher: ModelMatch,
    pub model: String,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub supports_1m: CapabilitySupport,
    pub user_input_image: CapabilitySupport,
}

impl Default for ModelCapabilityRule {
    fn default() -> Self {
        Self {
            provider_id: String::new(),
            matcher: ModelMatch::Exact,
            model: String::new(),
            context_window: None,
            max_output_tokens: None,
            supports_1m: CapabilitySupport::Unknown,
            user_input_image: CapabilitySupport::Unknown,
        }
    }
}

impl ModelCapabilityRule {
    pub(crate) fn matches(&self, model: &str) -> bool {
        let model = model_base_id(model);
        match self.matcher {
            ModelMatch::Exact => model == self.model,
            ModelMatch::Prefix => model.starts_with(&self.model),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct UnknownCapabilityPolicy {
    pub tool_result_image: NormalizationAction,
    pub tool_result_file: NormalizationAction,
    pub tool_history: NormalizationAction,
    pub namespace_tools: NormalizationAction,
    pub web_search_tool: NormalizationAction,
    pub tool_choice: NormalizationAction,
    pub user_input_image: NormalizationAction,
    pub optional_sampling: NormalizationAction,
    pub preserve_user_intent: bool,
    pub failure_mode: FailureMode,
}

impl Default for UnknownCapabilityPolicy {
    fn default() -> Self {
        Self {
            tool_result_image: NormalizationAction::Degrade,
            tool_result_file: NormalizationAction::Degrade,
            tool_history: NormalizationAction::Degrade,
            namespace_tools: NormalizationAction::Degrade,
            web_search_tool: NormalizationAction::Degrade,
            tool_choice: NormalizationAction::Degrade,
            user_input_image: NormalizationAction::Native,
            optional_sampling: NormalizationAction::Degrade,
            preserve_user_intent: true,
            failure_mode: FailureMode::StructuredDiagnostic,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CapabilityMatrixV1 {
    pub schema_version: u32,
    pub updated_at: String,
    pub clients: Vec<ClientCapability>,
    pub providers: Vec<ProviderCapability>,
    pub model_rules: Vec<ModelCapabilityRule>,
    pub unknown_policy: UnknownCapabilityPolicy,
}

impl Default for CapabilityMatrixV1 {
    fn default() -> Self {
        Self {
            schema_version: AI_COMPATIBILITY_SCHEMA_VERSION,
            updated_at: String::new(),
            clients: Vec::new(),
            providers: Vec::new(),
            model_rules: Vec::new(),
            unknown_policy: UnknownCapabilityPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CompatibilityDiagnosticV1 {
    pub schema_version: u32,
    pub client: String,
    pub provider: String,
    pub client_protocol: String,
    pub provider_protocol: String,
    pub unsupported_feature: String,
    pub upstream_status: u16,
    pub request_id: Option<String>,
    pub attempted_normalizations: Vec<NormalizationAction>,
    pub suggested_action: String,
    pub safe_message: String,
}

impl Default for CompatibilityDiagnosticV1 {
    fn default() -> Self {
        Self {
            schema_version: AI_COMPATIBILITY_SCHEMA_VERSION,
            client: String::new(),
            provider: UNKNOWN_PROVIDER_ID.to_string(),
            client_protocol: String::new(),
            provider_protocol: String::new(),
            unsupported_feature: "unknown".to_string(),
            upstream_status: 0,
            request_id: None,
            attempted_normalizations: Vec::new(),
            suggested_action: "检查客户端、Provider 与模型组合是否在兼容矩阵中".to_string(),
            safe_message: "请求与 Provider 能力不兼容".to_string(),
        }
    }
}

impl CapabilityMatrixV1 {
    pub(crate) fn builtin() -> Self {
        Self {
            schema_version: AI_COMPATIBILITY_SCHEMA_VERSION,
            updated_at: "2026-11-04".to_string(),
            clients: builtin_clients(),
            providers: builtin_providers(),
            model_rules: builtin_model_rules(),
            unknown_policy: UnknownCapabilityPolicy::default(),
        }
    }

    pub(crate) fn client(&self, id: &str) -> Option<&ClientCapability> {
        let id = id.trim();
        self.clients
            .iter()
            .find(|client| client.id == id || client.aliases.iter().any(|alias| alias == id))
    }

    pub(crate) fn provider(&self, id: &str) -> Option<&ProviderCapability> {
        let id = id.trim();
        self.providers.iter().find(|provider| provider.id == id)
    }

    /// 按协议和 URL 指纹解析 Provider。没有精确命中时返回安全默认 Provider，
    /// 而不是把最先注册的 Provider 当成通用实现。
    pub(crate) fn resolve_provider(
        &self,
        base_url: &str,
        protocol: &AIServiceProtocol,
    ) -> &ProviderCapability {
        let Ok(parsed) = url::Url::parse(base_url.trim()) else {
            return self
                .provider(UNKNOWN_PROVIDER_ID)
                .expect("builtin matrix must contain unknown provider");
        };
        let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
        let path = parsed.path().trim_end_matches('/');
        let best = self
            .providers
            .iter()
            .filter(|provider| provider.id != UNKNOWN_PROVIDER_ID)
            .filter(|provider| provider.protocols.contains(protocol))
            .filter_map(|provider| {
                provider_match_score(provider, &host, path).map(|score| (score, provider))
            })
            .max_by_key(|(score, _)| *score)
            .map(|(_, provider)| provider);
        best.unwrap_or_else(|| {
            self.provider(protocol_fallback_provider_id(protocol))
                .or_else(|| self.provider(UNKNOWN_PROVIDER_ID))
                .expect("builtin matrix must contain protocol fallback providers")
        })
    }

    pub(crate) fn model_rule(
        &self,
        provider_id: &str,
        model: &str,
    ) -> Option<&ModelCapabilityRule> {
        self.model_rules
            .iter()
            .filter(|rule| rule.provider_id == provider_id)
            .find(|rule| rule.matches(model))
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != AI_COMPATIBILITY_SCHEMA_VERSION {
            return Err(format!(
                "能力矩阵版本必须为 {AI_COMPATIBILITY_SCHEMA_VERSION}，实际为 {}",
                self.schema_version
            ));
        }
        if self.updated_at.trim().is_empty() {
            return Err("能力矩阵缺少 updated_at".to_string());
        }
        if self.clients.is_empty() || self.providers.is_empty() {
            return Err("能力矩阵必须同时包含客户端和 Provider".into());
        }
        let mut client_ids = BTreeSet::new();
        for client in &self.clients {
            if client.id.trim().is_empty() || !client_ids.insert(client.id.as_str()) {
                return Err("客户端 ID 为空或重复".into());
            }
            if client.entry_protocols.is_empty() {
                return Err(format!("客户端 {} 缺少入口协议", client.id));
            }
            for alias in &client.aliases {
                if alias == &client.id || !client_ids.insert(alias.as_str()) {
                    return Err(format!("客户端 {} 的别名冲突：{alias}", client.id));
                }
            }
        }
        let mut provider_ids = BTreeSet::new();
        for provider in &self.providers {
            if provider.id.trim().is_empty() || !provider_ids.insert(provider.id.as_str()) {
                return Err("Provider ID 为空或重复".into());
            }
            if provider.protocols.is_empty() {
                return Err(format!("Provider {} 缺少协议", provider.id));
            }
        }
        if !provider_ids.contains(UNKNOWN_PROVIDER_ID) {
            return Err("能力矩阵缺少 unknown Provider 安全默认".into());
        }
        for rule in &self.model_rules {
            if !provider_ids.contains(rule.provider_id.as_str()) {
                return Err(format!("模型规则引用未知 Provider：{}", rule.provider_id));
            }
            if rule.model.trim().is_empty() {
                return Err("模型规则缺少模型 ID".into());
            }
            if rule.supports_1m.is_supported()
                && rule
                    .context_window
                    .is_some_and(|context| context < 1_000_000)
            {
                return Err(format!("模型 {} 标记支持 1M 但上下文小于 1M", rule.model));
            }
        }
        Ok(())
    }

    pub(crate) fn feature_support(
        &self,
        client_id: &str,
        provider_id: &str,
        feature: CompatibilityFeature,
    ) -> CapabilitySupport {
        let Some(client) = self.client(client_id) else {
            return CapabilitySupport::Unknown;
        };
        let Some(provider) = self.provider(provider_id) else {
            return CapabilitySupport::Unknown;
        };
        feature.supports(client, provider)
    }

    /// 客户端希望使用、但 Provider 无法证明支持的能力。非空结果必须交给规范化层
    /// 处理，不能静默发送。
    pub(crate) fn unsupported_features(
        &self,
        client_id: &str,
        provider_id: &str,
    ) -> Vec<CompatibilityFeature> {
        use CompatibilityFeature::*;
        [
            ToolResultImage,
            ToolResultFile,
            FunctionCallHistory,
            NamespaceTools,
            WebSearchTool,
            ToolChoice,
            UserInputImage,
            SamplingTemperature,
            SamplingTopP,
        ]
        .into_iter()
        .filter(|feature| {
            self.feature_support(client_id, provider_id, *feature) != CapabilitySupport::Supported
        })
        .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompatibilityFeature {
    ToolResultImage,
    ToolResultFile,
    FunctionCallHistory,
    NamespaceTools,
    WebSearchTool,
    ToolChoice,
    UserInputImage,
    SamplingTemperature,
    SamplingTopP,
}

impl CompatibilityFeature {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ToolResultImage => "tool_result_image",
            Self::ToolResultFile => "tool_result_file",
            Self::FunctionCallHistory => "function_call_history",
            Self::NamespaceTools => "namespace_tools",
            Self::WebSearchTool => "web_search_tool",
            Self::ToolChoice => "tool_choice",
            Self::UserInputImage => "user_input_image",
            Self::SamplingTemperature => "sampling_temperature",
            Self::SamplingTopP => "sampling_top_p",
        }
    }

    fn supports(
        self,
        client: &ClientCapability,
        provider: &ProviderCapability,
    ) -> CapabilitySupport {
        match self {
            Self::ToolResultImage => client.tool_result.image.merge(provider.tool_result.image),
            Self::ToolResultFile => client.tool_result.file.merge(provider.tool_result.file),
            Self::FunctionCallHistory => client
                .function_call_history
                .merge(provider.function_call_history),
            Self::NamespaceTools => client.namespace_tools.merge(provider.namespace_tools),
            Self::WebSearchTool => client.web_search_tool.merge(provider.web_search_tool),
            Self::ToolChoice => client.tool_choice.merge(provider.tool_choice),
            Self::UserInputImage => client.user_input_image.merge(provider.user_input_image),
            Self::SamplingTemperature => client
                .sampling
                .temperature
                .merge(provider.sampling.temperature),
            Self::SamplingTopP => client.sampling.top_p.merge(provider.sampling.top_p),
        }
    }
}

fn protocol_fallback_provider_id(protocol: &AIServiceProtocol) -> &'static str {
    match protocol {
        AIServiceProtocol::OpenaiResponses => "openai_responses_limited",
        AIServiceProtocol::OpenaiChat => "openai_chat_compatible",
        AIServiceProtocol::Anthropic => "anthropic_native",
    }
}
fn provider_match_score(provider: &ProviderCapability, host: &str, path: &str) -> Option<usize> {
    let host_score = provider
        .fingerprint
        .host_suffixes
        .iter()
        .map(|suffix| suffix.trim().to_ascii_lowercase())
        .filter(|suffix| !suffix.is_empty() && host.ends_with(suffix))
        .map(|suffix| suffix.len())
        .max()?;
    let path_score = provider
        .fingerprint
        .path_prefixes
        .iter()
        .map(|prefix| prefix.trim().trim_end_matches('/'))
        .filter(|prefix| !prefix.is_empty() && path.starts_with(prefix))
        .map(|value| value.len())
        .max()
        .unwrap_or(0);
    Some(host_score + path_score)
}

fn support(is_supported: bool) -> CapabilitySupport {
    if is_supported {
        CapabilitySupport::Supported
    } else {
        CapabilitySupport::Unsupported
    }
}

fn sample(temperature: CapabilitySupport, top_p: CapabilitySupport) -> SamplingCapabilities {
    SamplingCapabilities {
        temperature,
        top_p,
        max_tokens: CapabilitySupport::Supported,
        max_output_tokens: CapabilitySupport::Supported,
    }
}

#[allow(clippy::too_many_arguments)]
fn client(
    id: &str,
    display_name: &str,
    protocols: &[AIServiceProtocol],
    discovery: bool,
    image: CapabilitySupport,
    file: CapabilitySupport,
    namespace: CapabilitySupport,
    web_search: CapabilitySupport,
    one_million: CapabilitySupport,
    sampling: SamplingCapabilities,
) -> ClientCapability {
    ClientCapability {
        id: id.to_string(),
        display_name: display_name.to_string(),
        aliases: Vec::new(),
        entry_protocols: protocols.to_vec(),
        requires_model_discovery: discovery,
        tool_result: ToolResultCapabilities {
            text: CapabilitySupport::Supported,
            image,
            file,
        },
        function_call_history: CapabilitySupport::Supported,
        namespace_tools: namespace,
        web_search_tool: web_search,
        tool_choice: CapabilitySupport::Supported,
        user_input_image: CapabilitySupport::Supported,
        requires_1m_variant: one_million,
        sampling,
        rich_tool_client: true,
        default_gateway: true,
        direct_policy: DirectModePolicy::Warn,
    }
}

fn builtin_clients() -> Vec<ClientCapability> {
    use AIServiceProtocol::{Anthropic, OpenaiChat, OpenaiResponses};
    let s = CapabilitySupport::Supported;
    let u = CapabilitySupport::Unsupported;
    let x = CapabilitySupport::Unknown;
    let chat_sampling = sample(s, s);
    vec![
        client(
            "vscode",
            "VS Code",
            &[OpenaiChat],
            false,
            x,
            u,
            u,
            x,
            x,
            chat_sampling.clone(),
        ),
        client(
            "cc-switch",
            "CC Switch",
            &[OpenaiChat, OpenaiResponses],
            false,
            x,
            x,
            x,
            x,
            x,
            chat_sampling.clone(),
        ),
        client(
            "codex",
            "Codex",
            &[OpenaiResponses],
            false,
            x,
            u,
            s,
            s,
            u,
            sample(u, u),
        ),
        client(
            "workbuddy",
            "WorkBuddy",
            &[OpenaiChat],
            false,
            x,
            u,
            u,
            x,
            x,
            chat_sampling.clone(),
        ),
        client(
            "kimi-code",
            "Kimi Code",
            &[OpenaiChat],
            false,
            x,
            u,
            u,
            x,
            x,
            chat_sampling.clone(),
        ),
        client(
            "qwen-code",
            "Qwen Code",
            &[OpenaiChat],
            false,
            x,
            u,
            u,
            x,
            x,
            chat_sampling.clone(),
        ),
        client(
            "claude-code",
            "Claude Code",
            &[Anthropic],
            false,
            s,
            u,
            u,
            u,
            s,
            sample(s, s),
        ),
        client(
            "claude-desktop",
            "Claude Desktop",
            &[Anthropic],
            true,
            s,
            u,
            u,
            u,
            s,
            sample(s, s),
        ),
        client(
            "opencode",
            "OpenCode",
            &[OpenaiChat, OpenaiResponses, Anthropic],
            false,
            s,
            s,
            u,
            u,
            x,
            chat_sampling.clone(),
        ),
        client(
            "continue",
            "Continue",
            &[OpenaiChat, Anthropic],
            false,
            x,
            u,
            u,
            x,
            x,
            chat_sampling.clone(),
        ),
        client(
            "aider",
            "Aider",
            &[OpenaiChat],
            false,
            x,
            u,
            u,
            x,
            x,
            chat_sampling.clone(),
        ),
        client(
            "crush",
            "Crush",
            &[OpenaiChat],
            false,
            x,
            u,
            u,
            x,
            x,
            chat_sampling.clone(),
        ),
        client(
            "qoder",
            "Qoder",
            &[OpenaiChat],
            false,
            x,
            u,
            u,
            x,
            x,
            chat_sampling.clone(),
        ),
        client(
            "qoder-cn",
            "Qoder CN",
            &[OpenaiChat],
            false,
            x,
            u,
            u,
            x,
            x,
            chat_sampling.clone(),
        ),
        client(
            "zcode",
            "ZCode",
            &[Anthropic],
            false,
            x,
            u,
            u,
            u,
            x,
            sample(x, x),
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn provider(
    id: &str,
    display_name: &str,
    protocols: &[AIServiceProtocol],
    fingerprint: ProviderFingerprint,
    discovery: ModelDiscovery,
    tool_image: CapabilitySupport,
    tool_file: CapabilitySupport,
    history: CapabilitySupport,
    namespace: CapabilitySupport,
    web_search: CapabilitySupport,
    tool_choice: CapabilitySupport,
    input_image: CapabilitySupport,
    sampling: SamplingCapabilities,
) -> ProviderCapability {
    ProviderCapability {
        id: id.to_string(),
        display_name: display_name.to_string(),
        protocols: protocols.to_vec(),
        fingerprint,
        model_discovery: discovery,
        tool_result: ToolResultCapabilities {
            text: CapabilitySupport::Supported,
            image: tool_image,
            file: tool_file,
        },
        function_call_history: history,
        namespace_tools: namespace,
        web_search_tool: web_search,
        tool_choice,
        user_input_image: input_image,
        system_messages: CapabilitySupport::Supported,
        sampling,
        max_output_tokens: CapabilitySupport::Supported,
        context_window: CapabilitySupport::Supported,
        supports_1m: CapabilitySupport::Unknown,
        unknown_policy: FailureMode::StructuredDiagnostic,
    }
}

fn builtin_providers() -> Vec<ProviderCapability> {
    use AIServiceProtocol::{Anthropic, OpenaiChat, OpenaiResponses};
    let s = CapabilitySupport::Supported;
    let u = CapabilitySupport::Unsupported;
    let x = CapabilitySupport::Unknown;
    vec![
        provider(
            "anthropic_native",
            "Anthropic Messages native",
            &[Anthropic],
            ProviderFingerprint::default(),
            ModelDiscovery::Native,
            s,
            s,
            s,
            u,
            u,
            s,
            s,
            sample(s, s),
        ),
        provider(
            "openai_chat_compatible",
            "OpenAI Chat compatible",
            &[OpenaiChat],
            ProviderFingerprint::default(),
            ModelDiscovery::Native,
            x,
            u,
            s,
            u,
            x,
            s,
            x,
            sample(s, s),
        ),
        provider(
            "openai_responses_strict",
            "OpenAI Responses full",
            &[OpenaiResponses],
            ProviderFingerprint::default(),
            ModelDiscovery::Native,
            s,
            s,
            s,
            s,
            s,
            s,
            s,
            sample(s, s),
        ),
        // Verified 2026-11-05: Vector Tide accepts structured function_call /
        // function_call_output history plus tools, tool_choice and sampling on
        // both /v1/responses and /v1/chat/completions.
        provider(
            "vectide_responses",
            "Vector Tide Responses",
            &[OpenaiResponses],
            ProviderFingerprint {
                host_suffixes: vec!["vectide.cn".to_string()],
                path_prefixes: vec!["/v1".to_string()],
            },
            ModelDiscovery::Native,
            u,
            u,
            s,
            u,
            u,
            s,
            x,
            sample(s, s),
        ),
        // Vector Tide's Anthropic endpoint accepts text/tool history and
        // sampling, but tool-result image blocks have caused generic 400s in
        // Claude Code sessions. Keep the safe media degradation boundary.
        provider(
            "vectide_anthropic",
            "Vector Tide Anthropic",
            &[Anthropic],
            ProviderFingerprint {
                host_suffixes: vec!["vectide.cn".to_string()],
                path_prefixes: vec!["/v1".to_string()],
            },
            ModelDiscovery::Native,
            u,
            u,
            s,
            u,
            u,
            s,
            x,
            sample(s, s),
        ),
        provider(
            "openai_responses_limited",
            "OpenAI Responses limited",
            &[OpenaiResponses],
            ProviderFingerprint::default(),
            ModelDiscovery::Native,
            u,
            u,
            u,
            u,
            u,
            s,
            s,
            sample(x, x),
        ),
        provider(
            "volcengine_agent_plan",
            "Volcengine Agent Plan",
            &[Anthropic],
            ProviderFingerprint {
                host_suffixes: vec!["volces.com".to_string()],
                path_prefixes: vec!["/api/plan".to_string()],
            },
            ModelDiscovery::LocalCatalog,
            x,
            u,
            s,
            u,
            u,
            s,
            x,
            sample(x, x),
        ),
        ProviderCapability {
            id: UNKNOWN_PROVIDER_ID.to_string(),
            display_name: "Unknown compatible provider".to_string(),
            protocols: vec![OpenaiChat, OpenaiResponses, Anthropic],
            fingerprint: ProviderFingerprint::default(),
            model_discovery: ModelDiscovery::Unknown,
            tool_result: ToolResultCapabilities {
                text: CapabilitySupport::Unknown,
                image: CapabilitySupport::Unknown,
                file: CapabilitySupport::Unknown,
            },
            function_call_history: CapabilitySupport::Unknown,
            namespace_tools: CapabilitySupport::Unknown,
            web_search_tool: CapabilitySupport::Unknown,
            tool_choice: CapabilitySupport::Unknown,
            user_input_image: CapabilitySupport::Unknown,
            system_messages: CapabilitySupport::Unknown,
            sampling: SamplingCapabilities::default(),
            max_output_tokens: CapabilitySupport::Unknown,
            context_window: CapabilitySupport::Unknown,
            supports_1m: CapabilitySupport::Unknown,
            unknown_policy: FailureMode::StructuredDiagnostic,
        },
    ]
}

fn agent_plan_rule(model: &str, image: CapabilitySupport) -> ModelCapabilityRule {
    ModelCapabilityRule {
        provider_id: "volcengine_agent_plan".to_string(),
        matcher: ModelMatch::Exact,
        model: model.to_string(),
        context_window: Some(1_000_000),
        max_output_tokens: Some(64_000),
        supports_1m: CapabilitySupport::Supported,
        user_input_image: image,
    }
}

fn builtin_model_rules() -> Vec<ModelCapabilityRule> {
    let s = CapabilitySupport::Supported;
    let u = CapabilitySupport::Unsupported;
    [
        ("doubao-seed-evolving", s),
        ("glm-5.3", u),
        ("glm-5.3-flash", s),
        ("kimi-k2.8-preview", s),
        ("kimi-k3", s),
        ("kimi-k3.1-pro", s),
        ("deepseek-v4.1-flash", s),
        ("deepseek-v4.1-flash-economy", s),
        ("deepseek-v4-flash", u),
        ("deepseek-v4-pro", u),
    ]
    .into_iter()
    .map(|(model, image)| agent_plan_rule(model, image))
    .collect()
}

pub(crate) fn model_base_id(model: &str) -> &str {
    model.trim().strip_suffix("[1m]").unwrap_or(model.trim())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct NormalizationChange {
    pub feature: String,
    pub action: NormalizationAction,
    pub detail: String,
}

impl Default for NormalizationChange {
    fn default() -> Self {
        Self {
            feature: String::new(),
            action: NormalizationAction::Native,
            detail: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedProviderRequest {
    pub request: serde_json::Value,
    pub changes: Vec<NormalizationChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompatibilityNormalizationError {
    pub feature: CompatibilityFeature,
    pub message: String,
}

impl std::fmt::Display for CompatibilityNormalizationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CompatibilityNormalizationError {}

/// 把任意入口协议产生的上游请求收敛到 Provider 能接受的子集。
///
/// - 工具结果图片/文件、工具历史、namespace/web_search、tool_choice 和可选采样参数
///   可以在不改变用户意图的前提下降级；
/// - 用户在消息里主动输入的图片不允许静默删除，Provider 不支持时返回明确错误。
pub(crate) fn normalize_provider_request(
    protocol: &AIServiceProtocol,
    request: serde_json::Value,
    client: &ClientCapability,
    provider: &ProviderCapability,
    model_rule: Option<&ModelCapabilityRule>,
) -> Result<NormalizedProviderRequest, CompatibilityNormalizationError> {
    let mut effective_provider = provider.clone();
    if let Some(rule) = model_rule {
        if rule.user_input_image != CapabilitySupport::Unknown {
            effective_provider.user_input_image = rule.user_input_image;
        }
        if rule.supports_1m != CapabilitySupport::Unknown {
            effective_provider.supports_1m = rule.supports_1m;
        }
        if rule.max_output_tokens.is_some() {
            effective_provider.max_output_tokens = CapabilitySupport::Supported;
        }
        if rule.context_window.is_some() {
            effective_provider.context_window = CapabilitySupport::Supported;
        }
    }
    let provider = &effective_provider;
    let mut request = request;
    let mut changes = Vec::new();
    match protocol {
        AIServiceProtocol::OpenaiResponses => {
            normalize_responses_request(&mut request, client, provider, &mut changes)?;
        }
        AIServiceProtocol::OpenaiChat => {
            normalize_chat_request(&mut request, client, provider, &mut changes)?;
        }
        AIServiceProtocol::Anthropic => {
            normalize_anthropic_request(&mut request, client, provider, &mut changes)?;
        }
    }
    Ok(NormalizedProviderRequest { request, changes })
}

fn normalize_responses_request(
    request: &mut serde_json::Value,
    client: &ClientCapability,
    provider: &ProviderCapability,
    changes: &mut Vec<NormalizationChange>,
) -> Result<(), CompatibilityNormalizationError> {
    use serde_json::json;

    if let Some(input) = request
        .get_mut("input")
        .and_then(serde_json::Value::as_array_mut)
    {
        let mut neutralized_legacy_marker = false;
        for item in input.iter_mut() {
            neutralized_legacy_marker |= neutralize_legacy_tool_call_marker(item);
            if item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
            {
                degrade_response_tool_output_media(item, provider, changes);
                continue;
            }
            if item.get("type").and_then(serde_json::Value::as_str) == Some("message") {
                if let Some(content) = item
                    .get_mut("content")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    if content.iter().any(|part| {
                        part.get("type").and_then(serde_json::Value::as_str) == Some("input_image")
                    }) && !provider.user_input_image.is_supported()
                    {
                        return Err(CompatibilityNormalizationError {
                            feature: CompatibilityFeature::UserInputImage,
                            message: format!(
                                "{} 不支持用户主动输入图片；已保留原请求并拒绝发送",
                                provider.display_name
                            ),
                        });
                    }
                }
            }
        }

        if neutralized_legacy_marker {
            record_change(
                changes,
                CompatibilityFeature::FunctionCallHistory,
                NormalizationAction::Translate,
                "已将历史中的伪工具调用文本改为普通说明，避免模型继续模仿",
            );
        }

        if !provider.function_call_history.is_supported() {
            // Keep the fallback as ordinary prose, never as bracketed pseudo
            // tool-call syntax: models copy that syntax and return it as final
            // text, which makes OpenCode stop after a non-executed "tool call".
            let mut rewritten = Vec::with_capacity(input.len());
            let mut changed = false;
            for item in std::mem::take(input) {
                match item.get("type").and_then(serde_json::Value::as_str) {
                    Some("function_call") => {
                        changed = true;
                        let name = item
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("tool");
                        let arguments = item
                            .get("arguments")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        rewritten.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": format!(
                                "Earlier the assistant used the {name} tool. Arguments: {arguments}"
                            )
                            }]
                        }));
                    }
                    Some("function_call_output") => {
                        changed = true;
                        let call_id = item
                            .get("call_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown");
                        let output = response_output_text(item.get("output"));
                        rewritten.push(json!({
                            "type": "message",
                            "role": "user",
                            "content": [{
                                "type": "input_text",
                                "text": format!(
                                    "Result from the earlier {call_id} tool call: {output}"
                                )
                            }]
                        }));
                    }
                    _ => rewritten.push(item),
                }
            }
            *input = rewritten;
            if changed {
                record_change(
                    changes,
                    CompatibilityFeature::FunctionCallHistory,
                    NormalizationAction::Degrade,
                    "已将 Responses 工具调用历史降级为非可执行的文本摘要",
                );
            }
        }
    }

    if let Some(tools) = request
        .get_mut("tools")
        .and_then(serde_json::Value::as_array_mut)
    {
        let before = tools.len();
        tools.retain(|tool| {
            let kind = tool
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            match kind {
                "namespace" if !provider.namespace_tools.is_supported() => false,
                "web_search" if !provider.web_search_tool.is_supported() => false,
                _ => true,
            }
        });
        if tools.len() != before {
            let feature = if provider.namespace_tools.is_supported() {
                CompatibilityFeature::WebSearchTool
            } else {
                CompatibilityFeature::NamespaceTools
            };
            record_change(
                changes,
                feature,
                NormalizationAction::Degrade,
                "已移除 Provider 不支持的 namespace/web_search 工具",
            );
        }
        if tools.is_empty() {
            request.as_object_mut().map(|object| object.remove("tools"));
        }
    }

    if !provider.tool_choice.is_supported()
        || request
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        if request
            .as_object_mut()
            .is_some_and(|object| object.remove("tool_choice").is_some())
        {
            record_change(
                changes,
                CompatibilityFeature::ToolChoice,
                NormalizationAction::Degrade,
                "已移除无效的 tool_choice",
            );
        }
    }
    normalize_sampling(request, provider, changes);
    let _ = client;
    Ok(())
}

/// Older compatibility revisions rendered structured function calls as
/// `[工具调用 name] {...}` assistant text. Models copied that marker and clients
/// treated the result as a completed answer. Rewrite only that legacy marker
/// into prose so existing sessions can recover without inventing a tool result.
fn neutralize_legacy_tool_call_marker(item: &mut serde_json::Value) -> bool {
    if item.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
        return false;
    }
    let Some(content) = item
        .get_mut("content")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    let mut changed = false;
    for part in content {
        if part.get("type").and_then(serde_json::Value::as_str) != Some("output_text") {
            continue;
        }
        let Some(text) = part
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        let Some(marker) = text.find("[工具调用 ") else {
            continue;
        };
        let detail = &text[marker + "[工具调用 ".len()..];
        let (name, arguments) = detail
            .split_once("] ")
            .map(|(name, arguments)| (name.trim(), arguments))
            .unwrap_or((detail.trim(), ""));
        let neutral = format!(
            "Earlier the assistant attempted to call the {name} tool, but the call was not executed. Arguments: {arguments}"
        );
        part["text"] = serde_json::Value::String(format!("{}{neutral}", &text[..marker]));
        changed = true;
    }
    changed
}

fn degrade_response_tool_output_media(
    item: &mut serde_json::Value,
    provider: &ProviderCapability,
    changes: &mut Vec<NormalizationChange>,
) {
    let Some(output) = item
        .get_mut("output")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for part in output.iter_mut() {
        let Some(kind) = part.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let (feature, label, allowed) = match kind {
            "input_image" => (
                CompatibilityFeature::ToolResultImage,
                "图片",
                provider.tool_result.image.is_supported(),
            ),
            "input_file" => (
                CompatibilityFeature::ToolResultFile,
                "文件",
                provider.tool_result.file.is_supported(),
            ),
            _ => continue,
        };
        if allowed {
            continue;
        }
        *part = serde_json::json!({
            "type": "input_text",
            "text": format!("[工具结果中的{label}已由 HiMind 兼容层省略：{} 不支持]", provider.display_name)
        });
        record_change(
            changes,
            feature,
            NormalizationAction::Degrade,
            format!("已将工具结果中的{label}降级为文本占位"),
        );
    }
}

fn response_output_text(output: Option<&serde_json::Value>) -> String {
    match output {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| part.get("image_url").and_then(serde_json::Value::as_str))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

fn normalize_chat_request(
    request: &mut serde_json::Value,
    _client: &ClientCapability,
    provider: &ProviderCapability,
    changes: &mut Vec<NormalizationChange>,
) -> Result<(), CompatibilityNormalizationError> {
    if let Some(messages) = request
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    {
        for message in messages.iter_mut() {
            let role = message
                .get("role")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let Some(content) = message
                .get_mut("content")
                .and_then(serde_json::Value::as_array_mut)
            else {
                continue;
            };
            for part in content.iter_mut() {
                let kind = part
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if role == "tool" && matches!(kind, "image_url" | "file") {
                    let (feature, label) = if kind == "file" {
                        (CompatibilityFeature::ToolResultFile, "文件")
                    } else {
                        (CompatibilityFeature::ToolResultImage, "图片")
                    };
                    let allowed = if kind == "file" {
                        provider.tool_result.file.is_supported()
                    } else {
                        provider.tool_result.image.is_supported()
                    };
                    if !allowed {
                        *part = serde_json::json!({
                            "type": "text",
                            "text": format!("[工具结果中的{label}已由 HiMind 兼容层省略]")
                        });
                        record_change(
                            changes,
                            feature,
                            NormalizationAction::Degrade,
                            format!("已将 Chat 工具结果中的{label}降级为文本占位"),
                        );
                    }
                    continue;
                }
                if role == "user"
                    && matches!(kind, "image_url" | "input_image")
                    && !provider.user_input_image.is_supported()
                {
                    return Err(CompatibilityNormalizationError {
                        feature: CompatibilityFeature::UserInputImage,
                        message: format!(
                            "{} 不支持用户主动输入图片；已保留原请求并拒绝发送",
                            provider.display_name
                        ),
                    });
                }
            }
        }
    }
    if !provider.tool_choice.is_supported()
        && request
            .as_object_mut()
            .is_some_and(|object| object.remove("tool_choice").is_some())
    {
        record_change(
            changes,
            CompatibilityFeature::ToolChoice,
            NormalizationAction::Degrade,
            "已移除 Provider 不支持的 tool_choice",
        );
    }
    normalize_sampling(request, provider, changes);
    Ok(())
}

fn normalize_anthropic_request(
    request: &mut serde_json::Value,
    _client: &ClientCapability,
    provider: &ProviderCapability,
    changes: &mut Vec<NormalizationChange>,
) -> Result<(), CompatibilityNormalizationError> {
    if let Some(messages) = request
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    {
        for message in messages.iter_mut() {
            let role = message
                .get("role")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let Some(content) = message
                .get_mut("content")
                .and_then(serde_json::Value::as_array_mut)
            else {
                continue;
            };
            for block in content.iter_mut() {
                let kind = block
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if role == "user" && kind == "image" && !provider.user_input_image.is_supported() {
                    return Err(CompatibilityNormalizationError {
                        feature: CompatibilityFeature::UserInputImage,
                        message: format!(
                            "{} 不支持用户主动输入图片；已保留原请求并拒绝发送",
                            provider.display_name
                        ),
                    });
                }
                if kind != "tool_result" {
                    continue;
                }
                let Some(tool_content) = block
                    .get_mut("content")
                    .and_then(serde_json::Value::as_array_mut)
                else {
                    continue;
                };
                for part in tool_content.iter_mut() {
                    let part_kind = part
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    let (feature, label, allowed) = match part_kind {
                        "image" => (
                            CompatibilityFeature::ToolResultImage,
                            "图片",
                            provider.tool_result.image.is_supported(),
                        ),
                        "document" => (
                            CompatibilityFeature::ToolResultFile,
                            "文件",
                            provider.tool_result.file.is_supported(),
                        ),
                        _ => continue,
                    };
                    if allowed {
                        continue;
                    }
                    *part = serde_json::json!({
                        "type": "text",
                        "text": format!("[工具结果中的{label}已由 HiMind 兼容层省略]")
                    });
                    record_change(
                        changes,
                        feature,
                        NormalizationAction::Degrade,
                        format!("已将 Anthropic 工具结果中的{label}降级为文本占位"),
                    );
                }
            }
        }
    }
    if !provider.tool_choice.is_supported()
        && request
            .as_object_mut()
            .is_some_and(|object| object.remove("tool_choice").is_some())
    {
        record_change(
            changes,
            CompatibilityFeature::ToolChoice,
            NormalizationAction::Degrade,
            "已移除 Provider 不支持的 tool_choice",
        );
    }
    normalize_sampling(request, provider, changes);
    Ok(())
}

fn normalize_sampling(
    request: &mut serde_json::Value,
    provider: &ProviderCapability,
    changes: &mut Vec<NormalizationChange>,
) {
    for (field, feature, allowed) in [
        (
            "temperature",
            CompatibilityFeature::SamplingTemperature,
            provider.sampling.temperature.is_supported(),
        ),
        (
            "top_p",
            CompatibilityFeature::SamplingTopP,
            provider.sampling.top_p.is_supported(),
        ),
    ] {
        if allowed || !request.get(field).is_some() {
            continue;
        }
        if request
            .as_object_mut()
            .is_some_and(|object| object.remove(field).is_some())
        {
            record_change(
                changes,
                feature,
                NormalizationAction::Degrade,
                format!("已移除 Provider 未证明支持的 {field}"),
            );
        }
    }
}

fn record_change(
    changes: &mut Vec<NormalizationChange>,
    feature: CompatibilityFeature,
    action: NormalizationAction,
    detail: impl Into<String>,
) {
    let feature = feature.as_str().to_string();
    let detail = detail.into();
    if changes
        .iter()
        .any(|change| change.feature == feature && change.action == action)
    {
        return;
    }
    changes.push(NormalizationChange {
        feature,
        action,
        detail,
    });
}
#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_CLIENTS: &[&str] = &[
        "vscode",
        "cc-switch",
        "codex",
        "workbuddy",
        "kimi-code",
        "qwen-code",
        "claude-code",
        "claude-desktop",
        "opencode",
        "continue",
        "aider",
        "crush",
        "qoder",
        "qoder-cn",
        "zcode",
    ];

    #[test]
    fn builtin_matrix_is_versioned_complete_and_round_trips() {
        let matrix = CapabilityMatrixV1::builtin();
        matrix.validate().unwrap();
        assert_eq!(matrix.schema_version, AI_COMPATIBILITY_SCHEMA_VERSION);
        assert_eq!(matrix.clients.len(), EXPECTED_CLIENTS.len());
        for id in EXPECTED_CLIENTS {
            assert!(matrix.client(id).is_some(), "missing client {id}");
        }
        for provider in [
            "anthropic_native",
            "openai_chat_compatible",
            "openai_responses_strict",
            "vectide_responses",
            "vectide_anthropic",
            "openai_responses_limited",
            "volcengine_agent_plan",
            UNKNOWN_PROVIDER_ID,
        ] {
            assert!(
                matrix.provider(provider).is_some(),
                "missing provider {provider}"
            );
        }
        let encoded = serde_json::to_vec(&matrix).unwrap();
        let decoded: CapabilityMatrixV1 = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, matrix);
    }

    #[test]
    fn agent_plan_has_ten_one_million_context_models() {
        let matrix = CapabilityMatrixV1::builtin();
        let rules = matrix
            .model_rules
            .iter()
            .filter(|rule| rule.provider_id == "volcengine_agent_plan")
            .collect::<Vec<_>>();
        assert_eq!(rules.len(), 10);
        assert!(rules
            .iter()
            .all(|rule| rule.context_window == Some(1_000_000)));
        assert!(rules
            .iter()
            .all(|rule| rule.max_output_tokens == Some(64_000)));
        assert!(rules
            .iter()
            .all(|rule| rule.supports_1m == CapabilitySupport::Supported));
        assert!(matrix
            .model_rule("volcengine_agent_plan", "glm-5.3[1m]")
            .is_some());
    }

    #[test]
    fn unknown_capability_is_never_treated_as_supported() {
        assert!(!CapabilitySupport::Unknown.is_supported());
        assert!(!CapabilitySupport::Unsupported.is_supported());
        assert!(CapabilitySupport::Supported.is_supported());
        let matrix = CapabilityMatrixV1::builtin();
        let unknown = matrix.provider(UNKNOWN_PROVIDER_ID).unwrap();
        assert_eq!(unknown.tool_result.image, CapabilitySupport::Unknown);
        assert_eq!(
            matrix.unknown_policy.tool_result_image,
            NormalizationAction::Degrade
        );
        assert!(matrix.unknown_policy.preserve_user_intent);
    }

    #[test]
    fn rich_tool_clients_default_to_gateway_and_direct_warns() {
        let matrix = CapabilityMatrixV1::builtin();
        for id in DEFAULT_GATEWAY_CLIENT_IDS {
            let client = matrix.client(id).unwrap();
            assert!(client.rich_tool_client, "{id} must be a rich-tool client");
            assert!(client.default_gateway, "{id} must default to gateway");
            assert_eq!(client.direct_policy, DirectModePolicy::Warn);
        }
    }

    #[test]
    fn agent_plan_url_fingerprint_wins_over_unknown_default() {
        let matrix = CapabilityMatrixV1::builtin();
        let provider = matrix.resolve_provider(
            "https://ark.cn-beijing.volces.com/api/plan",
            &AIServiceProtocol::Anthropic,
        );
        assert_eq!(provider.id, "volcengine_agent_plan");
        assert_eq!(provider.model_discovery, ModelDiscovery::LocalCatalog);
        assert!(provider.model_discovery.is_available());
    }

    #[test]
    fn vectide_anthropic_degrades_tool_result_images_safely() {
        let matrix = CapabilityMatrixV1::builtin();
        let provider =
            matrix.resolve_provider("https://vectide.cn/v1", &AIServiceProtocol::Anthropic);
        assert_eq!(provider.id, "vectide_anthropic");
        let client = matrix.client("claude-desktop").unwrap();
        let request = serde_json::json!({
            "model": "claude-himind-10984728388438050670[1m]",
            "max_tokens": 32000,
            "thinking": {"type": "adaptive"},
            "output_config": {"effort": "high"},
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "检查截图"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "shot.png"}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": [
                    {"type": "text", "text": "screenshot"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "AAAA"}}
                ]}]}
            ],
            "tools": [{"name": "Read", "description": "读取文件", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "auto"}
        });
        let normalized = normalize_provider_request(
            &AIServiceProtocol::Anthropic,
            request,
            client,
            provider,
            None,
        )
        .unwrap();
        let nested = normalized.request["messages"][2]["content"][0]["content"]
            .as_array()
            .unwrap();
        assert_eq!(nested[1]["type"], "text");
        assert!(!normalized.request.to_string().contains("data:image"));
        assert!(normalized
            .changes
            .iter()
            .any(|change| change.feature == "tool_result_image"));
        assert_eq!(
            normalized.request["messages"][1]["content"][0]["type"],
            "tool_use"
        );
        assert_eq!(normalized.request["thinking"]["type"], "adaptive");
        assert_eq!(normalized.request["output_config"]["effort"], "high");
    }

    #[test]
    fn vectide_responses_keeps_structured_tool_history() {
        let matrix = CapabilityMatrixV1::builtin();
        let provider =
            matrix.resolve_provider("https://vectide.cn/v1", &AIServiceProtocol::OpenaiResponses);
        assert_eq!(provider.id, "vectide_responses");
        let unsupported = matrix.unsupported_features("opencode", &provider.id);
        assert!(!unsupported.contains(&CompatibilityFeature::FunctionCallHistory));
        assert!(!unsupported.contains(&CompatibilityFeature::ToolChoice));
        assert!(!unsupported.contains(&CompatibilityFeature::SamplingTemperature));
        assert!(!unsupported.contains(&CompatibilityFeature::SamplingTopP));
    }

    #[test]
    fn vectide_responses_normalization_never_turns_tool_calls_into_visible_markers() {
        let matrix = CapabilityMatrixV1::builtin();
        let client = matrix.client("opencode").unwrap();
        let provider = matrix.provider("vectide_responses").unwrap();
        let request = serde_json::json!({
            "model": "deepseek-v4.1-flash-1017",
            "temperature": 0.2,
            "top_p": 0.9,
            "tool_choice": "auto",
            "tools": [{"type": "function", "name": "shell"}],
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "运行检查"}]},
                {"type": "function_call", "name": "shell", "arguments": "{\"command\":\"pwd\"}", "call_id": "c1"},
                {"type": "function_call_output", "call_id": "c1", "output": [
                    {"type": "input_text", "text": "/workspace"},
                    {"type": "input_image", "image_url": "data:image/jpeg;base64,AAAA"}
                ]}
            ]
        });
        let normalized = normalize_provider_request(
            &AIServiceProtocol::OpenaiResponses,
            request,
            client,
            provider,
            None,
        )
        .unwrap();
        let items = normalized.request["input"].as_array().unwrap();
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(items[2]["type"], "function_call_output");
        let serialized = normalized.request.to_string();
        assert!(!serialized.contains("[工具调用"));
        assert!(!serialized.contains("data:image"));
        assert!(normalized
            .changes
            .iter()
            .any(|change| change.feature == "tool_result_image"));
        assert!(!normalized
            .changes
            .iter()
            .any(|change| change.feature == "function_call_history"));
    }

    #[test]
    fn one_million_suffix_is_client_syntax_not_provider_capability() {
        assert_eq!(model_base_id("glm-5.3[1m]"), "glm-5.3");
        assert_eq!(model_base_id(" glm-5.3 "), "glm-5.3");
        let matrix = CapabilityMatrixV1::builtin();
        let rule = matrix
            .model_rule("volcengine_agent_plan", "glm-5.3[1m]")
            .unwrap();
        assert_eq!(rule.model, "glm-5.3");
    }

    #[test]
    fn legacy_visible_tool_marker_is_neutralized_for_existing_sessions() {
        let matrix = CapabilityMatrixV1::builtin();
        let client = matrix.client("opencode").unwrap();
        let provider = matrix.provider("vectide_responses").unwrap();
        let request = serde_json::json!({
            "model": "deepseek-v4.1-flash-1017",
            "input": [
                {"type": "message", "role": "assistant", "content": [{
                    "type": "output_text",
                    "text": "先检查样式。\n[工具调用 shell] {\"command\":\"Get-Content styles.css\"}"
                }]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "继续"}]}
            ]
        });
        let normalized = normalize_provider_request(
            &AIServiceProtocol::OpenaiResponses,
            request,
            client,
            provider,
            None,
        )
        .unwrap();
        let serialized = normalized.request.to_string();
        assert!(!serialized.contains("[工具调用"));
        assert!(serialized.contains("the call was not executed"));
        assert!(normalized
            .changes
            .iter()
            .any(|change| change.feature == "function_call_history"
                && change.action == NormalizationAction::Translate));
    }

    #[test]
    fn limited_responses_marks_open_code_risky_features_unsupported() {
        let matrix = CapabilityMatrixV1::builtin();
        let unsupported = matrix.unsupported_features("opencode", "openai_responses_limited");
        assert!(unsupported.contains(&CompatibilityFeature::ToolResultImage));
        assert!(unsupported.contains(&CompatibilityFeature::ToolResultFile));
        assert!(unsupported.contains(&CompatibilityFeature::FunctionCallHistory));
        assert!(unsupported.contains(&CompatibilityFeature::NamespaceTools));
        assert!(unsupported.contains(&CompatibilityFeature::WebSearchTool));
        assert!(!unsupported.contains(&CompatibilityFeature::UserInputImage));
    }

    #[test]
    fn diagnostic_contains_required_safe_fields() {
        let diagnostic = CompatibilityDiagnosticV1 {
            client: "opencode".to_string(),
            provider: "openai_responses_limited".to_string(),
            client_protocol: "openai-responses".to_string(),
            provider_protocol: "openai-responses".to_string(),
            unsupported_feature: "tool_result_image".to_string(),
            upstream_status: 400,
            request_id: Some("req_vt_test".to_string()),
            attempted_normalizations: vec![NormalizationAction::Degrade],
            suggested_action: "走本机网关并降级工具结果图片".to_string(),
            safe_message: "Provider 不支持工具结果图片".to_string(),
            ..CompatibilityDiagnosticV1::default()
        };
        let value = serde_json::to_value(&diagnostic).unwrap();
        for field in [
            "client",
            "provider",
            "unsupported_feature",
            "request_id",
            "suggested_action",
            "attempted_normalizations",
        ] {
            assert!(value.get(field).is_some(), "diagnostic missing {field}");
        }
        let encoded = value.to_string();
        assert!(!encoded.contains("Authorization"));
        assert!(!encoded.contains("data:image"));
        for feature in [
            CompatibilityFeature::ToolResultImage,
            CompatibilityFeature::ToolResultFile,
            CompatibilityFeature::FunctionCallHistory,
            CompatibilityFeature::NamespaceTools,
            CompatibilityFeature::WebSearchTool,
            CompatibilityFeature::ToolChoice,
            CompatibilityFeature::UserInputImage,
            CompatibilityFeature::SamplingTemperature,
            CompatibilityFeature::SamplingTopP,
        ] {
            assert!(!feature.as_str().is_empty());
        }
        assert!(!support(false).is_supported());
    }
    #[test]
    fn limited_responses_normalizes_before_upstream_call() {
        let matrix = CapabilityMatrixV1::builtin();
        let client = matrix.client("opencode").unwrap();
        let provider = matrix.provider("openai_responses_limited").unwrap();
        let request = serde_json::json!({
            "model": "model-a",
            "temperature": 0.2,
            "top_p": 0.9,
            "tool_choice": "auto",
            "tools": [
                {"type": "function", "name": "read"},
                {"type": "namespace", "name": "mcp__cua", "tools": []},
                {"type": "web_search"}
            ],
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "看图"}]},
                {"type": "function_call", "name": "execute", "arguments": "{}", "call_id": "c1"},
                {"type": "function_call_output", "call_id": "c1", "output": [
                    {"type": "input_text", "text": "screenshot"},
                    {"type": "input_image", "image_url": "data:image/jpeg;base64,AAAA"}
                ]}
            ]
        });
        let normalized = normalize_provider_request(
            &AIServiceProtocol::OpenaiResponses,
            request,
            client,
            provider,
            None,
        )
        .unwrap();
        assert!(normalized.request.get("temperature").is_none());
        assert!(normalized.request.get("top_p").is_none());
        assert_eq!(normalized.request["tools"].as_array().unwrap().len(), 1);
        assert_eq!(normalized.request["tools"][0]["type"], "function");
        assert!(normalized.request["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["type"] == "message"));
        let serialized = normalized.request.to_string();
        assert!(!serialized.contains("input_image"));
        assert!(!serialized.contains("data:image"));
        assert!(!serialized.contains("[工具调用"));
        for feature in [
            "tool_result_image",
            "function_call_history",
            "namespace_tools",
            "sampling_temperature",
            "sampling_top_p",
        ] {
            assert!(
                normalized
                    .changes
                    .iter()
                    .any(|change| change.feature == feature),
                "missing normalization change {feature}"
            );
        }
    }

    #[test]
    fn chat_tool_result_image_is_degraded_but_user_image_is_rejected() {
        let matrix = CapabilityMatrixV1::builtin();
        let client = matrix.client("opencode").unwrap();
        let provider = matrix.provider("openai_chat_compatible").unwrap();
        let request = serde_json::json!({
            "model": "model-a",
            "messages": [
                {"role": "user", "content": "执行工具"},
                {"role": "tool", "tool_call_id": "c1", "content": [
                    {"type": "text", "text": "screenshot"},
                    {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,AAAA"}}
                ]}
            ]
        });
        let normalized = normalize_provider_request(
            &AIServiceProtocol::OpenaiChat,
            request,
            client,
            provider,
            None,
        )
        .unwrap();
        assert!(!normalized.request.to_string().contains("data:image"));
        assert!(normalized
            .changes
            .iter()
            .any(|change| change.feature == "tool_result_image"));

        let user_image = serde_json::json!({
            "model": "model-a",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "看图"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
            ]}]
        });
        let error = normalize_provider_request(
            &AIServiceProtocol::OpenaiChat,
            user_image,
            client,
            provider,
            None,
        )
        .unwrap_err();
        assert_eq!(error.feature, CompatibilityFeature::UserInputImage);
        assert!(error.message.contains("用户主动输入图片"));
    }

    #[test]
    fn agent_plan_keeps_supported_images_and_core_fields() {
        let matrix = CapabilityMatrixV1::builtin();
        let client = matrix.client("claude-desktop").unwrap();
        let provider = matrix.provider("volcengine_agent_plan").unwrap();
        let request = serde_json::json!({
            "model": "glm-5.3-flash",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "看图"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}
            ]}],
            "tools": [{"name": "read", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "auto"}
        });
        let model_rule = matrix
            .model_rule("volcengine_agent_plan", "glm-5.3-flash")
            .unwrap();
        let normalized = normalize_provider_request(
            &AIServiceProtocol::Anthropic,
            request,
            client,
            provider,
            Some(model_rule),
        )
        .unwrap();
        assert_eq!(normalized.request["max_tokens"], 64);
        assert_eq!(
            normalized.request["messages"][0]["content"][1]["type"],
            "image"
        );
        assert!(normalized.request.get("tools").is_some());
    }
}
#[cfg(test)]
mod fixture_tests {
    use super::*;
    use std::collections::BTreeMap;

    const MANIFEST: &str = include_str!("../../contracts/ai-compatibility/v1/manifest.json");
    const FIXTURES: &[(&str, &str)] = &[
        (
            "opencode-tool-media.json",
            include_str!("../../contracts/ai-compatibility/v1/fixtures/opencode-tool-media.json"),
        ),
        (
            "codex-namespace-web-search.json",
            include_str!(
                "../../contracts/ai-compatibility/v1/fixtures/codex-namespace-web-search.json"
            ),
        ),
        (
            "anthropic-to-chat.json",
            include_str!("../../contracts/ai-compatibility/v1/fixtures/anthropic-to-chat.json"),
        ),
        (
            "anthropic-to-responses.json",
            include_str!(
                "../../contracts/ai-compatibility/v1/fixtures/anthropic-to-responses.json"
            ),
        ),
        (
            "responses-to-chat.json",
            include_str!("../../contracts/ai-compatibility/v1/fixtures/responses-to-chat.json"),
        ),
        (
            "chat-to-responses.json",
            include_str!("../../contracts/ai-compatibility/v1/fixtures/chat-to-responses.json"),
        ),
    ];

    #[derive(Debug, serde::Deserialize)]
    struct Manifest {
        schema_version: u32,
        matrix: String,
        fixtures: Vec<String>,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Pipeline {
        Normalize,
        AnthropicToChat,
        AnthropicToResponses,
        ResponsesToChat,
        ChatToResponses,
    }

    #[derive(Debug, Default, serde::Deserialize)]
    #[serde(default)]
    struct Expectations {
        paths: BTreeMap<String, serde_json::Value>,
        lengths: BTreeMap<String, usize>,
        absent_paths: Vec<String>,
        forbidden_substrings: Vec<String>,
        changes_include: Vec<String>,
    }

    #[derive(Debug, serde::Deserialize)]
    struct Fixture {
        schema_version: u32,
        name: String,
        pipeline: Pipeline,
        client: String,
        provider: String,
        protocol: String,
        #[serde(default)]
        default_model: String,
        input: serde_json::Value,
        expect: Expectations,
    }

    #[test]
    fn golden_fixtures_are_versioned_and_cover_required_pipelines() {
        let manifest: Manifest = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest.schema_version, AI_COMPATIBILITY_SCHEMA_VERSION);
        assert_eq!(manifest.matrix, "CapabilityMatrixV1");
        assert_eq!(manifest.fixtures.len(), FIXTURES.len());
        for (file, _) in FIXTURES {
            assert!(manifest.fixtures.iter().any(|item| item == file));
        }
        for (_, raw) in FIXTURES {
            let fixture: Fixture = serde_json::from_str(raw).unwrap();
            assert_eq!(fixture.schema_version, AI_COMPATIBILITY_SCHEMA_VERSION);
            assert!(!fixture.name.trim().is_empty());
        }
    }

    #[test]
    fn golden_fixtures_match_capability_normalization() {
        for (file, raw) in FIXTURES {
            let fixture: Fixture = serde_json::from_str(raw).unwrap();
            let normalized = run_fixture(&fixture);
            let request = &normalized.request;
            for (pointer, expected) in &fixture.expect.paths {
                let actual = request.pointer(pointer).unwrap_or_else(|| {
                    panic!("{file}: missing JSON pointer {pointer}: {request:#}")
                });
                assert_eq!(actual, expected, "{file}: pointer {pointer}");
            }
            for (pointer, expected_len) in &fixture.expect.lengths {
                let actual_len = request
                    .pointer(pointer)
                    .and_then(serde_json::Value::as_array)
                    .map(Vec::len)
                    .unwrap_or_else(|| panic!("{file}: missing array pointer {pointer}"));
                assert_eq!(actual_len, *expected_len, "{file}: length {pointer}");
            }
            for pointer in &fixture.expect.absent_paths {
                assert!(
                    request.pointer(pointer).is_none(),
                    "{file}: pointer should be absent: {pointer}"
                );
            }
            let encoded = request.to_string();
            for forbidden in &fixture.expect.forbidden_substrings {
                assert!(
                    !encoded.contains(forbidden),
                    "{file}: forbidden substring {forbidden}: {encoded}"
                );
            }
            let change_features = normalized
                .changes
                .iter()
                .map(|change| change.feature.as_str())
                .collect::<Vec<_>>();
            for expected in &fixture.expect.changes_include {
                assert!(
                    change_features.contains(&expected.as_str()),
                    "{file}: missing normalization change {expected}; actual={change_features:?}"
                );
            }
        }
    }

    fn run_fixture(fixture: &Fixture) -> NormalizedProviderRequest {
        let matrix = CapabilityMatrixV1::builtin();
        let client = matrix
            .client(&fixture.client)
            .unwrap_or_else(|| panic!("{}: unknown client", fixture.name));
        let provider = matrix
            .provider(&fixture.provider)
            .unwrap_or_else(|| panic!("{}: unknown provider", fixture.name));
        let protocol = AIServiceProtocol::parse(&fixture.protocol).unwrap();
        let translated = match fixture.pipeline {
            Pipeline::Normalize => fixture.input.clone(),
            Pipeline::AnthropicToChat => crate::app::anthropic_openai::request_to_openai(
                &fixture.input,
                &fixture.default_model,
            )
            .unwrap(),
            Pipeline::AnthropicToResponses => {
                crate::app::anthropic_responses::request_to_responses(
                    &fixture.input,
                    &fixture.default_model,
                )
                .unwrap()
            }
            Pipeline::ResponsesToChat => {
                crate::app::responses_chat::request_to_chat(&fixture.input, &fixture.default_model)
                    .unwrap()
            }
            Pipeline::ChatToResponses => crate::app::chat_responses::request_to_responses(
                &fixture.input,
                &fixture.default_model,
            )
            .unwrap(),
        };
        let model = translated
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let rule = matrix.model_rule(&provider.id, model);
        normalize_provider_request(&protocol, translated, client, provider, rule).unwrap()
    }
}
