use crate::api::distribution::{distribution_trust_bundle, DistributionTrustBundle};
use crate::store::atomic_file;
use crate::Options;
use reqwest::blocking::Client;
use rsa::pkcs8::DecodePublicKey;
use rsa::RsaPublicKey;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TRUST_BUNDLE_SCHEMA_VERSION: &str = "distribution_trust_bundle.v1";
const MAX_TRUST_KEYS: usize = 32;
const MAX_REVOKED_KEYS: usize = 128;
const MAX_PUBLIC_KEY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TrustSyncReport {
    pub active_key_id: String,
    pub imported_keys: usize,
    pub revoked_key_ids: Vec<String>,
    pub target_directory: String,
    pub bundle_digest: String,
    pub generated_at: String,
    pub synced_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TrustSyncState {
    pub schema_version: String,
    pub active_key_id: String,
    pub key_ids: Vec<String>,
    pub revoked_key_ids: Vec<String>,
    pub bundle_digest: String,
    pub generated_at: String,
    pub synced_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TrustStatusReport {
    pub healthy: bool,
    pub issues: Vec<String>,
    pub local_key_ids: Vec<String>,
    pub state: TrustSyncState,
}

pub(crate) fn sync(options: &Options) -> Result<TrustSyncReport, Box<dyn Error>> {
    let state = crate::api::client::load_agent_state(&options.state_path)?;
    if state.agent_id.trim().is_empty() || state.credential.trim().is_empty() {
        return Err("HiMind 账号尚未授权".into());
    }
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let bundle = distribution_trust_bundle(
        &client,
        &options.api_base(),
        &state.agent_id,
        &state.credential,
    )?;
    let target_directory = trusted_keys_directory()?;
    install_trust_bundle(&bundle, &target_directory)
}

pub(crate) fn status() -> Result<TrustStatusReport, Box<dyn Error>> {
    let target_directory = trusted_keys_directory()?;
    audit_trust_directory(&target_directory)
}

pub(crate) fn verify() -> Result<TrustStatusReport, Box<dyn Error>> {
    let report = status()?;
    if report.healthy {
        return Ok(report);
    }
    Err(format!(
        "distribution trust audit failed: {}",
        report.issues.join("; ")
    )
    .into())
}

fn audit_trust_directory(target_directory: &Path) -> Result<TrustStatusReport, Box<dyn Error>> {
    let path = target_directory.join("trust-state.json");
    if !path.is_file() {
        return Err("distribution trust state is unavailable".into());
    }
    let state: TrustSyncState = serde_json::from_slice(&std::fs::read(path)?)?;
    let mut issues = Vec::new();
    if state.schema_version != "distribution_trust_sync_state.v1" {
        issues.push(format!(
            "unsupported trust state schema: {}",
            state.schema_version
        ));
    }
    let mut keys = Vec::with_capacity(state.key_ids.len());
    for key_id in &state.key_ids {
        let path = target_directory.join(format!("{key_id}.pem"));
        if !path.is_file() {
            issues.push(format!("trusted public key is missing: {key_id}"));
            continue;
        }
        let public_key_pem = std::fs::read_to_string(&path)?;
        if let Err(error) = RsaPublicKey::from_public_key_pem(&public_key_pem) {
            issues.push(format!("trusted public key is invalid {key_id}: {error}"));
            continue;
        }
        keys.push(crate::api::distribution::DistributionTrustKey {
            key_id: key_id.clone(),
            public_key_pem,
        });
    }
    let revoked_path = target_directory.join("revoked-keys.json");
    let local_revoked = if revoked_path.is_file() {
        match serde_json::from_slice::<serde_json::Value>(&std::fs::read(&revoked_path)?) {
            Ok(value) => value
                .get("revoked_key_ids")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            Err(error) => {
                issues.push(format!("revoked key list is invalid: {error}"));
                Vec::new()
            }
        }
    } else {
        issues.push("revoked key list is missing".to_string());
        Vec::new()
    };
    if local_revoked != state.revoked_key_ids {
        issues.push("revoked key list differs from the synchronized state".to_string());
    }
    if keys.len() == state.key_ids.len() {
        let bundle = DistributionTrustBundle {
            schema_version: TRUST_BUNDLE_SCHEMA_VERSION.to_string(),
            active_key_id: state.active_key_id.clone(),
            revoked_key_ids: local_revoked,
            keys,
            generated_at: state.generated_at.clone(),
        };
        if trust_bundle_digest(&bundle)? != state.bundle_digest {
            issues.push("trusted public keys differ from the synchronized bundle".to_string());
        }
    }
    let mut local_key_ids = Vec::new();
    for entry in std::fs::read_dir(target_directory)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("pem") {
            continue;
        }
        if let Some(key_id) = path.file_stem().and_then(|value| value.to_str()) {
            local_key_ids.push(key_id.to_string());
        }
    }
    local_key_ids.sort();
    Ok(TrustStatusReport {
        healthy: issues.is_empty(),
        issues,
        local_key_ids,
        state,
    })
}

fn trusted_keys_directory() -> Result<PathBuf, Box<dyn Error>> {
    std::env::var_os("HIMIND_TRUSTED_SIGNING_KEYS_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HIMIND_TRUSTED_SIGNING_KEYS_DIR is not configured".into())
}

fn install_trust_bundle(
    bundle: &DistributionTrustBundle,
    target_directory: &Path,
) -> Result<TrustSyncReport, Box<dyn Error>> {
    validate_trust_bundle(bundle)?;
    std::fs::create_dir_all(target_directory)?;

    for key in &bundle.keys {
        write_if_changed(
            &target_directory.join(format!("{}.pem", key.key_id)),
            key.public_key_pem.as_bytes(),
        )?;
    }
    let revoked = json!({
        "revoked_key_ids": bundle.revoked_key_ids,
    });
    write_if_changed(
        &target_directory.join("revoked-keys.json"),
        &serde_json::to_vec_pretty(&revoked)?,
    )?;
    let bundle_digest = trust_bundle_digest(bundle)?;
    let synced_at = unix_timestamp_string();
    let state = TrustSyncState {
        schema_version: "distribution_trust_sync_state.v1".to_string(),
        active_key_id: bundle.active_key_id.clone(),
        key_ids: bundle.keys.iter().map(|key| key.key_id.clone()).collect(),
        revoked_key_ids: bundle.revoked_key_ids.clone(),
        bundle_digest: bundle_digest.clone(),
        generated_at: bundle.generated_at.clone(),
        synced_at: synced_at.clone(),
    };
    write_if_changed(
        &target_directory.join("trust-state.json"),
        &serde_json::to_vec_pretty(&state)?,
    )?;

    Ok(TrustSyncReport {
        active_key_id: bundle.active_key_id.clone(),
        imported_keys: bundle.keys.len(),
        revoked_key_ids: bundle.revoked_key_ids.clone(),
        target_directory: target_directory.to_string_lossy().to_string(),
        bundle_digest,
        generated_at: bundle.generated_at.clone(),
        synced_at,
    })
}

fn trust_bundle_digest(bundle: &DistributionTrustBundle) -> Result<String, Box<dyn Error>> {
    let material = json!({
        "active_key_id": &bundle.active_key_id,
        "keys": &bundle.keys,
        "revoked_key_ids": &bundle.revoked_key_ids,
    });
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&material)?)
    ))
}

fn unix_timestamp_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_default()
}

fn write_if_changed(path: &Path, content: &[u8]) -> Result<(), Box<dyn Error>> {
    if path.is_file() && std::fs::read(path)? == content {
        return Ok(());
    }
    atomic_file::atomic_write(path, content)?;
    Ok(())
}

fn validate_trust_bundle(bundle: &DistributionTrustBundle) -> Result<(), Box<dyn Error>> {
    if bundle.schema_version != TRUST_BUNDLE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported distribution trust bundle schema: {}",
            bundle.schema_version
        )
        .into());
    }
    validate_key_id(&bundle.active_key_id)?;
    if bundle.keys.is_empty() || bundle.keys.len() > MAX_TRUST_KEYS {
        return Err("distribution trust bundle contains an invalid key count".into());
    }
    if bundle.revoked_key_ids.len() > MAX_REVOKED_KEYS {
        return Err("distribution trust bundle contains too many revoked keys".into());
    }
    let mut key_ids = HashSet::new();
    let mut active_present = false;
    for key in &bundle.keys {
        validate_key_id(&key.key_id)?;
        if !key_ids.insert(key.key_id.as_str()) {
            return Err(format!("duplicate distribution trust key: {}", key.key_id).into());
        }
        if key.public_key_pem.len() > MAX_PUBLIC_KEY_BYTES {
            return Err(format!("distribution public key is too large: {}", key.key_id).into());
        }
        RsaPublicKey::from_public_key_pem(&key.public_key_pem)
            .map_err(|error| format!("invalid RSA public key {}: {error}", key.key_id))?;
        active_present |= key.key_id == bundle.active_key_id;
    }
    if !active_present {
        return Err("distribution active signing key is missing from the bundle".into());
    }
    let mut revoked = HashSet::new();
    for key_id in &bundle.revoked_key_ids {
        validate_key_id(key_id)?;
        if !revoked.insert(key_id.as_str()) {
            return Err(format!("duplicate revoked signing key: {key_id}").into());
        }
        if key_id == &bundle.active_key_id {
            return Err("distribution active signing key is revoked".into());
        }
    }
    Ok(())
}

fn validate_key_id(key_id: &str) -> Result<(), Box<dyn Error>> {
    if key_id.trim() != key_id
        || key_id.is_empty()
        || key_id.len() > 64
        || key_id.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
    {
        return Err(format!("invalid distribution signing key id: {key_id}").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::EncodePublicKey;
    use std::sync::MutexGuard;

    /// 与 `app::system` 的测试共用同一把锁：两边动的是同一个环境变量
    /// （`HIMIND_TRUSTED_SIGNING_KEYS_DIR`），各持一把锁会互相插队 —— 这边刚
    /// 把变量删掉、那边正读它，签名相关用例就会随机失败。锁必须只有一把。
    fn env_lock() -> MutexGuard<'static, ()> {
        crate::app::system::signing_env_lock()
    }

    fn public_key() -> String {
        let private = rsa::RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).unwrap();
        private
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap()
    }

    #[test]
    fn installs_only_validated_keys_and_revocation_list() {
        let root = std::env::temp_dir().join(format!(
            "himind-trust-sync-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bundle = DistributionTrustBundle {
            schema_version: TRUST_BUNDLE_SCHEMA_VERSION.to_string(),
            active_key_id: "new-key".to_string(),
            revoked_key_ids: vec!["old-key".to_string()],
            keys: vec![
                crate::api::distribution::DistributionTrustKey {
                    key_id: "old-key".to_string(),
                    public_key_pem: public_key(),
                },
                crate::api::distribution::DistributionTrustKey {
                    key_id: "new-key".to_string(),
                    public_key_pem: public_key(),
                },
            ],
            generated_at: "100".to_string(),
        };
        let report = install_trust_bundle(&bundle, &root).unwrap();
        assert_eq!(report.imported_keys, 2);
        assert!(root.join("old-key.pem").is_file());
        assert!(root.join("new-key.pem").is_file());
        let revoked: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("revoked-keys.json")).unwrap())
                .unwrap();
        assert_eq!(revoked["revoked_key_ids"][0], "old-key");
        let state: TrustSyncState =
            serde_json::from_slice(&std::fs::read(root.join("trust-state.json")).unwrap()).unwrap();
        assert_eq!(state.active_key_id, "new-key");
        assert_eq!(state.key_ids, vec!["old-key", "new-key"]);
        assert_eq!(state.revoked_key_ids, vec!["old-key"]);
        assert_eq!(state.bundle_digest.len(), 64);
        let audit = audit_trust_directory(&root).unwrap();
        assert!(audit.healthy, "{:?}", audit.issues);
        std::fs::write(root.join("new-key.pem"), b"tampered").unwrap();
        let tampered = audit_trust_directory(&root).unwrap();
        assert!(!tampered.healthy);
        assert!(tampered
            .issues
            .iter()
            .any(|issue| issue.contains("trusted public key is invalid")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_revoked_active_key_before_writing() {
        let root = std::env::temp_dir().join(format!(
            "himind-trust-sync-rejected-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bundle = DistributionTrustBundle {
            schema_version: TRUST_BUNDLE_SCHEMA_VERSION.to_string(),
            active_key_id: "key-1".to_string(),
            revoked_key_ids: vec!["key-1".to_string()],
            keys: vec![crate::api::distribution::DistributionTrustKey {
                key_id: "key-1".to_string(),
                public_key_pem: public_key(),
            }],
            generated_at: "100".to_string(),
        };
        assert!(install_trust_bundle(&bundle, &root).is_err());
        assert!(!root.exists());
    }

    #[test]
    fn trusted_directory_must_be_configured() {
        let _guard = env_lock();
        let previous = std::env::var_os("HIMIND_TRUSTED_SIGNING_KEYS_DIR");
        std::env::remove_var("HIMIND_TRUSTED_SIGNING_KEYS_DIR");
        let error = trusted_keys_directory().unwrap_err();
        match previous {
            Some(value) => std::env::set_var("HIMIND_TRUSTED_SIGNING_KEYS_DIR", value),
            None => std::env::remove_var("HIMIND_TRUSTED_SIGNING_KEYS_DIR"),
        }
        assert!(error.to_string().contains("not configured"));
    }
}
