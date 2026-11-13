//! 工作区登记表的行为测试。
//!
//! 重点放在三类最容易出错的地方：路径归一化（UNC 与边界）、父子关系
//! （环与级联删除）、以及并发写（每个能力调用都是独立的 Tauri 命令线程，
//! 锁必须真的挡住并发）。

use super::*;

#[test]
fn chinese_display_name_is_explicit_or_unambiguous() {
    let mut project = RegistryProject { name: "IOStudio".into(), aliases: vec!["设备配置与编排".into()], ..Default::default() };
    assert_eq!(project.display_name(), "设备配置与编排");
    project.aliases.push("编辑器".into());
    assert_eq!(project.display_name(), "IOStudio");
    project.name = "设备配置与编排".into();
    assert_eq!(project.display_name(), "设备配置与编排");
}

#[test]
fn renaming_preserves_technical_lookup_and_rejects_another_identity() {
    let mut registry = Registry::default();
    let original = upsert_project(&mut registry, ProjectInput { name: "IOStudio".into(), ..Default::default() }).unwrap();
    let renamed = upsert_project(&mut registry, ProjectInput { id: original.id.clone(), name: "设备配置与编排".into(), ..Default::default() }).unwrap();
    assert_eq!(renamed.id, original.id);
    assert_eq!(registry.find_project("IOStudio").unwrap().id, original.id);
    let other = upsert_project(&mut registry, ProjectInput { name: "其他项目".into(), ..Default::default() }).unwrap();
    assert!(upsert_project(&mut registry, ProjectInput { id: other.id, name: "IOStudio".into(), ..Default::default() }).is_err());
}

/// 建一个临时目录 + 登记表路径。返回值里的 `PathBuf` 是目录，调用方
/// 在测试结束时靠它清理；仓里没有 `tempfile` 依赖，沿用 `workspace_lease`
/// 的 `std::env::temp_dir()` 写法。
///
/// **必须能回退**：有些机器的 `%TEMP%` 带着 DSH 沙箱留下的 capability-SID
/// ACE，Rust 进程在那里 `create_dir` 会拿到 `拒绝访问`，而同一台机器上用别的
/// 运行时（Python/Node）却能建出来。那是环境状态，不是被测代码的行为——
/// 遇到就换到工作区的 `.himind/` 下，而不是让 20 个用例一起变红。
fn temp_registry() -> (PathBuf, PathBuf) {
    let name = format!(
        "himind-workspace-registry-{}-{}",
        std::process::id(),
        NEXT_TEST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let candidates = [
        std::env::temp_dir().join(&name),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".himind")
            .join("test-tmp")
            .join(&name),
    ];
    for dir in candidates {
        match std::fs::create_dir_all(&dir) {
            Ok(()) => return (dir.clone(), dir.join("workspace-registry.json")),
            Err(error) => eprintln!(
                "temp_registry: {} 不可用（{error}），换下一个落点",
                dir.display()
            ),
        }
    }
    panic!("没有可写的临时目录：%TEMP% 与 <工作区>/.himind/test-tmp 都建不出来");
}

static NEXT_TEST_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn seed(path: &Path) -> (String, String) {
    let mut root_id = String::new();
    let mut sub_id = String::new();
    Registry::update_at(path, |registry| {
        let root = upsert_project(
            registry,
            ProjectInput {
                name: "扩展仓库".to_string(),
                kind: KIND_AGGREGATE.to_string(),
                aliases: vec!["himind-extensions".to_string()],
                ..Default::default()
            },
        )?;
        root_id = root.id.clone();
        let sub = upsert_project(
            registry,
            ProjectInput {
                name: "工作日志".to_string(),
                kind: KIND_SUBPROJECT.to_string(),
                aliases: vec!["work-log".to_string()],
                parent_id: ParentField::Set(root.id.clone()),
                ..Default::default()
            },
        )?;
        sub_id = sub.id.clone();
        bind_path(
            registry,
            BindInput {
                project_ref: root.id,
                path: "F:/WebProjects/himind-extensions".to_string(),
                ..Default::default()
            },
        )?;
        bind_path(
            registry,
            BindInput {
                project_ref: sub.id,
                path: "F:/WebProjects/himind-extensions/plugins/work-log".to_string(),
                ..Default::default()
            },
        )?;
        Ok(())
    })
    .expect("seed");
    (root_id, sub_id)
}

#[test]
fn normalize_path_handles_windows_forms() {
    let cases = [
        (
            r"F:\WebProjects\himind-extensions",
            "F:/WebProjects/himind-extensions",
        ),
        (
            "F:/WebProjects/himind-extensions/",
            "F:/WebProjects/himind-extensions",
        ),
        (r"C:\", "C:/"),
        ("c:", "C:/"),
        (r"F:\a\..\b", "F:/b"),
        (r"F:\.\a\\b", "F:/a/b"),
        (r"\\?\C:\very\long\path", "C:/very/long/path"),
        (r"\\server\share\folder", "//server/share/folder"),
        (r"\\server\share", "//server/share"),
        (r"\\?\UNC\server\share\dir", "//server/share/dir"),
    ];
    for (input, want) in cases {
        let got = normalize_path(input).unwrap_or_else(|error| panic!("{input}: {error}"));
        assert_eq!(got, want, "normalize_path({input:?})");
    }
}

#[test]
fn normalize_path_rejects_relative_and_escapes() {
    // UNC 的 `..` 单独判：标准库的 path::clean 会把 `//server/share` 压成
    // `/server/share`，所以这里必须自己拦，不能靠归一化兜底。
    for value in ["", "relative/path", "./here", r"F:\a\..\..\b", r"\\server"] {
        assert!(
            normalize_path(value).is_err(),
            "normalize_path({value:?}) 应当报错"
        );
    }
}

#[test]
fn normalize_path_keeps_unc_host_and_share() {
    // 这条断言是防止有人日后「顺手简化」回 path::clean。
    assert_eq!(
        normalize_path("//server/share/dir").unwrap(),
        "//server/share/dir"
    );
}

#[test]
fn is_under_respects_path_boundaries() {
    assert!(
        !is_under("F:/a/subway", "F:/a/sub"),
        "A/SUB 不应匹配 A/SUBWAY"
    );
    assert!(is_under("F:/a/sub/deep", "F:/a/sub"));
    assert!(is_under("F:/a/sub", "F:/a/sub"));
}

#[test]
fn resolve_picks_longest_prefix_and_reports_ancestors() {
    let (_dir, path) = temp_registry();
    let (root_id, sub_id) = seed(&path);
    let registry = Registry::load_from(&path).unwrap();

    let hit = resolve_path(
        &registry,
        "F:/WebProjects/himind-extensions/plugins/work-log/main.go",
    )
    .unwrap()
    .expect("应命中子项目");
    assert_eq!(hit.project.id, sub_id);

    let ancestors = registry.ancestors(&sub_id);
    assert_eq!(ancestors.len(), 1);
    assert_eq!(ancestors[0].id, root_id);
}

#[test]
fn resolve_reports_conflict_when_two_projects_tie() {
    let (_dir, path) = temp_registry();
    Registry::update_at(&path, |registry| {
        let first = upsert_project(
            registry,
            ProjectInput {
                name: "甲".to_string(),
                ..Default::default()
            },
        )?;
        let second = upsert_project(
            registry,
            ProjectInput {
                name: "乙".to_string(),
                ..Default::default()
            },
        )?;
        registry.workspaces.push(RegistryWorkspace {
            id: "w1".to_string(),
            project_id: first.id,
            path: "F:/tie".to_string(),
            role: ROLE_ROOT.to_string(),
            source: SOURCE_MANUAL.to_string(),
            confidence: CONFIDENCE_EXACT.to_string(),
            created_at: String::new(),
        });
        registry.workspaces.push(RegistryWorkspace {
            id: "w2".to_string(),
            project_id: second.id,
            path: "F:/tie".to_string(),
            role: ROLE_ROOT.to_string(),
            source: SOURCE_MANUAL.to_string(),
            confidence: CONFIDENCE_EXACT.to_string(),
            created_at: String::new(),
        });
        Ok(())
    })
    .unwrap();

    let registry = Registry::load_from(&path).unwrap();
    let error = resolve_path(&registry, "F:/tie/sub").unwrap_err();
    assert!(error.contains(ERR_PATH_CONFLICT), "{error}");
}

#[test]
fn validate_reports_cycles_aliases_and_duplicate_paths() {
    let (_dir, path) = temp_registry();
    Registry::update_at(&path, |registry| {
        registry.projects.push(RegistryProject {
            id: "a".to_string(),
            name: "A".to_string(),
            kind: KIND_PRODUCT.to_string(),
            parent_id: "b".to_string(),
            aliases: vec!["shared".to_string()],
            ..Default::default()
        });
        registry.projects.push(RegistryProject {
            id: "b".to_string(),
            name: "B".to_string(),
            kind: KIND_PRODUCT.to_string(),
            parent_id: "a".to_string(),
            aliases: vec!["Shared".to_string()],
            ..Default::default()
        });
        registry.projects.push(RegistryProject {
            id: "c".to_string(),
            name: "C".to_string(),
            kind: "bogus".to_string(),
            ..Default::default()
        });
        registry.workspaces.push(RegistryWorkspace {
            id: "w1".to_string(),
            project_id: "a".to_string(),
            path: "F:/dup".to_string(),
            role: ROLE_ROOT.to_string(),
            source: SOURCE_MANUAL.to_string(),
            confidence: CONFIDENCE_EXACT.to_string(),
            created_at: String::new(),
        });
        registry.workspaces.push(RegistryWorkspace {
            id: "w2".to_string(),
            project_id: "b".to_string(),
            path: "F:/dup".to_string(),
            role: ROLE_ROOT.to_string(),
            source: SOURCE_MANUAL.to_string(),
            confidence: CONFIDENCE_EXACT.to_string(),
            created_at: String::new(),
        });
        Ok(())
    })
    .unwrap();

    let registry = Registry::load_from(&path).unwrap();
    let joined = registry.validate().join("\n");
    for needle in ["父子环", "别名", "同时绑定", "kind 无效"] {
        assert!(joined.contains(needle), "校验未报告 {needle}：\n{joined}");
    }
}

#[test]
fn worktree_hint_flags_release_and_mirror() {
    assert_eq!(worktree_hint("项目看板-release-966aeec"), ROLE_RELEASE);
    assert_eq!(worktree_hint("repo-worktree-a"), ROLE_WORKTREE);
    assert_eq!(worktree_hint("repo-mirror"), ROLE_MIRROR);
    assert_eq!(worktree_hint("work-log"), "");
}

#[test]
fn upsert_rejects_alias_conflict_and_cycle() {
    let (_dir, path) = temp_registry();
    let (root_id, sub_id) = seed(&path);

    let alias_error = Registry::update_at(&path, |registry| {
        upsert_project(
            registry,
            ProjectInput {
                name: "另一个".to_string(),
                aliases: vec!["work-log".to_string()],
                ..Default::default()
            },
        )
        .map(|_| ())
    })
    .unwrap_err()
    .to_string();
    assert!(alias_error.contains(ERR_ALIAS_CONFLICT), "{alias_error}");

    let cycle_error = Registry::update_at(&path, |registry| {
        upsert_project(
            registry,
            ProjectInput {
                id: root_id.clone(),
                name: "扩展仓库".to_string(),
                parent_id: ParentField::Set(sub_id.clone()),
                ..Default::default()
            },
        )
        .map(|_| ())
    })
    .unwrap_err()
    .to_string();
    assert!(cycle_error.contains(ERR_PARENT_CYCLE), "{cycle_error}");
}

#[test]
fn delete_project_refuses_with_children_or_paths() {
    let (_dir, path) = temp_registry();
    let (root_id, sub_id) = seed(&path);

    let children_error =
        Registry::update_at(&path, |registry| delete_project(registry, &root_id, false))
            .unwrap_err()
            .to_string();
    assert!(
        children_error.contains(ERR_PROJECT_HAS_CHILDREN),
        "{children_error}"
    );

    let paths_error =
        Registry::update_at(&path, |registry| delete_project(registry, &sub_id, false))
            .unwrap_err()
            .to_string();
    assert!(
        paths_error.contains(ERR_PROJECT_HAS_WORKSPACES),
        "{paths_error}"
    );

    Registry::update_at(&path, |registry| delete_project(registry, &sub_id, true)).unwrap();
    let registry = Registry::load_from(&path).unwrap();
    assert!(registry.project(&sub_id).is_none());
}

#[test]
fn bind_path_rejects_conflict_and_unbind_removes() {
    let (_dir, path) = temp_registry();
    let (_root_id, sub_id) = seed(&path);

    let conflict = Registry::update_at(&path, |registry| {
        bind_path(
            registry,
            BindInput {
                project_ref: sub_id.clone(),
                path: "F:/WebProjects/himind-extensions".to_string(),
                ..Default::default()
            },
        )
        .map(|_| ())
    })
    .unwrap_err()
    .to_string();
    assert!(conflict.contains(ERR_PATH_CONFLICT), "{conflict}");

    Registry::update_at(&path, |registry| {
        bind_path(
            registry,
            BindInput {
                path: "F:/WebProjects/himind-extensions/plugins/work-log".to_string(),
                unbind: true,
                ..Default::default()
            },
        )
        .map(|_| ())
    })
    .unwrap();
    let registry = Registry::load_from(&path).unwrap();
    assert!(registry.workspaces_of(&sub_id).is_empty());
}

#[test]
fn load_falls_back_to_backup_when_primary_is_broken() {
    let (_dir, path) = temp_registry();
    Registry::update_at(&path, |registry| {
        upsert_project(
            registry,
            ProjectInput {
                name: "第一版".to_string(),
                ..Default::default()
            },
        )
        .map(|_| ())
    })
    .unwrap();
    // 第二次写入会把第一版留成 .bak，然后把主文件写坏。
    Registry::update_at(&path, |registry| {
        upsert_project(
            registry,
            ProjectInput {
                name: "第二版".to_string(),
                ..Default::default()
            },
        )
        .map(|_| ())
    })
    .unwrap();
    fs::write(&path, b"{ broken json").unwrap();

    let registry = Registry::load_from(&path).expect("应回落到备份");
    assert_eq!(registry.projects.len(), 1);
    assert_eq!(registry.projects[0].name, "第一版");
}

#[test]
fn update_does_not_write_when_mutate_fails() {
    let (_dir, path) = temp_registry();
    Registry::update_at(&path, |registry| {
        upsert_project(
            registry,
            ProjectInput {
                name: "已写入".to_string(),
                ..Default::default()
            },
        )
        .map(|_| ())
    })
    .unwrap();

    let _ = Registry::update_at(&path, |registry| {
        registry.projects.push(RegistryProject {
            id: "ghost".to_string(),
            name: "不该落盘".to_string(),
            ..Default::default()
        });
        Err("故意失败".into())
    });

    let registry = Registry::load_from(&path).unwrap();
    assert_eq!(registry.projects.len(), 1);
    assert_eq!(registry.projects[0].name, "已写入");
}

#[test]
fn parent_field_distinguishes_missing_from_explicit_null() {
    let (_dir, path) = temp_registry();
    let (root_id, sub_id) = seed(&path);

    // ① 不传 parent_id：保持原上级。
    Registry::update_at(&path, |registry| {
        upsert_project(
            registry,
            ProjectInput {
                id: sub_id.clone(),
                name: "工作日志".to_string(),
                kind: KIND_SUBPROJECT.to_string(),
                ..Default::default()
            },
        )
        .map(|_| ())
    })
    .unwrap();
    let registry = Registry::load_from(&path).unwrap();
    assert_eq!(registry.project(&sub_id).unwrap().parent_id, root_id);

    // ② 显式 null：提升为根项目。
    Registry::update_at(&path, |registry| {
        upsert_project(
            registry,
            ProjectInput {
                id: sub_id.clone(),
                name: "工作日志".to_string(),
                kind: KIND_SUBPROJECT.to_string(),
                parent_id: ParentField::Set(String::new()),
                ..Default::default()
            },
        )
        .map(|_| ())
    })
    .unwrap();
    let registry = Registry::load_from(&path).unwrap();
    assert!(registry.project(&sub_id).unwrap().parent_id.is_empty());
}

#[test]
fn candidate_flow_records_and_accepts() {
    let (_dir, path) = temp_registry();
    let (root_id, _sub_id) = seed(&path);

    // 造一个真的带 .git 的目录，让 suggest 能推断出来。
    // 落点同样走可回退选择：这条用例要读真实目录，不能只靠内存里的路径。
    let (work_dir, registry_path) = temp_registry();
    let project_dir = work_dir.join("new-thing");
    fs::create_dir_all(project_dir.join(".git")).unwrap();
    let project_path = normalize_path(&project_dir.to_string_lossy()).unwrap();
    drop(registry_path);

    Registry::update_at(&path, |registry| {
        let suggestions = suggest_candidates(registry, &project_path);
        assert!(!suggestions.is_empty(), "应产出候选");
        record_candidates(registry, suggestions.clone());
        record_candidates(registry, suggestions);
        Ok(())
    })
    .unwrap();

    let registry = Registry::load_from(&path).unwrap();
    assert_eq!(registry.candidates.len(), 1);
    assert_eq!(registry.candidates[0].seen_count, 2);

    Registry::update_at(&path, |registry| {
        accept_candidate(registry, &project_path, "create", "新项目", &root_id).map(|_| ())
    })
    .unwrap();
    let registry = Registry::load_from(&path).unwrap();
    assert_eq!(registry.candidates[0].status, CANDIDATE_ACCEPTED);
    let created = registry
        .projects
        .iter()
        .find(|project| project.name == "新项目")
        .expect("应建出项目");
    assert_eq!(created.parent_id, root_id);
    assert_eq!(registry.workspaces_of(&created.id).len(), 1);
}

#[test]
fn cross_thread_updates_do_not_lose_writes() {
    // 每个能力调用都是一条独立的 Tauri 命令线程，锁必须在进程内真的生效，
    // 且 `.bak` 与原子写不能互相踩。
    let (_dir, path) = temp_registry();
    let workers = 8;
    let mut handles = Vec::new();
    for index in 0..workers {
        let path = path.clone();
        handles.push(std::thread::spawn(move || {
            Registry::update_at(&path, |registry| {
                upsert_project(
                    registry,
                    ProjectInput {
                        name: format!("并发项目 {index}"),
                        ..Default::default()
                    },
                )
                .map(|_| ())
            })
            .expect("并发写");
        }));
    }
    for handle in handles {
        handle.join().expect("线程不应 panic");
    }
    let registry = Registry::load_from(&path).unwrap();
    assert_eq!(
        registry.projects.len(),
        workers,
        "并发写丢了更新：{}",
        registry.projects.len()
    );
}

#[test]
fn capability_resolve_requires_explicit_workspace_root() {
    // 内核不读环境变量也不猜当前目录：缺参数就是明确错误，不是静默降级。
    let error = capability_resolve(&serde_json::json!({}), true).unwrap_err();
    assert!(error.to_string().contains("workspace_root"), "{error}");
}

#[test]
fn capability_context_reports_unmatched_brief() {
    let (_dir, path) = temp_registry();
    std::env::set_var("HIMIND_WORKSPACE_REGISTRY_FILE", &path);
    let value = capability_context(
        &serde_json::json!({ "workspace_root": "F:/definitely/not/registered" }),
        false,
    )
    .unwrap();
    std::env::remove_var("HIMIND_WORKSPACE_REGISTRY_FILE");
    assert_eq!(value["matched"], serde_json::Value::Bool(false));
    let brief = value["brief"].as_str().unwrap();
    assert!(brief.contains("未登记"), "{brief}");
    assert!(brief.contains("不要猜测项目名"), "{brief}");
}

/// 登记表必须落在 `data/` 下——`plugin-data/` 不进备份包，丢了就是丢了。
/// 这条断言钉住「数据位置」这个设计决定本身，而不只是路径字符串。
#[test]
fn registry_file_lives_under_data_for_backup_coverage() {
    // 用 `agent_home` 的默认分支（不设覆盖变量）算出真实落点。
    std::env::remove_var("HIMIND_WORKSPACE_REGISTRY_FILE");
    let path = registry_path();
    let components: Vec<String> = path
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_string())
        .collect();
    assert!(
        components.iter().any(|part| part == "data"),
        "登记表必须落在 data/ 下才进备份包：{}",
        path.display()
    );
    assert!(
        !components.iter().any(|part| part == "plugin-data"),
        "登记表不能落在 plugin-data/：{}",
        path.display()
    );
    assert!(path.ends_with("workspace-registry.json"));
}
