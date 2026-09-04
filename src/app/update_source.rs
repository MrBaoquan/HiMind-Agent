use crate::api::distribution::UpdateCheckResponse;
use crate::Options;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::error::Error;

const DEFAULT_REPOSITORY: &str = "MrBaoquan/HiMind-Agent";
const UPDATE_MANIFEST_ASSET: &str = "himind-agent-update.json";
const UPDATE_ARCHIVE_NAME: &str = "himind-agent-update.zip";

#[derive(Debug, Deserialize)]
struct GithubRelease {
    id: u64,
    tag_name: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Deserialize)]
struct UpdateManifest {
    #[serde(default)]
    product: String,
    version: String,
    #[serde(default)]
    channel: String,
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    package_type: String,
    #[serde(default)]
    size_bytes: u64,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    signature: String,
    #[serde(default)]
    signature_key_id: String,
    #[serde(default)]
    signature_algorithm: String,
    #[serde(default)]
    mandatory: bool,
    #[serde(default)]
    min_supported_version: String,
    #[serde(default)]
    release_notes: String,
}

pub(crate) fn check_github(
    client: &Client,
    _options: &Options,
) -> Result<UpdateCheckResponse, Box<dyn Error>> {
    let repository = configured_repository()?;
    let endpoint = format!("https://api.github.com/repos/{repository}/releases/latest");
    let release = client
        .get(endpoint)
        .header("User-Agent", "HiMind-Agent")
        .header("Accept", "application/vnd.github+json")
        .send()?
        .error_for_status()?
        .json::<GithubRelease>()?;
    let manifest_asset = release
        .assets
        .iter()
        .find(|asset| asset.name == UPDATE_MANIFEST_ASSET)
        .ok_or("GitHub Release 缺少 himind-agent-update.json 更新索引")?;
    let manifest = client
        .get(&manifest_asset.browser_download_url)
        .header("User-Agent", "HiMind-Agent")
        .send()?
        .error_for_status()?
        .json::<UpdateManifest>()?;
    if manifest.product != "himind-agent" {
        return Err("GitHub 更新索引的产品标识不是 himind-agent".into());
    }
    if !valid_version(&manifest.version) {
        return Err("GitHub 更新索引缺少版本号".into());
    }
    let release_version = release.tag_name.trim().trim_start_matches('v');
    if release_version != manifest.version.trim() {
        return Err("GitHub Release 标签与更新索引版本不一致".into());
    }
    if manifest.package_type != "directory-zip" {
        return Err("GitHub Agent 更新包必须是 directory-zip".into());
    }
    if manifest.file_name != UPDATE_ARCHIVE_NAME {
        return Err("GitHub Agent 更新索引的文件名必须是 himind-agent-update.zip".into());
    }
    let update_asset = release
        .assets
        .iter()
        .find(|asset| asset.name == manifest.file_name)
        .ok_or("GitHub Release 缺少 himind-agent-update.zip 更新包")?;
    if manifest.size_bytes == 0
        || manifest.sha256.len() != 64
        || !manifest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("GitHub 更新索引缺少合法的大小或 SHA-256".into());
    }
    if update_asset.size != 0 && update_asset.size != manifest.size_bytes {
        return Err("GitHub 更新索引的包大小与 Release asset 不一致".into());
    }
    let channel = if manifest.channel.trim().is_empty() {
        "stable"
    } else {
        manifest.channel.as_str()
    };
    if channel != "stable" {
        return Err(format!("GitHub Agent 当前只支持 stable 发布渠道，收到 {channel}").into());
    }
    if manifest.version.contains('-') {
        return Err("stable GitHub Release 不允许预发布版本".into());
    }
    let has_update = crate::skill::resolver::compare_versions(&manifest.version, crate::VERSION)
        == std::cmp::Ordering::Greater;
    let release_notes = if manifest.release_notes.trim().is_empty() {
        if !release.body.trim().is_empty() {
            release.body
        } else {
            release.name
        }
    } else {
        manifest.release_notes
    };
    Ok(UpdateCheckResponse {
        has_update,
        version: manifest.version,
        release_id: if release.id == 0 {
            release.tag_name
        } else {
            release.id.to_string()
        },
        file_name: manifest.file_name,
        package_type: manifest.package_type,
        size_bytes: manifest.size_bytes as i64,
        download_url: update_asset.browser_download_url.clone(),
        sha256: manifest.sha256,
        signature: manifest.signature,
        signature_key_id: manifest.signature_key_id,
        signature_algorithm: manifest.signature_algorithm,
        mandatory: manifest.mandatory,
        min_supported_version: manifest.min_supported_version,
        release_notes,
    })
}

pub(crate) fn configured_repository() -> Result<String, Box<dyn Error>> {
    let value = std::env::var("HIMIND_AGENT_GITHUB_REPOSITORY")
        .unwrap_or_else(|_| DEFAULT_REPOSITORY.to_string());
    let value = value.trim().trim_end_matches('/').trim_end_matches(".git");
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || owner.is_empty()
        || name.is_empty()
        || !owner.bytes().all(valid_segment_byte)
        || !name.bytes().all(valid_segment_byte)
    {
        return Err("HIMIND_AGENT_GITHUB_REPOSITORY 必须是 owner/repo".into());
    }
    Ok(format!("{owner}/{name}"))
}

fn valid_segment_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

fn valid_version(value: &str) -> bool {
    let value = value.trim();
    let mut pieces = value.splitn(2, |character| character == '-' || character == '+');
    let base = pieces.next().unwrap_or_default();
    let suffix = pieces.next().unwrap_or_default();
    let parts = base.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && (suffix.is_empty()
            || suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
}

#[cfg(test)]
mod tests {
    use super::{configured_repository, valid_version};

    #[test]
    fn validates_github_repository_setting() {
        std::env::remove_var("HIMIND_AGENT_GITHUB_REPOSITORY");
        assert_eq!(configured_repository().unwrap(), "MrBaoquan/HiMind-Agent");
        std::env::set_var("HIMIND_AGENT_GITHUB_REPOSITORY", "Owner/repo.git");
        assert_eq!(configured_repository().unwrap(), "Owner/repo");
        std::env::set_var(
            "HIMIND_AGENT_GITHUB_REPOSITORY",
            "https://evil.example/repo",
        );
        assert!(configured_repository().is_err());
        std::env::remove_var("HIMIND_AGENT_GITHUB_REPOSITORY");
    }

    #[test]
    fn validates_release_versions() {
        assert!(valid_version("0.3.40"));
        assert!(valid_version("0.3.40-rc.1"));
        assert!(valid_version("0.3.40+build.7"));
        assert!(!valid_version("v0.3.40"));
        assert!(!valid_version("0.3"));
        assert!(!valid_version("0.3.40/evil"));
    }
}
