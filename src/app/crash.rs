//! Crash visibility for the Agent process.
//!
//! The Windows Agent runs as a long-lived desktop process with no supervising
//! console, so an unexpected exit previously left nothing behind except a WER
//! "APPCRASH" entry with an offset.  Two real incidents were only diagnosable
//! because that offset happened to land inside `__chkstk`.  This module makes
//! the next crash self-describing: Rust panics are logged as events, and any
//! unhandled Windows exception writes a minidump next to the Agent logs.

use serde_json::json;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;

const CRASH_DIR: &str = "crashes";

/// Directory that receives crash dumps and panic reports.
pub(crate) fn crash_dir() -> PathBuf {
    crate::store::paths::agent_home()
        .join("logs")
        .join(CRASH_DIR)
}

/// Append a crash record to the Agent event log so a crash is distinguishable
/// from a normal quit in `agent-events.jsonl`.
pub(crate) fn record_event(level: &str, message: &str) {
    let path = crate::store::paths::agent_home()
        .join("logs")
        .join("agent-events.jsonl");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs();
    let line = json!({
        "time": format_clock(seconds),
        "timestamp": seconds,
        "level": level,
        "message": message,
    });
    if let Ok(mut text) = serde_json::to_string(&line) {
        text.push('\n');
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, text.as_bytes()));
    }
}

fn format_clock(unix_seconds: u64) -> String {
    let seconds_of_day = unix_seconds % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds_of_day / 3_600,
        (seconds_of_day % 3_600) / 60,
        seconds_of_day % 60
    )
}

/// Log Rust panics before the process aborts.
pub(crate) fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|value| format!("{}:{}", value.file(), value.line()))
            .unwrap_or_else(|| "unknown".to_string());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|value| (*value).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string());
        record_event(
            "error",
            &format!("Agent 发生 panic（{location}）：{payload}"),
        );
        previous(info);
    }));
}

/// Debug-only self test for the crash pipeline.
///
/// Set `HIMIND_AGENT_CRASH_SELFTEST=stack_overflow` to make a debug build blow
/// a thread stack on purpose.  It proves that panic logging and minidump
/// capture work before we rely on them to diagnose a real incident.
pub(crate) fn run_selftest_if_requested() {
    #[cfg(debug_assertions)]
    {
        if std::env::var("HIMIND_AGENT_CRASH_SELFTEST").as_deref() != Ok("stack_overflow") {
            return;
        }
        record_event("warn", "崩溃自检已启用：即将制造一次栈溢出");
        fn recurse(depth: u64) -> u64 {
            let filler = [depth; 128];
            let value = std::hint::black_box(filler);
            if depth % 64 == 0 {
                std::hint::black_box(value);
            }
            recurse(depth + 1)
        }
        let _ = recurse(1);
    }
}

/// Register the Windows unhandled-exception filter that writes a minidump.
#[cfg(windows)]
pub(crate) fn install_exception_dump_filter() {
    use windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter;
    unsafe {
        SetUnhandledExceptionFilter(Some(unhandled_exception_filter));
    }
}

#[cfg(not(windows))]
pub(crate) fn install_exception_dump_filter() {}

#[cfg(windows)]
unsafe extern "system" fn unhandled_exception_filter(
    info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> i32 {
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
    let dump = write_minidump(info);
    let code = if info.is_null() || (*info).ExceptionRecord.is_null() {
        0
    } else {
        (*(*info).ExceptionRecord).ExceptionCode
    };
    match dump {
        Ok(path) => record_event(
            "error",
            &format!(
                "Agent 未处理异常 0x{code:08X}，已写入崩溃转储：{}",
                path.display()
            ),
        ),
        Err(error) => record_event(
            "error",
            &format!("Agent 未处理异常 0x{code:08X}，崩溃转储写入失败：{error}"),
        ),
    }
    EXCEPTION_CONTINUE_SEARCH
}

#[cfg(windows)]
unsafe fn write_minidump(
    info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> Result<PathBuf, String> {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        MiniDumpNormal, MiniDumpWithHandleData, MiniDumpWithThreadInfo, MiniDumpWriteDump,
        MINIDUMP_EXCEPTION_INFORMATION,
    };

    let directory = crash_dir();
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let path = directory.join(format!(
        "himind-agent-{}-{}.dmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or_default()
    ));
    let process = windows_sys::Win32::System::Threading::GetCurrentProcess();
    let file = std::fs::File::create(&path).map_err(|error| error.to_string())?;
    let mut exception = MINIDUMP_EXCEPTION_INFORMATION {
        ThreadId: windows_sys::Win32::System::Threading::GetCurrentThreadId(),
        ExceptionPointers: info as *mut _,
        ClientPointers: 0,
    };
    let dump_type = MiniDumpNormal | MiniDumpWithThreadInfo | MiniDumpWithHandleData;
    let written = MiniDumpWriteDump(
        process,
        std::process::id(),
        file.as_raw_handle() as _,
        dump_type,
        &mut exception as *mut _ as *const _,
        std::ptr::null(),
        std::ptr::null(),
    );
    if written == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(path)
}
