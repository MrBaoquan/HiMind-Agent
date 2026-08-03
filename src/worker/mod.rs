use reqwest::blocking::Client;
use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::api::client::{
    heartbeat_with_runtime_installations, load_or_register, poll_tasks, register_agent,
};
use crate::api::distribution::load_or_register as load_distribution;
use crate::approval::manager::ApprovalManager;
use crate::store::types::LocalWorkerStatus;
use crate::{execute_task, flush_report_outbox, Options, VERSION};

struct HeartbeatLoop {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

struct ExtensionReconcileLoop {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

struct UpdateCheckLoop {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for ExtensionReconcileLoop {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for HeartbeatLoop {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for UpdateCheckLoop {
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
    let mut state = if options.reenroll {
        register_agent(
            &client,
            &options.api_base,
            &options.state_path,
            VERSION,
            &options.enrollment_token,
        )
    } else {
        load_or_register(
            &client,
            &options.api_base,
            &options.state_path,
            VERSION,
            &options.enrollment_token,
        )
    }?;
    if crate::api::client::agent_credential_rotation_due(&state) {
        state = match crate::api::client::rotate_agent_credential(
            &client,
            &options.api_base,
            &options.state_path,
            &state,
        ) {
            Ok(rotated) => rotated,
            Err(error) => {
                eprintln!("Agent credential rotation deferred: {error}");
                load_or_register(
                    &client,
                    &options.api_base,
                    &options.state_path,
                    VERSION,
                    &options.enrollment_token,
                )?
            }
        };
    }
    options.set_agent_credential(&state.credential);
    let distribution_state =
        match load_distribution_client(&client, &options, &state.device_id, &state.agent_id) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("Distribution client registration deferred: {error}");
                None
            }
        };
    crate::api::oauth::cache_registration_access(&options, &state);
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
    let mut heartbeat_agent_state = state.clone();
    let heartbeat_interval = options.interval_seconds.max(1);
    let heartbeat_thread = thread::spawn(move || {
        let mut runtime_installations = crate::runtime::probe_installations();
        let mut last_runtime_probe = Instant::now();
        while !heartbeat_stop_for_thread.load(Ordering::Relaxed) {
            if crate::api::client::agent_credential_rotation_due(&heartbeat_agent_state) {
                heartbeat_agent_state = match crate::api::client::rotate_agent_credential(
                    &heartbeat_client,
                    &heartbeat_options.api_base,
                    &heartbeat_options.state_path,
                    &heartbeat_agent_state,
                ) {
                    Ok(rotated) => rotated,
                    Err(error) => {
                        eprintln!("Agent credential rotation deferred: {error}");
                        match load_or_register(
                            &heartbeat_client,
                            &heartbeat_options.api_base,
                            &heartbeat_options.state_path,
                            VERSION,
                            &heartbeat_options.enrollment_token,
                        ) {
                            Ok(recovered) => recovered,
                            Err(recovery_error) => {
                                set_status(
                                    &heartbeat_status,
                                    false,
                                    &heartbeat_agent_id,
                                    &format!("Dashboard Agent 凭据轮换恢复失败：{recovery_error}"),
                                );
                                break;
                            }
                        }
                    }
                };
                heartbeat_options.set_agent_credential(&heartbeat_agent_state.credential);
            }
            let heartbeat_credential = heartbeat_options.agent_credential();
            let remote_execution =
                crate::app::remote_execution::load(&heartbeat_options.state_path)
                    .unwrap_or_default()
                    .into();
            if last_runtime_probe.elapsed() >= Duration::from_secs(5 * 60) {
                runtime_installations = crate::runtime::probe_installations();
                last_runtime_probe = Instant::now();
            }
            match heartbeat_with_runtime_installations(
                &heartbeat_client,
                &heartbeat_options.api_base,
                &heartbeat_agent_id,
                &heartbeat_credential,
                Some(&runtime_installations),
                Some(&remote_execution),
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

    let update_stop = Arc::new(AtomicBool::new(false));
    let update_stop_for_thread = Arc::clone(&update_stop);
    let update_client = client.clone();
    let update_options = options.clone();
    let update_device_id = state.device_id.clone();
    let update_agent_id = state.agent_id.clone();
    let update_thread = thread::spawn(move || {
        let mut distribution_state = distribution_state;
        while !update_stop_for_thread.load(Ordering::Relaxed) {
            if distribution_state.is_none() {
                distribution_state = match load_distribution_client(
                    &update_client,
                    &update_options,
                    &update_device_id,
                    &update_agent_id,
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("Distribution client registration deferred: {error}");
                        None
                    }
                };
            }
            if let Some(state) = distribution_state.as_ref() {
                if let Err(error) = crate::app::update_manager::background_check(
                    &update_client,
                    &update_options,
                    state,
                ) {
                    eprintln!("background Agent update check failed: {error}");
                }
            }
            for _ in 0..60 {
                if update_stop_for_thread.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
    });
    let _update_check_loop = UpdateCheckLoop {
        stop: update_stop,
        handle: Some(update_thread),
    };

    let reconcile_stop = Arc::new(AtomicBool::new(false));
    let reconcile_stop_for_thread = Arc::clone(&reconcile_stop);
    let reconcile_options = options.clone();
    let reconcile_agent_id = state.agent_id.clone();
    let reconcile_thread = thread::spawn(move || {
        let mut generation = String::new();
        while !reconcile_stop_for_thread.load(Ordering::Relaxed) {
            if let Err(error) = crate::app::extension_reconciler::reconcile(
                &reconcile_options,
                &reconcile_agent_id,
                &mut generation,
            ) {
                eprintln!("extension reconcile failed: {error}");
            }
            for _ in 0..30 {
                if reconcile_stop_for_thread.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
    });
    let _extension_reconcile_loop = ExtensionReconcileLoop {
        stop: reconcile_stop,
        handle: Some(reconcile_thread),
    };

    loop {
        let credential = options.agent_credential();
        let tasks = poll_tasks(&client, &options.api_base, &state.agent_id, &credential)?;
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

fn load_distribution_client(
    client: &Client,
    options: &Options,
    device_id: &str,
    agent_id: &str,
) -> Result<Option<crate::api::distribution::DistributionState>, Box<dyn Error>> {
    load_distribution(
        client,
        &options.api_base,
        &crate::api::distribution::distribution_state_path(&options.state_path),
        &std::env::var("HIMIND_DISTRIBUTION_PRODUCT_KEY")
            .unwrap_or_else(|_| "himind-agent".to_string()),
        &std::env::var("HIMIND_DISTRIBUTION_CLIENT_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| device_id.to_string()),
        VERSION,
        &std::env::var("HIMIND_DISTRIBUTION_CHANNEL").unwrap_or_else(|_| "stable".to_string()),
        agent_id,
        &options.agent_credential(),
    )
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
