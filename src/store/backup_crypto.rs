//! 备份包的口令封装。
//!
//! Agent 落盘的秘密走 DPAPI（只对当前 Windows 用户可解），这决定了它
//! 跨不了机器。备份包需要的是另一种东西：一个用户能带走、能在另一台机器
//! 上重新解开的口令。所以这里不做"换一种本机密钥"，而是明确用
//! PBKDF2-HMAC-SHA256 从口令派生 256 位密钥，再用 AES-256-GCM 认证加密。
//!
//! 口令就是全部：它丢了，包里的凭据就解不开，没有后门也没有找回入口。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use pbkdf2::pbkdf2_hmac;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::error::Error;

const ENVELOPE_VERSION: u32 = 1;
const KDF: &str = "pbkdf2-hmac-sha256";
const CIPHER: &str = "aes-256-gcm";
const ITERATIONS: u32 = 210_000;
/// 解包时接受的迭代次数上限。文件是用户从外部拿来的，不能让一个自称
/// 迭代十亿次的包把界面卡死。
const MAX_ITERATIONS: u32 = 4_000_000;
const SALT_BYTES: usize = 16;
const NONCE_BYTES: usize = 12;
const KEY_BYTES: usize = 32;

/// 导出时强制的最短口令长度。太短的口令等于没有加密。
pub(crate) const MIN_PASSPHRASE_CHARS: usize = 8;

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    kdf: String,
    cipher: String,
    iterations: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

pub(crate) fn validate_passphrase(passphrase: &str) -> Result<String, Box<dyn Error>> {
    // 前后空格一律忽略：口令是手输两遍的，粘贴带来的空白只会变成"解不开"。
    let normalized = passphrase.trim();
    if normalized.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(format!("口令至少需要 {MIN_PASSPHRASE_CHARS} 个字符").into());
    }
    Ok(normalized.to_string())
}

pub(crate) fn seal(plaintext: &[u8], passphrase: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let passphrase = validate_passphrase(passphrase)?;
    let mut salt = [0_u8; SALT_BYTES];
    let mut nonce_bytes = [0_u8; NONCE_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

    let mut key = [0_u8; KEY_BYTES];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), &salt, ITERATIONS, &mut key);
    let cipher = Aes256Gcm::new_from_slice(&key)?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|_| "备份凭据加密失败")?;
    key.fill(0);

    let envelope = Envelope {
        version: ENVELOPE_VERSION,
        kdf: KDF.to_string(),
        cipher: CIPHER.to_string(),
        iterations: ITERATIONS,
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce_bytes),
        ciphertext: STANDARD.encode(ciphertext),
    };
    Ok(serde_json::to_vec_pretty(&envelope)?)
}

pub(crate) fn open(envelope: &[u8], passphrase: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let passphrase = validate_passphrase(passphrase)?;
    let envelope: Envelope = serde_json::from_slice(envelope)?;
    if envelope.version != ENVELOPE_VERSION {
        return Err(format!("不支持的备份凭据版本 {}", envelope.version).into());
    }
    if envelope.kdf != KDF || envelope.cipher != CIPHER {
        return Err("备份凭据使用了无法识别的加密方式".into());
    }
    if envelope.iterations == 0 || envelope.iterations > MAX_ITERATIONS {
        return Err("备份凭据的迭代次数超出可接受范围".into());
    }
    let salt = STANDARD.decode(envelope.salt.trim())?;
    let nonce = STANDARD.decode(envelope.nonce.trim())?;
    let ciphertext = STANDARD.decode(envelope.ciphertext.trim())?;
    if salt.len() != SALT_BYTES || nonce.len() != NONCE_BYTES {
        return Err("备份凭据的参数长度不正确".into());
    }

    let mut key = [0_u8; KEY_BYTES];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), &salt, envelope.iterations, &mut key);
    let cipher = Aes256Gcm::new_from_slice(&key)?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_slice())
        .map_err(|_| "口令不正确，或备份凭据已被改动")?;
    key.fill(0);
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::{open, seal, validate_passphrase};

    #[test]
    fn sealed_payload_round_trips_and_needs_the_same_passphrase() {
        let sealed = seal(b"{\"1\":\"secret\"}", "correct horse battery").unwrap();
        assert_eq!(
            open(&sealed, "correct horse battery").unwrap(),
            b"{\"1\":\"secret\"}"
        );
        // 换口令必须失败，而不是解出一段垃圾。
        assert!(open(&sealed, "correct horse batteru").is_err());
        // 同一个明文两次封装的密文不同（盐和 nonce 都是随机的）。
        let again = seal(b"{\"1\":\"secret\"}", "correct horse battery").unwrap();
        assert_ne!(sealed, again);
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let sealed = seal(b"payload", "another passphrase").unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&sealed).unwrap();
        let text = value["ciphertext"].as_str().unwrap().to_string();
        let mut bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, text.as_bytes())
                .unwrap();
        bytes[0] ^= 0x01;
        value["ciphertext"] = serde_json::Value::String(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &bytes,
        ));
        assert!(open(&serde_json::to_vec(&value).unwrap(), "another passphrase").is_err());
    }

    #[test]
    fn short_or_blank_passphrases_are_refused() {
        assert!(validate_passphrase("short").is_err());
        assert!(validate_passphrase("        ").is_err());
        assert!(validate_passphrase("  longer than eight  ").is_ok());
    }
}
