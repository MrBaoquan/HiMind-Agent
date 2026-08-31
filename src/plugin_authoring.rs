use crate::app::plugin_manager::verify_plugin_checksums;
use crate::capability::plugin::{
    parse_plugin_manifest, validate_development_entry, validate_manifest_contributions,
    PluginManifest,
};
use crate::{Options, VERSION};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::ZipArchive;

const MAX_PLUGIN_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PluginDraftInput {
    pub package_path: PathBuf,
    #[serde(default)]
    pub revision_of_version: Option<String>,
    #[serde(default)]
    pub parent_submission_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PluginDraft {
    pub manifest: PluginManifest,
    pub candidate_path: PathBuf,
    pub candidate_sha256: String,
    #[serde(default)]
    pub development_path: Option<PathBuf>,
    #[serde(default)]
    pub workspace_path: Option<PathBuf>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub revision_of: Option<String>,
    #[serde(default)]
    pub parent_submission_id: Option<String>,
    pub tested_at: Option<String>,
    pub confirmed_at: Option<String>,
    pub submitted_at: Option<String>,
    pub dashboard_submission_id: Option<String>,
    #[serde(default)]
    pub test_report: Option<Value>,
    pub updated_at: String,
}

pub(crate) fn list() -> Result<Vec<PluginDraft>, Box<dyn Error>> {
    let root = drafts_root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut drafts = Vec::new();
    for entry in walkdir::WalkDir::new(root).min_depth(3).max_depth(3) {
        let entry = entry?;
        if entry.file_type().is_file() && entry.file_name() == "draft.json" {
            if let Ok(draft) =
                serde_json::from_str::<PluginDraft>(&fs::read_to_string(entry.path())?)
            {
                drafts.push(draft);
            }
        }
    }
    drafts.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(drafts)
}

pub(crate) fn save(input: PluginDraftInput) -> Result<PluginDraft, Box<dyn Error>> {
    let source = input.package_path.canonicalize()?;
    if source.extension().and_then(|value| value.to_str()) != Some("hmpkg") {
        return Err("插件候选包必须使用 .hmpkg 扩展名".into());
    }
    if fs::metadata(&source)?.len() > MAX_PLUGIN_ARCHIVE_BYTES {
        return Err("插件候选包超过 512 MiB".into());
    }
    let manifest = read_archive_manifest(&source)?;
    validate_identity(&manifest)?;
    let root = draft_version_root(&manifest.id, &manifest.version);
    let candidate_sha256 = sha256_file(&source)?;
    let previous = read(&manifest.id, &manifest.version).ok();
    let unchanged = previous
        .as_ref()
        .map(|draft| draft.candidate_sha256 == candidate_sha256)
        .unwrap_or(false);
    fs::create_dir_all(&root)?;
    let candidate_path = root.join(format!("{}-{}.hmpkg", manifest.id, manifest.version));
    fs::copy(&source, &candidate_path)?;
    let package_root = root.join("package");
    if package_root.exists() {
        fs::remove_dir_all(&package_root)?;
    }
    extract_and_validate(&candidate_path, &package_root, &manifest)?;
    crate::capability::plugin::register_development_plugin(&package_root)?;
    let draft = PluginDraft {
        manifest,
        candidate_path,
        candidate_sha256,
        development_path: Some(package_root),
        workspace_path: Some(source.parent().unwrap_or(source.as_path()).to_path_buf()),
        source: "local_package".to_string(),
        revision_of: input
            .revision_of_version
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        parent_submission_id: input
            .parent_submission_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        tested_at: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.tested_at.clone()),
        confirmed_at: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.confirmed_at.clone()),
        submitted_at: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.submitted_at.clone()),
        dashboard_submission_id: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.dashboard_submission_id.clone()),
        test_report: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.test_report.clone()),
        updated_at: now_stamp(),
    };
    persist(&draft)?;
    Ok(draft)
}

pub(crate) fn read(plugin_id: &str, version: &str) -> Result<PluginDraft, Box<dyn Error>> {
    validate_identifier(plugin_id)?;
    validate_version(version)?;
    Ok(serde_json::from_str(&fs::read_to_string(
        draft_version_root(plugin_id, version).join("draft.json"),
    )?)?)
}

pub(crate) fn associate_workspace(
    mut draft: PluginDraft,
    workspace: &Path,
) -> Result<PluginDraft, Box<dyn Error>> {
    draft.workspace_path = Some(workspace.canonicalize()?);
    draft.source = "workspace".to_string();
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(draft)
}

pub(crate) fn create_revision(
    plugin_id: &str,
    version: &str,
) -> Result<PluginDraft, Box<dyn Error>> {
    let previous = read(plugin_id, version)?;
    let next_version = bump_patch(version);
    if read(plugin_id, &next_version).is_ok() {
        return Err(format!("插件版本已存在: {next_version}").into());
    }
    let source = previous
        .development_path
        .ok_or("原版本没有可复制的工作区")?;
    let root = draft_version_root(plugin_id, &next_version);
    let package_root = root.join("package");
    copy_directory(&source, &package_root)?;
    let manifest_path = package_root.join("plugin.json");
    let mut manifest = parse_plugin_manifest(&fs::read_to_string(&manifest_path)?)?;
    manifest.version = next_version.clone();
    manifest.release_notes = format!("基于 v{version} 的功能改进与问题修复。");
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    write_checksums(&package_root)?;
    let candidate_path = root.join(format!("{plugin_id}-{next_version}.hmpkg"));
    archive_directory(&package_root, &candidate_path)?;
    let draft = PluginDraft {
        manifest,
        candidate_sha256: sha256_file(&candidate_path)?,
        candidate_path,
        development_path: Some(package_root.clone()),
        workspace_path: Some(package_root),
        source: "revision".to_string(),
        revision_of: Some(version.to_string()),
        parent_submission_id: previous.dashboard_submission_id,
        tested_at: None,
        confirmed_at: None,
        submitted_at: None,
        dashboard_submission_id: None,
        test_report: None,
        updated_at: now_stamp(),
    };
    persist(&draft)?;
    crate::capability::plugin::register_development_plugin(
        draft
            .development_path
            .as_deref()
            .ok_or("新版本工作区无效")?,
    )?;
    Ok(draft)
}

fn bump_patch(version: &str) -> String {
    let mut parts = version
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0));
    format!(
        "{}.{}.{}",
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0) + 1
    )
}

fn copy_directory(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    for entry in walkdir::WalkDir::new(source).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination)?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn write_checksums(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut content = String::new();
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry?;
        if entry.file_type().is_file() && entry.file_name() != "checksums.sha256" {
            let relative = entry
                .path()
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            content.push_str(&format!(
                "{:x}  {relative}\n",
                Sha256::digest(fs::read(entry.path())?)
            ));
        }
    }
    fs::write(root.join("checksums.sha256"), content)?;
    Ok(())
}

fn archive_directory(root: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    let mut archive = zip::ZipWriter::new(File::create(target)?);
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            archive.start_file(relative, options)?;
            std::io::copy(&mut File::open(entry.path())?, &mut archive)?;
        }
    }
    archive.finish()?;
    Ok(())
}

pub(crate) fn test(plugin_id: &str, version: &str) -> Result<PluginDraft, Box<dyn Error>> {
    let mut draft = read(plugin_id, version)?;
    ensure_candidate_unchanged(&draft)?;
    let dependency_issues =
        crate::capability::plugin::plugin_manifest_dependency_issues(&draft.manifest);
    if !dependency_issues.is_empty() {
        return Err(format!("插件候选包依赖预检未通过: {}", dependency_issues.join("; ")).into());
    }
    let root = draft_version_root(plugin_id, version).join("test-package");
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    extract_and_validate(&draft.candidate_path, &root, &draft.manifest)?;
    let mut registry_snapshot = DevelopmentRegistrySnapshot::capture()?;
    crate::capability::plugin::register_development_plugin(&root)?;
    // Candidate tests use an isolated development registration only for the
    // duration of validation.  Do not leave a stale registry entry after the
    // report has been persisted.
    let runtime = run_runtime_contract_test(&draft.manifest)?;
    let registered_before_cleanup = development_registry_contains(&draft.manifest.id);
    let registration_cleanup =
        crate::capability::plugin::unregister_development_plugin(&draft.manifest.id);
    let absent_after_cleanup = !development_registry_contains(&draft.manifest.id);
    let re_registration = crate::capability::plugin::register_development_plugin(&root).map(|_| ());
    let present_after_restore = development_registry_contains(&draft.manifest.id);
    let final_cleanup =
        crate::capability::plugin::unregister_development_plugin(&draft.manifest.id);
    let cleanup_errors = [registration_cleanup, re_registration, final_cleanup]
        .into_iter()
        .filter_map(Result::err)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if !registered_before_cleanup || !absent_after_cleanup || !present_after_restore {
        return Err("插件候选测试注册生命周期未通过".into());
    }
    if !cleanup_errors.is_empty() {
        return Err(format!("插件候选测试注册清理失败: {}", cleanup_errors.join("; ")).into());
    }
    if runtime.get("state").and_then(Value::as_str) == Some("failed") {
        return Err(format!(
            "插件候选测试运行时契约未通过: {}",
            runtime
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown runtime contract failure")
        )
        .into());
    }
    registry_snapshot.restore()?;
    draft.development_path = Some(root);
    draft.tested_at = Some(now_stamp());
    draft.confirmed_at = None;
    draft.submitted_at = None;
    draft.dashboard_submission_id = None;
    draft.test_report = Some(json!({
        "manifest": "passed",
        "dependencies": "passed",
        "package": "passed",
        "runtime": runtime,
        "lifecycle": {
            "registered": registered_before_cleanup,
            "unregistered": absent_after_cleanup,
            "re_registered": present_after_restore,
            "state": "passed"
        },
        "cleanup": { "development_registry": "passed" }
    }));
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(draft)
}

fn development_registry_contains(plugin_id: &str) -> bool {
    let path = development_registry_path_for_test();
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<Vec<Value>>(&content).ok())
        .map(|items| {
            items
                .iter()
                .any(|item| item.get("id").and_then(Value::as_str) == Some(plugin_id))
        })
        .unwrap_or(false)
}

fn development_registry_path_for_test() -> PathBuf {
    env::var_os("HIMIND_PLUGIN_DEVELOPMENT_REGISTRY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            crate::capability::plugin::plugin_registry_dir()
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("plugin-development.json")
        })
}

struct DevelopmentRegistrySnapshot {
    path: PathBuf,
    content: Option<Vec<u8>>,
    armed: bool,
}

impl DevelopmentRegistrySnapshot {
    fn capture() -> Result<Self, Box<dyn Error>> {
        let path = development_registry_path_for_test();
        Ok(Self {
            content: fs::read(&path).ok(),
            path,
            armed: true,
        })
    }

    fn restore(&mut self) -> Result<(), Box<dyn Error>> {
        self.restore_inner()?;
        self.armed = false;
        Ok(())
    }

    fn restore_inner(&self) -> Result<(), Box<dyn Error>> {
        match self.content.as_deref() {
            Some(content) => {
                if let Some(parent) = self.path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&self.path, content)?;
            }
            None => {
                if self.path.exists() {
                    fs::remove_file(&self.path)?;
                }
            }
        }
        Ok(())
    }
}

impl Drop for DevelopmentRegistrySnapshot {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.restore_inner();
        }
    }
}

fn run_runtime_contract_test(manifest: &PluginManifest) -> Result<Value, Box<dyn Error>> {
    let Some(capability) = manifest
        .capabilities
        .iter()
        .find(|capability| capability.risk_level.eq_ignore_ascii_case("read_only"))
        .or_else(|| manifest.capabilities.first())
    else {
        return Ok(json!({ "state": "skipped", "reason": "插件未声明 Capability" }));
    };
    if !capability.risk_level.eq_ignore_ascii_case("read_only") {
        return Ok(json!({
            "state": "skipped",
            "capability_id": capability.id,
            "reason": "候选测试不会自动执行有副作用的 Capability"
        }));
    }
    let input = sample_input(&capability.input_schema);
    match crate::capability::plugin::invoke_plugin_capability_for_plugin(
        &manifest.id,
        &capability.id,
        input.clone(),
        None,
    ) {
        Ok(output) => Ok(json!({
            "state": "passed",
            "capability_id": capability.id,
            "input": input,
            "output": output
        })),
        Err(error) if is_runtime_transport_failure(&error.to_string()) => Ok(json!({
            "state": "failed",
            "capability_id": capability.id,
            "input": input,
            "error": error.to_string()
        })),
        Err(error) => Ok(json!({
            "state": "passed",
            "capability_id": capability.id,
            "input": input,
            "outcome": "error_response",
            "error": error.to_string()
        })),
    }
}

fn is_runtime_transport_failure(error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();
    [
        "failed to start plugin",
        "plugin timed out",
        "plugin output channel closed",
        "plugin returned empty response",
        "plugin exited with status",
        "invalid json",
        "plugin input",
        "plugin capability not found",
        "plugin not found or unavailable",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn sample_input(schema: &Value) -> Value {
    let Some(object) = schema.as_object() else {
        return json!({});
    };
    if object.get("type").and_then(Value::as_str) != Some("object") {
        return json!({});
    }
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required = object
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut input = serde_json::Map::new();
    for name in required {
        if let Some(property) = properties.get(name) {
            input.insert(name.to_string(), sample_value(name, property));
        }
    }
    Value::Object(input)
}

fn sample_value(name: &str, schema: &Value) -> Value {
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if let Some(value) = values.first() {
            return value.clone();
        }
    }
    match schema
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("string")
    {
        "string" => {
            if name.ends_with("path") || name.ends_with("_root") {
                if name.ends_with("path") {
                    let path =
                        env::temp_dir().join(format!("himind-plugin-contract-{}.txt", now_stamp()));
                    let _ = fs::write(&path, "HiMind plugin contract sample\n");
                    Value::String(path.to_string_lossy().to_string())
                } else {
                    Value::String(env::temp_dir().to_string_lossy().to_string())
                }
            } else {
                Value::String("sample".to_string())
            }
        }
        "integer" | "number" => schema.get("minimum").cloned().unwrap_or_else(|| json!(1)),
        "boolean" => json!(false),
        "array" => json!([]),
        "object" => json!({}),
        _ => Value::Null,
    }
}

pub(crate) fn confirm(plugin_id: &str, version: &str) -> Result<PluginDraft, Box<dyn Error>> {
    let mut draft = read(plugin_id, version)?;
    ensure_candidate_unchanged(&draft)?;
    if draft.tested_at.is_none() {
        return Err("请先完成插件候选包测试".into());
    }
    draft.confirmed_at = Some(now_stamp());
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(draft)
}

pub(crate) fn submit(
    options: &Options,
    agent_id: &str,
    plugin_id: &str,
    version: &str,
) -> Result<PluginDraft, Box<dyn Error>> {
    let mut draft = read(plugin_id, version)?;
    ensure_ready_to_submit(&draft)?;
    if agent_id.trim().is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".into());
    }
    let access = crate::api::oauth::platform_access_token(
        options,
        crate::api::oauth::CREATIVE_SUBMIT_SCOPE,
    )?;
    let mut report = draft.test_report.clone().unwrap_or_else(|| json!({}));
    report["candidate_sha256"] = json!(draft.candidate_sha256);
    report["agent_version"] = json!(VERSION);
    report["tested_at"] = json!(draft.tested_at);
    report["built_at"] = json!(draft.updated_at);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let source = crate::extension_projects::submission_source(
        crate::extension_projects::ExtensionProjectKind::Plugin,
        plugin_id,
    )?;
    let submitted = crate::api::distribution::submit_plugin(
        &client,
        &options.api_base,
        agent_id,
        &access.token,
        &draft.candidate_path,
        &report,
        draft.revision_of.as_deref(),
        &source,
    )?;
    draft.dashboard_submission_id = submitted
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    draft.submitted_at = Some(now_stamp());
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(draft)
}

fn ensure_ready_to_submit(draft: &PluginDraft) -> Result<(), Box<dyn Error>> {
    ensure_candidate_unchanged(draft)
}

fn read_archive_manifest(path: &Path) -> Result<PluginManifest, Box<dyn Error>> {
    let mut archive = ZipArchive::new(File::open(path)?)?;
    let mut entry = archive.by_name("plugin.json")?;
    let mut content = String::new();
    std::io::Read::read_to_string(&mut entry, &mut content)?;
    Ok(parse_plugin_manifest(&content)?)
}

fn extract_and_validate(
    path: &Path,
    root: &Path,
    manifest: &PluginManifest,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(root)?;
    let mut archive = ZipArchive::new(File::open(path)?)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = entry
            .enclosed_name()
            .ok_or("插件 ZIP 包含非法路径")?
            .to_path_buf();
        let output = root.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        std::io::copy(&mut entry, &mut File::create(output)?)?;
    }
    verify_plugin_checksums(root)?;
    let parsed = parse_plugin_manifest(&fs::read_to_string(root.join("plugin.json"))?)?;
    if parsed.id != manifest.id || parsed.version != manifest.version {
        return Err("插件候选包 Manifest 在测试期间发生变化".into());
    }
    validate_manifest_contributions(root, &parsed)?;
    validate_development_entry(root, &parsed)?;
    Ok(())
}

fn validate_identity(manifest: &PluginManifest) -> Result<(), Box<dyn Error>> {
    validate_identifier(&manifest.id)?;
    validate_version(&manifest.version)?;
    if manifest.name.trim().is_empty() {
        return Err("插件中文名称不能为空".into());
    }
    if manifest.release_notes.trim().is_empty() {
        return Err("请在 plugin.json 中填写本版本更新说明 release_notes".into());
    }
    for dependency in &manifest.plugin_dependencies {
        validate_identifier(&dependency.plugin_id)?;
        if dependency.plugin_id == manifest.id {
            return Err("插件不能依赖自身".into());
        }
        if !dependency.min_version.trim().is_empty() {
            validate_version(&dependency.min_version)?;
        }
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), Box<dyn Error>> {
    if value.trim().is_empty()
        || value.split('.').any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        return Err(format!("插件 ID 无效: {value}").into());
    }
    Ok(())
}

fn validate_version(value: &str) -> Result<(), Box<dyn Error>> {
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 3 || parts.iter().any(|part| part.parse::<u64>().is_err()) {
        return Err(format!("插件版本无效: {value}").into());
    }
    Ok(())
}

fn ensure_candidate_unchanged(draft: &PluginDraft) -> Result<(), Box<dyn Error>> {
    if sha256_file(&draft.candidate_path)? != draft.candidate_sha256 {
        return Err("插件候选包已变化，请重新保存并测试".into());
    }
    Ok(())
}

fn persist(draft: &PluginDraft) -> Result<(), Box<dyn Error>> {
    let root = draft_version_root(&draft.manifest.id, &draft.manifest.version);
    fs::create_dir_all(&root)?;
    fs::write(root.join("draft.json"), serde_json::to_vec_pretty(draft)?)?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

fn drafts_root() -> PathBuf {
    if let Some(root) = env::var_os("HIMIND_PLUGIN_DRAFTS_DIR") {
        return PathBuf::from(root);
    }
    crate::store::paths::agent_home().join("plugin-drafts")
}

fn draft_version_root(plugin_id: &str, version: &str) -> PathBuf {
    drafts_root().join(plugin_id).join(version)
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::FileOptions;

    #[test]
    fn rejects_invalid_plugin_identity_and_version() {
        assert!(validate_identifier("../plugin").is_err());
        assert!(validate_identifier("com.himind.valid-plugin").is_ok());
        assert!(validate_version("1.2.3").is_ok());
        assert!(validate_version("1.2").is_err());
    }

    #[test]
    fn saves_tests_and_confirms_complete_plugin_candidate() {
        let root = env::temp_dir().join(format!("himind-plugin-authoring-test-{}", now_stamp()));
        let drafts = root.join("drafts");
        let development_registry = root.join("plugin-development.json");
        fs::create_dir_all(&root).unwrap();
        env::set_var("HIMIND_PLUGIN_DRAFTS_DIR", &drafts);
        env::set_var("HIMIND_PLUGIN_DEVELOPMENT_REGISTRY", &development_registry);
        let package = root.join("candidate.hmpkg");
        let manifest = r#"{"id":"com.himind.authoring-test","name":"候选测试插件","author":"测试用户","description":"测试插件候选链路","release_notes":"新增候选包保存、测试与修订验证。","version":"1.0.0","entry":"plugin.exe","runtime":"process-jsonrpc-stdio","min_agent_version":"0.3.0","governance":"optional","capabilities":[],"permissions":[],"contributes":{"commands":[],"views":[]}}"#
            .as_bytes();
        let entry = b"test-binary";
        let checksums = format!(
            "{:x}  plugin.exe\n{:x}  plugin.json\n",
            Sha256::digest(entry),
            Sha256::digest(manifest)
        );
        let file = File::create(&package).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        let options = FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, content) in [
            ("plugin.exe", entry.as_slice()),
            ("plugin.json", manifest),
            ("checksums.sha256", checksums.as_bytes()),
        ] {
            archive.start_file(name, options).unwrap();
            archive.write_all(content).unwrap();
        }
        archive.finish().unwrap();

        let saved = save(PluginDraftInput {
            package_path: package,
            revision_of_version: None,
            parent_submission_id: None,
        })
        .unwrap();
        assert!(saved.candidate_path.exists());
        assert!(saved
            .development_path
            .as_ref()
            .is_some_and(|path| path.exists()));
        assert!(fs::read_to_string(&development_registry)
            .unwrap()
            .contains("com.himind.authoring-test"));
        assert!(ensure_ready_to_submit(&saved).is_ok());
        let tested = test(&saved.manifest.id, &saved.manifest.version).unwrap();
        assert!(tested.tested_at.is_some());
        let confirmed = confirm(&saved.manifest.id, &saved.manifest.version).unwrap();
        assert!(confirmed.confirmed_at.is_some());
        assert!(ensure_ready_to_submit(&confirmed).is_ok());
        let revision = create_revision(&saved.manifest.id, &saved.manifest.version).unwrap();
        assert_eq!(revision.manifest.version, "1.0.1");
        assert_eq!(revision.revision_of.as_deref(), Some("1.0.0"));
        assert!(revision.tested_at.is_none());
        extract_and_validate(
            &revision.candidate_path,
            &root.join("revision-check"),
            &revision.manifest,
        )
        .unwrap();

        env::remove_var("HIMIND_PLUGIN_DRAFTS_DIR");
        env::remove_var("HIMIND_PLUGIN_DEVELOPMENT_REGISTRY");
        let _ = fs::remove_dir_all(root);
    }
}
