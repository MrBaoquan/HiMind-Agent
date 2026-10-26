use image::GenericImageView;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const OVERRIDE_SCHEMA_VERSION: u32 = 1;
const MAX_ICON_BYTES: u64 = 256 * 1024;
const MIN_ICON_DIMENSION: u32 = 64;
const MAX_ICON_DIMENSION: u32 = 1024;
const ICON_CACHE_DIRECTORY: &str = "extension-icons";
const ICON_OVERRIDES_FILE: &str = "overrides.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExtensionIconInfo {
    /// Opaque custom-protocol URL. Never expose a filesystem path to the webview.
    pub url: String,
    pub sha256: String,
    /// user | author
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct IconOverrideStore {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    #[serde(default)]
    items: BTreeMap<String, StoredIconOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredIconOverride {
    sha256: String,
    extension: String,
    updated_at: String,
}

fn default_schema_version() -> u32 {
    OVERRIDE_SCHEMA_VERSION
}

fn icon_cache_dir() -> PathBuf {
    crate::store::paths::agent_home().join(ICON_CACHE_DIRECTORY)
}

fn override_store_path() -> PathBuf {
    crate::store::paths::agent_home()
        .join(ICON_CACHE_DIRECTORY)
        .join(ICON_OVERRIDES_FILE)
}

fn override_key(kind: &str, id: &str) -> String {
    format!("{}:{}", kind.trim(), id.trim())
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().to_string())
        .unwrap_or_default()
}

pub(crate) fn validate_extension_kind(kind: &str) -> Result<&str, String> {
    let kind = kind.trim();
    match kind {
        "plugin" | "skill" | "workflow" | "expert" | "instruction" => Ok(kind),
        _ => Err(format!("不支持的扩展类型: {kind}")),
    }
}

pub(crate) fn validate_extension_id(id: &str) -> Result<&str, String> {
    let id = id.trim();
    if id.is_empty() || id.len() > 200 {
        return Err("扩展 ID 不能为空且不能超过 200 字节".into());
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err("扩展 ID 含有非法字符".into());
    }
    Ok(id)
}

/// Manifest-level validation. This does not require the file to exist yet, but
/// rejects paths that could escape the extension package.
pub(crate) fn validate_icon_reference(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("扩展图标路径不能为空".into());
    }
    if value.len() > 400 {
        return Err("扩展图标路径不能超过 400 字节".into());
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("扩展图标必须是包内相对路径，不能跳出扩展目录".into());
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "png" | "webp") {
        return Err("扩展图标只支持 PNG 或 WebP".into());
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn cached_icon_url(file_name: &str) -> String {
    // Tauri maps custom protocols to `<scheme>.<host>` on Windows. WebView2
    // loads the mapped URL reliably; the opaque filesystem boundary is unchanged.
    format!("http://extension-icon.localhost/{file_name}")
}

fn detect_extension(bytes: &[u8]) -> Option<(&'static str, image::ImageFormat)> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some(("png", image::ImageFormat::Png));
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(("webp", image::ImageFormat::WebP));
    }
    None
}

pub(crate) fn cache_icon_bytes(
    bytes: &[u8],
    source: &str,
) -> Result<Option<ExtensionIconInfo>, String> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_ICON_BYTES {
        return Err(format!(
            "扩展图标必须大于 0 且不超过 {} KB",
            MAX_ICON_BYTES / 1024
        ));
    }
    let Some((extension, format)) = detect_extension(bytes) else {
        return Err("扩展图标只支持有效的 PNG 或 WebP".into());
    };
    let decoded = image::load_from_memory_with_format(bytes, format)
        .map_err(|_| "扩展图标文件已损坏或无法解码".to_string())?;
    let (width, height) = decoded.dimensions();
    if width < MIN_ICON_DIMENSION
        || height < MIN_ICON_DIMENSION
        || width > MAX_ICON_DIMENSION
        || height > MAX_ICON_DIMENSION
    {
        return Err(format!(
            "扩展图标尺寸必须在 {MIN_ICON_DIMENSION}–{MAX_ICON_DIMENSION}px 之间，当前 {width}×{height}"
        ));
    }
    let sha256 = hex_sha256(bytes);
    let directory = icon_cache_dir();
    fs::create_dir_all(&directory).map_err(|error| format!("创建图标缓存失败: {error}"))?;
    let file_name = format!("{sha256}.{extension}");
    let path = directory.join(&file_name);
    if !path.is_file() {
        crate::store::atomic_file::atomic_write(&path, bytes)
            .map_err(|error| format!("写入图标缓存失败: {error}"))?;
    }
    Ok(Some(ExtensionIconInfo {
        url: cached_icon_url(&file_name),
        sha256,
        source: source.to_string(),
    }))
}

fn load_override_store() -> IconOverrideStore {
    let path = override_store_path();
    let Ok(content) = fs::read(&path) else {
        return IconOverrideStore::default();
    };
    serde_json::from_slice::<IconOverrideStore>(&content)
        .ok()
        .filter(|store| store.schema_version <= OVERRIDE_SCHEMA_VERSION)
        .unwrap_or_default()
}

fn save_override_store(store: &IconOverrideStore) -> Result<(), String> {
    let path = override_store_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建图标配置目录失败: {error}"))?;
    }
    let bytes =
        serde_json::to_vec_pretty(store).map_err(|error| format!("序列化图标配置失败: {error}"))?;
    crate::store::atomic_file::atomic_write(&path, &bytes)
        .map_err(|error| format!("保存图标配置失败: {error}"))
}

pub(crate) fn resolve_user_override(kind: &str, id: &str) -> Option<ExtensionIconInfo> {
    let store = load_override_store();
    let record = store.items.get(&override_key(kind, id))?;
    let file_name = format!("{}.{}", record.sha256, record.extension);
    if !valid_cached_icon_name(&file_name) {
        return None;
    }
    let path = icon_cache_dir().join(&file_name);
    if !path.is_file() {
        return None;
    }
    Some(ExtensionIconInfo {
        url: cached_icon_url(&file_name),
        sha256: record.sha256.clone(),
        source: "user".to_string(),
    })
}

pub(crate) fn resolve_manifest_icon(
    kind: &str,
    id: &str,
    package_root: &Path,
    declared: Option<&str>,
) -> Result<Option<ExtensionIconInfo>, String> {
    let Some(declared) = declared.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    validate_icon_reference(declared)?;
    let relative = Path::new(declared);
    let root = package_root
        .canonicalize()
        .map_err(|error| format!("读取扩展目录失败: {error}"))?;
    let candidate = root.join(relative);
    let canonical = match candidate.canonicalize() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    if !canonical.starts_with(&root) {
        return Err("扩展图标路径跳出了扩展目录".into());
    }
    let metadata = match fs::metadata(&canonical) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return Ok(None),
    };
    if metadata.len() > MAX_ICON_BYTES {
        return Err(format!("扩展图标不能超过 {} KB", MAX_ICON_BYTES / 1024));
    }
    let bytes = fs::read(&canonical).map_err(|error| format!("读取扩展图标失败: {error}"))?;
    match cache_icon_bytes(&bytes, "author") {
        Ok(icon) => Ok(icon),
        Err(error) => {
            // A malformed author icon must not prevent the extension from loading.
            eprintln!("extension icon ignored ({kind}:{id}): {error}");
            Ok(None)
        }
    }
}

pub(crate) fn resolve_effective_icon(
    kind: &str,
    id: &str,
    package_root: &Path,
    declared: Option<&str>,
) -> Result<Option<ExtensionIconInfo>, String> {
    if let Some(icon) = resolve_user_override(kind, id) {
        return Ok(Some(icon));
    }
    resolve_manifest_icon(kind, id, package_root, declared)
}

fn cached_icon_path(sha256: &str) -> Option<PathBuf> {
    for extension in ["png", "webp"] {
        let path = icon_cache_dir().join(format!("{sha256}.{extension}"));
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Cache an icon advertised by a remote catalog. The icon URL must share the
/// artifact's origin and its declared SHA-256 must match the downloaded bytes.
pub(crate) fn cache_remote_icon(
    icon: &ExtensionIconInfo,
    artifact_url: &str,
) -> Option<ExtensionIconInfo> {
    if icon.url.starts_with("extension-icon://")
        || icon.url.starts_with("http://extension-icon.localhost/")
        || icon.url.starts_with("https://extension-icon.localhost/")
    {
        return cached_icon_path(&icon.sha256).map(|_| icon.clone());
    }
    let icon_url = url::Url::parse(icon.url.trim()).ok()?;
    let artifact_url = url::Url::parse(artifact_url.trim()).ok()?;
    if !matches!(icon_url.scheme(), "https" | "http")
        || icon_url.scheme() != artifact_url.scheme()
        || icon_url.host_str() != artifact_url.host_str()
        || icon_url.port_or_known_default() != artifact_url.port_or_known_default()
    {
        return None;
    }
    let declared = icon.sha256.trim().to_ascii_lowercase();
    if declared.len() != 64 || !declared.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    if let Some(path) = cached_icon_path(&declared) {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("png");
        return Some(ExtensionIconInfo {
            url: cached_icon_url(&format!("{declared}.{extension}")),
            sha256: declared,
            source: "author".to_string(),
        });
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("HiMind-Agent")
        .build()
        .ok()?;
    let response = client.get(icon_url).send().ok()?.error_for_status().ok()?;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_ICON_BYTES)
    {
        return None;
    }
    let bytes = response.bytes().ok()?;
    if bytes.len() as u64 > MAX_ICON_BYTES {
        return None;
    }
    let cached = cache_icon_bytes(&bytes, "author").ok()??;
    if !cached.sha256.eq_ignore_ascii_case(&declared) {
        return None;
    }
    Some(cached)
}

pub(crate) fn icon_artifact_path(icon: &ExtensionIconInfo) -> Option<PathBuf> {
    cached_icon_path(&icon.sha256)
}

pub(crate) fn set_user_override(
    kind: &str,
    id: &str,
    source: &Path,
) -> Result<ExtensionIconInfo, String> {
    let kind = validate_extension_kind(kind)?;
    let id = validate_extension_id(id)?;
    let metadata = fs::metadata(source).map_err(|error| format!("读取图标文件失败: {error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_ICON_BYTES {
        return Err(format!(
            "请选择不超过 {} KB 的 PNG/WebP 文件",
            MAX_ICON_BYTES / 1024
        ));
    }
    let bytes = fs::read(source).map_err(|error| format!("读取图标文件失败: {error}"))?;
    let icon = cache_icon_bytes(&bytes, "user")?.ok_or_else(|| "无法解析图标文件".to_string())?;
    let extension = detect_extension(&bytes)
        .map(|(extension, _)| extension)
        .unwrap_or("png");
    let mut store = load_override_store();
    store.schema_version = OVERRIDE_SCHEMA_VERSION;
    store.items.insert(
        override_key(kind, id),
        StoredIconOverride {
            sha256: icon.sha256.clone(),
            extension: extension.to_string(),
            updated_at: now_stamp(),
        },
    );
    save_override_store(&store)?;
    Ok(icon)
}

pub(crate) fn clear_user_override(kind: &str, id: &str) -> Result<bool, String> {
    let kind = validate_extension_kind(kind)?;
    let id = validate_extension_id(id)?;
    let mut store = load_override_store();
    let removed = store.items.remove(&override_key(kind, id)).is_some();
    if removed {
        save_override_store(&store)?;
    }
    Ok(removed)
}

/// Remove a local override after the extension itself is gone. Cleanup must not
/// make an otherwise successful uninstall fail, so callers intentionally ignore
/// the result.
pub(crate) fn clear_user_override_after_uninstall(kind: &str, id: &str) {
    let _ = clear_user_override(kind, id);
}

fn valid_cached_icon_name(name: &str) -> bool {
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    matches!(extension, "png" | "webp")
        && stem.len() == 64
        && stem.chars().all(|ch| ch.is_ascii_hexdigit())
}

pub(crate) fn read_cached_icon(name: &str) -> Result<(Vec<u8>, &'static str), String> {
    if !valid_cached_icon_name(name) {
        return Err("图标资源名称无效".into());
    }
    let path = icon_cache_dir().join(name);
    let metadata = fs::metadata(&path).map_err(|_| "图标资源不存在".to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_ICON_BYTES {
        return Err("图标资源无效".into());
    }
    let content_type = if name.ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
    };
    let bytes = fs::read(path).map_err(|_| "图标资源读取失败".to_string())?;
    Ok((bytes, content_type))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let image = ImageBuffer::from_pixel(width, height, Rgba([20u8, 80, 140, 255]));
        let mut cursor = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut cursor, image::ImageFormat::Png)
            .expect("png");
        cursor.into_inner()
    }

    struct HomeGuard {
        _guard: std::sync::MutexGuard<'static, ()>,
        previous: Option<std::ffi::OsString>,
        root: PathBuf,
    }

    impl HomeGuard {
        fn new(label: &str) -> Self {
            let guard = crate::store::paths::test_env_lock();
            let previous = std::env::var_os("HIMIND_AGENT_HOME");
            let root =
                std::env::temp_dir().join(format!("himind-icon-test-{label}-{}", now_stamp()));
            std::fs::create_dir_all(&root).expect("home");
            std::env::set_var("HIMIND_AGENT_HOME", &root);
            Self {
                _guard: guard,
                previous,
                root,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
                None => std::env::remove_var("HIMIND_AGENT_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn rejects_path_traversal_and_unsupported_files() {
        assert!(validate_icon_reference("../icon.png").is_err());
        assert!(validate_icon_reference("C:/icon.png").is_err());
        assert!(validate_icon_reference("assets/icon.svg").is_err());
        assert!(validate_icon_reference("assets/icon.webp").is_ok());
    }

    #[test]
    fn caches_and_resolves_author_icon() {
        let home = HomeGuard::new("author");
        let package = home.root.join("package");
        std::fs::create_dir_all(package.join("assets")).unwrap();
        let bytes = png_bytes(256, 256);
        std::fs::write(package.join("assets/icon.png"), &bytes).unwrap();
        let icon =
            resolve_manifest_icon("plugin", "com.test.icon", &package, Some("assets/icon.png"))
                .unwrap()
                .expect("icon");
        assert_eq!(icon.source, "author");
        assert_eq!(icon.sha256, hex_sha256(&bytes));
        assert!(icon.url.starts_with("http://extension-icon.localhost/"));
    }

    #[test]
    fn user_override_wins_and_can_be_cleared() {
        let home = HomeGuard::new("override");
        let source = home.root.join("picked.png");
        let bytes = png_bytes(128, 128);
        std::fs::write(&source, &bytes).unwrap();
        let icon = set_user_override("skill", "com.test.skill", &source).unwrap();
        assert_eq!(icon.source, "user");
        assert_eq!(
            resolve_user_override("skill", "com.test.skill")
                .unwrap()
                .sha256,
            icon.sha256
        );
        assert!(clear_user_override("skill", "com.test.skill").unwrap());
        assert!(resolve_user_override("skill", "com.test.skill").is_none());
    }

    #[test]
    fn malformed_author_icon_falls_back_without_blocking_manifest() {
        let home = HomeGuard::new("broken");
        let package = home.root.join("package");
        std::fs::create_dir_all(package.join("assets")).unwrap();
        std::fs::write(package.join("assets/icon.png"), b"not a png").unwrap();
        let icon = resolve_manifest_icon(
            "workflow",
            "com.test.workflow",
            &package,
            Some("assets/icon.png"),
        )
        .unwrap();
        assert!(icon.is_none());
    }

    #[test]
    fn rejects_oversized_and_undersized_icons() {
        let _home = HomeGuard::new("limits");
        let oversized = vec![0x89; MAX_ICON_BYTES as usize + 1];
        assert!(cache_icon_bytes(&oversized, "author").is_err());
        let tiny = png_bytes(32, 32);
        assert!(cache_icon_bytes(&tiny, "author").is_err());
        let huge = png_bytes(MAX_ICON_DIMENSION + 1, 128);
        assert!(cache_icon_bytes(&huge, "author").is_err());
    }

    #[test]
    fn remote_icon_requires_origin_and_digest() {
        let _home = HomeGuard::new("remote");
        let artifact = "https://github.com/owner/repo/releases/download/tag/plugin.hmpkg";
        let missing_digest = ExtensionIconInfo {
            url: "https://github.com/owner/repo/releases/download/tag/icon.png".into(),
            sha256: String::new(),
            source: "author".into(),
        };
        assert!(cache_remote_icon(&missing_digest, artifact).is_none());
        let wrong_origin = ExtensionIconInfo {
            url: "https://example.com/icon.png".into(),
            sha256: "a".repeat(64),
            source: "author".into(),
        };
        assert!(cache_remote_icon(&wrong_origin, artifact).is_none());
    }

    #[test]
    fn tampered_override_record_cannot_escape_the_cache() {
        let home = HomeGuard::new("tampered");
        let directory = home.root.join(ICON_CACHE_DIRECTORY);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(ICON_OVERRIDES_FILE),
            r#"{"schema_version":1,"items":{"plugin:com.test.icon":{"sha256":"../escape","extension":"png","updated_at":"0"}}}"#,
        )
        .unwrap();
        assert!(resolve_user_override("plugin", "com.test.icon").is_none());
    }
}
