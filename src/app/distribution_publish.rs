//! 扩展分发编排。
//!
//! 把「目标约束 → 制品解析 → 各落点投递 → 台账记账」收在一处：UI、CLI、MCP
//! 三条入口调用同一实现，避免各自拼装发布顺序。固定顺序是先 GitHub 后工作台，
//! 工作台提审带入 GitHub 的 tag 与摘要作为溯源；任一步失败进入部分完成状态。

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::path::PathBuf;

use crate::app::distribution_state::{
    self, DistributionStateEntry, STATUS_FAILED, STATUS_PENDING, STATUS_PUBLISHED,
};
use crate::app::extension_signing;
use crate::app::github_publisher::{self, GithubArtifact, GithubPublishPlan};
use crate::extension_contracts::DistributionTarget;
use crate::extension_projects::{self, ExtensionProjectKind};

/// 一次发布要用到的制品事实。所有字段都来自已确认候选，不做二次推断。
#[derive(Debug, Clone)]
pub(crate) struct DistributionAsset {
    pub kind: String,
    pub id: String,
    pub version: String,
    pub name: String,
    pub release_notes: String,
    pub min_agent_version: String,
    pub package_path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
    /// 依赖的精确 pin：`version` + `sha256` + 来源定位。
    pub dependencies: Value,
    /// 依赖锁定情况摘要，用于报告与 UI 说明「几项已 pin、几项没有」。
    pub dependency_summary: Value,
    /// 必需依赖无法解析时的阻断原因；非空时不允许发布到 GitHub。
    pub dependency_blocker: Option<String>,
}

/// 声明层依赖：来自 Plugin/Skill 的 `plugin_dependencies` 或 Workflow 的依赖锁。
#[derive(Debug, Clone)]
struct DeclaredDependency {
    kind: String,
    id: String,
    required: bool,
    /// 声明的最低版本；Workflow 锁里是精确版本。
    min_version: String,
    /// 声明层已有的摘要（Workflow 锁会带）。
    sha256: String,
}

/// 依赖 pin 结果。`blocker` 非空时不允许发布到 GitHub——清单里没有任何可解析的
/// 定位信息，消费侧无法据此取到依赖。
#[derive(Debug, Clone, Default)]
struct PinnedDependencies {
    items: Vec<Value>,
    pinned: usize,
    unpinned: Vec<String>,
    blocker: Option<String>,
}

/// 把声明层依赖解析成精确 pin。
///
/// 优先级：已安装台账（`extension.lock`，含版本、摘要与来源）→ 声明层自带的摘要
/// （Workflow 锁）→ 只有最低版本（标记 `pinned: false`）。必需依赖连版本都无法
/// 确定时视为不可解析，直接阻断发布。
fn pin_dependencies(declared: &[DeclaredDependency]) -> PinnedDependencies {
    let mut result = PinnedDependencies::default();
    let mut unresolved = Vec::new();
    for dependency in declared {
        let lock = crate::app::extension_lock::read(&dependency.kind, &dependency.id)
            .ok()
            .flatten();
        let (version, sha256, source_kind, repository, reference, artifact_url, source_id) =
            match lock {
                Some(entry) => (
                    if entry.version.trim().is_empty() {
                        dependency.min_version.clone()
                    } else {
                        entry.version.clone()
                    },
                    entry.sha256.clone(),
                    entry.source.clone(),
                    entry.repository.clone(),
                    entry.reference.clone(),
                    entry.artifact_url.clone(),
                    entry.source_id.clone(),
                ),
                None => (
                    dependency.min_version.clone(),
                    dependency.sha256.clone(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                ),
            };
        let pinned = !version.trim().is_empty()
            && (!sha256.trim().is_empty() || !repository.trim().is_empty());
        if pinned {
            result.pinned += 1;
        } else {
            result.unpinned.push(dependency.id.clone());
        }
        if dependency.required && !pinned && version.trim().is_empty() {
            unresolved.push(dependency.id.clone());
        }
        result.items.push(json!({
            "kind": dependency.kind,
            "id": dependency.id,
            "required": dependency.required,
            "min_version": dependency.min_version,
            "version": version,
            "sha256": sha256,
            "source": {
                "kind": source_kind,
                "id": source_id,
                "repository": repository,
                "reference": reference,
                "artifact_url": artifact_url,
            },
            "pinned": pinned,
        }));
    }
    if !unresolved.is_empty() {
        result.blocker = Some(format!(
            "必需依赖无法解析到可定位的版本，不能在 GitHub 分发清单里给出精确 pin：{}。请先安装这些依赖（或改用工作台分发）后重试。",
            unresolved.join(", ")
        ));
    }
    result
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TargetOutcome {
    pub target: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// 解析已确认候选制品。未测试、未确认或产物被改动过都会在这里被拦下。
pub(crate) fn resolve_asset(
    kind: ExtensionProjectKind,
    id: &str,
    version: &str,
) -> Result<DistributionAsset, Box<dyn Error>> {
    match kind {
        ExtensionProjectKind::Plugin => {
            let draft = crate::plugin_authoring::read(id, version)?;
            if draft.tested_at.is_none() {
                return Err("插件候选包尚未完成测试，无法发布".into());
            }
            if draft.confirmed_at.is_none() {
                return Err("插件候选版本尚未确认，无法发布".into());
            }
            let declared = draft
                .manifest
                .plugin_dependencies
                .iter()
                .map(|dependency| DeclaredDependency {
                    kind: "plugin".to_string(),
                    id: dependency.plugin_id.clone(),
                    required: dependency.required,
                    min_version: dependency.min_version.clone(),
                    sha256: String::new(),
                })
                .collect::<Vec<_>>();
            build_asset(
                "plugin",
                id,
                version,
                &draft.manifest.name,
                &draft.manifest.release_notes,
                &draft.manifest.min_agent_version,
                &draft.candidate_path,
                &draft.candidate_sha256,
                pin_dependencies(&declared),
            )
        }
        ExtensionProjectKind::Skill => {
            let draft = crate::skill::authoring::read(id, version)?;
            if draft.tested_at.is_none() {
                return Err("Skill 候选包尚未完成测试，无法发布".into());
            }
            if draft.confirmed_at.is_none() {
                return Err("技能候选版本尚未确认，无法发布".into());
            }
            let declared = draft
                .manifest
                .plugin_dependencies
                .iter()
                .map(|dependency| DeclaredDependency {
                    kind: "plugin".to_string(),
                    id: dependency.plugin_id.clone(),
                    required: dependency.required,
                    min_version: dependency.min_version.clone().unwrap_or_default(),
                    sha256: String::new(),
                })
                .collect::<Vec<_>>();
            build_asset(
                "skill",
                id,
                version,
                &draft.manifest.name,
                &draft.manifest.release_notes,
                &draft.manifest.min_agent_version,
                &draft.candidate_path,
                &draft.candidate_sha256,
                pin_dependencies(&declared),
            )
        }
        ExtensionProjectKind::Workflow => {
            let draft = crate::workflow::read_authoring_draft(id, version)?;
            if draft.state != crate::extension_contracts::ExtensionCandidateState::Confirmed {
                return Err("工作流候选版本尚未确认，无法发布".into());
            }
            // Workflow 锁里的依赖已经带精确版本与摘要，直接作为声明层输入。
            let declared = draft
                .lock
                .as_ref()
                .map(|lock| {
                    lock.dependencies
                        .iter()
                        .map(|dependency| DeclaredDependency {
                            kind: dependency.kind.as_str().to_string(),
                            id: dependency.id.clone(),
                            required: dependency.required,
                            min_version: dependency.version.clone(),
                            sha256: dependency.sha256.clone(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            build_asset(
                "workflow",
                id,
                version,
                &draft.manifest.name,
                &draft.manifest.release_notes,
                &draft.manifest.min_agent_version,
                &draft.candidate_path,
                &draft.candidate_sha256,
                pin_dependencies(&declared),
            )
        }
        ExtensionProjectKind::Expert => {
            let draft = crate::expert::read_authoring_draft(id, version)?;
            if draft.tested_at.is_none() { return Err("专家候选包尚未完成测试，无法发布".into()); }
            if draft.confirmed_at.is_none() { return Err("专家候选版本尚未确认，无法发布".into()); }
            build_asset("expert", id, version, &draft.definition.name, &draft.definition.release_notes, &draft.definition.min_agent_version, &draft.candidate_path, &draft.candidate_sha256, PinnedDependencies::default())
        }
        // 项目规则的收敛动作是发布到本机规则库，仓库与工作台分发都还没有对应端点。
        // 这里显式报错，避免落到默认分支后把规则当成插件发出去。
        ExtensionProjectKind::Instruction => Err(
            "项目规则请在「规则库」完成预检、确认和发布；仓库分发暂未接入".into(),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_asset(
    kind: &str,
    id: &str,
    version: &str,
    name: &str,
    release_notes: &str,
    min_agent_version: &str,
    package_path: &std::path::Path,
    expected_sha256: &str,
    dependencies: PinnedDependencies,
) -> Result<DistributionAsset, Box<dyn Error>> {
    if !package_path.is_file() {
        return Err(format!("候选制品不存在: {}", package_path.display()).into());
    }
    let bytes = std::fs::read(package_path)?;
    let sha256 = hex_sha256(&bytes);
    // 产物被改动过就说明候选与测试过的内容不是同一份，必须重新构建。
    if !expected_sha256.trim().is_empty() && !sha256.eq_ignore_ascii_case(expected_sha256.trim()) {
        return Err("候选制品摘要与构建记录不一致，请重新构建后再发布".into());
    }
    Ok(DistributionAsset {
        kind: kind.to_string(),
        id: id.trim().to_string(),
        version: version.trim().to_string(),
        name: name.trim().to_string(),
        release_notes: release_notes.trim().to_string(),
        min_agent_version: min_agent_version.trim().to_string(),
        package_path: package_path.to_path_buf(),
        sha256,
        size_bytes: bytes.len() as u64,
        dependency_summary: json!({
            "total": dependencies.items.len(),
            "pinned": dependencies.pinned,
            "unpinned": dependencies.unpinned,
            "blocked": dependencies.blocker.is_some(),
        }),
        dependency_blocker: dependencies.blocker,
        dependencies: json!(dependencies.items),
    })
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

/// 发布预览：不产生任何远端副作用，用于 UI 展示「这次会发到哪里、发什么」。
pub(crate) fn preview(
    kind: ExtensionProjectKind,
    id: &str,
    version: &str,
) -> Result<Value, Box<dyn Error>> {
    let targets = extension_projects::effective_distribution_targets(kind, id);
    let source = extension_projects::submission_source(kind, id)?;
    let asset = resolve_asset(kind, id, version)?;
    let tag = github_publisher::tag_name(&asset.kind, &asset.id, &asset.version)?;
    let asset_file = github_publisher::asset_name(&asset.kind, &asset.id, &asset.version)?;
    let account = crate::store::github_credentials::status()?;
    // 项目里可能存完整 URL，这里统一展示成 GitHub API 需要的 owner/repo。
    let slug = github_publisher::normalize_repository_slug(&source.source_repository)
        .unwrap_or_else(|_| source.source_repository.clone());
    Ok(json!({
        "kind": asset.kind,
        "id": asset.id,
        "version": asset.version,
        "name": asset.name,
        "targets": targets.iter().map(|target| target.as_str()).collect::<Vec<_>>(),
        "github": {
            "repository": slug,
            "branch": source.source_default_branch,
            "commit": source.source_commit,
            "tag": tag,
            "asset_name": asset_file,
            "manifest_name": github_publisher::manifest_name(&asset.id, &asset.version),
            "sha256": asset.sha256,
            "size_bytes": asset.size_bytes,
            "authorized": account.authorized,
            "login": account.login,
            "signature": extension_signing::status(),
            "dependencies": asset.dependency_summary,
            "dependency_blocker": asset.dependency_blocker,
        },
        "workbench": {
            "distribution_id": source.distribution_id,
            "channel": source.channel,
            "catalog_id": source.catalog_id,
        },
    }))
}

/// 按项目生效目标发布。固定顺序 GitHub → 工作台；任一失败进入部分完成。
pub(crate) fn publish(
    options: &crate::Options,
    agent_id: &str,
    kind: ExtensionProjectKind,
    id: &str,
    version: &str,
) -> Result<Value, Box<dyn Error>> {
    let targets = extension_projects::effective_distribution_targets(kind, id);
    let asset = resolve_asset(kind, id, version)?;
    let mut outcomes = Vec::new();

    // 顺序固定：GitHub 先发布，工作台提审才能带上可追溯的 tag 与摘要。
    let ordered = [DistributionTarget::Github, DistributionTarget::Workbench];
    for target in ordered {
        if !targets.contains(&target) {
            continue;
        }
        outcomes.push(match target {
            DistributionTarget::Github => publish_github(&asset)?,
            DistributionTarget::Workbench => publish_workbench(options, agent_id, &asset, kind)?,
        });
    }

    if outcomes.is_empty() {
        return Err("当前项目没有可用的分发目标".into());
    }
    let published = outcomes
        .iter()
        .filter(|outcome| outcome.status == STATUS_PUBLISHED)
        .count();
    let status = if published == outcomes.len() {
        "released"
    } else if published == 0 {
        STATUS_FAILED
    } else {
        "partially_published"
    };
    Ok(json!({
        "kind": asset.kind,
        "id": asset.id,
        "version": asset.version,
        "targets": targets.iter().map(|target| target.as_str()).collect::<Vec<_>>(),
        "status": status,
        "outcomes": outcomes,
    }))
}

fn publish_github(asset: &DistributionAsset) -> Result<TargetOutcome, Box<dyn Error>> {
    let target = DistributionTarget::Github;
    let tag = github_publisher::tag_name(&asset.kind, &asset.id, &asset.version)?;
    // 依赖无法 pin 时清单对消费侧没有意义，这里就停下，不产生任何远端副作用。
    if let Some(blocker) = asset.dependency_blocker.as_deref() {
        let previous = distribution_state::load()?
            .get(&asset.kind, &asset.id, &asset.version, target)
            .cloned();
        distribution_state::record(failed_entry(
            asset,
            target,
            &tag,
            blocker,
            previous.as_ref(),
        ))?;
        return Ok(TargetOutcome {
            target: target.as_str().to_string(),
            status: STATUS_FAILED.to_string(),
            detail: Some(json!({ "dependency": asset.dependency_summary })),
            error: blocker.to_string(),
        });
    }
    let source = extension_projects::submission_source(
        match asset.kind.as_str() {
            "skill" => ExtensionProjectKind::Skill,
            "workflow" => ExtensionProjectKind::Workflow,
            "expert" => ExtensionProjectKind::Expert,
            "instruction" => ExtensionProjectKind::Instruction,
            _ => ExtensionProjectKind::Plugin,
        },
        &asset.id,
    )?;
    let manifest_name = github_publisher::manifest_name(&asset.id, &asset.version);
    let manifest_path = manifest_scratch_path(&asset.id, &asset.version);
    let asset_file = github_publisher::asset_name(&asset.kind, &asset.id, &asset.version)?;
    let previous = distribution_state::load()?
        .get(&asset.kind, &asset.id, &asset.version, target)
        .cloned();
    // 签名在本地完成：未配置私钥就按未签名发布，配置了却签不出来则停下，
    // 不允许把「本该签名的制品」当成未签名制品发出去。
    let signature = match sign_release_artifact(asset) {
        Ok(value) => value,
        Err(message) => {
            distribution_state::record(failed_entry(
                asset,
                target,
                &tag,
                &message,
                previous.as_ref(),
            ))?;
            return Ok(TargetOutcome {
                target: target.as_str().to_string(),
                status: STATUS_FAILED.to_string(),
                detail: None,
                error: message,
            });
        }
    };
    let plan = GithubPublishPlan {
        kind: asset.kind.clone(),
        id: asset.id.clone(),
        version: asset.version.clone(),
        repository: source.source_repository.clone(),
        commit: source.source_commit.clone(),
        branch: source.source_default_branch.clone(),
        release_name: format!("{} v{}", asset.name, asset.version),
        release_notes: asset.release_notes.clone(),
        channel: if source.channel.trim().is_empty() {
            "stable".to_string()
        } else {
            source.channel.clone()
        },
        prerelease: !source.channel.trim().is_empty() && source.channel.trim() != "stable",
        artifacts: Vec::new(),
        signature: signature.clone(),
    };
    if plan.repository.trim().is_empty() {
        let entry = failed_entry(
            asset,
            target,
            &tag,
            "项目未绑定 GitHub 仓库，无法发布到 GitHub。请在项目设置里填写代码仓库地址。",
            previous.as_ref(),
        );
        distribution_state::record(entry)?;
        return Ok(TargetOutcome {
            target: target.as_str().to_string(),
            status: STATUS_FAILED.to_string(),
            detail: None,
            error: "项目未绑定 GitHub 仓库，无法发布到 GitHub。请在项目设置里填写代码仓库地址。"
                .to_string(),
        });
    }
    let primary = GithubArtifact {
        name: asset_file.clone(),
        path: asset.package_path.clone(),
        sha256: asset.sha256.clone(),
        size_bytes: asset.size_bytes,
        content_type: "application/octet-stream".to_string(),
    };
    // 清单与制品一起上传：消费侧只读清单就能确认摘要与依赖。
    let manifest_content = {
        let mut resolved_plan = plan.clone();
        resolved_plan.artifacts = vec![primary.clone()];
        let manifest = github_publisher::build_release_manifest(
            &resolved_plan,
            &tag,
            &primary,
            &asset.dependencies,
            &asset.min_agent_version,
        );
        serde_json::to_vec_pretty(&manifest)?
    };
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&manifest_path, &manifest_content)?;
    let manifest_artifact = GithubArtifact {
        name: manifest_name,
        path: manifest_path.clone(),
        sha256: hex_sha256(&manifest_content),
        size_bytes: manifest_content.len() as u64,
        content_type: "application/json".to_string(),
    };
    let mut plan = plan;
    plan.artifacts = vec![primary];
    plan.artifacts.push(manifest_artifact);
    let previous_release = github_publisher::PreviousRelease {
        asset_name: previous
            .as_ref()
            .map(|entry| entry.asset_name.clone())
            .unwrap_or_default(),
        sha256: previous
            .as_ref()
            .map(|entry| entry.sha256.clone())
            .unwrap_or_default(),
        published: previous
            .as_ref()
            .map(|entry| entry.status == STATUS_PUBLISHED)
            .unwrap_or(false),
    };
    let pending = pending_entry(asset, target, &tag, &plan, previous.as_ref());
    distribution_state::record(pending)?;

    match github_publisher::publish_with_stored_credential(&plan, &previous_release) {
        Ok(outcome) => {
            let mut entry = published_entry(asset, target, &outcome, previous.as_ref());
            entry.sha256 = asset.sha256.clone();
            distribution_state::record(entry)?;
            Ok(TargetOutcome {
                target: target.as_str().to_string(),
                status: STATUS_PUBLISHED.to_string(),
                detail: Some(serde_json::to_value(&outcome)?),
                error: String::new(),
            })
        }
        Err(error) => {
            let message = safe_error(&error.to_string());
            distribution_state::record(failed_entry(
                asset,
                target,
                &tag,
                &message,
                previous.as_ref(),
            ))?;
            Ok(TargetOutcome {
                target: target.as_str().to_string(),
                status: STATUS_FAILED.to_string(),
                detail: None,
                error: message,
            })
        }
    }
}

fn publish_workbench(
    options: &crate::Options,
    agent_id: &str,
    asset: &DistributionAsset,
    kind: ExtensionProjectKind,
) -> Result<TargetOutcome, Box<dyn Error>> {
    let target = DistributionTarget::Workbench;
    let previous = distribution_state::load()?
        .get(&asset.kind, &asset.id, &asset.version, target)
        .cloned();
    let result = match kind {
        ExtensionProjectKind::Plugin => {
            crate::plugin_authoring::submit(options, agent_id, &asset.id, &asset.version)
                .map(|draft| draft.dashboard_submission_id.unwrap_or_default())
        }
        ExtensionProjectKind::Skill => {
            crate::skill::authoring::submit(options, agent_id, &asset.id, &asset.version)
                .map(|draft| draft.dashboard_draft_id.unwrap_or_default())
        }
        ExtensionProjectKind::Workflow => crate::workflow::submit_authoring_candidate(
            options,
            agent_id,
            &asset.id,
            &asset.version,
        )
        .map(|draft| draft.dashboard_submission_id.unwrap_or_default()),
        ExtensionProjectKind::Expert => Err("专家扩展暂未接入工作台发布端点".into()),
        ExtensionProjectKind::Instruction => {
            Err("项目规则暂未接入工作台发布端点，请先在「规则库」发布到本机".into())
        }
    };
    match result {
        Ok(submission_id) => {
            distribution_state::record(DistributionStateEntry {
                submission_id,
                status: STATUS_PUBLISHED.to_string(),
                published_at: now_stamp(),
                error: String::new(),
                ..entry_shell(asset, target, previous.as_ref())
            })?;
            Ok(TargetOutcome {
                target: target.as_str().to_string(),
                status: STATUS_PUBLISHED.to_string(),
                detail: None,
                error: String::new(),
            })
        }
        Err(error) => {
            let message = safe_error(&error.to_string());
            distribution_state::record(DistributionStateEntry {
                status: STATUS_FAILED.to_string(),
                error: message.clone(),
                ..entry_shell(asset, target, previous.as_ref())
            })?;
            Ok(TargetOutcome {
                target: target.as_str().to_string(),
                status: STATUS_FAILED.to_string(),
                detail: None,
                error: message,
            })
        }
    }
}

/// 台账条目的公共骨架。`published_at` 只由各落点在成功时写入。
fn entry_shell(
    asset: &DistributionAsset,
    target: DistributionTarget,
    previous: Option<&DistributionStateEntry>,
) -> DistributionStateEntry {
    let source = extension_projects::submission_source(
        match asset.kind.as_str() {
            "skill" => ExtensionProjectKind::Skill,
            "workflow" => ExtensionProjectKind::Workflow,
            "expert" => ExtensionProjectKind::Expert,
            "instruction" => ExtensionProjectKind::Instruction,
            _ => ExtensionProjectKind::Plugin,
        },
        &asset.id,
    )
    .unwrap_or_default();
    DistributionStateEntry {
        kind: asset.kind.clone(),
        id: asset.id.clone(),
        version: asset.version.clone(),
        target,
        status: STATUS_PENDING.to_string(),
        tag: String::new(),
        release_id: String::new(),
        html_url: previous
            .map(|entry| entry.html_url.clone())
            .unwrap_or_default(),
        // 主制品名始终记进台账：即使这次失败，重试时也能据此判断「是不是同一批」。
        asset_name: github_publisher::asset_name(&asset.kind, &asset.id, &asset.version)
            .unwrap_or_default(),
        sha256: asset.sha256.clone(),
        size_bytes: asset.size_bytes,
        submission_id: String::new(),
        release_reference: previous
            .map(|entry| entry.release_reference.clone())
            .unwrap_or_default(),
        channel: source.channel,
        published_at: String::new(),
        error: String::new(),
        attempts: 0,
        updated_at: String::new(),
    }
}

fn pending_entry(
    asset: &DistributionAsset,
    target: DistributionTarget,
    tag: &str,
    plan: &GithubPublishPlan,
    previous: Option<&DistributionStateEntry>,
) -> DistributionStateEntry {
    DistributionStateEntry {
        tag: tag.to_string(),
        asset_name: plan
            .artifacts
            .first()
            .map(|artifact| artifact.name.clone())
            .unwrap_or_default(),
        ..entry_shell(asset, target, previous)
    }
}

fn published_entry(
    asset: &DistributionAsset,
    target: DistributionTarget,
    outcome: &github_publisher::GithubPublishOutcome,
    previous: Option<&DistributionStateEntry>,
) -> DistributionStateEntry {
    DistributionStateEntry {
        status: STATUS_PUBLISHED.to_string(),
        tag: outcome.tag.clone(),
        release_id: outcome.release_id.clone(),
        html_url: outcome.html_url.clone(),
        asset_name: outcome.asset_name.clone(),
        sha256: outcome.sha256.clone(),
        size_bytes: outcome.size_bytes,
        published_at: now_stamp(),
        error: String::new(),
        ..entry_shell(asset, target, previous)
    }
}

fn failed_entry(
    asset: &DistributionAsset,
    target: DistributionTarget,
    tag: &str,
    error: &str,
    previous: Option<&DistributionStateEntry>,
) -> DistributionStateEntry {
    DistributionStateEntry {
        status: STATUS_FAILED.to_string(),
        tag: tag.to_string(),
        error: error.to_string(),
        ..entry_shell(asset, target, previous)
    }
}

/// 台账与报告里保存的错误信息只来自本机，不含凭据；这里再兜一层截断。
fn safe_error(message: &str) -> String {
    message.trim().chars().take(500).collect()
}

fn manifest_scratch_path(id: &str, version: &str) -> PathBuf {
    crate::store::paths::agent_home()
        .join("data/distribution-manifests")
        .join(github_publisher::manifest_name(id, version))
}

/// 生成本次发布的签名元数据。`Err` 表示「本机想签名但签不出来」，直接阻断发布；
/// `Ok(None)` 表示本机没有配置签名私钥，按未签名制品发布。
fn sign_release_artifact(asset: &DistributionAsset) -> Result<Option<Value>, String> {
    let key = extension_signing::configured_key().map_err(|error| error.to_string())?;
    let Some(key) = key else {
        return Ok(None);
    };
    extension_signing::sign_artifact(&asset.package_path, &key)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn now_stamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_sha256_matches_known_vector() {
        assert_eq!(
            hex_sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn safe_error_truncates_and_keeps_short_messages() {
        assert_eq!(safe_error("  出错了  "), "出错了");
        let long = "x".repeat(800);
        assert_eq!(safe_error(&long).chars().count(), 500);
    }
}
