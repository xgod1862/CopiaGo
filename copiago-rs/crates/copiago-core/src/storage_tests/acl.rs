use std::ffi::c_void;
use std::fs;
use std::mem::size_of;
use std::path::Path;
use std::process::Command;
use std::ptr::null_mut;

use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, CONTAINER_INHERIT_ACE, EqualSid, GetAce,
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, INHERITED_ACE, IsWellKnownSid,
    OBJECT_INHERIT_ACE, SE_DACL_PROTECTED, WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

use crate::atomic;
use crate::paths::ensure_data_dir;
use crate::test_support::TestDir;
use crate::win32::LocalAllocation;

const INHERITABLE: u32 = OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE;

#[test]
fn icacls_confirms_only_user_and_system_and_inheritance() {
    let dir = TestDir::new();
    let private_dir = dir.path().join("private");
    ensure_data_dir(&private_dir).unwrap();
    let child = private_dir.join("child");
    fs::create_dir(&child).unwrap();
    atomic::write(&child.join("sample.bin"), b"synthetic sample").unwrap();

    let acl_file = dir.path().join("acl-export.txt");
    let output = Command::new("icacls.exe")
        .arg(&private_dir)
        .arg("/save")
        .arg(&acl_file)
        .args(["/t", "/q"])
        .output()
        .unwrap();
    assert!(output.status.success(), "icacls /save failed");

    let export = read_utf16_file(&acl_file);
    let lines: Vec<_> = export
        .trim_start_matches('\u{feff}')
        .lines()
        .filter(|line| !line.is_empty())
        .collect();
    let (records, remainder) = lines.as_chunks::<2>();
    assert!(
        remainder.is_empty(),
        "expected path and SDDL pairs in icacls export"
    );
    assert_eq!(
        records.len(),
        3,
        "icacls should export both folders and the file"
    );

    let current_user = current_user_sid();
    for (path, flags, protected) in [
        (r"private", INHERITABLE, true),
        (r"private\child", INHERITABLE | INHERITED_ACE, false),
        (r"private\child\sample.bin", INHERITED_ACE, false),
    ] {
        let sddl = records
            .iter()
            .find_map(|[entry, sddl]| Path::new(entry).ends_with(path).then_some(*sddl))
            .unwrap_or_else(|| panic!("icacls omitted {path}"));
        check_dacl(sddl, &current_user, flags, protected)
            .unwrap_or_else(|reason| panic!("{path}: {reason}"));
    }
}

#[test]
fn dacl_check_accepts_equivalent_sid_and_access_mask_spellings() {
    let user = parse_sid("S-1-5-19");
    for sddl in [
        "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;S-1-5-19)",
        "D:P(A;CIOI;0x001f01ff;;;LS)(A;CIOI;0x001f01ff;;;S-1-5-18)",
    ] {
        assert_eq!(check_dacl(sddl, &user, INHERITABLE, true), Ok(()));
    }

    let system = parse_sid("S-1-5-18");
    let sddl = "D:P(A;OICI;0x001f01ff;;;S-1-5-18)";
    assert_eq!(check_dacl(sddl, &system, INHERITABLE, true), Ok(()));
}

#[test]
fn dacl_check_rejects_changed_permissions_and_principals() {
    let user = parse_sid("S-1-5-19");
    for (sddl, failure) in [
        (
            "D:P(A;OICI;FA;;;LS)(A;OICI;FA;;;SY)(A;OICI;FA;;;BU)",
            "unexpected DACL entry count",
        ),
        (
            "D:P(A;OICI;FA;;;LS)(A;OICI;FA;;;BU)",
            "unexpected or duplicate DACL principal",
        ),
        (
            "D:P(A;OICI;FA;;;LS)(A;OICI;FA;;;S-1-5-19)",
            "unexpected or duplicate DACL principal",
        ),
        (
            "D:P(A;OICI;FR;;;LS)(A;OICI;FA;;;SY)",
            "DACL entry does not grant full file access",
        ),
        (
            "D:P(D;OICI;FA;;;LS)(A;OICI;FA;;;SY)",
            "DACL entry is not a plain allow entry",
        ),
        (
            "D:P(A;CI;FA;;;LS)(A;OICI;FA;;;SY)",
            "unexpected ACE inheritance flags",
        ),
        (
            "D:(A;OICI;FA;;;LS)(A;OICI;FA;;;SY)",
            "unexpected DACL protection flag",
        ),
    ] {
        assert_eq!(check_dacl(sddl, &user, INHERITABLE, true), Err(failure));
    }
}

fn check_dacl(
    sddl: &str,
    current_user: &LocalAllocation<c_void>,
    expected_flags: u32,
    protected: bool,
) -> Result<(), &'static str> {
    let wide: Vec<_> = sddl.encode_utf16().chain([0]).collect();
    let mut descriptor = null_mut();
    // SAFETY: wide is NUL-terminated and the descriptor output is writable.
    let parsed = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if parsed == 0 {
        return Err("Windows could not parse icacls SDDL");
    }
    // SAFETY: Successful conversion returns an owned LocalFree allocation.
    let descriptor =
        unsafe { LocalAllocation::from_raw(descriptor) }.ok_or("missing security descriptor")?;

    let (mut control, mut revision) = (0, 0);
    // SAFETY: The descriptor is live and both outputs are writable.
    let got_control =
        unsafe { GetSecurityDescriptorControl(descriptor.as_ptr(), &mut control, &mut revision) };
    if got_control == 0 || (control & SE_DACL_PROTECTED != 0) != protected {
        return Err("unexpected DACL protection flag");
    }

    let (mut present, mut defaulted) = (0, 0);
    let mut acl = null_mut();
    // SAFETY: The descriptor is live and the three output parameters are writable.
    let got_dacl = unsafe {
        GetSecurityDescriptorDacl(descriptor.as_ptr(), &mut present, &mut acl, &mut defaulted)
    };
    if got_dacl == 0 || present == 0 || acl.is_null() {
        return Err("expected a populated DACL");
    }

    let mut need_user = true;
    // SAFETY: current_user owns a valid, live SID.
    let mut need_system = unsafe { IsWellKnownSid(current_user.as_ptr(), WinLocalSystemSid) == 0 };
    // SAFETY: acl points to the parsed ACL inside the live descriptor.
    let count = unsafe { (*acl).AceCount };
    if count != 1 + u16::from(need_system) {
        return Err("unexpected DACL entry count");
    }

    for index in 0..u32::from(count) {
        let mut entry = null_mut();
        // SAFETY: acl is valid, index is within AceCount and entry is writable.
        if unsafe { GetAce(acl, index, &mut entry) } == 0 || entry.is_null() {
            return Err("Windows could not read a DACL entry");
        }
        // SAFETY: GetAce returned a live, aligned ACE, which begins with an ACE_HEADER.
        let header = unsafe { entry.cast::<ACE_HEADER>().read() };
        if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE
            || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
        {
            return Err("DACL entry is not a plain allow entry");
        }
        if u32::from(header.AceFlags) != expected_flags {
            return Err("unexpected ACE inheritance flags");
        }

        let entry = entry.cast::<ACCESS_ALLOWED_ACE>();
        // SAFETY: The header identifies a plain ACCESS_ALLOWED_ACE in the parsed DACL.
        if unsafe { (*entry).Mask } != FILE_ALL_ACCESS {
            return Err("DACL entry does not grant full file access");
        }
        // SAFETY: SidStart is the beginning of the valid SID in this parsed allow ACE,
        // and current_user owns the SID returned by ConvertStringSidToSidW.
        let (is_user, is_system) = unsafe {
            let sid = (&raw mut (*entry).SidStart).cast();
            (
                EqualSid(sid, current_user.as_ptr()) != 0,
                IsWellKnownSid(sid, WinLocalSystemSid) != 0,
            )
        };
        if is_user && need_user {
            need_user = false;
        } else if is_system && need_system {
            need_system = false;
        } else {
            return Err("unexpected or duplicate DACL principal");
        }
    }
    Ok(())
}

fn current_user_sid() -> LocalAllocation<c_void> {
    let output = Command::new("whoami.exe")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "whoami could not query the current SID"
    );
    let csv = String::from_utf8_lossy(&output.stdout);
    let sid = csv.trim_end().rsplit_once(',').unwrap().1.trim_matches('"');
    parse_sid(sid)
}

fn parse_sid(sid: &str) -> LocalAllocation<c_void> {
    let wide: Vec<_> = sid.encode_utf16().chain([0]).collect();
    let mut parsed = null_mut();
    // SAFETY: wide is NUL-terminated and the parsed SID output is writable.
    let succeeded = unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut parsed) };
    assert_ne!(succeeded, 0, "Windows could not parse a test SID");
    // SAFETY: Successful conversion returns an owned LocalFree allocation.
    unsafe { LocalAllocation::from_raw(parsed) }.expect("missing parsed SID")
}

fn read_utf16_file(path: &Path) -> String {
    let raw = fs::read(path).unwrap();
    let (chunks, remainder) = raw.as_chunks::<2>();
    assert!(remainder.is_empty(), "expected UTF-16 icacls export");
    let units: Vec<_> = chunks.iter().copied().map(u16::from_le_bytes).collect();
    String::from_utf16(&units).unwrap()
}
