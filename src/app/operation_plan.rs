//! 统一的"这次操作会做什么"计划面（dry-run）。
//!
//! 安装、更新、发布三条链路各自都已经有 planner，但形状不同：技能计划里是
//! `skill` + `plugin_actions`，插件计划里是 `plugin` + `dependency_actions`，
//! 发布预览则是一坨自由形状的 JSON。结果是每个界面都要自己解释一遍"会发生
//! 什么"，同一个概念（写到哪里、用什么策略、有没有阻断）被讲了多套说法。
//!
//! 这里把三者收敛成同一份 [`OperationPlan`]：目标（targets）、策略
//! （strategy）、步骤（steps）、依赖（dependencies）、阻断（blocked_reasons）、
//! 就绪（ready）。前端只需要渲染一种计划卡；后端也只需要维护一种语义。

use serde::{Deserialize, Serialize};

pub(crate) const PLAN_SCHEMA_VERSION: &str = "capability-operation-plan.v1";

pub(crate) const OPERATION_INSTALL: &str = "install";
pub(crate) const OPERATION_PUBLISH: &str = "publish";

pub(crate) const CAPABILITY_SKILL: &str = "skill";
pub(crate) const CAPABILITY_PLUGIN: &str = "plugin";
pub(crate) const CAPABILITY_WORKFLOW: &str = "workflow";
pub(crate) const CAPABILITY_INSTRUCTION_PACK: &str = "instruction_pack";
pub(crate) const CAPABILITY_EXPERT: &str = "expert";

/// 写入 Agent 本机能力库（技能库 / 插件目录），没有额外投影目标。
pub(crate) const STRATEGY_STORE: &str = "store";
/// 解包成 `current/` + `previous/`，用于插件这类可回滚的本机安装。
pub(crate) const STRATEGY_EXTRACT: &str = "extract";
/// 发布到远端仓库（GitHub Release）。
pub(crate) const STRATEGY_RELEASE: &str = "release";
/// 提交到工作台审核。
pub(crate) const STRATEGY_SUBMIT: &str = "submit";

const TARGET_KIND_AGENT: &str = "agent";
const TARGET_KIND_CLIENT: &str = "client";
const TARGET_KIND_GITHUB: &str = "github";
const TARGET_KIND_WORKBENCH: &str = "workbench";
const TARGET_KIND_ORGANIZATION: &str = "organization";

const SCOPE_AGENT: &str = "agent";
const SCOPE_USER: &str = "user";
const SCOPE_PROJECT: &str = "project";
const SCOPE_REMOTE: &str = "remote";

/// 计划里的一个落点。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PlanTarget {
    /// `agent` / `client` / `organization` / `github` ...
    pub(crate) kind: String,
    /// 稳定标识：客户端 ID、工作台分发 ID、`github` 等。
    pub(crate) id: String,
    pub(crate) label: String,
    /// 解析后的真实落点：目录路径、`owner/repo`、目录项 ID。
    pub(crate) destination: String,
    /// `agent` / `user` / `project` / `organization` / `remote`
    pub(crate) scope: String,
    /// `store` / `copy` / `symlink` / `extract` / `release` / `submit`
    pub(crate) strategy: String,
    /// 该落点当前是否已存在/已授权。false 表示这次操作会新建它。
    pub(crate) detected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PlanStep {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) detail: String,
    /// 是否会改变本机或远端状态。只读步骤让"确认"这件事变得可判断。
    pub(crate) mutating: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct PlanDependency {
    pub(crate) kind: String,
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: String,
    pub(crate) required: bool,
    #[serde(default)]
    pub(crate) current_version: String,
    #[serde(default)]
    pub(crate) target_version: String,
    /// `install` / `update` / `keep`
    #[serde(default)]
    pub(crate) action: String,
    #[serde(default)]
    pub(crate) reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PlanItem {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) source: String,
    #[serde(default)]
    pub(crate) artifact_id: String,
    #[serde(default)]
    pub(crate) sha256: String,
    #[serde(default)]
    pub(crate) size_bytes: u64,
}

/// 一次安装 / 发布操作的完整计划。
///
/// `ready == false` 时执行面必须拒绝；阻断原因在 `blocked_reasons` 里逐条给出，
/// 让界面可以直接照读，而不是自己猜哪一步不能做。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct OperationPlan {
    pub(crate) schema_version: String,
    /// `install` / `publish`
    pub(crate) operation: String,
    /// `skill` / `plugin`
    pub(crate) capability: String,
    pub(crate) item: PlanItem,
    pub(crate) targets: Vec<PlanTarget>,
    pub(crate) dependencies: Vec<PlanDependency>,
    pub(crate) steps: Vec<PlanStep>,
    pub(crate) blocked_reasons: Vec<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) ready: bool,
}

impl OperationPlan {
    fn new(operation: &str, capability: &str, item: PlanItem) -> Self {
        Self {
            schema_version: PLAN_SCHEMA_VERSION.to_string(),
            operation: operation.to_string(),
            capability: capability.to_string(),
            item,
            targets: Vec::new(),
            dependencies: Vec::new(),
            steps: Vec::new(),
            blocked_reasons: Vec::new(),
            warnings: Vec::new(),
            ready: true,
        }
    }

    fn finish(mut self) -> Self {
        self.blocked_reasons.sort();
        self.blocked_reasons.dedup();
        self.ready = self.blocked_reasons.is_empty();
        self
    }
}

/// 技能安装计划：Agent 技能库 + 这台机器上真正会被投影到的客户端目录。
pub(crate) fn skill_install(
    item: &crate::api::distribution::SkillCatalogItem,
    blocked_reasons: Vec<String>,
    dependencies: Vec<PlanDependency>,
) -> OperationPlan {
    skill_install_with_targets(item, blocked_reasons, dependencies, None)
}

/// 技能安装计划，可指定投放目标。
///
/// `target_clients` 为 `None` 表示沿用默认投放（本机已探测到的客户端）；为
/// `Some` 时按用户点名的客户端逐个给落点，即使这台机器还没装那个客户端——写的
/// 是它的用户级技能目录。清单未声明支持的客户端不会出现在计划里，由调用方给出
/// 警告，避免"计划里承诺了、执行时静默跳过"。
pub(crate) fn skill_install_with_targets(
    item: &crate::api::distribution::SkillCatalogItem,
    blocked_reasons: Vec<String>,
    dependencies: Vec<PlanDependency>,
    target_clients: Option<&[String]>,
) -> OperationPlan {
    let mut plan = OperationPlan::new(
        OPERATION_INSTALL,
        CAPABILITY_SKILL,
        PlanItem {
            id: item.skill_id.clone(),
            name: item.name.clone(),
            version: item.version.clone(),
            description: item.description.clone(),
            source: item.source.clone(),
            artifact_id: item.artifact_id.clone(),
            sha256: item.sha256.clone(),
            size_bytes: item.file_size,
        },
    );
    plan.targets = skill_targets_with(&item.supported_clients, target_clients);
    plan.dependencies = dependencies;

    let client_targets = plan
        .targets
        .iter()
        .filter(|target| target.kind == TARGET_KIND_CLIENT)
        .count();
    plan.steps.push(PlanStep {
        id: "resolve".to_string(),
        title: "校验来源与摘要".to_string(),
        detail: format!(
            "锁定 {} {} 的制品并按签名与 SHA-256 校验后才会落盘",
            item.name, item.version
        ),
        mutating: false,
    });
    plan.steps.push(PlanStep {
        id: "store".to_string(),
        title: "写入 Agent 技能库".to_string(),
        detail: "技能包进入本机技能库，HiMind AI 立即可用".to_string(),
        mutating: true,
    });
    if !plan.dependencies.is_empty() {
        let pending = plan
            .dependencies
            .iter()
            .filter(|dependency| dependency.action != "keep")
            .count();
        plan.steps.push(PlanStep {
            id: "dependencies".to_string(),
            title: "处理依赖插件".to_string(),
            detail: format!("本次会安装或更新 {pending} 个依赖插件"),
            mutating: pending > 0,
        });
    }
    if client_targets > 0 {
        plan.steps.push(PlanStep {
            id: "project".to_string(),
            title: "投影到客户端目录".to_string(),
            detail: format!("按各客户端的落盘策略写入 {client_targets} 个目录"),
            mutating: true,
        });
    }
    plan.steps.push(PlanStep {
        id: "ledger".to_string(),
        title: "登记安装台账".to_string(),
        detail: "记录落点、策略与内容摘要，供后续更新与卸载使用".to_string(),
        mutating: true,
    });
    if item.sha256.trim().is_empty() {
        plan.warnings
            .push("该制品没有提供 SHA-256，无法在安装前比对内容".to_string());
    }
    plan.blocked_reasons = blocked_reasons;
    plan.finish()
}

/// 插件安装计划：插件只落在 Agent 本机插件目录，客户端通过 MCP 调用。
pub(crate) fn plugin_install(
    item: &crate::api::distribution::PluginCatalogItem,
    blocked_reasons: Vec<String>,
    dependencies: Vec<PlanDependency>,
) -> OperationPlan {
    let mut plan = OperationPlan::new(
        OPERATION_INSTALL,
        CAPABILITY_PLUGIN,
        PlanItem {
            id: item.plugin_id.clone(),
            name: item.name.clone(),
            version: item.version.clone(),
            description: item.description.clone(),
            source: item.source.clone(),
            artifact_id: item.artifact_id.clone(),
            sha256: item.sha256.clone(),
            size_bytes: item.file_size,
        },
    );
    plan.targets.push(PlanTarget {
        kind: TARGET_KIND_AGENT.to_string(),
        id: "himind-plugins".to_string(),
        label: "Agent 插件目录".to_string(),
        destination: crate::skill::target::display_path(
            &crate::capability::plugin::plugin_registry_dir(),
        ),
        scope: SCOPE_AGENT.to_string(),
        strategy: STRATEGY_EXTRACT.to_string(),
        detected: true,
    });
    plan.dependencies = dependencies;
    plan.steps.push(PlanStep {
        id: "resolve".to_string(),
        title: "校验来源与摘要".to_string(),
        detail: format!(
            "锁定 {} {} 的制品并按签名与 SHA-256 校验后才会落盘",
            item.name, item.version
        ),
        mutating: false,
    });
    plan.steps.push(PlanStep {
        id: "extract".to_string(),
        title: "解包到插件目录".to_string(),
        detail: "保留上一版本目录，便于回退".to_string(),
        mutating: true,
    });
    if !plan.dependencies.is_empty() {
        let pending = plan
            .dependencies
            .iter()
            .filter(|dependency| dependency.action != "keep")
            .count();
        plan.steps.push(PlanStep {
            id: "dependencies".to_string(),
            title: "处理依赖插件".to_string(),
            detail: format!("本次会安装或更新 {pending} 个依赖插件"),
            mutating: pending > 0,
        });
    }
    plan.steps.push(PlanStep {
        id: "handshake".to_string(),
        title: "刷新能力登记".to_string(),
        detail: "重新登记 MCP 工具与能力贡献，客户端下次调用即可生效".to_string(),
        mutating: true,
    });
    plan.blocked_reasons = blocked_reasons;
    plan.finish()
}

/// 工作流安装计划：工作流只落在 Agent 工作流库，运行期依赖由扩展锁声明。
///
/// 与技能 / 插件不同，工作流没有客户端投影：它由 Agent 自己执行，客户端只是
/// 通过 `workflow.run.start` 触发。因此这里只给一个落点，把"要不要装"和"能不能
/// 装"讲清楚，不制造不存在的目标。
pub(crate) fn workflow_install(
    item: &crate::api::distribution::WorkflowCatalogItem,
    blocked_reasons: Vec<String>,
    dependencies: Vec<PlanDependency>,
) -> OperationPlan {
    let mut plan = OperationPlan::new(
        OPERATION_INSTALL,
        CAPABILITY_WORKFLOW,
        PlanItem {
            id: item.workflow_id.clone(),
            name: item.name.clone(),
            version: item.version.clone(),
            description: item.description.clone(),
            source: item.source.clone(),
            artifact_id: item.artifact_id.clone(),
            sha256: item.sha256.clone(),
            size_bytes: item.file_size,
        },
    );
    plan.targets.push(PlanTarget {
        kind: TARGET_KIND_AGENT.to_string(),
        id: "himind-workflows".to_string(),
        label: "Agent 工作流库".to_string(),
        destination: crate::skill::target::display_path(
            &crate::store::paths::agent_home().join("workflows"),
        ),
        scope: SCOPE_AGENT.to_string(),
        strategy: STRATEGY_EXTRACT.to_string(),
        detected: true,
    });
    plan.dependencies = dependencies;
    plan.steps.push(PlanStep {
        id: "resolve".to_string(),
        title: "校验来源与摘要".to_string(),
        detail: format!(
            "锁定 {} {} 的制品并按签名与 SHA-256 校验后才会落盘",
            item.name, item.version
        ),
        mutating: false,
    });
    plan.steps.push(PlanStep {
        id: "extract".to_string(),
        title: "解包到工作流库".to_string(),
        detail: "保留上一版本目录，便于回退".to_string(),
        mutating: true,
    });
    let pending = plan
        .dependencies
        .iter()
        .filter(|dependency| dependency.action != "keep")
        .count();
    if pending > 0 {
        plan.steps.push(PlanStep {
            id: "dependencies".to_string(),
            title: "补齐依赖能力".to_string(),
            detail: format!("本次会安装或更新 {pending} 个依赖能力"),
            mutating: true,
        });
    }
    plan.steps.push(PlanStep {
        id: "preflight".to_string(),
        title: "登记启动前检查".to_string(),
        detail: "记录依赖锁与制品摘要，供启动前检查与自动更新使用".to_string(),
        mutating: true,
    });
    plan.blocked_reasons = blocked_reasons;
    plan.finish()
}

/// InstructionPack 安装只进入本机草稿库，随后必须经过预检和用户确认。
pub(crate) fn instruction_pack_install(
    item: &crate::api::distribution::InstructionPackCatalogItem,
    blocked_reasons: Vec<String>,
) -> OperationPlan {
    let mut plan = OperationPlan::new(
        OPERATION_INSTALL,
        CAPABILITY_INSTRUCTION_PACK,
        PlanItem {
            id: item.instruction_pack_id.clone(),
            name: item.name.clone(),
            version: item.version.clone(),
            description: item.description.clone(),
            source: item.source.clone(),
            artifact_id: item.artifact_id.clone(),
            sha256: item.sha256.clone(),
            size_bytes: item.file_size,
        },
    );
    plan.targets.push(PlanTarget {
        kind: TARGET_KIND_AGENT.to_string(),
        id: "himind-agent".to_string(),
        label: "HiMind Agent 指令包草稿库".to_string(),
        destination: "instruction-pack-drafts".to_string(),
        scope: SCOPE_AGENT.to_string(),
        strategy: STRATEGY_STORE.to_string(),
        detected: false,
    });
    plan.steps.push(PlanStep {
        id: "resolve".to_string(),
        title: "校验来源与摘要".to_string(),
        detail: format!(
            "下载 {} {} 并校验 SHA-256 与发布签名",
            item.name, item.version
        ),
        mutating: false,
    });
    plan.steps.push(PlanStep {
        id: "import".to_string(),
        title: "导入指令包草稿".to_string(),
        detail: "写入本机 InstructionPack 草稿库并执行包完整性校验".to_string(),
        mutating: true,
    });
    plan.steps.push(PlanStep {
        id: "confirm".to_string(),
        title: "等待用户确认".to_string(),
        detail: "市场安装不会自动发布或写入 AGENTS.md/CLAUDE.md；确认后才能发布到本机".to_string(),
        mutating: false,
    });
    plan.blocked_reasons = blocked_reasons;
    plan.finish()
}

pub(crate) fn expert_install(
    item: &crate::api::distribution::ExpertCatalogItem,
    blocked_reasons: Vec<String>,
) -> OperationPlan {
    let mut plan = OperationPlan::new(
        OPERATION_INSTALL,
        CAPABILITY_EXPERT,
        PlanItem {
            id: item.expert_id.clone(),
            name: item.name.clone(),
            version: item.version.clone(),
            description: item.description.clone(),
            source: item.source.clone(),
            artifact_id: item.artifact_id.clone(),
            sha256: item.sha256.clone(),
            size_bytes: item.file_size,
        },
    );
    plan.targets.push(PlanTarget {
        kind: TARGET_KIND_AGENT.to_string(),
        id: "himind-experts".to_string(),
        label: "HiMind Agent 专家库".to_string(),
        destination: "experts".to_string(),
        scope: SCOPE_AGENT.to_string(),
        strategy: STRATEGY_STORE.to_string(),
        detected: false,
    });
    plan.steps.push(PlanStep {
        id: "resolve".to_string(),
        title: "校验来源与摘要".to_string(),
        detail: format!(
            "下载 {} {} 并校验 SHA-256 与发布签名",
            item.name, item.version
        ),
        mutating: false,
    });
    plan.steps.push(PlanStep {
        id: "import".to_string(),
        title: "导入专家".to_string(),
        detail: "写入本机专家库，随后可在 AI 对话中选择".to_string(),
        mutating: true,
    });
    plan.blocked_reasons = blocked_reasons;
    plan.finish()
}

/// 发布计划：把发布预览里的生效目标翻译成同一份计划形状。
pub(crate) fn distribution_publish(preview: &serde_json::Value) -> OperationPlan {
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let github = preview.get("github").cloned().unwrap_or_default();
    let workbench = preview.get("workbench").cloned().unwrap_or_default();
    let targets = preview
        .get("targets")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut plan = OperationPlan::new(
        OPERATION_PUBLISH,
        &text(preview, "kind"),
        PlanItem {
            id: text(preview, "id"),
            name: text(preview, "name"),
            version: text(preview, "version"),
            description: String::new(),
            source: text(&github, "repository"),
            artifact_id: text(&github, "manifest_name"),
            sha256: text(&github, "sha256"),
            size_bytes: github
                .get("size_bytes")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
        },
    );

    // 预览里的依赖有两种形状：逐项明细（数组）与锁定情况摘要（对象）。
    // 两者都要能读，否则"计划里说没有依赖、发布时才发现没有 pin"就白做了。
    let dependencies = match github.get("dependencies") {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| {
                let pinned = item
                    .get("pinned")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                PlanDependency {
                    kind: text(item, "kind"),
                    id: text(item, "id"),
                    name: String::new(),
                    required: item
                        .get("required")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    current_version: text(item, "version"),
                    target_version: text(item, "version"),
                    action: if pinned { "keep" } else { "resolve" }.to_string(),
                    reason: if pinned {
                        "已锁定精确版本"
                    } else {
                        "只有最低版本，未锁定精确 pin"
                    }
                    .to_string(),
                }
            })
            .collect(),
        Some(serde_json::Value::Object(summary)) => summary
            .get("unpinned")
            .and_then(serde_json::Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(|id| PlanDependency {
                        kind: String::new(),
                        id: id.to_string(),
                        name: String::new(),
                        required: false,
                        current_version: String::new(),
                        target_version: String::new(),
                        action: "resolve".to_string(),
                        reason: "只有最低版本，未锁定精确 pin".to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    for target in &targets {
        match target.as_str() {
            "github" => plan.targets.push(PlanTarget {
                kind: TARGET_KIND_GITHUB.to_string(),
                id: "github".to_string(),
                label: "GitHub Release".to_string(),
                destination: text(&github, "repository"),
                scope: SCOPE_REMOTE.to_string(),
                strategy: STRATEGY_RELEASE.to_string(),
                detected: github
                    .get("authorized")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            }),
            "workbench" => plan.targets.push(PlanTarget {
                kind: TARGET_KIND_WORKBENCH.to_string(),
                id: "workbench".to_string(),
                label: "AI 工作台".to_string(),
                destination: text(&workbench, "catalog_id"),
                scope: TARGET_KIND_ORGANIZATION.to_string(),
                strategy: STRATEGY_SUBMIT.to_string(),
                detected: !text(&workbench, "distribution_id").is_empty(),
            }),
            _ => {}
        }
    }
    plan.dependencies = dependencies;

    plan.steps.push(PlanStep {
        id: "package".to_string(),
        title: "打包并计算摘要".to_string(),
        detail: format!(
            "生成 {} 并计算 SHA-256，用同一份制品供全部目标使用",
            text(&github, "asset_name")
        ),
        mutating: false,
    });
    if plan
        .targets
        .iter()
        .any(|target| target.kind == TARGET_KIND_GITHUB)
    {
        plan.steps.push(PlanStep {
            id: "github".to_string(),
            title: "发布 GitHub Release".to_string(),
            detail: format!(
                "在 {} 打 tag {} 并上传制品与分发清单",
                text(&github, "repository"),
                text(&github, "tag")
            ),
            mutating: true,
        });
    }
    if plan
        .targets
        .iter()
        .any(|target| target.kind == TARGET_KIND_WORKBENCH)
    {
        plan.steps.push(PlanStep {
            id: "workbench".to_string(),
            title: "提交工作台审核".to_string(),
            detail: format!(
                "带上 tag 与摘要提交目录项审核（渠道 {})",
                text(&workbench, "channel")
            ),
            mutating: true,
        });
    }
    plan.steps.push(PlanStep {
        id: "ledger".to_string(),
        title: "记录分发台账".to_string(),
        detail: "逐个目标记录结果，便于重试与追溯".to_string(),
        mutating: true,
    });

    if let Some(blocker) = github
        .get("dependency_blocker")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        plan.blocked_reasons.push(blocker.to_string());
    }
    if plan
        .targets
        .iter()
        .any(|target| target.kind == TARGET_KIND_GITHUB && !target.detected)
    {
        plan.blocked_reasons
            .push("GitHub 账号未授权，无法发布 Release".to_string());
    }
    if targets.is_empty() {
        plan.blocked_reasons
            .push("当前项目没有可用的分发目标".to_string());
    }
    plan.finish()
}

/// 技能会落到哪些客户端目录。
///
/// 与真实安装共用 [`crate::skill::active_client_ids_for_supported`] 与
/// [`crate::skill::direct::client_target`]，因此计划里列的落点和执行时写入的
/// 落点必然一致。
fn skill_targets_with(
    supported_clients: &[String],
    target_clients: Option<&[String]>,
) -> Vec<PlanTarget> {
    let store = crate::skill::store::SkillStore::new();
    let configured_sync_mode = store.sync_mode().unwrap_or_default();
    let mut targets = vec![PlanTarget {
        kind: TARGET_KIND_AGENT.to_string(),
        id: "himind-ai".to_string(),
        label: "HiMind AI 技能库".to_string(),
        destination: crate::skill::target::display_path(store.root()),
        scope: SCOPE_AGENT.to_string(),
        strategy: STRATEGY_STORE.to_string(),
        detected: true,
    }];
    let client_ids = match target_clients {
        Some(requested) => crate::skill::requested_client_ids(supported_clients, requested),
        None => crate::skill::active_client_ids_for_supported(supported_clients),
    };
    for client_id in client_ids {
        if client_id == "himind-ai" {
            continue;
        }
        if let Some((target, detected)) = crate::skill::direct::client_target(&client_id) {
            let strategy =
                crate::skill::target::effective_sync_mode(&configured_sync_mode, &target);
            targets.push(PlanTarget {
                kind: TARGET_KIND_CLIENT.to_string(),
                id: client_id.clone(),
                label: crate::skill::clients::directory_client(&client_id)
                    .map(|definition| definition.name.to_string())
                    .unwrap_or_else(|| client_id.clone()),
                destination: crate::skill::target::display_path(&target.root),
                scope: if target.is_workspace() {
                    SCOPE_PROJECT.to_string()
                } else {
                    SCOPE_USER.to_string()
                },
                strategy,
                detected,
            });
            continue;
        }
        if client_id == "codex" {
            if let Some(root) = crate::skill::codex::active_root() {
                let target = crate::skill::target::SkillTarget::global(root.clone(), "codex", true);
                targets.push(PlanTarget {
                    kind: TARGET_KIND_CLIENT.to_string(),
                    id: client_id,
                    label: "Codex".to_string(),
                    destination: crate::skill::target::display_path(&root),
                    scope: SCOPE_USER.to_string(),
                    strategy: crate::skill::target::effective_sync_mode(
                        &configured_sync_mode,
                        &target,
                    ),
                    detected: true,
                });
            }
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_item(supported_clients: &[&str]) -> crate::api::distribution::SkillCatalogItem {
        serde_json::from_value(serde_json::json!({
            "skill_id": "com.himind.skill.demo",
            "name": "Demo Skill",
            "description": "demo",
            "author_name": "himind",
            "categories": [],
            "version": "1.2.0",
            "release_notes": "",
            "min_agent_version": "0.1.0",
            "supported_clients": supported_clients,
            "capability_ids": [],
            "plugin_dependencies": [],
            "risk_summary": "",
            "channel": "stable",
            "artifact_id": "artifact-1",
            "file_name": "demo.hmskill",
            "file_size": 1024,
            "sha256": "sha256:abc",
            "signature": "",
            "signature_key_id": "",
            "signature_algorithm": "",
            "download_url": "https://example.invalid/demo.hmskill",
            "source": "organization",
            "assignment": "optional",
            "management": "user",
            "install_mode": "prompt"
        }))
        .unwrap()
    }

    #[test]
    fn skill_plan_lists_the_agent_store_and_every_projection() {
        let plan = skill_install(&skill_item(&["agent-skills"]), Vec::new(), Vec::new());
        assert!(plan.ready);
        assert_eq!(plan.schema_version, PLAN_SCHEMA_VERSION);
        assert_eq!(plan.operation, OPERATION_INSTALL);
        assert_eq!(plan.capability, CAPABILITY_SKILL);
        assert_eq!(plan.targets[0].id, "himind-ai");
        assert_eq!(plan.targets[0].strategy, STRATEGY_STORE);
        assert!(plan.steps.iter().any(|step| step.id == "store"));
        assert!(plan.steps.iter().any(|step| step.id == "ledger"));
        // 每个投影目标都必须带一个可执行的策略，而不是只列个目录名。
        for target in plan.targets.iter().skip(1) {
            assert_eq!(target.kind, TARGET_KIND_CLIENT);
            assert!(
                matches!(target.strategy.as_str(), "copy" | "symlink"),
                "unexpected strategy {}",
                target.strategy
            );
        }
    }

    #[test]
    fn blocked_reasons_decide_ready_instead_of_the_caller() {
        let plan = skill_install(
            &skill_item(&["agent-skills"]),
            vec!["需要更高版本的 Agent".to_string()],
            Vec::new(),
        );
        assert!(!plan.ready);
        assert_eq!(
            plan.blocked_reasons,
            vec!["需要更高版本的 Agent".to_string()]
        );
    }

    #[test]
    fn publish_plan_mirrors_the_preview_targets() {
        let preview = serde_json::json!({
            "kind": "plugin",
            "id": "com.himind.plugin.demo",
            "version": "1.0.0",
            "name": "Demo Plugin",
            "targets": ["github", "workbench"],
            "github": {
                "repository": "himind/apps",
                "branch": "main",
                "commit": "abc",
                "tag": "plugin-com.himind.plugin.demo-1.0.0",
                "asset_name": "demo.hmplugin",
                "manifest_name": "demo.manifest.json",
                "sha256": "sha256:abc",
                "size_bytes": 2048,
                "authorized": true,
                "signature": "enabled",
                "dependencies": { "total": 2, "pinned": 1, "unpinned": ["com.himind.plugin.dep"], "blocked": false },
                "dependency_blocker": null
            },
            "workbench": {
                "distribution_id": "dist-1",
                "channel": "stable",
                "catalog_id": "catalog-1"
            }
        });
        let plan = distribution_publish(&preview);
        assert!(plan.ready);
        assert_eq!(plan.operation, OPERATION_PUBLISH);
        assert_eq!(plan.targets.len(), 2);
        assert_eq!(plan.targets[0].kind, TARGET_KIND_GITHUB);
        assert_eq!(plan.targets[0].strategy, STRATEGY_RELEASE);
        assert_eq!(plan.targets[1].strategy, STRATEGY_SUBMIT);
        assert!(plan.steps.iter().any(|step| step.mutating));
        assert_eq!(plan.dependencies.len(), 1);
        assert_eq!(plan.dependencies[0].id, "com.himind.plugin.dep");
        assert_eq!(plan.dependencies[0].action, "resolve");
    }

    #[test]
    fn publish_plan_blocks_when_github_is_not_authorized() {
        let preview = serde_json::json!({
            "kind": "plugin",
            "id": "demo",
            "version": "1.0.0",
            "name": "Demo",
            "targets": ["github"],
            "github": {
                "repository": "himind/apps",
                "authorized": false,
                "dependencies": [],
                "dependency_blocker": null
            },
            "workbench": {}
        });
        let plan = distribution_publish(&preview);
        assert!(!plan.ready);
        assert!(plan
            .blocked_reasons
            .iter()
            .any(|reason| reason.contains("GitHub 账号未授权")));
    }
}
