use crate::api::distribution::{plugin_catalog, skill_catalog, SkillCatalogItem};
use crate::app::plugin_manager;
use crate::app::system::{validate_signature_metadata, verify_rsa_pss_sha256};
use crate::skill::manifest::{validate_relative_package_path, validate_skill_package_root};
use crate::skill::resolver::compare_versions;
use crate::skill::store::SkillStore;
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

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SkillPluginInstallAction {
    pub plugin_id: String,
    pub required: bool,
    pub current_version: String,
    pub target_version: String,
    pub action: String,
    pub reason: String,
}

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
    install_with_dependencies(options, agent_id, skill_id, &[])
}

pub(crate) fn plan_install(
    options: &Options,
    agent_id: &str,
    skill_id: &str,
) -> Result<SkillInstallPlan, Box<dyn Error>> {
    let credential = options.agent_credential();
    if agent_id.trim().is_empty() || credential.trim().is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".into());
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let item = catalog_item(&client, options, agent_id, skill_id)?;
    ensure_supported(&item)?;
    let plugins = plugin_catalog(&client, &options.api_base, agent_id, &credential)?;
    let mut plugin_actions = Vec::new();
    let mut blocked_reasons = Vec::new();
    for dependency in &item.plugin_dependencies {
        let local = plugin_manager::local_status(&dependency.plugin_id);
        let available = plugins
            .iter()
            .find(|plugin| plugin.plugin_id == dependency.plugin_id);
        let minimum = dependency.min_version.trim();
        let satisfied = !local.current_version.is_empty()
            && (minimum.is_empty()
                || compare_versions(&local.current_version, minimum) != Ordering::Less);
        let (action, target_version, reason) = if satisfied {
            ("satisfied", local.current_version.clone(), "本机版本已满足")
        } else if let Some(plugin) = available {
            if plugin.governance == "blocked" {
                if dependency.required {
                    blocked_reasons.push(format!("插件 {} 被组织策略阻止", dependency.plugin_id));
                }
                ("blocked", plugin.version.clone(), "组织策略阻止安装")
            } else if !minimum.is_empty()
                && compare_versions(&plugin.version, minimum) == Ordering::Less
            {
                if dependency.required {
                    blocked_reasons.push(format!(
                        "插件 {} 商城版本 {} 低于要求 {}",
                        dependency.plugin_id, plugin.version, minimum
                    ));
                }
                ("blocked", plugin.version.clone(), "商城版本不满足最低要求")
            } else if local.current_version.is_empty() {
                ("install", plugin.version.clone(), "安装 Skill 必需的插件")
            } else {
                (
                    "update",
                    plugin.version.clone(),
                    "升级到 Skill 要求的插件版本",
                )
            }
        } else {
            if dependency.required {
                blocked_reasons.push(format!("商城缺少必需插件 {}", dependency.plugin_id));
            }
            ("unavailable", String::new(), "插件未上架")
        };
        plugin_actions.push(SkillPluginInstallAction {
            plugin_id: dependency.plugin_id.clone(),
            required: dependency.required,
            current_version: local.current_version,
            target_version,
            action: action.to_string(),
            reason: reason.to_string(),
        });
    }
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
    selected_optional_plugins: &[String],
) -> Result<(SkillCatalogItem, SkillRecord), Box<dyn Error>> {
    let plan = plan_install(options, agent_id, skill_id)?;
    if !plan.ready {
        return Err(format!("Skill 安装计划被阻止: {}", plan.blocked_reasons.join(", ")).into());
    }
    for action in &plan.plugin_actions {
        let selected = action.required
            || selected_optional_plugins
                .iter()
                .any(|plugin_id| plugin_id == &action.plugin_id);
        if selected && matches!(action.action.as_str(), "install" | "update") {
            plugin_manager::install(options, agent_id, &action.plugin_id)?;
        }
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let item = plan.skill;
    ensure_supported(&item)?;
    let archive = download(&client, options, agent_id, &item)?;
    let staging = env::temp_dir().join(format!("himind-skill-unpack-{}", unique_suffix()));
    let result = (|| {
        extract_archive(&archive, &staging)?;
        verify_checksums(&staging)?;
        verify_declared_contents(&staging)?;
        let record = SkillStore::new().install_organization_package(
            &staging,
            &item.skill_id,
            &item.version,
        )?;
        Ok((item.clone(), record))
    })();
    let _ = fs::remove_file(archive);
    let _ = fs::remove_dir_all(staging);
    result
}

fn catalog_item(
    client: &Client,
    options: &Options,
    agent_id: &str,
    skill_id: &str,
) -> Result<SkillCatalogItem, Box<dyn Error>> {
    let credential = options.agent_credential();
    if agent_id.trim().is_empty() || credential.trim().is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".into());
    }
    skill_catalog(client, &options.api_base, agent_id, &credential)?
        .into_iter()
        .find(|item| item.skill_id == skill_id)
        .ok_or_else(|| "Skill 未上架或当前不可用".into())
}

fn ensure_supported(item: &SkillCatalogItem) -> Result<(), Box<dyn Error>> {
    if !item
        .supported_clients
        .iter()
        .any(|client| client.eq_ignore_ascii_case("codex"))
    {
        return Err("该 Skill 当前不支持 Codex".into());
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
    verify_signature(&path, item)?;
    Ok(path)
}

fn verify_signature(path: &Path, item: &SkillCatalogItem) -> Result<(), Box<dyn Error>> {
    validate_signature_metadata(
        &item.signature,
        &item.signature_key_id,
        &item.signature_algorithm,
        true,
    )?;
    let trusted =
        env::var_os("HIMIND_TRUSTED_SIGNING_KEYS_DIR").ok_or("未配置 Skill 商城受信公钥目录")?;
    let pem =
        fs::read_to_string(PathBuf::from(trusted).join(format!("{}.pem", item.signature_key_id)))?;
    verify_rsa_pss_sha256(path, &pem, &item.signature)
}

fn extract_archive(archive_path: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(target)?;
    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
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

fn verify_checksums(root: &Path) -> Result<(), Box<dyn Error>> {
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

fn verify_declared_contents(root: &Path) -> Result<(), Box<dyn Error>> {
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

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{verify_checksums, verify_declared_contents};
    use sha2::{Digest, Sha256};
    use std::fs;

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
