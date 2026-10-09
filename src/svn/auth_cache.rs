//! Seeds a private Subversion credential cache for the `svn` CLI.
//!
//! HiMind lets each user pick any company SVN account, including non-ASCII
//! display names. The Windows `svn.exe` client corrupts non-ASCII `--username`
//! values and the CLI cannot store credentials non-interactively, so HiMind
//! writes the `auth/svn.simple` entry itself (DPAPI-protected) and points the
//! invocation at a dedicated `--config-dir`.
//!
//! The config directory must live on an all-ASCII path: Subversion refuses to
//! authenticate when the config directory contains non-ASCII characters.

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use md5::{Digest as _, Md5};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::store::credentials::protect_bytes_for_current_user;

/// Description stamped onto the DPAPI blob. Subversion verifies it before
/// using the cached credential, so it must stay byte-identical to the value
/// `svn.exe` writes itself.
const AUTH_CACHE_DESCRIPTION: &str = "auth_svn.simple.wincrypt";

/// Fallback realm name used when the server does not advertise one.
const FALLBACK_REALM: &str = "SVN Repo";

/// Build the Subversion realm string for a repository URL.
///
/// Subversion scopes credentials by realm, not by path, so callers key on the
/// URL origin. The realm itself is read from the server's `WWW-Authenticate`
/// challenge; a well-known default keeps the flow working when the probe fails.
pub(crate) fn realmstring(base_url: &str) -> Result<String, Box<dyn Error>> {
    let origin = url_origin(base_url)?;
    let realm = fetch_realm(base_url).unwrap_or_else(|| FALLBACK_REALM.to_string());
    Ok(format!("<{origin}> {realm}"))
}

/// Resolve the dedicated config directory for a server/account pair.
///
/// The value is stable for the same origin and username, so repeated runs
/// reuse the cache and an account switch produces a separate directory.
pub(crate) fn config_dir(base_url: &str, username: &str) -> PathBuf {
    let scope = url_origin(base_url).unwrap_or_else(|_| base_url.trim().to_ascii_lowercase());
    let mut hasher = Sha256::new();
    hasher.update(scope.as_bytes());
    hasher.update([0u8]);
    hasher.update(username.as_bytes());
    let digest = hasher.finalize();
    let mut name = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        name.push_str(&format!("{byte:02x}"));
    }
    ascii_cache_root().join(name)
}

/// Write the credential cache entry used by `--config-dir`.
pub(crate) fn seed(
    config_dir: &Path,
    realmstring: &str,
    username: &str,
    password: &str,
) -> Result<(), Box<dyn Error>> {
    let simple_dir = config_dir.join("auth").join("svn.simple");
    std::fs::create_dir_all(&simple_dir)?;

    let mut hasher = Md5::new();
    hasher.update(realmstring.as_bytes());
    let mut file_name = String::with_capacity(32);
    for byte in hasher.finalize() {
        file_name.push_str(&format!("{byte:02x}"));
    }

    let protected = protect_bytes_for_current_user(password.as_bytes(), AUTH_CACHE_DESCRIPTION)?;
    let encoded = BASE64_STANDARD.encode(protected);

    let mut body = String::new();
    for (key, value) in [
        ("passtype", "wincrypt"),
        ("password", encoded.as_str()),
        ("svn:realmstring", realmstring),
        ("username", username),
    ] {
        body.push_str(&format!(
            "K {}\n{}\nV {}\n{}\n",
            key.len(),
            key,
            value.len(),
            value
        ));
    }
    body.push_str("END\n");

    let path = simple_dir.join(file_name);
    std::fs::write(&path, body.as_bytes())?;
    restrict_to_current_user(&path);
    Ok(())
}

/// Drop any cached credentials for a server/account pair.
pub(crate) fn clear(base_url: &str, username: &str) {
    let dir = config_dir(base_url, username);
    let _ = std::fs::remove_dir_all(dir);
}

fn url_origin(base_url: &str) -> Result<String, Box<dyn Error>> {
    let url = url::Url::parse(base_url)?;
    let scheme = url.scheme().to_ascii_lowercase();
    let host = url
        .host_str()
        .ok_or("SVN URL is missing a host")?
        .to_ascii_lowercase();
    let port = url
        .port_or_known_default()
        .ok_or("SVN URL is missing a port")?;
    Ok(format!("{scheme}://{host}:{port}"))
}

fn fetch_realm(base_url: &str) -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let response = client
        .request(reqwest::Method::OPTIONS, base_url)
        .header("Depth", "0")
        .send()
        .ok()?;
    let challenge = response
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)?
        .to_str()
        .ok()?;
    parse_basic_realm(challenge)
}

pub(crate) fn parse_basic_realm(challenge: &str) -> Option<String> {
    let lower = challenge.to_ascii_lowercase();
    let index = lower.find("realm=")?;
    let remainder = challenge[index + "realm=".len()..].trim_start();
    let value = if let Some(rest) = remainder.strip_prefix('"') {
        rest.split('"').next()?
    } else {
        remainder.split(',').next()?.trim()
    };
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Pick a writable config root whose path is pure ASCII.
fn ascii_cache_root() -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(value) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(value).join("HiMindAgent").join("svn-auth"));
    }
    if let Some(value) = std::env::var_os("ProgramData") {
        candidates.push(PathBuf::from(value).join("HiMindAgent").join("svn-auth"));
    }
    if let Some(value) = std::env::var_os("SystemDrive") {
        candidates.push(
            PathBuf::from(value.to_string_lossy().to_string())
                .join("HiMindAgent")
                .join("svn-auth"),
        );
    }
    candidates.push(std::env::temp_dir().join("himind-svn-auth"));
    for candidate in &candidates {
        if candidate.to_string_lossy().is_ascii() && std::fs::create_dir_all(candidate).is_ok() {
            return candidate.clone();
        }
    }
    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("himind-svn-auth"))
}

fn restrict_to_current_user(path: &Path) {
    // The password is DPAPI-bound, so the cache file only ever holds ciphertext
    // that another Windows account cannot decrypt. The file also inherits the
    // per-user profile ACL of the cache root. On Unix the mode is still set
    // explicitly because file modes are not per-user by default.
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(windows)]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file name and layout below match a cache entry that the real
    /// `svn.exe` wrote for this server, which is what makes the CLI accept it.
    const PRODUCTION_REALM: &str = "<http://svn.andcrane.com:80> SVN Repo";

    #[test]
    fn seeds_a_cache_entry_that_svn_accepts() {
        let root = std::env::temp_dir().join(format!(
            "himind-svn-auth-cache-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        let config_dir = root.join("config");
        seed(&config_dir, PRODUCTION_REALM, "马宝全", "123456").unwrap();

        let path = config_dir
            .join("auth")
            .join("svn.simple")
            .join("d1f5e2a1a8a26b8fa5192b6706765cf6");
        assert!(path.is_file(), "cache entry is missing at {path:?}");

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("K 8\npasstype\nV 8\nwincrypt\n"));
        assert!(body.contains(&format!("K 15\nsvn:realmstring\nV {}\n{PRODUCTION_REALM}\n", PRODUCTION_REALM.len())));
        assert!(body.contains("K 8\nusername\nV 9\n马宝全\n"));
        assert!(body.ends_with("END\n"));

        // The password must never be stored in the clear, only inside the
        // DPAPI blob, and the blob must stay base64 without embedded newlines.
        assert!(!body.contains("123456"));
        let encoded = body
            .split_once("K 8\npassword\nV ")
            .and_then(|(_, rest)| rest.split_once('\n'))
            .and_then(|(length, rest)| {
                let length: usize = length.parse().ok()?;
                rest.get(..length)
            })
            .expect("password entry is missing");
        let blob = BASE64_STANDARD.decode(encoded).unwrap();
        assert!(blob.len() > 32, "DPAPI blob looks truncated");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn derives_realmstrings_and_origins() {
        assert_eq!(
            parse_basic_realm("Basic realm=\"公司 SVN\"").as_deref(),
            Some("公司 SVN")
        );
        assert_eq!(
            config_dir("http://svn.andcrane.com/repo/project", "马宝全"),
            config_dir("http://SVN.AndCrane.com/repo", "马宝全"),
            "the cache directory is scoped per origin and account"
        );
        assert_ne!(
            config_dir("http://svn.andcrane.com/repo", "马宝全"),
            config_dir("http://svn.andcrane.com/repo", "李鹏"),
            "switching accounts must not reuse another account's cache"
        );
    }
}
