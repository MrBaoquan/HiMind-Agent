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
            // 降级原因同样要能展示：否则用户只看到"部分功能不可用"却不知道是哪条依赖。
            if resolution.required && resolution.state == "degraded" {
                if let Some(reason) = resolution.reason.as_deref() {
                    reasons.push(format!("capability {}: {reason}", resolution.id));
                }
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
            if resolution.required && resolution.state == "degraded" {
                if let Some(reason) = resolution.reason.as_deref() {
                    reasons.push(format!("plugin {}: {reason}", resolution.id));
                }
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

/// 失败是否仍在"最近"窗口内。健康记录是历史事实，太久远的失败只留给插件卡片排障。
fn recent_plugin_failure(plugin: &crate::capability::plugin::PluginRegistryItem) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default();
    plugin_failure_is_recent(plugin.last_failure_at, now)
}

fn plugin_failure_is_recent(last_failure_at: Option<u64>, now_epoch: u64) -> bool {
    match last_failure_at {
        Some(at) => {
            now_epoch.saturating_sub(at)
                <= crate::capability::plugin::PLUGIN_FAILURE_RECENCY_SECONDS
        }
        // 没有时间戳的失败记录视为最近，保持保守。
        None => true,
    }
}

/// 插件依赖判定所需的全部事实。
///
/// 抽成独立结构，才能对「缺失 / 停用 / 熔断 / 最近失败 / 陈旧失败 / 版本过低」逐一举证，
/// 而不是只能拿真实注册表碰运气。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PluginDependencyFacts {
    version: String,
    enabled: bool,
    circuit_open: bool,
    recent_error: Option<String>,
}

fn plugin_dependency_facts(
    plugin: &crate::capability::plugin::PluginRegistryItem,
    now_epoch: u64,
) -> PluginDependencyFacts {
    PluginDependencyFacts {
        version: plugin.version.clone(),
        enabled: plugin.enabled,
        circuit_open: plugin.circuit_open,
        recent_error: plugin
            .error
            .clone()
            .filter(|_| plugin_failure_is_recent(plugin.last_failure_at, now_epoch)),
    }
}

fn resolve_plugin_dependency_facts(
    dependency: &crate::skill::types::SkillPluginDependency,
    facts: Option<&PluginDependencyFacts>,
    missing_reason: &str,
) -> SkillDependencyResolution {
    let unavailable = |reason: String, version: String| SkillDependencyResolution {
        id: dependency.plugin_id.clone(),
        required: dependency.required,
        state: if dependency.required {
            "blocked".to_string()
        } else {
            "degraded".to_string()
        },
        reason: Some(reason),
        capability_version: Some(version),
        provider: Some("plugin".to_string()),
    };
    let Some(facts) = facts else {
        return SkillDependencyResolution {
            id: dependency.plugin_id.clone(),
            required: dependency.required,
            state: if dependency.required {
                "blocked".to_string()
            } else {
                "degraded".to_string()
            },
            reason: Some(missing_reason.to_string()),
            capability_version: None,
            provider: Some("plugin".to_string()),
        };
    };
    // 只有真的不可用才阻断依赖者：被停用或连续失败已熔断。
    if !facts.enabled {
        return unavailable(
            if facts.circuit_open {
                "plugin is circuit-open after repeated failures".to_string()
            } else {
                "plugin is disabled".to_string()
            },
            facts.version.clone(),
        );
    }
    // 窗口内的失败只降级提示，不阻断——否则一次超限响应就会永久锁死所有依赖者。
    if let Some(error) = facts.recent_error.as_deref() {
        return SkillDependencyResolution {
            id: dependency.plugin_id.clone(),
            required: dependency.required,
            state: "degraded".to_string(),
            reason: Some(format!("最近一次调用失败：{error}")),
            capability_version: Some(facts.version.clone()),
            provider: Some("plugin".to_string()),
        };
    }
    if let Some(min_version) = dependency.min_version.as_deref() {
        if compare_versions(&facts.version, min_version) == Ordering::Less {
            return unavailable(
                format!(
                    "plugin version {} is below minimum {}",
                    facts.version, min_version
                ),
                facts.version.clone(),
            );
        }
    }
    SkillDependencyResolution {
        id: dependency.plugin_id.clone(),
        required: dependency.required,
        state: "ready".to_string(),
        reason: None,
        capability_version: Some(facts.version.clone()),
        provider: Some("plugin".to_string()),
    }
}

fn resolve_plugin_dependency(
    dependency: &crate::skill::types::SkillPluginDependency,
) -> SkillDependencyResolution {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default();
    match crate::capability::plugin::find_plugin(&dependency.plugin_id) {
        Ok(Some(plugin)) => {
            let facts = plugin_dependency_facts(&plugin, now);
            resolve_plugin_dependency_facts(dependency, Some(&facts), "plugin not found")
        }
        Ok(None) => resolve_plugin_dependency_facts(dependency, None, "plugin not found"),
        Err(error) => resolve_plugin_dependency_facts(
            dependency,
            None,
            &format!("plugin lookup failed: {error}"),
        ),
    }
}
fn resolve_dependency(
    dependency: &SkillCapabilityDependency,
    capability_facts: &[CapabilityFact],
) -> SkillDependencyResolution {
    // 内核已经内建的能力（构建、校验、打包、preflight）不该因为「插件没装」而被
    // 判成缺失：能力归属内核之后，插件只是执行器。
    //
    // 只在调用方没有指定提供方、或指定 agent/builtin 时兜底：显式 pin 了插件
    // （provider: com.himind.xxx）的声明仍然要求那个插件 —— 那是作者写下的契约，
    // 内核不替它兜底。
    let pinned = dependency
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let missing_from_facts = !capability_facts.iter().any(|item| item.id == dependency.id);
    let kernel_served = missing_from_facts
        && crate::extension_toolchain::covers(&dependency.id)
        && pinned
            .map(|provider| provider_matches(provider, "builtin"))
            .unwrap_or(true);
    if kernel_served {
        return SkillDependencyResolution {
            id: dependency.id.clone(),
            required: dependency.required,
            state: "ready".to_string(),
            reason: None,
            // 内核实现随 Agent 版本发布，与插件序号不同序：这里只做展示，
            // 不参与 min/max_version 比较（拿插件版本去卡内核实现是用了错误的尺子）。
            capability_version: Some(crate::VERSION.to_string()),
            provider: Some("kernel".to_string()),
        };
    }
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
    let actual_is_builtin = actual == "builtin" || actual.starts_with("builtin:");
    expected.is_empty()
        || expected == actual
        // 内置插件的实现随内核发布，能力事实的 source 只有 "builtin"。清单里
        // pin 了内置插件 ID（例如扩展开发工具）时，必须按同一个身份匹配，
        // 否则能力明明已注册，技能预检仍会报 "missing required capability"。
        || (actual_is_builtin
            && (matches!(expected, "agent" | "builtin")
                || crate::capability::plugin::builtin_plugin_entry(expected).is_some()))
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
            icon: String::new(),
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
        dependency_with_provider(id, required, Some("agent"))
    }

    fn dependency_with_provider(
        id: &str,
        required: bool,
        provider: Option<&str>,
    ) -> SkillCapabilityDependency {
        SkillCapabilityDependency {
            id: id.to_string(),
            required,
            min_version: Some("1.0.0".to_string()),
            max_version: None,
            provider: provider.map(str::to_string),
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
        // 用一条内核与外置插件都不提供的能力：缺失判定本身要有效。
        // （extension.plugin.build 已由内核提供，见下面的内核兜底用例。）
        let readiness = SkillReadiness::resolve(
            &manifest(vec![dependency("com.example.absent.capability", true)]),
            &[],
            "0.3.32",
            "codex",
        );

        assert_eq!(readiness.state, "blocked");
        assert_eq!(
            readiness.reasons,
            vec!["missing required capability: com.example.absent.capability"]
        );
        assert_eq!(readiness.dependencies[0].state, "blocked");
    }

    #[test]
    fn kernel_served_capabilities_satisfy_unpinned_and_agent_dependencies() {
        // 内核内建（extension.plugin.build 等）之后，插件只是执行器：
        // 没声明提供方、或声明 agent/builtin 的能力都应当就绪。
        for capability in [
            dependency_with_provider("extension.plugin.build", true, None),
            dependency_with_provider("extension.plugin.build", true, Some("agent")),
            dependency_with_provider("extension.skill.package", true, Some("builtin")),
        ] {
            let readiness = SkillReadiness::resolve(
                &manifest(vec![capability.clone()]),
                &[],
                "0.3.75",
                "codex",
            );
            assert_eq!(
                readiness.state, "ready",
                "{:?} 应当由内核满足: {:?}",
                capability.id, readiness.reasons
            );
            assert_eq!(
                readiness.dependencies[0].provider.as_deref(),
                Some("kernel")
            );
        }
    }

    #[test]
    fn plugin_pinned_capabilities_still_require_that_plugin() {
        // 显式 pin 了外部插件，就是写下的契约：内核不替它兜底。
        let readiness = SkillReadiness::resolve(
            &manifest(vec![dependency_with_provider(
                "extension.plugin.build",
                true,
                Some("com.example.extension-tools"),
            )]),
            &[],
            "0.3.75",
            "codex",
        );

        assert_eq!(readiness.state, "blocked");
        assert_eq!(
            readiness.reasons,
            vec!["missing required capability: extension.plugin.build"]
        );
    }

    #[test]
    fn pinned_builtin_plugin_id_resolves_to_the_kernel() {
        // 扩展开发工具已内置：清单仍按旧契约 pin 了插件 ID，但内置身份
        // 不会缺席，预检不应把它判成缺少插件。
        let readiness = SkillReadiness::resolve(
            &manifest(vec![dependency_with_provider(
                "extension.plugin.build",
                true,
                Some(crate::extension_toolchain::DEVELOPMENT_TOOLS_PLUGIN_ID),
            )]),
            &[],
            "0.3.75",
            "codex",
        );

        assert_eq!(readiness.state, "ready", "{:?}", readiness.reasons);
        assert_eq!(
            readiness.dependencies[0].provider.as_deref(),
            Some("kernel")
        );
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

    /// 最近窗口内的失败只降级，不阻断；陈旧失败完全忽略。
    /// 这条规则修的是一个真实死锁：一次超限响应曾让两个技能永久"不可用"。
    #[test]
    fn plugin_failure_degrades_only_within_the_recency_window() {
        let now = 1_800_000_000_u64;
        let window = crate::capability::plugin::PLUGIN_FAILURE_RECENCY_SECONDS;

        assert!(plugin_failure_is_recent(Some(now - 60), now));
        assert!(!plugin_failure_is_recent(Some(now - window - 1), now));
        // 没有时间戳视为最近，保持保守。
        assert!(plugin_failure_is_recent(None, now));
    }

    fn plugin_facts(
        enabled: bool,
        circuit_open: bool,
        recent_error: Option<&str>,
    ) -> PluginDependencyFacts {
        PluginDependencyFacts {
            version: "0.3.13".to_string(),
            enabled,
            circuit_open,
            recent_error: recent_error.map(str::to_string),
        }
    }

    fn required_plugin_dependency() -> SkillPluginDependency {
        SkillPluginDependency {
            plugin_id: "com.example.provider".to_string(),
            required: true,
            min_version: Some("0.3.11".to_string()),
        }
    }

    /// 依赖是否阻断只看插件是否真的不可用；"最近失败"只降级提示。
    #[test]
    fn plugin_dependency_blocks_only_when_plugin_is_unavailable() {
        let dependency = required_plugin_dependency();

        // 可用且无失败 → ready
        let ready = plugin_facts(true, false, None);
        assert_eq!(
            resolve_plugin_dependency_facts(&dependency, Some(&ready), "plugin not found").state,
            "ready"
        );

        // 最近失败：降级但**不阻断**（本次修复的核心：一次超限响应不再锁死依赖者）
        let degraded = plugin_facts(true, false, Some("plugin response exceeds limit"));
        let resolution =
            resolve_plugin_dependency_facts(&dependency, Some(&degraded), "plugin not found");
        assert_eq!(resolution.state, "degraded");
        assert!(resolution
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("最近一次调用失败")));

        // 停用 → blocked
        let disabled = plugin_facts(false, false, None);
        assert_eq!(
            resolve_plugin_dependency_facts(&dependency, Some(&disabled), "plugin not found").state,
            "blocked"
        );

        // 连续失败熔断 → blocked，且原因说明是熔断
        let circuit = plugin_facts(false, true, None);
        let resolution =
            resolve_plugin_dependency_facts(&dependency, Some(&circuit), "plugin not found");
        assert_eq!(resolution.state, "blocked");
        assert!(resolution
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("circuit-open")));

        // 插件不存在 → blocked
        assert_eq!(
            resolve_plugin_dependency_facts(&dependency, None, "plugin not found").state,
            "blocked"
        );
    }

    #[test]
    fn plugin_dependency_blocks_provider_below_minimum_version() {
        let dependency = SkillPluginDependency {
            plugin_id: "com.example.provider".to_string(),
            required: true,
            min_version: Some("9.9.9".to_string()),
        };
        let facts = plugin_facts(true, false, None);
        let resolution =
            resolve_plugin_dependency_facts(&dependency, Some(&facts), "plugin not found");
        assert_eq!(resolution.state, "blocked");
        assert!(resolution
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("below minimum")));
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
    fn builtin_plugin_pin_resolves_to_kernel_capabilities() {
        // 扩展开发工具已内置：清单仍按旧契约 pin 了插件 ID，但能力事实的
        // source 是 builtin。两者必须视为同一个身份，否则开发助手 Skill
        // 会在预检里被判成缺少 extension.* 能力。
        let mut skill = manifest(vec![SkillCapabilityDependency {
            id: "extension.plugin.build".to_string(),
            required: true,
            min_version: None,
            max_version: None,
            provider: Some(crate::extension_toolchain::DEVELOPMENT_TOOLS_PLUGIN_ID.to_string()),
        }]);
        skill.plugin_dependencies = Vec::new();

        let readiness = SkillReadiness::resolve(
            &skill,
            &[CapabilityFact {
                id: "extension.plugin.build".to_string(),
                version: crate::extension_toolchain::DEVELOPMENT_TOOLS_PLUGIN_VERSION.to_string(),
                source: "builtin".to_string(),
            }],
            crate::VERSION,
            "codex",
        );

        assert_eq!(readiness.state, "ready", "{:?}", readiness.reasons);
        assert_eq!(readiness.dependencies[0].state, "ready");
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
