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
        Foundation::ERROR_SUCCESS,
        System::{
            Registry::{HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, RegCloseKey, RegOpenKeyExW},
            Threading::CREATE_NO_WINDOW,
        },
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    },
    w,
};

pub(super) fn detect() -> DriverState {
    if installed() {
        DriverState::Ready
    } else {
        DriverState::Missing
    }
}

fn installed() -> bool {
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
    child: Child,
    next_poll: Instant,
    timeout: Instant,
    exited: bool,
}

impl DriverInstall {
    pub fn start(now: Instant) -> io::Result<Self> {
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
            child,
            next_poll: now + Duration::from_millis(300),
            timeout: now + Duration::from_secs(300),
            exited: false,
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
        let success = match self.child.try_wait() {
            Ok(Some(status)) => {
                self.exited = true;
                status.success() || installed()
            }
            Ok(None) if now < self.timeout => return None,
            Ok(None) | Err(_) => return Some(failed_to_run()),
        };
        Some(if success {
            (
                DriverState::Ready,
                message!("连接组件已就绪，可以连接 iPhone。"),
            )
        } else {
            (
                DriverState::Failed,
                message!("安装未完成。可以在 Microsoft Store 安装「Apple Devices」后重试。"),
            )
        })
    }
}

impl Drop for DriverInstall {
    fn drop(&mut self) {
        if !self.exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub(super) fn failed_to_run() -> (DriverState, Message) {
    (
        DriverState::Failed,
        message!("无法运行 Windows 包管理器。可以在 Microsoft Store 安装「Apple Devices」后重试。"),
    )
}
