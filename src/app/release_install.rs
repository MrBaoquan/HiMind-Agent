//! 从 GitHub Release 清单安装扩展。
//!
//! 消费侧只认发布清单：清单给出制品的精确摘要与依赖 pin，安装按拓扑序先装依赖再装
//! 本体，任一环节失败整体回滚，不留半装状态。清单缺失或摘要不匹配一律拒绝安装，
//! 不做「尽力而为」的降级。

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::path::{Path, PathBuf};

use crate::app::github_publisher;

/// 递归解析依赖时允许的最大深度，避免异常清单把本机拖进无界请求。
const MAX_DEPENDENCY_DEPTH: usize = 8;
/// 单个 Release 资产的下载上限，与分发侧制品上限保持一致。
const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ReleaseArtifact {
    pub name: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub sha256: String,
}

/// 发布清单里的分离签名。有该字段就必须验签通过，否则整个安装失败；
/// 字段缺失是否可接受由分发策略决定（见 `system::signed_extension_releases_required`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct ReleaseSignature {
    #[serde(default)]
    pub file_name: String,
    #[serde(default)]
    pub file_size: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub signature_key_id: String,
    #[serde(default)]
    pub signature_algorithm: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct ReleaseDependencySource {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub reference: String,
    #[serde(default)]
    pub artifact_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ReleaseDependency {
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub min_version: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub source: ReleaseDependencySource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ReleaseManifest {
    pub schema_version: String,
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub tag: String,
    pub kind: String,
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub source_commit: String,
    #[serde(default)]
    pub min_agent_version: String,
    pub artifact: ReleaseArtifact,
    #[serde(default)]
    pub dependencies: Vec<ReleaseDependency>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<ReleaseSignature>,
}

impl ReleaseManifest {
    pub(crate) fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.schema_version != github_publisher::RELEASE_MANIFEST_SCHEMA {
            return Err(format!(
                "不支持的发布清单版本: {}（需要 {}）",
                self.schema_version,
                github_publisher::RELEASE_MANIFEST_SCHEMA
            )
            .into());
        }
        github_publisher::asset_extension(&self.kind)?;
        if self.id.trim().is_empty() || self.version.trim().is_empty() {
            return Err("发布清单缺少 id 或 version".into());
        }
        if self.artifact.name.trim().is_empty() {
            return Err("发布清单缺少制品名".into());
        }
        if !self.artifact.sha256.trim().is_empty() {
            crate::extension_contracts::validate_sha256("artifact sha256", &self.artifact.sha256)?;
        }
        validate_manifest_signature(
            &self.id,
            &self.artifact,
            self.signature.as_ref(),
            crate::app::system::signed_extension_releases_required(),
        )?;
        Ok(())
    }
}

/// 清单级签名检查：结构、与制品的绑定关系，以及「无签名时是否放行」的策略。
/// 密码学校验在做完摘要比对后、安装之前进行（见 `verify_staged_signature`）。
fn validate_manifest_signature(
    id: &str,
    artifact: &ReleaseArtifact,
    signature: Option<&ReleaseSignature>,
    require_signed: bool,
) -> Result<(), Box<dyn Error>> {
    let Some(signature) = signature else {
        return if require_signed {
            Err(crate::app::system::unsigned_extension_release_error(id).into())
        } else {
            Ok(())
        };
    };
    crate::app::system::validate_signature_metadata(
        signature.signature.trim(),
        signature.signature_key_id.trim(),
        signature.signature_algorithm.trim(),
        true,
    )?;
    if !signature.sha256.trim().is_empty() {
        crate::extension_contracts::validate_sha256("signature sha256", &signature.sha256)?;
        if !artifact.sha256.trim().is_empty()
            && !signature
                .sha256
                .trim()
                .eq_ignore_ascii_case(artifact.sha256.trim())
        {
            return Err(format!(
                "发布清单的签名摘要与制品摘要不一致：签名 {}，制品 {}",
                signature.sha256.trim(),
                artifact.sha256.trim()
            )
            .into());
        }
    }
    if !signature.file_name.trim().is_empty() && signature.file_name.trim() != artifact.name.trim()
    {
        return Err(format!(
            "发布清单的签名对象与制品名不一致：签名 {}，制品 {}",
            signature.file_name.trim(),
            artifact.name.trim()
        )
        .into());
    }
    Ok(())
}

/// 安装计划中的一个节点，按依赖优先排序。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct InstallNode {
    pub kind: String,
    pub id: String,
    pub version: String,
    pub repository: String,
    pub tag: String,
    pub artifact_name: String,
    pub sha256: String,
    pub size_bytes: u64,
    /// 该制品是否带签名，以及签名用的 key ID。安装前只做展示，真正的验签在下载后。
    pub signed: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub signature_key_id: String,
    /// 签名原文只在进程内使用，不下发到计划 JSON。
    #[serde(skip)]
    pub signature: Option<ReleaseSignature>,
    pub required: bool,
    pub root: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InstallPlan {
    pub root: InstallNode,
    /// 需要在根之前安装的依赖，按依赖优先的拓扑序。
    pub dependencies: Vec<InstallNode>,
    /// 已在本机同版本安装、本次会跳过的节点。
    pub already_installed: Vec<String>,
    /// 未 pin 的依赖（安装前要求本机已经具备）。
    #[serde(default)]
    pub unpinned: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InstallReport {
    pub root: String,
    pub installed: Vec<String>,
    pub skipped: Vec<String>,
    pub rolled_back: Vec<String>,
    #[serde(default)]
    pub errors: Vec<String>,
    pub state: String,
}

fn client_token() -> Option<String> {
    crate::store::github_credentials::resolve_token()
        .ok()
        .flatten()
}

fn manifest_asset_name(id: &str, version: &str) -> String {
    github_publisher::manifest_name(id, version)
}

/// 读取并校验发布清单。
pub(crate) fn load_manifest(
    repository: &str,
    tag: &str,
    id: &str,
    version: &str,
) -> Result<ReleaseManifest, Box<dyn Error>> {
    let token = client_token();
    let release = github_publisher::release_for_tag(token.as_deref(), repository, tag)?
        .ok_or_else(|| format!("未找到 {repository} 上 tag {tag} 的 Release"))?;
    let wanted = manifest_asset_name(id, version);
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == wanted)
        .ok_or_else(|| format!("Release {tag} 缺少发布清单 {wanted}"))?;
    if asset.size > MAX_ARTIFACT_BYTES {
        return Err("发布清单超过大小限制".into());
    }
    let bytes = github_publisher::download_asset_verified(
        token.as_deref(),
        &asset.browser_download_url,
        0,
        "",
    )?;
    let manifest: ReleaseManifest = serde_json::from_slice(&bytes)?;
    manifest.validate()?;
    if manifest.id != id || manifest.version != version {
        return Err(format!(
            "发布清单与请求的制品不一致：清单是 {}@{}",
            manifest.id, manifest.version
        )
        .into());
    }
    Ok(manifest)
}

/// 解析依赖并生成拓扑序计划。依赖自身的清单必须同样可读，否则计划不成立。
pub(crate) fn plan(
    repository: &str,
    tag: &str,
    id: &str,
    version: &str,
) -> Result<InstallPlan, Box<dyn Error>> {
    let root_manifest = load_manifest(repository, tag, id, version)?;
    let root_node = node_of(&root_manifest, repository, tag, true, true);
    let mut dependencies = Vec::new();
    let mut unpinned = Vec::new();
    let mut already_installed = Vec::new();
    let mut visited = vec![format!("{}:{}", root_manifest.kind, root_manifest.id)];
    collect_dependencies(
        &root_manifest,
        &mut visited,
        1,
        &mut dependencies,
        &mut unpinned,
        &mut already_installed,
    )?;
    Ok(InstallPlan {
        root: root_node,
        dependencies,
        already_installed,
        unpinned,
    })
}

fn node_of(
    manifest: &ReleaseManifest,
    repository: &str,
    tag: &str,
    required: bool,
    root: bool,
) -> InstallNode {
    InstallNode {
        kind: manifest.kind.clone(),
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        repository: if manifest.repository.trim().is_empty() {
            repository.to_string()
        } else {
            manifest.repository.clone()
        },
        tag: if manifest.tag.trim().is_empty() {
            tag.to_string()
        } else {
            manifest.tag.clone()
        },
        artifact_name: manifest.artifact.name.clone(),
        sha256: manifest.artifact.sha256.clone(),
        size_bytes: manifest.artifact.size_bytes,
        signed: manifest.signature.is_some(),
        signature_key_id: manifest
            .signature
            .as_ref()
            .map(|signature| signature.signature_key_id.trim().to_string())
            .unwrap_or_default(),
        signature: manifest.signature.clone(),
        required,
        root,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_dependencies(
    manifest: &ReleaseManifest,
    visited: &mut Vec<String>,
    depth: usize,
    ordered: &mut Vec<InstallNode>,
    unpinned: &mut Vec<String>,
    already_installed: &mut Vec<String>,
) -> Result<(), Box<dyn Error>> {
    if depth > MAX_DEPENDENCY_DEPTH {
        return Err(format!(
            "依赖层级超过 {MAX_DEPENDENCY_DEPTH} 层，已停止解析。请检查依赖是否形成环。"
        )
        .into());
    }
    for dependency in &manifest.dependencies {
        if !dependency.required {
            continue;
        }
        let key = format!("{}:{}", dependency.kind, dependency.id);
        if visited.contains(&key) {
            return Err(format!("依赖存在循环：{} 已在本次解析路径中出现", dependency.id).into());
        }
        if dependency.source.repository.trim().is_empty()
            || dependency.source.reference.trim().is_empty()
            || dependency.version.trim().is_empty()
        {
            // 未 pin 的依赖无法从远端定位，必须已经在本机可用。
            // 这里无法预知本机装的具体版本，只要求「本地存在该依赖」。
            unpinned.push(dependency.id.clone());
            if !has_local_install(&dependency.kind, &dependency.id) {
                return Err(format!(
                    "必需依赖 {} 未在清单里 pin，且本机没有可用版本；请先安装该依赖，或在发布侧补全依赖来源后重新发布。",
                    dependency.id
                )
                .into());
            }
            already_installed.push(dependency.id.clone());
            continue;
        }
        let dependency_manifest = load_manifest(
            &dependency.source.repository,
            &dependency.source.reference,
            &dependency.id,
            &dependency.version,
        )?;
        if dependency.sha256.trim().is_empty()
            || dependency
                .sha256
                .eq_ignore_ascii_case(&dependency_manifest.artifact.sha256)
        {
            visited.push(key);
            collect_dependencies(
                &dependency_manifest,
                visited,
                depth + 1,
                ordered,
                unpinned,
                already_installed,
            )?;
            visited.pop();
        } else {
            return Err(format!(
                "依赖 {} 的清单摘要与发布侧 pin 不一致：pin {}，清单 {}",
                dependency.id, dependency.sha256, dependency_manifest.artifact.sha256
            )
            .into());
        }
        if is_installed(&dependency.kind, &dependency.id, &dependency.version) {
            already_installed.push(dependency.id.clone());
            continue;
        }
        ordered.push(node_of(
            &dependency_manifest,
            &dependency.source.repository,
            &dependency.source.reference,
            true,
            false,
        ));
    }
    Ok(())
}

/// 判断「该 kind 的这个 id 是否恰好装着这个版本」。
///
/// 台账（extension lock）优先；台账缺失时才回退到各自的安装目录扫描，
/// 并且回退分支同样要比对版本——否则已装旧版本会被当成新版本已就绪，
/// 升级安装会被静默跳过。
fn is_installed(kind: &str, id: &str, version: &str) -> bool {
    if let Ok(Some(entry)) = crate::app::extension_lock::read(kind, id) {
        if entry.version == version {
            return true;
        }
    }
    match kind {
        "plugin" => matches!(
            crate::capability::plugin::find_plugin(id),
            Ok(Some(item)) if item.version == version
        ),
        "skill" => matches!(
            crate::skill::store::SkillStore::new().get_record(id),
            Ok(Some(record)) if record.manifest.version == version
        ),
        "workflow" => crate::workflow::WorkflowStore::open_default()
            .and_then(|store| store.list())
            .map(|items| {
                items
                    .iter()
                    .any(|item| item.package.id == id && item.package.version == version)
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// 判断「该 kind 的这个 id 在本机是否装过任意版本」。
///
/// 仅用于无法 pin 来源的依赖检查：此处无法预知具体版本，只要本地有就能跑。
fn has_local_install(kind: &str, id: &str) -> bool {
    match kind {
        "plugin" => matches!(crate::capability::plugin::find_plugin(id), Ok(Some(_))),
        "skill" => matches!(
            crate::skill::store::SkillStore::new().get_record(id),
            Ok(Some(_))
        ),
        "workflow" => crate::workflow::WorkflowStore::open_default()
            .and_then(|store| store.list())
            .map(|items| items.iter().any(|item| item.package.id == id))
            .unwrap_or(false),
        _ => false,
    }
}

/// 执行安装。`dry_run` 只下载与校验制品，不写入安装目录，也不改动锁。
pub(crate) fn install(plan: &InstallPlan, dry_run: bool) -> Result<InstallReport, Box<dyn Error>> {
    let token = client_token();
    let mut installed = Vec::new();
    let mut skipped = plan.already_installed.clone();
    let mut rollback: Vec<(String, String)> = Vec::new();
    let mut report = InstallReport {
        root: format!("{}@{}", plan.root.id, plan.root.version),
        installed: Vec::new(),
        skipped: skipped.clone(),
        rolled_back: Vec::new(),
        errors: Vec::new(),
        state: "ready".to_string(),
    };
    let mut ordered = plan.dependencies.clone();
    ordered.push(plan.root.clone());
    for node in &ordered {
        let staged = match stage_artifact(token.as_deref(), node) {
            Ok(value) => value,
            Err(error) => {
                rollback_installed(&rollback)?;
                report.rolled_back = rollback.iter().map(|(id, _)| id.clone()).collect();
                report.state = "failed".to_string();
                report.errors.push(safe_error(&error.to_string()));
                return Ok(report);
            }
        };
        if dry_run {
            let _ = std::fs::remove_file(&staged);
            installed.push(format!("{}@{}", node.id, node.version));
            continue;
        }
        if is_installed(&node.kind, &node.id, &node.version) {
            let _ = std::fs::remove_file(&staged);
            if !skipped.contains(&node.id) {
                skipped.push(node.id.clone());
            }
            continue;
        }
        match install_staged(&node.kind, &staged) {
            Ok(()) => {
                // 安装成功后把 Release 来源写回台账，下游才能据此生成精确 pin。
                if let Err(error) = crate::app::extension_lock::record_release_install(
                    &node.kind,
                    &node.id,
                    &node.version,
                    &node.sha256,
                    &node.repository,
                    &node.tag,
                    &format!(
                        "https://github.com/{}/releases/tag/{}",
                        node.repository.trim().trim_end_matches('/'),
                        node.tag
                    ),
                    Vec::new(),
                ) {
                    let _ = std::fs::remove_file(&staged);
                    rollback_installed(&rollback)?;
                    report.rolled_back = rollback.iter().map(|(id, _)| id.clone()).collect();
                    report.state = "failed".to_string();
                    report.errors.push(safe_error(&error.to_string()));
                    return Ok(report);
                }
                installed.push(format!("{}@{}", node.id, node.version));
                rollback.push((node.id.clone(), node.kind.clone()));
            }
            Err(error) => {
                let _ = std::fs::remove_file(&staged);
                rollback_installed(&rollback)?;
                report.rolled_back = rollback.iter().map(|(id, _)| id.clone()).collect();
                report.state = "failed".to_string();
                report.errors.push(safe_error(&error.to_string()));
                return Ok(report);
            }
        }
        let _ = std::fs::remove_file(&staged);
    }
    report.installed = installed;
    report.skipped = skipped;
    Ok(report)
}

fn stage_artifact(token: Option<&str>, node: &InstallNode) -> Result<PathBuf, Box<dyn Error>> {
    let release = github_publisher::release_for_tag(token, &node.repository, &node.tag)?
        .ok_or_else(|| format!("未找到 {} 上 tag {} 的 Release", node.repository, node.tag))?;
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == node.artifact_name)
        .ok_or_else(|| format!("Release {} 缺少制品 {}", node.tag, node.artifact_name))?;
    if node.size_bytes != 0 && asset.size != 0 && asset.size != node.size_bytes {
        return Err(format!(
            "{} 的 Release 资产大小与清单不一致：清单 {} 字节，Release {} 字节",
            node.id, node.size_bytes, asset.size
        )
        .into());
    }
    let bytes = github_publisher::download_asset_verified(
        token,
        &asset.browser_download_url,
        node.size_bytes,
        &node.sha256,
    )?;
    let extension = github_publisher::asset_extension(&node.kind)?;
    let path = std::env::temp_dir().join(format!(
        "himind-release-{}-{}.{}",
        sanitize(&node.id),
        sanitize(&node.version),
        extension
    ));
    std::fs::write(&path, bytes)?;
    if let Err(error) = verify_staged_signature(node, &path) {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

/// 摘要校验通过后仍然要验签：清单里的签名是「这份制品由持有该私钥的人发布」的证据。
/// 没有签名时是否放行由分发策略决定，策略拒绝时给出可执行的补救办法。
fn verify_staged_signature(node: &InstallNode, path: &Path) -> Result<(), Box<dyn Error>> {
    match &node.signature {
        Some(signature) => crate::app::system::verify_extension_artifact_signature(
            path,
            signature.signature.trim(),
            signature.signature_key_id.trim(),
            signature.signature_algorithm.trim(),
            true,
        )
        .map_err(|error| format!("{} 的制品验签失败：{error}", node.id).into()),
        None if crate::app::system::signed_extension_releases_required() => {
            Err(crate::app::system::unsigned_extension_release_error(&node.id).into())
        }
        None => Ok(()),
    }
}

fn install_staged(kind: &str, path: &Path) -> Result<(), Box<dyn Error>> {
    match kind {
        "plugin" => crate::app::plugin_manager::install_local_package_from_source(path, "github"),
        "skill" => {
            crate::app::skill_manager::install_local_package_from_source(path, "github").map(|_| ())
        }
        "workflow" => crate::app::workflow_manager::install_local_archive(path, false).map(|_| ()),
        other => Err(format!("不支持从 Release 安装的类型: {other}").into()),
    }
}

fn rollback_installed(rollback: &[(String, String)]) -> Result<(), Box<dyn Error>> {
    for (id, kind) in rollback.iter().rev() {
        match kind.as_str() {
            "plugin" => {
                let _ = crate::app::plugin_manager::uninstall(id);
            }
            "skill" => {
                let _ = crate::skill::store::SkillStore::new().remove_installed_skill(id);
            }
            "workflow" => {
                let _ = crate::workflow::WorkflowStore::open_default()
                    .and_then(|store| store.remove(id));
            }
            _ => {}
        }
    }
    Ok(())
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn safe_error(message: &str) -> String {
    message.trim().chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(id: &str, version: &str, dependencies: Vec<ReleaseDependency>) -> ReleaseManifest {
        ReleaseManifest {
            schema_version: github_publisher::RELEASE_MANIFEST_SCHEMA.to_string(),
            repository: "owner/repo".to_string(),
            tag: format!("plugin/{id}@{version}"),
            kind: "plugin".to_string(),
            id: id.to_string(),
            version: version.to_string(),
            channel: "stable".to_string(),
            source_commit: "deadbeef".to_string(),
            min_agent_version: String::new(),
            artifact: ReleaseArtifact {
                name: format!("{id}-{version}.hmpkg"),
                size_bytes: 10,
                sha256: "a".repeat(64),
            },
            dependencies,
            signature: Some(signature_of(id, version)),
        }
    }

    fn signature_of(id: &str, version: &str) -> ReleaseSignature {
        ReleaseSignature {
            file_name: format!("{id}-{version}.hmpkg"),
            file_size: 10,
            sha256: "a".repeat(64),
            signature: "c2lnbmF0dXJl".to_string(),
            signature_key_id: "himind-test".to_string(),
            signature_algorithm: "rsa-pss-sha256".to_string(),
        }
    }

    #[test]
    fn manifest_validation_rejects_wrong_schema_and_bad_digest() {
        let mut value = manifest("com.himind.x", "1.0.0", Vec::new());
        assert!(value.validate().is_ok());
        value.schema_version = "himind_extension_release.v2".to_string();
        assert!(value.validate().is_err());
        let mut value = manifest("com.himind.x", "1.0.0", Vec::new());
        value.artifact.sha256 = "not-a-digest".to_string();
        assert!(value.validate().is_err());
        let mut value = manifest("com.himind.x", "1.0.0", Vec::new());
        value.kind = "extension".to_string();
        assert!(value.validate().is_err());
    }

    #[test]
    fn manifest_parses_dependency_pins_from_release_document() {
        let document = json!({
            "schema_version": github_publisher::RELEASE_MANIFEST_SCHEMA,
            "repository": "owner/repo",
            "tag": "plugin/com.himind.x@1.0.0",
            "kind": "plugin",
            "id": "com.himind.x",
            "version": "1.0.0",
            "artifact": { "name": "com.himind.x-1.0.0.hmpkg", "size_bytes": 12, "sha256": "b".repeat(64) },
            "signature": {
                "file_name": "com.himind.x-1.0.0.hmpkg",
                "file_size": 12,
                "sha256": "b".repeat(64),
                "signature": "c2lnbmF0dXJl",
                "signature_key_id": "himind-test",
                "signature_algorithm": "rsa-pss-sha256"
            },
            "dependencies": [{
                "kind": "plugin",
                "id": "com.himind.y",
                "required": true,
                "min_version": "1.0.0",
                "version": "1.2.0",
                "sha256": "c".repeat(64),
                "pinned": true,
                "source": { "kind": "github", "id": "src-1", "repository": "owner/repo", "reference": "plugin/com.himind.y@1.2.0", "artifact_url": "" }
            }]
        });
        let parsed: ReleaseManifest = serde_json::from_value(document).unwrap();
        parsed.validate().unwrap();
        assert_eq!(parsed.dependencies.len(), 1);
        let dependency = &parsed.dependencies[0];
        assert!(dependency.pinned);
        assert_eq!(dependency.version, "1.2.0");
        assert_eq!(dependency.source.reference, "plugin/com.himind.y@1.2.0");
    }

    #[test]
    fn stage_path_sanitizes_identifiers() {
        assert_eq!(sanitize("com.himind.x@1.0.0"), "com.himind.x_1.0.0");
        assert_eq!(sanitize("a/b\\c"), "a_b_c");
    }

    fn artifact() -> ReleaseArtifact {
        ReleaseArtifact {
            name: "com.himind.x-1.0.0.hmpkg".to_string(),
            size_bytes: 10,
            sha256: "a".repeat(64),
        }
    }

    #[test]
    fn signature_is_required_only_when_the_policy_says_so() {
        let artifact = artifact();
        let error = validate_manifest_signature("com.himind.x", &artifact, None, true)
            .expect_err("未签名制品在要求签名的策略下必须被拒绝");
        let message = error.to_string();
        assert!(message.contains("HIMIND_EXTENSION_SIGNING_PRIVATE_KEY_PATH"));
        assert!(message.contains("HIMIND_REQUIRE_SIGNED_EXTENSIONS=false"));
        assert!(validate_manifest_signature("com.himind.x", &artifact, None, false).is_ok());
    }

    #[test]
    fn signature_must_be_complete_and_bound_to_the_artifact() {
        let artifact = artifact();
        let signature = signature_of("com.himind.x", "1.0.0");
        assert!(
            validate_manifest_signature("com.himind.x", &artifact, Some(&signature), true).is_ok()
        );

        let mut incomplete = signature.clone();
        incomplete.signature_key_id.clear();
        assert!(
            validate_manifest_signature("com.himind.x", &artifact, Some(&incomplete), true)
                .is_err()
        );

        let mut wrong_algorithm = signature.clone();
        wrong_algorithm.signature_algorithm = "rsa-pkcs1-sha256".to_string();
        assert!(validate_manifest_signature(
            "com.himind.x",
            &artifact,
            Some(&wrong_algorithm),
            true
        )
        .is_err());

        let mut other_artifact = signature.clone();
        other_artifact.sha256 = "b".repeat(64);
        assert!(validate_manifest_signature(
            "com.himind.x",
            &artifact,
            Some(&other_artifact),
            true
        )
        .is_err());

        let mut other_name = signature.clone();
        other_name.file_name = "com.himind.y-1.0.0.hmpkg".to_string();
        assert!(
            validate_manifest_signature("com.himind.x", &artifact, Some(&other_name), true)
                .is_err()
        );
    }

    #[test]
    fn signature_serialization_round_trips_through_the_release_document() {
        let mut value = manifest("com.himind.x", "1.0.0", Vec::new());
        value.signature = None;
        let document = serde_json::to_value(&value).unwrap();
        assert!(document.get("signature").is_none());
        let parsed: ReleaseManifest = serde_json::from_value(document).unwrap();
        assert!(parsed.signature.is_none());

        let value = manifest("com.himind.x", "1.0.0", Vec::new());
        let document = serde_json::to_value(&value).unwrap();
        let parsed: ReleaseManifest = serde_json::from_value(document).unwrap();
        assert_eq!(parsed.signature.unwrap().signature_key_id, "himind-test");
    }
}
