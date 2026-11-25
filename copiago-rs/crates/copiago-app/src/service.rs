//! Apple Mobile Device Service: read-only diagnosis and the elevated repair entry point.
//!
//! The device worker talks to this service (usbmuxd) on 127.0.0.1:27015. Diagnosis runs
//! without administrator rights. Repair runs in a separate elevated copy of CopiaGo that
//! only touches this one fixed service, so no shell and no user input is involved.

use std::{
    env, io,
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::PathBuf,
    process::Command,
    ptr::{addr_of, null, null_mut},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows_sys::{
    Win32::{
        Devices::DeviceAndDriverInstallation::{
            CM_Get_DevNode_Status, CR_SUCCESS, DIGCF_ALLCLASSES, DIGCF_PRESENT, DN_HAS_PROBLEM,
            HDEVINFO, SP_DEVINFO_DATA, SPDRP_HARDWAREID, SetupDiDestroyDeviceInfoList,
            SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW,
        },
        Foundation::{
            CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SERVICE_ALREADY_RUNNING,
            ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SUCCESS, GetLastError, INVALID_HANDLE_VALUE,
            WAIT_OBJECT_0,
        },
        NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
            TCP_TABLE_OWNER_PID_LISTENER,
        },
        System::{
            Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW},
            Services::{
                ChangeServiceConfigW, CloseServiceHandle, ControlService, OpenSCManagerW,
                OpenServiceW, QueryServiceStatus, SC_HANDLE, SC_MANAGER_CONNECT,
                SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG, SERVICE_CONTROL_STOP, SERVICE_NO_CHANGE,
                SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START, SERVICE_START_PENDING,
                SERVICE_STATUS, SERVICE_STOP, SERVICE_STOP_PENDING, SERVICE_STOPPED, StartServiceW,
            },
            Threading::{
                CREATE_NO_WINDOW, GetExitCodeProcess, OpenProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, WaitForSingleObject,
            },
        },
        UI::{
            Shell::{
                SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
                ShellExecuteExW,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    },
    core::PCWSTR,
    w,
};

/// Command-line flag of the elevated service repair process.
pub const REPAIR_FLAG: &str = "--repair-apple-service";
/// Command-line flag of the elevated USB driver repair process.
pub const DRIVER_REPAIR_FLAG: &str = "--repair-apple-driver";
/// Exit code of a repair process when the Apple component it needs is not installed.
pub const REPAIR_NOT_INSTALLED: u32 = 3;
/// Apple's USB driver, installed with Apple Mobile Device Support.
const DRIVER_INF: &str = "Apple\\Mobile Device Support\\Drivers\\usbaapl64.inf";

const SERVICE_NAME: PCWSTR = w!("Apple Mobile Device Service");
const SERVICE_KEY: PCWSTR = w!("SYSTEM\\CurrentControlSet\\Services\\Apple Mobile Device Service");
const USBMUXD_PORT: u16 = 27015;
const AF_INET: u32 = 2;
const STATE_TIMEOUT: Duration = Duration::from_secs(20);
const REPAIR_TIMEOUT_MS: u32 = 90_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Running,
    Stopped,
    Starting,
    Stopping,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartType {
    Automatic,
    Manual,
    Disabled,
    Other,
}

/// What a non-elevated process can learn about the service.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceReport {
    /// `None` when the Service Control Manager could not be asked.
    pub installed: Option<bool>,
    pub state: Option<RunState>,
    pub start: Option<StartType>,
    /// Something accepts connections on 127.0.0.1:27015.
    pub listening: bool,
    /// Executable file name of the listener, when Windows reveals it.
    pub port_owner: Option<String>,
}

/// The single most useful explanation of a [`ServiceReport`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diagnosis {
    /// A listener exists. Device enumeration remains the final check.
    Listening,
    NotInstalled,
    Disabled,
    Stopped,
    /// Another program holds the port, so the Apple service cannot listen on it.
    PortTaken,
    /// The service is installed but does not answer; a restart usually helps.
    Unresponsive,
}

impl ServiceReport {
    /// A listening port alone does not prove the Apple service is the listener.
    /// `enumeration_failed` reports that the worker could not list devices.
    pub fn diagnose(&self, enumeration_failed: bool) -> Diagnosis {
        let apple_running = self.state == Some(RunState::Running);
        if self.foreign_owner().is_some() && (enumeration_failed || !apple_running) {
            return Diagnosis::PortTaken;
        }
        if self.listening && !enumeration_failed {
            return Diagnosis::Listening;
        }
        match (self.installed, self.start, self.state) {
            (Some(false), ..) if !self.listening => Diagnosis::NotInstalled,
            (_, Some(StartType::Disabled), _) => Diagnosis::Disabled,
            (_, _, Some(RunState::Stopped)) => Diagnosis::Stopped,
            _ => Diagnosis::Unresponsive,
        }
    }

    /// The listener's file name when it is clearly not an Apple component.
    pub fn foreign_owner(&self) -> Option<&str> {
        self.port_owner
            .as_deref()
            .filter(|name| !name.to_ascii_lowercase().contains("apple"))
    }

    /// One fixed-format line for the diagnostic log. The file name of a port owner is
    /// included because it identifies the conflicting program; it is not personal data.
    pub fn log_fields(&self) -> String {
        format!(
            "installed={:?} state={:?} start={:?} listening={} port_owner={}",
            self.installed,
            self.state,
            self.start,
            self.listening,
            self.port_owner.as_deref().unwrap_or("-")
        )
    }
}

/// Collects the report. Takes up to about 300 ms when nothing listens on the port.
pub fn inspect() -> ServiceReport {
    let (installed, state) = match query_state() {
        Ok(state) => (Some(true), Some(state)),
        Err(ERROR_SERVICE_DOES_NOT_EXIST) => (Some(false), None),
        Err(_) => (None, None),
    };
    let listening = TcpStream::connect_timeout(
        &SocketAddrV4::new(Ipv4Addr::LOCALHOST, USBMUXD_PORT).into(),
        Duration::from_millis(300),
    )
    .is_ok();
    ServiceReport {
        installed,
        state,
        start: start_type(),
        listening,
        port_owner: if listening { port_owner() } else { None },
    }
}

fn query_state() -> Result<RunState, u32> {
    // SAFETY: The manager and service handles are checked for null, used only while
    // open, and closed exactly once.
    unsafe {
        let manager = OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            return Err(GetLastError());
        }
        let service = OpenServiceW(manager, SERVICE_NAME, SERVICE_QUERY_STATUS);
        let result = if service.is_null() {
            Err(GetLastError())
        } else {
            let result = current_state(service);
            CloseServiceHandle(service);
            result
        };
        CloseServiceHandle(manager);
        result
    }
}

/// # Safety
/// `service` must be an open handle with `SERVICE_QUERY_STATUS` access.
unsafe fn current_state(service: SC_HANDLE) -> Result<RunState, u32> {
    // SAFETY: All-zero is a valid SERVICE_STATUS.
    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: The caller guarantees the handle; the output is writable for the call.
    let ok = unsafe { QueryServiceStatus(service, &mut status) };
    if ok == 0 {
        // SAFETY: Reads the calling thread's last error.
        return Err(unsafe { GetLastError() });
    }
    Ok(match status.dwCurrentState {
        SERVICE_RUNNING => RunState::Running,
        SERVICE_STOPPED => RunState::Stopped,
        SERVICE_START_PENDING => RunState::Starting,
        SERVICE_STOP_PENDING => RunState::Stopping,
        _ => RunState::Other,
    })
}

fn start_type() -> Option<StartType> {
    let mut value = 0u32;
    let mut bytes = size_of::<u32>() as u32;
    // SAFETY: Both strings are static and NUL-terminated; value and bytes are writable.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            SERVICE_KEY,
            w!("Start"),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut value as *mut u32).cast(),
            &mut bytes,
        )
    };
    (status == ERROR_SUCCESS).then_some(match value {
        2 => StartType::Automatic,
        3 => StartType::Manual,
        4 => StartType::Disabled,
        _ => StartType::Other,
    })
}

/// File name of the process listening on the usbmuxd port, if Windows reveals it.
fn port_owner() -> Option<String> {
    let mut size = 0u32;
    // SAFETY: A null table with size 0 asks only for the required size.
    let status = unsafe {
        GetExtendedTcpTable(
            null_mut(),
            &mut size,
            0,
            AF_INET,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if status != ERROR_INSUFFICIENT_BUFFER || size == 0 {
        return None;
    }
    // u64 elements keep the table aligned for its u32 fields.
    let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
    let table = buffer.as_mut_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
    // SAFETY: The buffer holds at least `size` writable bytes.
    let status = unsafe {
        GetExtendedTcpTable(
            table.cast(),
            &mut size,
            0,
            AF_INET,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    // SAFETY: The call succeeded, so the buffer starts with a valid table header followed
    // by dwNumEntries rows. Raw pointers keep the whole allocation's provenance.
    let rows = unsafe {
        let count = (*table).dwNumEntries as usize;
        let first = addr_of!((*table).table).cast::<MIB_TCPROW_OWNER_PID>();
        std::slice::from_raw_parts(first, count)
    };
    let pid = rows
        .iter()
        .find(|row| u16::from_be(row.dwLocalPort as u16) == USBMUXD_PORT)?
        .dwOwningPid;
    process_name(pid)
}

fn process_name(pid: u32) -> Option<String> {
    // SAFETY: OpenProcess has no memory preconditions; the handle is checked and closed.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut path = [0u16; 1024];
    let mut length = path.len() as u32;
    // SAFETY: The handle is open and the buffer is writable for `length` characters.
    let ok = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, path.as_mut_ptr(), &mut length)
    };
    // SAFETY: The handle came from OpenProcess above and is closed once.
    unsafe { CloseHandle(process) };
    if ok == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&path[..length as usize]);
    // Only the file name: a full path can contain the user's account name.
    path.rsplit('\\').next().map(str::to_owned)
}

/// Starts the elevated repair on a background thread so the UAC prompt never blocks the
/// UI thread. The thread yields the repair process's exit code; an error with raw OS code
/// 1223 means the user declined the prompt.
pub fn spawn_repair(flag: &'static str) -> JoinHandle<io::Result<u32>> {
    thread::spawn(move || run_elevated_repair(flag))
}

fn run_elevated_repair(flag: &str) -> io::Result<u32> {
    let executable: Vec<u16> = env::current_exe()?
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let parameters: Vec<u16> = flag.encode_utf16().chain(Some(0)).collect();
    // SAFETY: All-zero is a valid initial state for this size-tagged Win32 structure.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
    info.lpVerb = w!("runas");
    info.lpFile = executable.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: All strings remain NUL-terminated and alive throughout the call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.hProcess.is_null() {
        return Err(io::Error::other("repair returned no process handle"));
    }
    // SAFETY: ShellExecuteExW transferred ownership of hProcess, which is closed once below.
    let waited = unsafe { WaitForSingleObject(info.hProcess, REPAIR_TIMEOUT_MS) };
    let mut code = 1u32;
    let mut read = false;
    if waited == WAIT_OBJECT_0 {
        // SAFETY: The handle is valid and the output points to initialized storage.
        read = unsafe { GetExitCodeProcess(info.hProcess, &mut code) } != 0;
    }
    // SAFETY: See above.
    unsafe { CloseHandle(info.hProcess) };
    if read {
        Ok(code)
    } else {
        Err(io::ErrorKind::TimedOut.into())
    }
}

/// Body of the elevated process. Re-enables automatic start, then starts the service, or
/// restarts it when it runs but does not answer. Returns the process exit code:
/// 0 running, [`REPAIR_NOT_INSTALLED`] missing, 1 any other failure.
pub fn repair() -> u32 {
    // SAFETY: Handles are checked for null, used only while open, and closed exactly once.
    unsafe {
        let manager = OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            return 1;
        }
        let access = SERVICE_QUERY_STATUS | SERVICE_START | SERVICE_STOP | SERVICE_CHANGE_CONFIG;
        let service = OpenServiceW(manager, SERVICE_NAME, access);
        let code = if service.is_null() {
            if GetLastError() == ERROR_SERVICE_DOES_NOT_EXIST {
                REPAIR_NOT_INSTALLED
            } else {
                1
            }
        } else {
            let code = repair_service(service);
            CloseServiceHandle(service);
            code
        };
        CloseServiceHandle(manager);
        code
    }
}

/// # Safety
/// `service` must be open with query, start, stop and change-config access.
unsafe fn repair_service(service: SC_HANDLE) -> u32 {
    // Apple installs the service as automatic. Optimizer tools often switch it to manual
    // or disabled, which also breaks the connection after the next restart.
    if matches!(start_type(), Some(StartType::Disabled | StartType::Manual)) {
        // SAFETY: The handle has change-config access; null strings mean "no change".
        unsafe {
            ChangeServiceConfigW(
                service,
                SERVICE_NO_CHANGE,
                SERVICE_AUTO_START,
                SERVICE_NO_CHANGE,
                null(),
                null(),
                null_mut(),
                null(),
                null(),
                null(),
                null(),
            )
        };
    }
    // SAFETY: The caller guarantees query access.
    let running = unsafe { current_state(service) } == Ok(RunState::Running);
    if running {
        // SAFETY: All-zero is a valid SERVICE_STATUS output for ControlService.
        let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
        // SAFETY: The handle has stop access and the output is writable.
        unsafe { ControlService(service, SERVICE_CONTROL_STOP, &mut status) };
        // SAFETY: Same handle and access.
        unsafe { wait_for(service, RunState::Stopped) };
    }
    // SAFETY: No arguments are passed; the handle has start access.
    let started = unsafe { StartServiceW(service, 0, null()) } != 0;
    // SAFETY: Reads the calling thread's last error right after the call.
    if !started && unsafe { GetLastError() } != ERROR_SERVICE_ALREADY_RUNNING {
        return 1;
    }
    // SAFETY: Same handle and access.
    if unsafe { wait_for(service, RunState::Running) } {
        0
    } else {
        1
    }
}

/// # Safety
/// `service` must be open with `SERVICE_QUERY_STATUS` access.
unsafe fn wait_for(service: SC_HANDLE, target: RunState) -> bool {
    let deadline = Instant::now() + STATE_TIMEOUT;
    while Instant::now() < deadline {
        // SAFETY: Forwarded from the caller.
        match unsafe { current_state(service) } {
            Ok(state) if state == target => return true,
            Ok(_) => thread::sleep(Duration::from_millis(250)),
            Err(_) => return false,
        }
    }
    false
}

/// Whether Windows itself sees an Apple phone on USB, independent of the Apple service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbPresence {
    /// No Apple mobile device is attached: cable, port, or a charge-only cable.
    Absent,
    /// Attached and every device node has a working driver.
    Working,
    /// Attached, but a device node reports a problem, usually a missing Apple driver.
    DriverProblem,
}

/// Looks for Apple mobile devices (vendor 05AC, product 12xx) among present USB device
/// nodes, including their interface nodes, which is where a missing Apple driver shows.
pub fn apple_usb_presence() -> UsbPresence {
    // SAFETY: The set is checked, used only while open and destroyed once. Every output
    // pointer refers to writable local storage of the stated size.
    unsafe {
        let set = SetupDiGetClassDevsW(
            null(),
            w!("USB"),
            null_mut(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        );
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return UsbPresence::Absent;
        }
        let mut found = false;
        let mut problem = false;
        let mut index = 0;
        loop {
            let mut data: SP_DEVINFO_DATA = std::mem::zeroed();
            data.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiEnumDeviceInfo(set, index, &mut data) == 0 {
                break;
            }
            index += 1;
            let mut ids = [0u16; 512];
            let read = SetupDiGetDeviceRegistryPropertyW(
                set,
                &data,
                SPDRP_HARDWAREID,
                null_mut(),
                ids.as_mut_ptr().cast(),
                size_of_val(&ids) as u32,
                null_mut(),
            );
            if read == 0 || !is_apple_mobile(&String::from_utf16_lossy(&ids)) {
                continue;
            }
            found = true;
            let (mut status, mut code) = (0, 0);
            if CM_Get_DevNode_Status(&mut status, &mut code, data.DevInst, 0) == CR_SUCCESS
                && status & DN_HAS_PROBLEM != 0
            {
                problem = true;
            }
        }
        SetupDiDestroyDeviceInfoList(set);
        match (found, problem) {
            (false, _) => UsbPresence::Absent,
            (true, false) => UsbPresence::Working,
            (true, true) => UsbPresence::DriverProblem,
        }
    }
}

fn is_apple_mobile(hardware_ids: &str) -> bool {
    hardware_ids
        .to_ascii_uppercase()
        .contains("VID_05AC&PID_12")
}

/// Body of the elevated driver repair. Adds Apple's bundled USB driver to the driver store
/// and installs it on matching devices, then asks Windows to re-scan. Returns 0 on success,
/// [`REPAIR_NOT_INSTALLED`] when Apple Mobile Device Support is missing, 1 otherwise.
pub fn repair_driver() -> u32 {
    let Some(common) =
        env::var_os("CommonProgramW6432").or_else(|| env::var_os("CommonProgramFiles"))
    else {
        return 1;
    };
    let inf = PathBuf::from(common).join(DRIVER_INF);
    if !inf.is_file() {
        return REPAIR_NOT_INSTALLED;
    }
    // Use the system copy, never a pnputil found through PATH.
    let root = env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let pnputil = PathBuf::from(root).join("System32").join("pnputil.exe");
    let added = Command::new(&pnputil)
        .arg("/add-driver")
        .arg(&inf)
        .arg("/install")
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    let _ = Command::new(&pnputil)
        .arg("/scan-devices")
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    // 3010: installed, restart recommended. 259: the store already had this driver.
    match added.ok().and_then(|status| status.code()) {
        Some(0 | 259 | 3010) => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(
        installed: Option<bool>,
        state: Option<RunState>,
        start: Option<StartType>,
    ) -> ServiceReport {
        ServiceReport {
            installed,
            state,
            start,
            listening: false,
            port_owner: None,
        }
    }

    #[test]
    fn explains_why_nothing_listens() {
        let missing = report(Some(false), None, None);
        assert_eq!(missing.diagnose(false), Diagnosis::NotInstalled);
        let disabled = report(
            Some(true),
            Some(RunState::Stopped),
            Some(StartType::Disabled),
        );
        assert_eq!(disabled.diagnose(false), Diagnosis::Disabled);
        let stopped = report(Some(true), Some(RunState::Stopped), Some(StartType::Manual));
        assert_eq!(stopped.diagnose(false), Diagnosis::Stopped);
        let silent = report(
            Some(true),
            Some(RunState::Running),
            Some(StartType::Automatic),
        );
        assert_eq!(silent.diagnose(false), Diagnosis::Unresponsive);
        assert_eq!(
            report(None, None, None).diagnose(false),
            Diagnosis::Unresponsive
        );
    }

    #[test]
    fn a_listener_is_enough_until_enumeration_fails() {
        let mut running = report(
            Some(true),
            Some(RunState::Running),
            Some(StartType::Automatic),
        );
        running.listening = true;
        running.port_owner = Some("AppleMobileDeviceService.exe".into());
        assert_eq!(running.diagnose(false), Diagnosis::Listening);
        assert_eq!(running.diagnose(true), Diagnosis::Unresponsive);
        // The Store edition can listen without the classic service.
        let mut store = report(Some(false), None, None);
        store.listening = true;
        assert_eq!(store.diagnose(false), Diagnosis::Listening);
    }

    #[test]
    fn recognizes_apple_mobile_hardware_ids() {
        assert!(is_apple_mobile("USB\\VID_05AC&PID_12A8&REV_1702&MI_01"));
        assert!(is_apple_mobile("usb\\vid_05ac&pid_12a8"));
        assert!(!is_apple_mobile("USB\\VID_05AC&PID_8290"));
        assert!(!is_apple_mobile("USB\\VID_046D&PID_C52B"));
    }

    #[test]
    fn names_a_foreign_port_owner() {
        let mut taken = report(
            Some(true),
            Some(RunState::Stopped),
            Some(StartType::Automatic),
        );
        taken.listening = true;
        taken.port_owner = Some("srcds.exe".into());
        assert_eq!(taken.diagnose(false), Diagnosis::PortTaken);
        assert_eq!(taken.foreign_owner(), Some("srcds.exe"));
        let mut apple = taken.clone();
        apple.port_owner = Some("AppleMobileDeviceProcess.exe".into());
        assert_eq!(apple.foreign_owner(), None);
    }
}
