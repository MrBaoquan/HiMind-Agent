use super::test_support::*;
use super::*;

fn setup() -> (Fixture, PathBuf, PathBuf) {
    let fixture = Fixture::new();
    let home = fixture.root.join("home");
    let alternate = fixture.root.join("alternate");
    (fixture, home, alternate)
}

fn prepare(fixture: &Fixture, home: &Path, task: &str) -> Result<CheckRuntimeResources, Box<dyn Error>> {
    CheckRuntimeResources::prepare_in(
        &fixture.workspace, "mission", task, &home.join("cache/mission-checks"),
        Some(&home.join("mission-runtime-storage.json")),
    )
}

#[test]
fn policy_routes_new_tasks_and_pins_existing_evidence_across_changes() {
    let (fixture, home, alternate) = setup();
    let old = prepare(&fixture, &home, "old").unwrap();
    let old_root = old.root.clone();
    fs::write(old_root.join("old-evidence.txt"), "keep").unwrap();
    let status = configure_storage_in(&home, &alternate).unwrap();
    assert_eq!(status["preferred_base"], alternate.to_str().unwrap());
    let new = prepare(&fixture, &home, "new").unwrap();
    let new_root = new.root.clone();
    assert!(new_root.starts_with(&alternate));
    fs::write(new_root.join("new-evidence.txt"), "keep-new").unwrap();
    let next = fixture.root.join("next-alternate");
    configure_storage_in(&home, &next).unwrap();
    let old_again = prepare(&fixture, &home, "old").unwrap();
    let new_again = prepare(&fixture, &home, "new").unwrap();
    assert_eq!(old_again.root, old_root);
    assert_eq!(new_again.root, new_root);
    let latest = prepare(&fixture, &home, "latest").unwrap();
    assert!(latest.root.starts_with(&next));
    assert_eq!(fs::read_to_string(old_root.join("old-evidence.txt")).unwrap(), "keep");
    assert_eq!(fs::read_to_string(new_root.join("new-evidence.txt")).unwrap(), "keep-new");
    // Parent environment and existing resource authority are unchanged.
    let mut command = Command::new("node");
    new.apply(&mut command).unwrap();
    assert!(new.input().get("approved_absolute_paths").is_none());
}

#[test]
fn legacy_evidence_is_adopted_in_place_and_alternate_orphans_are_rejected() {
    let (fixture, home, alternate) = setup();
    let mission_hash = format!("{:x}", Sha256::digest(b"mission"));
    let task_hash = format!("{:x}", Sha256::digest(b"legacy"));
    let legacy = home.join("cache/mission-checks").join(&mission_hash).join(&task_hash);
    fs::create_dir_all(&legacy).unwrap();
    fs::write(legacy.join("report.txt"), "legacy").unwrap();
    configure_storage_in(&home, &alternate).unwrap();
    let resources = prepare(&fixture, &home, "legacy").unwrap();
    assert_eq!(resources.root, legacy);
    assert_eq!(fs::read_to_string(legacy.join("report.txt")).unwrap(), "legacy");
    let orphan = alternate.join(&mission_hash).join(format!("{:x}", Sha256::digest(b"orphan")));
    fs::create_dir_all(&orphan).unwrap();
    fs::write(orphan.join("not-owned.txt"), "unowned").unwrap();
    assert!(prepare(&fixture, &home, "orphan").is_err());
    assert_eq!(fs::read_to_string(orphan.join("not-owned.txt")).unwrap(), "unowned");
}

#[test]
fn policy_rejects_ambiguous_json_without_changing_the_candidate() {
    let (fixture, home, alternate) = setup();
    configure_storage_in(&home, &alternate).unwrap();
    let policy = home.join("mission-runtime-storage.json");
    let good = fs::read(&policy).unwrap();
    for bytes in [
        br#"{"version":1,"version":1,"preferred_base":"C:/a"}"#.as_slice(),
        br#"{"version":1,"preferred_base":"relative"}"#,
        br#"{"version":2,"preferred_base":"C:/a"}"#,
        br#"{"version":1,"preferred_base":"C:/a","authority":"business"}"#,
        b"[]", b"null", b"not json",
    ] {
        fs::write(&policy, bytes).unwrap();
        assert!(prepare(&fixture, &home, "invalid").is_err());
    }
    fs::write(&policy, &good).unwrap();
    assert_eq!(fs::read_to_string(fixture.workspace.join("source.txt")).unwrap(), "confirmed candidate\n");
}

#[test]
fn storage_does_not_grant_business_or_allocation_control_paths() {
    let (fixture, home, alternate) = setup();
    configure_storage_in(&home, &alternate).unwrap();
    assert!(configure_storage_in(&home, &home.join("cache/mission-checks/.allocations-v1/unsafe")).is_err());
    configure_storage_in(&home, &fixture.workspace).unwrap();
    assert!(prepare(&fixture, &home, "overlap").is_err());
    assert_eq!(fs::read_dir(&fixture.workspace).unwrap().count(), 1);
}

#[test]
fn allocation_is_bound_to_workspace_and_corruption_cannot_redirect_it() {
    let (fixture, home, alternate) = setup();
    configure_storage_in(&home, &alternate).unwrap();
    let resources = prepare(&fixture, &home, "task").unwrap();
    let root = resources.root.clone();
    let other = fixture.root.join("other-workspace");
    fs::create_dir(&other).unwrap();
    assert!(CheckRuntimeResources::prepare_in(&other, "mission", "task",
        &home.join("cache/mission-checks"), Some(&home.join("mission-runtime-storage.json"))).is_err());
    drop(resources);
    let mission_hash = format!("{:x}", Sha256::digest(b"mission"));
    let task_hash = format!("{:x}", Sha256::digest(b"task"));
    let record = home.join("cache/mission-checks/.allocations-v1")
        .join(&mission_hash[..RESOURCE_PATH_HASH_LEN])
        .join(format!("{}.json", &task_hash[..RESOURCE_PATH_HASH_LEN]));
    let mut value: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    value["root"] = json!(other);
    fs::write(&record, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(prepare(&fixture, &home, "task").is_err());
    assert!(root.is_dir());
}

#[test]
fn concurrent_preparations_agree_on_one_immutable_allocation() {
    let (fixture, home, alternate) = setup();
    configure_storage_in(&home, &alternate).unwrap();
    std::thread::scope(|scope| {
        let first = scope.spawn(|| prepare(&fixture, &home, "concurrent").unwrap());
        let second = scope.spawn(|| prepare(&fixture, &home, "concurrent").unwrap());
        let (first, second) = (first.join().unwrap(), second.join().unwrap());
        assert_eq!(first.root, second.root);
        assert!(first.root.starts_with(&alternate));
    });
}

#[test]
fn a_new_storage_policy_cannot_nest_one_task_inside_another() {
    let (fixture, home, alternate) = setup();
    configure_storage_in(&home, &alternate).unwrap();
    let first = prepare(&fixture, &home, "first").unwrap();
    for base in [&first.root, &first.root.join("nested")] {
        assert!(configure_storage_in(&home, base).is_err());
    }
    // Even a manually altered host policy is checked again at allocation.
    let policy = StoragePolicy { version: 1, preferred_base: first.root.clone() };
    fs::write(home.join("mission-runtime-storage.json"), control_bytes(&policy).unwrap()).unwrap();
    assert!(prepare(&fixture, &home, "second").is_err());
    assert_eq!(fs::read_dir(&first.root).unwrap().count(), 2);
}

#[test]
fn oversized_control_json_is_rejected_before_publication() {
    let (fixture, home, alternate) = setup();
    configure_storage_in(&home, &alternate).unwrap();
    let policy = home.join("mission-runtime-storage.json");
    let before = fs::read(&policy).unwrap();
    let oversized = StoragePolicy { version: 1, preferred_base: PathBuf::from("长".repeat(6000)) };
    assert!(atomic_control_json(&policy, &oversized, true).is_err());
    assert_eq!(fs::read(&policy).unwrap(), before);
    assert_eq!(fs::read_dir(&home).unwrap().count(), 2);
    assert_eq!(fs::read_to_string(fixture.workspace.join("source.txt")).unwrap(), "confirmed candidate\n");
}

#[test]
fn interrupted_publication_preserves_temp_without_blocking_other_tasks() {
    let (fixture, home, alternate) = setup();
    configure_storage_in(&home, &alternate).unwrap();
    let first = prepare(&fixture, &home, "first").unwrap();
    let mission_hash = format!("{:x}", Sha256::digest(b"mission"));
    let folder = home.join("cache/mission-checks/.allocations-v1")
        .join(&mission_hash[..RESOURCE_PATH_HASH_LEN]);
    let temp = folder.join(".storage-123-0123456789abcdef.tmp");
    fs::write(&temp, "interrupted JSON bytes").unwrap();
    let second = prepare(&fixture, &home, "second").unwrap();
    assert_ne!(first.root, second.root);
    configure_storage_in(&home, &fixture.root.join("next")).unwrap();
    assert_eq!(fs::read_to_string(&temp).unwrap(), "interrupted JSON bytes");
    fs::write(folder.join(".storage-invalid.tmp"), "unknown").unwrap();
    assert!(prepare(&fixture, &home, "third").is_err());
}

#[cfg(windows)]
#[test]
fn status_reader_does_not_make_an_atomic_policy_update_fail() {
    let (_fixture, home, alternate) = setup();
    configure_storage_in(&home, &alternate).unwrap();
    let path = home.join("mission-runtime-storage.json");
    let file = control_options(false).unwrap().open(&path).unwrap();
    std::thread::scope(|scope| {
        let update = scope.spawn(|| configure_storage_in(&home, &home.join("new-base")).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(80));
        drop(file);
        assert_eq!(update.join().unwrap()["preferred_base"], home.join("new-base").to_str().unwrap());
    });
}

#[cfg(any(unix, windows))]
#[test]
fn redirected_policy_and_multiple_link_allocations_fail_closed() {
    let (fixture, home, alternate) = setup();
    let redirect = fixture.root.join("redirect");
    fs::create_dir(&alternate).unwrap();
    redirect_directory(&alternate, &redirect);
    assert!(configure_storage_in(&home, &redirect.join("cache")).is_err());
    assert!(!alternate.join("cache").exists());
    remove_redirect(&redirect);
    configure_storage_in(&home, &alternate).unwrap();
    let policy = home.join("mission-runtime-storage.json");
    let alias = fixture.root.join("policy-alias.json");
    fs::hard_link(&policy, &alias).unwrap();
    assert!(storage_status_in(&home).is_err());
    assert!(prepare(&fixture, &home, "hardlink").is_err());
    fs::remove_file(&alias).unwrap();
    let resources = prepare(&fixture, &home, "valid").unwrap();
    drop(resources);
    assert!(storage_status_in(&home).is_ok());
}
