use crate::app::plugin_manager::verify_plugin_checksums;
use crate::capability::plugin::{
    parse_plugin_manifest, validate_development_entry, validate_manifest_contributions,
    PluginManifest,
};
use crate::{Options, VERSION};
use serde::{Deserialize, Serialize};
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PluginDraft {
    pub manifest: PluginManifest,
    pub candidate_path: PathBuf,
    pub candidate_sha256: String,
    pub tested_at: Option<String>,
    pub confirmed_at: Option<String>,
    pub submitted_at: Option<String>,
    pub dashboard_submission_id: Option<String>,
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
    fs::create_dir_all(&root)?;
    let candidate_path = root.join(format!("{}-{}.hmpkg", manifest.id, manifest.version));
    fs::copy(&source, &candidate_path)?;
    let package_root = root.join("package");
    if package_root.exists() {
        fs::remove_dir_all(&package_root)?;
    }
    extract_and_validate(&candidate_path, &package_root, &manifest)?;
    let candidate_sha256 = sha256_file(&candidate_path)?;
    let previous = read(&manifest.id, &manifest.version).ok();
    let unchanged = previous
        .as_ref()
        .map(|draft| draft.candidate_sha256 == candidate_sha256)
        .unwrap_or(false);
    let draft = PluginDraft {
        manifest,
        candidate_path,
        candidate_sha256,
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

pub(crate) fn test(plugin_id: &str, version: &str) -> Result<PluginDraft, Box<dyn Error>> {
    let mut draft = read(plugin_id, version)?;
    ensure_candidate_unchanged(&draft)?;
    let root = draft_version_root(plugin_id, version).join("test-package");
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    extract_and_validate(&draft.candidate_path, &root, &draft.manifest)?;
    draft.tested_at = Some(now_stamp());
    draft.confirmed_at = None;
    draft.submitted_at = None;
    draft.dashboard_submission_id = None;
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(draft)
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
    let credential = options.agent_credential();
    if agent_id.trim().is_empty() || credential.trim().is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".into());
    }
    let report = serde_json::json!({
        "candidate_sha256": draft.candidate_sha256,
        "agent_version": VERSION,
        "tested_at": draft.tested_at,
        "confirmed_at": draft.confirmed_at,
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let submitted = crate::api::distribution::submit_plugin(
        &client,
        &options.api_base,
        agent_id,
        &credential,
        &draft.candidate_path,
        &report,
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
    ensure_candidate_unchanged(draft)?;
    if draft.tested_at.is_none() || draft.confirmed_at.is_none() {
        return Err("插件候选包尚未完成测试和用户确认".into());
    }
    Ok(())
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
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("HiMindAgent")
        .join("plugin-drafts")
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

    #[test]
    fn rejects_invalid_plugin_identity_and_version() {
        assert!(validate_identifier("../plugin").is_err());
        assert!(validate_identifier("com.himind.valid-plugin").is_ok());
        assert!(validate_version("1.2.3").is_ok());
        assert!(validate_version("1.2").is_err());
    }
}
