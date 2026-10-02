use reqwest::blocking::Client;
use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::api::distribution::{
    resolve_runtime_component as resolve_dashboard_runtime_component, RuntimeComponentUpdate,
};
use crate::app::system::validate_signature_metadata;
use crate::Options;

pub(crate) const RUNTIME_PRODUCT_ID: &str = "com.himind.runtime.deepseek-harness";
pub(crate) const RUNTIME_CONTRACT: &str = "himind.builtin";
pub(crate) const RUNTIME_ENGINE_ID: &str = "deepseek-harness";
pub(crate) const RUNTIME_CHANNEL: &str = "stable";
pub(crate) const RUNTIME_PLATFORM: &str = "windows";
pub(crate) const RUNTIME_ARCHITECTURE: &str = "x64";
pub(crate) const RUNTIME_MAX_PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;

const SOURCE_ENV: &str = "HIMIND_RUNTIME_DISTRIBUTION_SOURCE";
const LOCAL_MANIFEST_ENV: &str = "HIMIND_RUNTIME_LOCAL_MANIFEST";
const GITHUB_REPOSITORY_ENV: &str = "HIMIND_RUNTIME_GITHUB_REPOSITORY";
const DEFAULT_GITHUB_REPOSITORY: &str = "MrBaoquan/HiMind-Agent";
const GITHUB_RELEASE_MANIFEST_ASSET: &str = "himind-runtime-release.json";
const GITHUB_RELEASES_PER_PAGE: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeDistributionSource {
    Local,
    Github,
    Dashboard,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedRuntimeComponent {
    pub update: RuntimeComponentUpdate,
    pub source: RuntimeDistributionSource,
}

pub(crate) struct RuntimeDistributionRequest<'a> {
    pub options: &'a Options,
    pub current_version: &'a str,
    pub client_instance_id: &'a str,
}

trait RuntimeDistributionProvider {
    fn source(&self) -> RuntimeDistributionSource;

    fn resolve(
        &self,
        request: &RuntimeDistributionRequest<'_>,
    ) -> Result<Option<RuntimeComponentUpdate>, Box<dyn Error>>;
}

pub(crate) fn resolve_configured_runtime_component(
    options: &Options,
    current_version: &str,
    client_instance_id: &str,
    timeout: Duration,
) -> Result<Option<ResolvedRuntimeComponent>, Box<dyn Error>> {
    let source = configured_source(options)?;
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| format!("创建 Runtime Distribution 客户端失败: {error}"))?;
    let provider: Box<dyn RuntimeDistributionProvider> = match source {
        RuntimeDistributionSource::Local => Box::new(LocalRuntimeProvider::from_env()?),
        RuntimeDistributionSource::Github => Box::new(GitHubRuntimeProvider::new(
            client,
            configured_github_repository()?,
        )),
        RuntimeDistributionSource::Dashboard => Box::new(DashboardRuntimeProvider { client }),
    };
    let request = RuntimeDistributionRequest {
        options,
        current_version,
        client_instance_id,
    };
    Ok(provider
        .resolve(&request)?
        .map(|update| ResolvedRuntimeComponent {
            update,
            source: provider.source(),
        }))
}

pub(crate) fn use_local_manifest(path: &str) -> Result<(), Box<dyn Error>> {
    let guard = use_local_manifest_scoped(path)?;
    std::mem::forget(guard);
    Ok(())
}

pub(crate) struct LocalManifestOverrideGuard {
    previous_source: Option<std::ffi::OsString>,
    previous_manifest: Option<std::ffi::OsString>,
}

impl Drop for LocalManifestOverrideGuard {
    fn drop(&mut self) {
        match self.previous_source.take() {
            Some(value) => env::set_var(SOURCE_ENV, value),
            None => env::remove_var(SOURCE_ENV),
        }
        match self.previous_manifest.take() {
            Some(value) => env::set_var(LOCAL_MANIFEST_ENV, value),
            None => env::remove_var(LOCAL_MANIFEST_ENV),
        }
    }
}

pub(crate) fn use_local_manifest_scoped(
    path: &str,
) -> Result<LocalManifestOverrideGuard, Box<dyn Error>> {
    let manifest = PathBuf::from(path.trim());
    let manifest = manifest.canonicalize().map_err(|error| {
        format!(
            "本地 Runtime 发布清单不可读取 {}: {error}",
            manifest.display()
        )
    })?;
    if !manifest.is_file() {
        return Err(format!("本地 Runtime 发布清单不是文件: {}", manifest.display()).into());
    }
    let guard = LocalManifestOverrideGuard {
        previous_source: env::var_os(SOURCE_ENV),
        previous_manifest: env::var_os(LOCAL_MANIFEST_ENV),
    };
    env::set_var(SOURCE_ENV, "local");
    env::set_var(LOCAL_MANIFEST_ENV, manifest);
    Ok(guard)
}

fn configured_source(options: &Options) -> Result<RuntimeDistributionSource, Box<dyn Error>> {
    let configured = env::var(SOURCE_ENV).unwrap_or_else(|_| "auto".to_string());
    match configured.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => {
            if env::var_os(LOCAL_MANIFEST_ENV).is_some_and(|value| !value.is_empty()) {
                Ok(RuntimeDistributionSource::Local)
            } else if options.mode().dashboard_enabled() {
                Ok(RuntimeDistributionSource::Dashboard)
            } else {
                Ok(RuntimeDistributionSource::Github)
            }
        }
        "local" => Ok(RuntimeDistributionSource::Local),
        "github" => Ok(RuntimeDistributionSource::Github),
        "dashboard" => Ok(RuntimeDistributionSource::Dashboard),
        value => Err(
            format!("{SOURCE_ENV} 只能是 auto、local、github 或 dashboard，收到 {value}").into(),
        ),
    }
}

struct LocalRuntimeProvider {
    manifest_path: PathBuf,
}

impl LocalRuntimeProvider {
    fn from_env() -> Result<Self, Box<dyn Error>> {
        let manifest_path = env::var_os(LOCAL_MANIFEST_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                format!("本地 Runtime Provider 需要设置 {LOCAL_MANIFEST_ENV} 指向签名发布清单")
            })?;
        let manifest_path = manifest_path.canonicalize().map_err(|error| {
            format!(
                "本地 Runtime 发布清单不可读取 {}: {error}",
                manifest_path.display()
            )
        })?;
        if !manifest_path.is_file() {
            return Err(
                format!("本地 Runtime 发布清单不是文件: {}", manifest_path.display()).into(),
            );
        }
        Ok(Self { manifest_path })
    }
}

impl RuntimeDistributionProvider for LocalRuntimeProvider {
    fn source(&self) -> RuntimeDistributionSource {
        RuntimeDistributionSource::Local
    }

    fn resolve(
        &self,
        request: &RuntimeDistributionRequest<'_>,
    ) -> Result<Option<RuntimeComponentUpdate>, Box<dyn Error>> {
        let manifest = read_runtime_release_manifest(&self.manifest_path)?;
        let manifest_root = self
            .manifest_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let artifact_path = manifest_root.join(&manifest.file_name);
        let artifact_path = artifact_path.canonicalize().map_err(|error| {
            format!(
                "本地 Runtime 制品不可读取 {}: {error}",
                artifact_path.display()
            )
        })?;
        if !artifact_path.is_file() || !artifact_path.starts_with(manifest_root) {
            return Err("本地 Runtime 制品必须位于发布清单同级目录".into());
        }
        let artifact_size = fs::metadata(&artifact_path)?.len();
        runtime_update_from_manifest(
            manifest,
            artifact_path.to_string_lossy().to_string(),
            Some(artifact_size),
            request.current_version,
        )
    }
}

struct GitHubRuntimeProvider {
    client: Client,
    repository: String,
}

impl GitHubRuntimeProvider {
    fn new(client: Client, repository: String) -> Self {
        Self { client, repository }
    }
}

impl RuntimeDistributionProvider for GitHubRuntimeProvider {
    fn source(&self) -> RuntimeDistributionSource {
        RuntimeDistributionSource::Github
    }

    fn resolve(
        &self,
        request: &RuntimeDistributionRequest<'_>,
    ) -> Result<Option<RuntimeComponentUpdate>, Box<dyn Error>> {
        let releases = self
            .client
            .get(format!(
                "https://api.github.com/repos/{}/releases?per_page={GITHUB_RELEASES_PER_PAGE}",
                self.repository,
            ))
            .header("User-Agent", "HiMind-Agent")
            .header("Accept", "application/vnd.github+json")
            .send()?
            .error_for_status()?
            .json::<Vec<GithubRelease>>()?;
        resolve_github_runtime_release(&releases, request.current_version, |release| {
            let manifest_asset = release
                .assets
                .iter()
                .find(|asset| asset.name == GITHUB_RELEASE_MANIFEST_ASSET)
                .ok_or("GitHub Runtime Release 缺少 himind-runtime-release.json")?;
            self.client
                .get(&manifest_asset.browser_download_url)
                .header("User-Agent", "HiMind-Agent")
                .send()?
                .error_for_status()?
                .json::<RuntimeReleaseManifest>()
                .map_err(Into::into)
        })
    }
}

fn resolve_github_runtime_release<F>(
    releases: &[GithubRelease],
    current_version: &str,
    mut load_manifest: F,
) -> Result<Option<RuntimeComponentUpdate>, Box<dyn Error>>
where
    F: FnMut(&GithubRelease) -> Result<RuntimeReleaseManifest, Box<dyn Error>>,
{
    let mut candidates = Vec::new();
    for release in releases.iter().filter(|release| !release.draft) {
        let Some(tag_version) = runtime_release_version(&release.tag_name) else {
            continue;
        };
        if !release
            .assets
            .iter()
            .any(|asset| asset.name == GITHUB_RELEASE_MANIFEST_ASSET)
        {
            continue;
        }
        let manifest = load_manifest(release)?;
        if manifest.version.trim() != tag_version {
            return Err(format!(
                "GitHub Runtime Release 标签与发布清单版本不一致: tag={}, manifest={}",
                release.tag_name, manifest.version
            )
            .into());
        }
        let version = semver::Version::parse(manifest.version.trim())
            .map_err(|error| format!("GitHub Runtime Release 版本号无效: {error}"))?;
        let artifact = release
            .assets
            .iter()
            .find(|asset| asset.name == manifest.file_name)
            .ok_or_else(|| {
                format!(
                    "GitHub Runtime Release 缺少发布清单声明的制品 {}",
                    manifest.file_name
                )
            })?;
        candidates.push((
            version,
            manifest,
            artifact.browser_download_url.clone(),
            artifact.size,
        ));
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    let Some((_, manifest, artifact_url, artifact_size)) = candidates.into_iter().next() else {
        return Ok(None);
    };
    runtime_update_from_manifest(
        manifest,
        artifact_url,
        (artifact_size != 0).then_some(artifact_size),
        current_version,
    )
}

fn runtime_release_version(tag_name: &str) -> Option<&str> {
    let tag_name = tag_name.trim();
    ["runtime-v", "runtime-", "v"]
        .into_iter()
        .find_map(|prefix| {
            tag_name
                .strip_prefix(prefix)
                .filter(|value| !value.is_empty())
        })
}

struct DashboardRuntimeProvider {
    client: Client,
}

impl RuntimeDistributionProvider for DashboardRuntimeProvider {
    fn source(&self) -> RuntimeDistributionSource {
        RuntimeDistributionSource::Dashboard
    }

    fn resolve(
        &self,
        request: &RuntimeDistributionRequest<'_>,
    ) -> Result<Option<RuntimeComponentUpdate>, Box<dyn Error>> {
        resolve_dashboard_runtime_component(
            &self.client,
            &request.options.api_base(),
            RUNTIME_PRODUCT_ID,
            request.current_version,
            RUNTIME_CHANNEL,
            RUNTIME_PLATFORM,
            RUNTIME_ARCHITECTURE,
            request.client_instance_id,
        )
    }
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
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
struct RuntimeReleaseManifest {
    schema_version: u32,
    product_id: String,
    version: String,
    channel: String,
    platform: String,
    architecture: String,
    package_type: String,
    file_name: String,
    #[serde(alias = "size_bytes")]
    size: u64,
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
    published_at: String,
    #[serde(default)]
    release_notes: String,
    #[serde(default)]
    min_agent_version: String,
    #[serde(default)]
    max_agent_version: String,
    #[serde(default)]
    capabilities: Vec<String>,
}

fn read_runtime_release_manifest(
    manifest_path: &Path,
) -> Result<RuntimeReleaseManifest, Box<dyn Error>> {
    serde_json::from_slice(&fs::read(manifest_path)?).map_err(|error| {
        format!(
            "Runtime 发布清单格式无效 {}: {error}",
            manifest_path.display()
        )
        .into()
    })
}

fn runtime_update_from_manifest(
    manifest: RuntimeReleaseManifest,
    artifact_url: String,
    artifact_size: Option<u64>,
    current_version: &str,
) -> Result<Option<RuntimeComponentUpdate>, Box<dyn Error>> {
    validate_runtime_release_manifest(&manifest, artifact_size)?;
    if !runtime_version_is_newer(current_version, &manifest.version)? {
        return Ok(None);
    }
    Ok(Some(RuntimeComponentUpdate {
        product_id: manifest.product_id,
        version: manifest.version,
        release_name: String::new(),
        release_notes: manifest.release_notes,
        channel: manifest.channel,
        artifact_url,
        file_name: manifest.file_name,
        package_type: manifest.package_type,
        sha256: manifest.sha256.to_ascii_lowercase(),
        size: manifest.size,
        mandatory: manifest.mandatory,
        published_at: manifest.published_at,
        signature: manifest.signature,
        signature_key_id: manifest.signature_key_id,
        signature_algorithm: manifest.signature_algorithm,
        min_agent_version: manifest.min_agent_version,
        max_agent_version: manifest.max_agent_version,
        capabilities: manifest.capabilities,
    }))
}

fn validate_runtime_release_manifest(
    manifest: &RuntimeReleaseManifest,
    artifact_size: Option<u64>,
) -> Result<(), Box<dyn Error>> {
    if manifest.schema_version != 1
        || manifest.product_id != RUNTIME_PRODUCT_ID
        || manifest.channel != RUNTIME_CHANNEL
        || manifest.platform != RUNTIME_PLATFORM
        || manifest.architecture != RUNTIME_ARCHITECTURE
        || manifest.package_type != "directory-zip"
    {
        return Err("Runtime 发布清单的产品、渠道、平台或包类型不符合契约".into());
    }
    if semver::Version::parse(manifest.version.trim()).is_err() {
        return Err("Runtime 发布清单版本必须是语义化版本".into());
    }
    for (label, value) in [
        ("min_agent_version", manifest.min_agent_version.trim()),
        ("max_agent_version", manifest.max_agent_version.trim()),
    ] {
        if !value.is_empty() && semver::Version::parse(value).is_err() {
            return Err(format!("Runtime 发布清单 {label} 不是语义化版本").into());
        }
    }
    let mut capabilities = HashSet::new();
    if manifest.capabilities.len() > 32
        || manifest.capabilities.iter().any(|capability| {
            let capability = capability.trim();
            capability.is_empty()
                || capability.len() > 64
                || !capability
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
                || !capabilities.insert(capability.to_string())
        })
    {
        return Err("Runtime 发布清单 capabilities 非法或重复".into());
    }
    let file_name = Path::new(&manifest.file_name);
    if manifest.file_name.trim().is_empty()
        || file_name.file_name() != Some(std::ffi::OsStr::new(manifest.file_name.trim()))
        || !manifest.file_name.to_ascii_lowercase().ends_with(".zip")
    {
        return Err("Runtime 发布清单文件名不安全或不是 ZIP".into());
    }
    if manifest.size == 0 || manifest.size > RUNTIME_MAX_PACKAGE_BYTES {
        return Err("Runtime 发布清单包大小超出安全范围".into());
    }
    if artifact_size.is_some_and(|size| size != manifest.size) {
        return Err("Runtime 发布清单大小与制品不一致".into());
    }
    if manifest.sha256.len() != 64 || !manifest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("Runtime 发布清单缺少合法 SHA-256".into());
    }
    validate_signature_metadata(
        &manifest.signature,
        &manifest.signature_key_id,
        &manifest.signature_algorithm,
        true,
    )?;
    Ok(())
}

fn runtime_version_is_newer(
    current_version: &str,
    candidate_version: &str,
) -> Result<bool, Box<dyn Error>> {
    let current = current_version.trim();
    if current.is_empty() || current == "0.0.0" {
        return Ok(true);
    }
    let current = semver::Version::parse(current)
        .map_err(|error| format!("当前 Runtime 版本号无效: {error}"))?;
    let candidate = semver::Version::parse(candidate_version.trim())
        .map_err(|error| format!("候选 Runtime 版本号无效: {error}"))?;
    match candidate.cmp(&current) {
        std::cmp::Ordering::Greater => Ok(true),
        std::cmp::Ordering::Equal => Ok(false),
        std::cmp::Ordering::Less => {
            Err(format!("拒绝将 Runtime 从 {current_version} 降级到 {candidate_version}").into())
        }
    }
}

fn configured_github_repository() -> Result<String, Box<dyn Error>> {
    let value =
        env::var(GITHUB_REPOSITORY_ENV).unwrap_or_else(|_| DEFAULT_GITHUB_REPOSITORY.to_string());
    let value = value.trim().trim_end_matches('/').trim_end_matches(".git");
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    let valid_segment = |segment: &str| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    if parts.next().is_some() || !valid_segment(owner) || !valid_segment(name) {
        return Err(format!("{GITHUB_REPOSITORY_ENV} 必须是 owner/repo").into());
    }
    Ok(format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::{
        configured_github_repository, resolve_github_runtime_release, runtime_update_from_manifest,
        GithubAsset, GithubRelease, RuntimeReleaseManifest, GITHUB_REPOSITORY_ENV,
        RUNTIME_ARCHITECTURE, RUNTIME_CHANNEL, RUNTIME_PLATFORM, RUNTIME_PRODUCT_ID,
    };
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    fn manifest(version: &str, size: u64) -> RuntimeReleaseManifest {
        RuntimeReleaseManifest {
            schema_version: 1,
            product_id: RUNTIME_PRODUCT_ID.to_string(),
            version: version.to_string(),
            channel: RUNTIME_CHANNEL.to_string(),
            platform: RUNTIME_PLATFORM.to_string(),
            architecture: RUNTIME_ARCHITECTURE.to_string(),
            package_type: "directory-zip".to_string(),
            file_name: "runtime.zip".to_string(),
            size,
            sha256: "a".repeat(64),
            signature: "c2lnbmF0dXJl".to_string(),
            signature_key_id: "runtime-test-key".to_string(),
            signature_algorithm: "rsa-pss-sha256".to_string(),
            mandatory: false,
            published_at: String::new(),
            release_notes: String::new(),
            min_agent_version: String::new(),
            max_agent_version: String::new(),
            capabilities: Vec::new(),
        }
    }

    fn release(tag_name: &str, draft: bool, assets: &[(&str, &str, u64)]) -> GithubRelease {
        GithubRelease {
            tag_name: tag_name.to_string(),
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
    fn resolves_a_newer_local_runtime_manifest() {
        let update = runtime_update_from_manifest(
            manifest("1.2.3", 10),
            PathBuf::from("runtime.zip").to_string_lossy().to_string(),
            Some(10),
            "1.2.2",
        )
        .unwrap()
        .expect("newer manifest");
        assert_eq!(update.product_id, RUNTIME_PRODUCT_ID);
        assert_eq!(update.version, "1.2.3");
    }

    #[test]
    fn resolves_the_highest_runtime_release_from_mixed_github_releases() {
        let releases = vec![
            release(
                "v0.3.47",
                false,
                &[("himind-agent-update.json", "agent-manifest", 0)],
            ),
            release(
                "runtime-v0.1.5-rc.2",
                false,
                &[
                    ("himind-runtime-release.json", "runtime-manifest-rc2", 0),
                    ("runtime.zip", "runtime-artifact-rc2", 10),
                ],
            ),
            release(
                "runtime-v0.1.4",
                false,
                &[
                    ("himind-runtime-release.json", "runtime-manifest-old", 0),
                    ("runtime.zip", "runtime-artifact-old", 10),
                ],
            ),
            release(
                "runtime-v0.2.0",
                true,
                &[
                    ("himind-runtime-release.json", "runtime-manifest-draft", 0),
                    ("runtime.zip", "runtime-artifact-draft", 10),
                ],
            ),
        ];

        let update = resolve_github_runtime_release(&releases, "0.1.0", |release| {
            let version = match release.tag_name.as_str() {
                "runtime-v0.1.5-rc.2" => "0.1.5-rc.2",
                "runtime-v0.1.4" => "0.1.4",
                "runtime-v0.2.0" => "0.2.0",
                other => return Err(format!("unexpected release {other}").into()),
            };
            Ok(manifest(version, 10))
        })
        .unwrap()
        .expect("newer runtime release");

        assert_eq!(update.version, "0.1.5-rc.2");
        assert_eq!(update.artifact_url, "runtime-artifact-rc2");
    }

    #[test]
    fn accepts_signed_runtime_prerelease_versions() {
        let update = runtime_update_from_manifest(
            manifest("1.2.3-rc.2", 10),
            "runtime.zip".to_string(),
            Some(10),
            "1.2.3-rc.1",
        )
        .unwrap()
        .expect("newer prerelease runtime");
        assert_eq!(update.version, "1.2.3-rc.2");
    }

    #[test]
    fn ignores_the_already_installed_version() {
        let update = runtime_update_from_manifest(
            manifest("1.2.3", 10),
            "runtime.zip".to_string(),
            Some(10),
            "1.2.3",
        )
        .unwrap();
        assert!(update.is_none());
    }

    #[test]
    fn carries_runtime_agent_compatibility_metadata() {
        let mut release = manifest("1.2.3", 10);
        release.min_agent_version = "0.3.40".to_string();
        release.max_agent_version = "0.4.0".to_string();
        release.capabilities = vec!["interactive".to_string(), "workflow".to_string()];
        let update =
            runtime_update_from_manifest(release, "runtime.zip".to_string(), Some(10), "1.2.2")
                .unwrap()
                .expect("newer manifest");
        assert_eq!(update.min_agent_version, "0.3.40");
        assert_eq!(update.max_agent_version, "0.4.0");
        assert_eq!(update.capabilities, vec!["interactive", "workflow"]);
    }

    #[test]
    fn rejects_runtime_downgrades_and_size_mismatches() {
        let downgrade = runtime_update_from_manifest(
            manifest("1.2.2", 10),
            "runtime.zip".to_string(),
            Some(10),
            "1.2.3",
        )
        .unwrap_err();
        assert!(downgrade.to_string().contains("降级"));

        let mismatch = runtime_update_from_manifest(
            manifest("1.2.3", 10),
            "runtime.zip".to_string(),
            Some(9),
            "1.2.2",
        )
        .unwrap_err();
        assert!(mismatch.to_string().contains("大小"));
    }

    #[test]
    fn validates_github_runtime_repository_setting() {
        env::remove_var(GITHUB_REPOSITORY_ENV);
        assert_eq!(
            configured_github_repository().unwrap(),
            "MrBaoquan/HiMind-Agent"
        );
        env::set_var(GITHUB_REPOSITORY_ENV, "Owner/runtime.git");
        assert_eq!(configured_github_repository().unwrap(), "Owner/runtime");
        env::set_var(GITHUB_REPOSITORY_ENV, "https://evil.example/runtime");
        assert!(configured_github_repository().is_err());
        env::remove_var(GITHUB_REPOSITORY_ENV);
    }

    #[test]
    fn scoped_local_manifest_restores_previous_environment() {
        let root = std::env::temp_dir().join(format!(
            "himind-runtime-manifest-scope-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        let manifest = root.join("runtime-release.json");
        fs::write(&manifest, b"{}").unwrap();
        let previous_source = env::var_os("HIMIND_RUNTIME_DISTRIBUTION_SOURCE");
        let previous_manifest = env::var_os("HIMIND_RUNTIME_LOCAL_MANIFEST");
        env::set_var("HIMIND_RUNTIME_DISTRIBUTION_SOURCE", "dashboard");
        env::set_var("HIMIND_RUNTIME_LOCAL_MANIFEST", "previous.json");
        {
            let _guard =
                super::use_local_manifest_scoped(manifest.to_string_lossy().as_ref()).unwrap();
            assert_eq!(
                env::var("HIMIND_RUNTIME_DISTRIBUTION_SOURCE").unwrap(),
                "local"
            );
            assert_eq!(
                env::var("HIMIND_RUNTIME_LOCAL_MANIFEST").unwrap(),
                manifest
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            );
        }
        assert_eq!(
            env::var("HIMIND_RUNTIME_DISTRIBUTION_SOURCE").unwrap(),
            "dashboard"
        );
        assert_eq!(
            env::var("HIMIND_RUNTIME_LOCAL_MANIFEST").unwrap(),
            "previous.json"
        );
        match previous_source {
            Some(value) => env::set_var("HIMIND_RUNTIME_DISTRIBUTION_SOURCE", value),
            None => env::remove_var("HIMIND_RUNTIME_DISTRIBUTION_SOURCE"),
        }
        match previous_manifest {
            Some(value) => env::set_var("HIMIND_RUNTIME_LOCAL_MANIFEST", value),
            None => env::remove_var("HIMIND_RUNTIME_LOCAL_MANIFEST"),
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_windows_manifest_fixture() {
        let root = std::env::temp_dir().join(format!(
            "himind-runtime-distribution-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("runtime-release.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "product_id": RUNTIME_PRODUCT_ID,
                "version": "1.2.3",
                "channel": RUNTIME_CHANNEL,
                "platform": RUNTIME_PLATFORM,
                "architecture": RUNTIME_ARCHITECTURE,
                "package_type": "directory-zip",
                "file_name": "runtime.zip",
                "size": 10,
                "sha256": "a".repeat(64),
                "signature": "c2lnbmF0dXJl",
                "signature_key_id": "runtime-test-key",
                "signature_algorithm": "rsa-pss-sha256"
            }))
            .unwrap(),
        )
        .unwrap();
        let value = super::read_runtime_release_manifest(&path).unwrap();
        assert_eq!(value.version, "1.2.3");
        let _ = fs::remove_dir_all(root);
    }
}
