use serde::Serialize;
use std::collections::BTreeMap;
use std::error::Error;

use crate::agent_core_contracts::{InteractionSource, LocalRunStatus};
use crate::store::local_runs::LocalRunLedger;

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct WorkflowMetrics {
    pub total_runs: u64,
    pub terminal_runs: u64,
    pub succeeded_runs: u64,
    pub failed_runs: u64,
    pub canceled_runs: u64,
    pub active_runs: u64,
    pub completion_rate: f64,
    pub average_duration_seconds: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub estimated_cost: f64,
    pub rework_runs: u64,
    pub retry_count: u64,
    pub approval_count: u64,
    pub feedback_wait_count: u64,
    pub last_run_at: String,
    #[serde(skip)]
    total_duration_seconds: u64,
}

pub(crate) fn workflow_metrics_by_package(
    ledger: &LocalRunLedger,
) -> Result<BTreeMap<String, WorkflowMetrics>, Box<dyn Error>> {
    let mut metrics = BTreeMap::<String, WorkflowMetrics>::new();
    for run in ledger
        .list_runs(500)?
        .into_iter()
        .filter(|run| run.source == InteractionSource::Workflow)
    {
        let Some(interaction) = ledger.get_interaction(&run.interaction_id)? else {
            continue;
        };
        let Some(package_id) = interaction
            .business_context
            .get("workflow")
            .and_then(|workflow| workflow.get("id"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let item = metrics.entry(package_id.to_string()).or_default();
        if run.parent_run_id.is_empty() {
            item.total_runs = item.total_runs.saturating_add(1);
            match run.status {
                LocalRunStatus::Succeeded => {
                    item.succeeded_runs = item.succeeded_runs.saturating_add(1);
                    item.terminal_runs = item.terminal_runs.saturating_add(1);
                }
                LocalRunStatus::Failed => {
                    item.failed_runs = item.failed_runs.saturating_add(1);
                    item.terminal_runs = item.terminal_runs.saturating_add(1);
                }
                LocalRunStatus::Canceled => {
                    item.canceled_runs = item.canceled_runs.saturating_add(1);
                    item.terminal_runs = item.terminal_runs.saturating_add(1);
                }
                LocalRunStatus::Queued | LocalRunStatus::Running | LocalRunStatus::Waiting => {
                    item.active_runs = item.active_runs.saturating_add(1);
                }
            }
            item.total_duration_seconds = item
                .total_duration_seconds
                .saturating_add(run_duration_seconds(&run.created_at, &run.updated_at));
        } else {
            item.rework_runs = item.rework_runs.saturating_add(1);
        }
        item.retry_count = item.retry_count.saturating_add(
            run.steps
                .iter()
                .map(|step| u64::from(step.attempt.saturating_sub(1)))
                .sum::<u64>(),
        );
        item.approval_count = item
            .approval_count
            .saturating_add(run.approvals.len() as u64);
        if let Some(usage) = run.usage.as_ref() {
            item.total_input_tokens = item.total_input_tokens.saturating_add(usage.input_tokens);
            item.total_output_tokens = item.total_output_tokens.saturating_add(usage.output_tokens);
            item.estimated_cost += usage.estimated_cost.max(0.0);
        }
        item.feedback_wait_count = item.feedback_wait_count.saturating_add(
            ledger
                .list_events(&run.run_id)?
                .into_iter()
                .filter(|event| {
                    event.event_type
                        == crate::agent_core_contracts::RuntimeEventType::QuestionRequested
                        || event
                            .payload
                            .get("waiting_for_feedback")
                            .and_then(serde_json::Value::as_bool)
                            == Some(true)
                })
                .count() as u64,
        );
        if run.updated_at > item.last_run_at {
            item.last_run_at = run.updated_at;
        }
    }
    for item in metrics.values_mut() {
        item.completion_rate = if item.terminal_runs == 0 {
            0.0
        } else {
            item.succeeded_runs as f64 / item.terminal_runs as f64
        };
        item.average_duration_seconds = if item.total_runs == 0 {
            0
        } else {
            item.total_duration_seconds / item.total_runs
        };
    }
    Ok(metrics)
}

fn run_duration_seconds(created_at: &str, updated_at: &str) -> u64 {
    let Ok(created) = created_at.parse::<u64>() else {
        return 0;
    };
    let Ok(updated) = updated_at.parse::<u64>() else {
        return 0;
    };
    updated.saturating_sub(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_core_contracts::{
        InteractionEnvelope, InteractionPrincipal, InteractionSource, InteractionTransport,
        LocalRun, LocalRunStep, LocalRunUsage, LocalStepStatus,
        INTERACTION_ENVELOPE_SCHEMA_VERSION, LOCAL_RUN_SCHEMA_VERSION,
    };

    fn ledger(name: &str) -> LocalRunLedger {
        LocalRunLedger::new(
            std::env::temp_dir()
                .join(format!(
                    "himind-workflow-metrics-{name}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ))
                .join("local-runs.sqlite3"),
        )
    }

    fn interaction(id: &str, workflow_id: &str) -> InteractionEnvelope {
        InteractionEnvelope {
            schema_version: INTERACTION_ENVELOPE_SCHEMA_VERSION.to_string(),
            interaction_id: id.to_string(),
            correlation_id: format!("corr-{id}"),
            idempotency_key: format!("idem-{id}"),
            source: InteractionSource::Workflow,
            transport: InteractionTransport::Local,
            principal: InteractionPrincipal {
                local_principal_id: "local-user".to_string(),
                delegated_user_id: String::new(),
                ai_client_id: String::new(),
            },
            agent_id: "agent-1".to_string(),
            device_id: String::new(),
            workspace_ref: String::new(),
            business_context: serde_json::json!({"workflow": {"id": workflow_id}}),
            reply_target: serde_json::json!({}),
            attachments: Vec::new(),
            policy_context: serde_json::json!({}),
            runtime_hint: String::new(),
            created_at: "100".to_string(),
        }
    }

    fn run(
        id: &str,
        interaction_id: &str,
        parent_run_id: &str,
        status: LocalRunStatus,
    ) -> LocalRun {
        LocalRun {
            schema_version: LOCAL_RUN_SCHEMA_VERSION.to_string(),
            run_id: id.to_string(),
            interaction_id: interaction_id.to_string(),
            parent_run_id: parent_run_id.to_string(),
            source: InteractionSource::Workflow,
            transport: InteractionTransport::Local,
            status,
            runtime_provider: "test".to_string(),
            workspace_ref: String::new(),
            current_step_id: String::new(),
            completion_mode: "full".to_string(),
            execution_plan: None,
            steps: vec![LocalRunStep {
                step_id: "STEP".to_string(),
                title: "Step".to_string(),
                status: LocalStepStatus::Succeeded,
                capability_id: String::new(),
                runtime_provider: String::new(),
                attempt: 2,
                started_at: "100".to_string(),
                finished_at: "120".to_string(),
                error: String::new(),
            }],
            approvals: Vec::new(),
            artifacts: Vec::new(),
            usage: Some(LocalRunUsage {
                input_tokens: 10,
                output_tokens: 5,
                estimated_cost: 0.25,
                currency: "CNY".to_string(),
                billing_owner: "user".to_string(),
            }),
            error: String::new(),
            created_at: "100".to_string(),
            updated_at: "160".to_string(),
        }
    }

    #[test]
    fn aggregates_top_level_and_rework_runs() {
        let ledger = ledger("aggregate");
        ledger
            .record_interaction(&interaction("interaction-1", "workflow-1"))
            .unwrap();
        ledger
            .record_interaction(&interaction("interaction-2", "workflow-1"))
            .unwrap();
        ledger
            .save_run(&run(
                "run-1",
                "interaction-1",
                "",
                LocalRunStatus::Succeeded,
            ))
            .unwrap();
        ledger
            .save_run(&run("run-2", "interaction-2", "", LocalRunStatus::Failed))
            .unwrap();
        ledger
            .save_run(&run(
                "run-child",
                "interaction-2",
                "run-2",
                LocalRunStatus::Succeeded,
            ))
            .unwrap();

        let metrics = workflow_metrics_by_package(&ledger).unwrap();
        let item = metrics.get("workflow-1").unwrap();
        assert_eq!(item.total_runs, 2);
        assert_eq!(item.succeeded_runs, 1);
        assert_eq!(item.failed_runs, 1);
        assert_eq!(item.completion_rate, 0.5);
        assert_eq!(item.average_duration_seconds, 60);
        assert_eq!(item.rework_runs, 1);
        assert_eq!(item.retry_count, 3);
        assert_eq!(item.total_input_tokens, 30);
        assert_eq!(item.total_output_tokens, 15);
        assert!((item.estimated_cost - 0.75).abs() < f64::EPSILON);
    }
}
