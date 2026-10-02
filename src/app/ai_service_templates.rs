//! 本机 AI 服务预设模板。
//!
//! 事实源是工作台用户级 AI 服务目录（`personal-connections/catalog`，GCMP 供应商
//! 目录投影）。Agent 只在拿到目录后做协议映射与模型筛选，并把结果缓存到本机，
//! 不维护第二份供应商清单；未连接工作台或目录读取失败时回落上次成功缓存，
//! 由前端再回落到内置兜底列表。

use crate::api::ai::{fetch_ai_service_templates, AiProviderTemplate, AiProviderTemplateModel};
use crate::Options;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

const CACHE_DIRECTORY: &str = "data";
const CACHE_FILE: &str = "ai-service-templates.json";

/// 模板来源：工作台目录 / 本机缓存 / 不可用。
pub(crate) const SOURCE_WORKBENCH: &str = "workbench";
pub(crate) const SOURCE_CACHE: &str = "cache";
pub(crate) const SOURCE_UNAVAILABLE: &str = "unavailable";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AiServiceTemplate {
    pub id: String,
    pub name: String,
    pub category: String,
    pub description: String,
    pub base_url: String,
    /// Agent 本机协议名：`openai-chat` / `openai-responses` / `anthropic`。
    pub protocol: String,
    pub default_model: String,
    pub models: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedTemplates {
    #[serde(default)]
    synced_at: String,
    #[serde(default)]
    items: Vec<AiServiceTemplate>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AiServiceTemplateList {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub synced_at: String,
    pub items: Vec<AiServiceTemplate>,
}

/// 工作台协议名 → Agent 本机协议名。
///
/// `async_http` 是工作台的异步媒体执行协议，不是可用于本机推理的接入方式；
/// 协议为空来自旧版工作台，按既有默认 `openai-responses` 处理。
fn protocol_for(value: &str) -> Option<&'static str> {
    match value.trim() {
        "anthropic" => Some("anthropic"),
        "openai_compatible" | "" => Some("openai-responses"),
        _ => None,
    }
}

/// 本机直连供应商时使用上游模型 ID；别名只用于工作台内部路由。
fn model_id(model: &AiProviderTemplateModel) -> &str {
    let upstream = model.upstream_model.trim();
    if upstream.is_empty() {
        model.model_alias.trim()
    } else {
        upstream
    }
}

pub(crate) fn from_catalog(items: &[AiProviderTemplate]) -> Vec<AiServiceTemplate> {
    let mut templates = Vec::new();
    let mut seen = BTreeSet::new();
    for item in items {
        let id = item.id.trim();
        let base_url = item.base_url.trim();
        // 无接入地址的条目（例如工作台的“自定义”占位）不属于预设模板。
        if id.is_empty() || base_url.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        let Some(protocol) = protocol_for(&item.protocol) else {
            continue;
        };
        let mut models: Vec<String> = Vec::new();
        for model in &item.models {
            let id = model_id(model);
            if !id.is_empty() && !models.iter().any(|existing| existing == id) {
                models.push(id.to_string());
            }
        }
        let default_model = item
            .models
            .iter()
            .find(|model| model.recommended && !model_id(model).is_empty())
            .map(model_id)
            .or_else(|| models.first().map(String::as_str))
            .unwrap_or_default()
            .to_string();
        templates.push(AiServiceTemplate {
            id: id.to_string(),
            name: item.name.trim().to_string(),
            category: item.category.trim().to_string(),
            description: item.description.trim().to_string(),
            base_url: base_url.to_string(),
            protocol: protocol.to_string(),
            default_model,
            models,
        });
    }
    templates
}

pub(crate) fn cache_path() -> PathBuf {
    crate::store::paths::agent_home()
        .join(CACHE_DIRECTORY)
        .join(CACHE_FILE)
}

fn load_cache() -> PersistedTemplates {
    let path = cache_path();
    let Ok(content) = fs::read(path) else {
        return PersistedTemplates::default();
    };
    serde_json::from_slice(&content).unwrap_or_default()
}

fn save_cache(items: &[AiServiceTemplate], synced_at: &str) -> Result<(), String> {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let payload = PersistedTemplates {
        synced_at: synced_at.to_string(),
        items: items.to_vec(),
    };
    let content = serde_json::to_vec_pretty(&payload).map_err(|error| error.to_string())?;
    crate::store::atomic_file::atomic_write(&path, &content).map_err(|error| error.to_string())
}

/// 读取模板列表：优先工作台目录，失败回落本机缓存。
///
/// 无论成功失败都返回结构化状态，让 UI 自行决定是否使用内置兜底列表；
/// 目录读取失败不阻塞 AI 服务页其他数据。
pub(crate) fn list(options: &Options) -> AiServiceTemplateList {
    let cached = load_cache();
    if !options.mode().dashboard_enabled() {
        return AiServiceTemplateList {
            source: SOURCE_CACHE.to_string(),
            reason: Some("independent".to_string()),
            synced_at: cached.synced_at,
            items: cached.items,
        };
    }
    match fetch_ai_service_templates(options, "ai-service-templates") {
        Ok(items) => {
            let templates = from_catalog(&items);
            if templates.is_empty() {
                return AiServiceTemplateList {
                    source: if cached.items.is_empty() {
                        SOURCE_UNAVAILABLE
                    } else {
                        SOURCE_CACHE
                    }
                    .to_string(),
                    reason: Some("empty_catalog".to_string()),
                    synced_at: cached.synced_at,
                    items: cached.items,
                };
            }
            let synced_at = crate::app::ai_provider_import::unix_now_seconds().to_string();
            // 缓存写失败不影响本次结果，只是下次无法离线回落。
            let _ = save_cache(&templates, &synced_at);
            AiServiceTemplateList {
                source: SOURCE_WORKBENCH.to_string(),
                reason: None,
                synced_at,
                items: templates,
            }
        }
        Err(error) => AiServiceTemplateList {
            source: if cached.items.is_empty() {
                SOURCE_UNAVAILABLE
            } else {
                SOURCE_CACHE
            }
            .to_string(),
            reason: Some(error.to_string()),
            synced_at: cached.synced_at,
            items: cached.items,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::from_catalog;
    use crate::api::ai::{AiProviderTemplate, AiProviderTemplateModel};

    fn template(id: &str, protocol: &str, base_url: &str) -> AiProviderTemplate {
        AiProviderTemplate {
            id: id.to_string(),
            name: format!("{id} 名称"),
            vendor_name: id.to_string(),
            category: "国内厂商".to_string(),
            description: format!("{id} 描述"),
            service_type: "token".to_string(),
            protocol: protocol.to_string(),
            base_url: base_url.to_string(),
            models: Vec::new(),
        }
    }

    fn model(alias: &str, upstream: &str, recommended: bool) -> AiProviderTemplateModel {
        AiProviderTemplateModel {
            display_name: alias.to_string(),
            model_alias: alias.to_string(),
            upstream_model: upstream.to_string(),
            recommended,
        }
    }

    #[test]
    fn maps_workbench_protocols_to_local_protocols() {
        let mut anthropic = template("kimi", "anthropic", "https://api.moonshot.cn/anthropic");
        anthropic.models = vec![model("kimi-k3", "kimi-k3", true)];
        let mut compatible = template(
            "deepseek",
            "openai_compatible",
            "https://api.deepseek.com/v1",
        );
        compatible.models = vec![
            model("deepseek-flash", "deepseek-flash", true),
            model("deepseek-v4-pro", "deepseek-v4-pro", false),
        ];
        let async_http = template("media", "async_http", "https://media.example/v1");

        let mapped = from_catalog(&[anthropic, compatible, async_http]);
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].protocol, "anthropic");
        assert_eq!(mapped[1].protocol, "openai-responses");
        assert_eq!(mapped[1].default_model, "deepseek-flash");
        assert_eq!(
            mapped[1].models,
            vec!["deepseek-flash".to_string(), "deepseek-v4-pro".to_string()]
        );
    }

    #[test]
    fn skips_entries_without_base_url_and_deduplicates_ids() {
        let custom = template("custom_openai_compatible", "openai_compatible", "");
        let first = template("kimi", "anthropic", "https://api.moonshot.cn/anthropic");
        let duplicate = template("kimi", "anthropic", "https://other.example/anthropic");

        let mapped = from_catalog(&[custom, first, duplicate]);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].base_url, "https://api.moonshot.cn/anthropic");
    }

    #[test]
    fn falls_back_to_model_alias_and_first_model() {
        let mut item = template("openai", "openai_compatible", "https://api.openai.com/v1");
        item.models = vec![
            model("gpt-4.1", "", false),
            model("gpt-4.1", "gpt-4.1", false),
            AiProviderTemplateModel::default(),
        ];

        let mapped = from_catalog(&[item]);
        assert_eq!(mapped[0].models, vec!["gpt-4.1".to_string()]);
        assert_eq!(mapped[0].default_model, "gpt-4.1");
    }
}
