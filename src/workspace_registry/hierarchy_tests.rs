use super::*;

fn sample() -> (Registry, SubprojectInput) {
    let mut registry = Registry::default();
    registry.updated_at = "2026-11-13T00:00:00Z".into();
    let root = upsert_project(
        &mut registry,
        ProjectInput {
            name: "IODevice".into(),
            ..Default::default()
        },
    )
    .unwrap();
    for path in [
        "O:/synthetic/IODevice",
        "O:/synthetic/IODevice/IOStudio",
        "F:/synthetic/UNIPlayer",
    ] {
        bind_path(
            &mut registry,
            BindInput {
                project_ref: root.id.clone(),
                path: path.into(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let input = SubprojectInput {
        parent_id: root.id,
        parent_name: Some("IO 设备平台".into()),
        expected_revision: registry.revision(),
        entries: vec![
            SubprojectEntry {
                workspace_id: registry.workspaces[1].id.clone(),
                name: "设备配置与编排".into(),
                aliases: vec!["IOStudio".into()],
                kind: "product".into(),
            },
            SubprojectEntry {
                workspace_id: registry.workspaces[2].id.clone(),
                name: "交互播放器".into(),
                aliases: vec!["UNIPlayer".into()],
                kind: "product".into(),
            },
        ],
    };
    (registry, input)
}

#[test]
fn hierarchy_preview_preserves_ids_paths_and_has_deterministic_child_ids() {
    let (registry, input) = sample();
    let before = registry.revision();
    let (next, preview) = plan(&registry, &input).unwrap();
    assert_eq!(registry.revision(), before);
    assert_eq!(next.projects.len(), 3);
    assert_eq!(next.workspaces.len(), 3);
    assert_eq!(next.project(&input.parent_id).unwrap().name, "IO 设备平台");
    assert!(next
        .children(&input.parent_id)
        .iter()
        .all(|child| child.parent_id == input.parent_id));
    assert_eq!(
        next.workspaces
            .iter()
            .map(|w| (&w.id, &w.path))
            .collect::<Vec<_>>(),
        registry
            .workspaces
            .iter()
            .map(|w| (&w.id, &w.path))
            .collect::<Vec<_>>()
    );
    let (_, again) = plan(&registry, &input).unwrap();
    assert_eq!(preview.changes[0].project.id, again.changes[0].project.id);
    assert_eq!(
        resolve_path(&next, "O:/synthetic/IODevice/IOStudio/file.txt")
            .unwrap()
            .unwrap()
            .project
            .display_name(),
        "设备配置与编排"
    );
    assert_eq!(next.find_project("IODevice").unwrap().id, input.parent_id);
}

#[test]
fn hierarchy_rejects_partial_conflicts_duplicates_and_stale_versions() {
    let (registry, mut input) = sample();
    let before = registry.revision();
    input.entries[1].aliases = vec!["IOStudio".into()];
    assert!(plan(&registry, &input).is_err());
    assert_eq!(registry.revision(), before);
    input.entries[1] = input.entries[0].clone();
    assert!(plan(&registry, &input).is_err());
    input.expected_revision = "stale".into();
    assert!(plan(&registry, &input)
        .unwrap_err()
        .to_string()
        .contains("WORKSPACE_REGISTRY_CHANGED"));
}

#[test]
fn hierarchy_atomic_apply_and_guarded_restore_use_synthetic_store_only() {
    let (registry, input) = sample();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".himind/test-tmp")
        .join(next_id("hierarchy"));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("workspace-registry.json");
    fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();
    let applied = apply_at(&path, &input).unwrap();
    let current = Registry::load_from(&path).unwrap();
    assert_eq!(current.projects.len(), 3);
    assert!(apply_at(&path, &input).is_err());
    assert!(restore_at(
        &path,
        applied.rollback_revision.as_deref().unwrap(),
        "wrong revision"
    )
    .is_err());
    assert_eq!(
        Registry::load_from(&path).unwrap().revision(),
        current.revision()
    );
    restore_at(
        &path,
        applied.rollback_revision.as_deref().unwrap(),
        applied.applied_revision.as_deref().unwrap(),
    )
    .unwrap();
    let restored = Registry::load_from(&path).unwrap();
    assert_eq!(restored.projects, registry.projects);
    assert_eq!(restored.workspaces, registry.workspaces);
    fs::remove_dir_all(&dir).unwrap();
}
