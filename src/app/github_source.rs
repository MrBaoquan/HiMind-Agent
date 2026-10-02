use reqwest::blocking::Client;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::ZipArchive;

const MAX_GITHUB_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_GITHUB_EXTRACTED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_GITHUB_ARCHIVE_ENTRIES: usize = 100_000;

pub(crate) fn import_plugin(
    repository: &str,
    reference: &str,
    subpath: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let source = resolve_source(repository, reference, subpath)?;
    let root = download_source(&source.repository, &source.reference)?;
    let result = (|| {
        let package = package_root(&root, &source.subpath, "plugin.json")?;
        let identity = package_identity(&package, "plugin.json")?;
        let binding = bind_extension_source(&root, &source, "plugin", &identity.0, &identity.1)?;
        crate::app::plugin_manager::validate_local_plugin_package_dependencies(&package)?;
        crate::app::plugin_manager::install_local_package_from_source(&package, "github")?;
        if let Some((config, catalog)) = binding {
            crate::app::extension_source::upsert_source(config.clone())?;
            save_import_provenance(&config, &catalog, "plugin", &identity.0, &identity.1)?;
        }
        crate::capability::plugin::registry_json().map_err(Into::into)
    })();
    cleanup_source_root(&root);
    result
}

pub(crate) fn import_skill(
    repository: &str,
    reference: &str,
    subpath: &str,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let source = resolve_source(repository, reference, subpath)?;
    let root = download_source(&source.repository, &source.reference)?;
    let result = (|| {
        let package = skill_package_root(&root, &source.subpath)?;
        let identity = skill_package_identity(&package)?;
        let binding = bind_extension_source(&root, &source, "skill", &identity.0, &identity.1)?;
        let record =
            crate::app::skill_manager::install_local_package_from_source(&package, "github")?;
        if let Some((config, catalog)) = binding {
            crate::app::extension_source::upsert_source(config.clone())?;
            save_import_provenance(
                &config,
                &catalog,
                "skill",
                &record.manifest.id,
                &record.manifest.version,
            )?;
        }
        serde_json::to_value(record).map_err(Into::into)
    })();
    cleanup_source_root(&root);
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GithubSourceSpec {
    pub repository: String,
    pub reference: String,
    pub subpath: String,
}

/// Resolve both the legacy three-field form and a UPM-style GitHub URL.
///
/// Examples:
/// - `owner/repo` + `main` + `skills/example`
/// - `https://github.com/owner/repo.git?path=/skills/example#v1.2.0`
fn resolve_source(
    repository: &str,
    reference: &str,
    subpath: &str,
) -> Result<GithubSourceSpec, Box<dyn Error>> {
    let mut source = parse_source_url(repository)?;
    if !reference.trim().is_empty() {
        source.reference = validate_reference(reference)?;
    } else if source.reference.is_empty() {
        source.reference = "main".to_string();
    }
    if !subpath.trim().is_empty() {
        source.subpath = normalize_subpath(subpath)?;
    }
    Ok(source)
}

/// Parse the Git URL shape used by Unity UPM: `?path=/subdirectory#revision`.
/// The plain `owner/repo` form remains valid for callers using the legacy fields.
pub(crate) fn parse_source_url(value: &str) -> Result<GithubSourceSpec, Box<dyn Error>> {
    let value = value.trim();
    if value.is_empty() {
        return Err("GitHub 仓库链接不能为空".into());
    }

    if value.starts_with("https://") || value.starts_with("http://") {
        let url = url::Url::parse(value)?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str() != Some("github.com")
            || url.username() != ""
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err("GitHub 链接必须指向 github.com，且不能包含账号或端口".into());
        }
        let segments = url
            .path_segments()
            .map(|segments| {
                segments
                    .filter(|segment| !segment.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if segments.len() != 2 {
            return Err("GitHub 链接必须是 github.com/owner/repo".into());
        }
        let owner = segments[0];
        let name = segments[1].trim_end_matches(".git");
        validate_repository_segment(owner)?;
        validate_repository_segment(name)?;

        let mut subpath = String::new();
        let mut reference = url.fragment().unwrap_or_default().to_string();
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "path" => {
                    if !subpath.is_empty() {
                        return Err("GitHub 链接不能重复指定 path".into());
                    }
                    subpath = normalize_subpath(&value)?;
                }
                "ref" | "revision" => {
                    if !reference.is_empty() {
                        return Err("GitHub 链接不能同时指定多个版本".into());
                    }
                    reference = value.into_owned();
                }
                _ => return Err("GitHub 链接仅支持 path 和 ref 参数".into()),
            }
        }
        if !reference.is_empty() {
            reference = validate_reference(&reference)?;
        }
        return Ok(GithubSourceSpec {
            repository: format!("{owner}/{name}"),
            reference,
            subpath,
        });
    }

    if value.contains('?') || value.contains('#') {
        return Err("带子目录或版本的 GitHub 链接必须使用 github.com URL".into());
    }
    let (owner, name) = parse_repository_path(value)?;
    Ok(GithubSourceSpec {
        repository: format!("{owner}/{name}"),
        reference: String::new(),
        subpath: String::new(),
    })
}

fn cleanup_source_root(extracted: &Path) {
    if let Some(root) = extracted.parent() {
        let _ = fs::remove_dir_all(root);
    }
}

/// GitHub API 根地址。与发布/安装使用同一套覆盖开关，便于指向企业版或本地验证端点。
fn github_api_base() -> String {
    std::env::var("HIMIND_GITHUB_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.github.com".to_string())
}

/// 归档直链的站点根地址（公开仓库走这里，不带凭据）。
fn github_archive_base() -> String {
    std::env::var("HIMIND_GITHUB_ARCHIVE_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://github.com".to_string())
}

/// 归档下载失败时的可执行提示。私有仓库在未授权时同样返回 404，必须区分说明，
/// 否则用户只会看到「仓库不存在」这种误导性结论。
fn explain_archive_failure(
    response: reqwest::blocking::Response,
    repository: &str,
) -> Result<reqwest::blocking::Response, Box<dyn Error>> {
    if response.status().is_success() {
        return Ok(response);
    }
    Err(archive_failure_message(response.status(), repository).into())
}

/// 归档读取失败的文案映射。独立成纯函数，便于对每种状态逐一验证。
fn archive_failure_message(status: reqwest::StatusCode, repository: &str) -> String {
    if status == reqwest::StatusCode::NOT_FOUND {
        return format!(
            "读取 GitHub 仓库 {repository} 失败（404）：仓库不存在，或它是私有仓库而本机尚未授权。私有仓库请在设置 → 账号中授权 GitHub 账号后重试。"
        );
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return format!(
            "读取 GitHub 仓库 {repository} 失败（{}）：凭据无效或权限不足，请在设置 → 账号中重新授权。",
            status.as_u16()
        );
    }
    format!(
        "读取 GitHub 仓库 {repository} 失败：GitHub 返回 HTTP {}",
        status.as_u16()
    )
}

fn download_source(repository: &str, reference: &str) -> Result<PathBuf, Box<dyn Error>> {
    let (owner, name) = parse_repository(repository)?;
    let reference = validate_reference(reference)?;
    // 私仓要带凭据读取：优先走 GitHub API 的归档接口（支持 Bearer），
    // 公开仓库在未授权时继续用无凭据的 archive 直链，行为不变。
    let token = crate::store::github_credentials::resolve_token()
        .ok()
        .flatten();
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .user_agent("HiMind-Agent")
        .build()?;
    let mut response = match token.as_deref() {
        Some(token) => {
            let url = format!(
                "{}/repos/{owner}/{name}/zipball/{reference}",
                github_api_base()
            );
            let response = client
                .get(url)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .bearer_auth(token)
                .send()?;
            explain_archive_failure(response, repository)?
        }
        None => {
            let url = format!(
                "{}/{owner}/{name}/archive/{reference}.zip",
                github_archive_base()
            );
            let response = client.get(url).send()?;
            explain_archive_failure(response, repository)?
        }
    };
    let root = std::env::temp_dir().join(format!(
        "himind-github-source-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    fs::create_dir_all(&root)?;
    let archive_path = root.join("source.zip");
    let mut file = File::create(&archive_path)?;
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_GITHUB_ARCHIVE_BYTES {
            let _ = fs::remove_dir_all(&root);
            return Err("GitHub 仓库压缩包超过 512 MiB 限制".into());
        }
        file.write_all(&buffer[..count])?;
    }
    file.flush()?;
    let extracted = root.join("extracted");
    if let Err(error) = extract_archive(&archive_path, &extracted) {
        let _ = fs::remove_dir_all(&root);
        return Err(error);
    }
    let _ = fs::remove_file(archive_path);
    Ok(extracted)
}

fn extract_archive(archive_path: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(target)?;
    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    if archive.len() > MAX_GITHUB_ARCHIVE_ENTRIES {
        return Err("GitHub 压缩包文件数量超过 100000 个限制".into());
    }
    let mut extracted_bytes = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        extracted_bytes = extracted_bytes
            .checked_add(entry.size())
            .ok_or("GitHub 压缩包解压大小溢出")?;
        if extracted_bytes > MAX_GITHUB_EXTRACTED_BYTES {
            return Err("GitHub 压缩包解压后超过 512 MiB 限制".into());
        }
        let relative = entry
            .enclosed_name()
            .ok_or("GitHub 压缩包包含非法路径")?
            .to_path_buf();
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

fn package_root(root: &Path, subpath: &str, marker: &str) -> Result<PathBuf, Box<dyn Error>> {
    let relative = subpath.trim().replace('\\', "/");
    let package = if relative.is_empty() {
        find_marker(root, marker)?
            .ok_or_else(|| Box::<dyn Error>::from(format!("GitHub 仓库中未找到 {marker}")))?
    } else {
        validate_subpath(&relative)?;
        let candidate = archive_relative_path(root, &relative)?;
        if !candidate.join(marker).is_file() {
            return Err(format!("GitHub 子目录缺少 {marker}: {relative}").into());
        }
        candidate
    };
    Ok(package)
}

fn skill_package_root(root: &Path, subpath: &str) -> Result<PathBuf, Box<dyn Error>> {
    let relative = subpath.trim().replace('\\', "/");
    if !relative.is_empty() {
        validate_subpath(&relative)?;
        let candidate = archive_relative_path(root, &relative)?;
        if !candidate.join("skill.json").is_file() && !candidate.join("SKILL.md").is_file() {
            return Err(format!("GitHub 子目录缺少 skill.json 或 SKILL.md: {relative}").into());
        }
        return Ok(candidate);
    }
    let mut matches = Vec::new();
    for entry in walkdir::WalkDir::new(root).max_depth(4) {
        let entry = entry?;
        if !entry.file_type().is_dir() {
            continue;
        }
        if entry.path().join("skill.json").is_file() || entry.path().join("SKILL.md").is_file() {
            matches.push(entry.path().to_path_buf());
        }
    }
    matches.sort_by_key(|path| path.components().count());
    match matches.first() {
        Some(path)
            if matches
                .iter()
                .skip(1)
                .all(|candidate| candidate.components().count() > path.components().count()) =>
        {
            Ok(path.clone())
        }
        Some(_) => Err("GitHub 仓库中找到多个同层 Skill 包，请指定子目录".into()),
        None => Err("GitHub 仓库中未找到 skill.json 或 SKILL.md".into()),
    }
}

fn archive_relative_path(root: &Path, relative: &str) -> Result<PathBuf, Box<dyn Error>> {
    let direct = root.join(relative);
    if direct.exists() {
        return Ok(direct);
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let candidate = entry.path().join(relative);
        if candidate.exists() {
            candidates.push(candidate);
        }
    }
    match candidates.len() {
        0 => Ok(direct),
        1 => Ok(candidates.remove(0)),
        _ => Err(format!("GitHub 压缩包包含多个可匹配的顶层目录，无法确定路径: {relative}").into()),
    }
}

fn package_identity(package: &Path, marker: &str) -> Result<(String, String), Box<dyn Error>> {
    let content = fs::read_to_string(package.join(marker))?;
    let value = serde_json::from_str::<serde_json::Value>(content.trim_start_matches('\u{feff}'))?;
    let id = value
        .get("id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("{marker} 缺少 id"))?;
    let version = value
        .get("version")
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("{marker} 缺少 version"))?;
    Ok((id.to_string(), version.to_string()))
}

fn skill_package_identity(package: &Path) -> Result<(String, String), Box<dyn Error>> {
    if package.join("skill.json").is_file() {
        return package_identity(package, "skill.json");
    }
    let readme = fs::read_to_string(package.join("SKILL.md"))?;
    let (name, _, frontmatter_version) = crate::skill::manifest::parse_skill_frontmatter(&readme)?;
    let version = frontmatter_version
        .or_else(|| {
            let content = fs::read_to_string(package.join("skill.yaml")).ok()?;
            let value = serde_yaml::from_str::<serde_yaml::Value>(&content).ok()?;
            value.get("version").and_then(|item| match item {
                serde_yaml::Value::String(value) => Some(value.clone()),
                serde_yaml::Value::Number(value) => Some(value.to_string()),
                _ => None,
            })
        })
        .unwrap_or_else(|| {
            crate::skill::manifest::standard_skill_version(package)
                .unwrap_or_else(|_| "0.0.0".to_string())
        });
    Ok((name, version))
}

fn bind_extension_source(
    root: &Path,
    source: &GithubSourceSpec,
    kind: &str,
    key: &str,
    version: &str,
) -> Result<
    Option<(
        crate::app::extension_source::ExtensionSourceConfig,
        crate::app::extension_source::ExtensionSourceCatalog,
    )>,
    Box<dyn Error>,
> {
    let candidates = [
        if source.subpath.is_empty() {
            String::new()
        } else {
            format!("{}/.himind/catalog.json", source.subpath)
        },
        ".himind/catalog.json".to_string(),
    ];
    for catalog_path in candidates {
        if catalog_path.is_empty() {
            continue;
        }
        let path = archive_relative_path(root, &catalog_path)?;
        if !path.is_file() {
            continue;
        }
        let catalog = serde_json::from_slice::<crate::app::extension_source::ExtensionSourceCatalog>(
            &fs::read(&path)?,
        )?;
        let config = crate::app::extension_source::github_source_config(
            &format!("GitHub: {}", source.repository),
            &source.repository,
            &source.reference,
            Some(&catalog_path),
            None,
        )?;
        crate::app::extension_source::validate_catalog(&catalog, &config)?;
        let listed = match kind {
            "plugin" => catalog
                .plugins
                .iter()
                .any(|item| item.plugin_id == key && item.version == version),
            "skill" => catalog
                .skills
                .iter()
                .any(|item| item.skill_id == key && item.version == version),
            _ => return Err("扩展类型无效".into()),
        };
        if !listed {
            return Err(format!(
                "GitHub 扩展目录未声明 {kind} {key} v{version}，无法建立自动更新来源"
            )
            .into());
        }
        return Ok(Some((config, catalog)));
    }
    Ok(None)
}

fn save_import_provenance(
    source: &crate::app::extension_source::ExtensionSourceConfig,
    catalog: &crate::app::extension_source::ExtensionSourceCatalog,
    kind: &str,
    key: &str,
    version: &str,
) -> Result<(), Box<dyn Error>> {
    if kind == "plugin" {
        if let Some(item) = catalog
            .plugins
            .iter()
            .find(|item| item.plugin_id == key && item.version == version)
        {
            crate::app::extension_source::save_provenance(
                source,
                kind,
                key,
                version,
                &item.download_url,
                &item.sha256,
                &item.signature_key_id,
            )?;
        } else {
            return Err(format!(
                "GitHub 扩展目录未声明插件 {} v{}，无法记录更新来源",
                key, version
            )
            .into());
        }
    } else if kind == "skill" {
        if let Some(item) = catalog
            .skills
            .iter()
            .find(|item| item.skill_id == key && item.version == version)
        {
            crate::app::extension_source::save_provenance(
                source,
                kind,
                key,
                version,
                &item.download_url,
                &item.sha256,
                &item.signature_key_id,
            )?;
        } else {
            return Err(format!(
                "GitHub 扩展目录未声明技能 {} v{}，无法记录更新来源",
                key, version
            )
            .into());
        }
    } else {
        return Err("扩展类型无效".into());
    }
    Ok(())
}

fn find_marker(root: &Path, marker: &str) -> Result<Option<PathBuf>, Box<dyn Error>> {
    let mut matches = Vec::new();
    for entry in walkdir::WalkDir::new(root).max_depth(3) {
        let entry = entry?;
        if entry.file_type().is_file() && entry.file_name().to_string_lossy() == marker {
            matches.push(entry.path().parent().unwrap_or(root).to_path_buf());
        }
    }
    if matches.len() > 1 {
        return Err(format!("GitHub 仓库中找到多个 {marker}，请指定子目录").into());
    }
    Ok(matches.pop())
}

fn parse_repository(value: &str) -> Result<(String, String), Box<dyn Error>> {
    let source = parse_source_url(value)?;
    let mut parts = source.repository.split('/');
    Ok((
        parts.next().unwrap_or_default().to_string(),
        parts.next().unwrap_or_default().to_string(),
    ))
}

fn parse_repository_path(value: &str) -> Result<(String, String), Box<dyn Error>> {
    let value = value.trim().trim_end_matches('/').trim_end_matches(".git");
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if parts.next().is_some() || owner.is_empty() || name.is_empty() {
        return Err("GitHub 仓库必须是 owner/repo 或 github.com/owner/repo".into());
    }
    validate_repository_segment(owner)?;
    validate_repository_segment(name)?;
    Ok((owner.to_string(), name.to_string()))
}

fn validate_repository_segment(value: &str) -> Result<(), Box<dyn Error>> {
    if value.is_empty() || !value.bytes().all(is_github_segment_byte) {
        return Err("GitHub 仓库名称包含无效字符".into());
    }
    Ok(())
}

fn validate_reference(value: &str) -> Result<String, Box<dyn Error>> {
    let value = value.trim();
    if value.is_empty()
        || value.starts_with('-')
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'.'))
    {
        return Err("GitHub ref 必须是固定 tag、branch 或 commit，且不能包含路径穿越".into());
    }
    Ok(value.to_string())
}

fn validate_subpath(value: &str) -> Result<(), Box<dyn Error>> {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("GitHub 子目录路径无效".into());
    }
    Ok(())
}

fn normalize_subpath(value: &str) -> Result<String, Box<dyn Error>> {
    let normalized = value.trim().replace('\\', "/");
    let normalized = normalized.trim_matches('/').to_string();
    if normalized.is_empty() {
        return Ok(String::new());
    }
    validate_subpath(&normalized)?;
    Ok(normalized)
}

fn is_github_segment_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

#[cfg(test)]
mod tests {
    use super::{
        archive_failure_message, archive_relative_path, parse_repository, parse_source_url,
        validate_reference, validate_subpath,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn accepts_repository_forms_and_rejects_cross_host_urls() {
        assert_eq!(
            parse_repository("owner/repo").unwrap(),
            ("owner".to_string(), "repo".to_string())
        );
        assert_eq!(
            parse_repository("https://github.com/owner/repo.git").unwrap(),
            ("owner".to_string(), "repo".to_string())
        );
        assert!(parse_repository("https://evil.example/owner/repo").is_err());
        assert!(parse_repository("owner/repo/extra").is_err());
    }

    #[test]
    fn parses_upm_style_repository_urls() {
        let source =
            parse_source_url("https://github.com/Owner/repo.git?path=/skills/example#v1.2.3")
                .unwrap();
        assert_eq!(source.repository, "Owner/repo");
        assert_eq!(source.reference, "v1.2.3");
        assert_eq!(source.subpath, "skills/example");

        let source =
            parse_source_url("https://github.com/Owner/repo?path=plugins%2Fdemo&ref=main").unwrap();
        assert_eq!(source.reference, "main");
        assert_eq!(source.subpath, "plugins/demo");
    }

    #[test]
    fn rejects_unsafe_or_ambiguous_repository_urls() {
        assert!(parse_source_url("https://evil.example/owner/repo?path=plugins").is_err());
        assert!(parse_source_url("https://github.com/owner/repo?path=../outside").is_err());
        assert!(parse_source_url("https://github.com/owner/repo#../main").is_err());
        assert!(parse_source_url("https://github.com/owner/repo?ref=main#v1.0.0").is_err());
    }

    #[test]
    fn refs_and_subpaths_are_path_safe() {
        assert!(validate_reference("v1.2.3").is_ok());
        assert!(validate_reference("main").is_ok());
        assert!(validate_reference("../main").is_err());
        assert!(validate_subpath("skills/example").is_ok());
        assert!(validate_subpath("../outside").is_err());
    }

    #[test]
    fn archive_paths_support_github_top_level_directory_without_guessing() {
        let root = std::env::temp_dir().join(format!(
            "himind-github-path-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("repo-main").join("skills/demo")).unwrap();
        assert_eq!(
            archive_relative_path(&root, "skills/demo").unwrap(),
            root.join("repo-main/skills/demo")
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn archive_paths_reject_multiple_matching_top_level_directories() {
        let root = std::env::temp_dir().join(format!(
            "himind-github-ambiguous-path-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for directory in ["repo-main", "repo-extra"] {
            fs::create_dir_all(root.join(directory).join("skills/demo")).unwrap();
        }
        assert!(archive_relative_path(&root, "skills/demo").is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn archive_failures_explain_private_repository_authorization() {
        // 私仓未授权时 GitHub 也返回 404，文案必须同时给出「不存在」与「未授权」两种可能。
        let not_found =
            archive_failure_message(reqwest::StatusCode::NOT_FOUND, "owner/private-repo");
        assert!(not_found.contains("404"));
        assert!(not_found.contains("私有仓库"));
        assert!(not_found.contains("授权"));
        let unauthorized = archive_failure_message(reqwest::StatusCode::FORBIDDEN, "owner/repo");
        assert!(unauthorized.contains("403"));
        assert!(unauthorized.contains("重新授权"));
        let server_error =
            archive_failure_message(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "owner/repo");
        assert!(server_error.contains("500"));
    }
}
