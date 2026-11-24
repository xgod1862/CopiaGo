use std::io;

use windows_sys::Win32::UI::{
    HiDpi::GetSystemMetricsForDpi,
    WindowsAndMessaging::{
        CreateIconFromResourceEx, DestroyIcon, HICON, LR_DEFAULTCOLOR, SM_CXSMICON, SM_CYSMICON,
    },
};

use super::TrayIconState;

#[repr(C, align(4))]
struct Aligned<const N: usize>([u8; N]);

macro_rules! ico {
    ($filename:literal) => {{
        static FILE: Aligned<{ include_bytes!($filename).len() }> =
            Aligned(*include_bytes!($filename));
        &FILE.0
    }};
}

pub(super) struct Icon(pub(super) HICON);

impl Icon {
    pub(super) fn load(state: TrayIconState, dpi: u32) -> io::Result<Self> {
        let file: &[u8] = match state {
            // Frames from jade.ico, with the same grayscale conversion as Python for disconnected.
            TrayIconState::Connected => ico!("assets/connected.ico"),
            TrayIconState::Disconnected => ico!("assets/disconnected.ico"),
            TrayIconState::Error => ico!("assets/error.ico"),
        };
        // SAFETY: These system metrics take a positive DPI and have no pointer arguments.
        let (width, height) = unsafe {
            (
                GetSystemMetricsForDpi(SM_CXSMICON, dpi),
                GetSystemMetricsForDpi(SM_CYSMICON, dpi),
            )
        };
        let data = frame(file, width.max(height)).ok_or(io::ErrorKind::InvalidData)?;
        // SAFETY: The selected frame is a complete PNG from an embedded ICO. Both the file
        // and its frame offset are DWORD-aligned. Omitting LR_SHARED transfers ownership.
        let icon = unsafe {
            CreateIconFromResourceEx(
                data.as_ptr(),
                data.len() as u32,
                1,
                0x0003_0000,
                width,
                height,
                LR_DEFAULTCOLOR,
            )
        };
        if icon.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(icon))
    }
}

impl Drop for Icon {
    fn drop(&mut self) {
        // SAFETY: load obtained this non-shared icon handle.
        unsafe { DestroyIcon(self.0) };
    }
}

fn frame(file: &[u8], size: i32) -> Option<&[u8]> {
    let count = u16::from_le_bytes(file.get(4..6)?.try_into().ok()?) as usize;
    let (entries, _) = file.get(6..6 + count * 16)?.as_chunks::<16>();
    let entry = entries
        .iter()
        .find(|entry| i32::from(entry[0]) >= size)
        .or_else(|| entries.last())?;
    let size = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
    let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
    if !offset.is_multiple_of(4) {
        return None;
    }
    file.get(offset..offset.checked_add(size)?)
}
