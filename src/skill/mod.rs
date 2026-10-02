pub(crate) mod authoring;
pub(crate) mod cli;
pub(crate) mod clients;
pub(crate) mod codex;
pub(crate) mod copilot;
pub(crate) mod development;
pub(crate) mod direct;
pub(crate) mod hygiene;
pub(crate) mod manifest;
pub(crate) mod resolver;
pub(crate) mod store;
pub(crate) mod target;
pub(crate) mod types;

use crate::capability::service::CapabilityGateway;
use crate::capability::types::InvocationContext;
use crate::skill::clients::{
    declares_portable_skill, manifest_supports_client, PORTABLE_PROFILE_ID,
};
use crate::skill::resolver::{CapabilityFact, SkillReadiness};
use crate::skill::store::retired_skill_ids;
use crate::skill::store::SkillStore;
use crate::skill::types::{SkillManifest, SkillRecord};
use crate::store::types::LocalWorkerStatus;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub(crate) fn catalog_json(
    agent_version: &str,
    client_id: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    maintain_client_skill_directories();
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let records = store.list_records()?;
    let items = records
        .into_iter()
        .map(|record| {
            let readiness = SkillReadiness::resolve(
                &record.manifest,
                capability_facts,
                agent_version,
                client_id,
            );
            json!({
                "record": record,
                "readiness": readiness,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "client_id": client_id,
        "agent_version": agent_version,
        "store_root": store.root().to_string_lossy().to_string(),
        "items": items,
    }))
}

/// Project ready portable Skills into the MCP Prompt surface.  Skills remain
/// orchestration documents; their referenced capabilities are still invoked
/// through the normal Gateway tools.
pub(crate) fn mcp_prompts_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let records = ready_mcp_records(agent_version, capability_facts)?;
    let prompts = records
        .into_iter()
        .map(|record| {
            json!({
                "name": record.manifest.id,
                "title": record.manifest.name,
                "description": record.manifest.description,
                "arguments": [],
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "prompts": prompts }))
}

pub(crate) fn mcp_prompt_get(
    name: &str,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let record = ready_mcp_records(agent_version, capability_facts)?
        .into_iter()
        .find(|record| record.manifest.id == name.trim())
        .ok_or_else(|| format!("MCP Prompt not found or unavailable: {name}"))?;
    let readme = std::fs::read_to_string(record.version_root.join("SKILL.md"))?;
    Ok(json!({
        "description": record.manifest.description,
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": readme }
        }]
    }))
}

pub(crate) fn mcp_resources_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let mut resources = Vec::new();
    for record in ready_mcp_records(agent_version, capability_facts)? {
        for content in &record.manifest.contents {
            if content.eq_ignore_ascii_case("skill.json")
                || content.eq_ignore_ascii_case("SKILL.md")
                || crate::skill::manifest::is_internal_package_file(content)
            {
                continue;
            }
            resources.push(json!({
                "uri": format!("himind://skill/{}/{}", record.manifest.id, content.replace('\\', "/")),
                "name": format!("{} / {}", record.manifest.name, content),
                "description": record.manifest.description,
                "mimeType": mcp_mime_type(content),
            }));
        }
    }
    Ok(json!({ "resources": resources }))
}

pub(crate) fn mcp_resource_read(
    uri: &str,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let value = uri
        .strip_prefix("himind://skill/")
        .ok_or("不支持的 HiMind Skill Resource URI")?;
    let (skill_id, relative) = value.split_once('/').ok_or("Skill Resource URI 缺少路径")?;
    crate::skill::manifest::validate_relative_package_path(relative)?;
    if crate::skill::manifest::is_internal_package_file(relative) {
        return Err("Skill Resource 不允许读取 HiMind 内部元数据".into());
    }
    let record = ready_mcp_records(agent_version, capability_facts)?
        .into_iter()
        .find(|record| record.manifest.id == skill_id)
        .ok_or_else(|| format!("MCP Resource not found or unavailable: {uri}"))?;
    if !record
        .manifest
        .contents
        .iter()
        .any(|item| item.replace('\\', "/") == relative)
    {
        return Err("Skill Resource 未在 Manifest contents 中声明".into());
    }
    let path = record.version_root.join(relative);
    if !path.is_file() {
        return Err("Skill Resource 文件不存在".into());
    }
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": mcp_mime_type(relative),
            "text": std::fs::read_to_string(path)?
        }]
    }))
}

fn ready_mcp_records(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Vec<SkillRecord>, Box<dyn Error>> {
    maintain_client_skill_directories();
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    Ok(store
        .list_records()?
        .into_iter()
        .filter(|record| declares_portable_skill(&record.manifest))
        .filter(|record| {
            SkillReadiness::resolve(
                &record.manifest,
                capability_facts,
                agent_version,
                "himind-ai",
            )
            .state
                != "blocked"
        })
        .collect())
}

fn mcp_mime_type(path: &str) -> &'static str {
    let extension = path.rsplit('.').next().unwrap_or_default();
    if extension.eq_ignore_ascii_case("json") {
        "application/json"
    } else if extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml") {
        "application/yaml"
    } else if extension.eq_ignore_ascii_case("md") {
        "text/markdown"
    } else if extension.eq_ignore_ascii_case("txt") {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}

pub(crate) fn client_status_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    maintain_client_skill_directories();
    // 技能记录与同步模式在三个客户端族之间是同一份：读一次共享，省掉三份重复
    // 读盘。bootstrap 会真的动磁盘（下线残留、种子补齐），所以放在并发之前跑完，
    // 后面三个分支都只是只读扫描。
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let configured_sync_mode = store.sync_mode()?;
    let records = store.list_records()?;
    // HiMind AI、Codex 与二十来个目录客户端互不依赖：串行是"三段耗时相加"，
    // 并行只取最慢的一段。这是技能页从六秒级降到秒级的主要手段。
    let (himind_ai, codex_status, direct_status) = std::thread::scope(|scope| {
        let himind_ai = scope.spawn(|| {
            himind_ai_status_json(
                agent_version,
                capability_facts,
                &store,
                &records,
                &configured_sync_mode,
            )
            .map_err(|error| error.to_string())
        });
        let codex_status = scope.spawn(|| {
            codex::status_json(
                agent_version,
                capability_facts,
                &store,
                &records,
                &configured_sync_mode,
            )
            .map_err(|error| error.to_string())
        });
        let direct_status = scope.spawn(|| {
            direct::status_json(
                agent_version,
                capability_facts,
                &store,
                &records,
                &configured_sync_mode,
            )
            .map_err(|error| error.to_string())
        });
        (
            joined_result(himind_ai),
            joined_result(codex_status),
            joined_result(direct_status),
        )
    });
    let mut clients = BTreeMap::new();
    clients.insert("himind-ai".to_string(), himind_ai?);
    clients.insert("codex".to_string(), codex_status?);
    for (client_id, status) in direct_status? {
        clients.insert(client_id, status);
    }
    Ok(json!(clients))
}

/// 三份客户端快照并行计算。某个分支 panic 说明代码有 bug，不能把半个技能页
/// 当成正常结果发出去，所以这里把 panic 也翻译成错误交给前端。
fn joined_result<T>(
    handle: std::thread::ScopedJoinHandle<'_, Result<T, String>>,
) -> Result<T, Box<dyn Error>> {
    match handle.join() {
        Ok(result) => result.map_err(|error| error.into()),
        Err(_) => Err("技能状态快照计算线程异常退出".into()),
    }
}

/// 环境变量 `HIMIND_SKILL_TRACE` 指向文件时，把关键阶段耗时追加进去。
/// 只用于本机排查技能页耗时，不做任何常驻统计。
pub(crate) fn trace_lap(label: &str, started: std::time::Instant) {
    let Some(path) = std::env::var_os("HIMIND_SKILL_TRACE") else {
        return;
    };
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{label} {}ms", started.elapsed().as_millis());
    }
}

pub(crate) fn client_sync_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    maintain_client_skill_directories();
    let mut clients = BTreeMap::new();
    clients.insert(
        "himind-ai".to_string(),
        himind_ai_sync_json(agent_version, capability_facts)?,
    );
    clients.insert(
        "codex".to_string(),
        codex::sync_json(agent_version, capability_facts)?,
    );
    for (client_id, sync) in direct::sync_json(agent_version, capability_facts)? {
        clients.insert(client_id, sync);
    }
    Ok(json!(clients))
}

fn retire_removed_client_skills() {
    for skill_id in retired_skill_ids() {
        let _ = codex::uninstall_json(skill_id);
        let _ = direct::uninstall_json(skill_id);
    }
}

/// 客户端技能目录的日常维护：先收尾已下线的技能，再清掉历史布局与中断残留。
///
/// 客户端按 `**/SKILL.md` 递归发现技能，目录里多留一份副本就等于多出一个
/// "技能"（旧版 `<id>/current`、`<id>/previous` 以及渲染中断的 staging 目录
/// 都会命中）。所以每次列出/同步技能之前先跑一遍，清不掉的内容不会阻塞主流程。
fn maintain_client_skill_directories() {
    retire_removed_client_skills();
    sweep_client_skill_directories();
}

/// 清掉客户端技能目录里的历史布局与中断残留（判定规则见 [`hygiene`]）。
///
/// 与 [`maintain_client_skill_directories`] 分开，是因为启动时的后台维护只需要
/// 这一步，不必把"下线技能"的收尾逻辑也带上。
pub(crate) fn sweep_client_skill_directories() {
    let active_root = codex::active_root();
    let report = hygiene::run(active_root.as_deref());
    if report.touched() == 0 {
        return;
    }
    let mut details = Vec::new();
    if !report.removed_legacy.is_empty() {
        details.push(format!(
            "{} 个旧版布局目录（{}）",
            report.removed_legacy.len(),
            summarize_paths(&report.removed_legacy)
        ));
    }
    if !report.removed_residue.is_empty() {
        details.push(format!(
            "{} 个中断残留（{}）",
            report.removed_residue.len(),
            summarize_paths(&report.removed_residue)
        ));
    }
    crate::approval::manager::ApprovalManager::global().add_log(
        "info",
        &format!("已清理 AI 客户端技能目录：{}", details.join("；")),
    );
}

fn summarize_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("、")
}

pub(crate) fn sync_record_to_supported_clients(
    record: &SkillRecord,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<BTreeMap<String, Value>, Box<dyn Error>> {
    sync_record_to_clients(record, agent_version, capability_facts, None)
}

/// 与 [`sync_record_to_supported_clients`] 相同，但可以限定投放目标。
///
/// `targets` 为 `None` 表示沿用默认行为（本机探测到的全部工具）；为 `Some` 时只投放
/// 其中出现过的工具。`himind-ai` 始终保留：它是本产品自己的 agent，技能在库里就
/// 必须对它可用，否则"装了却不能用"。
pub(crate) fn sync_record_to_clients(
    record: &SkillRecord,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
    targets: Option<&[String]>,
) -> Result<BTreeMap<String, Value>, Box<dyn Error>> {
    ensure_workspace_record_is_current(record)?;
    let selected = targets.map(|values| {
        values
            .iter()
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect::<std::collections::BTreeSet<_>>()
    });
    let mut clients = BTreeMap::new();
    let candidates = match targets {
        Some(values) => sync_client_ids_for_targets(record, values),
        None => sync_client_ids(record),
    };
    for normalized in candidates {
        if !client_is_selected(&normalized, selected.as_ref()) {
            continue;
        }
        if clients.contains_key(&normalized) {
            continue;
        }
        let rendered = match normalized.as_str() {
            "codex" => codex::sync_record_json(record, agent_version, capability_facts)?,
            "himind-ai" => himind_ai_sync_record_json(record, agent_version, capability_facts)?,
            _ => direct::sync_record_json(&normalized, record, agent_version, capability_facts)?,
        };
        clients.insert(normalized, rendered);
    }
    Ok(clients)
}

/// A regular per-Skill sync is a repair operation for the current target.  In
/// a workspace it must not turn a newer Store record into an implicit project
/// upgrade; users use the explicit workspace-update action for that.  A Skill
/// with no lock/deployment is still allowed here because the same operation is
/// the first explicit install into a selected project.
pub(crate) fn ensure_workspace_record_is_current(
    record: &SkillRecord,
) -> Result<(), Box<dyn Error>> {
    let Some(workspace_root) = crate::skill::target::resolve_workspace_root(None)? else {
        return Ok(());
    };
    let lock = crate::skill::target::read_workspace_lock(&workspace_root)?;
    if let Some(entry) = lock.skills.get(&record.manifest.id) {
        if entry.management != crate::skill::target::MANAGEMENT_MODE_MANAGED {
            return Err(format!(
                "工作区 Skill {} 由项目原生目录管理，HiMind 不会覆盖",
                record.manifest.id
            )
            .into());
        }
        if !entry.enabled {
            return Err(format!(
                "工作区 Skill {} 已被禁用，请先启用后再同步",
                record.manifest.id
            )
            .into());
        }
        if entry.version != record.manifest.version {
            return Err(format!(
                "工作区 Skill {} 已锁定 v{}，当前 Store 为 v{}；请使用“更新工作区版本”",
                record.manifest.id, entry.version, record.manifest.version
            )
            .into());
        }
        return Ok(());
    }
    if let Some(pinned) =
        crate::skill::target::workspace_pinned_version(&workspace_root, &record.manifest.id)?
    {
        if pinned != record.manifest.version {
            return Err(format!(
                "工作区 Skill {} 已锁定 v{}，当前 Store 为 v{}；请使用“更新工作区版本”",
                record.manifest.id, pinned, record.manifest.version
            )
            .into());
        }
    }
    Ok(())
}

/// Synchronize one Skill to one explicitly selected AI client.
///
/// The regular sync path intentionally targets every active, supported client.
/// This narrower operation is used by the UI and MCP management surfaces when
/// a user wants to repair one client without changing the others.
pub(crate) fn update_workspace_skill_json(
    skill_id: &str,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let workspace = crate::skill::target::resolve_workspace_root(None)?
        .ok_or("更新工作区 Skill 需要先显式选择项目工作区")?;
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let target =
        crate::skill::target::SkillTarget::workspace(&workspace, ".agents/skills", "workspace")?;
    let previous = crate::skill::target::workspace_pinned_version(&workspace, skill_id)?;
    crate::skill::target::record_workspace_skill(&target, &record, "himind-store")?;
    let clients = sync_record_to_supported_clients(&record, agent_version, capability_facts)?;
    Ok(json!({
        "skill_id": record.manifest.id,
        "workspace_root": workspace.to_string_lossy().to_string(),
        "previous_version": previous,
        "version": record.manifest.version,
        "lock_updated": true,
        "lock_path": crate::skill::target::workspace_lock_path(&workspace)
            .to_string_lossy()
            .to_string(),
        "clients": clients,
    }))
}

/// Enable or disable a Skill inside the selected project.  Disabling removes
/// the project projection while keeping the pinned lock entry; enabling
/// re-projects only when the project and the Store already agree on a version.
pub(crate) fn set_workspace_skill_enabled_json(
    skill_id: &str,
    enabled: bool,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let workspace = crate::skill::target::resolve_workspace_root(None)?
        .ok_or("切换工作区 Skill 状态需要先显式选择项目工作区")?;
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let target =
        crate::skill::target::SkillTarget::workspace(&workspace, ".agents/skills", "workspace")?;
    let created =
        crate::skill::target::ensure_workspace_skill_entry(&target, &record, "himind-store")?;
    let updated = crate::skill::target::set_workspace_skill_enabled(&workspace, skill_id, enabled)?;
    let mut reprojected = false;
    if updated && !enabled {
        unregister_skill_clients_json(skill_id)?;
    } else if updated {
        let pinned = crate::skill::target::workspace_pinned_version(&workspace, skill_id)?;
        if pinned.as_deref() == Some(record.manifest.version.as_str()) {
            sync_record_to_supported_clients(&record, agent_version, capability_facts)?;
            reprojected = true;
        }
    }
    Ok(json!({
        "skill_id": skill_id,
        "workspace_root": workspace.to_string_lossy().to_string(),
        "enabled": enabled,
        "assignment_created": created,
        "updated": updated,
        "reprojected": reprojected,
        "pinned_version": crate::skill::target::workspace_pinned_version(&workspace, skill_id)?,
    }))
}

pub(crate) fn sync_skill_client_json(
    skill_id: &str,
    client_id: &str,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    ensure_workspace_record_is_current(&record)?;
    let normalized = client_id.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err("client_id 不能为空".into());
    }
    if normalized != "himind-ai"
        && normalized != "codex"
        && clients::directory_client(&normalized).is_none()
    {
        return Err(format!("Agent 尚未实现 Skill 客户端适配器: {normalized}").into());
    }
    if !manifest_supports_client(&record.manifest, &normalized) {
        return Ok(json!({
            "skill_id": skill_id,
            "client_id": normalized,
            "client_name": client_name(&normalized),
            "target_configured": false,
            "rendered": {
                "skill_id": skill_id,
                "version": record.manifest.version,
                "state": "unsupported",
                "reason": "该 Skill 未声明此客户端",
            },
        }));
    }
    match normalized.as_str() {
        "codex" => codex::sync_record_json(&record, agent_version, capability_facts),
        "himind-ai" => himind_ai_sync_record_json(&record, agent_version, capability_facts),
        _ => direct::sync_record_json(&normalized, &record, agent_version, capability_facts),
    }
}

/// Remove this Skill from every external client independently.
///
/// A stale or user-owned copy in one client must not prevent cleanup in other
/// clients, so failures are returned per client instead of aborting the batch.
pub(crate) fn unregister_skill_clients_json(skill_id: &str) -> Result<Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    ensure_skill_client_unregister_allowed(&store, skill_id)?;
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let mut results = BTreeMap::new();
    let mut failures = BTreeMap::new();
    let mut removed_count = 0usize;
    for client_id in uninstall_client_ids(&record)
        .into_iter()
        .filter(|client_id| client_id != "himind-ai")
    {
        match unregister_skill_client_json(skill_id, &client_id) {
            Ok(result) => {
                if result
                    .get("removed")
                    .and_then(|removed| removed.get("removed"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    removed_count += 1;
                }
                results.insert(client_id, result);
            }
            Err(error) => {
                failures.insert(client_id, error.to_string());
            }
        }
    }
    Ok(json!({
        "skill_id": skill_id,
        "removed_count": removed_count,
        "results": results,
        "failures": failures,
    }))
}

pub(crate) fn repair_record_for_supported_clients(
    record: &SkillRecord,
    preserve_modified: bool,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<BTreeMap<String, Value>, Box<dyn Error>> {
    ensure_workspace_record_is_current(record)?;
    let mut clients = BTreeMap::new();
    for normalized in sync_client_ids(record) {
        if clients.contains_key(&normalized) {
            continue;
        }
        let repaired = match normalized.as_str() {
            "codex" => codex::repair_json(
                &record.manifest.id,
                preserve_modified,
                agent_version,
                capability_facts,
            )?,
            "himind-ai" => himind_ai_sync_record_json(record, agent_version, capability_facts)?,
            _ => direct::repair_json(
                &normalized,
                &record.manifest.id,
                preserve_modified,
                agent_version,
                capability_facts,
            )?,
        };
        clients.insert(normalized, repaired);
    }
    Ok(clients)
}

fn himind_ai_status_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
    store: &SkillStore,
    records: &[SkillRecord],
    configured_sync_mode: &str,
) -> Result<Value, Box<dyn Error>> {
    let workspace_root = crate::skill::target::resolve_workspace_root(None)?;
    // 可见性判定要逐技能查部署台账，用快照读一次供全部技能复用。快照读不出来时
    // 按"不可见"处理：这与逐个技能查台账失败时的旧口径一致，也让页面照常渲染。
    let visibility = crate::skill::target::HimindVisibility::load(workspace_root.as_deref()).ok();
    let items = records
        .iter()
        .filter(|record| {
            matches!(
                record.manifest.scope,
                crate::skill::types::SkillScope::Builtin
            ) || visibility
                .as_ref()
                .is_some_and(|snapshot| snapshot.visible(record))
        })
        .map(|record| {
            let readiness = SkillReadiness::resolve(
                &record.manifest,
                capability_facts,
                agent_version,
                "himind-ai",
            );
            let supported = manifest_supports_client(&record.manifest, "himind-ai");
            let client_state = if !supported {
                "unsupported"
            } else if readiness.state == "blocked" {
                "blocked"
            } else {
                "installed"
            };
            json!({
                "record": record,
                "readiness": readiness,
                "rendered_root": record.version_root,
                "rendered": supported,
                "rendered_valid": supported && client_state == "installed",
                "client_state": client_state,
                "installed_version": if supported { Some(record.manifest.version.clone()) } else { None },
                "available_version": record.manifest.version,
                "last_synced_at": Value::Null,
                "managed_files": record.manifest.contents.clone(),
                "modified_files": Vec::<String>::new(),
                "available_actions": Vec::<String>::new(),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "client_id": "himind-ai",
        "client_name": "HiMind AI",
        "client_detected": true,
        "skill_standard": "agentskills.io",
        "support_level": "official",
        "support_note": "由 HiMind Agent 会话直接加载",
        "target_root": store.root().to_string_lossy().to_string(),
        "target_source": "agent-skill-store",
        "target_configured": true,
        "target_exists": store.root().exists(),
        "target_mode": "builtin",
        "target_kind": if workspace_root.is_some() { "workspace" } else { "global" },
        "workspace_root": workspace_root,
        "sync_mode": configured_sync_mode,
        "items": items,
    }))
}

fn himind_ai_sync_json(
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    let workspace_root = crate::skill::target::resolve_workspace_root(None)?;
    let visibility = crate::skill::target::HimindVisibility::load(workspace_root.as_deref())?;
    let mut rendered = Vec::new();
    let mut blocked = Vec::new();
    for record in store.list_records()? {
        if !manifest_supports_client(&record.manifest, "himind-ai") {
            continue;
        }
        if !visibility.visible(&record) {
            continue;
        }
        let readiness = SkillReadiness::resolve(
            &record.manifest,
            capability_facts,
            agent_version,
            "himind-ai",
        );
        if readiness.state == "blocked" {
            blocked.push(json!({
                "skill_id": record.manifest.id,
                "version": record.manifest.version,
                "reasons": readiness.reasons,
            }));
        } else {
            rendered.push(himind_ai_rendered_result(&record));
        }
    }
    Ok(json!({
        "client_id": "himind-ai",
        "target_root": store.root().to_string_lossy().to_string(),
        "target_source": "agent-skill-store",
        "target_configured": true,
        "target_kind": if workspace_root.is_some() { "workspace" } else { "global" },
        "workspace_root": workspace_root,
        "rendered": rendered,
        "skipped": [],
        "blocked": blocked,
    }))
}

fn himind_ai_sync_record_json(
    record: &SkillRecord,
    agent_version: &str,
    capability_facts: &[CapabilityFact],
) -> Result<Value, Box<dyn Error>> {
    let readiness = SkillReadiness::resolve(
        &record.manifest,
        capability_facts,
        agent_version,
        "himind-ai",
    );
    if readiness.state == "blocked" {
        return Err(format!("Skill is blocked: {}", readiness.reasons.join(", ")).into());
    }
    let store = SkillStore::new();
    let target = if let Some(workspace) = crate::skill::target::resolve_workspace_root(None)? {
        crate::skill::target::SkillTarget::workspace(&workspace, ".agents/skills", "workspace")?
    } else {
        crate::skill::target::SkillTarget::global(
            store.root().to_path_buf(),
            "agent-skill-store",
            true,
        )
    };
    crate::skill::target::record_workspace_skill(&target, record, "himind-store")?;
    Ok(json!({
        "client_id": "himind-ai",
        "target_root": store.root().to_string_lossy().to_string(),
        "target_source": "agent-skill-store",
        "target_configured": true,
        "target_kind": target.target_kind,
        "workspace_root": target.workspace_root,
        "workspace_id": target.workspace_id,
        "rendered": himind_ai_rendered_result(record),
        "activation": "next_session",
    }))
}

fn himind_ai_rendered_result(record: &SkillRecord) -> Value {
    json!({
        "skill_id": record.manifest.id,
        "version": record.manifest.version,
        "state": "available",
        "reason": Value::Null,
        "rendered_root": record.version_root,
        "files": record.manifest.contents,
    })
}

pub(crate) fn uninstall_supported_clients_json(skill_id: &str) -> Result<Value, Box<dyn Error>> {
    uninstall_supported_clients_impl(skill_id, false)
}

/// Remove a Skill's managed copy from one external AI client while keeping the
/// Skill in the Agent store. HiMind AI is backed by the Agent store directly,
/// so it has no registration to remove.
pub(crate) fn unregister_skill_client_json(
    skill_id: &str,
    client_id: &str,
) -> Result<Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    ensure_skill_client_unregister_allowed(&store, skill_id)?;
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let normalized = client_id.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err("client_id 不能为空".into());
    }
    if normalized != "himind-ai"
        && normalized != "codex"
        && clients::directory_client(&normalized).is_none()
    {
        return Err(format!("Agent 尚未实现 Skill 客户端适配器: {normalized}").into());
    }
    if !manifest_supports_client(&record.manifest, &normalized) {
        return Ok(json!({
            "skill_id": skill_id,
            "client_id": normalized,
            "client_name": client_name(&normalized),
            "target_root": if normalized == "himind-ai" { store.root().to_string_lossy().to_string() } else { String::new() },
            "target_source": if normalized == "himind-ai" { "agent-skill-store" } else { "" },
            "target_configured": normalized == "himind-ai",
            "removed": { "skill_id": skill_id, "removed": false },
            "state": "unsupported",
            "reason": "该 Skill 未声明此客户端",
        }));
    }
    let raw = match normalized.as_str() {
        "himind-ai" => json!({
            "skill_id": skill_id,
            "removed": false,
            "state": "builtin",
            "reason": "HiMind AI 直接从 Agent Skill Store 加载，无需注册",
        }),
        "codex" => codex::uninstall_json(skill_id)?,
        _ => direct::uninstall_for_client(&normalized, skill_id)?,
    };
    let removal = raw.get("removed").unwrap_or(&raw);
    let removed = removal
        .as_bool()
        .or_else(|| removal.get("removed").and_then(Value::as_bool))
        .unwrap_or(false);
    let mut response = json!({
        "skill_id": skill_id,
        "client_id": normalized,
        "client_name": client_name(&normalized),
        "target_root": raw.get("target_root").cloned().unwrap_or(Value::Null),
        "target_source": raw.get("target_source").cloned().unwrap_or(Value::Null),
        "target_configured": raw.get("target_configured").cloned().unwrap_or(Value::Bool(false)),
        "removed": { "skill_id": skill_id, "removed": removed },
    });
    if let Some(object) = response.as_object_mut() {
        if let Some(state) = raw.get("state").or_else(|| removal.get("state")) {
            object.insert("state".to_string(), state.clone());
        }
        if let Some(profile) = raw
            .get("managing_profile")
            .or_else(|| removal.get("managing_profile"))
        {
            object.insert("managing_profile".to_string(), profile.clone());
        }
        if let Some(reason) = raw.get("reason").or_else(|| removal.get("reason")) {
            object.insert("reason".to_string(), reason.clone());
        }
    }
    Ok(response)
}

fn ensure_skill_client_unregister_allowed(
    store: &SkillStore,
    skill_id: &str,
) -> Result<(), Box<dyn Error>> {
    if let Some(policy) = store.management_policy(skill_id)? {
        if policy.management != "user_managed" && !policy.allow_uninstall {
            return Err("由组织管理的 AI 技能不能取消客户端同步".into());
        }
    }
    Ok(())
}

fn client_name(client_id: &str) -> &'static str {
    if client_id == "himind-ai" {
        return "HiMind AI";
    }
    if client_id == "codex" {
        return "Codex";
    }
    clients::directory_client(client_id)
        .map(|definition| definition.name)
        .unwrap_or("AI 工具")
}

pub(crate) fn uninstall_supported_clients_for_policy_json(
    skill_id: &str,
) -> Result<Value, Box<dyn Error>> {
    uninstall_supported_clients_impl(skill_id, true)
}

fn uninstall_supported_clients_impl(
    skill_id: &str,
    policy_override: bool,
) -> Result<Value, Box<dyn Error>> {
    let store = SkillStore::new();
    store.bootstrap_builtin_skills()?;
    if !policy_override {
        if let Some(policy) = store.management_policy(skill_id)? {
            if policy.management != "user_managed" && !policy.allow_uninstall {
                return Err("由组织管理的 AI 技能不能自行卸载".into());
            }
        }
    }
    let record = store
        .get_record(skill_id)?
        .ok_or_else(|| format!("Skill not found: {skill_id}"))?;
    let mut clients = BTreeMap::new();
    for normalized in uninstall_client_ids(&record) {
        if clients.contains_key(&normalized) {
            continue;
        }
        let removed = match normalized.as_str() {
            "codex" => codex::uninstall_json(skill_id)?,
            "himind-ai" => json!({
                "client_id": "himind-ai",
                "target_root": store.root().to_string_lossy().to_string(),
                "target_source": "agent-skill-store",
                "target_configured": true,
                "removed": {
                    "skill_id": skill_id,
                    "removed": false,
                },
            }),
            _ => direct::uninstall_for_client(&normalized, skill_id)?,
        };
        clients.insert(normalized, removed);
    }
    Ok(json!({"skill_id": skill_id, "clients": clients}))
}

fn declared_client_ids(record: &SkillRecord) -> Vec<String> {
    record
        .manifest
        .supported_clients
        .iter()
        .map(|client| client.trim().to_ascii_lowercase())
        .filter(|client| !client.is_empty() && client != PORTABLE_PROFILE_ID)
        .collect()
}

fn sync_client_ids(record: &SkillRecord) -> Vec<String> {
    active_client_ids_for_manifest(&record.manifest)
}

/// 显式点名投放目标时的客户端集合。
///
/// 与默认投放不同：默认只投"本机已经装了/已指向"的工具，用户点名则按清单声明
/// 的支持范围投放，即使这台机器还没装那个客户端——写的是它的用户级技能目录，
/// 客户端装好即可用。声明不支持的客户端不会静默写入，调用方用
/// [`unsupported_target_clients`] 在计划面提前告知。
fn sync_client_ids_for_targets(record: &SkillRecord, targets: &[String]) -> Vec<String> {
    requested_client_ids(&record.manifest.supported_clients, targets)
}

/// 用户点名投放目标时的客户端集合：只保留清单声明支持的客户端，`himind-ai`
/// 始终包含。计划面用的是同一函数，计划与执行不会分叉。
pub(crate) fn requested_client_ids(
    supported_clients: &[String],
    requested: &[String],
) -> Vec<String> {
    let mut clients = requested
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty() && value != PORTABLE_PROFILE_ID)
        .filter(|client_id| clients::supported_clients_include(supported_clients, client_id))
        .collect::<Vec<_>>();
    clients.push("himind-ai".to_string());
    clients.sort();
    clients.dedup();
    clients
}

/// 用户点名了、但清单声明不支持的客户端，按原样（去重、保留顺序）返回。
///
/// 计划面用它给出"这几个目标投不进去"的警告，而不是等安装完才发现少了几个目录。
pub(crate) fn unsupported_target_clients(
    supported_clients: &[String],
    targets: &[String],
) -> Vec<String> {
    let mut unsupported = Vec::new();
    for client_id in targets {
        let client_id = client_id.trim().to_ascii_lowercase();
        if client_id.is_empty() || client_id == PORTABLE_PROFILE_ID {
            continue;
        }
        if clients::supported_clients_include(supported_clients, &client_id)
            || client_id == "himind-ai"
        {
            continue;
        }
        if !unsupported.contains(&client_id) {
            unsupported.push(client_id);
        }
    }
    unsupported
}

/// 是否把这次渲染投放到该客户端。
///
/// `himind-ai` 永远保留：它读的是技能库本体，如果把技能排除在它之外，就会出现
/// "装了但本产品自己用不了"的怪状态。
fn client_is_selected(
    client_id: &str,
    selected: Option<&std::collections::BTreeSet<String>>,
) -> bool {
    match selected {
        None => true,
        Some(selected) => client_id == "himind-ai" || selected.contains(client_id),
    }
}

pub(crate) fn active_client_ids_for_manifest(manifest: &SkillManifest) -> Vec<String> {
    active_client_ids_for_supported(&manifest.supported_clients)
}

/// 同一套投放规则，入参放宽到 `supported_clients` 声明。
///
/// 安装计划（dry-run）拿到的是目录项而不是本地 `SkillRecord`，但"这次会写到
/// 哪些客户端"必须与真正安装时完全一致；把规则收敛到这里，避免计划面与执行面
/// 各写一份、日后走偏。
pub(crate) fn active_client_ids_for_supported(supported_clients: &[String]) -> Vec<String> {
    let active_directory_clients = direct::active_client_ids();
    let mut clients = supported_clients
        .iter()
        .map(|client| client.trim().to_ascii_lowercase())
        .filter(|client| !client.is_empty() && client != PORTABLE_PROFILE_ID)
        .filter(|client_id| {
            clients::directory_client(client_id).is_none()
                || active_directory_clients.contains(&client_id.as_str())
        })
        .collect::<Vec<_>>();
    if clients::declares_portable(supported_clients) {
        clients.push("himind-ai".to_string());
        if codex::is_detected() {
            clients.push("codex".to_string());
        }
        clients.extend(active_directory_clients.into_iter().map(str::to_string));
    }
    clients.sort();
    clients.dedup();
    clients
}

fn uninstall_client_ids(record: &SkillRecord) -> Vec<String> {
    let mut clients = declared_client_ids(record);
    // 卸载只认落过盘的副本。可移植技能默认投放全部已探测工具，但用户也可以
    // 点名投到清单未逐条声明的客户端；照声明推断会漏删那些目录，照全部
    // 客户端枚举又会误删别人的文件。台账是唯一准确来源。
    if let Ok(deployments) = crate::skill::target::deployments_for_skill(&record.manifest.id) {
        clients.extend(
            deployments
                .into_iter()
                .map(|deployment| deployment.client_id),
        );
    }
    // 技能库本体永远是投放面的一部分。
    clients.push("himind-ai".to_string());
    clients.sort();
    clients.dedup();
    clients
}

pub(crate) fn uninstall_client_ids_for_record(record: &SkillRecord) -> Vec<String> {
    uninstall_client_ids(record)
}

pub(crate) fn capability_facts_from_gateway(
    options: &crate::Options,
    worker_status: Arc<Mutex<LocalWorkerStatus>>,
    context: &InvocationContext,
) -> Result<Vec<CapabilityFact>, Box<dyn Error>> {
    let gateway = CapabilityGateway::new(options.clone(), worker_status);
    let descriptors = gateway.list_capabilities(context)?;
    Ok(descriptors
        .into_iter()
        .map(|descriptor| CapabilityFact {
            id: descriptor.id,
            version: descriptor.version,
            source: descriptor.source,
        })
        .collect())
}

pub(crate) fn records_json(records: &[SkillRecord]) -> Value {
    json!({
        "items": records,
        "total": records.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::store::SkillManagementPolicy;
    use std::fs;

    #[test]
    fn install_targets_filter_clients_but_always_keep_himind_ai() {
        use std::collections::BTreeSet;
        let selected: BTreeSet<String> = ["codex".to_string(), "claude".to_string()]
            .into_iter()
            .collect();
        // 未指定目标 = 默认投放全部
        assert!(client_is_selected("cursor", None));
        // 指定目标后只投放选中的工具
        assert!(client_is_selected("codex", Some(&selected)));
        assert!(client_is_selected("claude", Some(&selected)));
        assert!(!client_is_selected("cursor", Some(&selected)));
        // 本产品自己的 agent 永远保留，否则会出现"装了却用不了"
        assert!(client_is_selected("himind-ai", Some(&selected)));
        assert!(client_is_selected("himind-ai", Some(&BTreeSet::new())));
    }

    #[test]
    fn organization_policy_can_block_client_unregister() {
        let root = std::env::temp_dir().join(format!(
            "himind-skill-unregister-policy-{}",
            std::process::id()
        ));
        let store = SkillStore::with_root(root.clone());
        let skill_id = "com.himind.skill.managed";
        let skill_root = root.join("managed").join(skill_id);
        fs::create_dir_all(&skill_root).unwrap();
        store
            .apply_management_policy(
                skill_id,
                &SkillManagementPolicy {
                    management: "organization_managed".to_string(),
                    source: "organization".to_string(),
                    assignment_id: "assignment-1".to_string(),
                    reason: "required".to_string(),
                    allow_uninstall: false,
                },
            )
            .unwrap();

        let error = ensure_skill_client_unregister_allowed(&store, skill_id).unwrap_err();
        assert!(error.to_string().contains("不能取消客户端同步"));
        let _ = fs::remove_dir_all(root);
    }
}
