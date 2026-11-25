//! Apple Mobile Device Service: read-only diagnosis and the elevated repair entry point.
//!
//! The device worker talks to this service (usbmuxd) on 127.0.0.1:27015. Diagnosis runs
//! without administrator rights. Repair runs in a separate elevated copy of CopiaGo that
//! only touches this one fixed service, so no shell and no user input is involved.

use std::{
    env, fs, io,
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
            CM_Get_DevNode_Status, CM_PROB_DISABLED, CR_SUCCESS, DIGCF_ALLCLASSES, DIGCF_PRESENT,
            DN_HAS_PROBLEM, HDEVINFO, SP_DEVINFO_DATA, SPDRP_COMPATIBLEIDS, SPDRP_HARDWAREID,
            SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
            SetupDiGetDeviceRegistryPropertyW,
        },
        Foundation::{
            CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, ERROR_SERVICE_ALREADY_RUNNING,
            ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SUCCESS, GetLastError, INVALID_HANDLE_VALUE,
            WAIT_OBJECT_0,
        },
        NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
            TCP_TABLE_OWNER_PID_LISTENER,
        },
        System::{
            Registry::{
                HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_DWORD,
                RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
            },
            Services::{
                ChangeServiceConfigW, CloseServiceHandle, ControlService, OpenSCManagerW,
                OpenServiceW, QueryServiceStatus, SC_HANDLE, SC_MANAGER_CONNECT,
                SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG, SERVICE_CONTROL_STOP, SERVICE_NO_CHANGE,
                SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START, SERVICE_START_PENDING,
                SERVICE_STATUS, SERVICE_STOP, SERVICE_STOP_PENDING, SERVICE_STOPPED, StartServiceW,
            },
            Threading::{
                CREATE_NO_WINDOW, GetExitCodeProcess, INFINITE, OpenProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, WaitForSingleObject,
            },
        },
        UI::{
            Shell::{
                IsUserAnAdmin, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
                SHELLEXECUTEINFOW, ShellExecuteExW,
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
/// Exit code of a repair process that started without administrator rights, which
/// happens for standard users when UAC is turned off.
pub const REPAIR_ACCESS_DENIED: u32 = 5;
/// Apple's current USB driver: WinUSB with Apple's filter drivers. Current Apple Mobile
/// Device Support and Apple Devices place it only in the Windows driver store, as
/// `FileRepository\appleusb.inf_<arch>_<hash>`, with no copy under Common Files.
const STORE_DRIVER: &str = "appleusb.inf";
#[cfg(target_arch = "x86")]
const STORE_ARCH: &str = "x86";
#[cfg(target_arch = "aarch64")]
const STORE_ARCH: &str = "arm64";
#[cfg(not(any(target_arch = "x86", target_arch = "aarch64")))]
const STORE_ARCH: &str = "amd64";
/// The older iTunes-era driver, which Apple Mobile Device Support used to copy under
/// Common Files. Used only when the driver store has no `appleusb.inf`.
#[cfg(target_arch = "x86")]
const LEGACY_DRIVER_INF: &str = "Apple\\Mobile Device Support\\Drivers\\usbaapl.inf";
#[cfg(not(target_arch = "x86"))]
const LEGACY_DRIVER_INF: &str = "Apple\\Mobile Device Support\\Drivers\\usbaapl64.inf";

const SERVICE_NAME: PCWSTR = w!("Apple Mobile Device Service");
const SERVICE_KEY: PCWSTR = w!("SYSTEM\\CurrentControlSet\\Services\\Apple Mobile Device Service");
const USBMUXD_PORT: u16 = 27015;
const AF_INET: u32 = 2;
const STATE_TIMEOUT: Duration = Duration::from_secs(20);

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
    /// The Microsoft Store edition of Apple Devices is installed for this user. It has no
    /// Windows service: its AppleMobileDeviceProcess.exe, started by the app and its own
    /// sign-in task, provides the same port.
    pub store_app: bool,
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
    /// Only the Store edition of Apple Devices is installed and its device process is not
    /// running, typically right after installing it before it was ever opened.
    StoreAppStopped,
    /// Only the Store edition is installed; something listens but cannot list devices.
    StoreAppUnresponsive,
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
        // The Store edition has no service to start or repair: never send its users to
        // the service repair or to installing a second copy of the Apple components.
        if self.installed == Some(false) && self.store_app {
            return if self.listening {
                Diagnosis::StoreAppUnresponsive
            } else {
                Diagnosis::StoreAppStopped
            };
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
            "installed={:?} state={:?} start={:?} listening={} port_owner={} store_app={}",
            self.installed,
            self.state,
            self.start,
            self.listening,
            self.port_owner.as_deref().unwrap_or("-"),
            self.store_app
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
        store_app: apple_devices_app().is_some(),
    }
}

/// Per-user registry of installed packaged apps; each subkey is a package full name and
/// holds one subkey per application ID.
const PACKAGES_KEY: PCWSTR = w!(
    "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages"
);
const APPLE_DEVICES_PACKAGE: &str = "AppleInc.AppleDevices";

/// The application user model ID (`<family>!<app>`) of the Store edition of Apple
/// Devices when it is installed for this user. Read from the registry, without
/// PowerShell or WinRT.
pub fn apple_devices_app() -> Option<String> {
    let packages = RegKey::open(HKEY_CURRENT_USER, PACKAGES_KEY)?;
    let package = packages
        .subkeys()
        .into_iter()
        .find(|name| is_apple_devices_package(name))?;
    let family = package_family(&package)?;
    let wide: Vec<u16> = package.encode_utf16().chain(Some(0)).collect();
    let app = RegKey::open(packages.0, wide.as_ptr())?
        .subkeys()
        .into_iter()
        .next()?;
    Some(format!("{family}!{app}"))
}

fn is_apple_devices_package(full_name: &str) -> bool {
    full_name
        .split('_')
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case(APPLE_DEVICES_PACKAGE))
}

/// `Name_Version_Arch_ResourceId_PublisherId` to `Name_PublisherId`.
fn package_family(full_name: &str) -> Option<String> {
    let parts: Vec<&str> = full_name.split('_').collect();
    match parts.as_slice() {
        [name, _, _, _, publisher] if !name.is_empty() && !publisher.is_empty() => {
            Some(format!("{name}_{publisher}"))
        }
        _ => None,
    }
}

/// An open registry key, closed on drop.
struct RegKey(HKEY);

impl RegKey {
    fn open(root: HKEY, path: PCWSTR) -> Option<Self> {
        let mut key = null_mut();
        // SAFETY: The path is NUL-terminated and the output is writable.
        let status = unsafe { RegOpenKeyExW(root, path, 0, KEY_READ, &mut key) };
        (status == ERROR_SUCCESS).then_some(Self(key))
    }

    fn subkeys(&self) -> Vec<String> {
        let mut names = Vec::new();
        for index in 0.. {
            let mut name = [0u16; 256];
            let mut length = name.len() as u32;
            // SAFETY: The key is open and the buffer is writable for `length` characters.
            let status = unsafe {
                RegEnumKeyExW(
                    self.0,
                    index,
                    name.as_mut_ptr(),
                    &mut length,
                    null(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                )
            };
            match status {
                ERROR_SUCCESS => names.push(String::from_utf16_lossy(&name[..length as usize])),
                ERROR_MORE_DATA => continue,
                _ => break,
            }
        }
        names
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: The key was opened by RegOpenKeyExW and is closed exactly once.
        unsafe { RegCloseKey(self.0) };
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
    let waited = unsafe { WaitForSingleObject(info.hProcess, INFINITE) };
    let mut code = 1u32;
    // SAFETY: The handle is valid and the output points to initialized storage.
    let result = if waited == WAIT_OBJECT_0
        && unsafe { GetExitCodeProcess(info.hProcess, &mut code) } != 0
    {
        Ok(code)
    } else {
        Err(io::Error::last_os_error())
    };
    // SAFETY: See above.
    unsafe { CloseHandle(info.hProcess) };
    result
}

/// Body of the elevated process. Re-enables automatic start, then starts the service, or
/// restarts it when it runs but does not answer. Returns the process exit code:
/// 0 running, [`REPAIR_NOT_INSTALLED`] missing, 1 any other failure.
pub fn repair() -> u32 {
    if !is_admin() {
        return REPAIR_ACCESS_DENIED;
    }
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

fn is_admin() -> bool {
    // SAFETY: IsUserAnAdmin has no parameters or preconditions.
    unsafe { IsUserAnAdmin() != 0 }
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
    /// Attached and every device node that carries the Apple connection works.
    Working,
    /// Attached, but a node that carries the Apple connection reports a problem, usually
    /// a missing Apple driver. Reinstalling the driver can fix this.
    DriverProblem,
    /// Attached, but someone disabled the phone in Device Manager. Reinstalling a driver
    /// does not re-enable a device, so the user must enable it.
    Disabled,
    /// Windows could not list USB devices, so nothing is known about the phone.
    Unknown,
}

/// What [`apple_usb_presence`] learns about one present USB device node.
struct UsbNode {
    hardware_ids: String,
    compatible_ids: String,
    /// The node's problem code, `None` when it has no problem.
    problem: Option<u32>,
}

/// Looks for Apple mobile devices (vendor 05AC, product 12xx) among present USB device
/// nodes, including their interface nodes, which is where a missing Apple driver shows.
pub fn apple_usb_presence() -> UsbPresence {
    usb_nodes().map_or(UsbPresence::Unknown, |nodes| classify_usb(&nodes))
}

fn classify_usb(nodes: &[UsbNode]) -> UsbPresence {
    let mut apple: Vec<&UsbNode> = nodes
        .iter()
        .filter(|node| is_apple_mobile(&node.hardware_ids))
        .collect();
    if apple.is_empty() {
        return UsbPresence::Absent;
    }
    // A photo node establishes presence, but only connection nodes determine the repair.
    apple.retain(|node| !is_still_image(&node.compatible_ids));
    if apple
        .iter()
        .any(|node| node.problem == Some(CM_PROB_DISABLED))
    {
        return UsbPresence::Disabled;
    }
    if apple.iter().any(|node| node.problem.is_some()) {
        UsbPresence::DriverProblem
    } else {
        UsbPresence::Working
    }
}

fn is_still_image(compatible_ids: &str) -> bool {
    compatible_ids
        .to_ascii_uppercase()
        .contains(r"USB\CLASS_06")
}

/// Present device nodes under the USB enumerator, or `None` when Windows cannot list them.
fn usb_nodes() -> Option<Vec<UsbNode>> {
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
            return None;
        }
        let mut nodes = Vec::new();
        let mut index = 0;
        loop {
            let mut data: SP_DEVINFO_DATA = std::mem::zeroed();
            data.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiEnumDeviceInfo(set, index, &mut data) == 0 {
                break;
            }
            index += 1;
            let Some(hardware_ids) = device_property(set, &data, SPDRP_HARDWAREID) else {
                continue;
            };
            let compatible_ids =
                device_property(set, &data, SPDRP_COMPATIBLEIDS).unwrap_or_default();
            let (mut status, mut code) = (0, 0);
            let problem = (CM_Get_DevNode_Status(&mut status, &mut code, data.DevInst, 0)
                == CR_SUCCESS
                && status & DN_HAS_PROBLEM != 0)
                .then_some(code);
            nodes.push(UsbNode {
                hardware_ids,
                compatible_ids,
                problem,
            });
        }
        SetupDiDestroyDeviceInfoList(set);
        Some(nodes)
    }
}

/// A multi-string device property, with its entries joined by NUL characters.
///
/// # Safety
/// `set` must be an open device information set and `data` one of its elements.
unsafe fn device_property(set: HDEVINFO, data: &SP_DEVINFO_DATA, property: u32) -> Option<String> {
    let mut ids = [0u16; 512];
    // SAFETY: The caller guarantees the set and element; the buffer is writable for its size.
    let read = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set,
            data,
            property,
            null_mut(),
            ids.as_mut_ptr().cast(),
            size_of_val(&ids) as u32,
            null_mut(),
        )
    };
    (read != 0).then(|| String::from_utf16_lossy(&ids))
}

fn is_apple_mobile(hardware_ids: &str) -> bool {
    hardware_ids
        .to_ascii_uppercase()
        .contains("VID_05AC&PID_12")
}

/// Body of the elevated driver repair. Reinstalls Apple's USB driver on matching devices,
/// then asks Windows to re-scan. Returns 0 on success, [`REPAIR_NOT_INSTALLED`] when no
/// Apple USB driver package exists on this PC, 1 otherwise.
pub fn repair_driver() -> u32 {
    if !is_admin() {
        return REPAIR_ACCESS_DENIED;
    }
    let Some(inf) = apple_driver_inf() else {
        return REPAIR_NOT_INSTALLED;
    };
    // Use the system copy, never a pnputil found through PATH.
    let pnputil = system_root().join("System32").join("pnputil.exe");
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

fn system_root() -> PathBuf {
    PathBuf::from(env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
}

/// The newest `appleusb.inf` in the driver store, or the legacy `usbaapl` driver that
/// older Apple Mobile Device Support copied under Common Files.
fn apple_driver_inf() -> Option<PathBuf> {
    let repository = system_root()
        .join("System32")
        .join("DriverStore")
        .join("FileRepository");
    let current = fs::read_dir(repository)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_str().is_some_and(is_store_package))
        .map(|entry| entry.path().join(STORE_DRIVER))
        .filter(|inf| inf.is_file())
        .max_by_key(|inf| {
            fs::read(inf)
                .ok()
                .and_then(|bytes| driver_version(&decode_inf(&bytes)))
        });
    current.or_else(|| {
        env::var_os("CommonProgramW6432")
            .or_else(|| env::var_os("CommonProgramFiles"))
            .map(|common| PathBuf::from(common).join(LEGACY_DRIVER_INF))
            .filter(|inf| inf.is_file())
    })
}

/// Whether a driver store folder holds `appleusb.inf` for this build's architecture.
fn is_store_package(folder: &str) -> bool {
    folder
        .to_ascii_lowercase()
        .starts_with(&format!("{STORE_DRIVER}_{STORE_ARCH}_"))
}

/// INF files are ANSI or UTF-16 LE with a byte order mark.
fn decode_inf(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => {
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The version from `DriverVer = mm/dd/yyyy,a.b.c.d`, for picking the newest package.
fn driver_version(inf: &str) -> Option<[u32; 4]> {
    let line = inf.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("DriverVer")
            .then_some(value)
    })?;
    let version = line.split(';').next()?.split(',').nth(1)?.trim();
    let mut parts = [0u32; 4];
    for (slot, part) in parts.iter_mut().zip(version.split('.')) {
        *slot = part.trim().parse().ok()?;
    }
    Some(parts)
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
            store_app: false,
        }
    }

    #[test]
    fn the_store_edition_is_never_sent_to_service_repair_or_install() {
        let mut store = report(Some(false), None, None);
        store.store_app = true;
        assert_eq!(store.diagnose(false), Diagnosis::StoreAppStopped);
        store.listening = true;
        store.port_owner = Some("AppleMobileDeviceProcess.exe".into());
        assert_eq!(store.diagnose(false), Diagnosis::Listening);
        assert_eq!(store.diagnose(true), Diagnosis::StoreAppUnresponsive);
        // Another program on the port is still named, whichever edition is installed.
        store.port_owner = Some("srcds.exe".into());
        assert_eq!(store.diagnose(false), Diagnosis::PortTaken);
        // With the classic service installed too, its state is what can be repaired.
        let mut both = report(
            Some(true),
            Some(RunState::Stopped),
            Some(StartType::Disabled),
        );
        both.store_app = true;
        assert_eq!(both.diagnose(false), Diagnosis::Disabled);
    }

    #[test]
    fn derives_the_apple_devices_app_id() {
        let full = "AppleInc.AppleDevices_1.1792.39681.0_x64__nzyj5cx40ttqa";
        assert!(is_apple_devices_package(full));
        assert!(is_apple_devices_package(&full.to_ascii_lowercase()));
        assert!(!is_apple_devices_package(
            "AppleInc.iTunes_12.13.0.0_x64__nzyj5cx40ttqa"
        ));
        assert!(!is_apple_devices_package(
            "AppleInc.AppleDevicesBeta_1.0.0.0_x64__abc"
        ));
        assert_eq!(
            package_family(full).as_deref(),
            Some("AppleInc.AppleDevices_nzyj5cx40ttqa")
        );
        assert_eq!(package_family("AppleInc.AppleDevices"), None);
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
    fn finds_the_store_driver_for_this_architecture() {
        let folder = format!("appleusb.inf_{STORE_ARCH}_58854158183af679");
        assert!(is_store_package(&folder));
        assert!(is_store_package(&folder.to_ascii_uppercase()));
        assert!(!is_store_package("netaapl64.inf_amd64_56f23639c9617984"));
        assert!(!is_store_package("appleusb.inf"));
        let other = if STORE_ARCH == "amd64" {
            "arm64"
        } else {
            "amd64"
        };
        assert!(!is_store_package(&format!("appleusb.inf_{other}_0123")));
    }

    fn node(hardware: &str, compatible: &str, problem: Option<u32>) -> UsbNode {
        UsbNode {
            hardware_ids: hardware.into(),
            compatible_ids: compatible.into(),
            problem,
        }
    }

    #[test]
    fn classifies_what_windows_sees_on_usb() {
        const COMPOSITE: &str = "USB\\VID_05AC&PID_12A8&REV_1702";
        const PHOTOS: &str = "USB\\VID_05AC&PID_12A8&MI_00";
        const STILL: &str = "USB\\Class_06&SubClass_01&Prot_01\0USB\\Class_06";
        const MUX: &str = "USB\\VID_05AC&PID_12A8&MI_01";
        const VENDOR: &str = "USB\\Class_ff&SubClass_fe&Prot_02";
        let keyboard = node("USB\\VID_05AC&PID_024F", "USB\\Class_03", Some(28));
        assert_eq!(classify_usb(&[]), UsbPresence::Absent);
        assert_eq!(classify_usb(&[keyboard]), UsbPresence::Absent);
        let working = [
            node(COMPOSITE, "", None),
            node(PHOTOS, STILL, None),
            node(MUX, VENDOR, None),
        ];
        assert_eq!(classify_usb(&working), UsbPresence::Working);
        // A broken photo import interface is Windows' MTP driver, not Apple's.
        let photos = [node(PHOTOS, STILL, Some(10)), node(MUX, VENDOR, None)];
        assert_eq!(classify_usb(&photos), UsbPresence::Working);
        for (mux_problem, expected) in [
            (None, UsbPresence::Working),
            (Some(28), UsbPresence::DriverProblem),
        ] {
            let nodes = [
                node(PHOTOS, STILL, Some(CM_PROB_DISABLED)),
                node(MUX, VENDOR, mux_problem),
            ];
            assert_eq!(classify_usb(&nodes), expected);
        }
        let missing = [node(PHOTOS, STILL, None), node(MUX, VENDOR, Some(28))];
        assert_eq!(classify_usb(&missing), UsbPresence::DriverProblem);
        let disabled = [
            node(COMPOSITE, "", Some(CM_PROB_DISABLED)),
            node(MUX, VENDOR, Some(28)),
        ];
        assert_eq!(classify_usb(&disabled), UsbPresence::Disabled);
    }

    /// Needs a PC with Apple Mobile Device Support installed.
    #[test]
    #[ignore]
    fn finds_the_installed_apple_driver() {
        let inf = apple_driver_inf().expect("no Apple USB driver package found");
        println!("{}", inf.display());
        assert!(inf.is_file());
    }

    #[test]
    fn reads_driver_versions_from_ansi_and_utf16_infs() {
        let inf = "[Version]\r\nClass=USBDevice\r\nDriverVer = 06/14/2023,538.0.0.0 ; build\r\n";
        assert_eq!(driver_version(inf), Some([538, 0, 0, 0]));
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend(inf.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(driver_version(&decode_inf(&utf16)), Some([538, 0, 0, 0]));
        assert!(driver_version("DriverVer=06/14/2023,538.0.0.10").unwrap() > [538, 0, 0, 9]);
        assert_eq!(driver_version("Class=USBDevice"), None);
        assert_eq!(driver_version("DriverVer=06/14/2023,x.0"), None);
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
