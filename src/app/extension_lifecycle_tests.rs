use super::*;
use std::{fs, io::Write};
use zip::write::FileOptions;

struct Environment {
    root: PathBuf,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}
impl Environment {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("extension-lifecycle-{}", rand::random::<u64>()));
        fs::create_dir_all(root.join("home/data")).unwrap();
        let vars = [
            ("HIMIND_AGENT_HOME", root.join("home")),
            ("HIMIND_AGENT_PROFILE", PathBuf::from("activation-test")),
            ("HIMIND_PLUGIN_DRAFTS_DIR", root.join("home/plugin-drafts")),
            (
                "HIMIND_PLUGIN_DEVELOPMENT_VERSIONS_DIR",
                root.join("home/plugin-development-versions"),
            ),
            (
                "HIMIND_PLUGIN_DEVELOPMENT_REGISTRY",
                root.join("home/plugin-development.json"),
            ),
            (
                "HIMIND_EXTENSION_ACTIVATION_STORE",
                root.join("home/data/extension-activations.json"),
            ),
        ];
        let mut previous = Vec::new();
        for (key, value) in vars {
            previous.push((key, std::env::var_os(key)));
            std::env::set_var(key, value);
        }
        fs::write(
            root.join("home/data/distribution-state.json"),
            b"{\"sentinel\":true}",
        )
        .unwrap();
        Self { root, previous }
    }
    fn workspace(&self) -> PathBuf {
        let p = self.root.join("source");
        fs::create_dir_all(&p).unwrap();
        p
    }
    fn plugin(&self, version: &str) -> crate::plugin_authoring::PluginDraft {
        let package = self.root.join(format!("plugin-{version}.hmpkg"));
        let manifest=serde_json::to_vec(&json!({"id":"com.example.lifecycle","name":"Lifecycle fixture","author":"test","description":"synthetic activation test",
            "release_notes":"synthetic","version":version,"entry":"plugin.exe","runtime":"process-jsonrpc-stdio","min_agent_version":"0.3.0","governance":"optional",
            "capabilities":[],"permissions":[],"contributes":{"commands":[],"views":[]}})).unwrap();
        let binary = format!("synthetic-binary-{version}");
        let sums = format!(
            "{:x}  plugin.json\n{:x}  plugin.exe\n",
            Sha256::digest(&manifest),
            Sha256::digest(binary.as_bytes())
        );
        let mut archive = zip::ZipWriter::new(File::create(&package).unwrap());
        for (name, body) in [
            ("plugin.json", manifest.as_slice()),
            ("plugin.exe", binary.as_bytes()),
            ("checksums.sha256", sums.as_bytes()),
        ] {
            archive
                .start_file(
                    name,
                    FileOptions::default().compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            archive.write_all(body).unwrap();
        }
        archive.finish().unwrap();
        crate::plugin_authoring::save(crate::plugin_authoring::PluginDraftInput {
            package_path: package,
            revision_of_version: None,
            parent_submission_id: None,
        })
        .unwrap()
    }
    fn workflow(&self, version: &str) -> crate::workflow::WorkflowDraft {
        let source = self.workspace();
        fs::write(source.join("workflow.json"),serde_json::to_vec(&json!({"schema_version":"workflow_package.v1","id":"com.example.lifecycle-flow","version":version,"name":"Lifecycle fixture","min_agent_version":"0.3.0",
            "steps":[{"id":"START","title":"synthetic","kind":"manual","execution_mode":"sync"}],"artifacts":[],"ui":{"mode":"standard"}})).unwrap()).unwrap();
        crate::workflow::save_authoring_candidate(&source).unwrap()
    }
    fn assert_no_distribution_write(&self) {
        assert_eq!(
            fs::read(self.root.join("home/data/distribution-state.json")).unwrap(),
            b"{\"sentinel\":true}"
        );
    }
}
impl Drop for Environment {
    fn drop(&mut self) {
        for (key, value) in self.previous.iter().rev() {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn plugin_input(draft: &crate::plugin_authoring::PluginDraft) -> ActivationInput {
    ActivationInput {
        id: draft.manifest.id.clone(),
        version: draft.manifest.version.clone(),
        candidate_sha256: draft.candidate_sha256.clone(),
        workspace_root: draft.workspace_path.clone().unwrap(),
    }
}
fn workflow_input(draft: &crate::workflow::WorkflowDraft) -> ActivationInput {
    ActivationInput {
        id: draft.package_id.clone(),
        version: draft.version.clone(),
        candidate_sha256: draft.candidate_sha256.clone(),
        workspace_root: draft.source_root.clone(),
    }
}

#[test]
fn extension_activation_plugin_pins_workspace_hash_and_confirmed_archive_and_preserves_rollback() {
    let _lock = crate::store::paths::test_env_lock();
    let env = Environment::new();
    let first = env.plugin("1.0.0");
    assert!(activate_plugin(plugin_input(&first))
        .unwrap_err()
        .to_string()
        .contains("tested and confirmed"));
    crate::plugin_authoring::test(&first.manifest.id, &first.manifest.version).unwrap();
    assert!(activate_plugin(plugin_input(&first))
        .unwrap_err()
        .to_string()
        .contains("tested and confirmed"));
    let first =
        crate::plugin_authoring::confirm(&first.manifest.id, &first.manifest.version).unwrap();
    let mut changed = plugin_input(&first);
    changed.candidate_sha256 = "0".repeat(64);
    assert!(activate_plugin(changed)
        .unwrap_err()
        .to_string()
        .contains("identity changed"));
    let mut foreign = plugin_input(&first);
    foreign.workspace_root = env.workspace();
    assert!(activate_plugin(foreign)
        .unwrap_err()
        .to_string()
        .contains("different workspace"));
    let mut invalid = plugin_input(&first);
    invalid.id = "..".into();
    assert!(activate_plugin(invalid).is_err());
    // A modified test directory is not the activated artifact: read the pinned archive again.
    fs::write(
        first.development_path.as_ref().unwrap().join("plugin.exe"),
        b"not-the-candidate",
    )
    .unwrap();
    let one = activate_plugin(plugin_input(&first)).unwrap();
    let slot = PathBuf::from(one["draft"]["development_path"].as_str().unwrap());
    assert_eq!(
        fs::read(slot.join("plugin.exe")).unwrap(),
        b"synthetic-binary-1.0.0"
    );
    assert_eq!(
        crate::capability::plugin::find_plugin(&first.manifest.id)
            .unwrap()
            .unwrap()
            .version,
        "1.0.0"
    );
    let second = env.plugin("1.0.1");
    crate::plugin_authoring::test(&second.manifest.id, &second.manifest.version).unwrap();
    let second =
        crate::plugin_authoring::confirm(&second.manifest.id, &second.manifest.version).unwrap();
    let two = activate_plugin(plugin_input(&second)).unwrap();
    assert_eq!(two["activation"]["previous_sha256"], first.candidate_sha256);
    let repeat = activate_plugin(plugin_input(&second)).unwrap();
    assert_eq!(
        repeat["activation"]["previous_sha256"],
        first.candidate_sha256
    );
    let before = fs::read(env.root.join("home/plugin-development.json")).unwrap();
    fs::write(&second.candidate_path, b"changed archive").unwrap();
    assert!(activate_plugin(plugin_input(&second))
        .unwrap_err()
        .to_string()
        .contains("archive changed"));
    assert_eq!(
        fs::read(env.root.join("home/plugin-development.json")).unwrap(),
        before
    );
    assert!(rollback(RollbackInput {
        kind: RollbackKind::Plugin,
        id: second.manifest.id.clone(),
        candidate_sha256: first.candidate_sha256.clone(),
        workspace_root: second.workspace_path.clone().unwrap()
    })
    .is_err());
    let restored = rollback(RollbackInput {
        kind: RollbackKind::Plugin,
        id: second.manifest.id.clone(),
        candidate_sha256: second.candidate_sha256,
        workspace_root: second.workspace_path.unwrap(),
    })
    .unwrap();
    assert_eq!(
        restored["activation"]["candidate_sha256"],
        first.candidate_sha256
    );
    assert_eq!(list().unwrap()["activations"].as_array().unwrap().len(), 1);
    env.assert_no_distribution_write();
}

#[test]
fn extension_activation_workflow_uses_development_slot_and_keeps_confirm_separate() {
    let _lock = crate::store::paths::test_env_lock();
    let env = Environment::new();
    let first = env.workflow("1.0.0");
    assert!(activate_workflow(workflow_input(&first)).is_err());
    crate::workflow::test_authoring_candidate_with_capabilities(
        &first.package_id,
        &first.version,
        &[],
    )
    .unwrap();
    assert!(activate_workflow(workflow_input(&first))
        .unwrap_err()
        .to_string()
        .contains("tested and confirmed"));
    let first = crate::workflow::confirm_authoring_candidate_with_capabilities(
        &first.package_id,
        &first.version,
        &[],
    )
    .unwrap();
    assert!(crate::workflow::WorkflowStore::open_default()
        .unwrap()
        .list()
        .unwrap()
        .is_empty());
    let one = activate_workflow(workflow_input(&first)).unwrap();
    assert_eq!(
        one["activation"]["candidate_sha256"],
        first.candidate_sha256
    );
    let installed = crate::workflow::WorkflowStore::open_default()
        .unwrap()
        .load_enabled_for_run(&first.package_id)
        .unwrap();
    assert!(installed.development);
    assert_eq!(installed.package.version, "1.0.0");
    let second = env.workflow("1.0.1");
    let second = crate::workflow::confirm_authoring_candidate_with_capabilities(
        &second.package_id,
        &second.version,
        &[],
    )
    .unwrap();
    activate_workflow(workflow_input(&second)).unwrap();
    let repeat = activate_workflow(workflow_input(&second)).unwrap();
    assert_eq!(
        repeat["activation"]["previous_sha256"],
        first.candidate_sha256
    );
    let mut foreign = workflow_input(&second);
    foreign.workspace_root = env.root.clone();
    assert!(activate_workflow(foreign).is_err());
    let draft_before = fs::read(
        env.root
            .join("home/workflow-drafts")
            .join(&second.package_id)
            .join(&second.version)
            .join("draft.json"),
    )
    .unwrap();
    fs::write(&second.candidate_path, b"tampered archive").unwrap();
    assert!(
        crate::workflow::confirm_authoring_candidate_with_capabilities(
            &second.package_id,
            &second.version,
            &[]
        )
        .unwrap_err()
        .to_string()
        .contains("候选包已变化")
    );
    assert!(activate_workflow(workflow_input(&second)).is_err());
    assert_eq!(
        fs::read(
            env.root
                .join("home/workflow-drafts")
                .join(&second.package_id)
                .join(&second.version)
                .join("draft.json")
        )
        .unwrap(),
        draft_before
    );
    let restored = rollback(RollbackInput {
        kind: RollbackKind::Workflow,
        id: second.package_id.clone(),
        candidate_sha256: second.candidate_sha256,
        workspace_root: second.source_root,
    })
    .unwrap();
    assert_eq!(
        restored["activation"]["candidate_sha256"],
        first.candidate_sha256
    );
    assert_eq!(
        crate::workflow::WorkflowStore::open_default()
            .unwrap()
            .load_enabled_for_run(&first.package_id)
            .unwrap()
            .package
            .version,
        "1.0.0"
    );
    env.assert_no_distribution_write();
}

#[test]
fn extension_activation_contract_is_local_and_mutations_require_normal_approval() {
    use crate::capability::{
        service::CapabilityGateway,
        types::{CapabilityAvailability, InvocationContext, InvocationSource},
    };
    let _lock = crate::store::paths::test_env_lock();
    let _env = Environment::new();
    let mut options = crate::Options::from_env();
    options.set_mode(crate::app::runtime_mode::AgentMode::Independent);
    let gateway = CapabilityGateway::new(
        options,
        std::sync::Arc::new(std::sync::Mutex::new(
            crate::store::types::LocalWorkerStatus::default(),
        )),
    );
    let caps = gateway
        .list_capabilities(&InvocationContext::new(
            InvocationSource::Mcp,
            "activation-contract",
        ))
        .unwrap();
    for id in [
        "extension.plugin.candidate.activate",
        "extension.workflow.candidate.activate",
        "extension.activation.list",
        "extension.activation.rollback",
    ] {
        let cap = caps.iter().find(|c| c.id == id).unwrap();
        assert_eq!(cap.availability, CapabilityAvailability::Local);
        assert_eq!(cap.version, CONTRACT_VERSION);
        assert!(cap.required_scope.is_none());
        assert_eq!(cap.approval_required, !id.ends_with(".list"));
        assert_eq!(cap.input_schema["additionalProperties"], false);
        if id.ends_with(".activate") {
            let validator = jsonschema::validator_for(&cap.input_schema).unwrap();
            assert!(!validator
                .is_valid(&json!({"id":"demo","version":"1.0.0","workspace_root":"F:/test"})));
            assert!(!validator.is_valid(&json!({"id":"demo","version":"1.0.0","workspace_root":"F:/test","candidate_sha256":"bad"})));
            assert!(validator.is_valid(&json!({"id":"demo","version":"1.0.0","workspace_root":"F:/test","candidate_sha256":"a".repeat(64)})));
        }
    }
}

#[test]
fn extension_activation_rejected_approval_leaves_candidate_inactive_and_reviews_pin() {
    use crate::capability::{
        service::CapabilityGateway,
        types::{InvocationContext, InvocationSource},
    };
    let _lock = crate::store::paths::test_env_lock();
    let env = Environment::new();
    let workflow = env.workflow("1.0.0");
    let workflow = crate::workflow::confirm_authoring_candidate_with_capabilities(
        &workflow.package_id,
        &workflow.version,
        &[],
    )
    .unwrap();
    let input = workflow_input(&workflow);
    let value = json!({"id":input.id,"version":input.version,"candidate_sha256":input.candidate_sha256,"workspace_root":input.workspace_root});
    let cid = "extension.workflow.candidate.activate";
    let target = crate::approval::policy::target_description(cid, &value);
    assert!(target.contains(&workflow.candidate_sha256));
    assert!(target.contains(&workflow.package_id));
    assert!(
        serde_json::from_str::<Value>(&target).unwrap()["workspace_root"]
            == value["workspace_root"]
    );
    let mut settings = crate::approval::types::ApprovalSettings::default();
    settings.rules.insert(cid.into(), "auto_deny".into());
    fs::write(
        env.root.join("home/approval-settings.json"),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    let mut options = crate::Options::from_env();
    options.set_mode(crate::app::runtime_mode::AgentMode::Independent);
    let approvals = std::sync::Arc::new(crate::approval::manager::ApprovalManager::new());
    let gateway = CapabilityGateway::new_with_approval_manager(
        options,
        std::sync::Arc::new(std::sync::Mutex::new(
            crate::store::types::LocalWorkerStatus::default(),
        )),
        approvals,
    );
    let result = gateway.invoke(
        &InvocationContext::new(InvocationSource::Mcp, "activation-denied"),
        cid,
        value,
    );
    assert!(result.is_err());
    assert!(!env
        .root
        .join("home/data/extension-activations.json")
        .exists());
    assert!(crate::workflow::WorkflowStore::open_default()
        .unwrap()
        .list()
        .unwrap()
        .is_empty());
    env.assert_no_distribution_write();
}
