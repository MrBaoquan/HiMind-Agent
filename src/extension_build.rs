use crate::extension_contracts::ExtensionAssetKind;
use crate::extension_projects::{ExtensionBuildDependency, ExtensionProject, ExtensionProjectKind};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::error::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtensionBuildPlan {
    pub nodes: Vec<ExtensionBuildNode>,
    pub order: Vec<String>,
    pub reused: Vec<ExtensionBuildReuse>,
    pub blockers: Vec<ExtensionBuildBlocker>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtensionBuildNode {
    pub project_id: String,
    pub kind: ExtensionProjectKind,
    pub extension_id: String,
    pub name: String,
    pub version: String,
    pub workspace_path: String,
    pub dependencies: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtensionBuildReuse {
    pub kind: ExtensionAssetKind,
    pub id: String,
    pub version: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtensionBuildBlocker {
    pub code: String,
    pub project_id: String,
    pub dependency_kind: String,
    pub dependency_id: String,
    pub message: String,
    pub remediation: String,
}

pub(crate) fn plan(project_ids: &[String]) -> Result<ExtensionBuildPlan, Box<dyn Error>> {
    let projects = crate::extension_projects::list()?;
    Ok(plan_from_projects(&projects, project_ids))
}

fn plan_from_projects(projects: &[ExtensionProject], project_ids: &[String]) -> ExtensionBuildPlan {
    let by_id: HashMap<&str, &ExtensionProject> = projects
        .iter()
        .map(|project| (project.id.as_str(), project))
        .collect();
    let by_extension: HashMap<&str, &ExtensionProject> = projects
        .iter()
        .map(|project| (project.extension_id.as_str(), project))
        .collect();
    let mut planner = Planner {
        by_extension: by_extension.clone(),
        visited: HashSet::new(),
        visiting: HashSet::new(),
        stack: Vec::new(),
        nodes: HashMap::new(),
        order: Vec::new(),
        reused: Vec::new(),
        blocker_keys: BTreeSet::new(),
        blockers: Vec::new(),
        warnings: Vec::new(),
    };
    for id in project_ids {
        let project = by_id
            .get(id.as_str())
            .or_else(|| by_extension.get(id.as_str()));
        let Some(project) = project else {
            planner.block(
                "target_not_found",
                id,
                "",
                "",
                format!("未找到扩展项目 {id}"),
                "刷新扩展工作区后重试",
            );
            continue;
        };
        if !project.workspace_available {
            planner.block(
                "target_source_missing",
                &project.id,
                project.kind.as_str(),
                &project.extension_id,
                format!("扩展 {} 的源码目录当前不可用", project.name),
                "恢复源码目录或重新绑定扩展项目",
            );
            continue;
        }
        planner.visit(project);
    }
    let nodes = planner
        .order
        .iter()
        .filter_map(|id| planner.nodes.get(id).cloned())
        .collect();
    ExtensionBuildPlan {
        nodes,
        order: planner.order,
        reused: planner.reused,
        blockers: planner.blockers,
        warnings: planner.warnings,
    }
}

struct Planner<'a> {
    by_extension: HashMap<&'a str, &'a ExtensionProject>,
    visited: HashSet<String>,
    visiting: HashSet<String>,
    stack: Vec<String>,
    nodes: HashMap<String, ExtensionBuildNode>,
    order: Vec<String>,
    reused: Vec<ExtensionBuildReuse>,
    blocker_keys: BTreeSet<String>,
    blockers: Vec<ExtensionBuildBlocker>,
    warnings: Vec<String>,
}

impl Planner<'_> {
    fn visit(&mut self, project: &ExtensionProject) -> bool {
        if self.visited.contains(&project.id) {
            return true;
        }
        if self.visiting.contains(&project.id) {
            let start = self
                .stack
                .iter()
                .position(|id| id == &project.id)
                .unwrap_or(0);
            let mut cycle = self.stack[start..].to_vec();
            cycle.push(project.id.clone());
            self.block(
                "dependency_cycle",
                &project.id,
                project.kind.as_str(),
                &project.extension_id,
                format!("检测到循环依赖: {}", cycle.join(" -> ")),
                "拆分循环依赖后重试",
            );
            return false;
        }

        self.visiting.insert(project.id.clone());
        self.stack.push(project.id.clone());
        let mut dependencies = Vec::new();
        let mut node_warnings = Vec::new();
        let mut ready = true;
        for dependency in project
            .build_dependencies
            .iter()
            .filter(|dependency| dependency.required)
        {
            if !self.resolve_dependency(project, dependency, &mut dependencies, &mut node_warnings)
            {
                ready = false;
            }
        }
        for dependency in project
            .build_dependencies
            .iter()
            .filter(|dependency| !dependency.required)
        {
            self.warn_optional_dependency(project, dependency);
        }
        self.stack.pop();
        self.visiting.remove(&project.id);
        if !ready {
            return false;
        }

        self.visited.insert(project.id.clone());
        self.order.push(project.id.clone());
        self.nodes.insert(
            project.id.clone(),
            ExtensionBuildNode {
                project_id: project.id.clone(),
                kind: project.kind,
                extension_id: project.extension_id.clone(),
                name: project.name.clone(),
                version: project.version.clone(),
                workspace_path: project.workspace_path.clone(),
                dependencies,
                warnings: node_warnings,
            },
        );
        true
    }

    fn resolve_dependency(
        &mut self,
        project: &ExtensionProject,
        dependency: &ExtensionBuildDependency,
        dependencies: &mut Vec<String>,
        node_warnings: &mut Vec<String>,
    ) -> bool {
        if let Some(local) = self.by_extension.get(dependency.id.as_str()).copied() {
            if local.workspace_available {
                if let Some(warning) =
                    version_warning(&dependency.id, &local.version, &dependency.min_version)
                {
                    self.push_warning(node_warnings, warning);
                }
                if self.visit(local) {
                    dependencies.push(local.extension_id.clone());
                    return true;
                }
                self.block(
                    "dependency_build_blocked",
                    &project.id,
                    dependency.kind.as_str(),
                    &dependency.id,
                    format!("依赖 {} 无法加入自动构建队列", dependency.id),
                    "先处理依赖自身的构建错误后重试",
                );
                return false;
            }
        }

        match resolved_dependency_version(dependency) {
            Ok(Some(version)) if version_meets(&version, &dependency.min_version) => {
                self.reused.push(ExtensionBuildReuse {
                    kind: dependency.kind,
                    id: dependency.id.clone(),
                    version,
                    required: dependency.required,
                });
                true
            }
            Ok(Some(version)) => {
                self.block(
                    "dependency_version_unmet",
                    &project.id,
                    dependency.kind.as_str(),
                    &dependency.id,
                    format!(
                        "依赖 {} 的源码不可用，当前有效版本 {} 低于最低版本 {}",
                        dependency.id, version, dependency.min_version
                    ),
                    "恢复依赖源码目录，或安装满足最低版本的扩展",
                );
                false
            }
            Ok(None) => {
                self.block(
                    "dependency_unresolved",
                    &project.id,
                    dependency.kind.as_str(),
                    &dependency.id,
                    format!("依赖 {} 的源码和可用安装版本都不存在", dependency.id),
                    "登记包含该依赖的本地扩展源后重试",
                );
                false
            }
            Err(error) => {
                self.block(
                    "dependency_probe_failed",
                    &project.id,
                    dependency.kind.as_str(),
                    &dependency.id,
                    format!("读取依赖 {} 的当前版本失败: {error}", dependency.id),
                    "检查扩展安装状态后重试",
                );
                false
            }
        }
    }

    fn warn_optional_dependency(
        &mut self,
        project: &ExtensionProject,
        dependency: &ExtensionBuildDependency,
    ) {
        if let Some(local) = self.by_extension.get(dependency.id.as_str()) {
            if local.workspace_available {
                if let Some(warning) =
                    version_warning(&dependency.id, &local.version, &dependency.min_version)
                {
                    let message = format!("可选依赖 {}: {}", project.name, warning);
                    self.push_global_warning(message);
                }
                return;
            }
        }
        if let Ok(Some(version)) = resolved_dependency_version(dependency) {
            if !version_meets(&version, &dependency.min_version) {
                self.push_global_warning(format!(
                    "可选依赖 {} 当前版本 {} 低于建议版本 {}",
                    dependency.id, version, dependency.min_version
                ));
            }
        }
    }

    fn push_warning(&mut self, node_warnings: &mut Vec<String>, warning: String) {
        if !node_warnings.contains(&warning) {
            node_warnings.push(warning.clone());
        }
        self.push_global_warning(warning);
    }

    fn push_global_warning(&mut self, warning: String) {
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }

    fn block(
        &mut self,
        code: &str,
        project_id: &str,
        dependency_kind: &str,
        dependency_id: &str,
        message: String,
        remediation: &str,
    ) {
        let key = format!("{code}:{project_id}:{dependency_kind}:{dependency_id}:{message}");
        if !self.blocker_keys.insert(key) {
            return;
        }
        self.blockers.push(ExtensionBuildBlocker {
            code: code.to_string(),
            project_id: project_id.to_string(),
            dependency_kind: dependency_kind.to_string(),
            dependency_id: dependency_id.to_string(),
            message,
            remediation: remediation.to_string(),
        });
    }
}

fn resolved_dependency_version(
    dependency: &ExtensionBuildDependency,
) -> Result<Option<String>, Box<dyn Error>> {
    match dependency.kind {
        ExtensionAssetKind::Plugin => Ok(crate::capability::plugin::find_plugin(&dependency.id)?
            .filter(|plugin| plugin.enabled && plugin.error.is_none())
            .map(|plugin| plugin.version)),
        ExtensionAssetKind::Skill => Ok(crate::skill::store::SkillStore::new()
            .get_record(&dependency.id)?
            .map(|record| record.manifest.version)),
        ExtensionAssetKind::Workflow => Ok(None),
    }
}

fn version_warning(id: &str, actual: &str, minimum: &str) -> Option<String> {
    if minimum.trim().is_empty() {
        return None;
    }
    let (Ok(actual), Ok(minimum)) = (
        Version::parse(actual.trim()),
        Version::parse(minimum.trim()),
    ) else {
        return None;
    };
    (actual < minimum).then(|| {
        format!(
            "依赖 {id} 的本地源码版本 {actual} 低于声明最低版本 {minimum}；本地构建将使用当前源码，发布前仍需修正版本"
        )
    })
}

fn version_meets(actual: &str, minimum: &str) -> bool {
    if minimum.trim().is_empty() {
        return true;
    }
    let (Ok(actual), Ok(minimum)) = (
        Version::parse(actual.trim()),
        Version::parse(minimum.trim()),
    ) else {
        return false;
    };
    actual >= minimum
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_projects::ExtensionCandidate;
    use crate::skill::resolver::CapabilityFact;

    fn capability_fact(id: &str, version: &str, provider: &str) -> CapabilityFact {
        CapabilityFact {
            id: id.to_string(),
            version: version.to_string(),
            source: provider.to_string(),
        }
    }

    fn dependency(
        kind: ExtensionAssetKind,
        id: &str,
        min_version: &str,
        required: bool,
    ) -> ExtensionBuildDependency {
        ExtensionBuildDependency {
            kind,
            id: id.to_string(),
            min_version: min_version.to_string(),
            required,
        }
    }

    fn project(
        id: &str,
        name: &str,
        kind: ExtensionProjectKind,
        version: &str,
        dependencies: Vec<ExtensionBuildDependency>,
    ) -> ExtensionProject {
        ExtensionProject {
            id: format!("{}:{id}", kind.as_str()),
            kind,
            extension_id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            version: version.to_string(),
            workspace_path: format!("C:/test/{id}"),
            workspace_available: true,
            icon: None,
            source: "test".to_string(),
            source_repository: String::new(),
            source_default_branch: "main".to_string(),
            source_subdirectory: String::new(),
            source_commit: String::new(),
            distribution_targets: Vec::new(),
            distribution_targets_declared: Vec::new(),
            distribution_targets_source: "default".to_string(),
            plugin_dependencies: Vec::new(),
            extension_dependencies: Vec::new(),
            build_dependencies: dependencies,
            updated_at: "0".to_string(),
            source_unit_key: String::new(),
        }
    }

    #[test]
    fn plan_orders_local_plugin_and_skill_dependencies_before_target() {
        let projects = vec![
            project(
                "workflow.target",
                "目标工作流",
                ExtensionProjectKind::Workflow,
                "1.0.0",
                vec![
                    dependency(ExtensionAssetKind::Skill, "skill.dep", "0.4.2", true),
                    dependency(ExtensionAssetKind::Plugin, "plugin.dep", "0.5.2", true),
                ],
            ),
            project(
                "skill.dep",
                "依赖技能",
                ExtensionProjectKind::Skill,
                "0.4.2",
                Vec::new(),
            ),
            project(
                "plugin.dep",
                "依赖插件",
                ExtensionProjectKind::Plugin,
                "0.5.2",
                Vec::new(),
            ),
        ];
        let plan = plan_from_projects(&projects, &["workflow:workflow.target".to_string()]);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            plan.order,
            vec![
                "skill:skill.dep".to_string(),
                "plugin:plugin.dep".to_string(),
                "workflow:workflow.target".to_string()
            ]
        );
        let target = plan
            .nodes
            .iter()
            .find(|node| node.extension_id == "workflow.target")
            .unwrap();
        assert_eq!(
            target.dependencies,
            vec!["skill.dep".to_string(), "plugin.dep".to_string()]
        );
    }

    #[test]
    #[ignore = "reads the current Agent home and requires the registered himind-ext-projects workspace"]
    fn current_agent_home_plans_full_exhibit_workspace() {
        let projects = crate::extension_projects::list().unwrap();
        let targets: Vec<_> = projects
            .iter()
            .filter(|project| {
                project
                    .workspace_path
                    .replace('\\', "/")
                    .contains("/himind-ext-projects/")
            })
            .map(|project| project.id.clone())
            .collect();
        assert_eq!(targets.len(), 8, "unexpected project count: {targets:?}");
        let planned = plan(&targets).unwrap();
        assert!(planned.blockers.is_empty(), "{:?}", planned.blockers);
        for target in &targets {
            assert!(
                planned.order.contains(target),
                "missing planned target {target}"
            );
        }
    }

    #[test]
    #[ignore = "reads the current Agent home and requires the registered video-to-motion workspace"]
    fn current_agent_home_plans_video_to_motion_dependencies() {
        let projects = crate::extension_projects::list().unwrap();
        let workflow = projects
            .iter()
            .find(|project| project.extension_id == "com.himind.workflow.video-to-motion")
            .expect("missing video-to-motion project");
        let planned = plan(&[workflow.id.clone()]).unwrap();
        assert!(planned.blockers.is_empty(), "{:?}", planned.blockers);
        assert!(planned
            .nodes
            .iter()
            .any(|node| node.extension_id == "com.himind.motion-composer"));
    }

    #[test]
    #[ignore = "requires HIMIND_EXT_E2E_ROOT pointing at himind-ext-projects"]
    fn external_workspace_plan_orders_exhibit_web_dependencies() {
        let _guard = crate::store::paths::test_env_lock();
        let source_root = std::path::PathBuf::from(
            std::env::var("HIMIND_EXT_E2E_ROOT")
                .expect("set HIMIND_EXT_E2E_ROOT to the extension workspace"),
        );
        let previous_home = std::env::var_os("HIMIND_AGENT_HOME");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let staging = std::env::temp_dir().join(format!(
            "himind-build-plan-e2e-{}-{nonce}",
            std::process::id()
        ));
        std::env::set_var("HIMIND_AGENT_HOME", staging.join("home"));
        let plugin = crate::extension_projects::register(
            &source_root.join("plugins/exhibit-development-tools"),
        )
        .unwrap();
        let skill = crate::extension_projects::register(
            &source_root.join("skills/exhibit-web-development"),
        )
        .unwrap();
        let experience = crate::extension_projects::register(
            &source_root.join("skills/exhibit-experience-direction"),
        )
        .unwrap();
        let workflow = crate::extension_projects::register(
            &source_root.join("workflows/exhibit-web-development"),
        )
        .unwrap();
        let planned = plan(&[workflow.id.clone()]).unwrap();
        assert!(planned.blockers.is_empty(), "{:?}", planned.blockers);
        let position = |id: &str| planned.order.iter().position(|item| item == id).unwrap();
        assert!(position(&plugin.id) < position(&workflow.id));
        assert!(position(&skill.id) < position(&workflow.id));
        assert!(position(&experience.id) < position(&workflow.id));
        match previous_home {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = std::fs::remove_dir_all(staging);
    }

    #[test]
    #[ignore = "requires HIMIND_EXT_E2E_ROOT and builds the real exhibit web extension"]
    fn external_workspace_plan_builds_and_activates_exhibit_web_dependencies() {
        let _guard = crate::store::paths::test_env_lock();
        let source_root = std::path::PathBuf::from(
            std::env::var("HIMIND_EXT_E2E_ROOT")
                .expect("set HIMIND_EXT_E2E_ROOT to the extension workspace"),
        );
        let keys = [
            "HIMIND_AGENT_HOME",
            "USERPROFILE",
            "HOME",
            "HIMIND_CODEX_SKILL_DIR",
            "HIMIND_SKILL_DEVELOPMENT_REGISTRY",
            "HIMIND_SKILL_DEVELOPMENT_VERSIONS_DIR",
            "HIMIND_PLUGIN_DEVELOPMENT_REGISTRY",
        ];
        let previous: Vec<_> = keys
            .iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let staging = std::env::temp_dir().join(format!(
            "himind-build-execute-e2e-{}-{nonce}",
            std::process::id()
        ));
        std::env::set_var("HIMIND_AGENT_HOME", staging.join("home"));
        std::env::set_var("USERPROFILE", staging.join("userprofile"));
        std::env::set_var("HOME", staging.join("userprofile"));
        std::env::set_var("HIMIND_CODEX_SKILL_DIR", staging.join("codex-skills"));
        std::env::set_var(
            "HIMIND_SKILL_DEVELOPMENT_REGISTRY",
            staging.join("home/skill-development.json"),
        );
        std::env::set_var(
            "HIMIND_SKILL_DEVELOPMENT_VERSIONS_DIR",
            staging.join("home/skill-development-versions"),
        );
        std::env::set_var(
            "HIMIND_PLUGIN_DEVELOPMENT_REGISTRY",
            staging.join("home/plugin-development.json"),
        );
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            for path in [
                "plugins/exhibit-development-tools",
                "skills/exhibit-web-development",
                "skills/exhibit-experience-direction",
                "workflows/exhibit-web-development",
            ] {
                crate::extension_projects::register(&source_root.join(path))?;
            }
            let workflow = crate::extension_projects::list()?
                .into_iter()
                .find(|project| {
                    project.extension_id == "com.himind.workflow.exhibit-web-development"
                })
                .ok_or("missing exhibit-web-development project")?;
            let planned = plan(&[workflow.id.clone()])?;
            if !planned.blockers.is_empty() {
                return Err(format!("build plan blocked: {:?}", planned.blockers).into());
            }
            for node in planned.nodes {
                match node.kind {
                    ExtensionProjectKind::Plugin => {
                        let candidate = crate::extension_projects::build(&node.project_id)?;
                        let ExtensionCandidate::Plugin(draft) = candidate else {
                            return Err("expected plugin candidate".into());
                        };
                        let tested = crate::plugin_authoring::test(
                            &draft.manifest.id,
                            &draft.manifest.version,
                        )?;
                        crate::plugin_authoring::confirm(
                            &tested.manifest.id,
                            &tested.manifest.version,
                        )?;
                        crate::plugin_authoring::activate(
                            &tested.manifest.id,
                            &tested.manifest.version,
                        )?;
                    }
                    ExtensionProjectKind::Skill => {
                        let candidate = crate::extension_projects::build(&node.project_id)?;
                        let ExtensionCandidate::Skill(draft) = candidate else {
                            return Err("expected skill candidate".into());
                        };
                        let facts: Vec<_> = draft
                            .manifest
                            .capabilities
                            .iter()
                            .map(|capability| {
                                capability_fact(
                                    &capability.id,
                                    capability.min_version.as_deref().unwrap_or("1.0.0"),
                                    capability.provider.as_deref().unwrap_or("test-provider"),
                                )
                            })
                            .collect();
                        let tested = crate::skill::authoring::test(
                            &draft.manifest.id,
                            &draft.manifest.version,
                            &facts,
                        )?;
                        crate::skill::authoring::confirm(
                            &tested.draft.manifest.id,
                            &tested.draft.manifest.version,
                        )?;
                        crate::skill::authoring::activate(
                            &tested.draft.manifest.id,
                            &tested.draft.manifest.version,
                            &facts,
                        )?;
                    }
                    ExtensionProjectKind::Workflow => {
                        let candidate = crate::extension_projects::build(&node.project_id)?;
                        let ExtensionCandidate::Workflow(draft) = candidate else {
                            return Err("expected workflow candidate".into());
                        };
                        let tested = crate::workflow::test_authoring_candidate(
                            &draft.package_id,
                            &draft.version,
                        )?;
                        let confirmed = crate::workflow::confirm_authoring_candidate(
                            &tested.package_id,
                            &tested.version,
                        )?;
                        let installed = crate::app::workflow_manager::install_development_archive(
                            &confirmed.candidate_path,
                            &confirmed.source_root,
                            confirmed.lock.clone(),
                        )?;
                        if !installed.development {
                            return Err("workflow did not activate in the development slot".into());
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        })();
        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let _ = std::fs::remove_dir_all(staging);
        result.unwrap();
    }

    #[test]
    fn plan_allows_local_source_below_declared_minimum_with_warning() {
        let projects = vec![
            project(
                "workflow.target",
                "目标工作流",
                ExtensionProjectKind::Workflow,
                "1.0.0",
                vec![dependency(
                    ExtensionAssetKind::Skill,
                    "skill.local-old",
                    "0.4.2",
                    true,
                )],
            ),
            project(
                "skill.local-old",
                "本地旧版本技能",
                ExtensionProjectKind::Skill,
                "0.2.2",
                Vec::new(),
            ),
        ];
        let plan = plan_from_projects(&projects, &["workflow:workflow.target".to_string()]);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.order.len(), 2);
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.contains("发布前仍需修正版本")));
    }

    #[test]
    fn plan_blocks_missing_required_dependency_without_manual_build_prompt() {
        let projects = vec![project(
            "workflow.missing",
            "缺依赖工作流",
            ExtensionProjectKind::Workflow,
            "1.0.0",
            vec![dependency(
                ExtensionAssetKind::Skill,
                "skill.definitely-missing-for-build-plan",
                "1.0.0",
                true,
            )],
        )];
        let plan = plan_from_projects(&projects, &["workflow:workflow.missing".to_string()]);
        assert!(plan.order.is_empty());
        assert_eq!(plan.blockers[0].code, "dependency_unresolved");
        assert!(plan.blockers[0]
            .message
            .contains("源码和可用安装版本都不存在"));
    }

    #[test]
    fn plan_detects_dependency_cycles() {
        let projects = vec![
            project(
                "skill.a",
                "技能 A",
                ExtensionProjectKind::Skill,
                "1.0.0",
                vec![dependency(ExtensionAssetKind::Skill, "skill.b", "", true)],
            ),
            project(
                "skill.b",
                "技能 B",
                ExtensionProjectKind::Skill,
                "1.0.0",
                vec![dependency(ExtensionAssetKind::Skill, "skill.a", "", true)],
            ),
        ];
        let plan = plan_from_projects(&projects, &["skill:skill.a".to_string()]);
        assert!(plan.order.is_empty());
        assert!(plan
            .blockers
            .iter()
            .any(|blocker| blocker.code == "dependency_cycle"));
    }
}
