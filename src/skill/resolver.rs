use crate::skill::clients::manifest_supports_client;
use crate::skill::types::{SkillCapabilityDependency, SkillManifest};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CapabilityFact {
    pub id: String,
    pub version: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillDependencyResolution {
    pub id: String,
    pub required: bool,
    pub state: String,
    pub reason: Option<String>,
    pub capability_version: Option<String>,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillReadiness {
    pub state: String,
    pub reasons: Vec<String>,
    pub dependencies: Vec<SkillDependencyResolution>,
}

impl SkillReadiness {
    pub(crate) fn resolve(
        manifest: &SkillManifest,
        capability_facts: &[CapabilityFact],
        agent_version: &str,
        client_id: &str,
    ) -> Self {
        let mut state = "ready".to_string();
        let mut reasons = Vec::new();
        let mut dependencies = Vec::new();

        if !manifest.supported_clients.is_empty() && !manifest_supports_client(manifest, client_id)
        {
            state = "blocked".to_string();
            reasons.push(format!("unsupported client: {client_id}"));
        }

        if !manifest.min_agent_version.trim().is_empty()
            && compare_versions(agent_version, &manifest.min_agent_version) == Ordering::Less
        {
            state = "blocked".to_string();
            reasons.push(format!(
                "agent version {agent_version} does not satisfy minimum {}",
                manifest.min_agent_version
            ));
        }

        for dependency in &manifest.capabilities {
            let resolution = resolve_dependency(dependency, capability_facts);
            if resolution.required && resolution.state == "blocked" {
                state = "blocked".to_string();
                reasons.push(format!("missing required capability: {}", resolution.id));
            } else if resolution.required && resolution.state == "degraded" && state == "ready" {
                state = "degraded".to_string();
            }
            dependencies.push(resolution);
        }

        // Plugin dependencies are part of the Skill runtime contract as well
        // as a candidate-time check. Resolve them here so every client
        // adapter reports the same readiness state before rendering a Skill.
        for dependency in &manifest.plugin_dependencies {
            let resolution = resolve_plugin_dependency(dependency);
            if resolution.required && resolution.state == "blocked" {
                state = "blocked".to_string();
                reasons.push(format!("missing required plugin: {}", resolution.id));
            } else if resolution.required && resolution.state == "degraded" && state == "ready" {
                state = "degraded".to_string();
            }
            dependencies.push(resolution);
        }

        if !reasons.is_empty() && state == "ready" {
            state = "degraded".to_string();
        }
        Self {
            state,
            reasons,
            dependencies,
        }
    }
}

fn resolve_plugin_dependency(
    dependency: &crate::skill::types::SkillPluginDependency,
) -> SkillDependencyResolution {
    let found = crate::capability::plugin::find_plugin(&dependency.plugin_id);
    let plugin = match found {
        Ok(Some(plugin)) => plugin,
        Ok(None) => {
            return SkillDependencyResolution {
                id: dependency.plugin_id.clone(),
                required: dependency.required,
                state: if dependency.required {
                    "blocked".to_string()
                } else {
                    "degraded".to_string()
                },
                reason: Some("plugin not found".to_string()),
                capability_version: None,
                provider: Some("plugin".to_string()),
            }
        }
        Err(error) => {
            return SkillDependencyResolution {
                id: dependency.plugin_id.clone(),
                required: dependency.required,
                state: if dependency.required {
                    "blocked".to_string()
                } else {
                    "degraded".to_string()
                },
                reason: Some(format!("plugin lookup failed: {error}")),
                capability_version: None,
                provider: Some("plugin".to_string()),
            }
        }
    };
    if !plugin.enabled || plugin.error.is_some() {
        return SkillDependencyResolution {
            id: dependency.plugin_id.clone(),
            required: dependency.required,
            state: if dependency.required {
                "blocked".to_string()
            } else {
                "degraded".to_string()
            },
            reason: Some("plugin unavailable".to_string()),
            capability_version: Some(plugin.version),
            provider: Some("plugin".to_string()),
        };
    }
    if let Some(min_version) = dependency.min_version.as_deref() {
        if compare_versions(&plugin.version, min_version) == Ordering::Less {
            return SkillDependencyResolution {
                id: dependency.plugin_id.clone(),
                required: dependency.required,
                state: if dependency.required {
                    "blocked".to_string()
                } else {
                    "degraded".to_string()
                },
                reason: Some(format!(
                    "plugin version {} is below minimum {}",
                    plugin.version, min_version
                )),
                capability_version: Some(plugin.version),
                provider: Some("plugin".to_string()),
            };
        }
    }
    SkillDependencyResolution {
        id: dependency.plugin_id.clone(),
        required: dependency.required,
        state: "ready".to_string(),
        reason: None,
        capability_version: Some(plugin.version),
        provider: Some("plugin".to_string()),
    }
}

fn resolve_dependency(
    dependency: &SkillCapabilityDependency,
    capability_facts: &[CapabilityFact],
) -> SkillDependencyResolution {
    let matching_id = capability_facts
        .iter()
        .find(|item| item.id == dependency.id);
    let Some(capability) = matching_id.filter(|item| {
        dependency
            .provider
            .as_deref()
            .map(|provider| provider_matches(provider, &item.source))
            .unwrap_or(true)
    }) else {
        let reason =
            if let (Some(provider), Some(actual)) = (dependency.provider.as_deref(), matching_id) {
                format!(
                    "capability provider {} does not satisfy {}",
                    actual.source, provider
                )
            } else {
                "capability not found".to_string()
            };
        return SkillDependencyResolution {
            id: dependency.id.clone(),
            required: dependency.required,
            state: if dependency.required {
                "blocked".to_string()
            } else {
                "degraded".to_string()
            },
            reason: Some(reason),
            capability_version: None,
            provider: dependency.provider.clone(),
        };
    };

    if let Some(min_version) = dependency.min_version.as_ref() {
        if compare_versions(&capability.version, min_version) == Ordering::Less {
            return SkillDependencyResolution {
                id: dependency.id.clone(),
                required: dependency.required,
                state: if dependency.required {
                    "blocked".to_string()
                } else {
                    "degraded".to_string()
                },
                reason: Some(format!(
                    "capability version {} is below minimum {}",
                    capability.version, min_version
                )),
                capability_version: Some(capability.version.clone()),
                provider: dependency.provider.clone(),
            };
        }
    }
    if let Some(max_version) = dependency.max_version.as_ref() {
        if compare_versions(&capability.version, max_version) == Ordering::Greater {
            return SkillDependencyResolution {
                id: dependency.id.clone(),
                required: dependency.required,
                state: if dependency.required {
                    "blocked".to_string()
                } else {
                    "degraded".to_string()
                },
                reason: Some(format!(
                    "capability version {} exceeds maximum {}",
                    capability.version, max_version
                )),
                capability_version: Some(capability.version.clone()),
                provider: dependency.provider.clone(),
            };
        }
    }

    SkillDependencyResolution {
        id: dependency.id.clone(),
        required: dependency.required,
        state: "ready".to_string(),
        reason: None,
        capability_version: Some(capability.version.clone()),
        provider: dependency.provider.clone(),
    }
}

fn provider_matches(expected: &str, actual: &str) -> bool {
    let expected = expected.trim();
    expected.is_empty()
        || expected == actual
        || (matches!(expected, "agent" | "builtin")
            && (actual == "builtin" || actual.starts_with("builtin:")))
        || actual == format!("plugin:{expected}")
}

pub(crate) fn compare_versions(left: &str, right: &str) -> Ordering {
    let parse = |value: &str| {
        value
            .split(['.', '-', '+'])
            .take(3)
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let left = parse(left);
    let right = parse(right);
    for index in 0..3 {
        match left
            .get(index)
            .unwrap_or(&0)
            .cmp(right.get(index).unwrap_or(&0))
        {
            Ordering::Less => return Ordering::Less,
            Ordering::Greater => return Ordering::Greater,
            Ordering::Equal => {}
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::types::{
        SkillCapabilityDependency, SkillManifest, SkillPluginDependency, SkillScope,
    };

    fn manifest(capabilities: Vec<SkillCapabilityDependency>) -> SkillManifest {
        SkillManifest {
            id: "com.himind.skill.test".to_string(),
            name: "测试技能".to_string(),
            author: "测试作者".to_string(),
            categories: Vec::new(),
            version: "1.0.0".to_string(),
            scope: SkillScope::User,
            description: String::new(),
            release_notes: String::new(),
            min_agent_version: "0.3.0".to_string(),
            supported_clients: vec!["codex".to_string()],
            capabilities,
            plugin_dependencies: Vec::new(),
            risk_summary: String::new(),
            contents: Vec::new(),
        }
    }

    fn dependency(id: &str, required: bool) -> SkillCapabilityDependency {
        SkillCapabilityDependency {
            id: id.to_string(),
            required,
            min_version: Some("1.0.0".to_string()),
            max_version: None,
            provider: Some("agent".to_string()),
        }
    }

    #[test]
    fn missing_optional_capability_does_not_degrade_skill_readiness() {
        let readiness = SkillReadiness::resolve(
            &manifest(vec![dependency("extension.submission.submit", false)]),
            &[],
            "0.3.32",
            "codex",
        );

        assert_eq!(readiness.state, "ready");
        assert!(readiness.reasons.is_empty());
        assert_eq!(readiness.dependencies[0].state, "degraded");
        assert!(!readiness.dependencies[0].required);
    }

    #[test]
    fn missing_required_capability_blocks_skill_readiness() {
        let readiness = SkillReadiness::resolve(
            &manifest(vec![dependency("extension.plugin.build", true)]),
            &[],
            "0.3.32",
            "codex",
        );

        assert_eq!(readiness.state, "blocked");
        assert_eq!(
            readiness.reasons,
            vec!["missing required capability: extension.plugin.build"]
        );
        assert_eq!(readiness.dependencies[0].state, "blocked");
    }

    #[test]
    fn missing_required_plugin_blocks_skill_readiness() {
        let mut skill = manifest(Vec::new());
        skill.plugin_dependencies = vec![SkillPluginDependency {
            plugin_id: "com.example.missing".to_string(),
            required: true,
            min_version: Some("1.0.0".to_string()),
        }];
        let readiness = SkillReadiness::resolve(&skill, &[], "0.3.32", "codex");

        assert_eq!(readiness.state, "blocked");
        assert!(readiness
            .reasons
            .iter()
            .any(|reason| reason.contains("com.example.missing")));
        assert_eq!(
            readiness.dependencies[0].provider.as_deref(),
            Some("plugin")
        );
    }

    #[test]
    fn missing_optional_plugin_does_not_block_skill_readiness() {
        let mut skill = manifest(Vec::new());
        skill.plugin_dependencies = vec![SkillPluginDependency {
            plugin_id: "com.example.optional".to_string(),
            required: false,
            min_version: None,
        }];
        let readiness = SkillReadiness::resolve(&skill, &[], "0.3.32", "codex");

        assert_eq!(readiness.state, "ready");
        assert_eq!(readiness.dependencies[0].state, "degraded");
    }

    #[test]
    fn optional_dependency_version_mismatch_degrades_without_blocking() {
        let mut skill = manifest(vec![SkillCapabilityDependency {
            id: "example.inspect".to_string(),
            required: false,
            min_version: Some("2.0.0".to_string()),
            max_version: None,
            provider: Some("agent".to_string()),
        }]);
        skill.plugin_dependencies = vec![SkillPluginDependency {
            plugin_id: "com.example.optional".to_string(),
            required: false,
            min_version: Some("2.0.0".to_string()),
        }];

        let readiness = SkillReadiness::resolve(
            &skill,
            &[CapabilityFact {
                id: "example.inspect".to_string(),
                version: "1.0.0".to_string(),
                source: "builtin".to_string(),
            }],
            "0.3.32",
            "codex",
        );

        assert_eq!(readiness.state, "ready");
        assert!(readiness
            .dependencies
            .iter()
            .all(|dependency| dependency.state != "blocked"));
        assert!(readiness
            .dependencies
            .iter()
            .all(|dependency| dependency.state == "degraded"));
    }

    #[test]
    fn capability_provider_is_part_of_the_dependency_contract() {
        let skill = manifest(vec![dependency("example.inspect", true)]);
        let wrong = SkillReadiness::resolve(
            &skill,
            &[CapabilityFact {
                id: "example.inspect".to_string(),
                version: "1.0.0".to_string(),
                source: "plugin:com.example.other".to_string(),
            }],
            "0.3.32",
            "codex",
        );
        assert_eq!(wrong.state, "blocked");
        assert!(wrong.dependencies[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("does not satisfy")));

        let ready = SkillReadiness::resolve(
            &skill,
            &[CapabilityFact {
                id: "example.inspect".to_string(),
                version: "1.0.0".to_string(),
                source: "builtin".to_string(),
            }],
            "0.3.32",
            "codex",
        );
        assert_eq!(ready.state, "ready");
    }
}
