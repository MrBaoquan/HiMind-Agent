use crate::capability::service::CapabilityGateway;
use crate::mcp::{spawn_registry_watcher, McpServer, McpServerAction};
use crate::mcp_ipc::{
    read_frame, start_service, write_frame, ServiceEvent, ServiceFrame, ServiceScope,
    ServiceStartOutcome, IPC_PROTOCOL_VERSION,
};
use crate::store::types::LocalWorkerStatus;
use crate::{Options, VERSION};
use std::error::Error;
use std::io::{BufReader, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

#[allow(unused_imports)]
pub(crate) use crate::mcp_ipc::*;

#[cfg(windows)]
static NEXT_SERVICE_SESSION: AtomicU64 = AtomicU64::new(1);
#[cfg(windows)]
static ACTIVE_REGISTRY_WATCHERS: AtomicUsize = AtomicUsize::new(0);

#[cfg(windows)]
pub(crate) fn active_registry_watcher_count() -> usize {
    ACTIVE_REGISTRY_WATCHERS.load(Ordering::Relaxed)
}

#[cfg(windows)]
struct RegistryBroadcaster {
    subscribers: Arc<Mutex<Vec<Sender<String>>>>,
}

#[cfg(windows)]
impl RegistryBroadcaster {
    fn new(gateway: CapabilityGateway) -> Self {
        let subscribers = Arc::new(Mutex::new(Vec::<Sender<String>>::new()));
        let updates = spawn_registry_watcher(gateway);
        let workers = Arc::clone(&subscribers);
        ACTIVE_REGISTRY_WATCHERS.fetch_add(1, Ordering::SeqCst);
        thread::spawn(move || {
            while let Ok(generation) = updates.recv() {
                let mut subscribers = workers.lock().unwrap_or_else(|error| error.into_inner());
                subscribers.retain(|sender| sender.send(generation.clone()).is_ok());
            }
            ACTIVE_REGISTRY_WATCHERS.fetch_sub(1, Ordering::SeqCst);
        });
        Self { subscribers }
    }

    fn subscribe(&self) -> Receiver<String> {
        let (sender, receiver) = channel();
        self.subscribers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(sender);
        receiver
    }
}

#[cfg(windows)]
pub(crate) fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let scope =
        ServiceScope::for_current_user(&crate::store::paths::profile_name(), &options.state_path)?;
    let runtime_dir = scope.runtime_dir(&crate::store::paths::agent_home());
    let gateway =
        CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())));
    serve_with_gateway(gateway, &scope, &runtime_dir, VERSION, None)
}

#[cfg(windows)]
pub(crate) fn serve_with_gateway(
    gateway: CapabilityGateway,
    scope: &ServiceScope,
    runtime_dir: &Path,
    agent_version: &str,
    max_connections: Option<usize>,
) -> Result<(), Box<dyn Error>> {
    let instance = match start_service(scope, runtime_dir, agent_version)? {
        ServiceStartOutcome::Started(instance) => instance,
        ServiceStartOutcome::AlreadyRunning(_) => {
            return Err("MCP service is already running for this scope".into())
        }
    };
    let endpoint = instance.discovery().endpoint.clone();
    let token = instance.discovery().token.clone();
    let registry = RegistryBroadcaster::new(gateway.clone());
    let mut accepted = 0_usize;
    loop {
        let stream = crate::mcp_pipe::accept(&endpoint)?;
        let session_id = format!(
            "{}-{}",
            std::process::id(),
            NEXT_SERVICE_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let profile = scope.profile().to_string();
        let state_hash = scope.state_hash().to_string();
        let gateway = gateway.clone();
        let token = token.clone();
        let agent_version = agent_version.to_string();
        let registry_updates = registry.subscribe();
        if max_connections.is_some() {
            handle_service_connection(
                stream,
                &gateway,
                &token,
                &profile,
                &state_hash,
                &agent_version,
                &session_id,
                registry_updates,
            )?;
            accepted += 1;
            if max_connections.is_some_and(|limit| accepted >= limit) {
                break;
            }
        } else {
            thread::spawn(move || {
                if let Err(error) = handle_service_connection(
                    stream,
                    &gateway,
                    &token,
                    &profile,
                    &state_hash,
                    &agent_version,
                    &session_id,
                    registry_updates,
                ) {
                    eprintln!("HiMind MCP service connection failed: {error}");
                }
            });
        }
    }
    drop(instance);
    Ok(())
}

#[cfg(windows)]
fn handle_service_connection(
    stream: crate::mcp_pipe::PipeStream,
    gateway: &CapabilityGateway,
    token: &str,
    profile: &str,
    state_hash: &str,
    agent_version: &str,
    session_id: &str,
    registry_updates: Receiver<String>,
) -> Result<(), Box<dyn Error>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let hello: ServiceFrame = read_frame(&mut reader)?.ok_or("MCP bridge closed before hello")?;
    let _client = match hello {
        ServiceFrame::Hello {
            protocol_version,
            token: client_token,
            profile: client_profile,
            state_hash: client_state_hash,
            client,
        } => {
            if protocol_version != IPC_PROTOCOL_VERSION {
                return write_service_error(
                    &mut writer,
                    Some(session_id),
                    None,
                    "protocol_mismatch",
                    &format!("unsupported IPC protocol: {protocol_version}"),
                );
            }
            if client_token != token || client_profile != profile || client_state_hash != state_hash
            {
                return write_service_error(
                    &mut writer,
                    Some(session_id),
                    None,
                    "handshake_rejected",
                    "MCP service scope or token does not match",
                );
            }
            client
        }
        _ => {
            return write_service_error(
                &mut writer,
                Some(session_id),
                None,
                "hello_required",
                "first bridge frame must be hello",
            )
        }
    };
    write_frame(
        &mut writer,
        &ServiceEvent::HelloOk {
            protocol_version: IPC_PROTOCOL_VERSION,
            service_pid: std::process::id(),
            agent_version: agent_version.to_string(),
            session_id: session_id.to_string(),
            registry_watchers: active_registry_watcher_count(),
        },
    )?;

    let mut server = McpServer::new_with_registry_updates(
        gateway.clone(),
        String::new(),
        Some(registry_updates),
    );
    loop {
        let Some(frame) = read_frame(&mut reader)? else {
            break;
        };
        match frame {
            ServiceFrame::Mcp {
                session_id: frame_session,
                request_id,
                payload,
            } => {
                if frame_session != session_id {
                    write_service_error(
                        &mut writer,
                        Some(session_id),
                        Some(request_id),
                        "session_mismatch",
                        "MCP frame session_id does not match the connection",
                    )?;
                    continue;
                }
                let line = serde_json::to_string(&payload)?;
                match server.process_line(&line)? {
                    McpServerAction::None => {}
                    McpServerAction::Respond {
                        response,
                        notifications,
                    } => {
                        write_frame(
                            &mut writer,
                            &ServiceEvent::Mcp {
                                session_id: session_id.to_string(),
                                request_id,
                                payload: response,
                            },
                        )?;
                        for notification in notifications {
                            write_frame(
                                &mut writer,
                                &ServiceEvent::Notification {
                                    session_id: session_id.to_string(),
                                    payload: notification,
                                },
                            )?;
                        }
                    }
                    McpServerAction::Exit => break,
                }
                for notification in server.poll_registry_notifications()? {
                    write_frame(
                        &mut writer,
                        &ServiceEvent::Notification {
                            session_id: session_id.to_string(),
                            payload: notification,
                        },
                    )?;
                }
            }
            ServiceFrame::Ping { request_id } => {
                write_frame(&mut writer, &ServiceEvent::Pong { request_id })?;
            }
            ServiceFrame::Cancel { request_id, .. } => {
                write_service_error(
                    &mut writer,
                    Some(session_id),
                    Some(request_id),
                    "cancel_not_supported",
                    "MCP cancellation is not implemented in this service revision",
                )?;
            }
            ServiceFrame::Close {
                session_id: frame_session,
            } => {
                if frame_session == session_id {
                    break;
                }
            }
            ServiceFrame::Hello { .. } => {
                write_service_error(
                    &mut writer,
                    Some(session_id),
                    None,
                    "duplicate_hello",
                    "hello may only be sent once per bridge connection",
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn write_service_error<W: Write>(
    writer: &mut W,
    session_id: Option<&str>,
    request_id: Option<u64>,
    code: &str,
    message: &str,
) -> Result<(), Box<dyn Error>> {
    write_frame(
        writer,
        &ServiceEvent::Error {
            session_id: session_id.map(str::to_string),
            request_id,
            code: code.to_string(),
            message: message.to_string(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use crate::store::types::LocalWorkerStatus;
    #[cfg(windows)]
    use crate::Options;
    use serde_json::Value;
    use std::fs;
    #[cfg(windows)]
    use std::io::BufReader;
    use std::io::Cursor;
    use std::path::PathBuf;
    #[cfg(windows)]
    use std::sync::Mutex;
    use std::sync::{mpsc, Arc, Barrier};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "himind-mcp-service-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn scope_is_isolated_by_profile_and_state() {
        let state = PathBuf::from(r"C:\agent\data\agent-state.json");
        let first = ServiceScope::for_identity("sid-a", "development", &state).unwrap();
        let second = ServiceScope::for_identity("sid-b", "development", &state).unwrap();
        let third = ServiceScope::for_identity("sid-a", "production", &state).unwrap();
        let fourth = ServiceScope::for_identity(
            "sid-a",
            "development",
            Path::new(r"C:\agent\data\other-state.json"),
        )
        .unwrap();
        assert_ne!(first.slug(), second.slug());
        assert_ne!(first.slug(), third.slug());
        assert_ne!(first.slug(), fourth.slug());
    }

    #[test]
    fn current_user_identity_is_available() {
        let scope =
            ServiceScope::for_current_user("development", Path::new("agent-state.json")).unwrap();
        assert!(!scope.identity_hash().trim().is_empty());
    }

    #[test]
    fn discovery_round_trips_and_is_removed_on_drop() {
        let root = test_dir("discovery");
        let scope =
            ServiceScope::for_identity("sid-a", "development", &root.join("agent-state.json"))
                .unwrap();
        let outcome = start_service(&scope, &root, "0.3.76").unwrap();
        let instance = match outcome {
            ServiceStartOutcome::Started(instance) => instance,
            ServiceStartOutcome::AlreadyRunning(_) => panic!("first start must own the service"),
        };
        let discovery = read_discovery(&root).expect("discovery file must exist");
        assert_eq!(discovery.protocol_version, IPC_PROTOCOL_VERSION);
        assert_eq!(discovery.profile, "development");
        assert_eq!(discovery.endpoint, scope.endpoint(&root));
        assert!(instance.discovery_path().is_file());
        drop(instance);
        assert!(!scope.discovery_path(&root).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parallel_start_has_exactly_one_owner() {
        let root = test_dir("singleton");
        let scope = Arc::new(
            ServiceScope::for_identity("sid-a", "development", &root.join("agent-state.json"))
                .unwrap(),
        );
        let barrier = Arc::new(Barrier::new(9));
        let (tx, rx) = mpsc::channel();
        for _ in 0..8 {
            let root = root.clone();
            let scope = Arc::clone(&scope);
            let barrier = Arc::clone(&barrier);
            let tx = tx.clone();
            thread::spawn(move || {
                barrier.wait();
                tx.send(start_service(&scope, &root, "0.3.76").unwrap())
                    .unwrap();
            });
        }
        barrier.wait();
        let outcomes = (0..8).map(|_| rx.recv().unwrap()).collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, ServiceStartOutcome::Started(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, ServiceStartOutcome::AlreadyRunning(_)))
                .count(),
            7
        );
        drop(outcomes);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn service_frames_round_trip() {
        let frame = ServiceFrame::Mcp {
            session_id: "session-1".to_string(),
            request_id: 7,
            payload: serde_json::json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &frame).unwrap();
        let mut reader = Cursor::new(bytes);
        let decoded: ServiceFrame = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(decoded, frame);
    }

    #[cfg(windows)]
    fn test_gateway(state_path: PathBuf) -> CapabilityGateway {
        let mut options = Options::from_env();
        options.set_api_base("http://127.0.0.1:9");
        options.set_mode(crate::app::runtime_mode::AgentMode::Independent);
        options.state_path = state_path;
        CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())))
    }

    #[cfg(windows)]
    fn read_mcp_event(
        reader: &mut BufReader<crate::mcp_pipe::PipeStream>,
        request_id: u64,
    ) -> Value {
        loop {
            match read_frame::<ServiceEvent, _>(reader).unwrap().unwrap() {
                ServiceEvent::Mcp {
                    request_id: actual,
                    payload,
                    ..
                } if actual == request_id => return payload,
                ServiceEvent::Notification { .. } | ServiceEvent::Progress { .. } => continue,
                event => panic!("unexpected service event: {event:?}"),
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn pipe_service_handles_hello_initialize_and_tools_list() {
        let root = test_dir("serve-handshake");
        let scope =
            ServiceScope::for_identity("sid-a", "development", &root.join("agent-state.json"))
                .unwrap();
        let gateway = test_gateway(root.join("agent-state.json"));
        let service_scope = scope.clone();
        let service_root = root.clone();
        let service = thread::spawn(move || {
            serve_with_gateway(gateway, &service_scope, &service_root, "0.3.76", Some(1)).unwrap();
        });
        let discovery = read_discovery_with_retry(&root).expect("service discovery must appear");
        let mut client = crate::mcp_pipe::connect(&discovery.endpoint).unwrap();
        let mut reader = BufReader::new(client.try_clone().unwrap());
        write_frame(
            &mut client,
            &ServiceFrame::Hello {
                protocol_version: IPC_PROTOCOL_VERSION,
                token: discovery.token.clone(),
                profile: discovery.profile.clone(),
                state_hash: discovery.state_hash.clone(),
                client: serde_json::json!({"name":"bridge-test"}),
            },
        )
        .unwrap();
        let hello: ServiceEvent = read_frame(&mut reader).unwrap().unwrap();
        let session_id = match hello {
            ServiceEvent::HelloOk {
                session_id,
                protocol_version,
                registry_watchers,
                ..
            } => {
                assert_eq!(protocol_version, IPC_PROTOCOL_VERSION);
                assert!(registry_watchers >= 1);
                session_id
            }
            event => panic!("expected hello_ok, got {event:?}"),
        };

        write_frame(
            &mut client,
            &ServiceFrame::Mcp {
                session_id: session_id.clone(),
                request_id: 1,
                payload: serde_json::json!({
                    "jsonrpc":"2.0",
                    "id":1,
                    "method":"initialize",
                    "params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"bridge-test","version":"1"}}
                }),
            },
        )
        .unwrap();
        let initialized = read_mcp_event(&mut reader, 1);
        assert_eq!(initialized["result"]["serverInfo"]["name"], "himind-agent");

        write_frame(
            &mut client,
            &ServiceFrame::Mcp {
                session_id: session_id.clone(),
                request_id: 2,
                payload: serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            },
        )
        .unwrap();
        write_frame(
            &mut client,
            &ServiceFrame::Mcp {
                session_id: session_id.clone(),
                request_id: 3,
                payload: serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
            },
        )
        .unwrap();
        let listed = read_mcp_event(&mut reader, 3);
        assert!(listed["result"]["tools"].is_array());

        write_frame(
            &mut client,
            &ServiceFrame::Close {
                session_id: session_id,
            },
        )
        .unwrap();
        drop(reader);
        drop(client);
        service.join().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn bridge_forwards_stdio_to_service_and_back() {
        let root = test_dir("bridge-e2e");
        let scope =
            ServiceScope::for_identity("sid-a", "development", &root.join("agent-state.json"))
                .unwrap();
        let gateway = test_gateway(root.join("agent-state.json"));
        let service_scope = scope.clone();
        let service_root = root.clone();
        let service = thread::spawn(move || {
            serve_with_gateway(gateway, &service_scope, &service_root, "0.3.76", Some(1)).unwrap();
        });
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"clientInfo\":{\"name\":\"bridge-e2e\",\"version\":\"1\"}}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"system.health\",\"arguments\":{}}}\n"
        );
        let mut input = Cursor::new(input.as_bytes());
        let mut output = Vec::new();
        crate::mcp_bridge::run_bridge_io(&scope, &root, &mut input, &mut output).unwrap();
        service.join().unwrap();
        let lines = String::from_utf8(output).unwrap();
        let responses = lines
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0]["result"]["serverInfo"]["name"], "himind-agent");
        assert!(responses[1]["result"]["tools"].is_array());
        assert_eq!(responses[2]["result"]["isError"], false);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn distinct_profiles_and_states_start_independently() {
        let root = test_dir("isolation");
        let first_scope =
            ServiceScope::for_identity("sid-a", "development", &root.join("dev-state.json"))
                .unwrap();
        let second_scope =
            ServiceScope::for_identity("sid-a", "production", &root.join("prod-state.json"))
                .unwrap();
        let first_runtime = first_scope.runtime_dir(&root);
        let second_runtime = second_scope.runtime_dir(&root);
        let first = start_service(&first_scope, &first_runtime, "0.3.76").unwrap();
        let second = start_service(&second_scope, &second_runtime, "0.3.76").unwrap();
        assert!(matches!(first, ServiceStartOutcome::Started(_)));
        assert!(matches!(second, ServiceStartOutcome::Started(_)));
        assert_ne!(
            first_scope.endpoint(&first_runtime),
            second_scope.endpoint(&second_runtime)
        );
        drop(first);
        drop(second);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn bridge_disconnect_does_not_stop_the_service() {
        let root = test_dir("disconnect-isolation");
        let scope =
            ServiceScope::for_identity("sid-a", "development", &root.join("agent-state.json"))
                .unwrap();
        let gateway = test_gateway(root.join("agent-state.json"));
        let service_scope = scope.clone();
        let service_root = root.clone();
        let service = thread::spawn(move || {
            serve_with_gateway(gateway, &service_scope, &service_root, "0.3.76", Some(2)).unwrap();
        });

        for request_id in [1_u64, 2_u64] {
            let discovery = read_discovery_with_retry(&root).expect("service discovery must exist");
            let mut client = crate::mcp_pipe::connect(&discovery.endpoint).unwrap();
            let mut reader = BufReader::new(client.try_clone().unwrap());
            write_frame(
                &mut client,
                &ServiceFrame::Hello {
                    protocol_version: IPC_PROTOCOL_VERSION,
                    token: discovery.token,
                    profile: discovery.profile,
                    state_hash: discovery.state_hash,
                    client: serde_json::json!({"name":"disconnect-test"}),
                },
            )
            .unwrap();
            let hello: ServiceEvent = read_frame(&mut reader).unwrap().unwrap();
            let session_id = match hello {
                ServiceEvent::HelloOk { session_id, .. } => session_id,
                event => panic!("unexpected hello event: {event:?}"),
            };
            write_frame(
                &mut client,
                &ServiceFrame::Mcp {
                    session_id: session_id.clone(),
                    request_id,
                    payload: serde_json::json!({
                        "jsonrpc":"2.0","id":request_id,"method":"initialize",
                        "params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"disconnect-test","version":"1"}}
                    }),
                },
            )
            .unwrap();
            let initialized = read_mcp_event(&mut reader, request_id);
            assert_eq!(initialized["result"]["serverInfo"]["name"], "himind-agent");
            write_frame(&mut client, &ServiceFrame::Close { session_id }).unwrap();
        }
        service.join().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn bridge_does_not_replay_when_service_closes_mid_request() {
        let root = test_dir("no-replay");
        let scope =
            ServiceScope::for_identity("sid-a", "development", &root.join("agent-state.json"))
                .unwrap();
        let discovery = ServiceDiscovery {
            schema_version: DISCOVERY_SCHEMA_VERSION,
            protocol_version: IPC_PROTOCOL_VERSION,
            pid: 1234,
            profile: scope.profile().to_string(),
            identity_hash: scope.identity_hash().to_string(),
            state_hash: scope.state_hash().to_string(),
            endpoint: scope.endpoint(&root),
            token: "test-token".to_string(),
            agent_version: "0.3.76".to_string(),
            started_at_unix: 1,
        };
        fs::create_dir_all(&root).unwrap();
        fs::write(
            scope.discovery_path(&root),
            serde_json::to_vec_pretty(&discovery).unwrap(),
        )
        .unwrap();
        let endpoint = discovery.endpoint.clone();
        let server = thread::spawn(move || {
            let mut stream = crate::mcp_pipe::accept(&endpoint).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let _: ServiceFrame = read_frame(&mut reader).unwrap().unwrap();
            write_frame(
                &mut stream,
                &ServiceEvent::HelloOk {
                    protocol_version: IPC_PROTOCOL_VERSION,
                    service_pid: 1234,
                    agent_version: "0.3.76".to_string(),
                    session_id: "fake-session".to_string(),
                    registry_watchers: 1,
                },
            )
            .unwrap();
            let request: ServiceFrame = read_frame(&mut reader).unwrap().unwrap();
            assert!(matches!(request, ServiceFrame::Mcp { .. }));
            drop(stream);
        });
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/call\",\"params\":{\"name\":\"plugin.install\",\"arguments\":{}}}\n"
        );
        let mut input = Cursor::new(input.as_bytes());
        let mut output = Vec::new();
        let error = crate::mcp_bridge::run_bridge_io(&scope, &root, &mut input, &mut output)
            .expect_err("service disconnect must surface as an error");
        assert!(error.to_string().contains("closed before returning"));
        assert!(output.is_empty());
        server.join().unwrap();
        let _ = fs::remove_dir_all(root);
    }
}
