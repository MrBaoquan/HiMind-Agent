use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::distribution::SoftwareReleasePublishRequest;
use crate::capability::types::InvocationContext;

const RECEIPT_PREFIX: &str = "inspection_";
const RECEIPT_TTL_SECONDS: u64 = 15 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArtifactInspectionReceipt {
    token: String,
    principal: String,
    source: String,
    workspace_root: String,
    artifact_path: String,
    product_id: String,
    version: String,
    channel: String,
    platform: String,
    architecture: String,
    package_type: String,
    size: u64,
    sha256: String,
    issued_at: u64,
    expires_at: u64,
}

#[derive(Debug)]
pub(crate) struct VerifiedSoftwareArtifact {
    pub file: File,
    pub file_name: String,
    pub artifact_path: PathBuf,
    pub size: u64,
    pub sha256: String,
    receipt_path: PathBuf,
}

pub(crate) fn attach_inspection_receipt(
    context: &InvocationContext,
    input: &Value,
    output: Value,
) -> Result<Value, Box<dyn Error>> {
    attach_inspection_receipt_in(context, input, output, &receipts_root())
}

pub(crate) fn verify_inspection_receipt(
    context: &InvocationContext,
    request: &SoftwareReleasePublishRequest,
) -> Result<VerifiedSoftwareArtifact, Box<dyn Error>> {
    verify_inspection_receipt_in(context, request, &receipts_root())
}

pub(crate) fn consume_inspection_receipt(
    verified: &VerifiedSoftwareArtifact,
) -> Result<(), Box<dyn Error>> {
    let _lock = crate::store::atomic_file::lock(&verified.receipt_path)?;
    if !verified.receipt_path.is_file() {
        return Err("制品预检凭证已被使用或不存在，请重新检查制品".into());
    }
    fs::remove_file(&verified.receipt_path)?;
    Ok(())
}

fn attach_inspection_receipt_in(
    context: &InvocationContext,
    input: &Value,
    mut output: Value,
    root: &Path,
) -> Result<Value, Box<dyn Error>> {
    if output.get("ready").and_then(Value::as_bool) != Some(true) {
        return Ok(output);
    }
    let workspace_root = required_string(input, "workspace_root")?;
    let artifact_path = required_string(input, "artifact_path")?;
    let (workspace, artifact) = canonical_workspace_file(&workspace_root, &artifact_path)?;
    let (size, sha256, _) = open_and_hash(&artifact)?;
    if output.get("size").and_then(Value::as_u64) != Some(size)
        || output
            .get("sha256")
            .and_then(Value::as_str)
            .map(|value| value.eq_ignore_ascii_case(&sha256))
            != Some(true)
    {
        return Err("插件返回的制品摘要与 Agent 独立校验结果不一致".into());
    }

    let issued_at = now_seconds()?;
    let token = new_receipt_token();
    let receipt = ArtifactInspectionReceipt {
        token: token.clone(),
        principal: context.principal.clone(),
        source: context.source.as_str().to_string(),
        workspace_root: workspace.to_string_lossy().to_string(),
        artifact_path: artifact.to_string_lossy().to_string(),
        product_id: normalized(input, "product_id"),
        version: required_string(input, "version")?.trim().to_string(),
        channel: normalized_default(input, "channel", "stable"),
        platform: normalized(input, "platform"),
        architecture: normalized(input, "architecture"),
        package_type: normalized(input, "package_type"),
        size,
        sha256: sha256.clone(),
        issued_at,
        expires_at: issued_at + RECEIPT_TTL_SECONDS,
    };
    fs::create_dir_all(root)?;
    cleanup_expired_receipts(root, issued_at);
    let protected = crate::store::credentials::protect_secret_for_current_user(
        &serde_json::to_string(&receipt)?,
    )?;
    crate::store::atomic_file::atomic_write(&receipt_path(root, &token)?, protected.as_bytes())?;

    let object = output
        .as_object_mut()
        .ok_or("制品检查结果必须是 JSON 对象")?;
    object.insert("inspection_receipt".to_string(), Value::String(token));
    object.insert(
        "inspection_expires_at".to_string(),
        receipt.expires_at.into(),
    );
    object.insert("size".to_string(), size.into());
    object.insert("sha256".to_string(), Value::String(sha256));
    Ok(output)
}

fn verify_inspection_receipt_in(
    context: &InvocationContext,
    request: &SoftwareReleasePublishRequest,
    root: &Path,
) -> Result<VerifiedSoftwareArtifact, Box<dyn Error>> {
    let path = receipt_path(root, request.inspection_receipt.trim())?;
    let protected =
        fs::read_to_string(&path).map_err(|_| "制品预检凭证不存在或已被使用，请重新检查制品")?;
    let receipt: ArtifactInspectionReceipt = serde_json::from_str(
        &crate::store::credentials::unprotect_secret_for_current_user(protected.trim())?,
    )?;
    if receipt.token != request.inspection_receipt.trim()
        || receipt.principal != context.principal
        || receipt.source != context.source.as_str()
    {
        return Err("制品预检凭证不属于当前调用会话".into());
    }
    if receipt.expires_at < now_seconds()? {
        return Err("制品预检凭证已过期，请重新检查制品".into());
    }
    let (workspace, artifact) =
        canonical_workspace_file(&request.workspace_root, &request.artifact_path)?;
    let expected = [
        (receipt.workspace_root.as_str(), workspace.to_string_lossy()),
        (receipt.artifact_path.as_str(), artifact.to_string_lossy()),
        (
            receipt.product_id.as_str(),
            request.product_id.clone().into(),
        ),
        (
            receipt.version.as_str(),
            request.version.trim().to_string().into(),
        ),
        (receipt.channel.as_str(), request.channel.clone().into()),
        (receipt.platform.as_str(), request.platform.clone().into()),
        (
            receipt.architecture.as_str(),
            request.architecture.clone().into(),
        ),
        (
            receipt.package_type.as_str(),
            request.package_type.clone().into(),
        ),
    ];
    if expected
        .iter()
        .any(|(actual, expected)| *actual != expected.as_ref())
    {
        return Err("发布参数与制品预检结果不一致，请重新检查制品".into());
    }
    if receipt.size != request.expected_size
        || !receipt
            .sha256
            .eq_ignore_ascii_case(request.expected_sha256.trim())
    {
        return Err("发布摘要与制品预检结果不一致，请重新检查制品".into());
    }
    let (size, sha256, file) = open_and_hash(&artifact)?;
    if size != receipt.size || !sha256.eq_ignore_ascii_case(&receipt.sha256) {
        return Err("制品在预检后发生变化，请重新检查制品".into());
    }
    let file_name = artifact
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("制品文件名无效")?
        .to_string();
    Ok(VerifiedSoftwareArtifact {
        file,
        file_name,
        artifact_path: artifact,
        size,
        sha256,
        receipt_path: path,
    })
}

fn canonical_workspace_file(
    workspace_root: &str,
    target: &str,
) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let root = Path::new(workspace_root).canonicalize()?;
    let requested = Path::new(target);
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    }
    .canonicalize()?;
    if !candidate.starts_with(&root) || !candidate.is_file() {
        return Err("制品必须是 workspace_root 内的普通文件".into());
    }
    Ok((root, candidate))
}

fn open_and_hash(path: &Path) -> Result<(u64, String, File), Box<dyn Error>> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok((size, format!("{:x}", digest.finalize()), file))
}

fn receipts_root() -> PathBuf {
    std::env::var_os("HIMIND_DISTRIBUTION_INSPECTION_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            crate::store::paths::agent_home().join("software-distribution-inspections")
        })
}

fn receipt_path(root: &Path, token: &str) -> Result<PathBuf, Box<dyn Error>> {
    if token.len() < RECEIPT_PREFIX.len() + 32
        || token.len() > 96
        || !token.starts_with(RECEIPT_PREFIX)
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("制品预检凭证格式无效".into());
    }
    Ok(root.join(format!("{token}.receipt")))
}

fn new_receipt_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("{RECEIPT_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn cleanup_expired_receipts(root: &Path, now: u64) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten().take(256) {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("receipt") {
            continue;
        }
        let expired = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .is_some_and(|modified| modified.as_secs() + RECEIPT_TTL_SECONDS < now);
        if expired {
            let _ = fs::remove_file(path);
        }
    }
}

fn required_string(input: &Value, key: &str) -> Result<String, Box<dyn Error>> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{key} 不能为空").into())
}

fn normalized(input: &Value, key: &str) -> String {
    input
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn normalized_default(input: &Value, key: &str, default: &str) -> String {
    let value = normalized(input, key);
    if value.is_empty() {
        default.to_string()
    } else {
        value
    }
}

fn now_seconds() -> Result<u64, Box<dyn Error>> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use super::{
        attach_inspection_receipt_in, consume_inspection_receipt, verify_inspection_receipt_in,
    };
    use crate::api::distribution::SoftwareReleasePublishRequest;
    use crate::capability::types::{InvocationContext, InvocationSource};
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "himind-distribution-receipt-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn request(
        workspace: &std::path::Path,
        artifact: &std::path::Path,
        receipt: &str,
        size: u64,
        sha256: &str,
    ) -> SoftwareReleasePublishRequest {
        SoftwareReleasePublishRequest {
            workspace_root: workspace.to_string_lossy().to_string(),
            artifact_path: artifact.to_string_lossy().to_string(),
            product_id: "com.himind.test".to_string(),
            product_name: "Test".to_string(),
            product_type: "desktop_app".to_string(),
            version: "1.0.0".to_string(),
            channel: "stable".to_string(),
            platform: "windows".to_string(),
            architecture: "x64".to_string(),
            package_type: "content".to_string(),
            release_notes: String::new(),
            mandatory: false,
            rollout_percent: 100,
            inspection_receipt: receipt.to_string(),
            expected_size: size,
            expected_sha256: sha256.to_string(),
            confirmed: true,
        }
    }

    #[cfg(windows)]
    #[test]
    fn receipt_binds_metadata_and_is_single_use() {
        let workspace = root("single-use");
        let receipts = workspace.join("receipts");
        fs::create_dir_all(&workspace).unwrap();
        let artifact = workspace.join("app.zip");
        fs::write(&artifact, b"stable-content").unwrap();
        let sha256 = format!("{:x}", Sha256::digest(b"stable-content"));
        let context = InvocationContext::new(InvocationSource::Mcp, "ai-client:test");
        let input = json!({
            "workspace_root": workspace,
            "artifact_path": artifact,
            "product_id": "com.himind.test",
            "version": "1.0.0",
            "channel": "stable",
            "platform": "windows",
            "architecture": "x64",
            "package_type": "content"
        });
        let output = attach_inspection_receipt_in(
            &context,
            &input,
            json!({"ready": true, "size": 14, "sha256": sha256}),
            &receipts,
        )
        .unwrap();
        let token = output["inspection_receipt"].as_str().unwrap();
        let request = request(&workspace, &artifact, token, 14, &sha256);
        let verified = verify_inspection_receipt_in(&context, &request, &receipts).unwrap();
        assert_eq!(verified.size, 14);
        consume_inspection_receipt(&verified).unwrap();
        assert!(verify_inspection_receipt_in(&context, &request, &receipts).is_err());
        let _ = fs::remove_dir_all(workspace);
    }

    #[cfg(windows)]
    #[test]
    fn receipt_rejects_artifact_replacement() {
        let workspace = root("replacement");
        let receipts = workspace.join("receipts");
        fs::create_dir_all(&workspace).unwrap();
        let artifact = workspace.join("app.zip");
        fs::write(&artifact, b"first").unwrap();
        let sha256 = format!("{:x}", Sha256::digest(b"first"));
        let context = InvocationContext::new(InvocationSource::Mcp, "ai-client:test");
        let input = json!({
            "workspace_root": workspace,
            "artifact_path": artifact,
            "product_id": "com.himind.test",
            "version": "1.0.0",
            "channel": "stable",
            "platform": "windows",
            "architecture": "x64",
            "package_type": "content"
        });
        let output = attach_inspection_receipt_in(
            &context,
            &input,
            json!({"ready": true, "size": 5, "sha256": sha256}),
            &receipts,
        )
        .unwrap();
        fs::write(&artifact, b"second").unwrap();
        let request = request(
            &workspace,
            &artifact,
            output["inspection_receipt"].as_str().unwrap(),
            5,
            &sha256,
        );
        let error = verify_inspection_receipt_in(&context, &request, &receipts)
            .unwrap_err()
            .to_string();
        assert!(error.contains("发生变化"));
        let _ = fs::remove_dir_all(workspace);
    }
}
