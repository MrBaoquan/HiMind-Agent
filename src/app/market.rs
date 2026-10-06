//! 市场能力面：把「我想要的能力」和「我已经拥有的能力」放进同一份数据。
//!
//! AI 干活时会缺东西，人也会。两边需要的是同一组答案：市场上有没有、装上会发生
//! 什么、装完落在哪。所以搜索、计划、安装三条链路共用同一批目录解析——搜得到的
//! 一定能计划出来，计划里写的落点一定就是执行时写入的落点。
//!
//! 这里刻意不引入第二套安装实现：技能走 `skill_manager` / `extension_source`，
//! 插件走 `plugin_manager` / `extension_source`，工作流走 `workflow_manager` /
//! `extension_source`。市场只是把"我该调哪一条"这件事收敛掉。

use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::distribution::{
    ExpertCatalogItem, InstructionPackCatalogItem, PluginCatalogItem, SkillCatalogItem,
    WorkflowCatalogItem,
};
use crate::app::operation_plan::{OperationPlan, PlanDependency};
use crate::app::plugin_manager::PluginDependencyAction;
use crate::capability::types::InvocationSource;
use crate::{Options, VERSION};

pub(crate) const KIND_SKILL: &str = "skill";
pub(crate) const KIND_PLUGIN: &str = "plugin";
pub(crate) const KIND_WORKFLOW: &str = "workflow";
pub(crate) const KIND_INSTRUCTION_PACK: &str = "instruction_pack";
pub(crate) const KIND_EXPERT: &str = "expert";

const ALL_KINDS: [&str; 5] = [
    KIND_SKILL,
    KIND_PLUGIN,
    KIND_WORKFLOW,
    KIND_INSTRUCTION_PACK,
    KIND_EXPERT,
];
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;

/// 市场里的一条能力。
///
/// 「市场有没有」和「我有没有」在同一行里给出：调用方不需要读完目录再回本地查一遍。
/// `installed_version` 为空即未安装；`update_available` 是两者比较后的结论。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MarketItem {
    pub(crate) kind: String,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) description: String,
    pub(crate) author: String,
    pub(crate) categories: Vec<String>,
    pub(crate) source: String,
    pub(crate) channel: String,
    pub(crate) artifact_id: String,
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
    pub(crate) supported_clients: Vec<String>,
    pub(crate) capability_ids: Vec<String>,
    pub(crate) assignment: String,
    pub(crate) management: String,
    pub(crate) installed: bool,
    pub(crate) installed_version: String,
    pub(crate) update_available: bool,
    /// 开发直挂的草稿，不是"我拥有的能力"。
    pub(crate) development: bool,
}

/// 一条"缺了所以跑不起来"的依赖，用来从市场反查能补上它的条目。
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub(crate) struct MissingDependency {
    /// `skill` / `plugin` / `workflow` / `capability` / `runtime` / `connector`
    pub(crate) kind: String,
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) required: bool,
    #[serde(default)]
    pub(crate) reason: String,
}

/// 三个目录的原始条目。计划与安装直接用它，避免把目录项降级成摘要后再拼回去。
#[derive(Default)]
struct Catalog {
    skills: Vec<SkillCatalogItem>,
    plugins: Vec<PluginCatalogItem>,
    workflows: Vec<WorkflowCatalogItem>,
    instruction_packs: Vec<InstructionPackCatalogItem>,
    experts: Vec<ExpertCatalogItem>,
    errors: Vec<String>,
}

fn text(input: &Value, key: &str) -> String {
    input
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn optional_text(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn bool_field(input: &Value, key: &str) -> bool {
    input.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn number_field(input: &Value, key: &str) -> Option<usize> {
    input
        .get(key)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
}

fn normalized_kind(input: &Value) -> Result<Option<String>, Box<dyn Error>> {
    let kind = text(input, "kind");
    if kind.is_empty() || kind == "all" {
        return Ok(None);
    }
    if !ALL_KINDS.contains(&kind.as_str()) {
        return Err(format!(
            "能力类型必须是 skill、plugin、workflow、instruction_pack 或 expert，收到: {kind}"
        )
        .into());
    }
    Ok(Some(kind))
}

/// 能力来源是本地目录或 GitHub 扩展源（`local:` / `github:` 前缀）。
///
/// 与 Tauri 命令用的是同一个判据：显式给了公共来源，或者目录里这个 id 只存在于
/// 公共来源下。少了这一条，"搜到的是扩展源的版本、装的却是工作台的版本"就会悄悄发生。
fn public_source_id(source: Option<&str>) -> Option<&str> {
    source.and_then(|value| {
        value
            .strip_prefix("local:")
            .or_else(|| value.strip_prefix("github:"))
    })
}

fn source_id_of(source: &str) -> Option<&str> {
    source
        .strip_prefix("local:")
        .or_else(|| source.strip_prefix("github:"))
}

fn load_catalog(options: &Options, agent_id: &str) -> Catalog {
    let mut errors = Vec::new();
    let (skills, skill_errors) = crate::app::commands::merged_skill_catalog_for(options, agent_id);
    errors.extend(skill_errors);
    let (plugins, plugin_errors) =
        crate::app::commands::merged_plugin_catalog_for(options, agent_id);
    errors.extend(plugin_errors);
    let (workflows, workflow_error) = match crate::app::commands::merged_workflow_catalog(options) {
        Ok(value) => value,
        Err(error) => (Vec::new(), error),
    };
    if !workflow_error.trim().is_empty() {
        errors.push(workflow_error);
    }
    let instruction_packs = if unauthorized(options, agent_id) {
        Vec::new()
    } else {
        match Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|error| error.to_string())
            .and_then(|client| {
                crate::api::distribution::instruction_pack_catalog(
                    &client,
                    &options.api_base(),
                    agent_id,
                    &options.agent_credential(),
                )
                .map_err(|error| error.to_string())
            }) {
            Ok(items) => items,
            Err(error) => {
                errors.push(format!("InstructionPack 目录读取失败: {error}"));
                Vec::new()
            }
        }
    };
    let experts = if unauthorized(options, agent_id) {
        Vec::new()
    } else {
        Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .ok()
            .and_then(|client| {
                crate::api::distribution::expert_catalog(
                    &client,
                    &options.api_base(),
                    agent_id,
                    &options.agent_credential(),
                )
                .ok()
            })
            .unwrap_or_default()
    };
    errors.sort();
    errors.dedup();
    Catalog {
        skills,
        plugins,
        workflows,
        instruction_packs,
        experts,
        errors,
    }
}

fn choose_version<T>(
    items: Vec<T>,
    version: Option<&str>,
    version_of: impl Fn(&T) -> &str,
) -> Option<T> {
    if let Some(version) = version.filter(|value| !value.trim().is_empty()) {
        return items
            .into_iter()
            .find(|item| version_of(item).eq_ignore_ascii_case(version));
    }
    items.into_iter().max_by(|left, right| {
        crate::skill::resolver::compare_versions(version_of(left), version_of(right))
    })
}

impl Catalog {
    fn market_items(&self) -> Vec<MarketItem> {
        let mut items = Vec::new();
        for item in &self.skills {
            items.push(MarketItem {
                kind: KIND_SKILL.to_string(),
                id: item.skill_id.clone(),
                name: item.name.clone(),
                version: item.version.clone(),
                description: item.description.clone(),
                author: item.author_name.clone(),
                categories: item.categories.clone(),
                source: item.source.clone(),
                channel: item.channel.clone(),
                artifact_id: item.artifact_id.clone(),
                sha256: item.sha256.clone(),
                size_bytes: item.file_size,
                supported_clients: item.supported_clients.clone(),
                capability_ids: item.capability_ids.clone(),
                assignment: item.assignment.clone(),
                management: item.management.clone(),
                installed: false,
                installed_version: String::new(),
                update_available: false,
                development: false,
            });
        }
        for item in &self.plugins {
            items.push(MarketItem {
                kind: KIND_PLUGIN.to_string(),
                id: item.plugin_id.clone(),
                name: item.name.clone(),
                version: item.version.clone(),
                description: item.description.clone(),
                author: item.author_name.clone(),
                categories: item.categories.clone(),
                source: item.source.clone(),
                channel: item.channel.clone(),
                artifact_id: item.artifact_id.clone(),
                sha256: item.sha256.clone(),
                size_bytes: item.file_size,
                supported_clients: Vec::new(),
                capability_ids: item.capability_ids.clone(),
                assignment: item.assignment.clone(),
                management: item.management.clone(),
                installed: false,
                installed_version: String::new(),
                update_available: false,
                development: false,
            });
        }
        for item in &self.workflows {
            items.push(MarketItem {
                kind: KIND_WORKFLOW.to_string(),
                id: item.workflow_id.clone(),
                name: item.name.clone(),
                version: item.version.clone(),
                description: item.description.clone(),
                author: item.author_name.clone(),
                categories: item.categories.clone(),
                source: item.source.clone(),
                channel: item.channel.clone(),
                artifact_id: item.artifact_id.clone(),
                sha256: item.sha256.clone(),
                size_bytes: item.file_size,
                supported_clients: Vec::new(),
                capability_ids: item.capability_ids.clone(),
                assignment: item.assignment.clone(),
                management: item.management.clone(),
                installed: false,
                installed_version: String::new(),
                update_available: false,
                development: false,
            });
        }
        for item in &self.instruction_packs {
            items.push(MarketItem {
                kind: KIND_INSTRUCTION_PACK.to_string(),
                id: item.instruction_pack_id.clone(),
                name: item.name.clone(),
                version: item.version.clone(),
                description: item.description.clone(),
                author: item.author_name.clone(),
                categories: item.categories.clone(),
                source: item.source.clone(),
                channel: item.channel.clone(),
                artifact_id: item.artifact_id.clone(),
                sha256: item.sha256.clone(),
                size_bytes: item.file_size,
                supported_clients: item.supported_clients.clone(),
                capability_ids: Vec::new(),
                assignment: item.assignment.clone(),
                management: item.management.clone(),
                installed: false,
                installed_version: String::new(),
                update_available: false,
                development: false,
            });
        }
        for item in &self.experts {
            items.push(MarketItem {
                kind: KIND_EXPERT.to_string(),
                id: item.expert_id.clone(),
                name: item.name.clone(),
                version: item.version.clone(),
                description: item.description.clone(),
                author: item.author_name.clone(),
                categories: item.categories.clone(),
                source: item.source.clone(),
                channel: String::new(),
                artifact_id: item.artifact_id.clone(),
                sha256: item.sha256.clone(),
                size_bytes: item.file_size,
                supported_clients: item.supported_clients.clone(),
                capability_ids: Vec::new(),
                assignment: item.assignment.clone(),
                management: item.management.clone(),
                installed: false,
                installed_version: String::new(),
                update_available: false,
                development: false,
            });
        }
        items
    }

    /// 同一个 id 可能同时存在于工作台与扩展源，`source` 用来把"搜到的那一条"
    /// 和"要装的那一条"钉在一起；不传时才按版本挑最新的。
    fn skill(
        &self,
        id: &str,
        version: Option<&str>,
        source: Option<&str>,
    ) -> Option<SkillCatalogItem> {
        choose_version(
            self.skills
                .iter()
                .filter(|item| {
                    item.skill_id == id && source.is_none_or(|value| item.source == value)
                })
                .cloned()
                .collect(),
            version,
            |item| item.version.as_str(),
        )
    }

    fn plugin(
        &self,
        id: &str,
        version: Option<&str>,
        source: Option<&str>,
    ) -> Option<PluginCatalogItem> {
        choose_version(
            self.plugins
                .iter()
                .filter(|item| {
                    item.plugin_id == id && source.is_none_or(|value| item.source == value)
                })
                .cloned()
                .collect(),
            version,
            |item| item.version.as_str(),
        )
    }

    fn workflow(
        &self,
        id: &str,
        version: Option<&str>,
        source: Option<&str>,
    ) -> Option<WorkflowCatalogItem> {
        choose_version(
            self.workflows
                .iter()
                .filter(|item| {
                    item.workflow_id == id && source.is_none_or(|value| item.source == value)
                })
                .cloned()
                .collect(),
            version,
            |item| item.version.as_str(),
        )
    }

    fn expert(&self, id: &str, version: Option<&str>) -> Option<ExpertCatalogItem> {
        choose_version(
            self.experts
                .iter()
                .filter(|item| item.expert_id == id)
                .cloned()
                .collect(),
            version,
            |item| item.version.as_str(),
        )
    }

    fn instruction_pack(
        &self,
        id: &str,
        version: Option<&str>,
    ) -> Option<InstructionPackCatalogItem> {
        choose_version(
            self.instruction_packs
                .iter()
                .filter(|item| item.instruction_pack_id == id)
                .cloned()
                .collect(),
            version,
            |item| item.version.as_str(),
        )
    }
}

/// 「我已经拥有什么」的本地索引。
///
/// 技能用**已安装记录**，插件用插件登记表，工作流用工作流库：三者都是各自的权威
/// 来源，市场不去猜。开发直挂单独记一笔——它是"我正在做的"，不是"我拥有的"。
#[derive(Default)]
struct InstalledIndex {
    skills: HashMap<String, String>,
    development_skills: HashMap<String, String>,
    plugins: HashMap<String, String>,
    development_plugins: HashMap<String, String>,
    workflows: HashMap<String, String>,
    instruction_packs: HashMap<String, String>,
    experts: HashMap<String, String>,
}

impl InstalledIndex {
    fn load() -> Self {
        let mut index = Self::default();
        let store = crate::skill::store::SkillStore::new();
        if let Ok(records) = store.list_records() {
            for record in records {
                let id = record.manifest.id.clone();
                let version = record.manifest.version.clone();
                if crate::skill::development::record(&id).is_some() {
                    index.development_skills.insert(id, version);
                } else {
                    index.skills.insert(id, version);
                }
            }
        }
        if let Ok(items) = crate::capability::plugin::scan_plugins() {
            for item in items {
                if item.status == "uninstalled" {
                    continue;
                }
                if item.development {
                    index.development_plugins.insert(item.id, item.version);
                } else {
                    index.plugins.insert(item.id, item.version);
                }
            }
        }
        if let Ok(store) = crate::workflow::WorkflowStore::open_default() {
            if let Ok((items, _)) = store.list_with_issues() {
                for item in items {
                    index
                        .workflows
                        .insert(item.package.id.clone(), item.package.version.clone());
                }
            }
        }
        if let Ok(drafts) = crate::instruction_pack::list() {
            for draft in drafts {
                let id = draft.manifest.id.clone();
                let version = draft.manifest.version.clone();
                let current = index
                    .instruction_packs
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();
                if current.is_empty()
                    || crate::skill::resolver::compare_versions(&version, &current)
                        == Ordering::Greater
                {
                    index.instruction_packs.insert(id, version);
                }
            }
        }
        if let Ok(experts) = crate::expert::list() {
            for expert in experts.into_iter().filter(|item| !item.builtin) {
                let current = index.experts.get(&expert.id).cloned().unwrap_or_default();
                if current.is_empty()
                    || crate::skill::resolver::compare_versions(&expert.version, &current)
                        == Ordering::Greater
                {
                    index.experts.insert(expert.id, expert.version);
                }
            }
        }
        if let Ok(experts) = crate::expert::list() {
            for expert in experts {
                index.experts.insert(expert.id, expert.version);
            }
        }
        index
    }

    fn version_of(&self, kind: &str, id: &str) -> String {
        let bucket = match kind {
            KIND_SKILL => &self.skills,
            KIND_PLUGIN => &self.plugins,
            KIND_WORKFLOW => &self.workflows,
            KIND_INSTRUCTION_PACK => &self.instruction_packs,
            KIND_EXPERT => &self.experts,
            _ => return String::new(),
        };
        bucket.get(id).cloned().unwrap_or_default()
    }

    fn is_development(&self, kind: &str, id: &str) -> bool {
        match kind {
            KIND_SKILL => self.development_skills.contains_key(id),
            KIND_PLUGIN => self.development_plugins.contains_key(id),
            _ => false,
        }
    }

    fn annotate(&self, items: &mut [MarketItem]) {
        for item in items.iter_mut() {
            let installed_version = self.version_of(&item.kind, &item.id);
            item.installed_version = installed_version.clone();
            item.installed = !installed_version.is_empty();
            item.update_available = item.installed
                && crate::skill::resolver::compare_versions(&item.version, &installed_version)
                    == Ordering::Greater;
            item.development = self.is_development(&item.kind, &item.id);
        }
    }
}

fn matches_query(item: &MarketItem, query: &str) -> bool {
    let haystack = [
        item.id.as_str(),
        item.name.as_str(),
        item.description.as_str(),
        item.author.as_str(),
        &item.categories.join(" "),
        &item.capability_ids.join(" "),
    ]
    .join(" ")
    .to_ascii_lowercase();
    haystack.contains(query)
}

/// 市场搜索：按类型、关键词、分类过滤后分页。
///
/// 分页之后才查询本地安装状态，所以"搜一页"不会变成"扫一遍全库"。
pub(crate) fn search(
    options: &Options,
    agent_id: &str,
    input: &Value,
) -> Result<Value, Box<dyn Error>> {
    let kind = normalized_kind(input)?;
    let query = text(input, "query").to_ascii_lowercase();
    let category = text(input, "category");
    let limit = number_field(input, "limit")
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);
    let cursor = number_field(input, "cursor").unwrap_or(0);

    let catalog = load_catalog(options, agent_id);
    let mut items = catalog.market_items();
    if let Some(kind) = kind.as_deref() {
        items.retain(|item| item.kind == kind);
    }
    if !query.is_empty() {
        items.retain(|item| matches_query(item, &query));
    }
    if !category.is_empty() && !category.eq_ignore_ascii_case("all") {
        items.retain(|item| {
            item.categories
                .iter()
                .any(|value| value.eq_ignore_ascii_case(&category))
        });
    }

    let total = items.len();
    let start = cursor.min(total);
    let end = start.saturating_add(limit).min(total);
    let mut page = items[start..end].to_vec();
    InstalledIndex::load().annotate(&mut page);

    Ok(json!({
        "items": page,
        "total": total,
        "cursor": start,
        "next_cursor": if end < total { Value::from(end) } else { Value::Null },
        "categories": collect_categories(&items),
        // 目录不全时结果就不完整，这件事必须一起返回，而不是让调用方以为"就这些"。
        "errors": catalog.errors,
    }))
}

fn collect_categories(items: &[MarketItem]) -> Vec<String> {
    let mut categories = items
        .iter()
        .flat_map(|item| item.categories.iter().cloned())
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>();
    categories.sort();
    categories.dedup();
    categories
}

/// 「我已经拥有的能力」：技能带着落点，插件带着运行态，工作流带着启停与来源。
///
/// 技能的落点来自投放台账而不是"当前配置"：一个技能可能同时装在全局和两个项目里，
/// 只看当前工作区会把另外两份说没了。
pub(crate) fn installed(input: &Value) -> Result<Value, Box<dyn Error>> {
    let kind = normalized_kind(input)?;
    let want = |candidate: &str| kind.as_deref().is_none_or(|value| value == candidate);

    let deployments = crate::skill::target::all_deployments().unwrap_or_default();
    let mut deployments_by_skill: HashMap<String, Vec<crate::skill::target::SkillDeployment>> =
        HashMap::new();
    for deployment in deployments {
        deployments_by_skill
            .entry(deployment.skill_id.clone())
            .or_default()
            .push(deployment);
    }

    let mut skills = Vec::new();
    if want(KIND_SKILL) {
        let store = crate::skill::store::SkillStore::new();
        let records = store.list_records().unwrap_or_default();
        for record in records {
            let id = record.manifest.id.clone();
            let development = crate::skill::development::record(&id).is_some();
            let locations = deployments_by_skill
                .remove(&id)
                .unwrap_or_default()
                .into_iter()
                .map(|deployment| {
                    json!({
                        "client": deployment.client_id,
                        "scope": deployment.target_kind,
                        "workspace_root": deployment.workspace_root,
                        "path": crate::skill::target::display_path(
                            std::path::Path::new(&deployment.rendered_root)
                        ),
                        "strategy": deployment.strategy,
                        "source": deployment.source,
                        "updated_at": deployment.updated_at,
                    })
                })
                .collect::<Vec<_>>();
            let policy = store
                .management_policy(&id)
                .ok()
                .flatten()
                .map(|policy| json!(policy))
                .unwrap_or(Value::Null);
            skills.push(json!({
                "id": id,
                "name": record.manifest.name,
                "description": record.manifest.description,
                "version": record.manifest.version,
                "previous_version": record.previous_version,
                "categories": record.manifest.categories,
                "supported_clients": record.manifest.supported_clients,
                "path": crate::skill::target::display_path(&record.root),
                "scope": format!("{:?}", record.manifest.scope).to_ascii_lowercase(),
                "development": development,
                "locations": locations,
                "management_policy": policy,
            }));
        }
        skills.sort_by(|left, right| {
            left.get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .cmp(right.get("id").and_then(Value::as_str).unwrap_or_default())
        });
    }

    let mut plugins = Vec::new();
    if want(KIND_PLUGIN) {
        for item in crate::capability::plugin::scan_plugins().unwrap_or_default() {
            plugins.push(json!({
                "id": item.id,
                "name": item.name,
                "description": item.description,
                "version": item.version,
                "previous_version": item.previous_version,
                "status": item.status,
                "enabled": item.enabled,
                "development": item.development,
                "source": item.source,
                "runtime": item.runtime,
                "path": crate::skill::target::display_path(std::path::Path::new(&item.path)),
                "capabilities": item.capabilities,
                "permissions": item.permissions,
                "plugin_dependencies": item.plugin_dependencies,
                "commands": item.commands,
                "error": item.error,
            }));
        }
    }

    let mut workflows = Vec::new();
    let mut issues = Vec::new();
    if want(KIND_WORKFLOW) {
        let store = crate::workflow::WorkflowStore::open_default()?;
        let (installed, load_issues) = store.list_with_issues()?;
        for item in installed {
            workflows.push(json!({
                "id": item.package.id,
                "name": item.package.name,
                "description": item.package.description,
                "version": item.package.version,
                "previous_version": item.previous_version,
                "enabled": item.enabled,
                "source": item.source,
                "installed_at": item.installed_at,
                "updated_at": item.updated_at,
                "package_digest": item.package_digest,
                "lock_required": item.extension_lock.is_some(),
            }));
        }
        for issue in load_issues {
            issues.push(json!(issue));
        }
    }

    let mut instruction_packs = Vec::new();
    if want(KIND_INSTRUCTION_PACK) {
        for draft in crate::instruction_pack::list().unwrap_or_default() {
            instruction_packs.push(json!({
                "id": draft.manifest.id,
                "name": draft.manifest.name,
                "description": draft.manifest.description,
                "version": draft.manifest.version,
                "source": draft.source,
                "scope": draft.manifest.scope,
                "supported_clients": draft.manifest.supported_clients,
                "tested_at": draft.tested_at,
                "confirmed_at": draft.confirmed_at,
                "published_at": draft.published_at,
                "readiness": if draft.published_at.is_some() { "published_local" } else if draft.confirmed_at.is_some() { "confirmed" } else if draft.tested_at.is_some() { "tested" } else { "draft" },
                "projection_required": draft.published_at.is_some(),
            }));
        }
    }
    let mut experts = Vec::new();
    if want(KIND_EXPERT) {
        experts = crate::expert::list()?.into_iter().filter(|item| !item.builtin).map(|item| json!({"id":item.id,"name":item.name,"description":item.description,"version":item.version,"categories":item.categories,"supported_clients":item.supported_clients,"source":"marketplace"})).collect();
    }

    Ok(json!({
        "skills": skills,
        "plugins": plugins,
        "workflows": workflows,
        "instruction_packs": instruction_packs,
        "experts": experts,
        "issues": issues,
    }))
}

/// 安装计划：只回答"会发生什么"，不写任何目录。
///
/// 计划里的落点与执行时写入的落点来自同一段代码（技能走 `with_skill_location`，
/// 插件与工作流只有一个本机落点），所以计划不会承诺一个执行时不存在的位置。
pub(crate) fn plan(
    options: &Options,
    agent_id: &str,
    input: &Value,
) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::to_value(plan_operation(
        options, agent_id, input,
    )?)?)
}

/// 安装：先算计划，再按计划执行。
///
/// 执行前重新算一遍计划有代价，但换来一条硬约束：`ready == false` 的计划绝不会
/// 被执行。AI 看到的"会发生什么"和真正发生的事因此不会分叉——包括授权、制品摘要
/// 变化、组织禁用这些在执行瞬间才可能出现的结论。
pub(crate) fn install(
    options: &Options,
    agent_id: &str,
    input: &Value,
    source: InvocationSource,
) -> Result<Value, Box<dyn Error>> {
    let plan = plan_operation(options, agent_id, input)?;
    if !plan.ready {
        return Err(format!(
            "安装计划未就绪，已取消执行：{}",
            plan.blocked_reasons.join("；")
        )
        .into());
    }
    if bool_field(input, "dry_run") {
        return Ok(json!({
            "kind": plan.capability,
            "id": plan.item.id,
            "version": plan.item.version,
            "dry_run": true,
            "plan": plan,
            "installed": Value::Null,
        }));
    }
    let kind = plan.capability.clone();
    let item = plan.item.clone();
    let installed = match kind.as_str() {
        KIND_SKILL => install_skill(options, agent_id, input, &item, source)?,
        KIND_PLUGIN => install_plugin(options, agent_id, input, &item)?,
        KIND_WORKFLOW => install_workflow(options, input, &item)?,
        KIND_INSTRUCTION_PACK => install_instruction_pack(options, agent_id, input, &item)?,
        KIND_EXPERT => install_expert(options, agent_id, input, &item)?,
        other => return Err(format!("未知的能力类型: {other}").into()),
    };
    // 装完能力就变了：不刷新目录的话，客户端 tools/list 里看不到刚装上的工具，
    // 只能靠下次重启——那正是"装完还要重启一次"这类体感的来源。
    crate::capability::service::invalidate_capability_discovery();
    Ok(json!({
        "kind": kind,
        "id": item.id,
        "version": item.version,
        "source": item.source,
        "dry_run": false,
        "plan": plan,
        "installed": installed,
    }))
}

fn plan_operation(
    options: &Options,
    agent_id: &str,
    input: &Value,
) -> Result<OperationPlan, Box<dyn Error>> {
    let kind = normalized_kind(input)?
        .ok_or("安装计划必须指定 kind：skill、plugin、workflow、instruction_pack 或 expert")?;
    let id = text(input, "id");
    if id.is_empty() {
        return Err("安装计划必须指定 id".into());
    }
    let version = optional_text(input, "version");
    let source = optional_text(input, "source");
    let artifact_id = optional_text(input, "artifact_id");
    let sha256 = optional_text(input, "sha256");
    let catalog = load_catalog(options, agent_id);

    match kind.as_str() {
        KIND_SKILL => {
            let targets = target_clients(input);
            let location = optional_text(input, "workspace_root");
            crate::app::commands::with_skill_location(location.as_deref(), || {
                let item = resolve_skill(&catalog, &id, version.as_deref(), source.as_deref())?;
                skill_plan(
                    options,
                    agent_id,
                    &item,
                    artifact_id.as_deref(),
                    sha256.as_deref(),
                    targets.as_deref(),
                )
                .map_err(|error| error.to_string())
            })
            .map_err(|error| -> Box<dyn Error> { error.into() })
        }
        KIND_PLUGIN => {
            let item = resolve_plugin(&catalog, &id, version.as_deref(), source.as_deref())?;
            plugin_plan(
                options,
                agent_id,
                &item,
                artifact_id.as_deref(),
                sha256.as_deref(),
            )
        }
        KIND_WORKFLOW => {
            let item = resolve_workflow(&catalog, &id, version.as_deref(), source.as_deref())?;
            workflow_plan(
                options,
                agent_id,
                &item,
                artifact_id.as_deref(),
                sha256.as_deref(),
            )
        }
        KIND_INSTRUCTION_PACK => {
            let item = catalog
                .instruction_pack(&id, version.as_deref())
                .ok_or_else(|| format!("市场中未找到 InstructionPack: {id}"))?;
            instruction_pack_plan(
                options,
                agent_id,
                &item,
                artifact_id.as_deref(),
                sha256.as_deref(),
            )
        }
        KIND_EXPERT => {
            let item = catalog
                .expert(&id, version.as_deref())
                .ok_or_else(|| format!("市场中未找到专家: {id}"))?;
            expert_plan(
                options,
                agent_id,
                &item,
                artifact_id.as_deref(),
                sha256.as_deref(),
            )
        }
        other => Err(format!("未知的能力类型: {other}").into()),
    }
}

fn target_clients(input: &Value) -> Option<Vec<String>> {
    let values = input.get("target_clients").and_then(Value::as_array)?;
    let clients = values
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if clients.is_empty() {
        None
    } else {
        Some(clients)
    }
}

/// 是不是走公共扩展源（local / GitHub），而不是工作台目录。
///
/// 判据与 Tauri 命令一致：显式给了公共来源前缀，或者用户没指定来源而这个 id 在
/// 公共来源里存在。否则一律按工作台目录处理——包括"工作台没授权"这种情况，
/// 它应该报"需要 HiMind 账号"，而不是静默变成一条空计划。
fn uses_public_source(source: Option<&str>, has_public: bool) -> bool {
    public_source_id(source).is_some() || (source.is_none() && has_public)
}

/// 工作台目录需要授权才能读。
///
/// 这里不抛错：未授权是"缺一步"而不是"这条能力不存在"，计划里要能直接读到
/// 原因，AI 才有机会把"去授权"讲清楚，而不是把失败翻译成"市场没有"。
fn unauthorized(options: &Options, agent_id: &str) -> bool {
    agent_id.trim().is_empty() || options.agent_credential().trim().is_empty()
}

fn resolve_skill(
    catalog: &Catalog,
    id: &str,
    version: Option<&str>,
    source: Option<&str>,
) -> Result<SkillCatalogItem, String> {
    let has_public = catalog
        .skills
        .iter()
        .any(|item| item.skill_id == id && source_id_of(&item.source).is_some());
    if uses_public_source(source, has_public) {
        // 解析只读：计划阶段绝不能顺手把东西装上。
        let plan = crate::app::extension_source::plan_skill_bound(
            id,
            version,
            public_source_id(source),
            None,
        )
        .map_err(|error| error.to_string())?;
        return Ok(plan.skill);
    }
    catalog
        .skill(id, version, source)
        .ok_or_else(|| format!("市场中未找到技能: {id}"))
}

fn resolve_plugin(
    catalog: &Catalog,
    id: &str,
    version: Option<&str>,
    source: Option<&str>,
) -> Result<PluginCatalogItem, String> {
    let has_public = catalog
        .plugins
        .iter()
        .any(|item| item.plugin_id == id && source_id_of(&item.source).is_some());
    if uses_public_source(source, has_public) {
        let plan = crate::app::extension_source::plan_plugin_bound(
            id,
            version,
            public_source_id(source),
            None,
        )
        .map_err(|error| error.to_string())?;
        return Ok(plan.plugin);
    }
    catalog
        .plugin(id, version, source)
        .ok_or_else(|| format!("市场中未找到插件: {id}"))
}

fn resolve_workflow(
    catalog: &Catalog,
    id: &str,
    version: Option<&str>,
    source: Option<&str>,
) -> Result<WorkflowCatalogItem, String> {
    let has_public = catalog
        .workflows
        .iter()
        .any(|item| item.workflow_id == id && source_id_of(&item.source).is_some());
    if uses_public_source(source, has_public) {
        return crate::app::extension_source::plan_workflow(id, version)
            .map_err(|error| error.to_string());
    }
    catalog
        .workflow(id, version, source)
        .ok_or_else(|| format!("市场中未找到 Workflow: {id}"))
}

fn skill_dependencies(actions: &[PluginDependencyAction]) -> Vec<PlanDependency> {
    actions
        .iter()
        .map(|action| PlanDependency {
            kind: KIND_PLUGIN.to_string(),
            id: action.plugin_id.clone(),
            name: action.plugin_name.clone(),
            required: action.required,
            current_version: action.current_version.clone(),
            target_version: action.target_version.clone(),
            action: action.action.clone(),
            reason: action.reason.clone(),
        })
        .collect()
}

/// 技能安装计划：扩展源走扩展源计划器，工作台走工作台计划器。
///
/// 两条路都返回同一份 [`OperationPlan`]，调用方不需要知道这个技能从哪来。
fn skill_plan(
    options: &Options,
    agent_id: &str,
    item: &SkillCatalogItem,
    artifact_id: Option<&str>,
    sha256: Option<&str>,
    targets: Option<&[String]>,
) -> Result<OperationPlan, Box<dyn Error>> {
    if let Some(source_id) = source_id_of(&item.source) {
        let plan = crate::app::extension_source::plan_skill_bound(
            &item.skill_id,
            Some(&item.version),
            Some(source_id),
            sha256,
        )?;
        return Ok(crate::app::operation_plan::skill_install_with_targets(
            &plan.skill,
            plan.blocked_reasons,
            skill_dependencies(&plan.plugin_actions),
            targets,
        ));
    }
    if unauthorized(options, agent_id) {
        return Ok(crate::app::operation_plan::skill_install_with_targets(
            item,
            vec!["HiMind 账号尚未授权，无法读取工作台技能".to_string()],
            Vec::new(),
            targets,
        ));
    }
    let plan = crate::app::skill_manager::plan_install_bound(
        options,
        agent_id,
        &item.skill_id,
        Some(&item.version),
        artifact_id,
        sha256,
    )?;
    Ok(crate::app::operation_plan::skill_install_with_targets(
        &plan.skill,
        plan.blocked_reasons,
        skill_dependencies(&plan.plugin_actions),
        targets,
    ))
}

fn plugin_plan(
    options: &Options,
    agent_id: &str,
    item: &PluginCatalogItem,
    artifact_id: Option<&str>,
    sha256: Option<&str>,
) -> Result<OperationPlan, Box<dyn Error>> {
    if let Some(source_id) = source_id_of(&item.source) {
        let plan = crate::app::extension_source::plan_plugin_bound(
            &item.plugin_id,
            Some(&item.version),
            Some(source_id),
            sha256,
        )?;
        return Ok(crate::app::operation_plan::plugin_install(
            &plan.plugin,
            plan.blocked_reasons,
            skill_dependencies(&plan.dependency_actions),
        ));
    }
    if unauthorized(options, agent_id) {
        return Ok(crate::app::operation_plan::plugin_install(
            item,
            vec!["HiMind 账号尚未授权，无法读取工作台插件".to_string()],
            Vec::new(),
        ));
    }
    let plan = crate::app::plugin_manager::plan_install_bound(
        options,
        agent_id,
        &item.plugin_id,
        Some(&item.version),
        artifact_id,
        sha256,
    )?;
    Ok(crate::app::operation_plan::plugin_install(
        &plan.plugin,
        plan.blocked_reasons,
        skill_dependencies(&plan.dependency_actions),
    ))
}

/// 工作流安装计划。
///
/// 工作流的运行期依赖写在扩展锁里（能力、连接器、运行时），这里把它翻成计划里
/// 的依赖项，让"装完还缺一个运行时"在执行前就可见。
fn workflow_plan(
    options: &Options,
    agent_id: &str,
    item: &WorkflowCatalogItem,
    artifact_id: Option<&str>,
    sha256: Option<&str>,
) -> Result<OperationPlan, Box<dyn Error>> {
    let mut blocked = Vec::new();
    if source_id_of(&item.source).is_none() {
        if unauthorized(options, agent_id) {
            blocked.push("HiMind 账号尚未授权，无法安装工作台 Workflow".to_string());
        } else if item.assignment == "blocked" {
            blocked.push("该 Workflow 已被组织禁止安装".to_string());
        }
    }
    if let Err(error) = crate::app::extension_lock::verify_catalog_artifact(
        "Workflow",
        &item.artifact_id,
        &item.sha256,
        artifact_id,
        sha256,
    ) {
        blocked.push(error.to_string());
    }
    if !item.min_agent_version.trim().is_empty()
        && crate::skill::resolver::compare_versions(VERSION, &item.min_agent_version)
            == Ordering::Less
    {
        blocked.push(format!(
            "当前 Agent {VERSION} 不满足该 Workflow 的最低版本 {}",
            item.min_agent_version
        ));
    }
    Ok(crate::app::operation_plan::workflow_install(
        item,
        blocked,
        workflow_dependencies(item),
    ))
}

fn instruction_pack_plan(
    options: &Options,
    agent_id: &str,
    item: &InstructionPackCatalogItem,
    expected_artifact_id: Option<&str>,
    expected_sha256: Option<&str>,
) -> Result<OperationPlan, Box<dyn Error>> {
    let mut blocked = Vec::new();
    if unauthorized(options, agent_id) {
        blocked.push("HiMind 账号尚未授权，无法安装工作台 InstructionPack".to_string());
    }
    if item.assignment == "blocked" {
        blocked.push("该 InstructionPack 已被组织禁止安装".to_string());
    }
    if item.managed {
        blocked.push("该 InstructionPack 由组织管理，不能从个人市场安装".to_string());
    }
    if !item.min_agent_version.trim().is_empty()
        && crate::skill::resolver::compare_versions(VERSION, &item.min_agent_version)
            == Ordering::Less
    {
        blocked.push(format!(
            "当前 Agent {VERSION} 不满足该 InstructionPack 的最低版本 {}",
            item.min_agent_version
        ));
    }
    if expected_artifact_id.is_some_and(|value| value != item.artifact_id) {
        blocked.push("InstructionPack 制品已变化，请重新读取市场目录".to_string());
    }
    if expected_sha256.is_some_and(|value| !value.eq_ignore_ascii_case(&item.sha256)) {
        blocked.push("InstructionPack 摘要已变化，请重新读取市场目录".to_string());
    }
    Ok(crate::app::operation_plan::instruction_pack_install(
        item, blocked,
    ))
}

fn expert_plan(
    options: &Options,
    agent_id: &str,
    item: &ExpertCatalogItem,
    expected_artifact_id: Option<&str>,
    expected_sha256: Option<&str>,
) -> Result<OperationPlan, Box<dyn Error>> {
    let mut blocked = Vec::new();
    if unauthorized(options, agent_id) {
        blocked.push("HiMind 账号尚未授权，无法安装工作台专家".to_string());
    }
    if item.assignment == "blocked" {
        blocked.push("该专家已被组织禁止安装".to_string());
    }
    if expected_artifact_id.is_some_and(|value| value != item.artifact_id) {
        blocked.push("专家制品已变化，请重新读取市场目录".to_string());
    }
    if expected_sha256.is_some_and(|value| !value.eq_ignore_ascii_case(&item.sha256)) {
        blocked.push("专家摘要已变化，请重新读取市场目录".to_string());
    }
    Ok(crate::app::operation_plan::expert_install(item, blocked))
}

fn workflow_dependencies(item: &WorkflowCatalogItem) -> Vec<PlanDependency> {
    let Some(lock) = item.extension_lock.clone() else {
        return Vec::new();
    };
    let Ok(lock) = serde_json::from_value::<crate::extension_contracts::ExtensionLock>(lock) else {
        return Vec::new();
    };
    lock.dependencies
        .iter()
        .map(|dependency| PlanDependency {
            kind: dependency.kind.as_str().to_string(),
            id: dependency.id.clone(),
            name: dependency.id.clone(),
            required: dependency.required,
            current_version: String::new(),
            target_version: dependency.version.clone(),
            action: "resolve".to_string(),
            reason: "Workflow 扩展锁依赖".to_string(),
        })
        .collect()
}

fn install_skill(
    options: &Options,
    agent_id: &str,
    input: &Value,
    item: &crate::app::operation_plan::PlanItem,
    source: InvocationSource,
) -> Result<Value, Box<dyn Error>> {
    let targets = target_clients(input);
    let location = optional_text(input, "workspace_root");
    let requested_version = Some(item.version.clone());
    let artifact_id = optional_text(input, "artifact_id");
    let sha256 = optional_text(input, "sha256");
    let record = crate::app::commands::with_skill_location(location.as_deref(), || {
        if let Some(source_id) = source_id_of(&item.source) {
            let (_, record) = crate::app::extension_source::install_skill_bound(
                &item.id,
                requested_version.as_deref(),
                Some(source_id),
                sha256.as_deref(),
            )
            .map_err(|error| error.to_string())?;
            return Ok(record);
        }
        let (_, record) = crate::app::skill_manager::install_with_dependencies_bound(
            options,
            agent_id,
            &item.id,
            requested_version.as_deref(),
            &[],
            artifact_id.as_deref(),
            sha256.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        Ok(record)
    })
    .map_err(|error| -> Box<dyn Error> { error.into() })?;
    let capability_facts = crate::skill::capability_facts_from_gateway(
        options,
        std::sync::Arc::new(std::sync::Mutex::new(
            crate::store::types::LocalWorkerStatus::default(),
        )),
        &crate::capability::types::InvocationContext::new(source, "market-install"),
    )?;
    let clients = crate::skill::sync_record_to_clients(
        &record,
        VERSION,
        &capability_facts,
        targets.as_deref(),
    )?;
    let _ = crate::app::extension_source::reconcile_dsh_presets_now();
    Ok(json!({
        "record": record,
        "clients": clients,
    }))
}

fn install_plugin(
    options: &Options,
    agent_id: &str,
    input: &Value,
    item: &crate::app::operation_plan::PlanItem,
) -> Result<Value, Box<dyn Error>> {
    let version = Some(item.version.clone());
    let artifact_id = optional_text(input, "artifact_id");
    let sha256 = optional_text(input, "sha256");
    if let Some(source_id) = source_id_of(&item.source) {
        crate::app::extension_source::install_plugin_bound(
            &item.id,
            version.as_deref(),
            Some(source_id),
            sha256.as_deref(),
        )?;
    } else {
        crate::app::plugin_manager::install_bound(
            options,
            agent_id,
            &item.id,
            version.as_deref(),
            artifact_id.as_deref(),
            sha256.as_deref(),
        )?;
    }
    let status = crate::app::plugin_manager::local_status(&item.id);
    Ok(json!({
        "current_version": status.current_version,
        "previous_version": status.previous_version,
        "enabled": status.enabled,
        "status": status.status,
    }))
}

fn install_workflow(
    options: &Options,
    input: &Value,
    item: &crate::app::operation_plan::PlanItem,
) -> Result<Value, Box<dyn Error>> {
    let version = Some(item.version.clone());
    let artifact_id = optional_text(input, "artifact_id");
    let sha256 = optional_text(input, "sha256");
    if let Some(source_id) = source_id_of(&item.source) {
        let (_, installed) = crate::app::extension_source::install_workflow_bound(
            &item.id,
            version.as_deref(),
            Some(source_id),
            sha256.as_deref(),
        )?;
        return Ok(serde_json::to_value(installed)?);
    }
    let installed = crate::app::workflow_manager::install_dashboard_catalog_workflow_bound(
        options,
        &item.id,
        version.as_deref(),
        artifact_id.as_deref(),
        sha256.as_deref(),
    )?;
    Ok(serde_json::to_value(installed)?)
}

fn install_instruction_pack(
    options: &Options,
    agent_id: &str,
    input: &Value,
    item: &crate::app::operation_plan::PlanItem,
) -> Result<Value, Box<dyn Error>> {
    let catalog = load_catalog(options, agent_id);
    let remote = catalog
        .instruction_pack(&item.id, Some(&item.version))
        .ok_or_else(|| format!("市场中未找到 InstructionPack: {}", item.id))?;
    if remote.source != "marketplace" && !remote.source.is_empty() {
        return Err("InstructionPack 当前只支持 Dashboard 市场来源".into());
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let source = download_instruction_pack(&client, options, agent_id, &remote)?;
    let draft = crate::instruction_pack::import_package(
        crate::instruction_pack::InstructionPackImportInput {
            package_path: source.clone(),
            source: "marketplace".to_string(),
        },
    )?;
    let tested = crate::instruction_pack::test(&draft.manifest.id, &draft.manifest.version)?;
    let _ = fs::remove_file(source);
    Ok(json!({
        "draft": tested.draft,
        "readiness": tested.readiness,
        "issues": tested.issues,
        "requires_confirmation": true,
        "published_local": false,
        "projection_required": true,
        "confirmed": false,
        "input": input,
    }))
}

fn install_expert(
    options: &Options,
    agent_id: &str,
    _input: &Value,
    item: &crate::app::operation_plan::PlanItem,
) -> Result<Value, Box<dyn Error>> {
    let catalog = load_catalog(options, agent_id);
    let remote = catalog
        .expert(&item.id, Some(&item.version))
        .ok_or_else(|| format!("市场中未找到专家: {}", item.id))?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let source = download_expert(&client, options, agent_id, &remote)?;
    let summary = crate::expert::import_package(&source)?;
    let _ = fs::remove_file(source);
    Ok(json!({"expert": summary, "projection_required": false, "activated": false}))
}

fn download_expert(
    client: &Client,
    options: &Options,
    agent_id: &str,
    item: &ExpertCatalogItem,
) -> Result<PathBuf, Box<dyn Error>> {
    const MAX_BYTES: u64 = 16 * 1024 * 1024;
    if item.file_size == 0 || item.file_size > MAX_BYTES {
        return Err("专家制品大小无效或超过 16 MiB 限制".into());
    }
    let api = url::Url::parse(&options.api_base())?;
    let url = url::Url::parse(&item.download_url)?;
    if api.scheme() != url.scheme()
        || api.host_str() != url.host_str()
        || api.port_or_known_default() != url.port_or_known_default()
    {
        return Err("专家制品下载地址必须与 Dashboard 同源".into());
    }
    let mut response = client
        .get(url)
        .header(
            "Authorization",
            format!("Agent {agent_id}:{}", options.agent_credential()),
        )
        .send()?
        .error_for_status()?;
    let path = std::env::temp_dir().join(format!("himind-expert-{}.hmexpert", unique_suffix()));
    let mut file = File::create(&path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_BYTES || total > item.file_size {
            let _ = fs::remove_file(&path);
            return Err("专家制品实际大小超过发布记录".into());
        }
        file.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    file.flush()?;
    if total != item.file_size {
        let _ = fs::remove_file(&path);
        return Err("专家制品实际大小与发布记录不一致".into());
    }
    if !format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(&item.sha256) {
        let _ = fs::remove_file(&path);
        return Err("专家制品 SHA-256 校验失败".into());
    }
    crate::app::system::verify_extension_artifact_signature(
        &path,
        &item.signature,
        &item.signature_key_id,
        &item.signature_algorithm,
        true,
    )?;
    Ok(path)
}

fn download_instruction_pack(
    client: &Client,
    options: &Options,
    agent_id: &str,
    item: &InstructionPackCatalogItem,
) -> Result<PathBuf, Box<dyn Error>> {
    const MAX_BYTES: u64 = 16 * 1024 * 1024;
    if item.file_size == 0 || item.file_size > MAX_BYTES {
        return Err("InstructionPack 制品大小无效或超过 16 MiB 限制".into());
    }
    let api = url::Url::parse(&options.api_base())?;
    let url = url::Url::parse(&item.download_url)?;
    if api.scheme() != url.scheme()
        || api.host_str() != url.host_str()
        || api.port_or_known_default() != url.port_or_known_default()
    {
        return Err("InstructionPack 制品下载地址必须与 Dashboard 同源".into());
    }
    let mut response = client
        .get(url)
        .header(
            "Authorization",
            format!("Agent {agent_id}:{}", options.agent_credential()),
        )
        .send()?
        .error_for_status()?;
    let path = std::env::temp_dir().join(format!(
        "himind-instruction-pack-{}.hminstruction",
        unique_suffix()
    ));
    let mut file = File::create(&path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_BYTES || total > item.file_size {
            let _ = fs::remove_file(&path);
            return Err("InstructionPack 制品实际大小超过发布记录".into());
        }
        file.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    file.flush()?;
    if total != item.file_size {
        let _ = fs::remove_file(&path);
        return Err("InstructionPack 制品实际大小与发布记录不一致".into());
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(&item.sha256) {
        let _ = fs::remove_file(&path);
        return Err("InstructionPack 制品 SHA-256 校验失败".into());
    }
    crate::app::system::verify_extension_artifact_signature(
        &path,
        &item.signature,
        &item.signature_key_id,
        &item.signature_algorithm,
        true,
    )?;
    Ok(path)
}

fn unique_suffix() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos().to_string())
        .unwrap_or_else(|_| "0".to_string())
}
