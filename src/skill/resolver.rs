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

        let mut missing_optional = 0_usize;
        for dependency in &manifest.capabilities {
            let resolution = resolve_dependency(dependency, capability_facts);
            if resolution.state == "blocked" {
                state = "blocked".to_string();
                if resolution.required {
                    reasons.push(format!("missing required capability: {}", resolution.id));
                } else {
                    missing_optional += 1;
                }
            } else if resolution.state == "degraded" && state == "ready" {
                state = "degraded".to_string();
                missing_optional += 1;
            }
            dependencies.push(resolution);
        }

        if !reasons.is_empty() && state == "ready" {
            state = "degraded".to_string();
        }
        if missing_optional > 0 && state == "ready" {
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
