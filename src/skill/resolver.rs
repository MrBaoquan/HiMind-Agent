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

        if !manifest.supported_clients.is_empty()
            && !manifest
                .supported_clients
                .iter()
                .any(|item| item.eq_ignore_ascii_case(client_id))
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

fn resolve_dependency(
    dependency: &SkillCapabilityDependency,
    capability_facts: &[CapabilityFact],
) -> SkillDependencyResolution {
    let Some(capability) = capability_facts
        .iter()
        .find(|item| item.id == dependency.id)
    else {
        return SkillDependencyResolution {
            id: dependency.id.clone(),
            required: dependency.required,
            state: if dependency.required {
                "blocked".to_string()
            } else {
                "degraded".to_string()
            },
            reason: Some("capability not found".to_string()),
            capability_version: None,
            provider: dependency.provider.clone(),
        };
    };

    if let Some(min_version) = dependency.min_version.as_ref() {
        if compare_versions(&capability.version, min_version) == Ordering::Less {
            return SkillDependencyResolution {
                id: dependency.id.clone(),
                required: dependency.required,
                state: "blocked".to_string(),
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
                state: "blocked".to_string(),
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
    use crate::skill::types::{SkillCapabilityDependency, SkillManifest, SkillScope};

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
}
