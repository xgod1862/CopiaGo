//! Read and write the current Windows clipboard without exposing its contents to logs.

mod image;
mod memory;
#[cfg(test)]
mod tests;

use std::fmt;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HWND, SetLastError};
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, EnumClipboardFormats,
    GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData,
};
use windows_sys::Win32::System::Ole::{
    CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
use windows_sys::core::PCWSTR;
use windows_sys::w;

pub use windows_sys::Win32::UI::WindowsAndMessaging::WM_CLIPBOARDUPDATE;

use self::memory::{LockedMemory, OwnedMemory};

pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_IMAGE_PIXELS: u64 = 50_000_000;

#[derive(Debug)]
pub enum ClipboardError {
    Win32(io::Error),
    ImageCodec(i32),
    InvalidText,
    InvalidFileList,
    InvalidImage,
    ImageTooLarge,
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Win32(error) => write!(formatter, "Windows clipboard operation failed ({error})"),
            Self::ImageCodec(code) => {
                write!(formatter, "Clipboard image codec failed ({code:#010x})")
            }
            Self::InvalidText => formatter.write_str("Invalid Windows clipboard text"),
            Self::InvalidFileList => formatter.write_str("Invalid Windows clipboard file list"),
            Self::InvalidImage => formatter.write_str("Invalid or unsupported clipboard image"),
            Self::ImageTooLarge => formatter.write_str("Clipboard image exceeds the size limit"),
        }
    }
}

impl std::error::Error for ClipboardError {}

impl From<io::Error> for ClipboardError {
    fn from(error: io::Error) -> Self {
        Self::Win32(error)
    }
}

impl From<windows::core::Error> for ClipboardError {
    fn from(error: windows::core::Error) -> Self {
        Self::ImageCodec(error.code().0)
    }
}

type Result<T> = std::result::Result<T, ClipboardError>;

pub enum ClipboardRead {
    Empty,
    Sensitive,
    Text(String),
    Png(Vec<u8>),
}

/// Clipboard access for a window on the calling thread.
///
/// The window receives WM_CLIPBOARDUPDATE through its normal message loop.
/// Drop the Clipboard before destroying that window. Changes made through
/// this object are marked as sensitive and subsequently skipped by read.
pub struct Clipboard {
    owner: HWND,
    formats: Formats,
}

impl Clipboard {
    pub fn new(owner: HWND) -> Result<Self> {
        check_owner(owner)?;
        let formats = Formats::register()?;
        add_listener(owner)?;
        Ok(Self { owner, formats })
    }

    /// Current clipboard sequence, or zero if the window station denies access.
    pub fn sequence_number(&self) -> u32 {
        // SAFETY: This query takes no pointers and does not open the clipboard.
        unsafe { GetClipboardSequenceNumber() }
    }

    /// Reads data eligible for PC history or phone sync. The registered PNG
    /// format takes precedence over bitmaps and text.
    pub fn read(&self) -> Result<ClipboardRead> {
        let data = OpenClipboardGuard::run(self.owner, |clipboard| {
            if self.formats.sensitive(clipboard)? {
                return Ok(ReadData::Sensitive);
            }
            if clipboard.available(self.formats.png) {
                return clipboard.with_data(self.formats.png, |data| {
                    Ok(ReadData::Png(image::png_payload(data)?.to_vec()))
                });
            }
            if let Some(format) = clipboard.bitmap_format()? {
                return clipboard.with_data(format, |data| {
                    Ok(ReadData::Bitmap(image::dib_as_bmp(data)?))
                });
            }
            if clipboard.available(CF_UNICODETEXT.into()) {
                return clipboard.with_data(CF_UNICODETEXT.into(), read_text);
            }
            Ok(ReadData::Empty)
        })?;

        // Decode or encode after closing the clipboard.
        match data {
            ReadData::Empty => Ok(ClipboardRead::Empty),
            ReadData::Sensitive => Ok(ClipboardRead::Sensitive),
            ReadData::Text(text) => Ok(ClipboardRead::Text(text)),
            ReadData::Png(png) => {
                image::validate_png(&png)?;
                Ok(ClipboardRead::Png(png))
            }
            ReadData::Bitmap(bmp) => Ok(ClipboardRead::Png(image::bmp_to_png(&bmp)?)),
        }
    }

    pub fn write_text(&self, text: &str) -> Result<()> {
        let data = encode_text(text)?;
        self.write([(CF_UNICODETEXT.into(), OwnedMemory::from_bytes(&data)?)])
    }

    /// Publishes an alpha-capable DIBV5 bitmap and a PNG representation.
    pub fn write_image(&self, encoded: &[u8]) -> Result<()> {
        let (dib, png) = image::prepare_write(encoded)?;
        self.write([(CF_DIBV5.into(), dib), (self.formats.png, png)])
    }

    pub fn write_files(&self, paths: &[PathBuf]) -> Result<()> {
        let files = encode_file_list(paths)?;
        self.write([
            (CF_HDROP.into(), OwnedMemory::from_bytes(&files)?),
            (
                self.formats.drop_effect,
                OwnedMemory::from_bytes(&DROPEFFECT_COPY.to_le_bytes())?,
            ),
        ])
    }

    fn write(&self, contents: impl IntoIterator<Item = (u32, OwnedMemory)>) -> Result<()> {
        // Stage every allocation before replacing the current clipboard.
        let privacy = [
            (self.formats.excluded, OwnedMemory::from_bytes(b"1")?),
            (self.formats.history, OwnedMemory::from_bytes(&[0; 4])?),
            (self.formats.cloud, OwnedMemory::from_bytes(&[0; 4])?),
        ];

        OpenClipboardGuard::run(self.owner, |clipboard| {
            clipboard.empty()?;
            // No content is published if any privacy marker fails.
            for (format, data) in privacy.into_iter().chain(contents) {
                if let Err(error) = clipboard.set(format, data) {
                    let _ = clipboard.empty();
                    return Err(error.into());
                }
            }
            Ok(())
        })
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        let _ = retry(|| {
            // SAFETY: The listener is removed on the thread on which it was registered.
            check_bool(unsafe { RemoveClipboardFormatListener(self.owner) })
        });
    }
}

struct Formats {
    excluded: u32,
    viewer_ignore: u32,
    history: u32,
    cloud: u32,
    png: u32,
    drop_effect: u32,
}

impl Formats {
    fn register() -> io::Result<Self> {
        Ok(Self {
            excluded: register_format(w!("ExcludeClipboardContentFromMonitorProcessing"))?,
            viewer_ignore: register_format(w!("Clipboard Viewer Ignore"))?,
            history: register_format(w!("CanIncludeInClipboardHistory"))?,
            cloud: register_format(w!("CanUploadToCloudClipboard"))?,
            png: register_format(w!("PNG"))?,
            drop_effect: register_format(w!("Preferred DropEffect"))?,
        })
    }

    fn sensitive(&self, clipboard: &OpenClipboardGuard) -> Result<bool> {
        if clipboard.available(self.excluded) || clipboard.available(self.viewer_ignore) {
            return Ok(true);
        }
        for flag in [self.history, self.cloud] {
            if clipboard.available(flag)
                && clipboard.with_data(flag, |data| Ok(data.starts_with(&[0; 4])))?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

enum ReadData {
    Empty,
    Sensitive,
    Text(String),
    Png(Vec<u8>),
    Bitmap(Vec<u8>),
}

fn encode_text(text: &str) -> Result<Vec<u8>> {
    if text.contains('\0') {
        return Err(ClipboardError::InvalidText);
    }
    Ok(text
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect())
}

fn read_text(data: &[u8]) -> Result<ReadData> {
    let (units, _) = data.as_chunks::<2>();
    let end = units
        .iter()
        .position(|unit| *unit == [0, 0])
        .ok_or(ClipboardError::InvalidText)?;
    let words = units[..end].iter().copied().map(u16::from_le_bytes);
    let text: String = char::decode_utf16(words)
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| ClipboardError::InvalidText)?;
    if text.trim().is_empty() {
        Ok(ReadData::Empty)
    } else {
        Ok(ReadData::Text(text))
    }
}

fn encode_file_list(paths: &[PathBuf]) -> Result<Vec<u8>> {
    if paths.is_empty() {
        return Err(ClipboardError::InvalidFileList);
    }
    // DROPFILES layout: pFiles, POINT, fNC, fWide. The list is UTF-16LE.
    const HEADER_BYTES: usize = 20;
    let mut data = vec![0; HEADER_BYTES];
    data[..4].copy_from_slice(&(HEADER_BYTES as u32).to_le_bytes());
    data[16..20].copy_from_slice(&1u32.to_le_bytes());
    for path in paths {
        if path.as_os_str().is_empty() {
            return Err(ClipboardError::InvalidFileList);
        }
        for unit in path.as_os_str().encode_wide() {
            if unit == 0 {
                return Err(ClipboardError::InvalidFileList);
            }
            data.extend_from_slice(&unit.to_le_bytes());
        }
        data.extend_from_slice(&[0; 2]);
    }
    data.extend_from_slice(&[0; 2]);
    Ok(data)
}

fn check_owner(owner: HWND) -> io::Result<()> {
    // SAFETY: User32 validates the handle and the optional process output is null.
    let thread = unsafe { GetWindowThreadProcessId(owner, null_mut()) };
    if thread == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: This query takes no arguments.
    if thread != unsafe { GetCurrentThreadId() } {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clipboard owner belongs to another thread",
        ));
    }
    Ok(())
}

fn add_listener(owner: HWND) -> io::Result<()> {
    retry(|| {
        // SAFETY: check_owner verified that owner belongs to the calling thread.
        check_bool(unsafe { AddClipboardFormatListener(owner) })
    })
}

fn register_format(name: PCWSTR) -> io::Result<u32> {
    retry(|| {
        // SAFETY: All callers pass a static NUL-terminated UTF-16 name.
        let format = unsafe { RegisterClipboardFormatW(name) };
        if format == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(format)
        }
    })
}

fn check_bool(value: i32) -> io::Result<()> {
    if value == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn retry<T>(mut call: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    // Match the Python client's schedule when the clipboard is contended.
    for attempt in 1..8 {
        if let Ok(result) = call() {
            return Ok(result);
        }
        thread::sleep(Duration::from_millis(50 * attempt));
    }
    call()
}

struct OpenClipboardGuard {
    needs_close: bool,
}

impl OpenClipboardGuard {
    fn run<T>(owner: HWND, operation: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        retry(|| {
            // SAFETY: The window was checked at listener registration.
            check_bool(unsafe { OpenClipboard(owner) })
        })?;
        let mut clipboard = Self { needs_close: true };
        let result = operation(&clipboard);
        let close = close_clipboard();
        clipboard.needs_close = close.is_err();
        let value = result?;
        close?;
        Ok(value)
    }

    fn available(&self, format: u32) -> bool {
        // SAFETY: This query examines the open clipboard without acquiring a handle.
        unsafe { IsClipboardFormatAvailable(format) != 0 }
    }

    fn bitmap_format(&self) -> io::Result<Option<u32>> {
        // Windows enumerates originals before their synthesized representations.
        // Request the first DIB format directly, avoiding a needless conversion.
        let mut previous = 0;
        loop {
            let format = retry(|| {
                // SAFETY: The guard keeps the clipboard open. Clear the last error
                // to distinguish the end of the list from an enumeration failure.
                unsafe {
                    SetLastError(ERROR_SUCCESS);
                    let next = EnumClipboardFormats(previous);
                    if next == 0 && windows_sys::Win32::Foundation::GetLastError() != ERROR_SUCCESS
                    {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(next)
                }
            })?;
            if format == 0 {
                return Ok(None);
            }
            if format == u32::from(CF_DIB) || format == u32::from(CF_DIBV5) {
                return Ok(Some(format));
            }
            previous = format;
        }
    }

    fn with_data<T>(&self, format: u32, operation: impl FnOnce(&[u8]) -> Result<T>) -> Result<T> {
        let handle = retry(|| {
            // SAFETY: The guard keeps the clipboard open during the call.
            let handle = unsafe { GetClipboardData(format) };
            if handle.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(handle)
            }
        })?;
        // SAFETY: Clipboard formats requested here are HGLOBAL data; the guard
        // keeps the borrowed handle alive until after the lock has been released.
        let locked = unsafe { LockedMemory::new(handle) }?;
        operation(locked.bytes())
    }

    fn empty(&self) -> io::Result<()> {
        retry(|| {
            // SAFETY: The clipboard was successfully opened with the owning HWND.
            check_bool(unsafe { EmptyClipboard() })
        })
    }

    fn set(&self, format: u32, data: OwnedMemory) -> io::Result<()> {
        retry(|| {
            // SAFETY: data is an initialized, unlocked GMEM_MOVEABLE allocation.
            let written = unsafe { SetClipboardData(format, data.handle()) };
            if written.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })?;
        data.transfer();
        Ok(())
    }
}

fn close_clipboard() -> io::Result<()> {
    retry(|| {
        // SAFETY: The guard opened the clipboard on this thread.
        check_bool(unsafe { CloseClipboard() })
    })
}

impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        if self.needs_close {
            let _ = close_clipboard();
        }
    }
}
