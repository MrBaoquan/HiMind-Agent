use crate::skill::types::{SkillManifest, SkillScope};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

pub(crate) fn skill_manifest_path(root: &Path) -> PathBuf {
    root.join("skill.json")
}

fn internal_skill_manifest_path(root: &Path) -> PathBuf {
    root.join(".himind").join("manifest.json")
}

pub(crate) fn skill_readme_path(root: &Path) -> PathBuf {
    root.join("SKILL.md")
}

pub(crate) fn is_internal_package_file(path: &str) -> bool {
    let path = path.replace('\\', "/");
    matches!(
        path.as_str(),
        "checksums.sha256" | ".himind" | ".himind-render.json"
    ) || path.starts_with(".himind/")
}

pub(crate) fn load_skill_manifest(root: &Path) -> Result<SkillManifest, Box<dyn Error>> {
    let internal = internal_skill_manifest_path(root);
    if internal.is_file() {
        return parse_skill_manifest(&fs::read_to_string(internal)?);
    }
    let legacy = skill_manifest_path(root);
    if legacy.is_file() {
        let content = fs::read_to_string(legacy)?;
        if let Ok(manifest) = parse_skill_manifest(&content) {
            return Ok(manifest);
        }
    }
    load_standard_skill_manifest(root)
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
    if manifest.contents.is_empty()
        || !manifest
            .contents
            .iter()
            .any(|item| item.eq_ignore_ascii_case("SKILL.md"))
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
    }
    let mut capability_ids = HashSet::new();
    for dependency in &manifest.capabilities {
        validate_skill_id(&dependency.id)?;
        if !capability_ids.insert(dependency.id.as_str()) {
            return Err(format!("duplicate capability dependency: {}", dependency.id).into());
        }
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
    let mut plugin_ids = HashSet::new();
    for dependency in &manifest.plugin_dependencies {
        validate_skill_id(&dependency.plugin_id)?;
        if !plugin_ids.insert(dependency.plugin_id.as_str()) {
            return Err(format!("duplicate plugin dependency: {}", dependency.plugin_id).into());
        }
        if let Some(value) = dependency.min_version.as_deref() {
            validate_skill_version(value)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_skill_package_root(root: &Path) -> Result<SkillManifest, Box<dyn Error>> {
    let readme_path = skill_readme_path(root);
    if !readme_path.exists() {
        return Err("skill package missing SKILL.md".into());
    }
    load_skill_manifest(root)
}

/// The portable Agent Skills contract is a directory containing a SKILL.md
/// file.  HiMind keeps its richer `skill.json` internally, but standard
/// packages do not have to ship it.  This helper creates that internal view in
/// the staging directory without changing the original archive.
pub(crate) fn normalize_standard_package(root: &Path) -> Result<SkillManifest, Box<dyn Error>> {
    let wrapper_name = flatten_single_wrapper(root)?;
    let internal_path = internal_skill_manifest_path(root);
    let legacy_path = skill_manifest_path(root);
    let himind_manifest = if internal_path.is_file() {
        Some(parse_skill_manifest(&fs::read_to_string(&internal_path)?)?)
    } else if legacy_path.is_file() {
        parse_skill_manifest(&fs::read_to_string(&legacy_path)?).ok()
    } else {
        None
    };
    if let Some(manifest) = himind_manifest {
        return Ok(manifest);
    }

    let manifest = load_standard_skill_manifest(root)?;
    if let Some(wrapper_name) = wrapper_name {
        if wrapper_name != manifest.id {
            return Err(format!(
                "SKILL.md name {} must match ZIP directory {wrapper_name}",
                manifest.id
            )
            .into());
        }
    }
    if let Some(parent) = internal_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&internal_path, serde_json::to_vec_pretty(&manifest)?)?;
    write_package_checksums(root)?;
    Ok(manifest)
}

#[derive(Debug, Clone, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    version: Option<serde_yaml::Value>,
}

/// Parse the standard YAML frontmatter at the beginning of SKILL.md.  Unknown
/// fields are intentionally ignored so vendor extensions remain portable.
pub(crate) fn parse_skill_frontmatter(
    content: &str,
) -> Result<(String, String, Option<String>), Box<dyn Error>> {
    let content = content.trim_start_matches('\u{feff}');
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err("SKILL.md 缺少 YAML frontmatter 起始标记 ---".into());
    }
    let mut yaml = String::new();
    let mut closed = false;
    for line in lines {
        if matches!(line.trim(), "---" | "...") {
            closed = true;
            break;
        }
        yaml.push_str(line);
        yaml.push('\n');
    }
    if !closed {
        return Err("SKILL.md YAML frontmatter 未闭合".into());
    }
    let frontmatter: SkillFrontmatter = serde_yaml::from_str(&yaml)?;
    validate_standard_skill_name(&frontmatter.name)?;
    let description = frontmatter.description.trim().to_string();
    if description.is_empty() {
        return Err("SKILL.md description 不能为空".into());
    }
    if description.chars().count() > 1024 {
        return Err("SKILL.md description 不能超过 1024 个字符".into());
    }
    let version = frontmatter
        .version
        .as_ref()
        .and_then(yaml_scalar_string)
        .filter(|value| !value.trim().is_empty());
    Ok((frontmatter.name, description, version))
}

pub(crate) fn validate_standard_skill_name(value: &str) -> Result<(), Box<dyn Error>> {
    let trimmed = value.trim();
    let length = trimmed.chars().count();
    if length == 0
        || length > 64
        || trimmed.starts_with('-')
        || trimmed.ends_with('-')
        || trimmed.contains("--")
        || !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!("invalid Agent Skills name: {value}").into());
    }
    Ok(())
}

pub(crate) fn validate_standard_skill_directory(root: &Path) -> Result<(), Box<dyn Error>> {
    let readme = fs::read_to_string(skill_readme_path(root))?;
    let (name, _, _) = parse_skill_frontmatter(&readme)?;
    let directory = root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("Skill 目录名必须是 UTF-8")?;
    if name != directory {
        return Err(format!("SKILL.md name {name} must match directory {directory}").into());
    }
    Ok(())
}

fn load_standard_skill_manifest(root: &Path) -> Result<SkillManifest, Box<dyn Error>> {
    let readme_path = skill_readme_path(root);
    if !readme_path.is_file() {
        return Err("Skill package missing SKILL.md".into());
    }
    let readme = fs::read_to_string(&readme_path)?;
    let (name, description, frontmatter_version) = parse_skill_frontmatter(&readme)?;
    let version = frontmatter_version
        .or_else(|| read_skill_yaml_version(root))
        .map(Ok)
        .unwrap_or_else(|| standard_package_content_version(root))?;
    let mut contents = collect_package_files(root)?;
    contents.retain(|path| path != "checksums.sha256" && !path.starts_with(".himind/"));
    contents.push(".himind/manifest.json".to_string());
    contents.sort();
    contents.dedup();
    let manifest = SkillManifest {
        id: name.clone(),
        name,
        author: String::new(),
        categories: Vec::new(),
        version,
        scope: SkillScope::User,
        description,
        release_notes: String::new(),
        min_agent_version: String::new(),
        supported_clients: vec![crate::skill::clients::PORTABLE_PROFILE_ID.to_string()],
        capabilities: Vec::new(),
        plugin_dependencies: Vec::new(),
        risk_summary: "standard_agent_skill".to_string(),
        contents,
    };
    validate_skill_manifest(&manifest)?;
    Ok(manifest)
}

fn read_skill_yaml_version(root: &Path) -> Option<String> {
    let path = root.join("skill.yaml");
    let content = fs::read_to_string(path).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(&content).ok()?;
    value
        .get("version")
        .and_then(yaml_scalar_string)
        .filter(|value| !value.trim().is_empty())
}

fn yaml_scalar_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(value) => Some(value.clone()),
        serde_yaml::Value::Number(value) => Some(value.to_string()),
        serde_yaml::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn standard_package_content_version(root: &Path) -> Result<String, Box<dyn Error>> {
    let mut digest = Sha256::new();
    for relative in collect_package_files(root)? {
        if is_internal_package_file(&relative) {
            continue;
        }
        let content = fs::read(root.join(&relative))?;
        digest.update((relative.len() as u64).to_le_bytes());
        digest.update(relative.as_bytes());
        digest.update((content.len() as u64).to_le_bytes());
        digest.update(content);
    }
    let checksum = format!("{:x}", digest.finalize());
    Ok(format!("0.0.0+sha.{}", &checksum[..12]))
}

pub(crate) fn standard_skill_version(root: &Path) -> Result<String, Box<dyn Error>> {
    standard_package_content_version(root)
}

fn collect_package_files(root: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        validate_relative_package_path(&relative)?;
        files.push(relative);
    }
    files.sort();
    Ok(files)
}

fn flatten_single_wrapper(root: &Path) -> Result<Option<String>, Box<dyn Error>> {
    if root.join("SKILL.md").is_file() || root.join("skill.json").is_file() {
        return Ok(None);
    }
    let mut directories = Vec::new();
    let mut files = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            directories.push(entry.path());
        } else if entry.file_type()?.is_file()
            && entry.file_name().to_string_lossy() != "checksums.sha256"
        {
            files.push(entry.path());
        }
    }
    if directories.len() != 1 || !files.is_empty() {
        return Ok(None);
    }
    let wrapper = &directories[0];
    if !wrapper.join("SKILL.md").is_file() && !wrapper.join("skill.json").is_file() {
        return Ok(None);
    }
    let wrapper_name = wrapper
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("Skill ZIP 包装目录名必须是 UTF-8")?
        .to_string();
    let entries = fs::read_dir(wrapper)?.collect::<Result<Vec<_>, _>>()?;
    for entry in entries {
        let name = entry.file_name();
        let destination = root.join(&name);
        if name.to_string_lossy() == "checksums.sha256" && destination.exists() {
            let _ = fs::remove_file(entry.path());
            continue;
        }
        if destination.exists() {
            return Err(format!(
                "Skill ZIP 包装目录展开时发生路径冲突: {}",
                name.to_string_lossy()
            )
            .into());
        }
        fs::rename(entry.path(), destination)?;
    }
    fs::remove_dir(wrapper)?;
    Ok(Some(wrapper_name))
}

fn write_package_checksums(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut rows = Vec::new();
    for relative in collect_package_files(root)? {
        if relative == "checksums.sha256" {
            continue;
        }
        let digest = Sha256::digest(fs::read(root.join(&relative))?);
        rows.push(format!("{:x}  {relative}\n", digest));
    }
    rows.sort();
    fs::write(root.join("checksums.sha256"), rows.concat())?;
    Ok(())
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

/// 解析 `checksums.sha256`，返回 `相对路径 -> 摘要`。行序只是打包产物，
/// 不代表包内容，因此判断「同一版本内容是否一致」必须比较该映射。
pub(crate) fn parse_checksums(content: &str) -> Result<HashMap<String, String>, Box<dyn Error>> {
    let mut expected = HashMap::new();
    for (index, line) in content.lines().enumerate() {
        let Some((checksum, relative)) = line.split_once("  ") else {
            return Err(format!("checksums.sha256 第 {} 行格式无效", index + 1).into());
        };
        if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("checksums.sha256 第 {} 行摘要无效", index + 1).into());
        }
        validate_relative_package_path(relative)?;
        if relative == "checksums.sha256"
            || expected
                .insert(relative.replace('\\', "/"), checksum.to_ascii_lowercase())
                .is_some()
        {
            return Err(format!("checksums.sha256 包含无效或重复路径: {relative}").into());
        }
    }
    Ok(expected)
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
            name: "环境诊断".to_string(),
            author: String::new(),
            categories: vec![],
            version: "1.0.0".to_string(),
            scope: SkillScope::Builtin,
            description: "read only".to_string(),
            release_notes: "测试 Skill Manifest。".to_string(),
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
    fn accepts_standard_script_files_as_resources() {
        let manifest = SkillManifest {
            id: "com.himind.skill.environment-doctor".to_string(),
            name: "环境诊断".to_string(),
            author: String::new(),
            categories: vec![],
            version: "1.0.0".to_string(),
            scope: SkillScope::Builtin,
            description: "read only".to_string(),
            release_notes: "测试脚本拒绝规则。".to_string(),
            min_agent_version: "0.2.0".to_string(),
            supported_clients: vec!["codex".to_string()],
            capabilities: vec![],
            plugin_dependencies: vec![],
            risk_summary: "read_only".to_string(),
            contents: vec![
                "skill.json".to_string(),
                "SKILL.md".to_string(),
                "scripts/install.ps1".to_string(),
            ],
        };

        assert!(validate_skill_manifest(&manifest).is_ok());
    }

    #[test]
    fn parses_standard_frontmatter_without_private_manifest() {
        let root = std::env::temp_dir().join("himind-standard-skill-test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("references")).unwrap();
        fs::write(
            root.join("SKILL.md"),
            "---\nname: standard-skill\ndescription: >-\n  A portable skill.\nversion: 1.2.3\n---\n\n# Standard\n",
        )
        .unwrap();
        fs::write(root.join("references/guide.md"), "guide").unwrap();
        let manifest = normalize_standard_package(&root).unwrap();
        assert_eq!(manifest.id, "standard-skill");
        assert_eq!(manifest.version, "1.2.3");
        assert!(root.join(".himind/manifest.json").is_file());
        assert!(root.join("checksums.sha256").is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn standard_package_without_version_gets_content_identity() {
        let root = std::env::temp_dir().join("himind-standard-skill-version-test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("SKILL.md"),
            "---\nname: standard-skill\ndescription: Portable skill.\n---\n",
        )
        .unwrap();

        let manifest = load_standard_skill_manifest(&root).unwrap();
        assert!(manifest.version.starts_with("0.0.0+sha."));
        assert_eq!(manifest.version.len(), "0.0.0+sha.".len() + 12);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_consecutive_hyphens_in_standard_name() {
        assert!(validate_standard_skill_name("project--rules").is_err());
    }

    #[test]
    fn accepts_null_dependency_arrays_from_legacy_packages() {
        let manifest = parse_skill_manifest(
            r#"{"id":"com.himind.skill.legacy","name":"历史 Skill","version":"1.0.0","scope":"organization","supported_clients":["codex"],"capabilities":null,"plugin_dependencies":null,"contents":["skill.json","SKILL.md"]}"#,
        )
        .unwrap();
        assert!(manifest.capabilities.is_empty());
        assert!(manifest.plugin_dependencies.is_empty());
    }
}
