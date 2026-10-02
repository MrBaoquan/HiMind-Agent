//! 扩展制品签名。
//!
//! 发布到 GitHub Release 的制品在本机做 RSA-PSS/SHA-256 分离签名，签名元数据与制品
//! 一起上传，消费侧用受信公钥校验。私钥只在本机读取，不写入状态、不上传、不记日志。
//!
//! 未配置私钥时产物照常发布，只是标记为未签名：这样没有签名材料的开发者仍然能把
//! 自己的扩展发到自己的仓库，而要求签名的目录不会收录未签名条目。

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::rand_core::OsRng;
use rsa::{Pss, RsaPrivateKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::error::Error;
use std::path::Path;

/// 私钥路径与 key ID 通过进程环境提供，与仓库发布脚本保持同一套变量名。
pub(crate) const PRIVATE_KEY_ENV: &str = "HIMIND_EXTENSION_SIGNING_PRIVATE_KEY_PATH";
pub(crate) const KEY_ID_ENV: &str = "HIMIND_EXTENSION_SIGNING_KEY_ID";
pub(crate) const SIGNATURE_ALGORITHM: &str = "rsa-pss-sha256";

pub(crate) struct ExtensionSigningKey {
    key_id: String,
    private_key: RsaPrivateKey,
}

impl ExtensionSigningKey {
    pub(crate) fn key_id(&self) -> &str {
        &self.key_id
    }
}

fn read_setting(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 当前机器的签名配置状态。只读、不加载私钥内容，用于发布前展示。
pub(crate) fn status() -> Value {
    let path = read_setting(PRIVATE_KEY_ENV);
    let key_id = read_setting(KEY_ID_ENV);
    let error = match (&path, &key_id) {
        (None, None) => String::new(),
        (Some(_), None) => format!("缺少 {KEY_ID_ENV}"),
        (None, Some(_)) => format!("缺少 {PRIVATE_KEY_ENV}"),
        (Some(path), Some(_)) => {
            if Path::new(path).is_file() {
                String::new()
            } else {
                format!("私钥文件不存在：{path}")
            }
        }
    };
    json!({
        "configured": path.is_some() && key_id.is_some(),
        "key_id": key_id.unwrap_or_default(),
        "error": error,
    })
}

/// 读取本机签名私钥。两处配置都为空时返回 `None`，表示这次发布不带签名。
pub(crate) fn configured_key() -> Result<Option<ExtensionSigningKey>, Box<dyn Error>> {
    key_from_settings(read_setting(PRIVATE_KEY_ENV), read_setting(KEY_ID_ENV))
}

fn key_from_settings(
    path: Option<String>,
    key_id: Option<String>,
) -> Result<Option<ExtensionSigningKey>, Box<dyn Error>> {
    match (path, key_id) {
        (None, None) => Ok(None),
        (Some(_), None) => Err(format!("已配置 {PRIVATE_KEY_ENV}，但缺少 {KEY_ID_ENV}").into()),
        (None, Some(_)) => Err(format!("已配置 {KEY_ID_ENV}，但缺少 {PRIVATE_KEY_ENV}").into()),
        (Some(path), Some(key_id)) => {
            validate_key_id(&key_id)?;
            if !Path::new(&path).is_file() {
                return Err(format!("扩展签名私钥不存在：{path}").into());
            }
            let pem = std::fs::read_to_string(&path)
                .map_err(|error| format!("读取扩展签名私钥失败 {path}: {error}"))?;
            let private_key = RsaPrivateKey::from_pkcs8_pem(&pem)
                .or_else(|_| RsaPrivateKey::from_pkcs1_pem(&pem))
                .map_err(|_| "扩展签名私钥必须是 PKCS#8 或 PKCS#1 PEM 格式".to_string())?;
            Ok(Some(ExtensionSigningKey {
                key_id,
                private_key,
            }))
        }
    }
}

fn validate_key_id(key_id: &str) -> Result<(), Box<dyn Error>> {
    let valid = !key_id.is_empty()
        && key_id.len() <= 64
        && key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err("扩展签名 key ID 只能是字母、数字、点、下划线和连字符".into())
    }
}

/// 对制品生成分离签名元数据。
///
/// 字段名与 `sign-extension.ps1` 的输出保持一致，公共目录工具可直接消费。
pub(crate) fn sign_artifact(
    artifact: &Path,
    key: &ExtensionSigningKey,
) -> Result<Value, Box<dyn Error>> {
    if !artifact.is_file() {
        return Err(format!("待签名制品不存在：{}", artifact.display()).into());
    }
    let file_name = artifact
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    let bytes = std::fs::read(artifact)?;
    let digest = Sha256::digest(&bytes);
    let signature = key
        .private_key
        .sign_with_rng(&mut OsRng, Pss::new::<Sha256>(), &digest)
        .map_err(|error| format!("扩展制品签名失败：{error}"))?;
    Ok(json!({
        "file_name": file_name,
        "file_size": bytes.len(),
        "sha256": digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        "signature": BASE64_STANDARD.encode(&signature),
        "signature_key_id": key.key_id,
        "signature_algorithm": SIGNATURE_ALGORITHM,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::RsaPublicKey;
    use std::path::PathBuf;

    fn scratch(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "himind-extension-signing-{}-{label}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn incomplete_signing_settings_are_rejected() {
        assert!(key_from_settings(None, None).unwrap().is_none());
        assert!(key_from_settings(Some("C:/tmp/key.pem".into()), None).is_err());
        assert!(key_from_settings(None, Some("himind-test".into())).is_err());
        assert!(key_from_settings(Some("C:/tmp/key.pem".into()), Some("bad id".into())).is_err());
    }

    #[test]
    fn signature_metadata_matches_the_artifact_and_verifies() {
        let root = scratch("metadata");
        let artifact = root.join("com.himind.example-1.0.0.hmpkg");
        std::fs::write(&artifact, b"extension artifact").unwrap();
        let mut rng = OsRng;
        let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public_key = RsaPublicKey::from(&private_key);
        let key_path = root.join("key.pem");
        std::fs::write(
            &key_path,
            private_key
                .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        let key = key_from_settings(
            Some(key_path.to_string_lossy().to_string()),
            Some("himind-test".into()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(key.key_id(), "himind-test");

        let metadata = sign_artifact(&artifact, &key).unwrap();
        assert_eq!(metadata["file_name"], "com.himind.example-1.0.0.hmpkg");
        assert_eq!(metadata["signature_key_id"], "himind-test");
        assert_eq!(metadata["signature_algorithm"], SIGNATURE_ALGORITHM);
        assert_eq!(metadata["file_size"], 18);
        let expected = Sha256::digest(std::fs::read(&artifact).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(metadata["sha256"], expected);

        // 同一份制品必须能被对应公钥验签，且换内容后必须失败。
        let public_pem = {
            use rsa::pkcs8::EncodePublicKey;
            public_key
                .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
                .unwrap()
        };
        assert!(crate::app::system::verify_rsa_pss_sha256(
            &artifact,
            &public_pem,
            metadata["signature"].as_str().unwrap()
        )
        .is_ok());
        std::fs::write(&artifact, b"tampered artifact").unwrap();
        assert!(crate::app::system::verify_rsa_pss_sha256(
            &artifact,
            &public_pem,
            metadata["signature"].as_str().unwrap()
        )
        .is_err());
    }
}
