//! Discovery of authenticated proxies owned by the desktop host, for headless readers.
//! Only Agent-owned control references are stored; no DSH private files are inspected.
use crate::app::builtin_ai_proxy::BuiltinAiProxyControl;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct Host {
    version: u8,
    pid: u32,
    started_at: u64,
    profile: String,
    protected_controls: String,
}

fn directory() -> PathBuf {
    crate::store::paths::agent_home()
        .join("runtime")
        .join("session-history-hosts")
}

pub(crate) fn publish(controls: &[(String, String)]) -> Result<(), String> {
    publish_at(&directory(), &crate::store::paths::profile_name(), controls)
}

fn publish_at(
    directory: &std::path::Path,
    profile: &str,
    controls: &[(String, String)],
) -> Result<(), String> {
    let path = directory.join(format!("{}.json", std::process::id()));
    if controls.is_empty() {
        match std::fs::remove_file(path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("history_host_publish_failed".into()),
        }
        return Ok(());
    }
    let started_at = crate::mcp_ipc::process_started_at_unix(std::process::id())
        .ok_or_else(|| "history_host_identity_unavailable".to_string())?;
    let raw =
        serde_json::to_string(controls).map_err(|_| "history_host_publish_failed".to_string())?;
    let protected_controls = crate::store::credentials::protect_secret_for_current_user(&raw)
        .map_err(|_| "history_host_publish_failed".to_string())?;
    let host = Host {
        version: 1,
        pid: std::process::id(),
        started_at,
        profile: profile.to_string(),
        protected_controls,
    };
    std::fs::create_dir_all(&directory).map_err(|_| "history_host_publish_failed".to_string())?;
    let bytes = serde_json::to_vec(&host).map_err(|_| "history_host_publish_failed".to_string())?;
    crate::store::atomic_file::write_without_backup(&path, &bytes)
        .map_err(|_| "history_host_publish_failed".to_string())
}

pub(super) fn discover() -> Vec<(String, BuiltinAiProxyControl)> {
    discover_at(
        &directory(),
        &crate::store::paths::profile_name(),
        std::process::id(),
    )
}

fn discover_at(
    directory: &std::path::Path,
    profile: &str,
    reader_pid: u32,
) -> Vec<(String, BuiltinAiProxyControl)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return vec![];
    };
    let mut controls = Vec::new();
    let mut paths = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|v| v.to_str()) == Some("json"))
        .take(256)
        .map(|e| e.path())
        .collect::<Vec<_>>();
    paths.sort_by_key(|p| {
        std::cmp::Reverse(
            std::fs::symlink_metadata(p)
                .ok()
                .and_then(|m| m.modified().ok()),
        )
    });
    for path in paths.into_iter().take(32) {
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > 128 * 1024 {
            continue;
        }
        let Some(host) = std::fs::read(&path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<Host>(&raw).ok())
        else {
            continue;
        };
        if host.version != 1
            || host.pid == reader_pid
            || host.profile != profile
            || !crate::mcp_ipc::process_is_alive(host.pid)
            || crate::mcp_ipc::process_started_at_unix(host.pid) != Some(host.started_at)
        {
            continue;
        }
        let Some(items) =
            crate::store::credentials::unprotect_secret_for_current_user(&host.protected_controls)
                .ok()
                .and_then(|raw| serde_json::from_str::<Vec<(String, String)>>(&raw).ok())
        else {
            continue;
        };
        for (workspace, url) in items.into_iter().take(32) {
            if let Ok(control) = BuiltinAiProxyControl::from_history_url(&url) {
                controls.push((workspace, control));
            }
        }
    }
    controls.sort_by(|a, b| a.0.cmp(&b.0));
    controls
}

pub(super) fn controls() -> Result<Vec<(String, BuiltinAiProxyControl)>, String> {
    let mut controls = crate::app::ui::builtin_ai_history_controls()?;
    controls.extend(discover());
    Ok(controls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // The spawned process owns a protected record and stays alive until its stdin closes.
    #[test]
    fn history_host_child() {
        let Some(root) = std::env::var_os("HIMIND_HISTORY_TEST_DIRECTORY") else {
            return;
        };
        let url = std::env::var("HIMIND_HISTORY_TEST_URL").unwrap();
        publish_at(
            std::path::Path::new(&root),
            "synthetic",
            &[("G:/synthetic".into(), url)],
        )
        .unwrap();
        let _ = std::io::stdin().read(&mut [0u8; 1]);
    }

    #[test]
    fn history_host_cross_process_discovery_authentication_and_stale_identity() {
        let root = std::env::current_dir()
            .unwrap()
            .join(".himind")
            .join(format!("history-host-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/?himind_session=synthetic-token",
            listener.local_addr().unwrap()
        );
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "session_service::host::tests::history_host_child",
                "--nocapture",
            ])
            .env("HIMIND_HISTORY_TEST_DIRECTORY", &root)
            .env("HIMIND_HISTORY_TEST_URL", &url)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let path = root.join(format!("{}.json", child.id()));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.is_file() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut controls = discover_at(&root, "synthetic", std::process::id());
        // Ensure a failed assertion cannot leave a live helper process behind.
        if controls.is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("synthetic host was not discovered")
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("synthetic-token"));
        assert!(discover_at(&root, "different-profile", std::process::id()).is_empty());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = vec![0u8; 16384];
            let count = stream.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..count]);
            assert!(request.starts_with("POST /api/session/list "));
            assert!(request.contains("synthetic-token"));
            let body = r#"{"result":{"ok":true,"value":{"sessions":[]}}}"#;
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        });
        let response = controls
            .pop()
            .unwrap()
            .1
            .call_runtime_api("session/list", serde_json::json!({"_request":{}}))
            .unwrap();
        assert_eq!(response["result"]["ok"], true);
        server.join().unwrap();
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        assert!(discover_at(&root, "synthetic", std::process::id()).is_empty());
        let mut record: Host = serde_json::from_str(&raw).unwrap();
        record.pid = std::process::id();
        record.started_at = 0;
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(discover_at(&root, "synthetic", 0).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn history_host_rejects_foreign_urls_and_extra_credentials() {
        for url in [
            "https://127.0.0.1:1234/?himind_session=x",
            "http://example.com:1234/?himind_session=x",
            "http://127.0.0.1:1234/other?himind_session=x",
            "http://127.0.0.1:1234/?himind_session=x&other=y",
            "http://user:pass@127.0.0.1:1234/?himind_session=x",
            "http://127.0.0.1:1234/?himind_session=",
        ] {
            assert!(BuiltinAiProxyControl::from_history_url(url).is_err());
        }
    }
}
