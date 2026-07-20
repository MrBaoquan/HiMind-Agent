use crate::app::plugin_manager;
use crate::skill::manifest::{validate_skill_id, validate_skill_manifest};
use crate::skill::resolver::{compare_versions, CapabilityFact, SkillReadiness};
use crate::skill::types::{
    SkillCapabilityDependency, SkillManifest, SkillPluginDependency, SkillRecord, SkillScope,
};
use crate::Options;
use crate::VERSION;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::write::FileOptions;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillDraftInput {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_agent_version")]
    pub min_agent_version: String,
    #[serde(default = "default_clients")]
    pub supported_clients: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<SkillCapabilityDependency>,
    #[serde(default)]
    pub plugin_dependencies: Vec<SkillPluginDependency>,
    #[serde(default)]
    pub risk_summary: String,
    pub readme: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AuthoringDraft {
    pub manifest: SkillManifest,
    pub readme: String,
    pub candidate_path: PathBuf,
    pub candidate_sha256: String,
    pub tested_at: Option<String>,
    pub confirmed_at: Option<String>,
    pub submitted_at: Option<String>,
    pub dashboard_draft_id: Option<String>,
    pub codex_target: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AuthoringTestResult {
    pub draft: AuthoringDraft,
    pub readiness: SkillReadiness,
    pub plugin_issues: Vec<String>,
    pub codex: serde_json::Value,
}

pub(crate) fn list() -> Result<Vec<AuthoringDraft>, Box<dyn Error>> {
    let root = drafts_root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut drafts = Vec::new();
    for entry in walkdir::WalkDir::new(root).min_depth(3).max_depth(3) {
        let entry = entry?;
        if entry.file_type().is_file() && entry.file_name() == "draft.json" {
            if let Ok(draft) =
                serde_json::from_str::<AuthoringDraft>(&fs::read_to_string(entry.path())?)
            {
                drafts.push(draft);
            }
        }
    }
    drafts.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(drafts)
}

pub(crate) fn save(input: SkillDraftInput) -> Result<AuthoringDraft, Box<dyn Error>> {
    if input.readme.trim().is_empty() {
        return Err("SKILL.md 内容不能为空".into());
    }
    let manifest = SkillManifest {
        id: input.id.trim().to_string(),
        name: input.name.trim().to_string(),
        version: input.version.trim().to_string(),
        scope: SkillScope::Organization,
        description: input.description.trim().to_string(),
        min_agent_version: input.min_agent_version.trim().to_string(),
        supported_clients: input.supported_clients,
        capabilities: input.capabilities,
        plugin_dependencies: input.plugin_dependencies,
        risk_summary: input.risk_summary.trim().to_string(),
        contents: vec!["skill.json".to_string(), "SKILL.md".to_string()],
    };
    validate_skill_manifest(&manifest)?;
    let root = draft_version_root(&manifest.id, &manifest.version);
    let package_root = root.join("package");
    fs::create_dir_all(&package_root)?;
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::write(package_root.join("skill.json"), &manifest_bytes)?;
    fs::write(package_root.join("SKILL.md"), input.readme.as_bytes())?;
    let checksums = package_checksums(&manifest_bytes, input.readme.as_bytes());
    fs::write(package_root.join("checksums.sha256"), checksums.as_bytes())?;
    let candidate_path = root.join(format!("{}-{}.hmskill", manifest.id, manifest.version));
    build_archive(
        &candidate_path,
        &manifest_bytes,
        input.readme.as_bytes(),
        checksums.as_bytes(),
    )?;
    let candidate_sha256 = sha256_file(&candidate_path)?;
    let previous = read(&manifest.id, &manifest.version).ok();
    let unchanged = previous
        .as_ref()
        .map(|draft| draft.candidate_sha256 == candidate_sha256)
        .unwrap_or(false);
    let draft = AuthoringDraft {
        manifest,
        readme: input.readme,
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
        dashboard_draft_id: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.dashboard_draft_id.clone()),
        codex_target: previous
            .as_ref()
            .filter(|_| unchanged)
            .and_then(|value| value.codex_target.clone()),
        updated_at: now_stamp(),
    };
    persist(&draft)?;
    Ok(draft)
}

pub(crate) fn read(skill_id: &str, version: &str) -> Result<AuthoringDraft, Box<dyn Error>> {
    validate_skill_id(skill_id)?;
    if version.trim().is_empty()
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err("Skill 版本无效".into());
    }
    let path = draft_version_root(skill_id, version).join("draft.json");
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

pub(crate) fn test(
    skill_id: &str,
    version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<AuthoringTestResult, Box<dyn Error>> {
    let mut draft = read(skill_id, version)?;
    ensure_candidate_unchanged(&draft)?;
    let readiness = SkillReadiness::resolve(&draft.manifest, capability_facts, VERSION, "codex");
    let plugin_issues = plugin_dependency_issues(&draft.manifest.plugin_dependencies);
    if readiness.state == "blocked" || !plugin_issues.is_empty() {
        return Err(format!(
            "Skill 本地预检未通过: {}{}",
            readiness.reasons.join(", "),
            plugin_issues.join(", ")
        )
        .into());
    }
    let package_root = draft_version_root(skill_id, version).join("package");
    let record = SkillRecord {
        manifest: draft.manifest.clone(),
        root: draft_version_root(skill_id, version),
        version_root: package_root,
        current: true,
        previous_version: None,
    };
    let codex = crate::skill::codex::sync_record_json(&record, VERSION, capability_facts)?;
    draft.tested_at = Some(now_stamp());
    draft.confirmed_at = None;
    draft.submitted_at = None;
    draft.dashboard_draft_id = None;
    draft.codex_target = codex
        .get("target_root")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(AuthoringTestResult {
        draft,
        readiness,
        plugin_issues,
        codex,
    })
}

pub(crate) fn confirm(skill_id: &str, version: &str) -> Result<AuthoringDraft, Box<dyn Error>> {
    let mut draft = read(skill_id, version)?;
    ensure_candidate_unchanged(&draft)?;
    if draft.tested_at.is_none() {
        return Err("请先安装到 Codex 完成本地测试".into());
    }
    draft.confirmed_at = Some(now_stamp());
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(draft)
}

pub(crate) fn mark_submitted(
    skill_id: &str,
    version: &str,
    dashboard_draft_id: &str,
) -> Result<AuthoringDraft, Box<dyn Error>> {
    let mut draft = read(skill_id, version)?;
    ensure_ready_to_submit(&draft)?;
    draft.submitted_at = Some(now_stamp());
    draft.dashboard_draft_id = Some(dashboard_draft_id.to_string());
    draft.updated_at = now_stamp();
    persist(&draft)?;
    Ok(draft)
}

pub(crate) fn submit(
    options: &Options,
    agent_id: &str,
    skill_id: &str,
    version: &str,
) -> Result<AuthoringDraft, Box<dyn Error>> {
    let draft = read(skill_id, version)?;
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
        "codex_target": draft.codex_target,
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let submitted = crate::api::distribution::submit_skill(
        &client,
        &options.api_base,
        agent_id,
        &credential,
        &draft.candidate_path,
        &report,
    )?;
    let dashboard_draft_id = submitted
        .get("id")
        .and_then(|value| value.as_str())
        .ok_or("Dashboard 未返回 Skill 审核记录")?;
    mark_submitted(skill_id, version, dashboard_draft_id)
}

pub(crate) fn ensure_ready_to_submit(draft: &AuthoringDraft) -> Result<(), Box<dyn Error>> {
    ensure_candidate_unchanged(draft)?;
    if draft.tested_at.is_none() || draft.confirmed_at.is_none() {
        return Err("Skill 尚未完成本地测试确认".into());
    }
    Ok(())
}

fn plugin_dependency_issues(dependencies: &[SkillPluginDependency]) -> Vec<String> {
    dependencies
        .iter()
        .filter_map(|dependency| {
            let status = plugin_manager::local_status(&dependency.plugin_id);
            if !dependency.required {
                return None;
            }
            if status.current_version.is_empty() {
                return Some(format!("缺少必需插件 {}", dependency.plugin_id));
            }
            if let Some(minimum) = dependency.min_version.as_deref() {
                if compare_versions(&status.current_version, minimum) == Ordering::Less {
                    return Some(format!(
                        "插件 {} 版本低于 {}",
                        dependency.plugin_id, minimum
                    ));
                }
            }
            None
        })
        .collect()
}

fn persist(draft: &AuthoringDraft) -> Result<(), Box<dyn Error>> {
    let root = draft_version_root(&draft.manifest.id, &draft.manifest.version);
    fs::create_dir_all(&root)?;
    fs::write(root.join("draft.json"), serde_json::to_vec_pretty(draft)?)?;
    Ok(())
}

fn ensure_candidate_unchanged(draft: &AuthoringDraft) -> Result<(), Box<dyn Error>> {
    if sha256_file(&draft.candidate_path)? != draft.candidate_sha256 {
        return Err("Skill 候选包已变化，请重新保存并测试".into());
    }
    Ok(())
}

fn build_archive(
    target: &Path,
    manifest: &[u8],
    readme: &[u8],
    checksums: &[u8],
) -> Result<(), Box<dyn Error>> {
    let file = File::create(target)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, content) in [
        ("SKILL.md", readme),
        ("skill.json", manifest),
        ("checksums.sha256", checksums),
    ] {
        writer.start_file(name, options)?;
        writer.write_all(content)?;
    }
    writer.finish()?;
    Ok(())
}

fn package_checksums(manifest: &[u8], readme: &[u8]) -> String {
    format!(
        "{:x}  SKILL.md\n{:x}  skill.json\n",
        Sha256::digest(readme),
        Sha256::digest(manifest)
    )
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

fn drafts_root() -> PathBuf {
    if let Some(root) = env::var_os("HIMIND_SKILL_DRAFTS_DIR") {
        return PathBuf::from(root);
    }
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("HiMindAgent")
        .join("skill-drafts")
}

fn draft_version_root(skill_id: &str, version: &str) -> PathBuf {
    drafts_root().join(skill_id).join(version)
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

fn default_agent_version() -> String {
    VERSION.to_string()
}
fn default_clients() -> Vec<String> {
    vec!["codex".to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(readme: &str) -> SkillDraftInput {
        SkillDraftInput {
            id: "com.himind.skill.authoring-test".to_string(),
            name: "Authoring Test".to_string(),
            version: "1.0.0".to_string(),
            description: "Local authoring test".to_string(),
            min_agent_version: VERSION.to_string(),
            supported_clients: vec!["codex".to_string()],
            capabilities: vec![],
            plugin_dependencies: vec![],
            risk_summary: "read_only".to_string(),
            readme: readme.to_string(),
        }
    }

    #[test]
    fn creates_deterministic_candidate_and_invalidates_confirmation_after_edit() {
        let root = env::temp_dir().join(format!("himind-authoring-test-{}", now_stamp()));
        env::set_var("HIMIND_SKILL_DRAFTS_DIR", &root);
        let mut first = save(input("# Demo")).unwrap();
        assert!(first.candidate_path.exists());
        first.tested_at = Some("tested".to_string());
        first.confirmed_at = Some("confirmed".to_string());
        persist(&first).unwrap();
        let unchanged = save(input("# Demo")).unwrap();
        assert_eq!(unchanged.candidate_sha256, first.candidate_sha256);
        assert!(unchanged.confirmed_at.is_some());
        let changed = save(input("# Demo\n\nChanged")).unwrap();
        assert_ne!(changed.candidate_sha256, first.candidate_sha256);
        assert!(changed.tested_at.is_none());
        assert!(changed.confirmed_at.is_none());
        env::remove_var("HIMIND_SKILL_DRAFTS_DIR");
        let _ = fs::remove_dir_all(root);
    }
}
