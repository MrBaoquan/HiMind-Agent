use crate::mcp_ipc::{
    read_discovery, read_frame, write_frame, ServiceEvent, ServiceFrame, ServiceScope,
    IPC_PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use std::error::Error;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::thread;
use std::time::Duration;

const MAX_BRIDGE_FRAME_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn run(
    profile: &str,
    state_path: &Path,
    agent_home: &Path,
) -> Result<(), Box<dyn Error>> {
    let scope = ServiceScope::for_current_user(profile, state_path)?;
    let runtime_dir = scope.runtime_dir(agent_home);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    run_bridge_io(&scope, &runtime_dir, &mut reader, &mut writer)
}

pub(crate) fn run_bridge_io<R: BufRead, W: Write>(
    scope: &ServiceScope,
    runtime_dir: &Path,
    reader: &mut R,
    writer: &mut W,
) -> Result<(), Box<dyn Error>> {
    let discovery = wait_for_discovery(runtime_dir)?;
    if discovery.protocol_version != IPC_PROTOCOL_VERSION {
        return Err(format!(
            "MCP bridge/service protocol mismatch: bridge={} service={}",
            IPC_PROTOCOL_VERSION, discovery.protocol_version
        )
        .into());
    }
    if discovery.profile != scope.profile() || discovery.state_hash != scope.state_hash() {
        return Err("MCP bridge discovered a service from a different scope".into());
    }
    let mut pipe = crate::mcp_pipe::connect(&discovery.endpoint)?;
    let mut pipe_reader = BufReader::new(pipe.try_clone()?);
    write_frame(
        &mut pipe,
        &ServiceFrame::Hello {
            protocol_version: IPC_PROTOCOL_VERSION,
            token: discovery.token,
            profile: discovery.profile,
            state_hash: discovery.state_hash,
            client: json!({"name":"himind-agent-mcp-bridge","version":env!("CARGO_PKG_VERSION")}),
        },
    )?;
    let session_id = match read_frame::<ServiceEvent, _>(&mut pipe_reader)? {
        Some(ServiceEvent::HelloOk {
            protocol_version,
            session_id,
            ..
        }) if protocol_version == IPC_PROTOCOL_VERSION => session_id,
        Some(ServiceEvent::Error { code, message, .. }) => {
            return Err(format!("MCP service rejected bridge handshake ({code}): {message}").into())
        }
        Some(event) => {
            return Err(format!("unexpected MCP service handshake event: {event:?}").into())
        }
        None => return Err("MCP service closed during bridge handshake".into()),
    };

    let mut ipc_request_id = 0_u64;
    while let Some(line) = read_bounded_line(reader, MAX_BRIDGE_FRAME_BYTES)? {
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                write_json_line(
                    writer,
                    &json!({
                        "jsonrpc":"2.0",
                        "id":null,
                        "error":{"code":-32700,"message":error.to_string()}
                    }),
                )?;
                continue;
            }
        };
        let jsonrpc_id = request.get("id").cloned();
        ipc_request_id = ipc_request_id.wrapping_add(1);
        write_frame(
            &mut pipe,
            &ServiceFrame::Mcp {
                session_id: session_id.clone(),
                request_id: ipc_request_id,
                payload: request,
            },
        )?;
        if jsonrpc_id.is_none() {
            continue;
        }
        loop {
            let Some(event) = read_frame::<ServiceEvent, _>(&mut pipe_reader)? else {
                return Err("MCP service closed before returning a response".into());
            };
            match event {
                ServiceEvent::Mcp {
                    request_id,
                    payload,
                    ..
                } if request_id == ipc_request_id => {
                    write_json_line(writer, &payload)?;
                    break;
                }
                ServiceEvent::Notification { payload, .. }
                | ServiceEvent::Progress { payload, .. } => {
                    write_json_line(writer, &payload)?;
                }
                ServiceEvent::Error {
                    request_id,
                    code,
                    message,
                    ..
                } if request_id == Some(ipc_request_id) => {
                    write_json_line(
                        writer,
                        &json!({
                            "jsonrpc":"2.0",
                            "id":jsonrpc_id,
                            "error":{"code":-32000,"message":format!("{code}: {message}")}
                        }),
                    )?;
                    break;
                }
                event => return Err(format!("unexpected MCP service event: {event:?}").into()),
            }
        }
    }
    let _ = write_frame(&mut pipe, &ServiceFrame::Close { session_id });
    Ok(())
}

fn wait_for_discovery(
    runtime_dir: &Path,
) -> Result<crate::mcp_ipc::ServiceDiscovery, Box<dyn Error>> {
    for _ in 0..80 {
        if let Some(discovery) = read_discovery(runtime_dir) {
            return Ok(discovery);
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(format!(
        "HiMind MCP service is not available at {}",
        runtime_dir.display()
    )
    .into())
}

fn write_json_line(writer: &mut impl Write, value: &Value) -> Result<(), Box<dyn Error>> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn read_bounded_line<R: BufRead>(reader: &mut R, limit: usize) -> io::Result<Option<String>> {
    let mut bytes = Vec::with_capacity(8192.min(limit));
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            };
        }
        let take = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(buffer.len());
        if bytes.len().saturating_add(take) > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("MCP request exceeds {limit} bytes"),
            ));
        }
        bytes.extend_from_slice(&buffer[..take]);
        reader.consume(take);
        if bytes.last() == Some(&b'\n') {
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
    }
}
