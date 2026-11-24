use std::fs::{self, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::process::Command;

use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

use crate::paths::{DEVICE_DATA_DIR, ensure_data_dir};
use crate::test_support::TestDir;
use crate::{DeviceInfo, atomic, dpapi};

mod acl;

#[test]
fn dpapi_pairing_and_history_round_trip() {
    for sample in [b"synthetic DPAPI sample".as_slice(), b""] {
        let protected = dpapi::protect(sample).unwrap();
        assert!(protected.starts_with(dpapi::MAGIC));
        assert_eq!(dpapi::unprotect(&protected).unwrap(), sample);

        let history = dpapi::protect_history(sample).unwrap();
        assert!(!history.starts_with(dpapi::MAGIC));
        assert_eq!(dpapi::unprotect_history(&history).unwrap(), sample);
    }
    assert_eq!(
        dpapi::unprotect(b"old plaintext").unwrap(),
        b"old plaintext"
    );
    assert!(dpapi::unprotect(dpapi::MAGIC).is_err());
    assert!(dpapi::unprotect_history(b"unsealed history").is_err());
}

#[test]
fn atomic_write_creates_and_replaces_a_file() {
    let dir = TestDir::new();
    let parent = dir.path().join("数据".repeat(60)).join("a".repeat(120));
    ensure_data_dir(&parent).unwrap();
    let path = parent.join("sample.bin");
    atomic::write(&path, b"first synthetic value").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"first synthetic value");
    atomic::write(&path, b"updated").unwrap();
    assert_eq!(fs::read(path).unwrap(), b"updated");
    assert_eq!(fs::read_dir(parent).unwrap().count(), 1);
}

#[test]
fn atomic_write_preserves_the_destination_if_replacement_fails() {
    let dir = TestDir::new();
    let path = dir.path().join("sample.bin");
    atomic::write(&path, b"original").unwrap();
    let reader = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&path)
        .unwrap();
    assert!(atomic::write(&path, b"replacement").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"original");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    drop(reader);
    atomic::write(&path, b"replacement").unwrap();
    assert_eq!(fs::read(path).unwrap(), b"replacement");
}

#[test]
fn device_info_round_trip_and_replace() {
    let dir = TestDir::new();
    let root = dir.path().join("data");
    let first = DeviceInfo {
        name: "测试设备 📱".to_owned(),
        model: "iPhone 15 Pro".to_owned(),
        os_version: "18.1".to_owned(),
    };
    first.save(&root).unwrap();
    assert_eq!(DeviceInfo::load(&root), first);
    assert!(
        fs::read(root.join(DEVICE_DATA_DIR).join("device-info.bin"))
            .unwrap()
            .starts_with(dpapi::MAGIC)
    );

    let updated = DeviceInfo {
        os_version: "18.2".to_owned(),
        ..first
    };
    updated.save(&root).unwrap();
    assert_eq!(DeviceInfo::load(&root), updated);
}

#[test]
fn device_info_reads_older_plaintext_and_handles_missing_or_corrupt_files() {
    let dir = TestDir::new();
    assert_eq!(DeviceInfo::load(dir.path()), DeviceInfo::default());
    let devices = dir.path().join(DEVICE_DATA_DIR);
    ensure_data_dir(&devices).unwrap();
    let path = devices.join("device-info.bin");

    atomic::write(&path, br#"{"name":"legacy","unused":true,"model":null}"#).unwrap();
    assert_eq!(
        DeviceInfo::load(dir.path()),
        DeviceInfo {
            name: "legacy".to_owned(),
            ..DeviceInfo::default()
        }
    );

    for corrupt in [b"invalid JSON".as_slice(), b"[]", dpapi::MAGIC] {
        atomic::write(&path, corrupt).unwrap();
        assert_eq!(DeviceInfo::load(dir.path()), DeviceInfo::default());
    }
}

#[test]
fn reads_python_compatible_powershell_fixture() {
    let dir = TestDir::new();
    let devices = dir.path().join(DEVICE_DATA_DIR);
    ensure_data_dir(&devices).unwrap();
    let generated = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "& ([scriptblock]::Create($env:COPIAGO_TEST_FIXTURE_SCRIPT))",
        ])
        .env(
            "COPIAGO_TEST_FIXTURE_SCRIPT",
            include_str!("../tests/fixtures/generate_device_info.ps1"),
        )
        .env("COPIAGO_TEST_FIXTURE_PATH", devices.join("device-info.bin"))
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "PowerShell fixture generation failed ({})",
        generated.status
    );
    assert_eq!(
        DeviceInfo::load(dir.path()),
        DeviceInfo {
            name: "测试 iPhone 📱".to_owned(),
            model: "iPhone 15 Pro".to_owned(),
            os_version: "18.1".to_owned(),
        }
    );
}
