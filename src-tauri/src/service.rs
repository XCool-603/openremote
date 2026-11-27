use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemServiceStatus {
    pub installed: bool,
    pub running: bool,
    pub running_as_system: bool,
    pub binary_path: Option<String>,
}

#[cfg(target_os = "windows")]
mod win_service {
    use super::SystemServiceStatus;
    use crate::config::AppConfig;
    use anyhow::{anyhow, Result};
    use std::ffi::OsStr;
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::time::{Duration, Instant};

    const SERVICE_NAME: &str = "OpenRemoteService";
    const SERVICE_DISPLAY_NAME: &str = "OpenRemote System Service";

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;

    // SCM access rights
    const SC_MANAGER_CONNECT: u32 = 0x0001;
    const SC_MANAGER_CREATE_SERVICE: u32 = 0x0002;
    const SC_MANAGER_ALL_ACCESS: u32 = 0x000F_003F;

    const SERVICE_QUERY_CONFIG: u32 = 0x0001;
    const SERVICE_CHANGE_CONFIG: u32 = 0x0002;
    const SERVICE_QUERY_STATUS: u32 = 0x0004;
    const SERVICE_START: u32 = 0x0010;
    const SERVICE_STOP: u32 = 0x0020;
    const DELETE: u32 = 0x0001_0000;
    const SERVICE_ALL_ACCESS: u32 = 0x000F_01FF;

    const SERVICE_WIN32_OWN_PROCESS: u32 = 0x0000_0010;
    const SERVICE_AUTO_START: u32 = 0x0000_0002;
    const SERVICE_ERROR_NORMAL: u32 = 0x0000_0001;

    const SERVICE_STOPPED: u32 = 0x0000_0001;
    const SERVICE_START_PENDING: u32 = 0x0000_0002;
    const SERVICE_STOP_PENDING: u32 = 0x0000_0003;
    const SERVICE_RUNNING: u32 = 0x0000_0004;

    const SERVICE_ACCEPT_STOP: u32 = 0x0000_0001;
    const SERVICE_ACCEPT_SHUTDOWN: u32 = 0x0000_0004;
    const SERVICE_ACCEPT_SESSIONCHANGE: u32 = 0x0000_0080;

    const SERVICE_CONTROL_STOP: u32 = 0x0000_0001;
    const SERVICE_CONTROL_INTERROGATE: u32 = 0x0000_0004;
    const SERVICE_CONTROL_SHUTDOWN: u32 = 0x0000_0005;
    const SERVICE_CONTROL_SESSIONCHANGE: u32 = 0x0000_000E;

    const SC_STATUS_PROCESS_INFO: u32 = 0;

    const TOKEN_ALL_ACCESS: u32 = 0x000F_01FF;
    const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
    const SECURITY_IDENTIFICATION: u32 = 2;
    const TOKEN_PRIMARY: u32 = 1;
    const TOKEN_SESSION_ID: u32 = 12;

    const STILL_ACTIVE: u32 = 259;

    static SERVICE_STOP_FLAG: AtomicBool = AtomicBool::new(false);
    static SERVICE_SESSION_CHANGED: AtomicBool = AtomicBool::new(false);
    static SERVICE_STATUS_HANDLE: AtomicIsize = AtomicIsize::new(0);

    #[repr(C)]
    struct ServiceTableEntryW {
        lp_service_name: *mut u16,
        lp_service_proc: Option<extern "system" fn(u32, *mut *mut u16)>,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub(super) struct ServiceStatus {
        dw_service_type: u32,
        dw_current_state: u32,
        dw_controls_accepted: u32,
        dw_win32_exit_code: u32,
        dw_service_specific_exit_code: u32,
        dw_check_point: u32,
        dw_wait_hint: u32,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub(super) struct ServiceStatusProcess {
        dw_service_type: u32,
        dw_current_state: u32,
        dw_controls_accepted: u32,
        dw_win32_exit_code: u32,
        dw_service_specific_exit_code: u32,
        dw_check_point: u32,
        dw_wait_hint: u32,
        dw_process_id: u32,
        dw_service_flags: u32,
    }

    #[repr(C)]
    pub(super) struct WtsSessionInfoW {
        session_id: u32,
        p_win_station_name: *mut u16,
        state: i32,
    }

    #[repr(C)]
    pub(super) struct StartupInfoW {
        cb: u32,
        lp_reserved: *mut u16,
        lp_desktop: *mut u16,
        lp_title: *mut u16,
        dw_x: u32,
        dw_y: u32,
        dw_x_size: u32,
        dw_y_size: u32,
        dw_x_count_chars: u32,
        dw_y_count_chars: u32,
        dw_fill_attribute: u32,
        dw_flags: u32,
        w_show_window: u16,
        cb_reserved2: u16,
        lp_reserved2: *mut u8,
        h_std_input: isize,
        h_std_output: isize,
        h_std_error: isize,
    }

    #[repr(C)]
    pub(super) struct ProcessInformation {
        h_process: isize,
        h_thread: isize,
        dw_process_id: u32,
        dw_thread_id: u32,
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenSCManagerW(
            lp_machine_name: *const u16,
            lp_database_name: *const u16,
            dw_desired_access: u32,
        ) -> isize;
        fn CloseServiceHandle(h_sc_object: isize) -> i32;
        fn OpenServiceW(
            h_sc_manager: isize,
            lp_service_name: *const u16,
            dw_desired_access: u32,
        ) -> isize;
        fn CreateServiceW(
            h_sc_manager: isize,
            lp_service_name: *const u16,
            lp_display_name: *const u16,
            dw_desired_access: u32,
            dw_service_type: u32,
            dw_start_type: u32,
            dw_error_control: u32,
            lp_binary_path_name: *const u16,
            lp_load_order_group: *const u16,
            lpdw_tag_id: *mut u32,
            lp_dependencies: *const u16,
            lp_service_start_name: *const u16,
            lp_password: *const u16,
        ) -> isize;
        fn ChangeServiceConfigW(
            h_service: isize,
            dw_service_type: u32,
            dw_start_type: u32,
            dw_error_control: u32,
            lp_binary_path_name: *const u16,
            lp_load_order_group: *const u16,
            lpdw_tag_id: *mut u32,
            lp_dependencies: *const u16,
            lp_service_start_name: *const u16,
            lp_password: *const u16,
            lp_display_name: *const u16,
        ) -> i32;
        fn StartServiceW(
            h_service: isize,
            dw_num_service_args: u32,
            lp_service_arg_vectors: *const *const u16,
        ) -> i32;
        fn ControlService(
            h_service: isize,
            dw_control: u32,
            lp_service_status: *mut ServiceStatus,
        ) -> i32;
        fn DeleteService(h_service: isize) -> i32;
        fn QueryServiceStatusEx(
            h_service: isize,
            info_level: u32,
            lp_buffer: *mut u8,
            cb_buf_size: u32,
            pcb_bytes_needed: *mut u32,
        ) -> i32;
        fn StartServiceCtrlDispatcherW(lp_service_start_table: *const ServiceTableEntryW) -> i32;
        fn RegisterServiceCtrlHandlerExW(
            lp_service_name: *const u16,
            lp_handler_proc: extern "system" fn(u32, u32, *mut u8, *mut u8) -> u32,
            lp_context: *mut u8,
        ) -> isize;
        fn SetServiceStatus(h_service_status: isize, lp_service_status: *const ServiceStatus)
            -> i32;
        fn OpenProcessToken(
            process_handle: isize,
            desired_access: u32,
            token_handle: *mut isize,
        ) -> i32;
        fn DuplicateTokenEx(
            h_existing_token: isize,
            dw_desired_access: u32,
            lp_token_attributes: *const u8,
            impersonation_level: u32,
            token_type: u32,
            ph_new_token: *mut isize,
        ) -> i32;
        fn SetTokenInformation(
            token_handle: isize,
            token_information_class: u32,
            token_information: *const u8,
            token_information_length: u32,
        ) -> i32;
        fn CreateProcessAsUserW(
            h_token: isize,
            lp_application_name: *const u16,
            lp_command_line: *mut u16,
            lp_process_attributes: *const u8,
            lp_thread_attributes: *const u8,
            b_inherit_handles: i32,
            dw_creation_flags: u32,
            lp_environment: *const u8,
            lp_current_directory: *const u16,
            lp_startup_info: *const StartupInfoW,
            lp_process_information: *mut ProcessInformation,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn CloseHandle(h_object: isize) -> i32;
        fn GetExitCodeProcess(h_process: isize, lp_exit_code: *mut u32) -> i32;
        fn TerminateProcess(h_process: isize, u_exit_code: u32) -> i32;
        fn WaitForSingleObject(h_handle: isize, dw_milliseconds: u32) -> u32;
        fn WTSGetActiveConsoleSessionId() -> u32;
    }

    #[link(name = "wtsapi32")]
    extern "system" {
        fn WTSEnumerateSessionsW(
            h_server: isize,
            reserved: u32,
            version: u32,
            pp_session_info: *mut *mut WtsSessionInfoW,
            p_count: *mut u32,
        ) -> i32;
        fn WTSFreeMemory(p_memory: *mut u8);
    }

    fn to_wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    pub fn installed_service_exe_path() -> PathBuf {
        AppConfig::shared_data_dir().join("openremote.exe")
    }

    /// Check if the current process is running under `NT AUTHORITY\SYSTEM`
    pub fn is_running_as_system() -> bool {
        if let Ok(user) = std::env::var("USERNAME") {
            let u = user.to_ascii_uppercase();
            if u == "SYSTEM" || u.ends_with('$') {
                return true;
            }
        }
        false
    }

    /// Query Windows SCM for `OpenRemoteService` installation & running state (works without elevation)
    pub fn query_service_status() -> SystemServiceStatus {
        let exe_path = installed_service_exe_path();
        let bin_str = if exe_path.exists() {
            Some(exe_path.to_string_lossy().to_string())
        } else {
            None
        };

        unsafe {
            let scm = OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT);
            if scm == 0 {
                return SystemServiceStatus {
                    installed: false,
                    running: false,
                    running_as_system: is_running_as_system(),
                    binary_path: bin_str,
                };
            }

            let svc_name = to_wide(SERVICE_NAME);
            let svc = OpenServiceW(scm, svc_name.as_ptr(), SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG);
            if svc == 0 {
                CloseServiceHandle(scm);
                return SystemServiceStatus {
                    installed: false,
                    running: false,
                    running_as_system: is_running_as_system(),
                    binary_path: bin_str,
                };
            }

            let mut ssp: ServiceStatusProcess = zeroed();
            let mut needed = 0u32;
            let ok = QueryServiceStatusEx(
                svc,
                SC_STATUS_PROCESS_INFO,
                &mut ssp as *mut _ as *mut u8,
                size_of::<ServiceStatusProcess>() as u32,
                &mut needed,
            );
            let running = ok != 0
                && (ssp.dw_current_state == SERVICE_RUNNING
                    || ssp.dw_current_state == SERVICE_START_PENDING);

            CloseServiceHandle(svc);
            CloseServiceHandle(scm);

            SystemServiceStatus {
                installed: true,
                running,
                running_as_system: is_running_as_system(),
                binary_path: bin_str,
            }
        }
    }

    fn stop_service_handle(svc: isize) {
        unsafe {
            let mut status: ServiceStatus = zeroed();
            let _ = ControlService(svc, SERVICE_CONTROL_STOP, &mut status);
            let deadline = Instant::now() + Duration::from_secs(6);
            while Instant::now() < deadline {
                let mut ssp: ServiceStatusProcess = zeroed();
                let mut needed = 0u32;
                if QueryServiceStatusEx(
                    svc,
                    SC_STATUS_PROCESS_INFO,
                    &mut ssp as *mut _ as *mut u8,
                    size_of::<ServiceStatusProcess>() as u32,
                    &mut needed,
                ) != 0
                    && ssp.dw_current_state == SERVICE_STOPPED
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        }
    }

    /// Install (or upgrade) `OpenRemoteService` as `LocalSystem` (`SERVICE_AUTO_START`),
    /// copy our binary to `C:\ProgramData\OpenRemote\openremote.exe`, configure crash auto-restart
    /// and `SoftwareSASGeneration` (real `Ctrl+Alt+Del`), and start the service immediately!
    pub fn install_and_start_service() -> Result<SystemServiceStatus> {
        // Ensure shared ProgramData config is saved before service boots so device_code matches 100%
        if let Ok(cfg) = AppConfig::load() {
            let _ = cfg.save();
        }

        let current_exe = std::env::current_exe()
            .map_err(|e| anyhow!("无法获取当前程序路径: {}", e))?;
        let target_exe = installed_service_exe_path();
        if let Some(parent) = target_exe.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("无法创建服务目录 {:?}: {}", parent, e))?;
        }

        unsafe {
            let scm = OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_ALL_ACCESS);
            if scm == 0 {
                return Err(anyhow!(
                    "打开 Windows 服务管理器失败 ({})，请以管理员身份运行 OpenRemote",
                    std::io::Error::last_os_error()
                ));
            }

            let svc_name = to_wide(SERVICE_NAME);
            let display_name = to_wide(SERVICE_DISPLAY_NAME);
            let existing = OpenServiceW(scm, svc_name.as_ptr(), SERVICE_ALL_ACCESS);

            // Stop existing service before replacing binary so openremote.exe isn't file-locked
            if existing != 0 {
                stop_service_handle(existing);
            }

            // Copy current exe into C:\ProgramData\OpenRemote\openremote.exe (if different path)
            let same_path = current_exe
                .canonicalize()
                .ok()
                .zip(target_exe.canonicalize().ok())
                .map(|(a, b)| a == b)
                .unwrap_or(false);

            if !same_path {
                let mut copied = false;
                let mut last_copy_err = String::new();
                for _ in 0..10 {
                    match std::fs::copy(&current_exe, &target_exe) {
                        Ok(_) => {
                            copied = true;
                            break;
                        }
                        Err(e) => {
                            last_copy_err = e.to_string();
                            std::thread::sleep(Duration::from_millis(200));
                        }
                    }
                }
                if !copied {
                    if existing != 0 {
                        CloseServiceHandle(existing);
                    }
                    CloseServiceHandle(scm);
                    return Err(anyhow!(
                        "复制服务程序到 {:?} 失败: {}",
                        target_exe,
                        last_copy_err
                    ));
                }
            }

            let bin_cmd = format!("\"{}\" --service", target_exe.to_string_lossy());
            let bin_wide = to_wide(&bin_cmd);

            let svc = if existing != 0 {
                let ok = ChangeServiceConfigW(
                    existing,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    bin_wide.as_ptr(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null(), // LocalSystem
                    ptr::null(),
                    display_name.as_ptr(),
                );
                if ok == 0 {
                    let err = std::io::Error::last_os_error();
                    CloseServiceHandle(existing);
                    CloseServiceHandle(scm);
                    return Err(anyhow!("更新 Windows 服务配置失败: {}", err));
                }
                existing
            } else {
                let created = CreateServiceW(
                    scm,
                    svc_name.as_ptr(),
                    display_name.as_ptr(),
                    SERVICE_ALL_ACCESS,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    bin_wide.as_ptr(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null(), // LocalSystem (NT AUTHORITY\SYSTEM)
                    ptr::null(),
                );
                if created == 0 {
                    let err = std::io::Error::last_os_error();
                    CloseServiceHandle(scm);
                    return Err(anyhow!("创建 Windows SYSTEM 服务失败: {}", err));
                }
                created
            };

            // Configure automatic service recovery on failure & enable SoftwareSASGeneration for Ctrl+Alt+Del
            let _ = std::process::Command::new("sc")
                .args([
                    "failure",
                    SERVICE_NAME,
                    "reset=",
                    "86400",
                    "actions=",
                    "restart/3000/restart/3000/restart/5000",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .output();
            let _ = std::process::Command::new("reg")
                .args([
                    "add",
                    r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System",
                    "/v",
                    "SoftwareSASGeneration",
                    "/t",
                    "REG_DWORD",
                    "/d",
                    "1",
                    "/f",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .output();

            let started = StartServiceW(svc, 0, ptr::null());
            if started == 0 {
                let err = std::io::Error::last_os_error();
                // ERROR_SERVICE_ALREADY_RUNNING = 1056
                if err.raw_os_error() != Some(1056) {
                    CloseServiceHandle(svc);
                    CloseServiceHandle(scm);
                    return Err(anyhow!("启动 Windows SYSTEM 服务失败: {}", err));
                }
            }

            // Wait up to 6s for SERVICE_RUNNING
            let deadline = Instant::now() + Duration::from_secs(6);
            while Instant::now() < deadline {
                let mut ssp: ServiceStatusProcess = zeroed();
                let mut needed = 0u32;
                if QueryServiceStatusEx(
                    svc,
                    SC_STATUS_PROCESS_INFO,
                    &mut ssp as *mut _ as *mut u8,
                    size_of::<ServiceStatusProcess>() as u32,
                    &mut needed,
                ) != 0
                    && ssp.dw_current_state == SERVICE_RUNNING
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(150));
            }

            CloseServiceHandle(svc);
            CloseServiceHandle(scm);
        }

        crate::diag_log("Installed and started OpenRemoteService (LocalSystem)");
        Ok(query_service_status())
    }

    /// Stop and uninstall `OpenRemoteService` cleanly
    pub fn uninstall_service() -> Result<SystemServiceStatus> {
        unsafe {
            let scm = OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_ALL_ACCESS);
            if scm == 0 {
                return Err(anyhow!(
                    "打开 Windows 服务管理器失败 ({})",
                    std::io::Error::last_os_error()
                ));
            }
            let svc_name = to_wide(SERVICE_NAME);
            let svc = OpenServiceW(
                scm,
                svc_name.as_ptr(),
                SERVICE_STOP | SERVICE_QUERY_STATUS | DELETE,
            );
            if svc != 0 {
                stop_service_handle(svc);
                let _ = DeleteService(svc);
                CloseServiceHandle(svc);
            }
            CloseServiceHandle(scm);
        }

        let target_exe = installed_service_exe_path();
        if target_exe.exists() {
            let _ = std::fs::remove_file(&target_exe);
        }
        crate::diag_log("Stopped and uninstalled OpenRemoteService");
        Ok(query_service_status())
    }

    fn report_service_status(state: u32, exit_code: u32, wait_hint: u32) {
        let handle = SERVICE_STATUS_HANDLE.load(Ordering::Acquire);
        if handle == 0 {
            return;
        }
        let controls = if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE
        } else {
            0
        };
        let status = ServiceStatus {
            dw_service_type: SERVICE_WIN32_OWN_PROCESS,
            dw_current_state: state,
            dw_controls_accepted: controls,
            dw_win32_exit_code: exit_code,
            dw_service_specific_exit_code: 0,
            dw_check_point: 0,
            dw_wait_hint: wait_hint,
        };
        unsafe {
            let _ = SetServiceStatus(handle, &status);
        }
    }

    extern "system" fn service_ctrl_handler(
        control: u32,
        _event_type: u32,
        _event_data: *mut u8,
        _context: *mut u8,
    ) -> u32 {
        match control {
            SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
                report_service_status(SERVICE_STOP_PENDING, 0, 4000);
                SERVICE_STOP_FLAG.store(true, Ordering::Release);
                0
            }
            SERVICE_CONTROL_SESSIONCHANGE => {
                SERVICE_SESSION_CHANGED.store(true, Ordering::Release);
                0
            }
            SERVICE_CONTROL_INTERROGATE => 0,
            _ => 120, // ERROR_CALL_NOT_IMPLEMENTED
        }
    }

    /// Pick the best Windows session to host our SYSTEM `--host-worker`:
    /// Always prefers the physical Console session (including Lock Screen / Pre-Logon Winlogon!),
    /// or follows an Active RDP session while active and falls right back to Console when RDP disconnects.
    fn pick_target_session_id() -> Option<u32> {
        unsafe {
            let console_sid = WTSGetActiveConsoleSessionId();
            let mut active_console = false;
            let mut active_other: Option<u32> = None;

            let mut info_ptr: *mut WtsSessionInfoW = ptr::null_mut();
            let mut count: u32 = 0;
            if WTSEnumerateSessionsW(0, 0, 1, &mut info_ptr, &mut count) != 0 && !info_ptr.is_null() {
                let slice = std::slice::from_raw_parts(info_ptr, count as usize);
                for s in slice {
                    if s.session_id == 0 {
                        continue; // Never render desktop from Session 0
                    }
                    if s.state == 0 {
                        // WTSActive
                        if s.session_id == console_sid {
                            active_console = true;
                        } else if active_other.is_none() {
                            active_other = Some(s.session_id);
                        }
                    }
                }
                WTSFreeMemory(info_ptr as *mut u8);
            }

            if active_console && console_sid != u32::MAX {
                Some(console_sid)
            } else if let Some(rdp_sid) = active_other {
                Some(rdp_sid)
            } else if console_sid != u32::MAX && console_sid != 0 {
                // Pre-login / Lock Screen / Disconnected RDP -> always attach to Console (Session 1+)!
                Some(console_sid)
            } else {
                None
            }
        }
    }

    struct WorkerChild {
        h_process: isize,
        pid: u32,
        session_id: u32,
    }

    impl WorkerChild {
        fn is_alive(&self) -> bool {
            let mut code = 0u32;
            unsafe { GetExitCodeProcess(self.h_process, &mut code) != 0 && code == STILL_ACTIVE }
        }

        fn terminate_and_close(self) {
            unsafe {
                let _ = TerminateProcess(self.h_process, 0);
                let _ = WaitForSingleObject(self.h_process, 2000);
                CloseHandle(self.h_process);
            }
        }
    }

    /// Spawn `openremote.exe --host-worker` inside `target_session_id` with full `LocalSystem` privileges!
    fn spawn_system_worker_in_session(target_session_id: u32) -> Result<WorkerChild> {
        let exe = std::env::current_exe()
            .map_err(|e| anyhow!("current_exe failed: {}", e))?;
        let mut cmdline = to_wide(&format!("\"{}\" --host-worker", exe.to_string_lossy()));
        let mut desktop = to_wide("winsta0\\default");

        unsafe {
            let mut cur_token: isize = 0;
            if OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut cur_token) == 0 {
                return Err(anyhow!(
                    "OpenProcessToken failed: {}",
                    std::io::Error::last_os_error()
                ));
            }

            let mut dup_token: isize = 0;
            let dup_ok = DuplicateTokenEx(
                cur_token,
                MAXIMUM_ALLOWED,
                ptr::null(),
                SECURITY_IDENTIFICATION,
                TOKEN_PRIMARY,
                &mut dup_token,
            );
            CloseHandle(cur_token);
            if dup_ok == 0 {
                return Err(anyhow!(
                    "DuplicateTokenEx failed: {}",
                    std::io::Error::last_os_error()
                ));
            }

            let sid = target_session_id;
            let set_ok = SetTokenInformation(
                dup_token,
                TOKEN_SESSION_ID,
                &sid as *const u32 as *const u8,
                size_of::<u32>() as u32,
            );
            if set_ok == 0 {
                let err = std::io::Error::last_os_error();
                CloseHandle(dup_token);
                return Err(anyhow!("SetTokenInformation(SessionId={}) failed: {}", sid, err));
            }

            let mut si: StartupInfoW = zeroed();
            si.cb = size_of::<StartupInfoW>() as u32;
            si.lp_desktop = desktop.as_mut_ptr();
            let mut pi: ProcessInformation = zeroed();

            let ok = CreateProcessAsUserW(
                dup_token,
                ptr::null(),
                cmdline.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                ptr::null(),
                ptr::null(),
                &si,
                &mut pi,
            );
            CloseHandle(dup_token);

            if ok == 0 {
                return Err(anyhow!(
                    "CreateProcessAsUserW(session={}) failed: {}",
                    sid,
                    std::io::Error::last_os_error()
                ));
            }

            CloseHandle(pi.h_thread);
            Ok(WorkerChild {
                h_process: pi.h_process,
                pid: pi.dw_process_id,
                session_id: sid,
            })
        }
    }

    extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
        let svc_name = to_wide(SERVICE_NAME);
        let handle = unsafe {
            RegisterServiceCtrlHandlerExW(svc_name.as_ptr(), service_ctrl_handler, ptr::null_mut())
        };
        if handle == 0 {
            return;
        }
        SERVICE_STATUS_HANDLE.store(handle, Ordering::Release);
        SERVICE_STOP_FLAG.store(false, Ordering::Release);
        report_service_status(SERVICE_START_PENDING, 0, 3000);

        crate::diag_log("OpenRemoteService (Session 0 LocalSystem) started.");
        report_service_status(SERVICE_RUNNING, 0, 0);

        let mut worker: Option<WorkerChild> = None;
        let mut pending_sid: Option<(u32, Instant)> = None;

        while !SERVICE_STOP_FLAG.load(Ordering::Acquire) {
            let _session_evt = SERVICE_SESSION_CHANGED.swap(false, Ordering::AcqRel);
            if let Some(target_sid) = pick_target_session_id() {
                let mut should_spawn = false;
                match worker {
                    None => {
                        should_spawn = true;
                        pending_sid = None;
                    }
                    Some(ref w) if !w.is_alive() => {
                        should_spawn = true;
                        pending_sid = None;
                    }
                    Some(ref w) if w.session_id != target_sid => {
                        // Debounce session switches by 1.5s so transient Windows RDP logon sessions don't churn!
                        match pending_sid {
                            Some((sid, since)) if sid == target_sid => {
                                if since.elapsed() >= Duration::from_millis(1500) {
                                    should_spawn = true;
                                    pending_sid = None;
                                }
                            }
                            _ => {
                                pending_sid = Some((target_sid, Instant::now()));
                            }
                        }
                    }
                    _ => {
                        pending_sid = None;
                    }
                }

                if should_spawn {
                    if let Some(old) = worker.take() {
                        crate::diag_log(&format!(
                            "Stopping previous SYSTEM host-worker (pid={}, sid={}) for target sid={}",
                            old.pid, old.session_id, target_sid
                        ));
                        old.terminate_and_close();
                        std::thread::sleep(Duration::from_millis(250));
                    }

                    match spawn_system_worker_in_session(target_sid) {
                        Ok(child) => {
                            crate::diag_log(&format!(
                                "Spawned SYSTEM host-worker (pid={}) in session #{}",
                                child.pid, child.session_id
                            ));
                            worker = Some(child);
                        }
                        Err(e) => {
                            crate::diag_log(&format!(
                                "Failed to spawn SYSTEM host-worker in session #{}: {}",
                                target_sid, e
                            ));
                            std::thread::sleep(Duration::from_secs(2));
                        }
                    }
                }
            }

            std::thread::sleep(Duration::from_millis(500));
        }

        if let Some(old) = worker.take() {
            crate::diag_log(&format!(
                "Service stopping -> terminating SYSTEM host-worker (pid={})",
                old.pid
            ));
            old.terminate_and_close();
        }
        crate::diag_log("OpenRemoteService stopped cleanly.");
        report_service_status(SERVICE_STOPPED, 0, 0);
    }

    /// Entrypoint for `openremote.exe --service` (called by Windows SCM)
    pub fn run_windows_service_dispatcher() -> Result<()> {
        let mut svc_name = to_wide(SERVICE_NAME);
        let table = [
            ServiceTableEntryW {
                lp_service_name: svc_name.as_mut_ptr(),
                lp_service_proc: Some(service_main),
            },
            ServiceTableEntryW {
                lp_service_name: ptr::null_mut(),
                lp_service_proc: None,
            },
        ];

        let ok = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
        if ok == 0 {
            return Err(anyhow!(
                "StartServiceCtrlDispatcherW failed ({})",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn verify_win32_service_abi_and_query() {
            assert_eq!(size_of::<ServiceStatus>(), 28);
            assert_eq!(size_of::<ServiceStatusProcess>(), 36);
            #[cfg(target_pointer_width = "64")]
            {
                assert_eq!(size_of::<StartupInfoW>(), 104);
                assert_eq!(size_of::<ProcessInformation>(), 24);
                assert_eq!(size_of::<WtsSessionInfoW>(), 24);
            }
            let st = query_service_status();
            let target = pick_target_session_id();
            println!(
                "Service status: {:?}, target_session={:?}",
                st, target
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod fallback_service {
    use super::SystemServiceStatus;
    use anyhow::{anyhow, Result};

    pub fn is_running_as_system() -> bool {
        false
    }

    pub fn query_service_status() -> SystemServiceStatus {
        SystemServiceStatus {
            installed: false,
            running: false,
            running_as_system: false,
            binary_path: None,
        }
    }

    pub fn install_and_start_service() -> Result<SystemServiceStatus> {
        Err(anyhow!("Windows SYSTEM service is only supported on Windows"))
    }

    pub fn uninstall_service() -> Result<SystemServiceStatus> {
        Ok(query_service_status())
    }

    pub fn run_windows_service_dispatcher() -> Result<()> {
        Err(anyhow!("Windows SYSTEM service is only supported on Windows"))
    }
}

#[cfg(target_os = "windows")]
pub use win_service::{
    install_and_start_service, is_running_as_system, query_service_status,
    run_windows_service_dispatcher, uninstall_service,
};

#[cfg(not(target_os = "windows"))]
pub use fallback_service::{
    install_and_start_service, is_running_as_system, query_service_status,
    run_windows_service_dispatcher, uninstall_service,
};
