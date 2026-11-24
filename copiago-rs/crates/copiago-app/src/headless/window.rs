//! Win32 notifications and the hidden window owned by the main thread.

use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut, with_exposed_provenance_mut};
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
    NetworkManagement::IpHelper::{
        CancelMibChangeNotify2, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE, MibInitialNotification,
        NotifyIpInterfaceChange,
    },
    Networking::WinSock::AF_UNSPEC,
    System::{
        Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler},
        LibraryLoader::GetModuleHandleW,
        Power::{
            HPOWERNOTIFY, RegisterSuspendResumeNotification, UnregisterSuspendResumeNotification,
        },
    },
    UI::WindowsAndMessaging::{
        CREATESTRUCTW, CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DestroyWindow,
        DispatchMessageW, GetMessageW, KillTimer, MSG, PBT_APMRESUMEAUTOMATIC, PostMessageW,
        PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage, UnregisterClassW, WM_APP,
        WM_CLOSE, WM_POWERBROADCAST, WM_TIMER, WNDCLASSW, WS_POPUP,
    },
};
use windows_sys::{core::PCWSTR, w};

use copiago_win::clipboard::WM_CLIPBOARDUPDATE;

const CLASS: PCWSTR = w!("CopiaGo.Headless");
const WM_APP_DEVICE: u32 = WM_APP + 1;
const WM_APP_RESUME: u32 = WM_APP + 2;
const WM_APP_NETWORK: u32 = WM_APP + 3;
const WM_APP_WORKER_EXIT: u32 = WM_APP + 4;
const WM_APP_FILE_ANSWER: u32 = WM_APP + 5;
const NETWORK_TIMER: usize = 1;
const NETWORK_SETTLE_MS: u32 = 300;

pub(super) enum WindowEvent {
    Clipboard,
    Device,
    Resume,
    NetworkChanged,
    NetworkSettled,
    WorkerExited,
    FileAnswer,
}

pub(super) struct Window {
    handle: HWND,
    instance: HINSTANCE,
}

impl Window {
    pub(super) fn new() -> io::Result<Self> {
        // SAFETY: A null module name requests the current executable's instance.
        let instance = unsafe { GetModuleHandleW(null()) };
        if instance.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: All unset WNDCLASSW fields accept zero.
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: CLASS,
            ..unsafe { std::mem::zeroed() }
        };
        // SAFETY: The class's name and procedure remain valid until it is unregistered.
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: The registered class and module belong to this thread's process.
        // WS_POPUP without WS_VISIBLE creates a hidden top-level window.
        let handle = unsafe {
            CreateWindowExW(
                0,
                CLASS,
                CLASS,
                WS_POPUP,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                null::<CREATESTRUCTW>().cast(),
            )
        };
        let window = Self { handle, instance };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(window)
    }

    pub(super) fn handle(&self) -> HWND {
        self.handle
    }

    pub(super) fn waker(&self) -> WindowWaker {
        WindowWaker(self.handle.expose_provenance())
    }

    pub(super) fn next_event(&self) -> io::Result<Option<WindowEvent>> {
        // SAFETY: All fields can be initialized to zero before GetMessageW sets them.
        let mut message: MSG = unsafe { std::mem::zeroed() };
        loop {
            // SAFETY: message is writable and a null HWND selects this thread's whole queue.
            match unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } {
                -1 => return Err(io::Error::last_os_error()),
                0 => return Ok(None),
                _ => {}
            }
            if message.hwnd == self.handle {
                let event = match message.message {
                    WM_CLIPBOARDUPDATE => Some(WindowEvent::Clipboard),
                    WM_APP_DEVICE => Some(WindowEvent::Device),
                    WM_APP_RESUME => Some(WindowEvent::Resume),
                    WM_APP_NETWORK => Some(WindowEvent::NetworkChanged),
                    WM_TIMER if message.wParam == NETWORK_TIMER => {
                        Some(WindowEvent::NetworkSettled)
                    }
                    WM_APP_WORKER_EXIT => Some(WindowEvent::WorkerExited),
                    WM_APP_FILE_ANSWER => Some(WindowEvent::FileAnswer),
                    _ => None,
                };
                if event.is_some() {
                    return Ok(event);
                }
            }
            // SAFETY: This is a message from the calling thread's queue. The window
            // procedure does not borrow any Rust state from the dispatch loop.
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    pub(super) fn debounce_network_change(&self) -> bool {
        // SAFETY: On the owning thread. Reusing the timer ID restarts the delay.
        unsafe { SetTimer(self.handle, NETWORK_TIMER, NETWORK_SETTLE_MS, None) != 0 }
    }

    pub(super) fn cancel_network_timer(&self) -> bool {
        // SAFETY: The timer belongs to this window on the current thread.
        unsafe { KillTimer(self.handle, NETWORK_TIMER) != 0 }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: The window and class are owned by this thread. Listeners and
        // the device worker are dropped before the window.
        unsafe {
            if !self.handle.is_null() {
                DestroyWindow(self.handle);
            }
            UnregisterClassW(CLASS, self.instance);
        }
    }
}

unsafe extern "system" fn window_proc(
    handle: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_POWERBROADCAST if wparam == PBT_APMRESUMEAUTOMATIC as WPARAM => {
            post(handle, WM_APP_RESUME);
            1
        }
        WM_CLOSE => {
            // SAFETY: The procedure runs on the window's owning thread and
            // requests an orderly exit before the window is destroyed.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: The parameters came from Windows for this window procedure.
        _ => unsafe { DefWindowProcW(handle, message, wparam, lparam) },
    }
}

fn post(handle: HWND, message: u32) -> bool {
    // SAFETY: The HWND is an opaque identifier. These messages have no borrowed data.
    unsafe { PostMessageW(handle, message, 0, 0) != 0 }
}

/// The device thread may post wakeups while the main thread owns the window.
pub(super) struct WindowWaker(usize);

impl WindowWaker {
    pub(super) fn device_event(&self) -> bool {
        post(with_exposed_provenance_mut(self.0), WM_APP_DEVICE)
    }

    pub(super) fn worker_exited(&self) {
        post(with_exposed_provenance_mut(self.0), WM_APP_WORKER_EXIT);
    }

    pub(super) fn file_answer(&self) -> bool {
        post(with_exposed_provenance_mut(self.0), WM_APP_FILE_ANSWER)
    }
}

pub(super) struct PowerNotifications(HPOWERNOTIFY);

impl PowerNotifications {
    pub(super) fn register(window: &Window) -> io::Result<Self> {
        // SAFETY: The recipient is a live HWND, as required by the flag.
        let handle = unsafe {
            RegisterSuspendResumeNotification(window.handle, DEVICE_NOTIFY_WINDOW_HANDLE)
        };
        if handle == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }
}

impl Drop for PowerNotifications {
    fn drop(&mut self) {
        // SAFETY: The handle is from a successful registration and is cancelled once.
        unsafe { UnregisterSuspendResumeNotification(self.0) };
    }
}

pub(super) struct NetworkNotifications(HANDLE);

impl NetworkNotifications {
    pub(super) fn register(window: &Window) -> io::Result<Self> {
        let mut handle = null_mut();
        // SAFETY: The context is the live window's opaque identifier. The
        // callback never dereferences it and cancellation precedes window destruction.
        let status = unsafe {
            NotifyIpInterfaceChange(
                AF_UNSPEC,
                Some(network_changed),
                window.handle.cast(),
                0,
                &mut handle,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(Self(handle))
    }
}

impl Drop for NetworkNotifications {
    fn drop(&mut self) {
        // SAFETY: The handle belongs to this registration. Cancellation is
        // performed off the notification callback to avoid deadlock.
        unsafe { CancelMibChangeNotify2(self.0) };
    }
}

unsafe extern "system" fn network_changed(
    context: *const c_void,
    _row: *const MIB_IPINTERFACE_ROW,
    notification: MIB_NOTIFICATION_TYPE,
) {
    if notification != MibInitialNotification {
        post(context.cast_mut().cast(), WM_APP_NETWORK);
    }
}

// The console API has no caller context. Share only the Win32 identifier.
static CONSOLE_RECIPIENT: AtomicUsize = AtomicUsize::new(0);

pub(super) struct ConsoleControl;

impl ConsoleControl {
    pub(super) fn register(window: &Window) -> io::Result<Self> {
        CONSOLE_RECIPIENT.store(window.handle.expose_provenance(), Ordering::Release);
        // SAFETY: The handler has the documented ABI and posts a data-free message.
        if unsafe { SetConsoleCtrlHandler(Some(console_control), 1) } == 0 {
            CONSOLE_RECIPIENT.store(0, Ordering::Release);
            return Err(io::Error::last_os_error());
        }
        Ok(Self)
    }
}

impl Drop for ConsoleControl {
    fn drop(&mut self) {
        CONSOLE_RECIPIENT.store(0, Ordering::Release);
        // SAFETY: Remove the same function registered by this object.
        unsafe { SetConsoleCtrlHandler(Some(console_control), 0) };
    }
}

unsafe extern "system" fn console_control(event: u32) -> i32 {
    let handle = CONSOLE_RECIPIENT.load(Ordering::Acquire);
    i32::from(
        matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT)
            && handle != 0
            && post(with_exposed_provenance_mut(handle), WM_CLOSE),
    )
}
