//! The Python release's best-effort Windows 11 tray overflow preference.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    Globalization::{CSTR_EQUAL, CompareStringOrdinal},
    System::{
        Com::{CLSIDFromString, CoTaskMemFree},
        Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, KEY_SET_VALUE,
            REG_DWORD, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegEnumKeyExW,
            RegGetValueW, RegQueryValueExW,
        },
    },
    UI::Shell::SHGetKnownFolderPath,
};
use windows_sys::{core::GUID, w};

use crate::registry::Key;

const SETTINGS: &str = "Control Panel\\NotifyIconSettings";

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Promotion {
    Done,
    Pending,
    Promoted,
}

pub(super) fn try_promote() -> Promotion {
    let Ok(executable) = std::env::current_exe() else {
        return Promotion::Done;
    };
    promote_in(HKEY_CURRENT_USER, &executable)
}

fn promote_in(root: HKEY, executable: &Path) -> Promotion {
    let Some(target) = normalized_path(executable.as_os_str()) else {
        return Promotion::Done;
    };
    let Ok(settings) = Key::open(root, SETTINGS, KEY_ENUMERATE_SUB_KEYS) else {
        return Promotion::Done;
    };

    for index in 0.. {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        // SAFETY: Only the subkey name buffer and size are requested, both writable.
        if unsafe {
            RegEnumKeyExW(
                settings.0,
                index,
                name.as_mut_ptr(),
                &mut len,
                null(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        } != ERROR_SUCCESS
        {
            // Explorer might not have created an entry for this executable yet.
            return Promotion::Pending;
        }
        let name = String::from_utf16_lossy(&name[..len as usize]);
        let Ok(key) = Key::open(settings.0, &name, KEY_QUERY_VALUE | KEY_SET_VALUE) else {
            continue;
        };
        let Some(path) = executable_path(&key).and_then(|path| expand_folder(&path)) else {
            continue;
        };
        let Some(path) = normalized_path(&path) else {
            continue;
        };
        // SAFETY: Both normalized paths are NUL-terminated.
        if unsafe { CompareStringOrdinal(path.as_ptr(), -1, target.as_ptr(), -1, 1) } != CSTR_EQUAL
        {
            continue;
        }

        // SAFETY: The key is live and only existence is queried, with no output buffer.
        let result = unsafe {
            RegQueryValueExW(
                key.0,
                w!("IsPromoted"),
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        match result {
            ERROR_SUCCESS => return Promotion::Done,
            ERROR_FILE_NOT_FOUND
                if key
                    .write("IsPromoted", REG_DWORD, &1u32.to_le_bytes())
                    .is_ok() =>
            {
                return Promotion::Promoted;
            }
            _ => {}
        }
    }
    Promotion::Pending
}

fn executable_path(key: &Key) -> Option<Vec<u16>> {
    let mut bytes = 0;
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    // SAFETY: Only the required buffer length is requested.
    if unsafe {
        RegGetValueW(
            key.0,
            null(),
            w!("ExecutablePath"),
            flags,
            null_mut(),
            null_mut(),
            &mut bytes,
        )
    } != ERROR_SUCCESS
        || bytes > 2 * 32_768
    {
        return None;
    }
    // Allow for RegGetValueW to add a missing terminator.
    let mut path = vec![0u16; (bytes as usize).div_ceil(2) + 1];
    bytes = size_of_val(path.as_slice()) as u32;
    // SAFETY: path supplies bytes bytes of writable storage.
    if unsafe {
        RegGetValueW(
            key.0,
            null(),
            w!("ExecutablePath"),
            flags,
            null_mut(),
            path.as_mut_ptr().cast(),
            &mut bytes,
        )
    } != ERROR_SUCCESS
    {
        return None;
    }
    path.truncate(path.iter().position(|&unit| unit == 0)?);
    Some(path)
}

fn expand_folder(path: &[u16]) -> Option<OsString> {
    if path.first() != Some(&u16::from(b'{')) {
        return Some(OsString::from_wide(path));
    }
    let end = path.iter().position(|&unit| unit == u16::from(b'}'))?;
    let folder: Vec<_> = path[..=end].iter().copied().chain([0]).collect();
    let mut id = GUID::from_u128(0);
    // SAFETY: folder is NUL-terminated and id is writable.
    if unsafe { CLSIDFromString(folder.as_ptr(), &mut id) } < 0 {
        return None;
    }

    let mut buffer = null_mut();
    // SAFETY: id and buffer are valid. The returned path uses CoTaskMemFree even on failure.
    let result = unsafe { SHGetKnownFolderPath(&id, 0, null_mut(), &mut buffer) };
    let expanded = if result >= 0 && !buffer.is_null() {
        let buffer = windows::core::PCWSTR(buffer);
        // SAFETY: The successful API returned a readable NUL-terminated path.
        let base = unsafe { buffer.as_wide() };
        let mut base = OsString::from_wide(base);
        base.push(OsString::from_wide(&path[end + 1..]));
        Some(base)
    } else {
        None
    };
    // SAFETY: The returned allocation belongs to this call; null is permitted.
    unsafe { CoTaskMemFree(buffer.cast()) };
    expanded
}

fn normalized_path(path: &OsStr) -> Option<Vec<u16>> {
    let path = std::path::absolute(path).ok()?;
    Some(path.as_os_str().encode_wide().chain([0]).collect())
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::System::Registry::{KEY_ALL_ACCESS, REG_SZ, RRF_RT_REG_DWORD};

    use crate::registry::fixture::Fixture;

    use super::*;

    #[test]
    fn promotion_waits_for_explorer_and_respects_an_existing_choice() {
        let registry = Fixture::new();
        let path = Path::new(r"C:\Program Files\CopiaGo\copiago.exe");
        assert_eq!(promote_in(registry.handle(), path), Promotion::Done);
        let settings = Key::create(registry.handle(), SETTINGS, KEY_ALL_ACCESS).unwrap();
        assert_eq!(promote_in(registry.handle(), path), Promotion::Pending);

        let entry = Key::create(settings.0, "test-app", KEY_ALL_ACCESS).unwrap();
        let stored_path = r"c:\PROGRAM FILES\CopiaGo\copiago.exe";
        let bytes: Vec<_> = stored_path
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        entry.write("ExecutablePath", REG_SZ, &bytes).unwrap();
        assert_eq!(promote_in(registry.handle(), path), Promotion::Promoted);
        assert_eq!(read_promoted(&entry), 1);

        entry
            .write("IsPromoted", REG_DWORD, &0u32.to_le_bytes())
            .unwrap();
        assert_eq!(promote_in(registry.handle(), path), Promotion::Done);
        assert_eq!(read_promoted(&entry), 0);
    }

    #[test]
    fn promotion_recognizes_known_folder_paths() {
        let path: Vec<_> = r"{F38BF404-1D43-42F2-9305-67DE0B28FC23}\copiago.exe"
            .encode_utf16()
            .collect();
        let expected = Path::new(&std::env::var_os("SystemRoot").unwrap()).join("copiago.exe");
        assert_eq!(Path::new(&expand_folder(&path).unwrap()), expected);
    }

    fn read_promoted(key: &Key) -> u32 {
        let mut state = 0u32;
        let mut bytes = size_of_val(&state) as u32;
        // SAFETY: The key is live, and state and bytes are writable.
        let status = unsafe {
            RegGetValueW(
                key.0,
                null(),
                w!("IsPromoted"),
                RRF_RT_REG_DWORD,
                null_mut(),
                (&mut state as *mut u32).cast(),
                &mut bytes,
            )
        };
        assert_eq!(status, ERROR_SUCCESS);
        state
    }
}
