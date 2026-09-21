use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(crate) const ENGINEERING_PROJECT_SCHEMA_VERSION: &str = "engineering_project.v1";
pub(crate) const ENGINEERING_PROJECT_MANIFEST: &str = ".himind/project.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngineeringProject {
    pub schema_version: String,
    pub project_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub workspace_root: String,
    #[serde(default)]
    pub source_root: String,
    pub targets: Vec<EngineeringTarget>,
    #[serde(default)]
    pub workflows: EngineeringWorkflowBindings,
    #[serde(default)]
    pub dependencies: EngineeringDependencies,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngineeringTarget {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub source_root: String,
    #[serde(default)]
    pub build_root: String,
    #[serde(default)]
    pub environments: BTreeMap<String, EngineeringEnvironment>,
    #[serde(default)]
    pub delivery: EngineeringDelivery,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngineeringEnvironment {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub build_script: String,
    #[serde(default)]
    pub package_manager: String,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngineeringDelivery {
    #[serde(default)]
    pub workflow_id: String,
    #[serde(default)]
    pub upload_channel: String,
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub default_environment: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngineeringWorkflowBindings {
    #[serde(default)]
    pub create: String,
    #[serde(default)]
    pub develop: String,
    #[serde(default)]
    pub deliver: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngineeringDependencies {
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub connectors: Vec<String>,
    #[serde(default)]
    pub runtimes: Vec<String>,
}

impl EngineeringProject {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != ENGINEERING_PROJECT_SCHEMA_VERSION {
            return Err(format!(
                "engineering project schema_version is invalid: {}",
                self.schema_version
            ));
        }
        validate_identifier("project_id", &self.project_id)?;
        validate_text("name", &self.name, 200)?;
        if self.targets.is_empty() {
            return Err("engineering project requires at least one target".to_string());
        }

        let mut aliases = HashSet::new();
        for alias in &self.aliases {
            validate_alias("project alias", alias)?;
            if !aliases.insert(normalize_reference(alias)) {
                return Err(format!("duplicate engineering project alias: {alias}"));
            }
        }

        let mut target_ids = HashSet::new();
        for target in &self.targets {
            validate_identifier("target id", &target.id)?;
            validate_text("target name", &target.name, 200)?;
            if !target_ids.insert(target.id.as_str()) {
                return Err(format!("duplicate engineering target id: {}", target.id));
            }
            let mut target_aliases = HashSet::new();
            target_aliases.insert(normalize_reference(&target.id));
            target_aliases.insert(normalize_reference(&target.name));
            for alias in &target.aliases {
                validate_alias("target alias", alias)?;
                if !target_aliases.insert(normalize_reference(alias)) {
                    return Err(format!(
                        "duplicate engineering target alias for {}: {alias}",
                        target.id
                    ));
                }
            }
            for environment in target.environments.keys() {
                validate_identifier("environment id", environment)?;
            }
            if !target.delivery.default_environment.trim().is_empty()
                && !target
                    .environments
                    .contains_key(target.delivery.default_environment.trim())
            {
                return Err(format!(
                    "target {} default environment is not declared: {}",
                    target.id, target.delivery.default_environment
                ));
            }
            if !target.delivery.upload_channel.trim().is_empty()
                && !matches!(
                    target.delivery.upload_channel.trim(),
                    "ci" | "wechatide" | "manual"
                )
            {
                return Err(format!(
                    "target {} upload_channel is invalid: {}",
                    target.id, target.delivery.upload_channel
                ));
            }
        }
        validate_unique_text("engineering dependency plugins", &self.dependencies.plugins)?;
        validate_unique_text("engineering dependency skills", &self.dependencies.skills)?;
        validate_unique_text(
            "engineering dependency connectors",
            &self.dependencies.connectors,
        )?;
        validate_unique_text(
            "engineering dependency runtimes",
            &self.dependencies.runtimes,
        )?;
        Ok(())
    }

    pub(crate) fn resolve_target(
        &self,
        reference: &str,
    ) -> Result<&EngineeringTarget, Box<dyn Error>> {
        let reference = normalize_reference(reference);
        if reference.is_empty() {
            if self.targets.len() == 1 {
                return Ok(&self.targets[0]);
            }
            return Err("target reference is required when a project has multiple targets".into());
        }
        for target in &self.targets {
            if normalize_reference(&target.id) == reference
                || normalize_reference(&target.name) == reference
                || target
                    .aliases
                    .iter()
                    .any(|alias| normalize_reference(alias) == reference)
            {
                return Ok(target);
            }
        }
        Err(format!("engineering target not found: {reference}").into())
    }

    pub(crate) fn resolved_snapshot(
        &self,
        workspace_root: &Path,
        target_reference: &str,
        environment_reference: &str,
    ) -> Result<Value, Box<dyn Error>> {
        let target = self.resolve_target(target_reference)?;
        let environment = if environment_reference.trim().is_empty() {
            if target.delivery.default_environment.trim().is_empty() {
                "development"
            } else {
                target.delivery.default_environment.trim()
            }
        } else {
            environment_reference.trim()
        };
        let environment_config = target.environments.get(environment).ok_or_else(|| {
            format!(
                "engineering environment is not declared for target {}: {}",
                target.id, environment
            )
        })?;
        let project_source = if self.source_root.trim().is_empty() {
            "."
        } else {
            self.source_root.trim()
        };
        let target_source = if target.source_root.trim().is_empty() {
            "."
        } else {
            target.source_root.trim()
        };
        let target_build = if target.build_root.trim().is_empty() {
            "."
        } else {
            target.build_root.trim()
        };
        let project_root = resolve_path(workspace_root, project_source);
        let target_source_root = resolve_path(workspace_root, target_source);
        let target_build_root = resolve_path(workspace_root, target_build);
        Ok(json!({
            "schema_version": "engineering_project_resolution.v1",
            "project_id": self.project_id,
            "project_name": self.name,
            "project_aliases": self.aliases,
            "workspace_root": display_path(workspace_root),
            "project_root": project_root,
            "target": {
                "id": target.id,
                "name": target.name,
                "aliases": target.aliases,
                "app_id": target.app_id,
                "source_root": target_source_root,
                "build_root": target_build_root
            },
            "environment": {
                "id": environment,
                "name": environment_config.name,
                "build_script": environment_config.build_script,
                "package_manager": environment_config.package_manager,
                "variables": environment_config.variables
            },
            "delivery": target.delivery,
            "workflows": self.workflows,
            "dependencies": self.dependencies,
            "metadata": self.metadata
        }))
    }
}

pub(crate) fn load_from_workspace(
    workspace_root: &Path,
) -> Result<(EngineeringProject, PathBuf), Box<dyn Error>> {
    let workspace_root = workspace_root.canonicalize()?;
    if !workspace_root.is_dir() {
        return Err("engineering workspace is not a directory".into());
    }
    let manifest_path = workspace_root.join(ENGINEERING_PROJECT_MANIFEST);
    let project = load_from_manifest(&manifest_path)?;
    Ok((project, workspace_root))
}

pub(crate) fn load_from_manifest(
    manifest_path: &Path,
) -> Result<EngineeringProject, Box<dyn Error>> {
    let source = fs::read_to_string(manifest_path)
        .map_err(|error| format!("工程清单读取失败 {}: {error}", manifest_path.display()))?;
    let project: EngineeringProject = serde_json::from_str(&source)
        .map_err(|error| format!("工程清单解析失败 {}: {error}", manifest_path.display()))?;
    project.validate().map_err(std::io::Error::other)?;
    Ok(project)
}

fn validate_identifier(name: &str, value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 120
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(format!("{name} is invalid: {value}"));
    }
    Ok(())
}

fn validate_alias(name: &str, value: &str) -> Result<(), String> {
    validate_text(name, value, 120)
}

fn validate_text(name: &str, value: &str, max_chars: usize) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{name} cannot be empty"));
    }
    if value.chars().count() > max_chars {
        return Err(format!("{name} exceeds {max_chars} characters"));
    }
    Ok(())
}

fn validate_unique_text(name: &str, values: &[String]) -> Result<(), String> {
    let mut unique = HashSet::new();
    for value in values {
        validate_text(name, value, 200)?;
        if !unique.insert(normalize_reference(value)) {
            return Err(format!("{name} contains a duplicate value: {value}"));
        }
    }
    Ok(())
}

fn normalize_reference(value: &str) -> String {
    value.trim().to_lowercase()
}

fn resolve_path(base: &Path, value: &str) -> String {
    let candidate = PathBuf::from(value.trim());
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        base.join(candidate)
    };
    display_path(&normalize_path(candidate))
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        load_from_manifest, load_from_workspace, EngineeringProject, ENGINEERING_PROJECT_MANIFEST,
    };
    use std::fs;
    use std::path::PathBuf;

    fn fixture_project() -> EngineeringProject {
        serde_json::from_str(include_str!(
            "../contracts/agent-core/v1/examples/engineering-project.example.json"
        ))
        .unwrap()
    }

    #[test]
    fn example_project_is_valid() {
        fixture_project().validate().unwrap();
    }

    #[test]
    fn resolves_target_alias_and_environment() {
        let project = fixture_project();
        let snapshot = project
            .resolved_snapshot(PathBuf::from("C:/workspace").as_path(), "随州馆", "")
            .unwrap();
        assert_eq!(snapshot["target"]["id"], "szkjg");
        assert_eq!(snapshot["environment"]["id"], "development");
        let build_root = snapshot["target"]["build_root"].as_str().unwrap();
        assert!(build_root.ends_with("dist/wx") || build_root.ends_with(r"dist\wx"));
    }

    #[test]
    fn loads_project_from_workspace_manifest() {
        let root = std::env::temp_dir().join(format!(
            "himind-engineering-project-{}-{}",
            std::process::id(),
            crate::approval::manager::unix_now()
        ));
        fs::create_dir_all(root.join(".himind")).unwrap();
        let source =
            include_str!("../contracts/agent-core/v1/examples/engineering-project.example.json");
        fs::write(root.join(ENGINEERING_PROJECT_MANIFEST), source).unwrap();
        let (project, workspace) = load_from_workspace(&root).unwrap();
        assert_eq!(project.project_id, "kerun-user");
        assert_eq!(workspace, root.canonicalize().unwrap());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unknown_fields() {
        let manifest = std::env::temp_dir().join(format!(
            "himind-engineering-project-invalid-{}.json",
            std::process::id()
        ));
        fs::write(
            &manifest,
            r#"{"schema_version":"engineering_project.v1","project_id":"demo","name":"Demo","targets":[{"id":"main","name":"Main"}],"future":true}"#,
        )
        .unwrap();
        assert!(load_from_manifest(&manifest).is_err());
        let _ = fs::remove_file(manifest);
    }
}
