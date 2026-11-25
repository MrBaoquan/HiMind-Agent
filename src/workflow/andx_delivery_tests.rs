//! Optional cross-repository contract test. Modeled runtime/network results;
//! execution plans, receipt resolution, artifacts and approvals use the runner.
use super::*;
use crate::agent_core_contracts::LocalRunStatus;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::cell::RefCell;

struct Fixture {
    root: PathBuf,
    seen: RefCell<Vec<String>>,
}

impl WorkflowStepExecutor for Fixture {
    fn execute(
        &self,
        package: &WorkflowPackage,
        step: &WorkflowStep,
        input: &Value,
    ) -> Result<WorkflowStepExecution, Box<dyn Error>> {
        // Match the production gateway boundary before invoking its provider.
        let resolved_input = super::executor::resolve_completed_step_inputs(input);
        let input = &resolved_input;
        self.seen.borrow_mut().push(step.id.clone());
        let sha = "a".repeat(64);
        let output = match step.id.as_str() {
            "ACP-BINDING" => {
                json!({"credential_ref":"andx-fixture","project_id":"1","exhibit_id":"11","page_key":"exhibit-11","binding_revision":"binding-fixture"})
            }
            "ACP-SPEC" => {
                json!({"report":{"schema_version":"andx-custom-page-spec.v1","project_id":"1","exhibit_id":"11","page_key":"exhibit-11","version":"qa-v1","display_mode":"embedded","build_provider":"external","source_root":"dist","bridge":{"version":"1.0","required_capabilities":[]},"requirements":["欢迎屏"],"acceptance":["可预览"]}})
            }
            "ACP-PROJECT-INSPECT" => json!({"ok":true,"source_root":self.root.join("dist")}),
            "ACP-PACKAGE" => {
                json!({"ok":true,"output_zip":self.root.join("page.zip"),"sha256":sha,"artifact_bytes":12,"page_key":"exhibit-11","version":"qa-v1","source_kind":"static"})
            }
            "ACP-BRIDGE-TEST" => {
                json!({"schema":"andx-custom-page-bridge-test/v1","ok":true,"assertions":{"fixture":true}})
            }
            "ACP-PREVIEW" => {
                assert_eq!(input["binding_revision"], "binding-fixture");
                assert_eq!(input["expected_sha256"], sha);
                assert_eq!(input["artifact_zip"], json!(self.root.join("page.zip")));
                json!({"schema":"andx.page.delivery.v2","ok":true,"action":"preview","deployment":{"id":"10"},"artifact":{"sha256":sha},"expected_activation_id":"20","preview":{"previewUrl":"https://example.com/preview#unit"}})
            }
            "ACP-LIVE-VERIFY" => {
                json!({"report":{"schema_version":"andx-custom-page-acceptance.v1","ok":true,"page_url":"https://example.com/preview#unit","current_version":"qa-v1","artifact_sha256":sha,"screenshots":["fixture.png"],"console_errors":0,"network_failures":0,"channels":[],"manual_approved":false}})
            }
            "ACP-ACTIVATE" => {
                assert_eq!(input["binding_revision"], "binding-fixture");
                assert_eq!(input["deployment_id"], "10");
                assert_eq!(input["expected_activation_id"], "20");
                assert_eq!(input["expected_sha256"], sha);
                assert!(input["reason"]
                    .as_str()
                    .is_some_and(|value| !value.is_empty()));
                json!({"schema":"andx.page.delivery.v2","ok":true,"action":"activate","deployment":{"id":"10"},"activation":{"id":"21"}})
            }
            "ACP-QR" => json!({"ok":true,"output_png":self.root.join("qr.png")}),
            "ACP-EVIDENCE-PACK" => {
                assert_eq!(input["qr_png"], json!(self.root.join("qr.png")));
                json!({"schema_version":"andx-custom-page-evidence-manifest/v1","generated_at":"2026-11-25T00:00:00Z","workspace_root":self.root,"artifact_count":1,"total_bytes":12,"artifacts":[{"path":"page.zip","relative_path":"page.zip","artifact_kind":"build-package","file_name":"page.zip","size_bytes":12,"sha256":sha}]})
            }
            _ => json!({"ok":true}),
        };
        let id = match step.id.as_str() {
            "ACP-SPEC-RECEIPT" => Some("custom-page-spec"),
            "ACP-VERIFY-RECEIPT" => Some("acceptance-evidence"),
            "ACP-BRIDGE-TEST" => Some("bridge-test-report"),
            "ACP-PACKAGE" => Some("custom-page-candidate"),
            "ACP-PREVIEW" | "ACP-ACTIVATE" => Some("deployment-receipt"),
            "ACP-EVIDENCE-PACK" => Some("evidence-manifest"),
            _ => None,
        };
        let mut artifacts = Vec::new();
        if let Some(id) = id {
            let data = if step.capability_id == "andx.page.receipt.save" {
                &input["data"]
            } else {
                &output
            };
            let body = serde_json::to_vec(data)?;
            let path = self.root.join(format!("{}.json", step.id));
            fs::write(&path, &body)?;
            let definition = package.artifacts.iter().find(|a| a.id == id).unwrap();
            artifacts.push(WorkflowArtifactOutput {
                artifact_id: id.into(),
                artifact_type: definition.artifact_type.clone(),
                name: id.into(),
                uri: path.to_string_lossy().into(),
                sha256: format!("{:x}", Sha256::digest(&body)),
                size_bytes: body.len() as u64,
            });
        }
        Ok(WorkflowStepExecution {
            output,
            artifacts,
            usage: None,
        })
    }
}

#[test]
#[ignore = "requires ANDX_DELIVERY_WORKFLOW_ROOT pointing to extension source"]
fn andx_delivery_graph_resolves_receipts_and_requires_explicit_release() {
    let _env = crate::store::paths::test_env_lock();
    let source = PathBuf::from(
        std::env::var("ANDX_DELIVERY_WORKFLOW_ROOT").expect("workflow source required"),
    );
    let package = load_from_directory(&source).unwrap();
    for exit in [None, Some("delivered")] {
        let root = std::env::temp_dir().join(format!(
            "andx-delivery-{}-{}",
            std::process::id(),
            exit.unwrap_or("preview")
        ));
        fs::create_dir_all(root.join("dist")).unwrap();
        let mut input = json!({"workspace_root":root,"version":"qa-v1","source_root":"dist","bridge_test_dir":"bridge","artifact_zip":"page.zip","qr_output_png":"qr.png","steps":{"ACP-PREVIEW":{"expected_activation_id":"forged"}}});
        if let Some(exit) = exit {
            input["execution"] = json!({"exitpoint":exit});
        }
        let ledger = crate::store::local_runs::LocalRunLedger::new(root.join("ledger.sqlite3"));
        let runner = WorkflowRunner::with_ledger(ledger);
        let fixture = Fixture {
            root: root.clone(),
            seen: RefCell::new(Vec::new()),
        };
        let request = format!(
            "andx-fixture-{}-{}-{}",
            std::process::id(),
            exit.unwrap_or("preview"),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut run = runner.start("test", &package, &request, &input).unwrap();
        let mut approvals = Vec::new();
        for _ in 0..4 {
            let outcome = runner.run_ready(&package, run, &input, &fixture).unwrap();
            run = outcome.run;
            if run.status != LocalRunStatus::Waiting {
                break;
            }
            let step = run.current_step_id.clone();
            approvals.push(step.clone());
            run = runner.approve_step(run, &step).unwrap();
        }
        assert_eq!(run.status, LocalRunStatus::Succeeded, "{}", run.error);
        if exit.is_none() {
            assert!(approvals.is_empty());
            assert!(!fixture.seen.borrow().iter().any(|s| s == "ACP-ACTIVATE"));
            assert!(run
                .artifacts
                .iter()
                .any(|a| a.artifact_id == "deployment-receipt"));
        } else {
            assert_eq!(approvals, vec!["ACP-ACTIVATE", "ACP-WECHAT-REVIEW"]);
            assert_eq!(run.artifacts.len(), 6);
        }
        fs::remove_dir_all(root).unwrap();
    }
}
