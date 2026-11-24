//! The notification-area icon and its UI-thread window.

use std::cell::{Cell, RefCell};
use std::io;
use std::ptr::{null, null_mut};
use std::rc::Rc;
use std::sync::OnceLock;

use windows_sys::Win32::{
    Foundation::{
        GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SetLastError, WPARAM,
    },
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{
        HiDpi::GetDpiForWindow,
        Shell::{
            NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
            NIM_SETFOCUS, NIM_SETVERSION, NIN_SELECT, NINF_KEY, NOTIFY_ICON_MESSAGE,
            NOTIFYICON_VERSION_4, NOTIFYICONDATAW, NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect,
            Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
            GetCursorPos, GetWindowLongPtrW, GetWindowThreadProcessId, KillTimer, PostMessageW,
            RegisterClassW, RegisterWindowMessageW, SWP_NOACTIVATE, SWP_NOZORDER, SetTimer,
            SetWindowLongPtrW, SetWindowPos, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DISPLAYCHANGE,
            WM_DPICHANGED, WM_LBUTTONUP, WM_NCCREATE, WM_NCDESTROY, WM_RBUTTONUP, WM_SETTINGCHANGE,
            WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
        },
    },
};
use windows_sys::{core::PCWSTR, w};

use crate::instance::Instance;

mod icon;
mod menu;
mod promotion;
#[cfg(test)]
mod tests;

use icon::Icon;
use menu::Menu;
pub use menu::{TrayAction, TrayMenuLabels};
use promotion::Promotion;

pub(crate) const CLASS: PCWSTR = w!("CopiaGo.Tray");
const ICON_ID: u32 = 1;
const WM_CALLBACK: u32 = WM_APP + 1;
const WM_RESTORE: u32 = WM_APP + 2;
const WM_ICON_DPI: u32 = WM_APP + 3;
const WM_RELOCATE: u32 = WM_APP + 4;
const RETRY_TIMER: usize = 1;
const PROMOTION_TIMER: usize = 2;
const RETRY_INTERVAL_MS: u32 = 1_500;
const MAX_RETRIES: u8 = 10;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayIconState {
    Disconnected,
    Connected,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationKind {
    Standard,
    /// First-run connection guidance is shown even when regular notifications are disabled.
    Onboarding,
}

/// Lives on the thread that dispatches the host's Win32 messages. Drop it before the receiver.
pub struct Tray {
    window: HWND,
    state: Rc<State>,
}

impl Tray {
    /// Creates the tray after acquiring the primary instance. Keep that guard alive while the
    /// tray is in use. Commands are posted to `receiver`
    /// as `action_message` (an app-defined message), with a [TrayAction] in `wParam`.
    #[expect(
        clippy::not_unsafe_ptr_arg_deref,
        reason = "GetWindowThreadProcessId takes HWND as an opaque identifier"
    )]
    pub fn new(
        instance: &Instance,
        receiver: HWND,
        action_message: u32,
        labels: TrayMenuLabels<'_>,
    ) -> io::Result<Self> {
        // SAFETY: A window handle is an opaque identifier; no process ID output is requested.
        let on_this_thread =
            unsafe { GetWindowThreadProcessId(receiver, null_mut()) == GetCurrentThreadId() };
        if !on_this_thread || !(WM_APP..0xc000).contains(&action_message) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid tray command receiver",
            ));
        }
        // SAFETY: Both the module lookup and the static message name are valid.
        let module = unsafe { GetModuleHandleW(null()) };
        if module.is_null() {
            return Err(io::Error::last_os_error());
        }
        register_class(module)?;
        // SAFETY: The message name is a NUL-terminated literal.
        let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        if taskbar_created == 0 {
            return Err(io::Error::last_os_error());
        }

        let state = Rc::into_raw(Rc::new(State {
            taskbar_created,
            receiver,
            action_message,
            labels: RefCell::new(labels.into()),
            has_latest: Cell::new(false),
            menu_open: Cell::new(false),
            notifications: Cell::new(true),
            tooltip: Cell::new(fit_utf16("CopiaGo")),
            icon: Cell::new(TrayIconState::Disconnected),
            dpi: Cell::new(96),
            callbacks: Cell::new(None),
            retries: Cell::new(0),
            promotion_attempts: Cell::new(0),
            closing: Cell::new(false),
        }));
        // SAFETY: The class and title are valid and state has a strong count during creation.
        // A hidden top-level window can receive shell broadcasts.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS,
                instance.window_name(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                module,
                state.cast(),
            )
        };
        // SAFETY: Reclaim the strong count from into_raw. The tray keeps it through DestroyWindow.
        let state = unsafe { Rc::from_raw(state) };
        if window.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: The window was just created on this thread.
        let dpi = unsafe { GetDpiForWindow(window) };
        if dpi != 0 {
            state.dpi.set(dpi);
        }
        state.restore(window);
        if !instance.isolated() && set_timer(window, PROMOTION_TIMER) {
            state.promotion_attempts.set(MAX_RETRIES);
        }
        Ok(Self { window, state })
    }

    /// The hidden top-level window that owns the icon. Keep Win32 access on the UI thread.
    pub fn hwnd(&self) -> HWND {
        self.window
    }

    /// The icon's screen rectangle for positioning the popup. The shell can have no rectangle
    /// yet while the icon is being added or its overflow window is closed.
    pub fn icon_rect(&self) -> io::Result<RECT> {
        icon_rect(self.window)
    }

    pub fn set_icon_state(&self, icon: TrayIconState) {
        if self.state.icon.replace(icon) != icon {
            self.state.modify_icon(self.window);
        }
    }

    pub fn set_tooltip(&self, text: &str) {
        let tooltip = fit_utf16(text);
        if self.state.tooltip.replace(tooltip) != tooltip {
            let mut data = icon_data(self.window);
            data.uFlags = NIF_TIP | NIF_SHOWTIP;
            data.szTip = tooltip;
            self.state.modify(self.window, &data);
        }
    }

    pub fn set_menu_labels(&self, labels: TrayMenuLabels<'_>) {
        self.state.labels.replace(labels.into());
    }

    /// Shows the pending iPhone clipboard action in the next menu when an item is available.
    pub fn set_has_latest(&self, available: bool) {
        self.state.has_latest.set(available);
    }

    pub fn set_notifications_enabled(&self, enabled: bool) {
        self.state.notifications.set(enabled);
    }

    /// Requests a system balloon, returning whether the shell accepted it. The host controls
    /// when to show a notification and when to suppress duplicates or use the popup instead.
    pub fn notify(&self, message: &str, kind: NotificationKind) -> bool {
        if self.state.callbacks.get().is_none()
            || (!self.state.notifications.get() && kind == NotificationKind::Standard)
        {
            return false;
        }
        let mut data = icon_data(self.window);
        data.uFlags = NIF_INFO;
        data.szInfo = fit_utf16(message);
        data.szInfoTitle = fit_utf16("CopiaGo");
        data.szInfo[0] != 0 && shell_notify(NIM_MODIFY, &data)
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.state.closing.set(true);
        stop_timer(self.window, RETRY_TIMER);
        stop_timer(self.window, PROMOTION_TIMER);
        self.state.delete_icon(self.window);
        // SAFETY: The window was created on this thread and state remains live during teardown.
        unsafe { DestroyWindow(self.window) };
    }
}

#[derive(Clone, Copy)]
enum CallbackFormat {
    Legacy,
    Version4,
}

struct State {
    taskbar_created: u32,
    receiver: HWND,
    action_message: u32,
    labels: RefCell<menu::Labels>,
    has_latest: Cell<bool>,
    menu_open: Cell<bool>,
    notifications: Cell<bool>,
    tooltip: Cell<[u16; 128]>,
    icon: Cell<TrayIconState>,
    dpi: Cell<u32>,
    callbacks: Cell<Option<CallbackFormat>>,
    retries: Cell<u8>,
    promotion_attempts: Cell<u8>,
    closing: Cell<bool>,
}

impl State {
    fn add_icon(&self, window: HWND) -> bool {
        let Ok(icon) = Icon::load(self.icon.get(), self.dpi.get()) else {
            return false;
        };
        let mut data = icon_data(window);
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = WM_CALLBACK;
        data.hIcon = icon.0;
        data.szTip = self.tooltip.get();

        // A DPI change can also broadcast TaskbarCreated while retaining the icon.
        let had_icon = self.callbacks.take().is_some();
        if !(shell_notify(NIM_ADD, &data) || (had_icon && shell_notify(NIM_MODIFY, &data))) {
            return false;
        }
        let mut version = icon_data(window);
        version.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let format = if shell_notify(NIM_SETVERSION, &version) {
            CallbackFormat::Version4
        } else {
            CallbackFormat::Legacy
        };
        self.callbacks.set(Some(format));
        self.relocate(window);
        true
    }

    fn delete_icon(&self, window: HWND) {
        self.callbacks.set(None);
        shell_notify(NIM_DELETE, &icon_data(window));
    }

    fn modify_icon(&self, window: HWND) {
        if self.callbacks.get().is_some()
            && let Ok(icon) = Icon::load(self.icon.get(), self.dpi.get())
        {
            let mut data = icon_data(window);
            data.uFlags = NIF_ICON;
            data.hIcon = icon.0;
            self.modify(window, &data);
        }
    }

    fn modify(&self, window: HWND, data: &NOTIFYICONDATAW) {
        if self.callbacks.get().is_some() && !shell_notify(NIM_MODIFY, data) {
            self.restore(window);
        }
    }

    fn restore(&self, window: HWND) {
        stop_timer(window, RETRY_TIMER);
        self.retries.set(0);
        if !self.add_icon(window) && set_timer(window, RETRY_TIMER) {
            self.retries.set(MAX_RETRIES);
        }
    }

    fn retry(&self, window: HWND) {
        let remaining = self.retries.get();
        if remaining != 0 {
            if self.add_icon(window) || remaining == 1 {
                self.retries.set(0);
                stop_timer(window, RETRY_TIMER);
            } else {
                self.retries.set(remaining - 1);
            }
        }
    }

    fn promote(&self, window: HWND) {
        let remaining = self.promotion_attempts.get();
        if remaining == 0 {
            return;
        }
        match promotion::try_promote() {
            Promotion::Pending if remaining > 1 => self.promotion_attempts.set(remaining - 1),
            result => {
                self.promotion_attempts.set(0);
                stop_timer(window, PROMOTION_TIMER);
                if result == Promotion::Promoted {
                    self.delete_icon(window);
                    self.restore(window);
                }
            }
        }
    }

    fn relocate(&self, window: HWND) {
        if let Ok(rect) = icon_rect(window)
            && rect.left < rect.right
            && rect.top < rect.bottom
        {
            // SAFETY: This is the live hidden window. Placing it by the icon lets Windows send
            // WM_DPICHANGED for the same monitor. No activation or size change is needed.
            unsafe {
                SetWindowPos(
                    window,
                    null_mut(),
                    rect.left,
                    rect.top,
                    1,
                    1,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                )
            };
        }
    }

    fn post_action(&self, action: TrayAction) {
        post(self.receiver, self.action_message, action as WPARAM);
    }

    fn callback(&self, window: HWND, wparam: WPARAM, lparam: LPARAM) {
        let Some(format) = self.callbacks.get() else {
            return;
        };
        let input = match format {
            CallbackFormat::Version4 if (lparam as u32 >> 16) & 0xffff == ICON_ID => {
                lparam as u16 as u32
            }
            CallbackFormat::Legacy if wparam == ICON_ID as usize => lparam as u32,
            _ => return,
        };
        match (format, input) {
            (CallbackFormat::Version4, NIN_SELECT | NIN_KEYSELECT)
            | (CallbackFormat::Legacy, WM_LBUTTONUP) => {
                post(window, WM_RELOCATE, 0);
                self.post_action(TrayAction::OpenHistory);
            }
            (CallbackFormat::Version4, WM_CONTEXTMENU) => {
                if let Some(point) = context_point(window, wparam) {
                    self.show_menu(window, point);
                }
            }
            (CallbackFormat::Legacy, WM_RBUTTONUP) => {
                if let Some(point) = cursor_point() {
                    self.show_menu(window, point);
                }
            }
            _ => {}
        }
    }

    fn show_menu(&self, window: HWND, point: POINT) {
        if self.menu_open.replace(true) {
            return;
        }
        let labels = self.labels.borrow().clone();
        let menu = Menu::new(&labels, self.has_latest.get());
        let action = menu.ok().and_then(|menu| menu.track(window, point));
        self.menu_open.set(false);
        if self.closing.get() {
            return;
        }
        match action {
            Some(action) => self.post_action(action),
            None => {
                shell_notify(NIM_SETFOCUS, &icon_data(window));
            }
        }
    }
}

fn register_class(module: HINSTANCE) -> io::Result<()> {
    static CLASS_RESULT: OnceLock<Result<(), u32>> = OnceLock::new();
    let result = CLASS_RESULT.get_or_init(|| {
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module,
            lpszClassName: CLASS,
            // SAFETY: All other fields are nullable pointers or accept zero values.
            ..unsafe { std::mem::zeroed() }
        };
        // SAFETY: The class and procedure have process lifetime.
        unsafe {
            if RegisterClassW(&class) == 0 {
                Err(GetLastError())
            } else {
                Ok(())
            }
        }
    });
    result.map_err(|code| io::Error::from_raw_os_error(code as i32))
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: WM_NCCREATE supplies a CREATESTRUCTW; creation passed our shared state.
        return unsafe {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetLastError(0);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
            isize::from(GetLastError() == 0)
        };
    }
    // SAFETY: The HWND belongs to this procedure, which alone writes its user data.
    let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const State;
    if !state.is_null() {
        // SAFETY: The pointer was passed from Rc::into_raw and the tray holds a strong count
        // until WM_NCDESTROY clears the pointer. Hold another count across nested message loops.
        let state = unsafe {
            Rc::increment_strong_count(state);
            Rc::from_raw(state)
        };
        if message == WM_NCDESTROY {
            // SAFETY: This window's state pointer is no longer needed after teardown.
            unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, 0) };
            // SAFETY: Forward the original message parameters for standard cleanup.
            return unsafe { DefWindowProcW(window, message, wparam, lparam) };
        }
        if state.closing.get() {
            // SAFETY: Preserve default message handling during teardown.
            return unsafe { DefWindowProcW(window, message, wparam, lparam) };
        }
        if message == state.taskbar_created {
            // A broadcast can be sent during a shell call. Restore once that call has returned.
            post(window, WM_RESTORE, 0);
            return 0;
        }
        match message {
            WM_CALLBACK => state.callback(window, wparam, lparam),
            WM_RESTORE => state.restore(window),
            WM_TIMER if wparam == RETRY_TIMER => state.retry(window),
            WM_TIMER if wparam == PROMOTION_TIMER => state.promote(window),
            WM_DPICHANGED => {
                // The window is hidden and only its monitor matters. Use the supplied DPI.
                post(window, WM_ICON_DPI, wparam & 0xffff);
            }
            WM_ICON_DPI if wparam != 0 => {
                let dpi = wparam as u32;
                if state.dpi.replace(dpi) != dpi {
                    state.modify_icon(window);
                }
            }
            WM_DISPLAYCHANGE | WM_SETTINGCHANGE => post(window, WM_RELOCATE, 0),
            WM_RELOCATE => state.relocate(window),
            WM_CLOSE => state.post_action(TrayAction::Exit),
            // SAFETY: Forward messages outside our tray protocol with their original parameters.
            _ => return unsafe { DefWindowProcW(window, message, wparam, lparam) },
        }
        return 0;
    }
    // SAFETY: Windows supplied the parameters for this window procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn icon_data(window: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: ICON_ID,
        // SAFETY: All unset fields are integers, arrays, or nullable handles.
        ..unsafe { std::mem::zeroed() }
    }
}

fn shell_notify(message: NOTIFY_ICON_MESSAGE, data: &NOTIFYICONDATAW) -> bool {
    #[cfg(test)]
    if let Some(result) = tests::mock_notify(message, data) {
        return result;
    }
    // SAFETY: data is initialized for the operation and lives through the synchronous call.
    unsafe { Shell_NotifyIconW(message, data) != 0 }
}

fn icon_rect(window: HWND) -> io::Result<RECT> {
    let identifier = NOTIFYICONIDENTIFIER {
        cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: window,
        uID: ICON_ID,
        guidItem: windows_sys::core::GUID::from_u128(0),
    };
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: identifier is initialized and rect is writable.
    let result = unsafe { Shell_NotifyIconGetRect(&identifier, &mut rect) };
    if result < 0 {
        return Err(io::Error::other(format!(
            "Shell_NotifyIconGetRect: {result:#010x}"
        )));
    }
    Ok(rect)
}

fn context_point(window: HWND, wparam: WPARAM) -> Option<POINT> {
    if wparam as u32 == u32::MAX {
        return icon_rect(window).ok().map_or_else(cursor_point, |rect| {
            Some(POINT {
                x: rect.left,
                y: rect.bottom,
            })
        });
    }
    Some(POINT {
        x: i32::from(wparam as i16),
        y: i32::from((wparam >> 16) as i16),
    })
}

fn cursor_point() -> Option<POINT> {
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: point is writable.
    (unsafe { GetCursorPos(&mut point) } != 0).then_some(point)
}

fn set_timer(window: HWND, id: usize) -> bool {
    // SAFETY: The caller owns this UI-thread window and uses a timer ID reserved by it.
    unsafe { SetTimer(window, id, RETRY_INTERVAL_MS, None) != 0 }
}

fn stop_timer(window: HWND, id: usize) {
    // SAFETY: The timer belongs to this window. Cancelling an absent timer is harmless.
    unsafe { KillTimer(window, id) };
}

fn post(window: HWND, message: u32, wparam: WPARAM) {
    // SAFETY: Only opaque identifiers and value data are passed.
    unsafe { PostMessageW(window, message, wparam, 0) };
}

fn fit_utf16<const N: usize>(text: &str) -> [u16; N] {
    let mut result = [0; N];
    let mut end = 0;
    for ch in text.chars().take_while(|&ch| ch != '\0') {
        let next = end + ch.len_utf16();
        if next >= N {
            break;
        }
        ch.encode_utf16(&mut result[end..next]);
        end = next;
    }
    result
}
