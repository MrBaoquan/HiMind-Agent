//! 把扩展声明的 Capability 命名空间映射到统一功能分类。
//!
//! 三类扩展的分类来源并不一致：插件与技能的 manifest 可以声明 `categories`，
//! Workflow Package 契约没有该字段；Dashboard 发布的历史制品也可能带着空分类。
//! 这里按 Capability ID 前缀推断功能域作为兜底，取值与前端
//! `data/categoryCatalog.ts` 的 `FUNCTIONAL_CATEGORIES.id` 一一对应；
//! 扩展显式声明的分类不会被覆盖。

/// 前缀越长越优先，避免 `business.` 覆盖 `business.exhibit.workspace` 这类更细的命名空间。
const CAPABILITY_CATEGORY_PREFIXES: &[(&str, &[&str])] = &[
    ("short.video", &["video-post", "content-production"]),
    ("wechat.miniprogram", &["software-engineering"]),
    ("wechat.dev", &["software-engineering"]),
    ("extension.", &["software-engineering"]),
    ("engineering.", &["software-engineering"]),
    ("svn.", &["software-engineering"]),
    ("project.repository", &["software-engineering"]),
    ("plugin.manifest", &["software-engineering"]),
    ("media.image", &["visual-design"]),
    ("image.", &["visual-design"]),
    ("media.video", &["video-post"]),
    ("media.audio", &["audio-sound"]),
    ("document.", &["docs-knowledge"]),
    ("docs.", &["docs-knowledge"]),
    ("business.exhibit.workspace", &["collaboration-delivery"]),
    ("exhibit.workspace", &["collaboration-delivery"]),
    ("business.", &["collaboration-delivery"]),
    ("exhibit.", &["collaboration-delivery"]),
    ("project.", &["collaboration-delivery"]),
    ("inner_admin.", &["collaboration-delivery"]),
    ("software.distribution", &["collaboration-delivery"]),
    ("package.", &["collaboration-delivery"]),
    ("upload.", &["collaboration-delivery"]),
    ("artifact.", &["collaboration-delivery"]),
    ("workflow.", &["data-automation"]),
    ("operation.", &["data-automation"]),
    ("media.job", &["data-automation"]),
    ("ai.", &["system-device"]),
    ("mcp.", &["system-device"]),
    ("system.", &["system-device"]),
    ("remote.", &["system-device"]),
    ("capability.", &["system-device"]),
    ("filesystem.", &["system-device"]),
    ("workspace.", &["system-device"]),
];

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn match_prefix(capability: &str) -> Option<&'static [&'static str]> {
    CAPABILITY_CATEGORY_PREFIXES
        .iter()
        .filter(|(prefix, _)| {
            let namespace = prefix.trim_end_matches('.');
            capability == namespace || capability.starts_with(prefix) || capability == *prefix
        })
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, categories)| *categories)
}

/// 按 Capability ID 推断功能分类；无法识别的命名空间不产出分类。
pub(crate) fn infer_categories(capability_ids: &[String]) -> Vec<String> {
    let mut categories: Vec<String> = Vec::new();
    for capability in capability_ids {
        let id = normalize(capability);
        if id.is_empty() {
            continue;
        }
        let Some(candidates) = match_prefix(&id) else {
            continue;
        };
        for category in candidates {
            let value = category.to_string();
            if !categories.contains(&value) {
                categories.push(value);
            }
        }
    }
    categories.sort();
    categories
}

/// 只补空值，保留扩展显式声明的分类。
pub(crate) fn fill_missing_categories(capability_ids: &[String], categories: &mut Vec<String>) {
    if !categories.is_empty() {
        return;
    }
    *categories = infer_categories(capability_ids);
}

#[cfg(test)]
mod tests {
    use super::{fill_missing_categories, infer_categories};

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn infers_categories_from_capability_namespaces() {
        assert_eq!(
            infer_categories(&ids(&["wechat.miniprogram.build"])),
            vec!["software-engineering"]
        );
        assert_eq!(
            infer_categories(&ids(&["short.video.render"])),
            vec!["content-production", "video-post"]
        );
        assert_eq!(
            infer_categories(&ids(&["business.exhibit.list"])),
            vec!["collaboration-delivery"]
        );
        assert_eq!(
            infer_categories(&ids(&["document.inspect"])),
            vec!["docs-knowledge"]
        );
    }

    #[test]
    fn keeps_longest_prefix_priority() {
        // `business.` 也匹配，但更细的展项工作区命名空间必须胜出。
        assert_eq!(
            infer_categories(&ids(&["business.exhibit.workspace.status"])),
            vec!["collaboration-delivery"]
        );
        assert_eq!(infer_categories(&ids(&["media.video"])), vec!["video-post"]);
        assert_eq!(
            infer_categories(&ids(&["media.image.optimize"])),
            vec!["visual-design"]
        );
    }

    #[test]
    fn merges_multiple_namespaces_without_duplicates() {
        assert_eq!(
            infer_categories(&ids(&[
                "wechat.miniprogram.build",
                "wechat.miniprogram.upload",
                "engineering.workspace.lease"
            ])),
            vec!["software-engineering"]
        );
    }

    #[test]
    fn unknown_namespace_stays_uncategorized() {
        assert!(infer_categories(&ids(&["unknown.namespace"])).is_empty());
        assert!(infer_categories(&[]).is_empty());
    }

    #[test]
    fn explicit_categories_are_not_overwritten() {
        let mut categories = vec!["testing-quality".to_string()];
        fill_missing_categories(&ids(&["wechat.miniprogram.build"]), &mut categories);
        assert_eq!(categories, vec!["testing-quality"]);

        let mut empty = Vec::new();
        fill_missing_categories(&ids(&["wechat.miniprogram.build"]), &mut empty);
        assert_eq!(empty, vec!["software-engineering"]);
    }

    #[test]
    fn shipped_wechat_workflow_package_is_classified() {
        // 真实发布的 Workflow Package 必须能落到具体功能域，否则扩展市场里
        // 工作流会一直停在「未分类」。
        let package: crate::workflow::WorkflowPackage = serde_json::from_str(include_str!(
            "../workflows/wechat-miniprogram-delivery/workflow.json"
        ))
        .expect("workflow package fixture");
        assert_eq!(
            infer_categories(&package.capabilities),
            vec!["data-automation", "software-engineering"]
        );
    }

    #[test]
    fn plugin_manifest_keeps_declared_categories() {
        // plugin.json 的 categories 必须被 manifest 解析保留，否则本地插件
        // 目录项拿不到显式分类，只能退回推断。
        let manifest = crate::capability::plugin::parse_plugin_manifest(
            r#"{"id":"com.example.demo","name":"示例插件","version":"1.0.0","categories":["visual-design"],"capabilities":[]}"#,
        )
        .expect("plugin manifest fixture");
        assert_eq!(manifest.categories, vec!["visual-design"]);
    }
}
