use std::ffi::c_void;
use std::fs;
use std::io;
use std::mem::size_of;
use std::path::Path;
use std::process::Command;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW,
    GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid,
    GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl, INHERITED_ACE, IsWellKnownSid,
    OBJECT_INHERIT_ACE, SE_DACL_PROTECTED, WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

use crate::atomic;
use crate::paths::ensure_data_dir;
use crate::test_support::TestDir;
use crate::win32::{LocalAllocation, wide_path};

const INHERITABLE: u32 = OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE;

#[derive(Clone, Copy)]
enum DescriptorSource {
    Native,
    Icacls,
}

#[test]
fn icacls_confirms_only_user_and_system_and_inheritance() {
    let dir = TestDir::new();
    let private_dir = dir.path().join("private");
    ensure_data_dir(&private_dir).unwrap();
    let child = private_dir.join("child");
    fs::create_dir(&child).unwrap();
    atomic::write(&child.join("sample.bin"), b"synthetic sample").unwrap();
    let grandchild = child.join("grandchild");
    fs::create_dir(&grandchild).unwrap();
    atomic::write(&grandchild.join("sample.bin"), b"synthetic sample").unwrap();

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

    let current_user = current_user_sid();
    let expected = [
        (r"private", INHERITABLE, true),
        (r"private\child", INHERITABLE, false),
        (r"private\child\sample.bin", 0, false),
        (r"private\child\grandchild", INHERITABLE, false),
        (r"private\child\grandchild\sample.bin", 0, false),
    ];
    assert_eq!(
        records.len(),
        expected.len(),
        "icacls should export the entire test tree"
    );
    for (path, flags, protected) in expected {
        let sddl = records
            .iter()
            .find_map(|[entry, sddl]| Path::new(entry).ends_with(path).then_some(*sddl))
            .unwrap_or_else(|| panic!("icacls omitted {path}"));
        let exported = parse_sddl(sddl).unwrap_or_else(|reason| panic!("icacls {path}: {reason}"));
        check_dacl(
            &exported,
            &current_user,
            flags,
            protected,
            DescriptorSource::Icacls,
        )
        .unwrap_or_else(|reason| panic!("icacls {path}: {reason}"));

        let native = read_descriptor(&dir.path().join(path))
            .unwrap_or_else(|error| panic!("Win32 {path}: {error}"));
        check_dacl(
            &native,
            &current_user,
            flags,
            protected,
            DescriptorSource::Native,
        )
        .unwrap_or_else(|reason| panic!("Win32 {path}: {reason}"));
    }
}

#[test]
fn dacl_check_accepts_equivalent_sid_and_access_mask_spellings() {
    let user = parse_sid("S-1-5-19");
    for sddl in [
        "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;S-1-5-19)",
        "D:P(A;CIOI;0x001f01ff;;;LS)(A;CIOI;0x001f01ff;;;S-1-5-18)",
    ] {
        let descriptor = parse_sddl(sddl).unwrap();
        assert_eq!(
            check_dacl(
                &descriptor,
                &user,
                INHERITABLE,
                true,
                DescriptorSource::Icacls
            ),
            Ok(())
        );
    }

    let system = parse_sid("S-1-5-18");
    let descriptor = parse_sddl("D:P(A;OICI;0x001f01ff;;;S-1-5-18)").unwrap();
    assert_eq!(
        check_dacl(
            &descriptor,
            &system,
            INHERITABLE,
            true,
            DescriptorSource::Icacls
        ),
        Ok(())
    );
}

#[test]
fn dacl_check_requires_the_inherited_marker_only_in_native_descendants() {
    let user = parse_sid("S-1-5-19");
    let missing_marker = Err("unexpected ACE inheritance flags");
    for (flags, expected, native_result) in [
        ("OICI", INHERITABLE, missing_marker),
        ("OICIID", INHERITABLE, Ok(())),
        ("", 0, missing_marker),
        ("ID", 0, Ok(())),
    ] {
        let sddl = format!("D:(A;{flags};FA;;;LS)(A;{flags};FA;;;SY)");
        let descriptor = parse_sddl(&sddl).unwrap();
        assert_eq!(
            check_dacl(
                &descriptor,
                &user,
                expected,
                false,
                DescriptorSource::Icacls
            ),
            Ok(())
        );
        assert_eq!(
            check_dacl(
                &descriptor,
                &user,
                expected,
                false,
                DescriptorSource::Native
            ),
            native_result
        );
    }
}

#[test]
fn dacl_check_rejects_incorrect_propagation_flags() {
    let user = parse_sid("S-1-5-19");
    for (flags, expected, protected) in [
        ("OICIID", INHERITABLE, true),
        ("OIID", INHERITABLE, false),
        ("CIID", INHERITABLE, false),
        ("OICINPID", INHERITABLE, false),
        ("OICIIOID", INHERITABLE, false),
        ("OIID", 0, false),
        ("CIID", 0, false),
        ("NPID", 0, false),
        ("IOID", 0, false),
    ] {
        let dacl_flags = if protected { "P" } else { "" };
        let sddl = format!("D:{dacl_flags}(A;{flags};FA;;;LS)(A;{flags};FA;;;SY)");
        let descriptor = parse_sddl(&sddl).unwrap();
        for source in [DescriptorSource::Native, DescriptorSource::Icacls] {
            assert_eq!(
                check_dacl(&descriptor, &user, expected, protected, source),
                Err("unexpected ACE inheritance flags"),
                "ACE flags {flags}"
            );
        }
    }
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
        let descriptor = parse_sddl(sddl).unwrap();
        assert_eq!(
            check_dacl(
                &descriptor,
                &user,
                INHERITABLE,
                true,
                DescriptorSource::Icacls
            ),
            Err(failure)
        );
    }
}

fn parse_sddl(sddl: &str) -> Result<LocalAllocation<c_void>, &'static str> {
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
    unsafe { LocalAllocation::from_raw(descriptor) }.ok_or("missing security descriptor")
}

fn read_descriptor(path: &Path) -> io::Result<LocalAllocation<c_void>> {
    let wide = wide_path(path)?;
    let mut descriptor = null_mut();
    // SAFETY: wide is NUL-terminated, the descriptor output is writable, and the other outputs
    // are optional.
    let result = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    // SAFETY: A successful query returns an owned LocalFree allocation.
    unsafe { LocalAllocation::from_raw(descriptor) }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing security descriptor"))
}

fn check_dacl(
    descriptor: &LocalAllocation<c_void>,
    current_user: &LocalAllocation<c_void>,
    propagation_flags: u32,
    protected: bool,
    source: DescriptorSource,
) -> Result<(), &'static str> {
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

    // icacls /save can omit the inherited marker that the live descendant ACEs carry.
    let (expected_flags, ignored_flags) = match (protected, source) {
        (true, _) => (propagation_flags, 0),
        (false, DescriptorSource::Native) => (propagation_flags | INHERITED_ACE, 0),
        (false, DescriptorSource::Icacls) => (propagation_flags, INHERITED_ACE),
    };
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
        if u32::from(header.AceFlags) & !ignored_flags != expected_flags {
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
