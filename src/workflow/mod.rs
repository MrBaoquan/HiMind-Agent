use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(crate) const WORKFLOW_PACKAGE_SCHEMA_VERSION: &str = "workflow_package.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowPackage {
    pub schema_version: String,
    pub id: String,
    pub version: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub min_agent_version: String,
    #[serde(default)]
    pub local_requirements: Value,
    #[serde(default)]
    pub optional_providers: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub dependencies: WorkflowDependencies,
    pub steps: Vec<WorkflowStep>,
    #[serde(default)]
    pub artifacts: Vec<WorkflowArtifact>,
    pub ui: WorkflowUi,
    #[serde(default)]
    pub supported_runtimes: Vec<String>,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowDependencies {
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub connectors: Vec<String>,
    #[serde(default)]
    pub runtimes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowStep {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub capability_id: String,
    pub execution_mode: String,
    #[serde(default)]
    pub risk_level: String,
    #[serde(default)]
    pub approval_required: bool,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowArtifact {
    pub id: String,
    pub artifact_type: String,
    pub name: String,
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowUi {
    pub mode: String,
    #[serde(default)]
    pub entry: String,
    #[serde(default)]
    pub surfaces: Vec<String>,
}

impl WorkflowPackage {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != WORKFLOW_PACKAGE_SCHEMA_VERSION {
            return Err("workflow package schema_version is invalid".to_string());
        }
        validate_workflow_id(&self.id)?;
        Version::parse(&self.version)
            .map_err(|error| format!("invalid workflow version: {error}"))?;
        Version::parse(&self.min_agent_version)
            .map_err(|error| format!("invalid minimum Agent version: {error}"))?;
        if self.name.trim().is_empty() {
            return Err("workflow name is required".to_string());
        }
        if !self.local_requirements.is_object() {
            return Err("workflow local_requirements must be an object".to_string());
        }
        validate_unique_text("capability", &self.capabilities)?;
        validate_unique_text("optional provider", &self.optional_providers)?;
        validate_unique_text("supported runtime", &self.supported_runtimes)?;

        if self.steps.is_empty() {
            return Err("workflow must contain at least one step".to_string());
        }
        let mut step_ids = HashSet::new();
        for step in &self.steps {
            validate_workflow_id(&step.id)?;
            if step.title.trim().is_empty() {
                return Err(format!("workflow step {} title is required", step.id));
            }
            if !matches!(
                step.execution_mode.as_str(),
                "sync" | "long_running" | "provider_defined"
            ) {
                return Err(format!(
                    "workflow step {} execution_mode is invalid",
                    step.id
                ));
            }
            if step.approval_required && step.risk_level.trim().is_empty() {
                return Err(format!(
                    "workflow step {} approval requires a risk level",
                    step.id
                ));
            }
            if !step_ids.insert(step.id.as_str()) {
                return Err(format!("duplicate workflow step id: {}", step.id));
            }
        }
        for step in &self.steps {
            for dependency in &step.depends_on {
                if dependency == &step.id {
                    return Err(format!("workflow step {} depends on itself", step.id));
                }
                if !step_ids.contains(dependency.as_str()) {
                    return Err(format!(
                        "workflow step {} references missing dependency {}",
                        step.id, dependency
                    ));
                }
            }
        }
        validate_step_graph(&self.steps)?;

        let mut artifact_ids = HashSet::new();
        for artifact in &self.artifacts {
            validate_workflow_id(&artifact.id)?;
            if artifact.artifact_type.trim().is_empty() || artifact.name.trim().is_empty() {
                return Err(format!(
                    "workflow artifact {} identity is required",
                    artifact.id
                ));
            }
            if !artifact.schema.trim().is_empty() {
                validate_relative_asset_path(&artifact.schema)?;
            }
            if !artifact_ids.insert(artifact.id.as_str()) {
                return Err(format!("duplicate workflow artifact id: {}", artifact.id));
            }
        }

        if !matches!(self.ui.mode.as_str(), "standard" | "declarative" | "custom") {
            return Err("workflow ui mode is invalid".to_string());
        }
        for surface in &self.ui.surfaces {
            if !matches!(surface.as_str(), "agent" | "dashboard" | "mcp") {
                return Err(format!("unsupported workflow ui surface: {surface}"));
            }
        }
        if self.ui.mode == "custom" {
            if self.ui.entry.trim().is_empty() {
                return Err("custom workflow ui requires an entry".to_string());
            }
            validate_relative_asset_path(&self.ui.entry)?;
        }
        Ok(())
    }
}

pub(crate) fn load_from_directory(root: &Path) -> Result<WorkflowPackage, Box<dyn Error>> {
    let path = root.join("workflow.json");
    let source = fs::read_to_string(&path)
        .map_err(|error| format!("read workflow package {}: {error}", path.display()))?;
    let package: WorkflowPackage = serde_json::from_str(&source)?;
    package.validate().map_err(std::io::Error::other)?;
    validate_package_assets(root, &package)?;
    Ok(package)
}

pub(crate) fn package_dir(root: &Path, package_id: &str) -> Result<PathBuf, Box<dyn Error>> {
    validate_workflow_id(package_id).map_err(std::io::Error::other)?;
    Ok(root.join(package_id))
}

fn validate_workflow_id(value: &str) -> Result<(), String> {
    if value.trim().is_empty()
        || value.len() > 200
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("invalid workflow identifier: {value}"));
    }
    Ok(())
}

fn validate_unique_text(name: &str, values: &[String]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for value in values {
        if value.trim().is_empty() {
            return Err(format!("{name} entry cannot be empty"));
        }
        if !seen.insert(value.as_str()) {
            return Err(format!("duplicate {name}: {value}"));
        }
    }
    Ok(())
}

fn validate_relative_asset_path(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(format!("workflow asset path escapes package: {value}"));
    }
    Ok(())
}

fn validate_package_assets(root: &Path, package: &WorkflowPackage) -> Result<(), Box<dyn Error>> {
    let canonical_root = root.canonicalize()?;
    let mut assets = package
        .artifacts
        .iter()
        .map(|artifact| artifact.schema.as_str())
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>();
    if !package.ui.entry.trim().is_empty() {
        assets.push(package.ui.entry.as_str());
    }
    for asset in assets {
        let target = canonical_root.join(asset);
        let canonical_target = target
            .canonicalize()
            .map_err(|error| format!("workflow asset is unavailable: {asset}: {error}"))?;
        if !canonical_target.starts_with(&canonical_root) || !canonical_target.is_file() {
            return Err(format!("workflow asset escapes package or is not a file: {asset}").into());
        }
    }
    Ok(())
}

fn validate_step_graph(steps: &[WorkflowStep]) -> Result<(), String> {
    let dependencies = steps
        .iter()
        .map(|step| (step.id.as_str(), step.depends_on.as_slice()))
        .collect::<HashMap<_, _>>();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for step in steps {
        visit_step(step.id.as_str(), &dependencies, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit_step<'a>(
    step_id: &'a str,
    dependencies: &HashMap<&'a str, &'a [String]>,
    visiting: &mut HashSet<&'a str>,
    visited: &mut HashSet<&'a str>,
) -> Result<(), String> {
    if visited.contains(step_id) {
        return Ok(());
    }
    if !visiting.insert(step_id) {
        return Err(format!("workflow step graph contains a cycle at {step_id}"));
    }
    if let Some(step_dependencies) = dependencies.get(step_id) {
        for dependency in *step_dependencies {
            visit_step(dependency.as_str(), dependencies, visiting, visited)?;
        }
    }
    visiting.remove(step_id);
    visited.insert(step_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_wechat_miniprogram_delivery_package() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let package = load_from_directory(&root).unwrap();
        assert_eq!(
            package.id,
            "com.himind.workflow.wechat-miniprogram-delivery"
        );
        assert!(package.steps.iter().any(|step| step.id == "WX-14"));
        assert!(package
            .capabilities
            .contains(&"wechat.miniprogram.upload".to_string()));
    }

    #[test]
    fn rejects_step_dependency_cycles() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let mut package = load_from_directory(&root).unwrap();
        let first = package.steps[0].id.clone();
        let second = package.steps[1].id.clone();
        package.steps[0].depends_on.push(second);
        package.steps[1].depends_on.push(first);
        assert!(package.validate().unwrap_err().contains("cycle"));
    }

    #[test]
    fn rejects_custom_ui_path_escape() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let mut package = load_from_directory(&root).unwrap();
        package.ui.mode = "custom".to_string();
        package.ui.entry = "../outside.html".to_string();
        assert!(package.validate().unwrap_err().contains("escapes"));
    }

    #[test]
    fn rejects_missing_asset() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workflows")
            .join("wechat-miniprogram-delivery");
        let temp = std::env::temp_dir().join(format!(
            "himind-workflow-missing-asset-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        copy_dir(&root, &temp).unwrap();
        fs::remove_file(temp.join("ui/workflow-view.json")).unwrap();
        assert!(load_from_directory(&temp)
            .unwrap_err()
            .to_string()
            .contains("unavailable"));
        let _ = fs::remove_dir_all(temp);
    }

    fn copy_dir(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let target_path = target.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_dir(&entry.path(), &target_path)?;
            } else {
                fs::copy(entry.path(), target_path)?;
            }
        }
        Ok(())
    }
}
