use std::ptr::null;

use windows_sys::Win32::System::Registry::{KEY_QUERY_VALUE, REG_DWORD, RegQueryValueExW};
use windows_sys::core::PCWSTR;
use windows_sys::w;

use crate::registry::fixture::Fixture;

use super::*;

const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

fn read_value(key: &Key, name: PCWSTR) -> Option<Vec<u16>> {
    let mut actual = [0u16; 264];
    let mut bytes = size_of_val(&actual) as u32;
    let mut value_type = 0;
    // SAFETY: The handle is live, and all requested outputs are writable.
    let result = unsafe {
        RegQueryValueExW(
            key.0,
            name,
            null_mut(),
            &mut value_type,
            actual.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    (result == ERROR_SUCCESS && value_type == REG_SZ).then(|| actual[..bytes as usize / 2].to_vec())
}

fn missing(key: &Key, name: PCWSTR) -> bool {
    // SAFETY: The handle and registry name are valid. No data or size is requested.
    let result = unsafe {
        RegGetValueW(
            key.0,
            null(),
            name,
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            null_mut(),
        )
    };
    result == ERROR_FILE_NOT_FOUND
}

#[test]
fn isolated_registry_uses_the_copiago_run_value_and_command() {
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
    let run = Key::open(root, RUN, KEY_QUERY_VALUE | KEY_SET_VALUE).unwrap();
    assert_eq!(
        read_value(&run, w!("CopiaGo")).as_deref(),
        Some(&expected[..])
    );
    assert!(enabled_in(root, &command).unwrap());
    assert!(!enabled_in(root, &startup_command(Path::new(r"C:\Old\copiago.exe"))).unwrap());

    // A malformed or different command doesn't make this copy start at sign-in.
    run.write("CopiaGo", REG_DWORD, &1u32.to_le_bytes())
        .unwrap();
    assert!(!enabled_in(root, &command).unwrap());
    set_in(root, None).unwrap();
    set_in(root, None).unwrap();
    assert!(missing(&run, w!("CopiaGo")));
    assert!(!enabled_in(root, &command).unwrap());
}

#[test]
fn the_pre_rename_value_is_honoured_then_replaced() {
    let registry = Fixture::new();
    let root = registry.handle();
    let command = startup_command(Path::new(r"C:\Programs\CopiaGo\copiago.exe"));
    let bytes = command
        .iter()
        .flat_map(|unit| unit.to_le_bytes())
        .collect::<Vec<_>>();
    let run = Key::create(root, RUN, KEY_QUERY_VALUE | KEY_SET_VALUE).unwrap();
    run.write("iPhoneClipboardPrototype", REG_SZ, &bytes)
        .unwrap();

    // An upgrade keeps the user's choice before the app rewrites it.
    assert!(enabled_in(root, &command).unwrap());

    set_in(root, Some(&command)).unwrap();
    assert!(missing(&run, w!("iPhoneClipboardPrototype")));
    assert_eq!(
        read_value(&run, w!("CopiaGo")).as_deref(),
        Some(&command[..])
    );

    run.write("iPhoneClipboardPrototype", REG_SZ, &bytes)
        .unwrap();
    set_in(root, None).unwrap();
    assert!(missing(&run, w!("iPhoneClipboardPrototype")));
    assert!(missing(&run, w!("CopiaGo")));
    assert!(!enabled_in(root, &command).unwrap());
}
