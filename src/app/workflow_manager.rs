use crate::api::distribution::WorkflowCatalogItem;
use crate::app::system::verify_extension_artifact_signature;
use crate::extension_contracts::{ExtensionAssetKind, ExtensionLock};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::ZipArchive;

const MAX_WORKFLOW_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_WORKFLOW_EXTRACTED_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_WORKFLOW_ARCHIVE_ENTRIES: usize = 100_000;

/// 远端取用（GitHub Release 扩展源、工作台分发）安装时是否再要求包内 `manifest.sig`。
///
/// 远端链路在下载阶段就完成了一次更强的认证：`download_public` / `download_dashboard`
/// 先按发布记录校验大小与 SHA-256，再用发布清单的 `signature` / `signature_key_id` /
/// `rsa-pss-sha256` 对整份 `.hmwf` 验签。发布清单签名覆盖整个制品，强于只覆盖
/// `checksums.sha256` 的包内签名，所以远端安装不再重复要求包内签名——与插件、技能
/// 「只信任发布清单签名」的策略一致。
///
/// 包内签名仍是本地目录安装与 `workflow install-archive --require-signature`
/// 的唯一信任根；契约见 docs/workflow-package-contract-v1.md 第 9 节。
const REMOTE_REQUIRE_PACKAGE_SIGNATURE: bool = false;

pub(crate) fn install_local_catalog_item(
    item: &WorkflowCatalogItem,
    require_signature: bool,
) -> Result<crate::workflow::InstalledWorkflow, Box<dyn Error>> {
    let root = local_item_dir(&item.download_url)?;
    install_from_directory(item, &root, require_signature)
}

pub(crate) fn install_public_catalog_item(
    item: &WorkflowCatalogItem,
    require_signature: bool,
) -> Result<crate::workflow::InstalledWorkflow, Box<dyn Error>> {
    if item.management != "user_managed" || item.assignment != "optional" || item.managed {
        return Err("公共扩展源不能授予组织 Workflow 管理策略".into());
    }
    ensure_agent_version_supported(&item.min_agent_version)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .user_agent("HiMind-Agent")
        .build()?;
    // 这里的 require_signature 约束的是发布清单签名，已在上一步完成；解包后不再
    // 要求包内 manifest.sig，见 REMOTE_REQUIRE_PACKAGE_SIGNATURE。
    let archive = download_public(&client, item, require_signature)?;
    let staging = std::env::temp_dir().join(format!("himind-public-workflow-{}", unique_suffix()));
    let result = (|| {
        extract_archive(&archive, &staging)?;
        let root = package_root(&staging)?;
        install_from_directory(item, &root, REMOTE_REQUIRE_PACKAGE_SIGNATURE)
    })();
    let _ = fs::remove_file(archive);
    let _ = fs::remove_dir_all(staging);
    result
}

pub(crate) fn install_dashboard_catalog_item(
    item: &WorkflowCatalogItem,
    options: &crate::Options,
    agent_id: &str,
) -> Result<crate::workflow::InstalledWorkflow, Box<dyn Error>> {
    if item.assignment == "blocked" {
        return Err("该 Workflow 已被组织禁止安装".into());
    }
    ensure_agent_version_supported(&item.min_agent_version)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .user_agent("HiMind-Agent")
        .build()?;
    let archive = download_dashboard(&client, item, options, agent_id)?;
    let staging =
        std::env::temp_dir().join(format!("himind-dashboard-workflow-{}", unique_suffix()));
    let result = (|| {
        extract_archive(&archive, &staging)?;
        let root = package_root(&staging)?;
        let lock = catalog_lock(item)?.ok_or("组织 Workflow 缺少依赖锁 extension_lock")?;
        // 工作台分发同样在 download_dashboard 里按发布清单签名认证过整份制品。
        install_from_directory_with_lock(item, &root, REMOTE_REQUIRE_PACKAGE_SIGNATURE, Some(lock))
    })();
    let _ = fs::remove_file(archive);
    let _ = fs::remove_dir_all(staging);
    result
}

pub(crate) fn install_dashboard_catalog_workflow(
    options: &crate::Options,
    workflow_id: &str,
    version: Option<&str>,
) -> Result<crate::workflow::InstalledWorkflow, Box<dyn Error>> {
    install_dashboard_catalog_workflow_bound(options, workflow_id, version, None, None)
}

pub(crate) fn install_dashboard_catalog_workflow_bound(
    options: &crate::Options,
    workflow_id: &str,
    version: Option<&str>,
    expected_artifact_id: Option<&str>,
    expected_sha256: Option<&str>,
) -> Result<crate::workflow::InstalledWorkflow, Box<dyn Error>> {
    let state = crate::api::client::load_agent_state(&options.state_path)?;
    options.set_agent_credential(&state.credential);
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let item = if let Some(version) = version.map(str::trim).filter(|value| !value.is_empty()) {
        crate::api::distribution::workflow_versions(
            &client,
            &options.api_base(),
            &state.agent_id,
            &state.credential,
            workflow_id,
        )?
        .into_iter()
        .find(|item| item.version == version)
        .ok_or_else(|| format!("Workflow 版本 v{version} 不可用"))?
    } else {
        crate::api::distribution::workflow_catalog(
            &client,
            &options.api_base(),
            &state.agent_id,
            &state.credential,
        )?
        .into_iter()
        .find(|item| item.workflow_id == workflow_id)
        .ok_or_else(|| format!("Workflow 未上架或当前不可用: {workflow_id}"))?
    };
    crate::app::extension_lock::verify_catalog_artifact(
        "Workflow",
        &item.artifact_id,
        &item.sha256,
        expected_artifact_id,
        expected_sha256,
    )?;
    let store = crate::workflow::WorkflowStore::open_default()?;
    let previous = store
        .list()?
        .into_iter()
        .find(|installed| installed.package.id == item.workflow_id);
    let installed = install_dashboard_catalog_item(&item, options, &state.agent_id)?;
    if let Err(error) = crate::app::extension_lock::record_workflow(&item) {
        if let Some(previous) = previous {
            if previous.package.version != item.version {
                let _ = store.rollback(&item.workflow_id);
            }
        } else {
            let _ = store.remove(&item.workflow_id);
        }
        return Err(error);
    }
    Ok(installed)
}

fn install_from_directory(
    item: &WorkflowCatalogItem,
    root: &Path,
    require_package_signature: bool,
) -> Result<crate::workflow::InstalledWorkflow, Box<dyn Error>> {
    let lock = catalog_lock(item)?;
    let store = crate::workflow::WorkflowStore::open_default()?;
    install_into_store(&store, item, root, require_package_signature, lock)
}

fn install_from_directory_with_lock(
    item: &WorkflowCatalogItem,
    root: &Path,
    require_package_signature: bool,
    lock: Option<ExtensionLock>,
) -> Result<crate::workflow::InstalledWorkflow, Box<dyn Error>> {
    let store = crate::workflow::WorkflowStore::open_default()?;
    install_into_store(&store, item, root, require_package_signature, lock)
}

/// 把已经解包好的 Workflow 目录装进给定仓库。
///
/// `require_package_signature` 只决定包内 `manifest.sig` 是否为必需：远端取用传
/// [`REMOTE_REQUIRE_PACKAGE_SIGNATURE`]，本地目录与显式 `--require-signature`
/// 安装传调用方给出的策略。仓库由调用方传入，测试才能不依赖 `HIMIND_AGENT_HOME`。
fn install_into_store(
    store: &crate::workflow::WorkflowStore,
    item: &WorkflowCatalogItem,
    root: &Path,
    require_package_signature: bool,
    lock: Option<ExtensionLock>,
) -> Result<crate::workflow::InstalledWorkflow, Box<dyn Error>> {
    let package = crate::workflow::load_from_directory(root)?;
    if package.id != item.workflow_id || package.version != item.version {
        return Err("Workflow Manifest ID 或版本与扩展源记录不一致".into());
    }
    ensure_agent_version_supported(&package.min_agent_version)?;
    let lock_required = lock.is_some();
    store.install_from_directory_with_metadata(
        root,
        require_package_signature,
        &item.sha256,
        lock,
        lock_required,
    )
}

fn catalog_lock(item: &WorkflowCatalogItem) -> Result<Option<ExtensionLock>, Box<dyn Error>> {
    item.extension_lock
        .clone()
        .map(serde_json::from_value)
        .transpose()
        .map_err(Into::into)
}

pub(crate) fn install_local_archive(
    archive_path: &Path,
    require_signature: bool,
) -> Result<crate::workflow::InstalledWorkflow, Box<dyn Error>> {
    let archive_path = archive_path.canonicalize()?;
    let extension = archive_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "hmwf" | "zip") {
        return Err("Workflow 制品必须使用 .hmwf 或 .zip 扩展名".into());
    }
    let metadata = fs::metadata(&archive_path)?;
    if metadata.len() == 0 || metadata.len() > MAX_WORKFLOW_ARCHIVE_BYTES {
        return Err("Workflow 制品为空或超过 256 MiB 限制".into());
    }
    let artifact_sha256 = sha256_file(&archive_path)?;
    let staging =
        std::env::temp_dir().join(format!("himind-local-workflow-archive-{}", unique_suffix()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let result = (|| {
        extract_archive(&archive_path, &staging)?;
        let root = package_root(&staging)?;
        let package = crate::workflow::load_from_directory(&root)?;
        ensure_agent_version_supported(&package.min_agent_version)?;
        let store = crate::workflow::WorkflowStore::open_default()?;
        let previous = store
            .list()?
            .into_iter()
            .find(|installed| installed.package.id == package.id);
        let extension_lock = load_local_archive_lock(&archive_path, &package, &artifact_sha256)?;
        let lock_required = extension_lock.is_some();
        let installed = store.install_from_directory_with_metadata(
            &root,
            require_signature,
            &artifact_sha256,
            extension_lock.clone(),
            lock_required,
        )?;
        if let Some(extension_lock) = extension_lock.as_ref() {
            if let Err(error) = crate::app::extension_lock::record_local_workflow(
                &package,
                &archive_path.to_string_lossy(),
                &artifact_sha256,
                extension_lock,
            ) {
                match previous {
                    Some(previous) if previous.package.version != package.version => {
                        let _ = store.rollback(&package.id);
                    }
                    Some(_) => {}
                    None => {
                        let _ = store.remove(&package.id);
                    }
                }
                return Err(error);
            }
        }
        Ok(installed)
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

fn load_local_archive_lock(
    archive_path: &Path,
    package: &crate::workflow::WorkflowPackage,
    artifact_sha256: &str,
) -> Result<Option<ExtensionLock>, Box<dyn Error>> {
    let path = archive_path.with_extension("extension-lock.json");
    if !path.is_file() {
        return Ok(None);
    }
    let lock: ExtensionLock = serde_json::from_slice(&fs::read(&path)?)?;
    lock.validate()?;
    if lock.root.kind != ExtensionAssetKind::Workflow
        || lock.root.id != package.id
        || lock.root.version != package.version
        || !lock.root.sha256.eq_ignore_ascii_case(artifact_sha256)
    {
        return Err(format!("Workflow 依赖锁与制品不一致: {}", path.display()).into());
    }
    Ok(Some(lock))
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn download_public(
    client: &Client,
    item: &WorkflowCatalogItem,
    require_signature: bool,
) -> Result<PathBuf, Box<dyn Error>> {
    if item.file_size == 0 || item.file_size > MAX_WORKFLOW_ARCHIVE_BYTES {
        return Err("Workflow 制品大小无效或超过 256 MiB 限制".into());
    }
    let url = url::Url::parse(&item.download_url)?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        return Err("公共 Workflow 制品必须使用 github.com 的 HTTPS Release 地址".into());
    }
    let mut response = client.get(url).send()?.error_for_status()?;
    if response
        .content_length()
        .map(|size| size > item.file_size || size > MAX_WORKFLOW_ARCHIVE_BYTES)
        .unwrap_or(false)
    {
        return Err("Workflow 制品响应大小超过发布记录".into());
    }
    let path =
        std::env::temp_dir().join(format!("himind-public-workflow-{}.hmwf", unique_suffix()));
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
        if total > MAX_WORKFLOW_ARCHIVE_BYTES || total > item.file_size {
            let _ = fs::remove_file(&path);
            return Err("Workflow 制品实际大小超过发布记录".into());
        }
        file.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    file.flush()?;
    if total != item.file_size {
        let _ = fs::remove_file(&path);
        return Err("Workflow 制品实际大小与发布记录不一致".into());
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(&item.sha256) {
        let _ = fs::remove_file(&path);
        return Err("Workflow 制品 SHA-256 校验失败".into());
    }
    if let Err(error) = verify_extension_artifact_signature(
        &path,
        &item.signature,
        &item.signature_key_id,
        &item.signature_algorithm,
        require_signature,
    ) {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

fn download_dashboard(
    client: &Client,
    item: &WorkflowCatalogItem,
    options: &crate::Options,
    agent_id: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    if item.file_size == 0 || item.file_size > MAX_WORKFLOW_ARCHIVE_BYTES {
        return Err("Workflow 制品大小无效或超过 256 MiB 限制".into());
    }
    let api = url::Url::parse(&options.api_base())?;
    let url = url::Url::parse(&item.download_url)?;
    if api.scheme() != url.scheme()
        || api.host_str() != url.host_str()
        || api.port_or_known_default() != url.port_or_known_default()
    {
        return Err("组织 Workflow 制品下载地址必须与 Dashboard 同源".into());
    }
    let mut response = client
        .get(url)
        .header(
            "Authorization",
            format!("Agent {agent_id}:{}", options.agent_credential()),
        )
        .send()?
        .error_for_status()?;
    let path = std::env::temp_dir().join(format!(
        "himind-dashboard-workflow-{}.hmwf",
        unique_suffix()
    ));
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
        if total > MAX_WORKFLOW_ARCHIVE_BYTES || total > item.file_size {
            let _ = fs::remove_file(&path);
            return Err("Workflow 制品实际大小超过发布记录".into());
        }
        file.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    file.flush()?;
    if total != item.file_size {
        let _ = fs::remove_file(&path);
        return Err("Workflow 制品实际大小与发布记录不一致".into());
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(&item.sha256) {
        let _ = fs::remove_file(&path);
        return Err("Workflow 制品 SHA-256 校验失败".into());
    }
    if let Err(error) = verify_extension_artifact_signature(
        &path,
        &item.signature,
        &item.signature_key_id,
        &item.signature_algorithm,
        true,
    ) {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

fn extract_archive(archive_path: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(target)?;
    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    if archive.len() > MAX_WORKFLOW_ARCHIVE_ENTRIES {
        return Err("Workflow ZIP 文件数量超过 100000 个限制".into());
    }
    let mut extracted_bytes = 0_u64;
    let mut seen_paths = std::collections::HashSet::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = entry
            .enclosed_name()
            .ok_or("Workflow ZIP 包含非法路径")?
            .to_path_buf();
        let normalized = relative.to_string_lossy().replace('\\', "/");
        if normalized
            .split('/')
            .any(|component| component == "__MACOSX" || component.eq_ignore_ascii_case(".ds_store"))
        {
            continue;
        }
        extracted_bytes = extracted_bytes
            .checked_add(entry.size())
            .ok_or("Workflow ZIP 解压大小溢出")?;
        if extracted_bytes > MAX_WORKFLOW_EXTRACTED_BYTES {
            return Err("Workflow ZIP 解压后超过 1 GiB 限制".into());
        }
        crate::skill::manifest::validate_relative_package_path(&relative.to_string_lossy())?;
        if !seen_paths.insert(normalized.to_ascii_lowercase()) {
            return Err(format!("Workflow ZIP 包含重复或大小写冲突路径: {normalized}").into());
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(format!("Workflow ZIP 不允许符号链接: {normalized}").into());
        }
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

fn package_root(staging: &Path) -> Result<PathBuf, Box<dyn Error>> {
    if staging.join("workflow.json").is_file() {
        return Ok(staging.to_path_buf());
    }
    let mut directories = Vec::new();
    let mut files = Vec::new();
    for entry in fs::read_dir(staging)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            directories.push(entry.path());
        } else {
            files.push(entry.path());
        }
    }
    if directories.len() == 1 && files.is_empty() && directories[0].join("workflow.json").is_file()
    {
        return Ok(directories.remove(0));
    }
    Err("Workflow ZIP 根目录缺少 workflow.json".into())
}

fn local_item_dir(download_url: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = download_url
        .strip_prefix("local:")
        .ok_or("本地 Workflow 目录项缺少本地路径")?;
    if path.trim().is_empty() {
        return Err("本地 Workflow 目录项缺少本地路径".into());
    }
    let path = PathBuf::from(path);
    if !path.is_dir() {
        return Err(format!("本地 Workflow 目录不存在: {}", path.display()).into());
    }
    Ok(path)
}

fn ensure_agent_version_supported(minimum: &str) -> Result<(), Box<dyn Error>> {
    if !minimum.trim().is_empty()
        && crate::skill::resolver::compare_versions(crate::VERSION, minimum)
            == std::cmp::Ordering::Less
    {
        return Err(format!(
            "当前 Agent {} 不满足 Workflow 最低版本 {}",
            crate::VERSION,
            minimum
        )
        .into());
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
    use super::*;
    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
    use rand::rngs::OsRng;
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::{Pss, RsaPrivateKey, RsaPublicKey};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use zip::write::FileOptions;
    use zip::CompressionMethod;
    use zip::ZipWriter;

    fn item(url: &str) -> WorkflowCatalogItem {
        WorkflowCatalogItem {
            workflow_id: "com.himind.workflow.test".to_string(),
            name: "Test Workflow".to_string(),
            description: String::new(),
            author_name: String::new(),
            categories: Vec::new(),
            version: "1.0.0".to_string(),
            release_notes: String::new(),
            published_at: String::new(),
            min_agent_version: "0.3.0".to_string(),
            capability_ids: Vec::new(),
            channel: "stable".to_string(),
            artifact_id: String::new(),
            file_name: "test.hmwf".to_string(),
            file_size: 128,
            sha256: "a".repeat(64),
            signature: String::new(),
            signature_key_id: String::new(),
            signature_algorithm: String::new(),
            download_url: url.to_string(),
            source: "github:test".to_string(),
            assignment: "optional".to_string(),
            management: "user_managed".to_string(),
            install_mode: "prompt".to_string(),
            organization_reason: String::new(),
            managed: false,
            allow_disable: true,
            allow_uninstall: true,
            extension_lock: None,
        }
    }

    fn archive(entries: &[(&str, &str)]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "himind-workflow-manager-test-{}.zip",
            unique_suffix()
        ));
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        let options = FileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, content) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        path
    }

    #[test]
    fn public_workflow_artifact_must_use_github_https() {
        let client = Client::new();
        let error = download_public(&client, &item("https://example.com/workflow.hmwf"), false)
            .unwrap_err();
        assert!(error.to_string().contains("github.com"));
        let error =
            download_public(&client, &item("http://github.com/workflow.hmwf"), false).unwrap_err();
        assert!(error.to_string().contains("HTTPS"));
    }

    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), &target).unwrap();
            }
        }
    }

    /// 按安装期口径写一份 `checksums.sha256`：只覆盖内容文件，不含打包元数据。
    fn write_checksums(root: &Path) {
        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(root) {
            let entry = entry.unwrap();
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry.path().strip_prefix(root).unwrap().to_path_buf();
            let at_root = relative
                .parent()
                .map(|parent| parent.as_os_str().is_empty())
                .unwrap_or(true);
            let name = relative.file_name().and_then(|name| name.to_str());
            if at_root && matches!(name, Some("checksums.sha256") | Some("manifest.sig")) {
                continue;
            }
            files.push(relative);
        }
        files.sort();
        let mut content = String::new();
        for relative in files {
            let hash = sha256_file(&root.join(&relative)).unwrap();
            content.push_str(&format!(
                "{hash}  {}\n",
                relative.to_string_lossy().replace('\\', "/")
            ));
        }
        fs::write(root.join("checksums.sha256"), content).unwrap();
    }

    // 远端链路（GitHub Release 扩展源、工作台分发）在下载阶段已按发布清单签名认证整份
    // 制品，因此不再重复要求包内 manifest.sig；本地目录安装的签名要求保持不变。
    #[test]
    fn remote_install_accepts_package_without_manifest_signature() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("workflows/wechat-experience-upload");
        let package = crate::workflow::load_from_directory(&fixture).unwrap();
        let staging =
            std::env::temp_dir().join(format!("himind-remote-signature-test-{}", unique_suffix()));
        let package_root = staging.join("package");
        copy_dir(&fixture, &package_root);
        write_checksums(&package_root);
        assert!(!package_root.join("manifest.sig").is_file());

        let mut catalog_item = item("https://github.com/example/repo/releases/download/x/y.hmwf");
        catalog_item.workflow_id = package.id.clone();
        catalog_item.version = package.version.clone();

        let remote_store = crate::workflow::WorkflowStore::new(staging.join("store-remote"));
        let installed = install_into_store(
            &remote_store,
            &catalog_item,
            &package_root,
            REMOTE_REQUIRE_PACKAGE_SIGNATURE,
            None,
        )
        .unwrap();
        assert_eq!(installed.package.id, package.id);
        assert_eq!(installed.package.version, package.version);

        let strict_store = crate::workflow::WorkflowStore::new(staging.join("store-strict"));
        let error = install_into_store(&strict_store, &catalog_item, &package_root, true, None)
            .unwrap_err();
        assert!(
            error.to_string().contains("manifest.sig"),
            "unexpected error: {error}"
        );

        let _ = fs::remove_dir_all(staging);
    }

    #[test]
    fn extracts_workflow_archive_and_accepts_single_wrapper_directory() {
        let archive = archive(&[(
            "workflow-package/workflow.json",
            r#"{"schema_version":"workflow_package.v1"}"#,
        )]);
        let staging =
            std::env::temp_dir().join(format!("himind-workflow-extract-test-{}", unique_suffix()));
        extract_archive(&archive, &staging).unwrap();
        let root = package_root(&staging).unwrap();
        assert!(root.ends_with("workflow-package"));
        let _ = fs::remove_file(archive);
        let _ = fs::remove_dir_all(staging);
    }

    #[test]
    fn rejects_archive_path_traversal() {
        let archive = archive(&[("../outside.txt", "blocked")]);
        let staging = std::env::temp_dir().join(format!(
            "himind-workflow-traversal-test-{}",
            unique_suffix()
        ));
        assert!(extract_archive(&archive, &staging).is_err());
        assert!(!staging.parent().unwrap().join("outside.txt").is_file());
        let _ = fs::remove_file(archive);
        let _ = fs::remove_dir_all(staging);
    }

    #[test]
    fn local_archive_accepts_matching_companion_extension_lock() {
        let package_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("workflows/wechat-experience-upload");
        let package = crate::workflow::load_from_directory(&package_root).unwrap();
        let archive_path = std::env::temp_dir().join(format!(
            "himind-workflow-lock-match-{}.hmwf",
            unique_suffix()
        ));
        fs::write(&archive_path, b"immutable workflow archive").unwrap();
        let artifact_sha256 = sha256_file(&archive_path).unwrap();
        let lock = ExtensionLock {
            schema_version: crate::extension_contracts::EXTENSION_LOCK_SCHEMA_VERSION.to_string(),
            root: crate::extension_contracts::ExtensionAssetIdentity {
                kind: ExtensionAssetKind::Workflow,
                id: package.id.clone(),
                version: package.version.clone(),
                sha256: artifact_sha256.clone(),
            },
            dependencies: Vec::new(),
            environment: crate::extension_contracts::ExtensionLockEnvironment::default(),
            generated_at: "test".to_string(),
        };
        let lock_path = archive_path.with_extension("extension-lock.json");
        fs::write(&lock_path, serde_json::to_vec_pretty(&lock).unwrap()).unwrap();

        let loaded = load_local_archive_lock(&archive_path, &package, &artifact_sha256)
            .unwrap()
            .unwrap();
        assert_eq!(loaded, lock);
        let _ = fs::remove_file(lock_path);
        let _ = fs::remove_file(archive_path);
    }

    #[test]
    fn local_archive_rejects_companion_lock_for_different_artifact() {
        let package_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("workflows/wechat-experience-upload");
        let package = crate::workflow::load_from_directory(&package_root).unwrap();
        let archive_path = std::env::temp_dir().join(format!(
            "himind-workflow-lock-mismatch-{}.hmwf",
            unique_suffix()
        ));
        fs::write(&archive_path, b"immutable workflow archive").unwrap();
        let artifact_sha256 = sha256_file(&archive_path).unwrap();
        let lock = ExtensionLock {
            schema_version: crate::extension_contracts::EXTENSION_LOCK_SCHEMA_VERSION.to_string(),
            root: crate::extension_contracts::ExtensionAssetIdentity {
                kind: ExtensionAssetKind::Workflow,
                id: package.id.clone(),
                version: package.version.clone(),
                sha256: "f".repeat(64),
            },
            dependencies: Vec::new(),
            environment: crate::extension_contracts::ExtensionLockEnvironment::default(),
            generated_at: "test".to_string(),
        };
        let lock_path = archive_path.with_extension("extension-lock.json");
        fs::write(&lock_path, serde_json::to_vec_pretty(&lock).unwrap()).unwrap();

        let error = load_local_archive_lock(&archive_path, &package, &artifact_sha256).unwrap_err();
        assert!(error.to_string().contains("依赖锁与制品不一致"));
        let _ = fs::remove_file(lock_path);
        let _ = fs::remove_file(archive_path);
    }

    #[test]
    fn dashboard_download_requires_same_origin_auth_and_signature() {
        let _guard = crate::app::system::signing_env_lock();
        let payload = b"workflow archive";
        let sha256 = format!("{:x}", Sha256::digest(payload));
        let mut rng = OsRng;
        let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public_key = RsaPublicKey::from(&private_key);
        let signature = private_key
            .sign_with_rng(&mut rng, Pss::new::<Sha256>(), &Sha256::digest(payload))
            .unwrap();
        let key_id = "dashboard-workflow-test-key";
        let trusted_root = std::env::temp_dir().join(format!(
            "himind-dashboard-workflow-keys-{}",
            unique_suffix()
        ));
        fs::create_dir_all(&trusted_root).unwrap();
        fs::write(
            trusted_root.join(format!("{key_id}.pem")),
            public_key.to_public_key_pem(LineEnding::LF).unwrap(),
        )
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8192];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]).to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        payload.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
            stream.write_all(payload).unwrap();
            request
        });

        let previous = std::env::var_os("HIMIND_TRUSTED_SIGNING_KEYS_DIR");
        std::env::set_var("HIMIND_TRUSTED_SIGNING_KEYS_DIR", &trusted_root);
        let mut options = crate::Options::from_env();
        options.set_api_base(&format!("http://{address}"));
        options.set_agent_credential("test-credential");
        let item = WorkflowCatalogItem {
            file_size: payload.len() as u64,
            sha256,
            signature: BASE64_STANDARD.encode(signature),
            signature_key_id: key_id.to_string(),
            signature_algorithm: "rsa-pss-sha256".to_string(),
            download_url: format!("http://{address}/api/agent/workflows/artifacts/test"),
            ..item("http://unused")
        };
        let downloaded = download_dashboard(
            &Client::builder().no_proxy().build().unwrap(),
            &item,
            &options,
            "agent-1",
        )
        .unwrap();
        let request = server.join().unwrap();
        match previous {
            Some(value) => std::env::set_var("HIMIND_TRUSTED_SIGNING_KEYS_DIR", value),
            None => std::env::remove_var("HIMIND_TRUSTED_SIGNING_KEYS_DIR"),
        }
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: agent agent-1:test-credential"));
        assert_eq!(fs::read(&downloaded).unwrap(), payload);
        let _ = fs::remove_file(downloaded);

        let mut cross_origin = item.clone();
        cross_origin.download_url =
            "http://127.0.0.1:9/api/agent/workflows/artifacts/test".to_string();
        assert!(download_dashboard(
            &Client::builder().no_proxy().build().unwrap(),
            &cross_origin,
            &options,
            "agent-1"
        )
        .is_err());
        let _ = fs::remove_dir_all(trusted_root);
    }
}
