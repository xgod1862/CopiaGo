//! Login-session single instance and activation compatible with the Python entry point.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::null;

use windows_sys::Win32::{
    Foundation::{
        ERROR_ALREADY_EXISTS, GetLastError, HANDLE, SetLastError, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    System::Threading::{CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject},
    UI::WindowsAndMessaging::{AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId},
};
use windows_sys::core::PCWSTR;
use windows_sys::w;

use crate::tray::CLASS;

const MUTEX_NAME: PCWSTR = w!("Local\\iPhoneClipboardPrototype");
const EVENT_NAME: PCWSTR = w!("Local\\iPhoneClipboardShow");

/// Keep this guard alive for the lifetime of the primary instance.
pub struct Instance {
    _mutex: OwnedHandle,
    show: OwnedHandle,
    namespace: Option<Vec<u16>>,
}

impl Instance {
    /// Acquires the same objects as the Python release. Call before preparing the data directory.
    ///
    /// `Ok(None)` means another instance is already running. Unless `minimized` is true,
    /// the other instance has been signalled to open its panel.
    pub fn acquire(minimized: bool) -> io::Result<Option<Self>> {
        Self::acquire_named(None, EVENT_NAME, minimized)
    }

    /// Use an explicit namespace for test hosts that must coexist with a production instance.
    /// Both test launches must also use the same, separate test data directory.
    pub fn acquire_isolated(namespace: &str, minimized: bool) -> io::Result<Option<Self>> {
        if namespace.is_empty()
            || namespace.len() > 64
            || !namespace
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid single-instance test namespace",
            ));
        }
        let mutex = format!("Local\\CopiaGo.Test.{namespace}.Instance");
        let show = format!("Local\\CopiaGo.Test.{namespace}.Show");
        let mutex: Vec<_> = mutex.encode_utf16().chain([0]).collect();
        let show: Vec<_> = show.encode_utf16().chain([0]).collect();
        Self::acquire_named(Some(mutex), show.as_ptr(), minimized)
    }

    fn acquire_named(
        namespace: Option<Vec<u16>>,
        event_name: PCWSTR,
        minimized: bool,
    ) -> io::Result<Option<Self>> {
        // SAFETY: The name is NUL-terminated. Default security and auto-reset match Python.
        let show = unsafe { CreateEventW(null(), 0, 0, event_name) };
        if show.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateEventW returned a new owned handle.
        let show = unsafe { OwnedHandle::from_raw_handle(show) };

        // Create the event first so a second process can signal before the primary is ready.
        let mutex_name = namespace.as_ref().map_or(MUTEX_NAME, |name| name.as_ptr());
        // SAFETY: The valid name is NUL-terminated. Read the error before another Win32 call.
        let (mutex, error) = unsafe {
            SetLastError(0);
            let mutex = CreateMutexW(null(), 0, mutex_name);
            (mutex, GetLastError())
        };
        if mutex.is_null() {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        // SAFETY: CreateMutexW returned a new owned handle.
        let mutex = unsafe { OwnedHandle::from_raw_handle(mutex) };
        if error == ERROR_ALREADY_EXISTS {
            if !minimized {
                offer_foreground_permission(mutex_name);
                // SAFETY: The handle identifies the activation event opened above.
                if unsafe { SetEvent(show.as_raw_handle()) } == 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            return Ok(None);
        }

        Ok(Some(Self {
            _mutex: mutex,
            show,
            namespace,
        }))
    }

    /// Borrowed auto-reset event handle for `MsgWaitForMultipleObjectsEx` in the UI loop.
    /// A successful wait consumes the signal. Open the panel directly when the wait returns it.
    pub fn activation_handle(&self) -> HANDLE {
        self.show.as_raw_handle()
    }

    /// Checks for and consumes a queued activation without blocking.
    pub fn take_activation(&self) -> io::Result<bool> {
        // SAFETY: The valid handle stays open during this nonblocking wait.
        match unsafe { WaitForSingleObject(self.activation_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }

    pub(crate) fn window_name(&self) -> PCWSTR {
        self.namespace
            .as_ref()
            .map_or(MUTEX_NAME, |name| name.as_ptr())
    }

    pub(crate) fn isolated(&self) -> bool {
        self.namespace.is_some()
    }
}

fn offer_foreground_permission(name: PCWSTR) {
    let mut pid = 0;
    // SAFETY: The class and name are both NUL-terminated and pid is writable.
    unsafe {
        let window = FindWindowW(CLASS, name);
        if !window.is_null() && GetWindowThreadProcessId(window, &mut pid) != 0 {
            AllowSetForegroundWindow(pid);
        }
    }
}

#[cfg(test)]
mod tests;
