//! Deterministic, advisory team selection from an explicitly supplied directory.
//!
//! This module does not resolve assets, inspect the environment, create runs or
//! grant access. Only the fixed professional identities qualify; summary text
//! cannot substitute for them or prove tool access. Missing specialties stay visible.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::MissionExpertPin;
use crate::expert::ExpertSummary;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TeamRecommendation {
    pub(crate) roles: Vec<TeamRoleRecommendation>,
    pub(crate) warnings: Vec<String>,
    /// Requested duties that must be filled before creating a team method.
    #[serde(default)]
    pub(crate) missing_required_roles: Vec<MissingTeamRole>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct MissingTeamRole {
    pub(crate) role_id: String,
    pub(crate) label: String,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TeamRoleRecommendation {
    pub(crate) role_id: String,
    pub(crate) label: String,
    pub(crate) brief: String,
    pub(crate) expert: MissionExpertPin,
    pub(crate) reason: String,
    /// The caller must create a separate review run/context; this is not a run ID.
    pub(crate) independent_instance: bool,
}

struct RoleSpec {
    id: &'static str,
    label: &'static str,
    brief: &'static str,
    objective_terms: &'static [&'static str],
    expert_id: &'static str,
}

const ROLES: &[RoleSpec] = &[
    RoleSpec {
        id: "requirements",
        label: "需求与产品",
        brief: "澄清目标、用户路径、约束与验收条件，记录待确认问题。",
        objective_terms: &[
            "需求分析",
            "需求澄清",
            "需求梳理",
            "产品需求",
            "产品规划",
            "产品设计",
            "prd",
            "requirements",
            "product planning",
            "product design",
        ],
        expert_id: "com.himind.expert.product-designer",
    },
    RoleSpec {
        id: "architect",
        label: "技术架构",
        brief: "界定系统边界、技术权衡与故障恢复，给出可实施的技术方案。",
        objective_terms: &[
            "架构",
            "系统边界",
            "技术方案",
            "architecture",
            "architectural",
            "technical architect",
            "system design",
        ],
        expert_id: "com.himind.expert.technical-architect",
    },
    RoleSpec {
        id: "designer",
        label: "界面交互",
        brief: "设计界面结构、交互状态与视觉取值，交付可复核的界面规范。",
        objective_terms: &[
            "界面",
            "交互设计",
            "界面交互",
            "页面",
            "前端",
            "ui",
            "ux",
            "frontend",
            "front-end",
            "interaction design",
        ],
        expert_id: "com.himind.expert.interaction-designer",
    },
    RoleSpec {
        id: "implementer",
        label: "软件实现",
        brief: "理解现状，在确认的边界内实现修改，提交可复现的检查与交付证据。",
        objective_terms: &[
            "实现",
            "开发",
            "修复",
            "重构",
            "编码",
            "修改代码",
            "改代码",
            "构建",
            "搭建",
            "implement",
            "implementation",
            "develop",
            "development",
            "build",
            "fix",
            "refactor",
        ],
        expert_id: "com.himind.expert.software-engineer",
    },
    RoleSpec {
        id: "reviewer",
        label: "独立质量复核",
        brief: "使用独立运行实例复核交付候选、风险与回归证据，报告缺陷和未覆盖范围。",
        objective_terms: &[
            "测试",
            "验证",
            "验收",
            "复核",
            "审查",
            "评审",
            "审核",
            "检查",
            "回归",
            "test",
            "testing",
            "verify",
            "verification",
            "review",
            "qa",
            "quality",
            "audit",
        ],
        expert_id: "com.himind.expert.quality-engineer",
    },
    RoleSpec {
        id: "game-designer",
        label: "游戏设计",
        brief: "定义玩法、规则、关卡与数值反馈，给出可验证的游戏设计。",
        objective_terms: &[
            "游戏",
            "玩法",
            "关卡",
            "数值平衡",
            "game",
            "games",
            "gameplay",
            "level design",
        ],
        expert_id: "com.himind.expert.game-designer",
    },
    RoleSpec {
        id: "realtime-engineer",
        label: "实时引擎工程",
        brief: "处理 Unity 等实时引擎的运行逻辑、场景、性能与构建证据。",
        objective_terms: &[
            "unity",
            "unreal",
            "实时引擎",
            "实时渲染",
            "实时三维",
            "实时交互",
            "realtime",
            "real-time",
            "real time",
            "ue4",
            "ue5",
        ],
        expert_id: "com.himind.expert.realtime-engineer",
    },
    RoleSpec {
        id: "technical-artist",
        label: "技术美术",
        brief: "处理着色器、材质、特效与美术管线，核对视觉质量及性能预算。",
        objective_terms: &[
            "技术美术",
            "着色器",
            "材质",
            "特效",
            "美术管线",
            "渲染管线",
            "shader",
            "shaders",
            "material",
            "materials",
            "vfx",
            "technical art",
            "art pipeline",
        ],
        expert_id: "com.himind.expert.technical-artist",
    },
    RoleSpec {
        id: "exhibit-experience-designer",
        label: "展项体验设计",
        brief: "设计展项叙事、观众动线与现场反馈，明确体验及现场验收条件。",
        objective_terms: &[
            "展项",
            "展陈",
            "展览",
            "展馆",
            "观众动线",
            "exhibit",
            "exhibits",
            "exhibition",
        ],
        expert_id: "com.himind.expert.exhibit-experience-designer",
    },
    RoleSpec {
        id: "device-integration-engineer",
        label: "设备集成工程",
        brief: "界定设备协议、通信、联调与故障恢复，交付设备验收证据。",
        objective_terms: &[
            "设备",
            "硬件",
            "传感器",
            "串口",
            "物联网",
            "plc",
            "modbus",
            "mqtt",
            "device",
            "devices",
            "hardware",
            "sensor",
            "sensors",
            "serial port",
            "iot",
        ],
        expert_id: "com.himind.expert.device-integration-engineer",
    },
];

/// Recommend only requested responsibilities and exact installed expert pins.
///
/// Conservative Chinese/English keyword rules make this a reviewable suggestion,
/// not a semantic planner. Implementation also requests independent quality
/// review; all other specialties require objective signals. Review objectives use
/// the requested specialty's independent instance instead of production duties.
/// Latest versions are chosen per identity before specialty matching. Active
/// personas and reference counts never give older versions or capabilities rank.
pub(crate) fn recommend_team(objective: &str, experts: &[ExpertSummary]) -> TeamRecommendation {
    let mut result = TeamRecommendation {
        roles: Vec::new(),
        warnings: Vec::new(),
        missing_required_roles: Vec::new(),
    };
    let objective = objective.trim().to_lowercase();
    if objective.is_empty() {
        result
            .warnings
            .push("目标为空，无法判断所需职责；请先说明交付物与验收目标。".into());
        return result;
    }
    if experts.is_empty() {
        result
            .warnings
            .push("当前已安装专家目录为空；不会生成专家身份或默认能力。".into());
    }
    result.warnings.push(
        "职责由中英文关键词推断，不能完整理解复杂目标或证明专家能力；请核对建议及缺失职责。".into(),
    );
    let catalog = latest_by_identity(experts);
    for expert in catalog.values() {
        if !has_pin(expert) {
            result.warnings.push(format!(
                "专家 {} @ {} 缺少完整 id/version/digest，不能固定推荐；不回退到旧版本。",
                expert.id, expert.version,
            ));
        }
    }
    let objective = affirmative_objective(&objective);
    let implementation = ROLES
        .iter()
        .find(|role| role.id == "implementer")
        .and_then(|role| keyword(&objective, role.objective_terms));
    let review_only = is_review_only(&objective);
    let specialty_requested = ROLES.iter().any(|role| {
        !matches!(role.id, "implementer" | "reviewer")
            && keyword(&objective, role.objective_terms).is_some()
    });
    let code_review = keyword(&objective, &["代码", "软件", "code", "software"]).or(implementation);
    let quality_review = keyword(
        &objective,
        &[
            "质量", "测试", "回归", "quality", "qa", "test", "tests", "testing",
        ],
    );
    let mut requested = false;
    for role in ROLES {
        let trigger = if review_only {
            match role.id {
                "implementer" => {
                    if specialty_requested {
                        None
                    } else {
                        code_review
                    }
                }
                "reviewer" => quality_review.or_else(|| {
                    if !specialty_requested && code_review.is_none() {
                        keyword(&objective, role.objective_terms)
                    } else {
                        None
                    }
                }),
                _ => keyword(&objective, role.objective_terms),
            }
        } else {
            keyword(&objective, role.objective_terms).or_else(|| {
                if role.id == "reviewer" {
                    implementation
                } else {
                    None
                }
            })
        };
        let Some(trigger) = trigger else { continue };
        requested = true;
        let selected = catalog
            .get(role.expert_id)
            .copied()
            .filter(|expert| has_pin(expert));
        let Some(expert) = selected else {
            let reason = format!(
                "缺少职责 {}（{}）：目标包含「{}」，需要可固定的专家 {}；不以其他专业或摘要后备替代。",
                role.id, role.label, trigger, role.expert_id,
            );
            result.warnings.push(reason.clone());
            result.missing_required_roles.push(MissingTeamRole {
                role_id: role.id.into(),
                label: role.label.into(),
                reason,
            });
            continue;
        };
        let independent_instance = review_only || role.id == "reviewer";
        let mut reason = format!(
            "目标包含「{trigger}」，需要{}；仅匹配固定专家身份。同一专家身份优先最新语义版本；摘要不证明工具可用性或实际能力质量。",
            role.label,
        );
        if review_only {
            reason.push_str("本目标为专业审查，命中专业是被审查对象；仅推荐该专业的独立审查实例，不增加生产职责。审查必须使用与原交付不同的运行实例和上下文，本建议不创建实例。");
        } else if independent_instance {
            reason.push_str("软件交付需要独立复核；复核必须使用与交付不同的运行实例和上下文，本建议不创建实例。");
        }
        result.roles.push(TeamRoleRecommendation {
            role_id: role.id.into(),
            label: if review_only && role.id != "reviewer" {
                format!("{}专业审查", role.label)
            } else { role.label.into() },
            brief: if review_only {
                format!("使用独立运行实例审查{}的现有交付、风险与证据；报告缺陷和未覆盖范围，不承担原交付实现。", role.label)
            } else { role.brief.into() },
            expert: MissionExpertPin {
                expert_id: expert.id.clone(),
                expert_version: expert.version.clone(),
                expert_digest: expert.digest.clone(),
                skill_pins: Vec::new(),
            },
            reason,
            independent_instance,
        });
    }
    if !requested {
        result
            .warnings
            .push("目标未明确命中支持的专业职责；请明确产物，不默认拼凑团队。".into());
    }
    result
}

/// Recognize review as the requested action, without treating the reviewed
/// implementation/design as a request to produce another one. A later explicit
/// production clause keeps mixed review-and-repair objectives on the normal path.
fn is_review_only(objective: &str) -> bool {
    let review_terms = ROLES
        .iter()
        .find(|role| role.id == "reviewer")
        .unwrap()
        .objective_terms;
    let first = objective
        .split(['，', ',', '。', '.', ';', '；', '\n'])
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches("请")
        .trim_start_matches("please ");
    let starts_with_review = [
        "只审查",
        "仅审查",
        "只复核",
        "仅复核",
        "只评审",
        "仅评审",
        "只审核",
        "仅审核",
        "只验证",
        "仅验证",
        "只测试",
        "仅测试",
        "只做审查",
        "仅做审查",
        "只做复核",
        "仅做复核",
        "只读",
        "review",
        "only audit",
        "only verify",
        "审查",
        "复核",
        "评审",
        "审核",
        "检查",
        "验收",
        "audit",
        "对现有",
        "对已有",
        "验证",
        "测试",
        "only review",
        "only test",
        "read-only",
        "read only",
    ]
    .iter()
    .any(|prefix| first.starts_with(*prefix));
    if !starts_with_review || keyword(first, review_terms).is_none() {
        return false;
    }
    if first.starts_with("只读") || first.starts_with("read-only") || first.starts_with("read only")
    {
        return true;
    }
    // Split action conjunctions as well as punctuation. Nouns inside the first
    // review clause ("review existing UI implementation") remain review input.
    let clauses = objective
        .replace("并且", "，")
        .replace("并", "，")
        .replace("然后", "，")
        .replace("以及", "，")
        .replace(" and ", ",")
        .replace(" then ", ",");
    !clauses
        .split(['，', ',', '。', '.', ';', '；', '\n'])
        .skip(1)
        .any(|clause| {
            starts_production(
                clause.trim(),
                &[
                    "实现",
                    "开发",
                    "修复",
                    "重构",
                    "编码",
                    "修改",
                    "构建",
                    "搭建",
                    "设计",
                    "implement",
                    "develop",
                    "build",
                    "fix",
                    "refactor",
                    "modify",
                    "design",
                ],
            )
        })
}

fn starts_production(clause: &str, terms: &[&str]) -> bool {
    let clause = clause
        .trim_start_matches("请")
        .trim_start_matches("please ");
    // A repair/design noun in a review object or a suggestion is not an action.
    terms
        .iter()
        .any(|term| clause.starts_with(*term) && keyword(clause, &[*term]).is_some())
}

/// Keep positive clauses before matching specialties or production actions.
/// A negated list stays excluded through its conjunctions ("无需 Unity 开发和设备接入").
/// Punctuation and an explicit change of direction begin a new positive clause.
fn affirmative_objective(objective: &str) -> String {
    let clauses = objective
        .replace("不但", "")
        .replace("不仅", "")
        .replace("not only ", "")
        .replace("但是", "，")
        .replace("但", "，")
        .replace("而是", "，")
        .replace("不过", "，")
        .replace("然后", "，")
        .replace(" but ", ",")
        .replace(" then ", ",");
    clauses
        .split(['，', ',', '。', '.', ';', '；', '\n'])
        .map(|clause| {
            let end = [
                "不需要",
                "不涉及",
                "不进行",
                "不包含",
                "不要",
                "不做",
                "无需",
                "无须",
                "不必",
                "不用",
                "排除",
                "禁止",
                "避免",
                "勿",
                "不",
                "no",
                "not",
                "without",
                "do not",
                "don't",
                "never",
                "excluding",
            ]
            .iter()
            .flat_map(|negation| {
                clause
                    .match_indices(*negation)
                    .filter_map(move |(start, _)| {
                        let before = &clause[..start];
                        let after = &clause[start + negation.len()..];
                        let boundary = !negation.is_ascii()
                            || (!before
                                .chars()
                                .next_back()
                                .is_some_and(|c| c.is_ascii_alphanumeric())
                                && !after
                                    .chars()
                                    .next()
                                    .is_some_and(|c| c.is_ascii_alphanumeric()));
                        boundary.then_some(start)
                    })
            })
            .min()
            .unwrap_or(clause.len());
            &clause[..end]
        })
        .filter(|clause| !clause.trim().is_empty())
        .collect::<Vec<_>>()
        .join("，")
}

fn has_pin(expert: &ExpertSummary) -> bool {
    !expert.id.trim().is_empty()
        && !expert.version.trim().is_empty()
        && !expert.digest.trim().is_empty()
}

fn latest_by_identity(experts: &[ExpertSummary]) -> BTreeMap<&str, &ExpertSummary> {
    let mut catalog = BTreeMap::<&str, &ExpertSummary>::new();
    for expert in experts {
        let replace = catalog.get(expert.id.as_str()).is_none_or(|previous| {
            semver::Version::parse(&expert.version)
                .ok()
                .cmp(&semver::Version::parse(&previous.version).ok())
                .then_with(|| expert.version.cmp(&previous.version))
                // Same-version content has no provenance preference in a summary.
                // A digest tie-break makes directory enumeration irrelevant.
                .then_with(|| expert.digest.cmp(&previous.digest))
                .then_with(|| expert.name.cmp(&previous.name))
                .then_with(|| expert.description.cmp(&previous.description))
                .is_gt()
        });
        if replace {
            catalog.insert(expert.id.as_str(), expert);
        }
    }
    catalog
}

/// ASCII terms match whole words (so `build` does not imply `ui`).
/// Negation has already been handled at the objective clause boundary.
fn keyword<'a>(text: &str, terms: &'a [&str]) -> Option<&'a str> {
    terms.iter().copied().find(|term| {
        text.match_indices(*term).any(|(start, _)| {
            let before = &text[..start];
            let after = &text[start + term.len()..];
            let word_boundary = !term.is_ascii()
                || (!before
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
                    && !after
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric()));
            word_boundary
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expert(slug: &str, version: &str) -> ExpertSummary {
        ExpertSummary {
            id: format!("com.himind.expert.{slug}"),
            name: slug.into(),
            version: version.into(),
            description: String::new(),
            author: String::new(),
            categories: Vec::new(),
            supported_clients: Vec::new(),
            skill_count: 0,
            workflow_count: 0,
            capability_count: 0,
            digest: format!("sha256:{slug}-{version}"),
            builtin: false,
            active: false,
            icon: None,
        }
    }

    fn library() -> Vec<ExpertSummary> {
        ROLES
            .iter()
            .map(|role| {
                expert(
                    role.expert_id.strip_prefix("com.himind.expert.").unwrap(),
                    "1.0.0",
                )
            })
            .collect()
    }

    #[test]
    fn fixed_identities_do_not_use_aliases_or_summary_fallbacks() {
        let mut experts = library();
        let result = recommend_team("设计系统架构", &experts);
        assert_eq!(result.roles.len(), 1);
        assert_eq!(result.roles[0].role_id, "architect");
        assert_eq!(
            result.roles[0].expert.expert_id,
            "com.himind.expert.technical-architect"
        );
        experts.retain(|expert| !expert.id.ends_with("technical-architect"));
        let mut alias = expert("custom-architect", "9.0.0");
        alias.description = "技术架构 系统架构师 system architect".into();
        experts.push(alias);
        let missing = recommend_team("设计系统架构", &experts);
        assert!(missing.roles.is_empty());
        assert_eq!(missing.missing_required_roles[0].role_id, "architect");
        assert!(missing.missing_required_roles[0]
            .reason
            .contains("com.himind.expert.technical-architect"));
    }

    #[test]
    fn latest_semantic_version_wins_over_active_persona_and_reference_counts() {
        let mut old = expert("interaction-designer", "1.9.0");
        old.active = true;
        old.skill_count = 100;
        let current = expert("interaction-designer", "1.10.0");
        let experts = vec![
            old,
            expert("interaction-designer", "1.10.0-rc.1"),
            current.clone(),
        ];
        let result = recommend_team("设计 UI 界面", &experts);
        assert_eq!(result.roles.len(), 1);
        assert_eq!(result.roles[0].expert.expert_version, current.version);
        assert_eq!(result.roles[0].expert.expert_digest, current.digest);
        let reversed = experts.into_iter().rev().collect::<Vec<_>>();
        assert_eq!(result, recommend_team("设计 UI 界面", &reversed));
    }

    #[test]
    fn empty_library_and_missing_specialties_are_explained_without_fake_roles() {
        let result = recommend_team("实现 UI 页面并进行需求分析", &[]);
        assert!(result.roles.is_empty());
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("目录为空")));
        for role in ["requirements", "designer", "implementer", "reviewer"] {
            assert!(result.warnings.iter().any(|warning| warning.contains(role)));
        }
        let result = recommend_team(
            "进行需求分析并修复接口",
            &[expert("software-engineer", "1.0.0")],
        );
        assert_eq!(result.roles.len(), 1);
        assert_eq!(result.roles[0].role_id, "implementer");
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("requirements")));
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("reviewer")));
    }

    #[test]
    fn ui_delivery_selects_design_implementation_and_independent_quality_only() {
        let experts = library();
        let result = recommend_team("实现设置页面 UI，并验证交互状态", &experts);
        assert_eq!(
            result
                .roles
                .iter()
                .map(|role| role.role_id.as_str())
                .collect::<Vec<_>>(),
            ["designer", "implementer", "reviewer"]
        );
        assert!(result.missing_required_roles.is_empty());
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("关键词")));
        for role in &result.roles {
            assert_eq!(role.independent_instance, role.role_id == "reviewer");
            assert!(!role.brief.is_empty());
            assert!(!role.reason.is_empty());
            assert!(experts
                .iter()
                .any(|expert| expert.id == role.expert.expert_id
                    && expert.version == role.expert.expert_version
                    && expert.digest == role.expert.expert_digest));
        }
    }

    #[test]
    fn design_only_and_review_only_do_not_force_implementation() {
        let experts = library();
        let design = recommend_team("只做 UI 设计，无需实现", &experts);
        assert_eq!(design.roles.len(), 1);
        assert_eq!(design.roles[0].role_id, "designer");
        let review = recommend_team("review existing code", &experts);
        assert_eq!(review.roles.len(), 1);
        assert_eq!(review.roles[0].role_id, "implementer");
        assert!(review.roles[0].independent_instance);
        let build = recommend_team("build a service", &experts);
        assert_eq!(build.roles.len(), 2);
        assert!(!build.roles.iter().any(|role| role.role_id == "designer"));
        assert_eq!(recommend_team("piano UI design", &experts).roles.len(), 1);
        assert_eq!(
            recommend_team("UI design, do not implement", &experts)
                .roles
                .len(),
            1
        );
    }

    #[test]
    fn declarations_and_counts_cannot_replace_the_fixed_product_expert() {
        let mut empty = expert("placeholder", "1.0.0");
        empty.categories = vec!["software-engineering".into()];
        empty.capability_count = 99;
        assert!(recommend_team("需求分析", &[empty]).roles.is_empty());
        let mut product = expert("product", "1.0.0");
        product.description = "需求分析与产品设计".into();
        let result = recommend_team("需求分析", &[product]);
        assert!(result.roles.is_empty());
        assert_eq!(result.missing_required_roles[0].role_id, "requirements");
        let fixed = expert("product-designer", "1.0.0");
        assert_eq!(
            recommend_team("需求分析", &[fixed.clone()]).roles[0]
                .expert
                .expert_digest,
            fixed.digest
        );
    }

    #[test]
    fn missing_pin_is_not_replaced_by_an_older_version_and_ties_are_deterministic() {
        let old = expert("interaction-designer", "1.0.0");
        let mut latest = expert("interaction-designer", "1.1.0");
        latest.digest.clear();
        let result = recommend_team("UI设计", &[old.clone(), latest]);
        assert!(result.roles.is_empty());
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("id/version/digest")));
        let mut other = old.clone();
        other.digest = "sha256:other-content".into();
        assert_eq!(
            recommend_team("UI设计", &[old.clone(), other.clone()]),
            recommend_team("UI设计", &[other, old])
        );
    }

    #[test]
    fn unspecified_or_unsupported_objectives_do_not_default_to_a_software_team() {
        for objective in ["", "   ", "翻译一封信"] {
            let result = recommend_team(objective, &library());
            assert!(result.roles.is_empty());
            assert!(!result.warnings.is_empty());
        }
    }

    #[test]
    fn read_only_review_treats_ui_implementation_as_input_and_keeps_mixed_repairs() {
        let experts = library();
        for (objective, expected) in [
            ("只审查现有 UI 实现，不修改代码", vec!["designer"]),
            ("只读审查现有 UI 实现", vec!["designer"]),
            ("只读审查现有 UI 实现并提出修复建议", vec!["designer"]),
            (
                "review existing UI implementation, do not modify code",
                vec!["designer"],
            ),
            (
                "read-only review UI implementation and suggest fixes",
                vec!["designer"],
            ),
            (
                "only review the system architecture and UI implementation",
                vec!["architect", "designer"],
            ),
        ] {
            let result = recommend_team(objective, &experts);
            assert_eq!(
                result
                    .roles
                    .iter()
                    .map(|role| role.role_id.as_str())
                    .collect::<Vec<_>>(),
                expected,
                "{objective}"
            );
            assert!(result.roles.iter().all(|role| role.independent_instance));
            assert!(result.roles[0].reason.contains("被审查对象"));
            assert!(result.missing_required_roles.is_empty());
        }
        for objective in ["审查 UI 实现并修复缺陷", "review UI and implement fixes"] {
            assert!(recommend_team(objective, &experts)
                .roles
                .iter()
                .any(|role| role.role_id == "implementer"));
        }
        assert!(recommend_team("只设计 UI 并复核设计", &experts)
            .roles
            .iter()
            .any(|role| role.role_id == "designer"));
    }

    #[test]
    fn missing_required_review_survives_as_structured_information() {
        let result = recommend_team(
            "实现 UI 页面",
            &[
                expert("software-engineer", "1.0.0"),
                expert("interaction-designer", "1.0.0"),
            ],
        );
        assert_eq!(result.missing_required_roles.len(), 1);
        assert_eq!(result.missing_required_roles[0].role_id, "reviewer");
        assert!(result
            .warnings
            .contains(&result.missing_required_roles[0].reason));
        let restored: TeamRecommendation =
            serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
        assert_eq!(restored, result);
    }

    fn role_ids(result: &TeamRecommendation) -> Vec<&str> {
        result
            .roles
            .iter()
            .map(|role| role.role_id.as_str())
            .collect()
    }

    #[test]
    fn domain_delivery_requests_only_the_relevant_specialists_and_quality() {
        for (objective, expected) in [
            (
                "开发 Unity 游戏玩法及 shader 特效",
                vec![
                    "implementer",
                    "reviewer",
                    "game-designer",
                    "realtime-engineer",
                    "technical-artist",
                ],
            ),
            (
                "设计展项叙事与设备联调方案",
                vec!["exhibit-experience-designer", "device-integration-engineer"],
            ),
            (
                "修复 Unity 场景加载",
                vec!["implementer", "reviewer", "realtime-engineer"],
            ),
            (
                "实现串口传感器接入",
                vec!["implementer", "reviewer", "device-integration-engineer"],
            ),
            ("实现普通接口", vec!["implementer", "reviewer"]),
        ] {
            let result = recommend_team(objective, &library());
            assert_eq!(role_ids(&result), expected, "{objective}");
            assert!(result.missing_required_roles.is_empty(), "{objective}");
        }
    }

    #[test]
    fn domain_gaps_remain_required_even_when_generic_development_is_available() {
        let result = recommend_team(
            "实现 Unity 游戏展项，接入设备并编写 shader",
            &[
                expert("software-engineer", "1.0.0"),
                expert("quality-engineer", "1.0.0"),
            ],
        );
        assert_eq!(role_ids(&result), ["implementer", "reviewer"]);
        assert_eq!(
            result
                .missing_required_roles
                .iter()
                .map(|role| role.role_id.as_str())
                .collect::<Vec<_>>(),
            [
                "game-designer",
                "realtime-engineer",
                "technical-artist",
                "exhibit-experience-designer",
                "device-integration-engineer",
            ]
        );
        for missing in &result.missing_required_roles {
            assert!(result.warnings.contains(&missing.reason));
        }
    }

    #[test]
    fn specialty_reviews_use_only_independent_professional_instances() {
        for (objective, expected) in [
            (
                "审核 Unity 展项设备协议与 shader",
                vec![
                    "realtime-engineer",
                    "technical-artist",
                    "exhibit-experience-designer",
                    "device-integration-engineer",
                ],
            ),
            (
                "audit existing gameplay and suggest design improvements",
                vec!["game-designer"],
            ),
            ("对现有产品需求进行评审", vec!["requirements"]),
            ("review existing code", vec!["implementer"]),
            ("仅测试回归质量", vec!["reviewer"]),
        ] {
            let result = recommend_team(objective, &library());
            assert_eq!(role_ids(&result), expected, "{objective}");
            assert!(result.roles.iter().all(|role| role.independent_instance));
        }
        let missing = recommend_team("审核 Unity 实现", &[expert("quality-engineer", "1.0.0")]);
        assert!(missing.roles.is_empty());
        assert_eq!(
            missing.missing_required_roles[0].role_id,
            "realtime-engineer"
        );
    }

    #[test]
    fn negated_domains_and_production_lists_do_not_request_experts() {
        for objective in [
            "只设计 UI，无需游戏开发与设备接入",
            "不需要游戏设计、Unity开发及技术美术；只设计 UI",
            "UI design without Unity development and shaders",
            "UI design, do not implement games or devices",
            "只设计 UI 但不要实现设备接入",
        ] {
            let result = recommend_team(objective, &library());
            assert_eq!(role_ids(&result), ["designer"], "{objective}");
        }
        assert!(recommend_team("不需要 Unity 游戏设备开发", &library())
            .roles
            .is_empty());
        assert_eq!(
            role_ids(&recommend_team("不要实现，审查 Unity", &library())),
            ["realtime-engineer"]
        );
        assert_eq!(
            role_ids(&recommend_team("实现接口，无需测试", &library())),
            ["implementer", "reviewer"]
        );
    }

    #[test]
    fn mixed_domain_review_and_repair_retains_implementation_quality_gate() {
        for objective in [
            "审查 Unity 并修复缺陷",
            "review shaders and implement fixes",
        ] {
            let result = recommend_team(objective, &library());
            assert!(result
                .roles
                .iter()
                .any(|role| role.role_id == "implementer" && !role.independent_instance));
            assert!(result
                .roles
                .iter()
                .any(|role| role.role_id == "reviewer" && role.independent_instance));
        }
    }
}
