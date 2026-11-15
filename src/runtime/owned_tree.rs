//! Establishes tree ownership before any child code can execute.
use std::{error::Error, process::{Child, Command}};

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{ffi::c_void, os::windows::{io::AsRawHandle, process::CommandExt}};
    type Handle = *mut c_void;
    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        process_time: i64, job_time: i64, flags: u32,
        min_working_set: usize, max_working_set: usize,
        active_processes: u32, affinity: usize, priority: u32, scheduling: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits, io: [u64; 6],
        process_memory: usize, job_memory: usize, peak_process: usize, peak_job: usize,
    }
    #[repr(C)]
    #[derive(Default)]
    struct BasicAccounting {
        total_user_time: i64, total_kernel_time: i64,
        period_user_time: i64, period_kernel_time: i64,
        page_faults: u32, total_processes: u32, active_processes: u32, terminated_processes: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, value: *const c_void, size: u32) -> i32;
        fn QueryInformationJobObject(job: Handle, class: i32, value: *mut c_void, size: u32, returned: *mut u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn TerminateJobObject(job: Handle, code: u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
        fn Thread32First(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
        fn Thread32Next(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
        fn OpenThread(access: u32, inherit: i32, id: u32) -> Handle;
        fn ResumeThread(thread: Handle) -> u32;
    }
    #[repr(C)]
    struct ThreadEntry { size: u32, usage: u32, id: u32, owner: u32, base: i32, delta: i32, flags: u32 }
    struct HandleGuard(Handle);
    impl Drop for HandleGuard { fn drop(&mut self) { unsafe { CloseHandle(self.0); } } }
    // Faults are local to the calling test thread, never environment-controlled
    // in a release build. They exercise the same checked production branches.
    #[cfg(test)]
    thread_local! { pub(super) static FAULT: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") }; }
    #[cfg(test)]
    thread_local! { pub(super) static LAST_CHILD: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
    #[cfg(test)]
    thread_local! { pub(super) static BEFORE_ASSIGN: std::cell::RefCell<Option<Box<dyn FnMut(&Child)>>> = const { std::cell::RefCell::new(None) }; }
    #[cfg(test)]
    thread_local! { pub(super) static BEFORE_CLOSE: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) }; }
    #[cfg(test)]
    thread_local! { pub(crate) static BEFORE_TERMINATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) }; }
    fn fault(stage: &str) -> bool {
        #[cfg(test)] { return FAULT.with(|value| value.get() == stage); }
        #[cfg(not(test))] { let _ = stage; false }
    }
    pub(crate) struct OwnedTree(Handle);
    impl OwnedTree {
        /// Stop only the tree already owned before execution. Spawning taskkill
        /// here adds an unbounded external command to the shutdown deadline.
        pub(crate) fn terminate(&self, child: &mut Child) -> Result<(), Box<dyn Error>> {
            #[cfg(test)]
            if let Some(hook) = BEFORE_TERMINATE.with(|slot| slot.borrow_mut().take()) { hook(); }
            if unsafe { TerminateJobObject(self.0, 1) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            child.wait()?;
            Ok(())
        }
        /// Called after reaping the root. Remaining active members are owned
        /// descendants, not evidence of a clean provider shutdown.
        pub(crate) fn has_active_processes(&self) -> Result<bool, Box<dyn Error>> {
            let mut accounting = BasicAccounting::default();
            let queried = unsafe { QueryInformationJobObject(self.0, 1,
                &mut accounting as *mut _ as *mut c_void,
                std::mem::size_of::<BasicAccounting>() as u32, std::ptr::null_mut()) };
            if queried == 0 { return Err(std::io::Error::last_os_error().into()); }
            Ok(accounting.active_processes != 0)
        }

        pub(crate) fn spawn(command: &mut Command) -> Result<(Child, Self), Box<dyn Error>> {
            if fault("create") { return Err("injected Job create failure".into()); }
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                let error = std::io::Error::last_os_error();
                return Err(error.into());
            }
            let tree = Self(handle);
            let mut limits = ExtendedLimits::default();
            limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            let configured = unsafe { SetInformationJobObject(handle, 9,
                &limits as *const _ as *const c_void, std::mem::size_of::<ExtendedLimits>() as u32) };
            if configured == 0 || fault("configure") {
                let error = std::io::Error::last_os_error();
                return Err(format!("cannot configure runtime Job: {error}").into());
            }
            // Rust ORs these flags with its own creation flags. All current
            // entry points request hidden processes; preserve that policy.
            command.creation_flags(0x08000000 | 0x00000004); // NO_WINDOW | SUSPENDED
            let mut child = command.spawn()?;
            #[cfg(test)]
            LAST_CHILD.with(|slot| slot.set(child.id()));
            #[cfg(test)]
            BEFORE_ASSIGN.with(|hook| { if let Some(hook) = hook.borrow_mut().as_mut() { hook(&child); } });
            let setup = (|| -> Result<(), Box<dyn Error>> {
                if fault("assign") || unsafe { AssignProcessToJobObject(handle, child.as_raw_handle()) } == 0 {
                    return Err("cannot assign suspended runtime to Job".into());
                }
                // A suspended new process has exactly one primary thread. Use
                // documented Win32 APIs, retaining/closing every owned handle.
                let snapshot = unsafe { CreateToolhelp32Snapshot(4, 0) };
                if snapshot as isize == -1 { return Err(std::io::Error::last_os_error().into()); }
                let snapshot = HandleGuard(snapshot);
                let mut entry: ThreadEntry = unsafe { std::mem::zeroed() };
                entry.size = std::mem::size_of::<ThreadEntry>() as u32;
                let mut found = Vec::new();
                let mut more = unsafe { Thread32First(snapshot.0, &mut entry) };
                while more != 0 {
                    if entry.owner == child.id() { found.push(entry.id); }
                    more = unsafe { Thread32Next(snapshot.0, &mut entry) };
                }
                if found.len() != 1 { return Err("suspended runtime primary thread is ambiguous or missing".into()); }
                let thread = unsafe { OpenThread(2, 0, found[0]) }; // THREAD_SUSPEND_RESUME
                if thread.is_null() { return Err(std::io::Error::last_os_error().into()); }
                let thread = HandleGuard(thread);
                #[cfg(test)]
                LAST_CHILD.with(|slot| slot.set(child.id()));
                if fault("resume") || unsafe { ResumeThread(thread.0) } != 1 {
                    return Err("cannot resume owned runtime primary thread".into());
                }
                Ok(())
            })();
            if let Err(error) = setup {
                // The suspended child cannot have executed. Never depend on
                // taskkill/PID lookup: kill and reap this exact child handle.
                unsafe { TerminateJobObject(handle, 1); }
                let _ = child.kill();
                child.wait()?;
                return Err(error);
            }
            Ok((child, tree))
        }
    }
    impl Drop for OwnedTree {
        fn drop(&mut self) {
            #[cfg(test)]
            BEFORE_CLOSE.with(|hook| { if let Some(hook)=hook.borrow_mut().as_mut() { hook(); } });
            unsafe { TerminateJobObject(self.0, 1); CloseHandle(self.0); }
        }
    }
}

#[cfg(all(test,windows))]
mod tests {
    use super::*;
    use crate::runtime::process::codex_test_support::{Environment,ProcessProbe};
    use std::{cell::Cell,rc::Rc,process::Stdio,time::Duration};
    #[test]
    fn ownership_setup_failures_reap_suspended_child_before_code_runs() {
        use windows_sys::Win32::{System::Threading::{OpenProcess,WaitForSingleObject},Foundation::CloseHandle};
        let _lock=crate::store::paths::test_env_lock();
        for stage in ["create","configure","assign","resume"] {
            let env=Environment::new();env.case(&serde_json::json!({}));
            let observer=Rc::new(Cell::new(std::ptr::null_mut()));let retained=observer.clone();
            windows::BEFORE_ASSIGN.with(|hook|*hook.borrow_mut()=Some(Box::new(move |child| {
                let handle=unsafe {OpenProcess(0x0010_0000,0,child.id())};assert!(!handle.is_null());
                assert_eq!(unsafe {WaitForSingleObject(handle,0)},258);retained.set(handle);
            })));
            windows::LAST_CHILD.with(|v|v.set(0));windows::FAULT.with(|v|v.set(stage));
            let mut command=Command::new(std::env::var_os("HIMIND_CODEX_EXECUTABLE").unwrap());
            command.current_dir(&env.root).args(["exec","-o","unused","-"])
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            let result=OwnedTree::spawn(&mut command);
            windows::FAULT.with(|v|v.set(""));windows::BEFORE_ASSIGN.with(|h|*h.borrow_mut()=None);
            assert!(result.is_err(),"{stage}");
            let handle=observer.get();
            if matches!(stage,"assign"|"resume") {
                assert!(!handle.is_null());assert_eq!(unsafe {WaitForSingleObject(handle,5000)},0,"{stage} leaked child");
                unsafe {CloseHandle(handle);}
            } else {assert!(handle.is_null());assert_eq!(windows::LAST_CHILD.with(Cell::get),0);}
            assert!(!env.root.join("argv.json").exists(),"{stage}: child code ran");
            println!("ownership_fault={stage}; child_created={}; independently_observed_exit={}; child_code_executed=false",
                !handle.is_null(),if handle.is_null(){"not_applicable"}else{"true"});
        }
    }

    #[test]
    fn ownership_immediate_descendant_root_exit_and_close_are_owned() {
        let _lock=crate::store::paths::test_env_lock();
        for immediate_exit in [true,false] {
            let env=Environment::new();env.case(&serde_json::json!({"immediate_tree_exit":immediate_exit,"immediate_tree_wait":!immediate_exit}));
            let mut command=Command::new(std::env::var_os("HIMIND_CODEX_EXECUTABLE").unwrap());
            command.current_dir(&env.root).args(["exec","-o","unused","-"])
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            let (mut child,tree)=OwnedTree::spawn(&mut command).unwrap();
            let descendant=ProcessProbe::open(&env.root.join("child.pid"));
            assert!(descendant.is_alive());
            if immediate_exit { assert!(child.wait().unwrap().success());assert!(descendant.is_alive()); }
            drop(tree);let _=child.wait().unwrap();descendant.assert_exited();
            println!("immediate_descendant; root_exit={immediate_exit}; descendant_observed_alive_then_exited=true");
        }
    }

    #[test]
    fn ownership_transport_immediate_root_exit_cancel_timeout_and_shutdown() {
        let _lock=crate::store::paths::test_env_lock();
        for mode in ["root-exit","cancel","timeout","shutdown"] {
            let env=Environment::new();
            env.case(&match mode {
                "root-exit"=>serde_json::json!({"immediate_tree_exit":true}),
                "shutdown"=>serde_json::json!({"tree":true,"hang":true}),
                _=>serde_json::json!({"immediate_tree_wait":true})});
            let root=env.root.clone();let observed=std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));let signal=observed.clone();
            let probe=std::thread::spawn(move || {let child=ProcessProbe::open(&root.join("child.pid"));
                assert!(child.is_alive());signal.store(true,std::sync::atomic::Ordering::SeqCst);child.assert_exited();});
            let seen=observed.clone();
            windows::BEFORE_CLOSE.with(|hook|*hook.borrow_mut()=Some(Box::new(move || {
                let start=std::time::Instant::now();
                while !seen.load(std::sync::atomic::Ordering::SeqCst) {
                    assert!(start.elapsed()<Duration::from_secs(3));std::thread::sleep(Duration::from_millis(5));
                }
            })));
            let mut command=Command::new(std::env::var_os("HIMIND_CODEX_EXECUTABLE").unwrap());
            command.current_dir(&env.root).arg("exec").arg("-o").arg(env.root.join("final.txt")).arg("-")
                .stdout(Stdio::piped()).stderr(Stdio::piped());
            let result=crate::runtime::codex_transport::execute(&mut command,"test",&env.root.join("final.txt"),1,
                &||Ok(mode=="cancel" && observed.load(std::sync::atomic::Ordering::SeqCst)),Duration::from_millis(250),mode,"");
            windows::BEFORE_CLOSE.with(|hook|*hook.borrow_mut()=None);
            probe.join().unwrap();let error=result.err().expect("fixture is never successful");
            let message=error.to_string();
            match mode {"cancel"=>assert!(message.contains("canceled")),"timeout"=>assert!(message.contains("exceeded")),
                "shutdown"=>assert!(message.contains("shutdown deadline")),_=>{}}
            println!("product_ownership={mode}; observed_descendant_exited=true");
        }
    }
}
#[cfg(windows)]
pub(crate) use windows::OwnedTree;
#[cfg(all(test,windows))]
pub(crate) use windows::BEFORE_TERMINATE;

#[cfg(not(windows))]
pub(crate) struct OwnedTree;
#[cfg(not(windows))]
impl OwnedTree {
    pub(crate) fn terminate(&self, child: &mut Child) -> Result<(), Box<dyn Error>> {
        crate::runtime::process::terminate_process_tree(child);
        Ok(())
    }
    pub(crate) fn has_active_processes(&self) -> Result<bool, Box<dyn Error>> { Ok(false) }
    pub(crate) fn spawn(command: &mut Command) -> Result<(Child, Self), Box<dyn Error>> { Ok((command.spawn()?, Self)) }
}
