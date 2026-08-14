use serde_json::{json, Value};
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::write::FileOptions;

use crate::api::client::load_agent_state;
use crate::store::types::LocalWorkerStatus;
use crate::{Options, VERSION};

pub(crate) fn export_bundle(
    destination: &Path,
    options: &Options,
    worker: &LocalWorkerStatus,
) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(destination)?;
    let mut archive = zip::ZipWriter::new(file);
    let zip_options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    archive.start_file("diagnostics.json", zip_options)?;
    archive.write_all(serde_json::to_vec_pretty(&snapshot(options, worker))?.as_slice())?;

    let log_dir = crate::store::paths::agent_home().join("logs");
    for name in [
        "agent-events.jsonl",
        "agent-events.jsonl.1",
        "agent-events.jsonl.2",
    ] {
        let path = log_dir.join(name);
        if !path.is_file() {
            continue;
        }
        archive.start_file(format!("logs/{name}"), zip_options)?;
        let mut source = fs::File::open(path)?;
        let mut content = Vec::new();
        source.read_to_end(&mut content)?;
        archive.write_all(&content)?;
    }

    archive.start_file("feedback.txt", zip_options)?;
    archive.write_all(
        "请补充以下信息后提交诊断包：\r\n\
         1. 问题发生的大致时间\r\n\
         2. 当时执行的操作\r\n\
         3. 是否安装加密或终端安全软件，以及产品名称\r\n\
         4. 问题是否可稳定复现\r\n\
         诊断包不会收集密码、Agent credential 或 OAuth token。\r\n"
            .as_bytes(),
    )?;
    archive.finish()?;
    Ok(destination.to_path_buf())
}

fn snapshot(options: &Options, worker: &LocalWorkerStatus) -> Value {
    let state = match load_agent_state(&options.state_path) {
        Ok(state) => json!({
            "readable": true,
            "agent_id": state.agent_id,
            "device_id": state.device_id,
            "credential_present": !state.credential.is_empty(),
            "credential_pending": !state.credential_pending.is_empty(),
            "credential_updated_at": state.credential_updated_at,
        }),
        Err(error) => json!({ "readable": false, "error": error.to_string() }),
    };
    let authorization = match crate::api::oauth::authorization_snapshot(&options.state_path) {
        Ok(Some(value)) => json!({
            "readable": true,
            "present": true,
            "agent_id": value.agent_id,
            "user_id": value.user_id,
            "scope": value.scope,
            "refresh_expires_at": value.refresh_expires_at,
            "updated_at": value.updated_at,
            "last_verified_at": value.last_verified_at,
        }),
        Ok(None) => json!({ "readable": true, "present": false }),
        Err(error) => json!({ "readable": false, "error": error.to_string() }),
    };
    let state_file = &options.state_path;
    json!({
        "generated_at": unix_now(),
        "agent": {
            "version": VERSION,
            "profile": crate::store::paths::profile_name(),
            "api_base": sanitized_api_base(&options.api_base),
            "local_port": options.local_port,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "native_dpapi": cfg!(windows),
        },
        "worker": {
            "online": worker.dashboard_worker_online,
            "agent_id": worker.dashboard_agent_id,
            "error": crate::approval::manager::redact_message(&worker.dashboard_worker_error),
            "local_service_online": worker.local_service_online,
            "local_service_error": crate::approval::manager::redact_message(&worker.local_service_error),
        },
        "storage": {
            "state_file_exists": state_file.is_file(),
            "state_backup_exists": crate::store::atomic_file::backup_path(state_file).is_file(),
            "authorization_file_exists": state_file.with_file_name("agent-user-authorization.json").is_file(),
        },
        "device_identity": state,
        "user_authorization": authorization,
    })
}

fn sanitized_api_base(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return value.to_string();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string().trim_end_matches('/').to_string()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::export_bundle;
    use crate::api::client::save_agent_state;
    use crate::api::types::AgentState;
    use crate::store::types::LocalWorkerStatus;
    use crate::Options;
    use std::fs;
    use std::io::Read;
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, RwLock};

    #[test]
    fn bundle_contains_health_data_without_credentials() {
        let root = std::env::temp_dir().join(format!(
            "himind-diagnostics-{}-{}",
            std::process::id(),
            super::unix_now()
        ));
        fs::create_dir_all(&root).unwrap();
        let state_path = root.join("agent-state.json");
        let credential = "device-credential-that-must-not-leak";
        save_agent_state(
            &state_path,
            &AgentState {
                agent_id: "agt-test".to_string(),
                credential: credential.to_string(),
                credential_protected: String::new(),
                credential_pending: String::new(),
                credential_pending_protected: String::new(),
                credential_updated_at: super::unix_now(),
                device_id: "device-test".to_string(),
                access_token: String::new(),
                access_token_expires_in: 0,
                access_scope: String::new(),
                user_id: String::new(),
            },
        )
        .unwrap();
        let options = Options {
            api_base: "http://user:password@127.0.0.1:18083?token=secret".to_string(),
            state_path,
            once: false,
            interval_seconds: 10,
            local_app: true,
            local_port: 18182,
            reenroll: false,
            enrollment_token: String::new(),
            agent_credential: Arc::new(RwLock::new(String::new())),
            identity_generation: Arc::new(AtomicU64::new(0)),
            platform_access: Arc::new(RwLock::new(None)),
            task_execution: Arc::new(RwLock::new(None)),
        };
        let destination = root.join("diagnostics.zip");
        export_bundle(&destination, &options, &LocalWorkerStatus::default()).unwrap();
        let file = fs::File::open(&destination).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut diagnostics = String::new();
        archive
            .by_name("diagnostics.json")
            .unwrap()
            .read_to_string(&mut diagnostics)
            .unwrap();
        assert!(diagnostics.contains("agt-test"));
        assert!(!diagnostics.contains(credential));
        assert!(!diagnostics.contains("password"));
        assert!(!diagnostics.contains("token=secret"));
        let _ = fs::remove_dir_all(root);
    }
}
