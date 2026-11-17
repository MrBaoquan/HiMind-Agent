//! Deterministic, advisory team selection from an explicitly supplied directory.
//!
//! This module does not resolve assets, inspect the environment, create runs or
//! grant access. Summary metadata describes a specialty, not verified tool access
//! or measured expertise. Unknown objectives and missing specialties stay visible.
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
    // Audited professional families, in preference order, not built-in assets.
    reviewed_ids: &'static [&'static str],
    declaration_terms: &'static [&'static str],
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
        ],
        reviewed_ids: &[],
        declaration_terms: &[
            "需求分析",
            "需求澄清",
            "产品设计",
            "产品经理",
            "requirements analysis",
            "product designer",
            "product manager",
        ],
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
            "system design",
        ],
        reviewed_ids: &[
            "com.himind.expert.senior-system-architect",
            "com.himind.expert.project-architect",
        ],
        declaration_terms: &[
            "系统架构",
            "技术架构",
            "系统架构师",
            "system architect",
            "software architect",
            "system architecture",
        ],
    },
    RoleSpec {
        id: "designer",
        label: "界面交互",
        brief: "设计界面结构、交互状态与视觉取值，交付可复核的界面规范。",
        objective_terms: &[
            "界面",
            "交互",
            "页面",
            "前端",
            "视觉",
            "ui",
            "ux",
            "frontend",
            "front-end",
        ],
        reviewed_ids: &["com.himind.expert.senior-ui-designer"],
        declaration_terms: &[
            "界面设计",
            "交互设计",
            "ui 设计",
            "ui设计",
            "ui designer",
            "ui design",
            "ux designer",
            "interaction design",
        ],
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
        reviewed_ids: &["com.himind.expert.software-engineer"],
        declaration_terms: &[
            "软件实现",
            "软件开发",
            "软件工程师",
            "software engineer",
            "software development",
        ],
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
            "回归",
            "test",
            "testing",
            "verify",
            "verification",
            "review",
            "qa",
            "quality",
        ],
        reviewed_ids: &["com.himind.expert.senior-test-engineer"],
        declaration_terms: &[
            "测试工程师",
            "质量验证",
            "软件测试",
            "test engineer",
            "quality assurance",
            "software testing",
        ],
    },
];

/// Recommend only requested responsibilities and exact installed expert pins.
///
/// Conservative Chinese/English keyword rules make this a reviewable suggestion,
/// not a semantic planner. Implementation also requests independent quality
/// review; architecture, product and UI roles otherwise require objective signals.
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
    let implementation = ROLES
        .iter()
        .find(|role| role.id == "implementer")
        .and_then(|role| keyword(&objective, role.objective_terms));
    let review_only = is_review_only(&objective);
    for role in ROLES {
        if review_only && role.id != "reviewer" {
            continue;
        }
        let trigger = keyword(&objective, role.objective_terms).or_else(|| {
            if role.id == "reviewer" {
                implementation
            } else {
                None
            }
        });
        let Some(trigger) = trigger else { continue };
        let selected = role
            .reviewed_ids
            .iter()
            .filter_map(|id| catalog.get(*id).copied())
            .find(|expert| has_pin(expert))
            .map(|expert| (expert, "已审查的专业家族".to_string()))
            .or_else(|| {
                catalog
                    .values()
                    .copied()
                    .filter(|expert| has_pin(expert))
                    // Known identities retain their audited boundary, even if
                    // generic wording mentions another stage such as verification.
                    .filter(|expert| {
                        !ROLES
                            .iter()
                            .any(|spec| spec.reviewed_ids.contains(&expert.id.as_str()))
                    })
                    .find_map(|expert| {
                        let declaration =
                            format!("{} {}", expert.name, expert.description).to_lowercase();
                        keyword(&declaration, role.declaration_terms)
                            .map(|term| (expert, format!("摘要明确声明「{term}」")))
                    })
            });
        let Some((expert, evidence)) = selected else {
            let reason = format!(
                "缺少职责 {}（{}）：目标包含「{}」，当前目录没有可固定且明确匹配的专家；不以其他专业替代。",
                role.id, role.label, trigger,
            );
            result.warnings.push(reason.clone());
            result.missing_required_roles.push(MissingTeamRole {
                role_id: role.id.into(),
                label: role.label.into(),
                reason,
            });
            continue;
        };
        let independent_instance = role.id == "reviewer";
        let mut reason = format!(
            "目标包含「{trigger}」，需要{}；{evidence}。同一专家身份优先最新语义版本；摘要不证明工具可用性或实际能力质量。",
            role.label,
        );
        if role.id == "architect" {
            reason.push_str("项目架构师与系统架构师归为同一专业家族，只推荐一个架构职责；目录中可用时优先系统架构师，职级名称不构成认证。");
        }
        if independent_instance {
            reason.push_str("软件交付需要独立复核；复核必须使用与交付不同的运行实例和上下文，本建议不创建实例。");
            if review_only {
                reason.push_str("本目标为仅复核，界面、架构或实现是被审查对象，不增加生产职责。");
            }
        }
        result.roles.push(TeamRoleRecommendation {
            role_id: role.id.into(),
            label: role.label.into(),
            brief: role.brief.into(),
            expert: MissionExpertPin {
                expert_id: expert.id.clone(),
                expert_version: expert.version.clone(),
                expert_digest: expert.digest.clone(),
            },
            reason,
            independent_instance,
        });
    }
    if !ROLES
        .iter()
        .any(|role| keyword(&objective, role.objective_terms).is_some())
    {
        result.warnings.push("目标未明确命中需求、技术架构、界面交互、软件实现或质量复核职责；请明确产物，不默认拼凑团队。".into());
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
        .unwrap_or("");
    let starts_with_review = [
        "只审查",
        "仅审查",
        "只复核",
        "仅复核",
        "只评审",
        "仅评审",
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
        "审查",
        "复核",
        "评审",
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
            keyword(
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
            .is_some()
        })
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

/// ASCII terms match whole words (so `build` does not imply `ui`). Common direct
/// negations are ignored; complex intent remains the caller's review decision.
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
                && ![
                    "不",
                    "不要",
                    "无需",
                    "不需要",
                    "不做",
                    "不涉及",
                    "不进行",
                    "不包含",
                    "no",
                    "not",
                    "without",
                    "do not",
                    "don't",
                ]
                .iter()
                .any(|negation| {
                    let prefix = before.trim_end();
                    prefix.strip_suffix(*negation).is_some_and(|rest| {
                        !negation.is_ascii()
                            || !rest
                                .chars()
                                .next_back()
                                .is_some_and(|c| c.is_ascii_alphanumeric())
                    })
                })
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
        [
            "project-architect",
            "senior-system-architect",
            "senior-ui-designer",
            "software-engineer",
            "senior-test-engineer",
        ]
        .iter()
        .map(|slug| expert(slug, "1.0.0"))
        .collect()
    }

    #[test]
    fn overlapping_architects_share_one_responsibility_and_project_remains_usable() {
        let mut experts = library();
        let result = recommend_team("设计系统架构", &experts);
        assert_eq!(result.roles.len(), 1);
        assert_eq!(result.roles[0].role_id, "architect");
        assert_eq!(
            result.roles[0].expert.expert_id,
            "com.himind.expert.senior-system-architect"
        );
        assert!(result.roles[0].reason.contains("同一专业家族"));
        experts.retain(|expert| !expert.id.ends_with("senior-system-architect"));
        assert_eq!(
            recommend_team("设计系统架构", &experts).roles[0]
                .expert
                .expert_id,
            "com.himind.expert.project-architect"
        );
    }

    #[test]
    fn latest_semantic_version_wins_over_active_persona_and_reference_counts() {
        let mut old = expert("senior-ui-designer", "1.9.0");
        old.active = true;
        old.skill_count = 100;
        let current = expert("senior-ui-designer", "1.10.0");
        let experts = vec![
            old,
            expert("senior-ui-designer", "1.10.0-rc.1"),
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
        assert_eq!(review.roles[0].role_id, "reviewer");
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
    fn declarations_can_fill_a_gap_but_counts_and_old_metadata_cannot() {
        let mut empty = expert("placeholder", "1.0.0");
        empty.categories = vec!["software-engineering".into()];
        empty.capability_count = 99;
        assert!(recommend_team("需求分析", &[empty]).roles.is_empty());
        let mut product = expert("product", "1.0.0");
        product.description = "需求分析与产品设计".into();
        let result = recommend_team("需求分析", &[product.clone()]);
        assert_eq!(result.roles.len(), 1);
        assert_eq!(result.roles[0].expert.expert_digest, product.digest);
        assert!(result.roles[0].reason.contains("摘要明确声明"));
        let mut changed = product.clone();
        changed.version = "2.0.0".into();
        changed.digest = "sha256:changed".into();
        changed.description.clear();
        assert!(recommend_team("需求分析", &[product, changed])
            .roles
            .is_empty());
    }

    #[test]
    fn missing_pin_is_not_replaced_by_an_older_version_and_ties_are_deterministic() {
        let old = expert("senior-ui-designer", "1.0.0");
        let mut latest = expert("senior-ui-designer", "1.1.0");
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
        for objective in [
            "只审查现有 UI 实现，不修改代码",
            "只读审查现有 UI 实现",
            "只读审查现有 UI 实现并提出修复建议",
            "review existing UI implementation, do not modify code",
            "read-only review UI implementation and suggest fixes",
            "only review the system architecture and UI implementation",
        ] {
            let result = recommend_team(objective, &experts);
            assert_eq!(
                result
                    .roles
                    .iter()
                    .map(|role| role.role_id.as_str())
                    .collect::<Vec<_>>(),
                ["reviewer"],
                "{objective}"
            );
            assert!(result.roles[0].independent_instance);
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
                expert("senior-ui-designer", "1.0.0"),
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
}
