//! 单实例守卫：作用域 = `tauri.conf.json` 的 identifier + 运行 profile。
//!
//! 上游 `tauri-plugin-single-instance` 只用 identifier 当互斥体与隐藏窗口的
//! 名字，所以同一份二进制换 profile 启动时，两边会互相把对方当成「已经跑着
//! 的自己」：开发 profile 启动会把已安装的生产 Agent 顶掉，反过来生产启动
//! 又会把开发实例的深链全部转发到生产进程上。开发拓扑要求两者并存（本地
//! 工作台和已安装产品同时在用），所以这里把 profile 拼进键名。
//!
//! 键名规则：
//! - 生产 profile 继续走上游插件，键与历史逐字一致，安装版升级不掉单实例；
//! - 其它 profile 用 `<identifier>-<profile>-si{m,c,w}`，一个 profile 一个实例；
//! - `HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE=1` 完全跳过单实例，只给并行验证用。
//!
//! Windows 实现与上游同构（`CreateMutexW` 抢锁 + 隐藏窗口收 `WM_COPYDATA`）
//! 并带上上游 2.4.5 的修复：转发前把前台权限交给首个实例，否则 Windows 会
//! 拒绝它把主窗口带到前台。非 Windows 平台退回上游插件。

use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Runtime};

/// 收到后来实例的命令行与工作目录时执行的回调。
pub(crate) type ForwardCallback<R> =
    dyn FnMut(&AppHandle<R>, Vec<String>, String) + Send + Sync + 'static;

/// 当前进程需要的单实例作用域。
///
/// `None` 表示沿用上游插件的默认键：生产 profile 与 `default` 都保持历史
/// 行为，非 Windows 平台也由 [`init`] 直接交给上游实现。
fn profile_instance_scope() -> Option<String> {
    let profile = crate::store::paths::profile_name();
    if profile == "production" || profile == "default" {
        return None;
    }
    Some(profile)
}

/// 注册单实例守卫，用法与 `tauri_plugin_single_instance::init` 一致。
///
/// 调用点必须与 `HIMIND_AGENT_ALLOW_PARALLEL_INSTANCE` 的判断保持一致：
/// 显式放行并行实例时这里不会被调用。
pub(crate) fn init<R: Runtime, F>(callback: F) -> TauriPlugin<R>
where
    F: FnMut(&AppHandle<R>, Vec<String>, String) + Send + Sync + 'static,
{
    match profile_instance_scope() {
        Some(scope) => platform::init_scoped(scope, Box::new(callback)),
        None => tauri_plugin_single_instance::init(callback),
    }
}

#[cfg(windows)]
mod platform {
    use super::ForwardCallback;
    use std::ffi::CStr;
    use tauri::plugin::{self, TauriPlugin};
    use tauri::{AppHandle, Manager, RunEvent, Runtime};
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
        },
        System::{
            DataExchange::COPYDATASTRUCT,
            LibraryLoader::GetModuleHandleW,
            Threading::{CreateMutexW, ReleaseMutex},
        },
        UI::WindowsAndMessaging::{
            self as w32wm, AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW,
            DestroyWindow, FindWindowW, GetWindowThreadProcessId, RegisterClassExW, SendMessageW,
            CREATESTRUCTW, GWLP_USERDATA, GWL_STYLE, WINDOW_LONG_PTR_INDEX, WM_COPYDATA, WM_CREATE,
            WM_DESTROY, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
            WS_EX_TRANSPARENT, WS_OVERLAPPED, WS_POPUP, WS_VISIBLE,
        },
    };

    /// 与上游一致的 `WM_COPYDATA` 标识，用来区分单实例转发和其它消息。
    const WMCOPYDATA_SINGLE_INSTANCE_DATA: usize = 1542;

    struct MutexHandle(isize);

    struct TargetWindowHandle(isize);

    struct UserData<R: Runtime> {
        app: AppHandle<R>,
        callback: Box<ForwardCallback<R>>,
    }

    impl<R: Runtime> UserData<R> {
        unsafe fn from_hwnd_raw(hwnd: HWND) -> *mut Self {
            GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Self
        }

        unsafe fn from_hwnd<'a>(hwnd: HWND) -> &'a mut Self {
            &mut *Self::from_hwnd_raw(hwnd)
        }

        fn run_callback(&mut self, args: Vec<String>, cwd: String) {
            (self.callback)(&self.app, args, cwd)
        }
    }

    pub(crate) fn init_scoped<R: Runtime>(
        scope: String,
        callback: Box<ForwardCallback<R>>,
    ) -> TauriPlugin<R> {
        plugin::Builder::new("single-instance")
            .setup(move |app, _api| {
                let class_name = encode_wide(format!("{}-{scope}-sic", app.config().identifier));
                let window_name = encode_wide(format!("{}-{scope}-siw", app.config().identifier));
                let mutex_name = encode_wide(format!("{}-{scope}-sim", app.config().identifier));

                let hmutex =
                    unsafe { CreateMutexW(std::ptr::null(), true.into(), mutex_name.as_ptr()) };

                if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                    unsafe {
                        let hwnd = FindWindowW(class_name.as_ptr(), window_name.as_ptr());

                        if !hwnd.is_null() {
                            let cwd = std::env::current_dir().unwrap_or_default();
                            let cwd = cwd.to_str().unwrap_or_default();

                            let args = std::env::args().collect::<Vec<String>>().join("|");

                            let data = format!("{cwd}|{args}\0");

                            let bytes = data.as_bytes();
                            let cds = COPYDATASTRUCT {
                                dwData: WMCOPYDATA_SINGLE_INSTANCE_DATA,
                                cbData: bytes.len() as _,
                                lpData: bytes.as_ptr() as _,
                            };

                            // Windows 只允许当前活动窗口把别的窗口带到前台。这里
                            // 把这项权限移交给首个实例，回调里的聚焦才不会被拒。
                            let mut pid = 0;
                            GetWindowThreadProcessId(hwnd, &mut pid);
                            if pid != 0 {
                                AllowSetForegroundWindow(pid);
                            }

                            SendMessageW(hwnd, WM_COPYDATA, 0, &cds as *const _ as _);

                            app.cleanup_before_exit();
                            std::process::exit(0);
                        }
                    }
                } else {
                    app.manage(MutexHandle(hmutex as _));

                    let userdata = UserData {
                        app: app.clone(),
                        callback,
                    };
                    let userdata = Box::into_raw(Box::new(userdata));
                    let hwnd = create_event_target_window::<R>(&class_name, &window_name, userdata);
                    app.manage(TargetWindowHandle(hwnd as _));
                }

                Ok(())
            })
            .on_event(|app, event| {
                if let RunEvent::Exit = event {
                    destroy(app);
                }
            })
            .build()
    }

    pub(crate) fn destroy<R: Runtime, M: Manager<R>>(manager: &M) {
        if let Some(hmutex) = manager.try_state::<MutexHandle>() {
            unsafe {
                ReleaseMutex(hmutex.0 as _);
                CloseHandle(hmutex.0 as _);
            }
        }
        if let Some(hwnd) = manager.try_state::<TargetWindowHandle>() {
            unsafe { DestroyWindow(hwnd.0 as _) };
        }
    }

    unsafe extern "system" fn single_instance_window_proc<R: Runtime>(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CREATE => {
                let create_struct = &*(lparam as *const CREATESTRUCTW);
                let userdata = create_struct.lpCreateParams as *const UserData<R>;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, userdata as _);
                0
            }

            WM_COPYDATA => {
                let cds_ptr = lparam as *const COPYDATASTRUCT;
                if (*cds_ptr).dwData == WMCOPYDATA_SINGLE_INSTANCE_DATA {
                    let userdata = UserData::<R>::from_hwnd(hwnd);

                    let data = CStr::from_ptr((*cds_ptr).lpData as _).to_string_lossy();
                    let mut s = data.split('|');
                    let cwd = s.next().unwrap();
                    let args = s.map(|s| s.to_string()).collect();

                    userdata.run_callback(args, cwd.to_string());
                }
                1
            }

            WM_DESTROY => {
                let userdata = UserData::<R>::from_hwnd_raw(hwnd);
                drop(Box::from_raw(userdata));
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    fn create_event_target_window<R: Runtime>(
        class_name: &[u16],
        window_name: &[u16],
        userdata: *const UserData<R>,
    ) -> HWND {
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(single_instance_window_proc::<R>),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: GetModuleHandleW(std::ptr::null()),
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };

            RegisterClassExW(&class);

            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED
                    // WS_EX_TOOLWINDOW 让它不出现在任务栏里：去掉这个样式，
                    // 这个窗口有时会自己冒到任务栏上。
                    | WS_EX_TOOLWINDOW,
                class_name.as_ptr(),
                window_name.as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                userdata as _,
            );
            SetWindowLongPtrW(
                hwnd,
                GWL_STYLE,
                // 窗口必须「可见」才会收到 WM_PAINT（尺寸变化时用到），但
                // LAYERED 样式不会真的画出来。
                (WS_VISIBLE | WS_POPUP) as isize,
            );
            hwnd
        }
    }

    fn encode_wide(string: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
        std::os::windows::prelude::OsStrExt::encode_wide(string.as_ref())
            .chain(std::iter::once(0))
            .collect()
    }

    #[cfg(target_pointer_width = "32")]
    #[allow(non_snake_case)]
    unsafe fn SetWindowLongPtrW(hwnd: HWND, index: WINDOW_LONG_PTR_INDEX, value: isize) -> isize {
        w32wm::SetWindowLongW(hwnd, index, value as _) as _
    }

    #[cfg(target_pointer_width = "64")]
    #[allow(non_snake_case)]
    unsafe fn SetWindowLongPtrW(hwnd: HWND, index: WINDOW_LONG_PTR_INDEX, value: isize) -> isize {
        w32wm::SetWindowLongPtrW(hwnd, index, value)
    }

    #[cfg(target_pointer_width = "32")]
    #[allow(non_snake_case)]
    unsafe fn GetWindowLongPtrW(hwnd: HWND, index: WINDOW_LONG_PTR_INDEX) -> isize {
        w32wm::GetWindowLongW(hwnd, index) as _
    }

    #[cfg(target_pointer_width = "64")]
    #[allow(non_snake_case)]
    unsafe fn GetWindowLongPtrW(hwnd: HWND, index: WINDOW_LONG_PTR_INDEX) -> isize {
        w32wm::GetWindowLongPtrW(hwnd, index)
    }
}

#[cfg(not(windows))]
mod platform {
    use super::ForwardCallback;
    use tauri::plugin::TauriPlugin;
    use tauri::Runtime;

    /// 非 Windows 平台没有 profile 作用域实现，退回上游插件（键仍是 identifier）。
    pub(crate) fn init_scoped<R: Runtime>(
        _scope: String,
        callback: Box<ForwardCallback<R>>,
    ) -> TauriPlugin<R> {
        tauri_plugin_single_instance::Builder::new()
            .callback(callback)
            .build()
    }
}
