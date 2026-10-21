use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

pub(crate) struct PipeStream {
    file: File,
}

impl PipeStream {
    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
        })
    }
}

impl Read for PipeStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.read(buffer)
    }
}

impl Write for PipeStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

pub(crate) fn accept(endpoint: &str) -> io::Result<PipeStream> {
    let endpoint = wide(endpoint);
    let handle = unsafe {
        CreateNamedPipeW(
            endpoint.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    if unsafe { ConnectNamedPipe(handle, null_mut()) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_PIPE_CONNECTED as i32) {
            unsafe {
                CloseHandle(handle);
            }
            return Err(error);
        }
    }
    Ok(unsafe { PipeStream::from_handle(handle) })
}

pub(crate) fn connect(endpoint: &str) -> io::Result<PipeStream> {
    let mut last_error = None;
    for _ in 0..40 {
        match connect_once(endpoint) {
            Ok(stream) => return Ok(stream),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(231) =>
            {
                last_error = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::from(io::ErrorKind::NotFound)))
}

fn connect_once(endpoint: &str) -> io::Result<PipeStream> {
    let endpoint = wide(endpoint);
    let handle = unsafe {
        CreateFileW(
            endpoint.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { PipeStream::from_handle(handle) })
}

impl PipeStream {
    unsafe fn from_handle(handle: windows_sys::Win32::Foundation::HANDLE) -> Self {
        Self {
            file: File::from_raw_handle(handle as RawHandle),
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_ipc::{read_frame, write_frame, ServiceEvent, ServiceFrame};
    use std::io::BufReader;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_endpoint(label: &str) -> String {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!(
            r"\\.\pipe\himind-agent-test-{label}-{}-{nonce}",
            std::process::id()
        )
    }

    #[test]
    fn named_pipe_round_trips_service_frames() {
        let endpoint = test_endpoint("roundtrip");
        let server_endpoint = endpoint.clone();
        let server = thread::spawn(move || {
            let mut stream = accept(&server_endpoint).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let frame: ServiceFrame = read_frame(&mut reader).unwrap().unwrap();
            assert_eq!(frame, ServiceFrame::Ping { request_id: 41 });
            write_frame(&mut stream, &ServiceEvent::Pong { request_id: 41 }).unwrap();
        });
        let mut client = connect(&endpoint).unwrap();
        write_frame(&mut client, &ServiceFrame::Ping { request_id: 41 }).unwrap();
        let mut reader = BufReader::new(client.try_clone().unwrap());
        let event: ServiceEvent = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(event, ServiceEvent::Pong { request_id: 41 });
        server.join().unwrap();
    }
}
