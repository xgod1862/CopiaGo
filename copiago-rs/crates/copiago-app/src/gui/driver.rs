//! Apple device support on Windows. Installation runs only after activating its button.

use std::{
    io,
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
    ptr::null_mut,
    time::{Duration, Instant},
};

use copiago_core::i18n::{Message, message};
use copiago_ui::settings_page::DriverState;
use windows_sys::{
    Win32::{
        Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{
            Registry::{HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, RegCloseKey, RegOpenKeyExW},
            SystemInformation::GetSystemDirectoryW,
            Threading::{CREATE_NO_WINDOW, GetExitCodeProcess, WaitForSingleObject},
        },
        UI::{
            Shell::{
                SEE_MASK_NOCLOSEPROCESS, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
                ShellExecuteExW, ShellExecuteW,
            },
            WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL},
        },
    },
    w,
};

pub(super) fn detect() -> DriverState {
    classify(registered(), reachable())
}

fn classify(registered: bool, reachable: bool) -> DriverState {
    if reachable {
        DriverState::Ready
    } else if registered {
        DriverState::ServiceUnavailable
    } else {
        DriverState::Missing
    }
}

fn registered() -> bool {
    let mut key = null_mut();
    // SAFETY: The registry location is a static, terminated string and the handle output is writable.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!("SYSTEM\\CurrentControlSet\\Services\\Apple Mobile Device Service"),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if status == ERROR_SUCCESS {
        // SAFETY: RegOpenKeyExW returned a newly opened handle.
        unsafe { RegCloseKey(key) };
        return true;
    }
    false
}

fn reachable() -> bool {
    // A registered service can be stopped. A TCP listener is necessary, but device
    // enumeration in the worker remains the final protocol-level check.
    TcpStream::connect_timeout(
        &SocketAddrV4::new(Ipv4Addr::LOCALHOST, 27015).into(),
        Duration::from_millis(300),
    )
    .is_ok()
}

pub(super) fn open_store() -> io::Result<()> {
    // SAFETY: This is a fixed, NUL-terminated Store URI. The unused arguments are optional.
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            w!("open"),
            w!("ms-windows-store://search/?query=Apple%20Devices"),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result > 32 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result as i32))
    }
}

pub(super) struct DriverInstall {
    process: Process,
    next_poll: Instant,
    timeout: Instant,
    exited: bool,
    repair: bool,
}

impl DriverInstall {
    /// Runs a user-requested install or an elevated repair of the Apple service.
    /// A successful process exit is followed by a separate reachability check.
    pub fn start(now: Instant, repair: bool) -> io::Result<Self> {
        if repair {
            return Ok(Self {
                process: Process::Repair(start_service_repair()?),
                next_poll: Instant::now(),
                timeout: Instant::now() + Duration::from_secs(60),
                exited: false,
                repair: true,
            });
        }
        let child = Command::new("winget.exe")
            .args([
                "install",
                "--id",
                "Apple.AppleMobileDeviceSupport",
                "--exact",
                "--source",
                "winget",
                "--silent",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--disable-interactivity",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        Ok(Self {
            process: Process::Install(child),
            next_poll: now + Duration::from_millis(300),
            timeout: now + Duration::from_secs(300),
            exited: false,
            repair: false,
        })
    }

    pub fn deadline(&self) -> Instant {
        self.next_poll
    }

    pub fn poll(&mut self, now: Instant) -> Option<(DriverState, Message)> {
        if now < self.next_poll {
            return None;
        }
        self.next_poll = now + Duration::from_millis(300);
        if !self.exited {
            match self.process.try_wait() {
                Ok(Some(true)) => self.exited = true,
                Ok(Some(false)) | Err(_) => return Some(self.failed()),
                Ok(None) if now < self.timeout => return None,
                Ok(None) => return Some(self.failed()),
            }
        }
        // Neither a successful installer nor a registry entry proves the service
        // is listening. Allow startup to finish before reporting success.
        if detect() == DriverState::Ready {
            Some((
                DriverState::Ready,
                message!("连接组件已就绪，可以连接 iPhone。"),
            ))
        } else if now < self.timeout {
            None
        } else {
            Some(self.failed())
        }
    }

    fn failed(&self) -> (DriverState, Message) {
        if self.repair || registered() {
            (
                DriverState::ServiceUnavailable,
                message!("Apple 连接服务仍不可用。请重新启动电脑，或在 Microsoft Store 中修复或重新安装 Apple Devices。"),
            )
        } else {
            (
                DriverState::Failed,
                message!("安装未完成。可以在 Microsoft Store 安装「Apple Devices」后重试。"),
            )
        }
    }
}

enum Process {
    Install(Child),
    Repair(HANDLE),
}

impl Process {
    fn try_wait(&mut self) -> io::Result<Option<bool>> {
        match self {
            Self::Install(child) => child
                .try_wait()
                .map(|status| status.map(|status| status.success())),
            Self::Repair(handle) => {
                // SAFETY: The owned process handle is kept open until Process is dropped.
                match unsafe { WaitForSingleObject(*handle, 0) } {
                    WAIT_TIMEOUT => Ok(None),
                    WAIT_OBJECT_0 => {
                        let mut code = 0;
                        // SAFETY: The handle is valid and the output points to initialized storage.
                        if unsafe { GetExitCodeProcess(*handle, &mut code) } == 0 {
                            Err(io::Error::last_os_error())
                        } else {
                            Ok(Some(code == 0))
                        }
                    }
                    _ => Err(io::Error::last_os_error()),
                }
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        match self {
            Self::Install(child) => {
                if !matches!(child.try_wait(), Ok(Some(_))) {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            Self::Repair(handle) => {
                // Do not terminate a service operation if the app closes during repair.
                // SAFETY: ShellExecuteExW transferred ownership of this handle to us.
                unsafe { CloseHandle(*handle) };
            }
        }
    }
}

fn start_service_repair() -> io::Result<HANDLE> {
    let mut directory = [0u16; 32768];
    // SAFETY: The supplied UTF-16 buffer is writable for its stated length.
    let length =
        unsafe { GetSystemDirectoryW(directory.as_mut_ptr(), directory.len() as u32) } as usize;
    if length == 0 || length >= directory.len() {
        return Err(io::Error::last_os_error());
    }
    // Use the system executable, never a PowerShell found through PATH. No user
    // input enters the command; elevation applies only to this fixed service.
    let executable: Vec<u16> = directory[..length]
        .iter()
        .copied()
        .chain("\\WindowsPowerShell\\v1.0\\powershell.exe".encode_utf16())
        .chain(Some(0))
        .collect();
    let parameters: Vec<u16> = concat!(
        "-NoProfile -NonInteractive -Command \"try { ",
        "$ErrorActionPreference = 'Stop'; ",
        "$s = Get-Service -Name 'Apple Mobile Device Service'; ",
        "if ($s.Status -eq 'Running') { Restart-Service -Name 'Apple Mobile Device Service' } ",
        "else { Start-Service -Name 'Apple Mobile Device Service' }; ",
        "(Get-Service -Name 'Apple Mobile Device Service').WaitForStatus('Running', [TimeSpan]::FromSeconds(15)); ",
        "exit 0 } catch { exit 1 }\""
    )
    .encode_utf16()
    .chain(Some(0))
        .collect();
    // SAFETY: All-zero is a valid initial state for this size-tagged Win32 structure.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = w!("runas");
    info.lpFile = executable.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: All strings remain NUL-terminated and alive throughout the call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.hProcess.is_null() {
        return Err(io::Error::other("service repair returned no process handle"));
    }
    Ok(info.hProcess)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_but_stopped_service_is_not_ready() {
        assert_eq!(classify(true, false), DriverState::ServiceUnavailable);
    }

    #[test]
    fn reachable_service_does_not_require_legacy_registry_entry() {
        assert_eq!(classify(false, true), DriverState::Ready);
        assert_eq!(classify(true, true), DriverState::Ready);
        assert_eq!(classify(false, false), DriverState::Missing);
    }
}

pub(super) fn failed_to_run() -> (DriverState, Message) {
    (
        DriverState::Failed,
        message!("无法运行 Windows 包管理器。可以在 Microsoft Store 安装「Apple Devices」后重试。"),
    )
}
