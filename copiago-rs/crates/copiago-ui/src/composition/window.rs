//! Separate Win32 input and rendering windows so the shadow never intercepts clicks.

use std::{
    cell::Cell,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{
            COLORREF, E_FAIL, E_INVALIDARG, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT,
            POINT, RECT, WPARAM,
        },
        Graphics::Gdi::{CreateRectRgn, CreateRoundRectRgn, DeleteObject, SetWindowRgn},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::GetFocus,
            WindowsAndMessaging::{
                CREATESTRUCTW, CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow,
                GWLP_USERDATA, GetWindowLongPtrW, HWND_TOPMOST, IDC_ARROW, IsWindowVisible,
                KillTimer, LWA_ALPHA, LoadCursorW, PostMessageW, RegisterClassW, SW_HIDE,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
                SetForegroundWindow, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW,
                SetWindowPos, ShowWindow, WM_APP, WM_CLOSE, WM_DESTROY, WM_DPICHANGED,
                WM_GETOBJECT, WM_KILLFOCUS, WM_NCCREATE, WM_NCDESTROY, WM_SETFOCUS,
                WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
                WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
                WS_POPUP,
            },
        },
    },
    core::{Error, HRESULT, PCWSTR, Result, w},
};

use super::{DEFAULT_DPI, PopupBounds, SHADOW_MARGIN, bounded_radius};
use crate::focus::PopupInteraction;

const CLASS: PCWSTR = w!("CopiaGo.CompositionPopup");
pub(super) const DPI_CHANGED: u32 = WM_APP + 1;
pub(super) const CLOSE_POPUP: u32 = WM_APP + 2;
/// A system settings broadcast was received. Re-read system preferences on the UI thread.
pub const POPUP_SETTINGS_CHANGED: u32 = WM_APP + 3;
pub(super) const DISMISS_POPUP: u32 = WM_APP + 4;
const DISMISS_TIMER: usize = 0xC043;
const DISMISS_MS: u32 = 150;

type DpiChange = (u32, POINT);

struct WindowData {
    dpi_change: Cell<Option<DpiChange>>,
    dismiss_on_focus_loss: Cell<bool>,
    dismiss_at: Cell<Option<Instant>>,
    interaction: PopupInteraction,
}

impl WindowData {
    fn new() -> Self {
        Self {
            dpi_change: Cell::new(None),
            dismiss_on_focus_loss: Cell::new(true),
            dismiss_at: Cell::new(None),
            interaction: PopupInteraction::default(),
        }
    }

    fn cancel_dismiss(&self, hwnd: HWND) {
        self.dismiss_at.set(None);
        // SAFETY: The window owns the timer ID and runs on this thread.
        let _ = unsafe { KillTimer(Some(hwnd), DISMISS_TIMER) };
    }

    fn start_dismiss(&self, hwnd: HWND) {
        if self.dismiss_on_focus_loss.get() {
            let deadline = Instant::now() + Duration::from_millis(u64::from(DISMISS_MS));
            // SAFETY: The window owns the timer ID and runs on this thread.
            if unsafe { SetTimer(Some(hwnd), DISMISS_TIMER, DISMISS_MS, None) } != 0 {
                self.dismiss_at.set(Some(deadline));
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Placement {
    pub(super) position: POINT,
    pub(super) bounds: PopupBounds,
    pub(super) dpi: u32,
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) margin: i32,
    diameter: i32,
}

impl Placement {
    pub(super) fn new(position: POINT, bounds: PopupBounds, dpi: u32) -> Result<Self> {
        let scale = dpi as f32 / DEFAULT_DPI as f32;
        let radius = bounded_radius(bounds.size(), bounds.radius)?;
        let width = (bounds.width * scale).round();
        let height = (bounds.height * scale).round();
        let margin = (SHADOW_MARGIN * scale).round() as i32;
        // GDI regions use signed 27-bit coordinates.
        const MAX_REGION_DIMENSION: f32 = (1_u32 << 26) as f32;
        if !(1..=u32::from(u16::MAX)).contains(&dpi)
            || !(1.0..MAX_REGION_DIMENSION).contains(&width)
            || !(1.0..MAX_REGION_DIMENSION).contains(&height)
            || position.x.checked_sub(margin).is_none()
            || position.y.checked_sub(margin).is_none()
            || position.x.checked_add(width as i32 + margin).is_none()
            || position.y.checked_add(height as i32 + margin).is_none()
        {
            return Err(Error::new(E_INVALIDARG, "Invalid popup placement"));
        }
        Ok(Self {
            position,
            bounds,
            dpi,
            width: width as i32,
            height: height as i32,
            margin,
            diameter: (2.0 * radius * scale).round() as i32,
        })
    }

    pub(super) fn scale(self) -> f32 {
        self.dpi as f32 / DEFAULT_DPI as f32
    }
}

pub(super) struct PopupWindows {
    // Destroy the owned visual window before its input owner and window data.
    visual: Window,
    input: Window,
    data: Box<WindowData>,
}

impl PopupWindows {
    pub(super) fn new() -> Result<Self> {
        let data = Box::new(WindowData::new());
        // SAFETY: The module, class strings and window procedure live for the
        // process. The Box keeps the input window data alive until destruction.
        unsafe {
            let instance = GetModuleHandleW(None)?.into();
            let class = WNDCLASSW {
                style: CS_DBLCLKS,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: CLASS,
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                let error = Error::from_win32();
                if error.code() != HRESULT::from_win32(ERROR_CLASS_ALREADY_EXISTS.0) {
                    return Err(error);
                }
            }

            let input = Window(CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP,
                CLASS,
                w!("CopiaGo"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance),
                Some(std::ptr::from_ref(data.as_ref()).cast()),
            )?);

            // HWND hit testing cannot see the compositor's alpha. A layered,
            // transparent rendering window passes all input to windows underneath.
            let visual = Window(CreateWindowExW(
                WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOREDIRECTIONBITMAP,
                CLASS,
                w!("CopiaGo Composition"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                Some(input.0),
                None,
                Some(instance),
                None,
            )?);
            SetLayeredWindowAttributes(visual.0, COLORREF(0), 255, LWA_ALPHA)?;

            Ok(Self {
                visual,
                input,
                data,
            })
        }
    }

    pub(super) fn input(&self) -> HWND {
        self.input.0
    }

    pub(super) fn visual(&self) -> HWND {
        self.visual.0
    }

    pub(super) fn interaction(&self) -> &PopupInteraction {
        &self.data.interaction
    }

    pub(super) fn dpi_at(&self, position: POINT) -> Result<u32> {
        // SAFETY: Move the hidden input HWND wholly onto the requested monitor.
        unsafe {
            SetWindowPos(
                self.input.0,
                None,
                position.x,
                position.y,
                1,
                1,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )?;
            self.data.dpi_change.set(None);
            let dpi = GetDpiForWindow(self.input.0);
            if dpi == 0 {
                Err(Error::from_hresult(E_FAIL))
            } else {
                Ok(dpi)
            }
        }
    }

    pub(super) fn take_dpi_change(&self) -> Option<DpiChange> {
        self.data.dpi_change.take()
    }

    pub(super) fn set_dismiss_on_focus_loss(&self, enabled: bool) {
        self.data.dismiss_on_focus_loss.set(enabled);
        if !enabled {
            self.data.cancel_dismiss(self.input.0);
        }
    }

    pub(super) fn should_dismiss(&self) -> bool {
        if !self.data.dismiss_on_focus_loss.get()
            || !self
                .data
                .dismiss_at
                .take()
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return false;
        }
        // SAFETY: Both functions inspect the visible window on its creation thread.
        unsafe { IsWindowVisible(self.input.0).as_bool() && GetFocus() != self.input.0 }
    }

    pub(super) fn place(&self, placement: Placement) -> Result<()> {
        let Placement {
            position,
            width,
            height,
            margin,
            diameter,
            ..
        } = placement;
        // SAFETY: Both windows belong to this thread. Win32 takes ownership of
        // the region on success, otherwise it must be deleted by the caller.
        unsafe {
            let region = if diameter > 0 {
                CreateRoundRectRgn(0, 0, width, height, diameter, diameter)
            } else {
                CreateRectRgn(0, 0, width, height)
            };
            if region.is_invalid() {
                return Err(Error::new(
                    E_FAIL,
                    "Could not create the popup input region",
                ));
            }
            if SetWindowRgn(self.input.0, Some(region), false) == 0 {
                let _ = DeleteObject(region.into());
                return Err(Error::new(E_FAIL, "Could not apply the popup input region"));
            }

            let flags = SWP_NOZORDER | SWP_NOACTIVATE;
            SetWindowPos(
                self.input.0,
                None,
                position.x,
                position.y,
                width,
                height,
                flags,
            )?;
            SetWindowPos(
                self.visual.0,
                None,
                position.x - margin,
                position.y - margin,
                width + 2 * margin,
                height + 2 * margin,
                flags,
            )
        }
    }

    pub(super) fn show(&self, activate: bool) -> Result<()> {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW;
        // SAFETY: Both HWNDs are live and belong to this thread.
        unsafe {
            SetWindowPos(self.input.0, Some(HWND_TOPMOST), 0, 0, 0, 0, flags)?;
            SetWindowPos(self.visual.0, Some(HWND_TOPMOST), 0, 0, 0, 0, flags)?;
            if activate {
                let _ = SetForegroundWindow(self.input.0);
            }
        }
        Ok(())
    }

    pub(super) fn hide(&self) {
        // SAFETY: Both HWNDs are live and belong to this thread.
        unsafe {
            let _ = ShowWindow(self.visual.0, SW_HIDE);
            let _ = ShowWindow(self.input.0, SW_HIDE);
        }
        self.data.cancel_dismiss(self.input.0);
        self.data.dpi_change.set(None);
    }
}

struct Window(HWND);

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: This guard owns the window and is dropped on its creation thread.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: CreateWindowExW supplies CREATESTRUCTW for WM_NCCREATE. The input
    // window's data remains live until WM_NCDESTROY. WM_DPICHANGED supplies a
    // readable RECT for the duration of this callback.
    unsafe {
        let data = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowData).as_ref();
        if let Some(data) = data {
            match message {
                WM_GETOBJECT => {
                    if let Some(provider) = data.interaction.get_object(hwnd, wparam, lparam) {
                        return provider;
                    }
                }
                WM_SETFOCUS | WM_KILLFOCUS => {
                    data.interaction.native_focus(hwnd, message == WM_SETFOCUS);
                }
                WM_DESTROY => data.interaction.destroy(hwnd),
                _ => {}
            }
        } else {
            match message {
                WM_GETOBJECT => {
                    if let Some(provider) =
                        PopupInteraction::decoration_object(hwnd, wparam, lparam)
                    {
                        return provider;
                    }
                }
                WM_DESTROY => PopupInteraction::destroy_decoration(hwnd),
                _ => {}
            }
        }
        match message {
            WM_NCCREATE => {
                let create = &*(lparam.0 as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            WM_CLOSE => {
                if data.is_some() {
                    let _ = PostMessageW(Some(hwnd), CLOSE_POPUP, WPARAM(0), LPARAM(0));
                }
                return LRESULT(0);
            }
            WM_SETTINGCHANGE => {
                if data.is_some() {
                    let _ = PostMessageW(Some(hwnd), POPUP_SETTINGS_CHANGED, WPARAM(0), LPARAM(0));
                }
            }
            WM_DPICHANGED => {
                if let (Some(data), Some(rect)) = (data, (lparam.0 as *const RECT).as_ref()) {
                    let dpi = (wparam.0 & 0xffff) as u32;
                    data.dpi_change.set(Some((
                        dpi,
                        POINT {
                            x: rect.left,
                            y: rect.top,
                        },
                    )));
                    let _ = PostMessageW(Some(hwnd), DPI_CHANGED, WPARAM(0), LPARAM(0));
                }
                return LRESULT(0);
            }
            WM_KILLFOCUS => {
                if let Some(data) = data {
                    data.start_dismiss(hwnd);
                }
            }
            WM_SETFOCUS => {
                if let Some(data) = data {
                    data.cancel_dismiss(hwnd);
                }
            }
            WM_TIMER if wparam.0 == DISMISS_TIMER => {
                if let Some(data) = data
                    && data
                        .dismiss_at
                        .get()
                        .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    let _ = KillTimer(Some(hwnd), DISMISS_TIMER);
                    let _ = PostMessageW(Some(hwnd), DISMISS_POPUP, WPARAM(0), LPARAM(0));
                }
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_and_shadow_scale_together() {
        let position = POINT { x: -960, y: 40 };
        let bounds = PopupBounds::new(392.0, 452.0, 24.0);
        for (dpi, expected) in [
            (96, (392, 452, 48, 40)),
            (144, (588, 678, 72, 60)),
            (192, (784, 904, 96, 80)),
        ] {
            let placement = Placement::new(position, bounds, dpi).unwrap();
            assert_eq!(placement.position, position);
            assert_eq!(
                (
                    placement.width,
                    placement.height,
                    placement.diameter,
                    placement.margin
                ),
                expected
            );
        }
    }

    #[test]
    fn native_region_clamps_radius_and_rejects_invalid_extents() {
        let capsule = PopupBounds::new(48.0, 11.0, 20.0);
        let origin = POINT::default();
        let placement = Placement::new(origin, capsule, 144).unwrap();
        assert_eq!(placement.height, 17);
        assert_eq!(placement.diameter, placement.height);
        assert!(Placement::new(origin, capsule, 0).is_err());
        assert!(Placement::new(origin, capsule, u32::MAX).is_err());
        assert!(Placement::new(origin, PopupBounds::new(f32::NAN, 12.0, 4.0), 96).is_err());
        assert!(Placement::new(POINT { x: i32::MAX, y: 0 }, capsule, 96).is_err());
    }
}
