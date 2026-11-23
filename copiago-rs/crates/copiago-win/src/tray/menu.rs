use std::io;
use std::ptr::null;

use windows_sys::Win32::{
    Foundation::{HWND, POINT, WPARAM},
    UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, GetSystemMetrics, HMENU, MF_STRING,
        PostMessageW, SM_MENUDROPALIGNMENT, SetForegroundWindow, SetMenuDefaultItem, TPM_LEFTALIGN,
        TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_NULL,
    },
};

/// Commands posted to the host with the message supplied to [super::Tray::new].
#[repr(usize)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayAction {
    OpenHistory = 1,
    UseLatest,
    OpenSettings,
    Exit,
}

impl TrayAction {
    pub fn from_wparam(value: WPARAM) -> Option<Self> {
        match value {
            1 => Some(Self::OpenHistory),
            2 => Some(Self::UseLatest),
            3 => Some(Self::OpenSettings),
            4 => Some(Self::Exit),
            _ => None,
        }
    }
}

/// Localized menu strings supplied by the host's current language.
pub struct TrayMenuLabels<'a> {
    pub history: &'a str,
    pub use_latest: &'a str,
    pub settings: &'a str,
    pub exit: &'a str,
}

#[derive(Clone)]
pub(super) struct Labels {
    history: Vec<u16>,
    use_latest: Vec<u16>,
    settings: Vec<u16>,
    exit: Vec<u16>,
}

impl From<TrayMenuLabels<'_>> for Labels {
    fn from(labels: TrayMenuLabels<'_>) -> Self {
        fn wide(text: &str) -> Vec<u16> {
            text.encode_utf16().chain([0]).collect()
        }
        Self {
            history: wide(labels.history),
            use_latest: wide(labels.use_latest),
            settings: wide(labels.settings),
            exit: wide(labels.exit),
        }
    }
}

pub(super) struct Menu(pub(super) HMENU);

impl Menu {
    pub(super) fn new(labels: &Labels, has_latest: bool) -> io::Result<Self> {
        // SAFETY: CreatePopupMenu creates a fresh handle and takes no arguments.
        let menu = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            return Err(io::Error::last_os_error());
        }
        let menu = Self(menu);
        menu.append(TrayAction::OpenHistory, &labels.history)?;
        if has_latest {
            menu.append(TrayAction::UseLatest, &labels.use_latest)?;
        }
        menu.append(TrayAction::OpenSettings, &labels.settings)?;
        menu.append(TrayAction::Exit, &labels.exit)?;
        // SAFETY: The menu is live and the item was appended above with this identifier.
        if unsafe { SetMenuDefaultItem(menu.0, TrayAction::OpenHistory as u32, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(menu)
    }

    fn append(&self, action: TrayAction, label: &[u16]) -> io::Result<()> {
        // SAFETY: This menu owns its handle. label is NUL-terminated and remains live for the call.
        if unsafe { AppendMenuW(self.0, MF_STRING, action as usize, label.as_ptr()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn track(&self, window: HWND, point: POINT) -> Option<TrayAction> {
        // SAFETY: The owning window and menu are live. TPM_RETURNCMD returns the selection
        // without sending WM_COMMAND. WM_NULL lets the next invocation take focus.
        let selected = unsafe {
            SetForegroundWindow(window);
            let alignment = if GetSystemMetrics(SM_MENUDROPALIGNMENT) == 0 {
                TPM_LEFTALIGN
            } else {
                TPM_RIGHTALIGN
            };
            let selected = TrackPopupMenuEx(
                self.0,
                TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_RETURNCMD | alignment,
                point.x,
                point.y,
                window,
                null(),
            );
            PostMessageW(window, WM_NULL, 0, 0);
            selected
        };
        TrayAction::from_wparam(selected as WPARAM)
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        // SAFETY: This is the unique handle returned by CreatePopupMenu.
        unsafe { DestroyMenu(self.0) };
    }
}
