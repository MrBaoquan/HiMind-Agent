//! Invocation selection is distinct from the persisted global route table.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::error::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Selection {
    #[default]
    AgentDefault,
    ServiceDefault {
        service_id: String,
    },
    Pinned {
        service_id: String,
        model: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FallbackPolicy {
    None,
    Configured,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SelectionRequest {
    pub selection: Selection,
    pub fallback: FallbackPolicy,
    /// Advisory preference provenance, never an authorization identity.
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy_model: Option<String>,
}

fn text(input: &Value, key: &str) -> Result<Option<String>, Box<dyn Error>> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok((!value.trim().is_empty()).then(|| value.trim().into())),
        Some(_) => Err(format!("{key} 必须是字符串").into()),
    }
}

fn service_id(value: &str) -> Result<String, Box<dyn Error>> {
    let id = value.trim().strip_prefix("custom:").unwrap_or(value.trim());
    if id.is_empty() || id.len() > 80 {
        return Err("service_id 不能为空且不能超过 80 字节".into());
    }
    Ok(id.into())
}

impl SelectionRequest {
    pub(crate) fn parse(input: &Value) -> Result<Self, Box<dyn Error>> {
        if !input.is_object() {
            return Err("选择参数必须是对象".into());
        }
        let service = text(input, "service")?;
        let model = text(input, "model")?;
        if model.as_ref().is_some_and(|model| model.len() > 160) {
            return Err("model 不能超过 160 字节".into());
        }
        let structured = input.get("selection");
        if structured.is_some() && (service.is_some() || model.is_some()) {
            return Err("selection 与旧 service/model 不能同时指定".into());
        }
        let mut selection = if let Some(value) = structured {
            let fields: &[&str] = match value.get("mode").and_then(Value::as_str) {
                Some("agent_default") => &["mode"],
                Some("service_default") => &["mode", "service_id"],
                Some("pinned") => &["mode", "service_id", "model"],
                _ => return Err("selection.mode 无效".into()),
            };
            if value
                .as_object()
                .is_none_or(|object| object.keys().any(|key| !fields.contains(&key.as_str())))
            {
                return Err("selection 含当前模式不支持的字段".into());
            }
            serde_json::from_value::<Selection>(value.clone())?
        } else {
            match (&service, &model) {
                (Some(service), Some(model)) => Selection::Pinned {
                    service_id: service.clone(),
                    model: model.clone(),
                },
                (Some(service), None) => Selection::ServiceDefault {
                    service_id: service.clone(),
                },
                _ => Selection::AgentDefault,
            }
        };
        match &mut selection {
            Selection::AgentDefault => {}
            Selection::ServiceDefault { service_id: id } => *id = service_id(id)?,
            Selection::Pinned {
                service_id: id,
                model,
            } => {
                *id = service_id(id)?;
                *model = model.trim().into();
                if model.is_empty() || model.len() > 160 {
                    return Err("model 不能为空且不能超过 160 字节".into());
                }
            }
        }
        let legacy_model = (structured.is_none() && service.is_none())
            .then_some(model)
            .flatten();
        let explicit = selection != Selection::AgentDefault || legacy_model.is_some();
        let fallback = match input.get("fallback") {
            Some(value) => serde_json::from_value(value.clone())?,
            None if explicit => FallbackPolicy::None,
            None => FallbackPolicy::Configured,
        };
        let source = text(input, "selection_source")?.unwrap_or_else(|| {
            if explicit || structured.is_some() {
                "invocation".into()
            } else {
                "agent_default".into()
            }
        });
        if !["invocation", "project", "plugin", "agent_default"].contains(&source.as_str()) {
            return Err("selection_source 只支持 invocation/project/plugin/agent_default".into());
        }
        Ok(Self {
            selection,
            fallback,
            source,
            legacy_model,
        })
    }
}

pub(crate) fn selection_schema() -> Value {
    json!({"oneOf": [
        {"type":"object","properties":{"mode":{"const":"agent_default"}},"required":["mode"],"additionalProperties":false},
        {"type":"object","properties":{"mode":{"const":"service_default"},"service_id":{"type":"string","minLength":1,"maxLength":80}},"required":["mode","service_id"],"additionalProperties":false},
        {"type":"object","properties":{"mode":{"const":"pinned"},"service_id":{"type":"string","minLength":1,"maxLength":80},"model":{"type":"string","minLength":1,"maxLength":160}},"required":["mode","service_id","model"],"additionalProperties":false}
    ]})
}

/// Extend every public execution schema with the very same selection contract.
pub(crate) fn extend_schema(mut schema: Value) -> Value {
    let properties = schema["properties"].as_object_mut().expect("object schema");
    properties.insert("selection".into(), selection_schema());
    properties.insert("fallback".into(), json!({"type":"string","enum":["none","configured"],"description":"显式选择默认不回退；configured 明确允许同能力的已配置备用路由"}));
    properties.insert("selection_source".into(), json!({"type":"string","enum":["invocation","project","plugin","agent_default"],"description":"偏好来源，仅作诊断，不表示权限"}));
    schema
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_contract_rejects_conflicts_and_defaults_fallback_conservatively() {
        assert_eq!(
            SelectionRequest::parse(&json!({})).unwrap().fallback,
            FallbackPolicy::Configured
        );
        let pinned =
            SelectionRequest::parse(&json!({"service":"custom:a","model":" second "})).unwrap();
        assert_eq!(
            pinned.selection,
            Selection::Pinned {
                service_id: "a".into(),
                model: "second".into()
            }
        );
        assert_eq!(pinned.fallback, FallbackPolicy::None);
        assert!(SelectionRequest::parse(
            &json!({"selection":{"mode":"agent_default"},"model":"x"})
        )
        .is_err());
        assert!(SelectionRequest::parse(
            &json!({"selection":{"mode":"pinned","service_id":"a","model":" "}})
        )
        .is_err());
        assert!(SelectionRequest::parse(
            &json!({"selection":{"mode":"agent_default","model":"x"}})
        )
        .is_err());
        assert!(SelectionRequest::parse(&json!({"fallback":"surprise"})).is_err());
    }
}
