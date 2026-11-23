//! CopiaGo's Windows data directory layout.

use std::env;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use windows_sys::Win32::System::WindowsProgramming::GetUserNameW;

mod acl;
#[cfg(test)]
mod tests;

pub use acl::ensure_data_dir;

const LEGACY_DIR_NAME: &str = "iPhoneClipboardPrototype";
pub const DEVICE_DATA_DIR: &str = "device-data";

/// Whether the executable is running from a development build or a packaged distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    Development,
    Packaged,
}

/// Selects the data root for this run without creating or modifying anything.
/// A nonempty ICB_DATA_DIR always takes precedence.
pub fn data_dir(mode: RunMode) -> io::Result<PathBuf> {
    resolve_data_dir(
        mode,
        env::var_os("ICB_DATA_DIR"),
        env::current_exe,
        || env::var_os("LOCALAPPDATA"),
        || {
            current_user_name()
                .or_else(|| env::var_os("USERNAME"))
                .unwrap_or_default()
        },
    )
}

fn resolve_data_dir(
    mode: RunMode,
    override_dir: Option<OsString>,
    executable: impl FnOnce() -> io::Result<PathBuf>,
    local_app_data: impl FnOnce() -> Option<OsString>,
    user_name: impl FnOnce() -> OsString,
) -> io::Result<PathBuf> {
    if let Some(path) = override_dir.filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    match mode {
        RunMode::Development => local_app_data()
            .map(|path| PathBuf::from(path).join(LEGACY_DIR_NAME))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set")),
        RunMode::Packaged => {
            let executable = executable()?;
            let app = executable.parent().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "missing executable directory")
            })?;
            let data = app.join("data");
            if app.join("portable-data").is_file() {
                return Ok(data);
            }
            Ok(data.join(account_folder_name(&user_name())))
        }
    }
}

fn current_user_name() -> Option<OsString> {
    // UNLEN (256) plus the terminating NUL.
    let mut name = [0; 257];
    let mut len = name.len() as u32;
    // SAFETY: The buffer is writable for len code units and len is an output parameter.
    let succeeded = unsafe { GetUserNameW(name.as_mut_ptr(), &mut len) };
    (succeeded != 0).then(|| OsString::from_wide(&name[..len as usize - 1]))
}

fn account_folder_name(name: &OsStr) -> OsString {
    let mut sanitized: Vec<_> = name
        .encode_wide()
        .map(|unit| {
            let illegal = matches!(
                u8::try_from(unit),
                Ok(b'<' | b'>' | b':' | b'"' | b'/' | b'\\' | b'|' | b'?' | b'*')
            );
            if unit < 32 || illegal {
                u16::from(b'_')
            } else {
                unit
            }
        })
        .collect();
    while matches!(sanitized.last(), Some(32 | 46)) {
        sanitized.pop();
    }
    if sanitized.is_empty() {
        return OsString::from("user");
    }
    OsString::from_wide(&sanitized)
}
