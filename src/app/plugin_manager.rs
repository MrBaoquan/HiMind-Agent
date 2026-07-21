use crate::api::distribution::{
    plugin_catalog, report_plugin_status, PluginCatalogItem, PluginStatusReport,
};
use crate::app::system::{validate_signature_metadata, verify_rsa_pss_sha256};
use crate::capability::plugin::{is_builtin_plugin, plugin_registry_dir, PluginManifest};
use crate::store::plugin_outbox::{
    list as list_statuses, remove as remove_status, store as store_status, PluginStatusRecord,
};
use crate::Options;
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::ZipArchive;

const MAX_PLUGIN_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct LocalPluginStatus {
    pub current_version: String,
    pub previous_version: String,
    pub enabled: bool,
    pub status: String,
}

pub(crate) fn local_status(plugin_id: &str) -> LocalPluginStatus {
    let Ok(root) = plugin_root(plugin_id) else {
        return LocalPluginStatus::default();
    };
    let current_version = manifest_version(&root.join("current/plugin.json"));
    let previous_version = manifest_version(&root.join("previous/plugin.json"));
    LocalPluginStatus {
        enabled: !root.join("disabled").exists() && !current_version.is_empty(),
        status: if current_version.is_empty() {
            "uninstalled"
        } else if root.join("disabled").exists() {
            "disabled"
        } else {
            "installed"
        }
        .to_string(),
        current_version,
        previous_version,
    }
}

pub(crate) fn report_status(
    options: &Options,
    agent_id: &str,
    plugin_id: &str,
    action: &str,
    from_version: &str,
    error: &str,
) -> Result<(), Box<dyn Error>> {
    flush_status_outbox(options, agent_id);
    let local = local_status(plugin_id);
    let status = if error.is_empty() {
        local.status.as_str()
    } else {
        "failed"
    };
    let record = PluginStatusRecord {
        agent_id: agent_id.to_string(),
        plugin_id: plugin_id.to_string(),
        action: action.to_string(),
        from_version: from_version.to_string(),
        current_version: local.current_version,
        previous_version: local.previous_version,
        enabled: local.enabled,
        status: status.to_string(),
        error: error.chars().take(2048).collect(),
    };
    if let Err(send_error) = send_status(options, &record) {
        store_status(&options.state_path, &record)?;
        return Err(send_error);
    }
    Ok(())
}

fn send_status(options: &Options, record: &PluginStatusRecord) -> Result<(), Box<dyn Error>> {
    let credential = options.agent_credential();
    if record.agent_id.is_empty() || credential.is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".into());
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    report_plugin_status(
        &client,
        &options.api_base,
        &record.agent_id,
        &credential,
        &PluginStatusReport {
            plugin_id: &record.plugin_id,
            action: &record.action,
            from_version: &record.from_version,
            current_version: &record.current_version,
            previous_version: &record.previous_version,
            enabled: record.enabled,
            status: &record.status,
            error: &record.error,
        },
    )
}

pub(crate) fn flush_status_outbox(options: &Options, agent_id: &str) {
    let records = match list_statuses(&options.state_path) {
        Ok(records) => records,
        Err(error) => {
            eprintln!("plugin status outbox read failed: {error}");
            return;
        }
    };
    for (path, mut record) in records {
        if record.agent_id.is_empty() {
            record.agent_id = agent_id.to_string();
        }
        match send_status(options, &record) {
            Ok(()) => {
                if let Err(error) = remove_status(&path) {
                    eprintln!("plugin status outbox cleanup failed: {error}");
                }
            }
            Err(error) => {
                eprintln!("plugin status outbox replay failed: {error}");
                break;
            }
        }
    }
}

fn manifest_version(path: &Path) -> String {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| {
            serde_json::from_str::<PluginManifest>(content.trim_start_matches('\u{feff}')).ok()
        })
        .map(|manifest| manifest.version)
        .unwrap_or_default()
}

pub(crate) fn install(
    options: &Options,
    agent_id: &str,
    plugin_id: &str,
) -> Result<(), Box<dyn Error>> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let item = catalog_item(&client, options, agent_id, plugin_id)?;
    if item.governance == "blocked" {
        return Err("该插件已被组织策略禁止安装".into());
    }
    ensure_agent_version_supported(&item.min_agent_version)?;
    let archive = download(&client, options, agent_id, &item)?;
    let result = install_archive(&archive, &item);
    let _ = fs::remove_file(archive);
    result
}

pub(crate) fn rollback(plugin_id: &str) -> Result<(), Box<dyn Error>> {
    if is_builtin_plugin(plugin_id) {
        return Err("内置系统扩展不支持回滚".into());
    }
    rollback_root(&plugin_root(plugin_id)?, plugin_id)
}

fn rollback_root(root: &Path, plugin_id: &str) -> Result<(), Box<dyn Error>> {
    let current = root.join("current");
    let previous = root.join("previous");
    if !current.exists() || !previous.exists() {
        return Err("插件没有可用的上一版本".into());
    }
    let current_manifest: PluginManifest = serde_json::from_str(
        fs::read_to_string(current.join("plugin.json"))?.trim_start_matches('\u{feff}'),
    )?;
    let previous_manifest: PluginManifest = serde_json::from_str(
        fs::read_to_string(previous.join("plugin.json"))?.trim_start_matches('\u{feff}'),
    )?;
    if current_manifest.id != previous_manifest.id || current_manifest.id != plugin_id {
        return Err("插件 current/previous 身份不一致".into());
    }
    ensure_agent_version_supported(&previous_manifest.min_agent_version)?;
    swap_current_previous(&root)
}

pub(crate) fn uninstall(plugin_id: &str) -> Result<(), Box<dyn Error>> {
    if is_builtin_plugin(plugin_id) {
        return Err("内置系统扩展不允许卸载".into());
    }
    let root = plugin_root(plugin_id)?;
    let governance = installed_governance(&root)?;
    if matches!(governance.as_str(), "required" | "managed") {
        return Err("核心或组织管理插件不允许卸载".into());
    }
    if root.exists() {
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

pub(crate) fn set_enabled(plugin_id: &str, enabled: bool) -> Result<(), Box<dyn Error>> {
    if !enabled && is_builtin_plugin(plugin_id) {
        return Err("内置系统扩展不允许停用".into());
    }
    let root = plugin_root(plugin_id)?;
    if !root.exists() {
        return Err("插件未安装".into());
    }
    let governance = installed_governance(&root)?;
    if !enabled && matches!(governance.as_str(), "required" | "managed") {
        return Err("核心或组织管理插件不允许停用".into());
    }
    let marker = root.join("disabled");
    if enabled {
        crate::capability::plugin::reset_plugin_health(plugin_id)?;
        if marker.exists() {
            fs::remove_file(marker)?;
        }
    } else {
        fs::write(marker, b"disabled")?;
    }
    Ok(())
}

fn catalog_item(
    client: &Client,
    options: &Options,
    agent_id: &str,
    plugin_id: &str,
) -> Result<PluginCatalogItem, Box<dyn Error>> {
    let credential = options.agent_credential();
    if credential.is_empty() {
        return Err("Agent 尚未完成 Dashboard 配对".into());
    }
    plugin_catalog(client, &options.api_base, agent_id, &credential)?
        .into_iter()
        .find(|item| item.plugin_id == plugin_id)
        .ok_or_else(|| "插件未上架或当前不可用".into())
}

fn download(
    client: &Client,
    options: &Options,
    agent_id: &str,
    item: &PluginCatalogItem,
) -> Result<PathBuf, Box<dyn Error>> {
    if item.file_size == 0 || item.file_size > MAX_PLUGIN_ARCHIVE_BYTES {
        return Err("插件制品大小无效或超过 512 MiB 限制".into());
    }
    let api = url::Url::parse(&options.api_base)?;
    let url = url::Url::parse(&item.download_url)?;
    if api.scheme() != url.scheme()
        || api.host_str() != url.host_str()
        || api.port_or_known_default() != url.port_or_known_default()
    {
        return Err("插件制品下载地址必须与 Dashboard 同源".into());
    }
    let mut response = client
        .get(url)
        .header(
            "Authorization",
            format!("Agent {agent_id}:{}", options.agent_credential()),
        )
        .send()?
        .error_for_status()?;
    if response
        .content_length()
        .map(|size| size > item.file_size || size > MAX_PLUGIN_ARCHIVE_BYTES)
        .unwrap_or(false)
    {
        return Err("插件制品响应大小超过发布记录".into());
    }
    let path = env::temp_dir().join(format!("himind-plugin-{}.zip", unique_suffix()));
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
        if total > MAX_PLUGIN_ARCHIVE_BYTES || total > item.file_size {
            let _ = fs::remove_file(&path);
            return Err("插件制品实际大小超过发布记录".into());
        }
        file.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    file.flush()?;
    if total != item.file_size {
        let _ = fs::remove_file(&path);
        return Err(format!(
            "插件制品大小校验失败，期望 {}，实际 {total}",
            item.file_size
        )
        .into());
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(&item.sha256) {
        let _ = fs::remove_file(&path);
        return Err(format!(
            "插件制品 SHA-256 校验失败，期望 {}，实际 {actual}",
            item.sha256
        )
        .into());
    }
    verify_signature(&path, item)?;
    Ok(path)
}

fn verify_signature(path: &Path, item: &PluginCatalogItem) -> Result<(), Box<dyn Error>> {
    let require_signed = env::var("HIMIND_REQUIRE_SIGNED_UPDATES")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    validate_signature_metadata(
        &item.signature,
        &item.signature_key_id,
        &item.signature_algorithm,
        require_signed,
    )?;
    if item.signature.is_empty() {
        return Ok(());
    }
    let trusted = env::var_os("HIMIND_TRUSTED_SIGNING_KEYS_DIR").ok_or("未配置插件受信公钥目录")?;
    let pem =
        fs::read_to_string(PathBuf::from(trusted).join(format!("{}.pem", item.signature_key_id)))?;
    verify_rsa_pss_sha256(path, &pem, &item.signature)
}

fn install_archive(archive_path: &Path, item: &PluginCatalogItem) -> Result<(), Box<dyn Error>> {
    let root = plugin_root(&item.plugin_id)?;
    fs::create_dir_all(root.join("versions"))?;
    let staging = root.join(format!("staging-{}", unique_suffix()));
    fs::create_dir_all(&staging)?;
    let result = (|| {
        let mut archive = ZipArchive::new(File::open(archive_path)?)?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let relative = entry
                .enclosed_name()
                .ok_or("插件 ZIP 包含非法路径")?
                .to_path_buf();
            let output = staging.join(relative);
            if entry.is_dir() {
                fs::create_dir_all(output)?;
                continue;
            }
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)?;
            }
            std::io::copy(&mut entry, &mut File::create(output)?)?;
        }
        verify_plugin_checksums(&staging)?;
        let manifest_path = staging.join("plugin.json");
        let manifest: PluginManifest = serde_json::from_str(
            fs::read_to_string(manifest_path)?.trim_start_matches('\u{feff}'),
        )?;
        if manifest.id != item.plugin_id || manifest.version != item.version {
            return Err("插件 Manifest ID 或版本与发布记录不一致".into());
        }
        let version_dir = root.join("versions").join(&item.version);
        if version_dir.exists() {
            let existing = fs::read(version_dir.join("checksums.sha256"))?;
            let incoming = fs::read(staging.join("checksums.sha256"))?;
            if existing != incoming {
                return Err("同一插件版本已存在且内容不同，请提升版本号".into());
            }
            fs::remove_dir_all(&staging)?;
        } else {
            fs::rename(&staging, &version_dir)?;
        }
        let next = root.join(format!("current-{}", unique_suffix()));
        copy_dir(&version_dir, &next)?;
        fs::write(
            next.join("policy.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"governance": item.governance}))?,
        )?;
        let current = root.join("current");
        let previous = root.join("previous");
        if previous.exists() {
            fs::remove_dir_all(&previous)?;
        }
        if current.exists() {
            fs::rename(&current, &previous)?;
        }
        if let Err(error) = fs::rename(&next, &current) {
            if previous.exists() && !current.exists() {
                let _ = fs::rename(&previous, &current);
            }
            return Err(error.into());
        }
        let marker = root.join("disabled");
        if marker.exists() {
            fs::remove_file(marker)?;
        }
        Ok(())
    })();
    if staging.exists() {
        let _ = fs::remove_dir_all(staging);
    }
    result
}

fn swap_current_previous(root: &Path) -> Result<(), Box<dyn Error>> {
    let current = root.join("current");
    let previous = root.join("previous");
    let temporary = root.join(format!("swap-{}", unique_suffix()));
    fs::rename(&current, &temporary)?;
    if let Err(error) = fs::rename(&previous, &current) {
        let _ = fs::rename(&temporary, &current);
        return Err(error.into());
    }
    if let Err(error) = fs::rename(&temporary, &previous) {
        let restore_previous = root.join(format!("restore-{}", unique_suffix()));
        let _ = fs::rename(&current, &restore_previous);
        let _ = fs::rename(&temporary, &current);
        let _ = fs::rename(&restore_previous, &previous);
        return Err(error.into());
    }
    Ok(())
}

fn ensure_agent_version_supported(minimum: &str) -> Result<(), Box<dyn Error>> {
    let minimum = minimum.trim();
    if minimum.is_empty() {
        return Ok(());
    }
    if compare_versions(crate::VERSION, minimum) < 0 {
        return Err(format!(
            "当前 Agent {} 不满足插件最低版本 {}",
            crate::VERSION,
            minimum
        )
        .into());
    }
    Ok(())
}

fn compare_versions(left: &str, right: &str) -> i32 {
    let parse = |value: &str| {
        value
            .split(['.', '-', '+'])
            .take(3)
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let left = parse(left);
    let right = parse(right);
    for index in 0..3 {
        match left
            .get(index)
            .unwrap_or(&0)
            .cmp(right.get(index).unwrap_or(&0))
        {
            std::cmp::Ordering::Less => return -1,
            std::cmp::Ordering::Greater => return 1,
            std::cmp::Ordering::Equal => {}
        }
    }
    0
}

pub(crate) fn verify_plugin_checksums(root: &Path) -> Result<(), Box<dyn Error>> {
    let checksum_path = root.join("checksums.sha256");
    let content = fs::read_to_string(&checksum_path).map_err(|_| "插件包缺少 checksums.sha256")?;
    let mut expected = HashMap::new();
    for (index, line) in content.lines().enumerate() {
        let Some((checksum, relative)) = line.split_once("  ") else {
            return Err(format!("checksums.sha256 第 {} 行格式无效", index + 1).into());
        };
        if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("checksums.sha256 第 {} 行摘要无效", index + 1).into());
        }
        let relative_path = PathBuf::from(relative);
        if relative == "checksums.sha256"
            || relative.is_empty()
            || relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(format!("checksums.sha256 第 {} 行路径无效", index + 1).into());
        }
        if expected
            .insert(relative.replace('\\', "/"), checksum.to_ascii_lowercase())
            .is_some()
        {
            return Err(format!("checksums.sha256 包含重复路径: {relative}").into());
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
            .ok_or_else(|| format!("插件文件未包含在 checksums.sha256: {relative}"))?;
        let mut file = File::open(entry.path())?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher)?;
        let actual = format!("{:x}", hasher.finalize());
        if &actual != expected_checksum {
            return Err(format!("插件文件摘要不匹配: {relative}").into());
        }
    }
    if let Some(missing) = expected.keys().find(|name| !actual_files.contains(*name)) {
        return Err(format!("checksums.sha256 引用了缺失文件: {missing}").into());
    }
    Ok(())
}

fn installed_governance(root: &Path) -> Result<String, Box<dyn Error>> {
    let policy = root.join("current/policy.json");
    if policy.exists() {
        let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(policy)?)?;
        if let Some(value) = value.get("governance").and_then(|value| value.as_str()) {
            return Ok(value.to_string());
        }
    }
    let manifest: PluginManifest =
        serde_json::from_str(&fs::read_to_string(root.join("current/plugin.json"))?)?;
    Ok(manifest.governance)
}

fn plugin_root(plugin_id: &str) -> Result<PathBuf, Box<dyn Error>> {
    if plugin_id.is_empty()
        || !plugin_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("插件 ID 无效".into());
    }
    Ok(plugin_registry_dir().join(plugin_id))
}

fn copy_dir(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(target)?;
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(destination)?;
        } else {
            fs::copy(entry.path(), destination)?;
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
    use super::{
        compare_versions, flush_status_outbox, report_status, rollback_root, set_enabled,
        uninstall, verify_plugin_checksums,
    };
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, RwLock};

    #[test]
    fn builtin_plugins_cannot_be_disabled_or_uninstalled() {
        assert!(set_enabled("com.himind.builtin.svn", false).is_err());
        assert!(uninstall("com.himind.builtin.smb").is_err());
    }

    #[test]
    fn verifies_complete_plugin_checksums_and_rejects_tampering() {
        let root = std::env::temp_dir().join(format!(
            "himind-plugin-checksum-test-{}",
            super::unique_suffix()
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("plugin.json"), b"{}").unwrap();
        fs::write(root.join("bin/tool.exe"), b"binary").unwrap();
        let manifest_hash = format!("{:x}", Sha256::digest(b"{}"));
        let entry_hash = format!("{:x}", Sha256::digest(b"binary"));
        fs::write(
            root.join("checksums.sha256"),
            format!("{manifest_hash}  plugin.json\n{entry_hash}  bin/tool.exe\n"),
        )
        .unwrap();
        assert!(verify_plugin_checksums(&root).is_ok());
        fs::write(root.join("bin/tool.exe"), b"tampered").unwrap();
        assert!(verify_plugin_checksums(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn compares_agent_versions_for_plugin_minimum_gate() {
        assert!(compare_versions("0.2.0", "0.1.9") > 0);
        assert_eq!(compare_versions("0.2.0", "0.2.0"), 0);
        assert!(compare_versions("0.2.0", "0.3.0") < 0);
        assert_eq!(compare_versions("0.2.0-beta.1", "0.2.0"), 0);
    }

    #[test]
    fn rolls_back_by_swapping_current_and_previous() {
        let root = std::env::temp_dir().join(format!(
            "himind-plugin-rollback-test-{}",
            super::unique_suffix()
        ));
        fs::create_dir_all(root.join("current")).unwrap();
        fs::create_dir_all(root.join("previous")).unwrap();
        let manifest = |version: &str| {
            format!(
                r#"{{"id":"com.himind.rollback","name":"Rollback","version":"{version}","runtime":"process-jsonrpc-stdio","min_agent_version":"0.1.0"}}"#
            )
        };
        fs::write(root.join("current/plugin.json"), manifest("2.0.0")).unwrap();
        fs::write(root.join("previous/plugin.json"), manifest("1.0.0")).unwrap();

        rollback_root(&root, "com.himind.rollback").unwrap();

        let current = fs::read_to_string(root.join("current/plugin.json")).unwrap();
        let previous = fs::read_to_string(root.join("previous/plugin.json")).unwrap();
        assert!(current.contains(r#""version":"1.0.0""#));
        assert!(previous.contains(r#""version":"2.0.0""#));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn queues_failed_status_and_replays_after_dashboard_recovers() {
        let root = std::env::temp_dir().join(format!(
            "himind-plugin-status-replay-test-{}",
            super::unique_suffix()
        ));
        fs::create_dir_all(&root).unwrap();
        let state_path = root.join("agent-state.json");
        let options = crate::Options {
            api_base: "http://127.0.0.1:1".to_string(),
            state_path: state_path.clone(),
            once: false,
            interval_seconds: 10,
            local_app: false,
            local_port: 18181,
            enrollment_token: String::new(),
            agent_credential: Arc::new(RwLock::new("credential".to_string())),
            task_execution: Arc::new(RwLock::new(None)),
        };

        assert!(report_status(
            &options,
            "agent-1",
            "com.himind.replay",
            "enable",
            "1.0.0",
            ""
        )
        .is_err());
        assert_eq!(
            crate::store::plugin_outbox::list(&state_path)
                .unwrap()
                .len(),
            1
        );

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        let recovered = crate::Options {
            api_base: format!("http://{address}"),
            ..options
        };
        flush_status_outbox(&recovered, "agent-1");
        server.join().unwrap();

        assert!(crate::store::plugin_outbox::list(&state_path)
            .unwrap()
            .is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
