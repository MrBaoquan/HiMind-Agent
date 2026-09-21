use crate::api::distribution::UpdateCheckResponse;
use crate::Options;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::error::Error;

const DEFAULT_REPOSITORY: &str = "MrBaoquan/HiMind-Agent";
const UPDATE_MANIFEST_ASSET: &str = "himind-agent-update.json";
const UPDATE_ARCHIVE_NAME: &str = "himind-agent-update.zip";
const GITHUB_RELEASES_PER_PAGE: u32 = 100;

#[derive(Debug, Clone, Deserialize)]
struct GithubRelease {
    id: u64,
    tag_name: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
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
    let endpoint = format!(
        "https://api.github.com/repos/{repository}/releases?per_page={GITHUB_RELEASES_PER_PAGE}"
    );
    let releases = client
        .get(endpoint)
        .header("User-Agent", "HiMind-Agent")
        .header("Accept", "application/vnd.github+json")
        .send()?
        .error_for_status()?
        .json::<Vec<GithubRelease>>()?;
    let (release, manifest, update_asset) = resolve_github_agent_release(&releases, |release| {
        let manifest_asset = release
            .assets
            .iter()
            .find(|asset| asset.name == UPDATE_MANIFEST_ASSET)
            .ok_or("GitHub Release 缺少 himind-agent-update.json 更新索引")?;
        client
            .get(&manifest_asset.browser_download_url)
            .header("User-Agent", "HiMind-Agent")
            .send()?
            .error_for_status()?
            .json::<UpdateManifest>()
            .map_err(Into::into)
    })?;
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
        download_url: update_asset.browser_download_url,
        sha256: manifest.sha256,
        signature: manifest.signature,
        signature_key_id: manifest.signature_key_id,
        signature_algorithm: manifest.signature_algorithm,
        mandatory: manifest.mandatory,
        min_supported_version: manifest.min_supported_version,
        release_notes,
    })
}

fn resolve_github_agent_release<F>(
    releases: &[GithubRelease],
    mut load_manifest: F,
) -> Result<(GithubRelease, UpdateManifest, GithubAsset), Box<dyn Error>>
where
    F: FnMut(&GithubRelease) -> Result<UpdateManifest, Box<dyn Error>>,
{
    let mut candidates = Vec::new();
    for release in releases.iter().filter(|release| !release.draft) {
        let Some(tag_version) = agent_release_version(&release.tag_name) else {
            continue;
        };
        if !release
            .assets
            .iter()
            .any(|asset| asset.name == UPDATE_MANIFEST_ASSET)
        {
            continue;
        }
        let manifest = load_manifest(release)?;
        if manifest.product != "himind-agent" {
            return Err("GitHub 更新索引的产品标识不是 himind-agent".into());
        }
        if !valid_version(&manifest.version) {
            return Err("GitHub 更新索引缺少版本号".into());
        }
        if tag_version != manifest.version.trim() {
            return Err("GitHub Release 标签与更新索引版本不一致".into());
        }
        if manifest.package_type != "directory-zip" {
            return Err("GitHub Agent 更新包必须是 directory-zip".into());
        }
        if manifest.file_name != UPDATE_ARCHIVE_NAME {
            return Err("GitHub Agent 更新索引的文件名必须是 himind-agent-update.zip".into());
        }
        if manifest.size_bytes == 0
            || manifest.sha256.len() != 64
            || !manifest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("GitHub 更新索引缺少合法的大小或 SHA-256".into());
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
        let version = semver::Version::parse(manifest.version.trim())
            .map_err(|error| format!("GitHub Agent Release 版本号无效: {error}"))?;
        let update_asset = release
            .assets
            .iter()
            .find(|asset| asset.name == manifest.file_name)
            .ok_or("GitHub Release 缺少 himind-agent-update.zip 更新包")?
            .clone();
        if update_asset.size != 0 && update_asset.size != manifest.size_bytes {
            return Err("GitHub 更新索引的包大小与 Release asset 不一致".into());
        }
        candidates.push((version, release.clone(), manifest, update_asset));
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    candidates
        .into_iter()
        .next()
        .map(|(_, release, manifest, asset)| (release, manifest, asset))
        .ok_or_else(|| "GitHub Release 缺少 himind-agent-update.json 更新索引".into())
}

fn agent_release_version(tag_name: &str) -> Option<&str> {
    let tag_name = tag_name.trim();
    ["agent-v", "v"].into_iter().find_map(|prefix| {
        tag_name
            .strip_prefix(prefix)
            .filter(|value| !value.is_empty())
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
    use super::{
        configured_repository, resolve_github_agent_release, valid_version, GithubAsset,
        GithubRelease, UpdateManifest,
    };

    fn manifest(version: &str) -> UpdateManifest {
        UpdateManifest {
            product: "himind-agent".to_string(),
            version: version.to_string(),
            channel: "stable".to_string(),
            file_name: "himind-agent-update.zip".to_string(),
            package_type: "directory-zip".to_string(),
            size_bytes: 10,
            sha256: "a".repeat(64),
            signature: "c2lnbmF0dXJl".to_string(),
            signature_key_id: "agent-test-key".to_string(),
            signature_algorithm: "rsa-pss-sha256".to_string(),
            mandatory: false,
            min_supported_version: "0.0.0".to_string(),
            release_notes: String::new(),
        }
    }

    fn release(tag_name: &str, draft: bool, assets: &[(&str, &str, u64)]) -> GithubRelease {
        GithubRelease {
            id: 7,
            tag_name: tag_name.to_string(),
            name: String::new(),
            body: String::new(),
            draft,
            assets: assets
                .iter()
                .map(|(name, url, size)| GithubAsset {
                    name: (*name).to_string(),
                    browser_download_url: (*url).to_string(),
                    size: *size,
                })
                .collect(),
        }
    }

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

    #[test]
    fn selects_agent_release_from_mixed_product_releases() {
        let releases = vec![
            release(
                "runtime-v0.1.5-rc.2",
                false,
                &[
                    ("himind-runtime-release.json", "runtime-manifest", 0),
                    ("runtime.zip", "runtime-artifact", 10),
                ],
            ),
            release(
                "v0.3.47",
                false,
                &[
                    ("himind-agent-update.json", "agent-manifest-new", 0),
                    ("himind-agent-update.zip", "agent-artifact-new", 10),
                ],
            ),
            release(
                "v0.3.46",
                false,
                &[
                    ("himind-agent-update.json", "agent-manifest-old", 0),
                    ("himind-agent-update.zip", "agent-artifact-old", 10),
                ],
            ),
        ];

        let (selected, manifest, asset) = resolve_github_agent_release(&releases, |release| {
            let version = match release.tag_name.as_str() {
                "v0.3.47" => "0.3.47",
                "v0.3.46" => "0.3.46",
                other => return Err(format!("unexpected release {other}").into()),
            };
            Ok(manifest(version))
        })
        .unwrap();

        assert_eq!(selected.tag_name, "v0.3.47");
        assert_eq!(manifest.version, "0.3.47");
        assert_eq!(asset.name, "himind-agent-update.zip");
    }

    #[test]
    fn ignores_draft_agent_release_candidates() {
        let releases = vec![release(
            "v0.3.48",
            true,
            &[
                ("himind-agent-update.json", "agent-manifest", 0),
                ("himind-agent-update.zip", "agent-artifact", 10),
            ],
        )];

        let error =
            resolve_github_agent_release(&releases, |_| Ok(manifest("0.3.48"))).unwrap_err();
        assert!(error.to_string().contains("缺少"));
    }
}
