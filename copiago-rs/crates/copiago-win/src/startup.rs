//! Windows sign-in startup in the same Run value used by the Python release.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;

use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, ERROR_UNSUPPORTED_TYPE},
    System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ,
        RRF_RT_REG_SZ, RegDeleteValueW, RegGetValueW,
    },
};

use crate::registry::{Key, status};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "iPhoneClipboardPrototype";

/// Reports whether the Run value exactly matches this copy's command line.
pub fn startup_enabled() -> io::Result<bool> {
    enabled_in(
        HKEY_CURRENT_USER,
        &startup_command(&std::env::current_exe()?),
    )
}

/// Registers this copy for startup with `--minimized`, or removes the shared Run value.
pub fn set_startup_enabled(enabled: bool) -> io::Result<()> {
    let command = if enabled {
        Some(startup_command(&std::env::current_exe()?))
    } else {
        None
    };
    set_in(HKEY_CURRENT_USER, command.as_deref())
}

fn startup_command(executable: &Path) -> Vec<u16> {
    let mut command = OsString::from("\"");
    command.push(executable);
    command.push("\" --minimized");
    command.encode_wide().chain([0]).collect()
}

fn enabled_in(root: HKEY, command: &[u16]) -> io::Result<bool> {
    let key: Vec<_> = RUN_KEY.encode_utf16().chain([0]).collect();
    let name: Vec<_> = RUN_VALUE.encode_utf16().chain([0]).collect();
    let mut current = vec![0u16; command.len()];
    let mut bytes = size_of_val(current.as_slice()) as u32;
    // SAFETY: Names are NUL-terminated, and current has bytes bytes of writable storage.
    let result = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
            null_mut(),
            current.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    match result {
        ERROR_SUCCESS => Ok(bytes as usize == size_of_val(command) && current == command),
        ERROR_FILE_NOT_FOUND | ERROR_MORE_DATA | ERROR_UNSUPPORTED_TYPE => Ok(false),
        _ => Err(io::Error::from_raw_os_error(result as i32)),
    }
}

fn set_in(root: HKEY, command: Option<&[u16]>) -> io::Result<()> {
    let key = Key::create(root, RUN_KEY, KEY_SET_VALUE)?;
    if let Some(command) = command {
        // SAFETY: A u16 slice has size_of_val(command) initialized bytes, including the NUL.
        let bytes = unsafe {
            std::slice::from_raw_parts(command.as_ptr().cast::<u8>(), size_of_val(command))
        };
        key.write(RUN_VALUE, REG_SZ, bytes)
    } else {
        let name: Vec<_> = RUN_VALUE.encode_utf16().chain([0]).collect();
        // SAFETY: The key is live and the name is NUL-terminated.
        let result = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        status(result)
    }
}

#[cfg(test)]
mod tests;
