use crate::api::distribution::WorkflowCatalogItem;
use crate::app::system::verify_extension_artifact_signature;
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
    let archive = download_public(&client, item, require_signature)?;
    let staging = std::env::temp_dir().join(format!("himind-public-workflow-{}", unique_suffix()));
    let result = (|| {
        extract_archive(&archive, &staging)?;
        let root = package_root(&staging)?;
        install_from_directory(item, &root, require_signature)
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
        install_from_directory(item, &root, true)
    })();
    let _ = fs::remove_file(archive);
    let _ = fs::remove_dir_all(staging);
    result
}

fn install_from_directory(
    item: &WorkflowCatalogItem,
    root: &Path,
    require_signature: bool,
) -> Result<crate::workflow::InstalledWorkflow, Box<dyn Error>> {
    let package = crate::workflow::load_from_directory(root)?;
    if package.id != item.workflow_id || package.version != item.version {
        return Err("Workflow Manifest ID 或版本与扩展源记录不一致".into());
    }
    ensure_agent_version_supported(&package.min_agent_version)?;
    crate::workflow::WorkflowStore::open_default()?
        .install_from_directory_with_policy(root, require_signature)
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
    let api = url::Url::parse(&options.api_base)?;
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
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::thread;
    use zip::write::FileOptions;
    use zip::CompressionMethod;
    use zip::ZipWriter;

    static SIGNING_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn signing_env_lock() -> MutexGuard<'static, ()> {
        SIGNING_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap()
    }

    fn item(url: &str) -> WorkflowCatalogItem {
        WorkflowCatalogItem {
            workflow_id: "com.himind.workflow.test".to_string(),
            name: "Test Workflow".to_string(),
            description: String::new(),
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
    fn dashboard_download_requires_same_origin_auth_and_signature() {
        let _guard = signing_env_lock();
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
        options.api_base = format!("http://{address}");
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
