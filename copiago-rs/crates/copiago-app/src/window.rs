//! Win32 notifications and the hidden window owned by the main thread.

use std::{cell::RefCell, collections::VecDeque, time::Instant};

use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut, with_exposed_provenance_mut};
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::{
    Foundation::{
        ERROR_SUCCESS, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WAIT_FAILED, WAIT_OBJECT_0,
        WAIT_TIMEOUT, WPARAM,
    },
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
        DispatchMessageW, GWLP_USERDATA, GetWindowLongPtrW, KillTimer, MSG, MWMO_INPUTAVAILABLE,
        MsgWaitForMultipleObjectsEx, PBT_APMRESUMEAUTOMATIC, PM_REMOVE, PeekMessageW, PostMessageW,
        QS_ALLINPUT, RegisterClassW, SetTimer, SetWindowLongPtrW, TranslateMessage,
        UnregisterClassW, WM_APP, WM_CLOSE, WM_NCCREATE, WM_NULL, WM_POWERBROADCAST, WM_QUIT,
        WM_TIMER, WNDCLASSW, WS_POPUP,
    },
};
use windows_sys::{core::PCWSTR, w};

use copiago_win::{clipboard::WM_CLIPBOARDUPDATE, tray::TrayAction};

const HEADLESS_CLASS: PCWSTR = w!("CopiaGo.Headless");
const GUI_CLASS: PCWSTR = w!("CopiaGo.Host");
const WM_APP_DEVICE: u32 = WM_APP + 1;
const WM_APP_RESUME: u32 = WM_APP + 2;
const WM_APP_NETWORK: u32 = WM_APP + 3;
const WM_APP_WORKER_EXIT: u32 = WM_APP + 4;
const WM_APP_FILE_ANSWER: u32 = WM_APP + 5;
pub(super) const WM_APP_TRAY: u32 = WM_APP + 6;
const WM_APP_UPDATE: u32 = WM_APP + 7;
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
    Tray(TrayAction),
    Update,
    Close,
}

pub(super) enum DesktopMessage {
    Native(MSG),
    Activate,
    Timeout,
    Quit,
}

type WindowData = RefCell<VecDeque<WindowEvent>>;

pub(super) struct Window {
    class: PCWSTR,
    events: Box<WindowData>,
    handle: HWND,
    instance: HINSTANCE,
}

impl Window {
    pub(super) fn new() -> io::Result<Self> {
        Self::create(HEADLESS_CLASS)
    }

    pub(super) fn new_gui() -> io::Result<Self> {
        Self::create(GUI_CLASS)
    }

    fn create(name: PCWSTR) -> io::Result<Self> {
        // SAFETY: A null module name requests the current executable's instance.
        let instance = unsafe { GetModuleHandleW(null()) };
        if instance.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: All unset WNDCLASSW fields accept zero.
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: name,
            ..unsafe { std::mem::zeroed() }
        };
        // SAFETY: The class's name and procedure remain valid until it is unregistered.
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let events: Box<WindowData> = Box::default();
        // SAFETY: The registered class and module belong to this thread's process.
        // WS_POPUP without WS_VISIBLE creates a hidden top-level window.
        let handle = unsafe {
            CreateWindowExW(
                0,
                name,
                name,
                WS_POPUP,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                std::ptr::from_ref(events.as_ref()).cast(),
            )
        };
        let window = Self {
            handle,
            instance,
            class: name,
            events,
        };
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

    pub(super) fn take_event(&self) -> Option<WindowEvent> {
        self.events.borrow_mut().pop_front()
    }

    pub(super) fn next_event(&self) -> io::Result<Option<WindowEvent>> {
        loop {
            match self.take_event() {
                Some(WindowEvent::Close) => return Ok(None),
                Some(event) => return Ok(Some(event)),
                None => {}
            }
            match next_message(None, None)? {
                DesktopMessage::Native(message) => dispatch(&message),
                DesktopMessage::Quit => return Ok(None),
                DesktopMessage::Activate | DesktopMessage::Timeout => unreachable!(),
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
            UnregisterClassW(self.class, self.instance);
        }
    }
}

unsafe extern "system" fn window_proc(
    handle: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: WM_NCCREATE supplies a live CREATESTRUCTW and the application
        // passed the window's boxed event queue as lpCreateParams.
        unsafe {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(handle, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }
    let event = match message {
        WM_POWERBROADCAST if wparam == PBT_APMRESUMEAUTOMATIC as WPARAM => {
            post(handle, WM_APP_RESUME);
            return 1;
        }
        WM_CLOSE => Some(WindowEvent::Close),
        WM_CLIPBOARDUPDATE => Some(WindowEvent::Clipboard),
        WM_APP_DEVICE => Some(WindowEvent::Device),
        WM_APP_RESUME => Some(WindowEvent::Resume),
        WM_APP_NETWORK => Some(WindowEvent::NetworkChanged),
        WM_TIMER if wparam == NETWORK_TIMER => Some(WindowEvent::NetworkSettled),
        WM_APP_WORKER_EXIT => Some(WindowEvent::WorkerExited),
        WM_APP_FILE_ANSWER => Some(WindowEvent::FileAnswer),
        WM_APP_TRAY => TrayAction::from_wparam(wparam).map(WindowEvent::Tray),
        WM_APP_UPDATE => Some(WindowEvent::Update),
        _ => None,
    };
    if let Some(event) = event {
        // SAFETY: The owning thread holds the boxed queue through window destruction.
        // Its borrow ends before returning to the dispatcher.
        let data =
            unsafe { (GetWindowLongPtrW(handle, GWLP_USERDATA) as *const WindowData).as_ref() };
        if let Some(data) = data {
            data.borrow_mut().push_back(event);
            if message == WM_CLOSE {
                // A sent close can be dispatched inside PeekMessageW. Leave a posted
                // message so the outer loop observes the queued close without waiting.
                post(handle, WM_NULL);
            }
        }
        return 0;
    }
    // SAFETY: The parameters came from Windows for this window procedure.
    unsafe { DefWindowProcW(handle, message, wparam, lparam) }
}

pub(super) fn next_message(
    activation: Option<HANDLE>,
    deadline: Option<Instant>,
) -> io::Result<DesktopMessage> {
    let handles = activation.as_slice();
    loop {
        // SAFETY: The current thread owns the queue and the buffer is writable.
        let mut message: MSG = unsafe { std::mem::zeroed() };
        if unsafe_peek(&mut message) {
            return Ok(if message.message == WM_QUIT {
                DesktopMessage::Quit
            } else {
                DesktopMessage::Native(message)
            });
        }
        let timeout = deadline.map_or(u32::MAX, |deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            u32::try_from(remaining.as_nanos().div_ceil(1_000_000))
                .unwrap_or(u32::MAX - 1)
                .min(u32::MAX - 1)
        });
        // SAFETY: The optional borrowed event is owned by the current instance,
        // which outlives this call. A zero handle count uses a null pointer.
        let result = unsafe {
            MsgWaitForMultipleObjectsEx(
                handles.len() as u32,
                if handles.is_empty() {
                    null()
                } else {
                    handles.as_ptr()
                },
                timeout,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        match result {
            WAIT_FAILED => return Err(io::Error::last_os_error()),
            WAIT_TIMEOUT => return Ok(DesktopMessage::Timeout),
            WAIT_OBJECT_0 if activation.is_some() => return Ok(DesktopMessage::Activate),
            _ => {}
        }
    }
}

fn unsafe_peek(message: &mut MSG) -> bool {
    // SAFETY: The current thread owns the queue. A null HWND includes thread messages.
    unsafe { PeekMessageW(message, null_mut(), 0, 0, PM_REMOVE) != 0 }
}

pub(super) fn dispatch(message: &MSG) {
    // SAFETY: The message was removed from the calling thread's queue.
    unsafe {
        TranslateMessage(message);
        DispatchMessageW(message);
    }
}

fn post(handle: HWND, message: u32) -> bool {
    // SAFETY: The HWND is an opaque identifier. These messages have no borrowed data.
    unsafe { PostMessageW(handle, message, 0, 0) != 0 }
}

/// The device thread may post wakeups while the main thread owns the window.
#[derive(Clone, Copy)]
pub(super) struct WindowWaker(usize);

impl WindowWaker {
    pub(super) fn device_event(&self) -> bool {
        post(with_exposed_provenance_mut(self.0), WM_APP_DEVICE)
    }

    pub(super) fn worker_exited(&self) {
        post(with_exposed_provenance_mut(self.0), WM_APP_WORKER_EXIT);
    }

    pub(super) fn update(&self) {
        post(with_exposed_provenance_mut(self.0), WM_APP_UPDATE);
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
