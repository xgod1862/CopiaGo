use std::ptr::null;

use windows_sys::Win32::System::Registry::{KEY_QUERY_VALUE, REG_DWORD, RegQueryValueExW};
use windows_sys::w;

use crate::registry::fixture::Fixture;

use super::*;

#[test]
fn isolated_registry_uses_the_python_run_value_and_command() {
    let registry = Fixture::new();
    let root = registry.handle();
    let path = Path::new(r"C:\Copia Go\测试 📋.exe");
    let command = startup_command(path);
    let expected: Vec<_> = "\"C:\\Copia Go\\测试 📋.exe\" --minimized"
        .encode_utf16()
        .chain([0])
        .collect();
    assert_eq!(command, expected);
    assert!(!enabled_in(root, &command).unwrap());

    set_in(root, Some(&command)).unwrap();
    let python_key = Key::open(
        root,
        "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
        KEY_QUERY_VALUE | KEY_SET_VALUE,
    )
    .unwrap();
    let python_name = w!("iPhoneClipboardPrototype");
    let mut actual = [0u16; 264];
    let mut bytes = size_of_val(&actual) as u32;
    let mut value_type = 0;
    // SAFETY: The handle is live, and all requested outputs are writable.
    let result = unsafe {
        RegQueryValueExW(
            python_key.0,
            python_name,
            null_mut(),
            &mut value_type,
            actual.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    assert_eq!(result, ERROR_SUCCESS);
    assert_eq!(value_type, REG_SZ);
    assert_eq!(&actual[..bytes as usize / 2], expected);
    assert!(enabled_in(root, &command).unwrap());
    assert!(
        !enabled_in(
            root,
            &startup_command(Path::new(r"C:\Old\iPhoneClipboard.exe"))
        )
        .unwrap()
    );

    // A malformed or different command doesn't make this copy start at sign-in.
    python_key
        .write("iPhoneClipboardPrototype", REG_DWORD, &1u32.to_le_bytes())
        .unwrap();
    assert!(!enabled_in(root, &command).unwrap());
    set_in(root, None).unwrap();
    set_in(root, None).unwrap();
    // SAFETY: The handle and registry name are valid. No data or size is requested.
    let result = unsafe {
        RegGetValueW(
            python_key.0,
            null(),
            python_name,
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            null_mut(),
        )
    };
    assert_eq!(result, ERROR_FILE_NOT_FOUND);
    assert!(!enabled_in(root, &command).unwrap());
}
