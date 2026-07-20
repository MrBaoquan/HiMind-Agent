use crate::skill::types::{SkillManifest, SkillScope};
use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(crate) fn skill_manifest_path(root: &Path) -> PathBuf {
    root.join("skill.json")
}

pub(crate) fn skill_readme_path(root: &Path) -> PathBuf {
    root.join("SKILL.md")
}

pub(crate) fn load_skill_manifest(root: &Path) -> Result<SkillManifest, Box<dyn Error>> {
    let content = fs::read_to_string(skill_manifest_path(root))?;
    parse_skill_manifest(&content)
}

pub(crate) fn parse_skill_manifest(content: &str) -> Result<SkillManifest, Box<dyn Error>> {
    let manifest: SkillManifest = serde_json::from_str(content.trim_start_matches('\u{feff}'))?;
    validate_skill_manifest(&manifest)?;
    Ok(manifest)
}

pub(crate) fn validate_skill_manifest(manifest: &SkillManifest) -> Result<(), Box<dyn Error>> {
    validate_skill_id(&manifest.id)?;
    validate_skill_version(&manifest.version)?;
    if manifest.name.trim().is_empty() {
        return Err("skill name is required".into());
    }
    if manifest.supported_clients.is_empty() {
        return Err("supported_clients is required".into());
    }
    if manifest.contents.is_empty() {
        return Err("contents is required".into());
    }
    if !manifest
        .contents
        .iter()
        .any(|item| item.eq_ignore_ascii_case("skill.json"))
    {
        return Err("contents must include skill.json".into());
    }
    if !manifest
        .contents
        .iter()
        .any(|item| item.eq_ignore_ascii_case("skill.md") || item.eq_ignore_ascii_case("SKILL.md"))
    {
        return Err("contents must include SKILL.md".into());
    }
    if let SkillScope::Builtin = manifest.scope {
        if manifest.min_agent_version.trim().is_empty() {
            return Err("builtin skills must declare min_agent_version".into());
        }
    }
    for client in &manifest.supported_clients {
        validate_client_id(client)?;
    }
    for content in &manifest.contents {
        validate_relative_package_path(content)?;
        if looks_like_executable_or_script(content) {
            return Err(format!(
                "skill package must not contain executable or script file: {content}"
            )
            .into());
        }
    }
    for dependency in &manifest.capabilities {
        validate_skill_id(&dependency.id)?;
        if let Some(value) = dependency.min_version.as_deref() {
            validate_skill_version(value)?;
        }
        if let Some(value) = dependency.max_version.as_deref() {
            validate_skill_version(value)?;
        }
        if let Some(provider) = dependency.provider.as_deref() {
            if provider.trim().is_empty() {
                return Err("capability provider cannot be empty".into());
            }
        }
    }
    for dependency in &manifest.plugin_dependencies {
        validate_skill_id(&dependency.plugin_id)?;
        if let Some(value) = dependency.min_version.as_deref() {
            validate_skill_version(value)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_skill_package_root(root: &Path) -> Result<SkillManifest, Box<dyn Error>> {
    let manifest_path = skill_manifest_path(root);
    let readme_path = skill_readme_path(root);
    if !manifest_path.exists() {
        return Err("skill package missing skill.json".into());
    }
    if !readme_path.exists() {
        return Err("skill package missing SKILL.md".into());
    }
    load_skill_manifest(root)
}

pub(crate) fn write_skill_package(
    root: &Path,
    manifest: &SkillManifest,
    readme: &str,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(root)?;
    fs::write(
        skill_manifest_path(root),
        serde_json::to_vec_pretty(manifest)?,
    )?;
    fs::write(skill_readme_path(root), readme)?;
    Ok(())
}

pub(crate) fn validate_relative_package_path(path: &str) -> Result<(), Box<dyn Error>> {
    let relative = Path::new(path);
    if path.trim().is_empty() || relative.is_absolute() {
        return Err(format!("path must be relative: {path}").into());
    }
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("path contains invalid segment: {path}").into());
    }
    Ok(())
}

pub(crate) fn validate_skill_id(value: &str) -> Result<(), Box<dyn Error>> {
    if value.trim().is_empty()
        || value
            .split('.')
            .any(|segment| segment.is_empty() || !segment.bytes().all(is_ascii_identifier_byte))
    {
        return Err(format!("invalid skill id: {value}").into());
    }
    Ok(())
}

fn validate_client_id(value: &str) -> Result<(), Box<dyn Error>> {
    if value.trim().is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(format!("invalid client id: {value}").into());
    }
    Ok(())
}

fn validate_skill_version(value: &str) -> Result<(), Box<dyn Error>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("skill version is required".into());
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(format!("invalid version: {value}").into());
    }
    Ok(())
}

fn looks_like_executable_or_script(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".ps1")
        || lower.ends_with(".sh")
        || lower.ends_with(".bat")
        || lower.ends_with(".cmd")
        || lower.ends_with(".exe")
        || lower.ends_with(".com")
        || lower.ends_with(".dll")
        || lower.contains("/scripts/")
        || lower.contains("/bin/")
}

fn is_ascii_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::types::{SkillCapabilityDependency, SkillScope};

    #[test]
    fn validates_and_writes_skill_package() {
        let root = std::env::temp_dir().join("himind-skill-manifest-test");
        let _ = fs::remove_dir_all(&root);
        let manifest = SkillManifest {
            id: "com.himind.skill.environment-doctor".to_string(),
            name: "Environment Doctor".to_string(),
            version: "1.0.0".to_string(),
            scope: SkillScope::Builtin,
            description: "read only".to_string(),
            min_agent_version: "0.2.0".to_string(),
            supported_clients: vec!["codex".to_string()],
            capabilities: vec![SkillCapabilityDependency {
                id: "system.health".to_string(),
                required: true,
                min_version: Some("1.0.0".to_string()),
                max_version: None,
                provider: None,
            }],
            plugin_dependencies: vec![],
            risk_summary: "read_only".to_string(),
            contents: vec!["skill.json".to_string(), "SKILL.md".to_string()],
        };

        assert!(validate_skill_manifest(&manifest).is_ok());
        write_skill_package(&root, &manifest, "# Demo").unwrap();
        let loaded = validate_skill_package_root(&root).unwrap();
        assert_eq!(loaded.id, manifest.id);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_unsafe_package_paths() {
        assert!(validate_relative_package_path("../escape.txt").is_err());
        assert!(validate_relative_package_path("/absolute.txt").is_err());
    }

    #[test]
    fn rejects_script_files() {
        let manifest = SkillManifest {
            id: "com.himind.skill.environment-doctor".to_string(),
            name: "Environment Doctor".to_string(),
            version: "1.0.0".to_string(),
            scope: SkillScope::Builtin,
            description: "read only".to_string(),
            min_agent_version: "0.2.0".to_string(),
            supported_clients: vec!["codex".to_string()],
            capabilities: vec![],
            plugin_dependencies: vec![],
            risk_summary: "read_only".to_string(),
            contents: vec!["skill.json".to_string(), "scripts/install.ps1".to_string()],
        };

        assert!(validate_skill_manifest(&manifest).is_err());
    }
}
