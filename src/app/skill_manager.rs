use crate::api::distribution::{skill_catalog, skill_versions, SkillCatalogItem};
use crate::app::plugin_manager;
use crate::app::system::verify_extension_artifact_signature;
use crate::skill::manifest::{validate_relative_package_path, validate_skill_package_root};
use crate::skill::resolver::compare_versions;
use crate::skill::store::{SkillManagementPolicy, SkillStore};
use crate::skill::types::SkillRecord;
use crate::Options;
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::ZipArchive;

const MAX_SKILL_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SKILL_EXTRACTED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SKILL_ARCHIVE_ENTRIES: usize = 20_000;

pub(crate) type SkillPluginInstallAction = plugin_manager::PluginDependencyAction;

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SkillInstallPlan {
    pub skill: SkillCatalogItem,
    pub plugin_actions: Vec<SkillPluginInstallAction>,
    pub blocked_reasons: Vec<String>,
    pub ready: bool,
}

pub(crate) fn catalog(
    options: &Options,
    agent_id: &str,
) -> Result<Vec<SkillCatalogItem>, Box<dyn Error>> {
    let credential = options.agent_credential();
    if agent_id.trim().is_empty() || credential.trim().is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".into());
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    skill_catalog(&client, &options.api_base, agent_id, &credential)
}

pub(crate) fn install(
    options: &Options,
    agent_id: &str,
    skill_id: &str,
) -> Result<(SkillCatalogItem, SkillRecord), Box<dyn Error>> {
    install_with_dependencies(options, agent_id, skill_id, None, &[])
}

/// Installs a local .hmskill archive or unpacked user-managed Skill package.
pub(crate) fn install_local_package(path: &Path) -> Result<SkillRecord, Box<dyn Error>> {
    install_local_package_from_source(path, "local")
}

pub(crate) fn install_local_package_from_source(
    path: &Path,
    source_kind: &str,
) -> Result<SkillRecord, Box<dyn Error>> {
    let source = path.canonicalize()?;
    if !source.is_file() && !source.is_dir() {
        return Err("本地 Skill 路径不存在".into());
    }
    let staging = env::temp_dir().join(format!("himind-local-skill-{}", unique_suffix()));
    let package_root = if source.is_dir() {
        source.clone()
    } else {
        if !source
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("hmskill"))
        {
            return Err("本地 Skill 文件必须是 .hmskill".into());
        }
        if fs::metadata(&source)?.len() > MAX_SKILL_ARCHIVE_BYTES {
            return Err("本地 Skill 包超过 16 MiB 限制".into());
        }
        extract_archive(&source, &staging)?;
        staging.clone()
    };
    let result: Result<SkillRecord, Box<dyn Error>> = (|| {
        validate_package_size(&package_root)?;
        verify_checksums(&package_root)?;
        verify_declared_contents(&package_root)?;
        let manifest = validate_skill_package_root(&package_root)?;
        let store = SkillStore::new();
        match manifest.scope {
            crate::skill::types::SkillScope::User => {
                store.install_user_package(&package_root, &manifest.id, &manifest.version)
            }
            crate::skill::types::SkillScope::Organization => {
                // Public GitHub packages may use the marketplace's
                // organization scope. Imported copies are still local,
                // user-managed assets and never receive Dashboard policy.
                let record = store.install_organization_package(
                    &package_root,
                    &manifest.id,
                    &manifest.version,
                )?;
                store.apply_management_policy(
                    &manifest.id,
                    &SkillManagementPolicy {
                        management: "user_managed".to_string(),
                        source: source_kind.trim().to_string(),
                        assignment_id: String::new(),
                        reason: "从 GitHub 导入".to_string(),
                        allow_uninstall: true,
                    },
                )?;
                Ok(record)
            }
            crate::skill::types::SkillScope::Builtin => {
                Err("本地 Skill 不允许覆盖内置 Skill".into())
            }
        }
    })();
    if staging.exists() {
        let _ = fs::remove_dir_all(staging);
    }
    let record = result?;
    crate::app::extension_lock::record_local_skill(&record.manifest, source_kind)?;
    Ok(record)
}

/// Installs a Skill release from a configured public extension source.
/// Public sources never receive organization management authority.
pub(crate) fn install_public_catalog_item(
    item: &SkillCatalogItem,
    require_signature: bool,
) -> Result<SkillRecord, Box<dyn Error>> {
    if item.management != "user_managed" || item.assignment != "optional" || item.managed {
        return Err("公共扩展源不能授予组织管理策略".into());
    }
    ensure_supported(item)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .user_agent("HiMind-Agent")
        .build()?;
    let archive = download_public(&client, item, require_signature)?;
    let staging = env::temp_dir().join(format!("himind-public-skill-{}", unique_suffix()));
    let result: Result<SkillRecord, Box<dyn Error>> = (|| {
        extract_archive(&archive, &staging)?;
        validate_package_size(&staging)?;
        verify_checksums(&staging)?;
        verify_declared_contents(&staging)?;
        let manifest = validate_skill_package_root(&staging)?;
        if manifest.id != item.skill_id || manifest.version != item.version {
            return Err("Skill Manifest ID 或版本与扩展源记录不一致".into());
        }
        let store = SkillStore::new();
        let record = match manifest.scope {
            crate::skill::types::SkillScope::User => {
                store.install_user_package(&staging, &manifest.id, &manifest.version)?
            }
            crate::skill::types::SkillScope::Organization => {
                let record = store.install_organization_package(
                    &staging,
                    &manifest.id,
                    &manifest.version,
                )?;
                store.apply_management_policy(
                    &manifest.id,
                    &SkillManagementPolicy {
                        management: "user_managed".to_string(),
                        source: item.source.clone(),
                        assignment_id: String::new(),
                        reason: "来自 GitHub 扩展源".to_string(),
                        allow_uninstall: true,
                    },
                )?;
                record
            }
            crate::skill::types::SkillScope::Builtin => {
                return Err("公共扩展源不允许覆盖内置 Skill".into())
            }
        };
        Ok(record)
    })();
    let _ = fs::remove_file(archive);
    let _ = fs::remove_dir_all(staging);
    let record = result?;
    crate::app::extension_lock::record_skill(item)?;
    Ok(record)
}

pub(crate) fn plan_install(
    options: &Options,
    agent_id: &str,
    skill_id: &str,
    version: Option<&str>,
) -> Result<SkillInstallPlan, Box<dyn Error>> {
    let credential = options.agent_credential();
    if agent_id.trim().is_empty() || credential.trim().is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".into());
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let item = catalog_item(&client, options, agent_id, skill_id, version)?;
    ensure_supported(&item)?;
    let (plugin_actions, blocked_reasons) = plugin_manager::plan_dependency_set(
        options,
        agent_id,
        &item.plugin_dependencies,
        &item.name,
    )?;
    Ok(SkillInstallPlan {
        skill: item,
        plugin_actions,
        ready: blocked_reasons.is_empty(),
        blocked_reasons,
    })
}

pub(crate) fn install_with_dependencies(
    options: &Options,
    agent_id: &str,
    skill_id: &str,
    version: Option<&str>,
    selected_optional_plugins: &[String],
) -> Result<(SkillCatalogItem, SkillRecord), Box<dyn Error>> {
    let plan = plan_install(options, agent_id, skill_id, version)?;
    if !plan.ready {
        return Err(format!("Skill 安装计划被阻止: {}", plan.blocked_reasons.join(", ")).into());
    }
    let mut plugin_changes = Vec::new();
    let mut selected_plugin_ids = Vec::new();
    for action in &plan.plugin_actions {
        let selected = action.required
            || selected_optional_plugins
                .iter()
                .any(|plugin_id| plugin_id == &action.plugin_id);
        if !selected || !matches!(action.action.as_str(), "install" | "update") {
            if selected {
                selected_plugin_ids.push(action.plugin_id.clone());
            }
            continue;
        }
        selected_plugin_ids.push(action.plugin_id.clone());
        let before = plugin_manager::local_status(&action.plugin_id);
        if let Err(error) = plugin_manager::install(options, agent_id, &action.plugin_id, None) {
            compensate_plugin_changes(&plugin_changes);
            return Err(error);
        }
        plugin_changes.push((action.plugin_id.clone(), before));
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let item = plan.skill;
    ensure_supported(&item)?;
    let archive = download(&client, options, agent_id, &item)?;
    let staging = env::temp_dir().join(format!("himind-skill-unpack-{}", unique_suffix()));
    let owner = format!("skill:{skill_id}");
    let previous_references = plugin_manager::owner_dependency_ids(&owner);
    if let Err(error) = plugin_manager::set_owner_references(&owner, &selected_plugin_ids) {
        let _ = fs::remove_file(&archive);
        compensate_plugin_changes(&plugin_changes);
        return Err(format!("记录 Skill 插件依赖失败：{error}").into());
    }
    let result: Result<(SkillCatalogItem, SkillRecord), Box<dyn Error>> = (|| {
        extract_archive(&archive, &staging)?;
        verify_checksums(&staging)?;
        verify_declared_contents(&staging)?;
        let store = SkillStore::new();
        let record = store.install_organization_package(&staging, &item.skill_id, &item.version)?;
        store.apply_management_policy(
            &item.skill_id,
            &SkillManagementPolicy {
                management: item.management.clone(),
                source: item.source.clone(),
                assignment_id: String::new(),
                reason: item.organization_reason.clone(),
                allow_uninstall: item.allow_uninstall,
            },
        )?;
        Ok((item.clone(), record))
    })();
    let _ = fs::remove_file(archive);
    let _ = fs::remove_dir_all(staging);
    if result.is_err() {
        let _ = plugin_manager::set_owner_references(&owner, &previous_references);
        compensate_plugin_changes(&plugin_changes);
    }
    let result = result?;
    crate::app::extension_lock::record_skill(&result.0)?;
    Ok(result)
}

fn compensate_plugin_changes(changes: &[(String, plugin_manager::LocalPluginStatus)]) {
    for (plugin_id, before) in changes.iter().rev() {
        let current = plugin_manager::local_status(plugin_id);
        if before.current_version.is_empty() {
            let _ = plugin_manager::remove_for_policy(plugin_id);
        } else if current.current_version != before.current_version {
            let _ = plugin_manager::rollback(plugin_id);
        }
        if !before.enabled {
            let _ = plugin_manager::set_enabled(plugin_id, false);
        }
    }
}

fn catalog_item(
    client: &Client,
    options: &Options,
    agent_id: &str,
    skill_id: &str,
    version: Option<&str>,
) -> Result<SkillCatalogItem, Box<dyn Error>> {
    let credential = options.agent_credential();
    if agent_id.trim().is_empty() || credential.trim().is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".into());
    }
    let latest = skill_catalog(client, &options.api_base, agent_id, &credential)?
        .into_iter()
        .find(|item| item.skill_id == skill_id)
        .ok_or_else(|| "Skill 未上架或当前不可用".to_string())?;
    let Some(version) = version.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(latest);
    };
    if latest.version == version {
        return Ok(latest);
    }
    if latest.management != "user_managed" || latest.managed {
        return Err("该 Skill 由组织管理，不能切换版本".into());
    }
    skill_versions(client, &options.api_base, agent_id, &credential, skill_id)?
        .into_iter()
        .find(|item| item.version == version)
        .ok_or_else(|| format!("Skill 版本 v{version} 不可用").into())
}

fn ensure_supported(item: &SkillCatalogItem) -> Result<(), Box<dyn Error>> {
    if item.assignment == "blocked" {
        return Err("该 Skill 已被组织禁止安装".into());
    }
    if !item.supported_clients.iter().any(|client| {
        client.eq_ignore_ascii_case(crate::skill::clients::PORTABLE_PROFILE_ID)
            || crate::skill::clients::is_portable_client(client)
    }) {
        return Err("该 Skill 当前不支持本机已实现的 AI 客户端适配器".into());
    }
    if !item.min_agent_version.trim().is_empty()
        && compare_versions(crate::VERSION, &item.min_agent_version) == Ordering::Less
    {
        return Err(format!(
            "当前 Agent {} 不满足 Skill 最低版本 {}",
            crate::VERSION,
            item.min_agent_version
        )
        .into());
    }
    Ok(())
}

fn download(
    client: &Client,
    options: &Options,
    agent_id: &str,
    item: &SkillCatalogItem,
) -> Result<PathBuf, Box<dyn Error>> {
    if item.file_size == 0 || item.file_size > MAX_SKILL_ARCHIVE_BYTES {
        return Err("Skill 制品大小无效或超过 16 MiB 限制".into());
    }
    let api = url::Url::parse(&options.api_base)?;
    let url = url::Url::parse(&item.download_url)?;
    if api.scheme() != url.scheme()
        || api.host_str() != url.host_str()
        || api.port_or_known_default() != url.port_or_known_default()
    {
        return Err("Skill 制品下载地址必须与 Dashboard 同源".into());
    }
    let mut response = client
        .get(url)
        .header(
            "Authorization",
            format!("Agent {agent_id}:{}", options.agent_credential()),
        )
        .send()?
        .error_for_status()?;
    let path = env::temp_dir().join(format!("himind-skill-{}.hmskill", unique_suffix()));
    let mut file = File::create(&path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_SKILL_ARCHIVE_BYTES || total > item.file_size {
            let _ = fs::remove_file(&path);
            return Err("Skill 制品实际大小超过发布记录".into());
        }
        file.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    file.flush()?;
    if total != item.file_size {
        let _ = fs::remove_file(&path);
        return Err("Skill 制品实际大小与发布记录不一致".into());
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(&item.sha256) {
        let _ = fs::remove_file(&path);
        return Err("Skill 制品 SHA-256 校验失败".into());
    }
    verify_extension_artifact_signature(
        &path,
        &item.signature,
        &item.signature_key_id,
        &item.signature_algorithm,
        true,
    )?;
    Ok(path)
}

fn download_public(
    client: &Client,
    item: &SkillCatalogItem,
    require_signature: bool,
) -> Result<PathBuf, Box<dyn Error>> {
    if item.file_size == 0 || item.file_size > MAX_SKILL_ARCHIVE_BYTES {
        return Err("Skill 制品大小无效或超过 16 MiB 限制".into());
    }
    let url = url::Url::parse(&item.download_url)?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        return Err("公共 Skill 制品必须使用 github.com 的 HTTPS Release 地址".into());
    }
    let mut response = client.get(url).send()?.error_for_status()?;
    let path = env::temp_dir().join(format!("himind-public-skill-{}.hmskill", unique_suffix()));
    let mut file = File::create(&path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_SKILL_ARCHIVE_BYTES || total > item.file_size {
            let _ = fs::remove_file(&path);
            return Err("Skill 制品实际大小超过发布记录".into());
        }
        file.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    file.flush()?;
    if total != item.file_size {
        let _ = fs::remove_file(&path);
        return Err("Skill 制品实际大小与发布记录不一致".into());
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(&item.sha256) {
        let _ = fs::remove_file(&path);
        return Err("Skill 制品 SHA-256 校验失败".into());
    }
    verify_extension_artifact_signature(
        &path,
        &item.signature,
        &item.signature_key_id,
        &item.signature_algorithm,
        require_signature,
    )?;
    Ok(path)
}

pub(crate) fn extract_archive(archive_path: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(target)?;
    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    if archive.len() > MAX_SKILL_ARCHIVE_ENTRIES {
        return Err("Skill ZIP 文件数量超过 20000 个限制".into());
    }
    let mut extracted_bytes = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        extracted_bytes = extracted_bytes
            .checked_add(entry.size())
            .ok_or("Skill ZIP 解压大小溢出")?;
        if extracted_bytes > MAX_SKILL_EXTRACTED_BYTES {
            return Err("Skill ZIP 解压后超过 64 MiB 限制".into());
        }
        let relative = entry
            .enclosed_name()
            .ok_or("Skill ZIP 包含非法路径")?
            .to_path_buf();
        validate_relative_package_path(&relative.to_string_lossy())?;
        let output = target.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        std::io::copy(&mut entry, &mut File::create(output)?)?;
    }
    Ok(())
}

pub(crate) fn verify_checksums(root: &Path) -> Result<(), Box<dyn Error>> {
    let checksum_path = root.join("checksums.sha256");
    let content =
        fs::read_to_string(&checksum_path).map_err(|_| "Skill 包缺少 checksums.sha256")?;
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
    let mut actual_files = HashSet::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() || entry.path() == checksum_path {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        actual_files.insert(relative.clone());
        let expected_checksum = expected
            .get(&relative)
            .ok_or_else(|| format!("Skill 文件未包含在 checksums.sha256: {relative}"))?;
        let actual = format!("{:x}", Sha256::digest(fs::read(entry.path())?));
        if &actual != expected_checksum {
            return Err(format!("Skill 文件摘要不匹配: {relative}").into());
        }
    }
    if let Some(missing) = expected.keys().find(|name| !actual_files.contains(*name)) {
        return Err(format!("checksums.sha256 引用了缺失文件: {missing}").into());
    }
    for required in ["skill.json", "SKILL.md"] {
        if !actual_files.contains(required) {
            return Err(format!("Skill 包缺少必需文件: {required}").into());
        }
    }
    Ok(())
}

pub(crate) fn verify_declared_contents(root: &Path) -> Result<(), Box<dyn Error>> {
    let manifest = validate_skill_package_root(root)?;
    let declared = manifest
        .contents
        .iter()
        .map(|value| value.replace('\\', "/"))
        .collect::<HashSet<_>>();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        if relative != "checksums.sha256" && !declared.contains(&relative) {
            return Err(format!("Skill 包含 Manifest 未声明的文件: {relative}").into());
        }
    }
    Ok(())
}

fn validate_package_size(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut file_count = 0_usize;
    let mut total_bytes = 0_u64;
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        file_count += 1;
        if file_count > MAX_SKILL_ARCHIVE_ENTRIES {
            return Err("Skill 包文件数量超过 20000 个限制".into());
        }
        total_bytes = total_bytes
            .checked_add(entry.metadata()?.len())
            .ok_or("Skill 包大小溢出")?;
        if total_bytes > MAX_SKILL_EXTRACTED_BYTES {
            return Err("Skill 包内容超过 64 MiB 限制".into());
        }
    }
    Ok(())
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{verify_checksums, verify_declared_contents};
    use crate::api::distribution::SkillCatalogItem;
    use crate::app::system::verify_extension_artifact_signature;
    use sha2::{Digest, Sha256};
    use std::fs;

    fn catalog_skill() -> SkillCatalogItem {
        SkillCatalogItem {
            skill_id: "com.himind.skill.unsigned".to_string(),
            name: "未签名 Skill".to_string(),
            description: String::new(),
            author_name: String::new(),
            categories: vec![],
            version: "1.0.0".to_string(),
            release_notes: String::new(),
            published_at: String::new(),
            min_agent_version: String::new(),
            supported_clients: vec!["codex".to_string(), "github-copilot".to_string()],
            capability_ids: vec![],
            plugin_dependencies: vec![],
            risk_summary: String::new(),
            channel: "stable".to_string(),
            artifact_id: String::new(),
            file_name: "skill.hmskill".to_string(),
            file_size: 1,
            sha256: "0".repeat(64),
            signature: String::new(),
            signature_key_id: String::new(),
            signature_algorithm: String::new(),
            download_url: "https://github.com/Owner/repo/releases/download/v1/skill.hmskill"
                .to_string(),
            source: "github:test".to_string(),
            assignment: "optional".to_string(),
            management: "user_managed".to_string(),
            install_mode: "prompt".to_string(),
            organization_reason: String::new(),
            managed: false,
            allow_disable: true,
            allow_uninstall: true,
        }
    }

    #[test]
    fn unsigned_skill_is_allowed_only_when_the_source_policy_is_optional() {
        let path = std::env::temp_dir().join(format!(
            "himind-skill-signature-policy-test-{}",
            super::unique_suffix()
        ));
        fs::write(&path, b"unsigned skill").unwrap();
        let mut item = catalog_skill();

        assert!(verify_extension_artifact_signature(
            &path,
            &item.signature,
            &item.signature_key_id,
            &item.signature_algorithm,
            false
        )
        .is_ok());
        assert!(verify_extension_artifact_signature(
            &path,
            &item.signature,
            &item.signature_key_id,
            &item.signature_algorithm,
            true
        )
        .is_err());

        item.signature = "c2ln".to_string();
        item.signature_key_id = "missing-key".to_string();
        item.signature_algorithm = "rsa-pss-sha256".to_string();
        assert!(verify_extension_artifact_signature(
            &path,
            &item.signature,
            &item.signature_key_id,
            &item.signature_algorithm,
            false
        )
        .is_err());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn verifies_skill_checksums_and_rejects_unlisted_files() {
        let root = std::env::temp_dir().join(format!(
            "himind-skill-manager-test-{}",
            super::unique_suffix()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("skill.json"), b"{}").unwrap();
        fs::write(root.join("SKILL.md"), b"# Demo").unwrap();
        let checksums = format!(
            "{:x}  SKILL.md\n{:x}  skill.json\n",
            Sha256::digest(b"# Demo"),
            Sha256::digest(b"{}")
        );
        fs::write(root.join("checksums.sha256"), checksums).unwrap();
        assert!(verify_checksums(&root).is_ok());
        fs::write(root.join("unexpected.txt"), b"not listed").unwrap();
        assert!(verify_checksums(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_content_not_declared_by_skill_manifest() {
        let root = std::env::temp_dir().join(format!(
            "himind-skill-content-test-{}",
            super::unique_suffix()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("skill.json"), br#"{"id":"com.himind.skill.demo","name":"Demo","version":"1.0.0","scope":"organization","supported_clients":["codex"],"contents":["skill.json","SKILL.md"]}"#).unwrap();
        fs::write(root.join("SKILL.md"), b"# Demo").unwrap();
        fs::write(root.join("extra.md"), b"extra").unwrap();
        assert!(verify_declared_contents(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
