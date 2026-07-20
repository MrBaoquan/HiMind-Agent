use reqwest::blocking::Client;
use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::api::client::{heartbeat, load_or_register, poll_tasks};
use crate::approval::manager::ApprovalManager;
use crate::store::types::LocalWorkerStatus;
use crate::{execute_task, flush_report_outbox, Options, VERSION};

struct HeartbeatLoop {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for HeartbeatLoop {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn run_loop(
    options: Options,
    worker_status: Option<Arc<Mutex<LocalWorkerStatus>>>,
    approval_mgr: Option<Arc<ApprovalManager>>,
) -> Result<(), Box<dyn Error>> {
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    set_status(&worker_status, false, "", "正在连接 Dashboard 任务 Worker");
    let state = load_or_register(
        &client,
        &options.api_base,
        &options.state_path,
        VERSION,
        &options.enrollment_token,
    )?;
    options.set_agent_credential(&state.credential);
    set_status(&worker_status, true, &state.agent_id, "");
    flush_report_outbox(&client, &options, &state.agent_id);
    crate::app::plugin_manager::flush_status_outbox(&options, &state.agent_id);

    println!("agent {} connected to {}", state.agent_id, options.api_base);
    let heartbeat_stop = Arc::new(AtomicBool::new(false));
    let heartbeat_stop_for_thread = Arc::clone(&heartbeat_stop);
    let heartbeat_status = worker_status.clone();
    let heartbeat_client = client.clone();
    let heartbeat_options = options.clone();
    let heartbeat_agent_id = state.agent_id.clone();
    let heartbeat_credential = state.credential.clone();
    let heartbeat_interval = options.interval_seconds.max(1);
    let heartbeat_thread = thread::spawn(move || {
        while !heartbeat_stop_for_thread.load(Ordering::Relaxed) {
            match heartbeat(
                &heartbeat_client,
                &heartbeat_options.api_base,
                &heartbeat_agent_id,
                &heartbeat_credential,
            ) {
                Ok(true) => {
                    set_status(&heartbeat_status, true, &heartbeat_agent_id, "");
                    crate::app::plugin_manager::flush_status_outbox(
                        &heartbeat_options,
                        &heartbeat_agent_id,
                    );
                }
                Ok(false) => {
                    set_status(
                        &heartbeat_status,
                        false,
                        "",
                        "Dashboard Agent 凭据已失效，需要管理员重新授权配对",
                    );
                    break;
                }
                Err(error) => set_status(
                    &heartbeat_status,
                    false,
                    &heartbeat_agent_id,
                    &format!("Dashboard Agent 心跳失败：{error}"),
                ),
            }
            for _ in 0..heartbeat_interval {
                if heartbeat_stop_for_thread.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
    });
    let _heartbeat_loop = HeartbeatLoop {
        stop: heartbeat_stop,
        handle: Some(heartbeat_thread),
    };

    loop {
        let tasks = poll_tasks(
            &client,
            &options.api_base,
            &state.agent_id,
            &state.credential,
        )?;
        for task in tasks {
            execute_task(
                &client,
                &options,
                &state.agent_id,
                task,
                approval_mgr.as_deref(),
            )?;
        }
        if options.once {
            break;
        }
    }

    Ok(())
}

pub(crate) fn run_supervisor(
    options: Options,
    worker_status: Arc<Mutex<LocalWorkerStatus>>,
    approval_mgr: Option<Arc<ApprovalManager>>,
) {
    loop {
        match run_loop(
            options.clone(),
            Some(Arc::clone(&worker_status)),
            approval_mgr.clone(),
        ) {
            Ok(()) => {
                set_status(
                    &Some(Arc::clone(&worker_status)),
                    false,
                    "",
                    "Dashboard 任务 Worker 已停止",
                );
                break;
            }
            Err(error) => {
                let message = error.to_string();
                set_status(&Some(Arc::clone(&worker_status)), false, "", &message);
                eprintln!("agent worker stopped: {}", message);
                thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

fn set_status(
    status: &Option<Arc<Mutex<LocalWorkerStatus>>>,
    online: bool,
    agent_id: &str,
    error: &str,
) {
    if let Some(shared) = status {
        if let Ok(mut state) = shared.lock() {
            state.dashboard_worker_online = online;
            state.dashboard_agent_id = agent_id.to_string();
            state.dashboard_worker_error = error.to_string();
        }
    }
}
