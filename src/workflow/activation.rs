//! Workflow development activation shared by Tauri and the Capability Gateway.
use super::WorkflowDraft;
use crate::app::extension_activation::ActivationReceipt;
use serde::Serialize;
use std::{error::Error, path::Path};

#[derive(Debug, Serialize)]
pub(crate) struct WorkflowActivationResult {
    pub draft: WorkflowDraft,
    pub activation: ActivationReceipt,
}

pub(crate) fn activate(draft: WorkflowDraft) -> Result<WorkflowActivationResult, Box<dyn Error>> {
    let _operation_lock = crate::app::extension_lifecycle::operation_lock(
        crate::app::extension_activation::ActivationKind::Workflow,
        &draft.package_id,
    )?;
    let workspace_key = crate::extension_projects::workspace_key_of(&draft.source_root);
    let target = crate::app::extension_activation::resolve_activation_target(
        &crate::store::paths::profile_name(),
        &draft.source_root,
        &[],
        None,
    )
    .map_err(|error| error.to_string())?;
    let activation_store = crate::app::extension_activation::ActivationStore::new(
        crate::store::paths::agent_home()
            .join("data")
            .join("extension-activations.json"),
    );
    let previous = activation_store
        .active_for(
            &target,
            crate::app::extension_activation::ActivationKind::Workflow,
            &draft.package_id,
        )
        .map_err(|error| error.to_string())?
        .filter(|receipt| {
            receipt.status == crate::app::extension_activation::ActivationStatus::Active
        });
    let previous_receipt = previous.clone();
    let previous_sha256 = previous.as_ref().and_then(|item| {
        if item.candidate_sha256 == draft.candidate_sha256 {
            item.previous_sha256.clone()
        } else {
            Some(item.candidate_sha256.clone())
        }
    });
    let now = chrono::Local::now().to_rfc3339();
    let receipt = crate::app::extension_activation::ActivationReceipt {
        schema_version: crate::app::extension_activation::EXTENSION_ACTIVATION_SCHEMA_VERSION
            .to_string(),
        kind: crate::app::extension_activation::ActivationKind::Workflow,
        extension_id: draft.package_id.clone(),
        version: draft.version.clone(),
        candidate_sha256: draft.candidate_sha256.clone(),
        project_id: format!("workflow:{}@{}", draft.package_id, workspace_key),
        workspace_key,
        target,
        status: crate::app::extension_activation::ActivationStatus::Active,
        previous_sha256,
        surfaces: vec![
            crate::app::extension_activation::ActivationSurface::visible("runtime"),
            crate::app::extension_activation::ActivationSurface::visible("workflow"),
        ],
        activated_at: now.clone(),
        updated_at: now,
    };
    let mut transaction = activation_store
        .stage(&receipt)
        .map_err(|error| format!("Workflow 激活暂存失败: {error}"))?;
    if let Err(error) = crate::app::workflow_manager::archive_development_candidate(
        &draft.candidate_path,
        &draft.package_id,
        &draft.candidate_sha256,
        draft.lock.as_ref(),
    ) {
        let _ = activation_store.rollback(&mut transaction);
        return Err(format!("Workflow 开发版归档失败: {error}").into());
    }
    if let Err(error) = crate::app::workflow_manager::install_development_archive(
        &draft.candidate_path,
        &draft.source_root,
        draft.lock.clone(),
    ) {
        let _ = activation_store.rollback(&mut transaction);
        return Err(format!("Workflow 开发版安装失败: {error}").into());
    }
    if let Err(error) = activation_store.commit(&mut transaction) {
        let _ = restore_workflow_development_receipt(
            previous_receipt.as_ref(),
            &draft.package_id,
            Path::new(&receipt.target.workspace_root),
        );
        let _ = activation_store.rollback(&mut transaction);
        return Err(format!("Workflow 激活提交失败: {error}").into());
    }
    Ok(crate::workflow::WorkflowActivationResult {
        draft,
        activation: receipt,
    })
}

fn restore_workflow_development_receipt(
    receipt: Option<&crate::app::extension_activation::ActivationReceipt>,
    package_id: &str,
    workspace_root: &Path,
) -> Result<Option<crate::workflow::InstalledWorkflow>, Box<dyn Error>> {
    let Some(receipt) = receipt else {
        let store = crate::workflow::WorkflowStore::open_default()?;
        return store.remove_development(package_id);
    };
    let archive = crate::app::workflow_manager::development_candidate_archive(
        package_id,
        &receipt.candidate_sha256,
    )?;
    let lock = crate::app::workflow_manager::development_candidate_lock(
        package_id,
        &receipt.candidate_sha256,
    )?;
    crate::app::workflow_manager::install_development_archive(&archive, workspace_root, lock)
        .map(Some)
}

pub(crate) fn rollback(
    package_id: &str,
    workspace_root: &Path,
) -> Result<crate::app::extension_activation::ActivationRollbackResult, Box<dyn Error>> {
    let _operation_lock = crate::app::extension_lifecycle::operation_lock(
        crate::app::extension_activation::ActivationKind::Workflow,
        package_id,
    )?;
    let target = crate::app::extension_activation::resolve_activation_target(
        &crate::store::paths::profile_name(),
        workspace_root,
        &[],
        None,
    )?;
    let store = crate::app::extension_activation::ActivationStore::new(
        crate::store::paths::agent_home()
            .join("data")
            .join("extension-activations.json"),
    );
    let active = store
        .active_for(
            &target,
            crate::app::extension_activation::ActivationKind::Workflow,
            package_id,
        )?
        .filter(|receipt| {
            receipt.status == crate::app::extension_activation::ActivationStatus::Active
        })
        .ok_or("Workflow 没有可回滚的开发激活记录")?;
    crate::app::extension_activation::ensure_receipt_workspace(&active, workspace_root)?;
    let previous_sha = active
        .previous_sha256
        .clone()
        .ok_or("Workflow 没有可回滚的上一开发版")?;
    let previous_archive =
        crate::app::workflow_manager::development_candidate_archive(package_id, &previous_sha)?;
    let previous_lock =
        crate::app::workflow_manager::development_candidate_lock(package_id, &previous_sha)?;
    let installed = crate::app::workflow_manager::install_development_archive(
        &previous_archive,
        workspace_root,
        previous_lock,
    )?;
    let now = chrono::Local::now().to_rfc3339();
    let receipt = crate::app::extension_activation::ActivationReceipt {
        schema_version: crate::app::extension_activation::EXTENSION_ACTIVATION_SCHEMA_VERSION
            .to_string(),
        kind: crate::app::extension_activation::ActivationKind::Workflow,
        extension_id: package_id.to_string(),
        version: installed.package.version,
        candidate_sha256: previous_sha,
        project_id: active.project_id.clone(),
        workspace_key: active.workspace_key.clone(),
        target: active.target.clone(),
        status: crate::app::extension_activation::ActivationStatus::Active,
        previous_sha256: Some(active.candidate_sha256.clone()),
        surfaces: vec![
            crate::app::extension_activation::ActivationSurface::visible("runtime"),
            crate::app::extension_activation::ActivationSurface::visible("workflow"),
        ],
        activated_at: now.clone(),
        updated_at: now,
    };
    let mut transaction = match store.stage(&receipt) {
        Ok(transaction) => transaction,
        Err(error) => {
            let _ = restore_workflow_development_receipt(Some(&active), package_id, workspace_root);
            return Err(error);
        }
    };
    if let Err(error) = store.commit(&mut transaction) {
        let _ = restore_workflow_development_receipt(Some(&active), package_id, workspace_root);
        let _ = store.rollback(&mut transaction);
        return Err(error);
    }
    Ok(crate::app::extension_activation::ActivationRollbackResult {
        activation: receipt,
    })
}
