//! Apple device support on Windows. Installation and repair run only after the user
//! activates their button; diagnosis is read-only.

use std::{
    io,
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
    ptr::{null, null_mut},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use copiago_core::{
    diagnostic,
    i18n::{Message, message},
};
use copiago_ui::settings_page::DriverState;
use windows_sys::{
    Win32::{
        Foundation::ERROR_SUCCESS,
        System::{
            Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW},
            Threading::CREATE_NO_WINDOW,
        },
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    },
    core::PCWSTR,
    w,
};

use crate::service::{self, Diagnosis, REPAIR_NOT_INSTALLED, UsbPresence};

/// Raw OS error when the user answers "No" in the UAC prompt.
const ERROR_CANCELLED: i32 = 1223;
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
/// Covers the UAC prompt, stopping and starting the service, and the port coming up.
const REPAIR_TIMEOUT: Duration = Duration::from_secs(120);

/// Quick state for the pairing guide. Prefer [`diagnose`] when a precise reason is shown.
pub(super) fn detect() -> DriverState {
    driver_state(service::inspect().diagnose(false))
}

/// Result of [`diagnose`].
pub(super) struct ServiceCheck {
    pub state: DriverState,
    /// Precise reason and next step; `None` when nothing looks wrong.
    pub notice: Option<Message>,
    /// Starting or restarting the service is likely to fix it. A port held by another
    /// program or a missing installation needs a different action first.
    pub repairable: bool,
}

/// Explains why the Apple service cannot be used and logs the facts behind it.
/// `enumeration_failed` is true when the worker reached the port but could not list
/// devices.
pub(super) fn diagnose(enumeration_failed: bool) -> ServiceCheck {
    let report = service::inspect();
    let diagnosis = report.diagnose(enumeration_failed);
    diagnostic::event(format_args!(
        "event=service_check enumeration_failed={enumeration_failed} diagnosis={diagnosis:?} {}",
        report.log_fields()
    ));
    let notice = match diagnosis {
        Diagnosis::Listening => None,
        Diagnosis::NotInstalled => Some(message!(
            "未找到 Apple 设备连接组件。点「安装连接组件」即可自动安装。"
        )),
        Diagnosis::Disabled => Some(message!(
            "Apple Mobile Device Service 已被禁用，常见于系统优化软件。点「修复连接服务」并在 Windows 弹窗中点「是」，CopiaGo 会把它改回自动启动并启动它。"
        )),
        Diagnosis::Stopped => Some(message!(
            "Apple Mobile Device Service 没有运行。点「修复连接服务」并在 Windows 弹窗中点「是」即可启动。"
        )),
        Diagnosis::PortTaken => Some(message!(
            "端口 27015 被「{program}」占用，Apple 连接服务无法工作。请退出该程序（或在任务管理器中结束它），然后点「修复连接服务」。",
            program = report.foreign_owner().unwrap_or_default().to_owned(),
        )),
        Diagnosis::Unresponsive => Some(message!(
            "Apple Mobile Device Service 没有响应。点「修复连接服务」并在 Windows 弹窗中点「是」，CopiaGo 会重启它；仍不行请重启电脑。"
        )),
    };
    ServiceCheck {
        state: driver_state(diagnosis),
        notice,
        repairable: matches!(
            diagnosis,
            Diagnosis::Disabled | Diagnosis::Stopped | Diagnosis::Unresponsive
        ),
    }
}

fn driver_state(diagnosis: Diagnosis) -> DriverState {
    match diagnosis {
        Diagnosis::Listening => DriverState::Ready,
        Diagnosis::NotInstalled => DriverState::Missing,
        Diagnosis::Disabled
        | Diagnosis::Stopped
        | Diagnosis::PortTaken
        | Diagnosis::Unresponsive => DriverState::ServiceUnavailable,
    }
}

/// Fixed-format Windows version fields for the diagnostic log and reports.
pub(super) fn system_summary() -> String {
    const KEY: PCWSTR = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let build = read_string(KEY, w!("CurrentBuild")).unwrap_or_else(|| "?".into());
    let revision = read_dword(KEY, w!("UBR")).map_or_else(|| "?".into(), |ubr| ubr.to_string());
    let release = read_string(KEY, w!("DisplayVersion")).unwrap_or_else(|| "?".into());
    let arch = std::env::var("PROCESSOR_ARCHITECTURE").unwrap_or_else(|_| "?".into());
    format!("os_build={build}.{revision} os_release={release} arch={arch}")
}

fn read_string(key: PCWSTR, value: PCWSTR) -> Option<String> {
    let mut buffer = [0u16; 128];
    let mut bytes = size_of_val(&buffer) as u32;
    // SAFETY: Key and value are static NUL-terminated strings; the buffer is writable for
    // `bytes` bytes and RegGetValueW terminates the string it writes.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key,
            value,
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buffer[..length]))
}

fn read_dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut bytes = size_of::<u32>() as u32;
    // SAFETY: Key and value are static NUL-terminated strings; data and bytes are writable.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key,
            value,
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut data as *mut u32).cast(),
            &mut bytes,
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}

pub(super) fn open_store() -> io::Result<()> {
    open_url("ms-windows-store://search/?query=Apple%20Devices")
}

/// Opens a fixed or percent-encoded URL with the default handler.
pub(super) fn open_url(url: &str) -> io::Result<()> {
    let url: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    // SAFETY: Both strings are NUL-terminated and live through the call. Other arguments
    // are optional.
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            w!("open"),
            url.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result > 32 {
        Ok(())
    } else {
        Err(io::Error::other(format!("ShellExecuteW returned {result}")))
    }
}

/// What a [`DriverInstall`] does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Job {
    /// Install Apple Mobile Device Support through winget.
    Install,
    /// Start or restart Apple Mobile Device Service in an elevated copy of CopiaGo.
    RepairService,
    /// Reinstall Apple's bundled USB driver in an elevated copy of CopiaGo.
    RepairDriver,
}

impl Job {
    /// The job that fixes what the guide currently shows.
    pub fn for_state(state: DriverState) -> Self {
        match state {
            DriverState::ServiceUnavailable => Self::RepairService,
            DriverState::UsbDriver => Self::RepairDriver,
            _ => Self::Install,
        }
    }
}

pub(super) struct DriverInstall {
    job: Job,
    process: Process,
    next_poll: Instant,
    timeout: Instant,
    exited: bool,
}

impl DriverInstall {
    /// Runs a user-requested install, or an elevated repair on a background thread.
    /// A successful exit is followed by a separate check that the fix took effect.
    pub fn start(now: Instant, job: Job) -> io::Result<Self> {
        diagnostic::event(format_args!("event=driver_job_started job={job:?}"));
        let (process, timeout) = match job {
            Job::RepairService => (
                Process::Repair(Some(service::spawn_repair(service::REPAIR_FLAG))),
                REPAIR_TIMEOUT,
            ),
            Job::RepairDriver => (
                Process::Repair(Some(service::spawn_repair(service::DRIVER_REPAIR_FLAG))),
                REPAIR_TIMEOUT,
            ),
            Job::Install => {
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
                (Process::Install(child), INSTALL_TIMEOUT)
            }
        };
        Ok(Self {
            job,
            process,
            next_poll: now + Duration::from_millis(300),
            timeout: now + timeout,
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
        if !self.exited {
            match self.process.finish(self.job) {
                Finish::Running if now < self.timeout => return None,
                Finish::Running => return Some(self.failed()),
                Finish::Succeeded => self.exited = true,
                Finish::Failed(result) => return Some(result),
                Finish::Unsuccessful => return Some(self.failed()),
            }
        }
        // A successful process does not prove the fix took effect: the port may still be
        // coming up, and Windows may still be binding the driver.
        let fixed = match self.job {
            Job::RepairDriver => service::apple_usb_presence() != UsbPresence::DriverProblem,
            Job::Install | Job::RepairService => detect() == DriverState::Ready,
        };
        if fixed {
            diagnostic::event(format_args!("event=driver_ready job={:?}", self.job));
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
        let state = diagnose(false).state;
        diagnostic::event(format_args!(
            "event=driver_failed job={:?} state={state:?}",
            self.job
        ));
        match self.job {
            Job::RepairDriver => (
                DriverState::UsbDriver,
                message!(
                    "Apple USB 驱动仍未正常工作。请重新插拔数据线后再点「修复 Apple 驱动」；仍不行请重启电脑，或点「在 Microsoft Store 中查找」重新安装 Apple Devices。"
                ),
            ),
            Job::RepairService => (
                DriverState::ServiceUnavailable,
                message!(
                    "Apple 连接服务仍不可用。请重启电脑后再试；仍不行请在 Microsoft Store 中修复或重新安装 Apple Devices。"
                ),
            ),
            Job::Install if state == DriverState::ServiceUnavailable => (
                DriverState::ServiceUnavailable,
                message!(
                    "Apple 连接服务仍不可用。请重启电脑后再试；仍不行请在 Microsoft Store 中修复或重新安装 Apple Devices。"
                ),
            ),
            Job::Install => (
                DriverState::Failed,
                message!("安装未完成。可以在 Microsoft Store 安装「Apple Devices」后重试。"),
            ),
        }
    }
}

enum Finish {
    Running,
    Succeeded,
    Unsuccessful,
    Failed((DriverState, Message)),
}

enum Process {
    Install(Child),
    /// Taken when joined. Dropping an unjoined handle detaches the thread, so closing
    /// the app never interrupts a service or driver operation in progress.
    Repair(Option<JoinHandle<io::Result<u32>>>),
}

impl Process {
    fn finish(&mut self, job: Job) -> Finish {
        match self {
            Self::Install(child) => match child.try_wait() {
                Ok(None) => Finish::Running,
                Ok(Some(status)) if status.success() => Finish::Succeeded,
                Ok(Some(_)) | Err(_) => Finish::Unsuccessful,
            },
            Self::Repair(handle) => {
                if handle.as_ref().is_some_and(|thread| !thread.is_finished()) {
                    return Finish::Running;
                }
                let result = handle.take().map(JoinHandle::join);
                diagnostic::event(format_args!(
                    "event=driver_job_finished job={job:?} result={result:?}"
                ));
                match result {
                    Some(Ok(Ok(0))) => Finish::Succeeded,
                    Some(Ok(Ok(REPAIR_NOT_INSTALLED))) => Finish::Failed((
                        DriverState::Missing,
                        if job == Job::RepairDriver {
                            message!(
                                "没有找到 Apple 自带的 USB 驱动文件。请点「安装连接组件」重新安装 Apple Mobile Device Support。"
                            )
                        } else {
                            message!(
                                "未找到 Apple Mobile Device Service。请点「安装连接组件」重新安装。"
                            )
                        },
                    )),
                    Some(Ok(Err(error))) if error.raw_os_error() == Some(ERROR_CANCELLED) => {
                        Finish::Failed(if job == Job::RepairDriver {
                            (
                                DriverState::UsbDriver,
                                message!(
                                    "已取消管理员授权，未修复 Apple USB 驱动。需要时请再点「修复 Apple 驱动」，并在 Windows 弹窗中点「是」。"
                                ),
                            )
                        } else {
                            (
                                DriverState::ServiceUnavailable,
                                message!(
                                    "已取消管理员授权，未修复 Apple 连接服务。需要时请再点「修复连接服务」，并在 Windows 弹窗中点「是」。"
                                ),
                            )
                        })
                    }
                    _ => Finish::Unsuccessful,
                }
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Self::Install(child) = self
            && !matches!(child.try_wait(), Ok(Some(_)))
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Explains why "find a USB iPhone" failed from what Windows itself sees on USB.
/// Returns the driver state to show and a precise next step.
pub(super) fn explain_missing_iphone() -> (DriverState, Message) {
    let presence = service::apple_usb_presence();
    diagnostic::event(format_args!("event=usb_presence state={presence:?}"));
    match presence {
        UsbPresence::Absent => (
            DriverState::Ready,
            message!(
                "电脑没有检测到 iPhone。请确认数据线能传输数据（有些线只能充电），把手机直接插在电脑机身的 USB 口并解锁，然后点「连接 iPhone」。"
            ),
        ),
        UsbPresence::DriverProblem => (
            DriverState::UsbDriver,
            message!(
                "电脑检测到了 iPhone，但 Apple 的 USB 驱动没有正常工作。点「修复 Apple 驱动」并在 Windows 弹窗中点「是」即可自动修复。"
            ),
        ),
        UsbPresence::Working => (
            DriverState::Ready,
            message!(
                "电脑检测到了 iPhone，但还不能和它通信。请解锁 iPhone 并停留在主屏幕，手机上出现「要信任此电脑吗」时点「信任」，然后点「连接 iPhone」。"
            ),
        ),
    }
}

pub(super) fn failed_to_run() -> (DriverState, Message) {
    (
        DriverState::Failed,
        message!("无法运行 Windows 包管理器。可以在 Microsoft Store 安装「Apple Devices」后重试。"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_listener_counts_as_ready() {
        assert_eq!(driver_state(Diagnosis::Listening), DriverState::Ready);
        assert_eq!(driver_state(Diagnosis::NotInstalled), DriverState::Missing);
        for diagnosis in [
            Diagnosis::Disabled,
            Diagnosis::Stopped,
            Diagnosis::PortTaken,
            Diagnosis::Unresponsive,
        ] {
            assert_eq!(driver_state(diagnosis), DriverState::ServiceUnavailable);
        }
    }
}
