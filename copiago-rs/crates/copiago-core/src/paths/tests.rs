use std::fs;

use crate::test_support::TestDir;

use super::*;

#[test]
fn nonempty_override_takes_precedence_in_both_modes() {
    for mode in [RunMode::Development, RunMode::Packaged] {
        let path = resolve_data_dir(
            mode,
            Some("relative-data".into()),
            || panic!("override should skip executable lookup"),
            || panic!("override should skip LOCALAPPDATA lookup"),
            || panic!("override should skip account lookup"),
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("relative-data"));
    }
}

#[test]
fn portable_marker_file_takes_precedence_in_packaged_mode() {
    let dir = TestDir::new();
    fs::write(dir.path().join("portable-data"), []).unwrap();
    let path = resolve_data_dir(
        RunMode::Packaged,
        Some(OsString::new()),
        || Ok(dir.path().join("copiago.exe")),
        || panic!("portable mode should skip LOCALAPPDATA lookup"),
        || panic!("portable mode should skip account lookup"),
    )
    .unwrap();
    assert_eq!(path, dir.path().join("data"));
}

#[test]
fn installed_mode_uses_sanitized_windows_account() {
    let dir = TestDir::new();
    let executable = dir.path().join("copiago.exe");
    let resolve = || {
        resolve_data_dir(
            RunMode::Packaged,
            None,
            || Ok(executable.clone()),
            || panic!("installed mode should skip LOCALAPPDATA lookup"),
            || "测试<>:\"/\\|?*\u{001f} .".into(),
        )
        .unwrap()
    };

    let expected = dir.path().join("data").join("测试__________");
    assert_eq!(resolve(), expected);

    fs::create_dir(dir.path().join("portable-data")).unwrap();
    assert_eq!(resolve(), expected);
    assert_eq!(account_folder_name(OsStr::new(" . ")), "user");
    assert_eq!(account_folder_name(OsStr::new("\0")), "_");
}

#[test]
fn development_mode_uses_the_legacy_local_app_data_directory() {
    let dir = TestDir::new();
    let path = resolve_data_dir(
        RunMode::Development,
        Some(OsString::new()),
        || panic!("development mode should skip executable lookup"),
        || Some(dir.path().into()),
        || panic!("development mode should skip account lookup"),
    )
    .unwrap();
    assert_eq!(path, dir.path().join("iPhoneClipboardPrototype"));
}

#[test]
fn installed_upgrade_publishes_legacy_contents_and_retains_backup() {
    let dir = TestDir::new();
    let legacy = dir.path().join(LEGACY_DIR_NAME);
    let target = dir.path().join("app").join("data").join("user");
    ensure_data_dir(&legacy).unwrap();
    ensure_data_dir(&target).unwrap();
    fs::write(legacy.join("settings.json"), br#"{"pull":false}"#).unwrap();
    fs::write(legacy.join("history.sqlite3"), b"old history sample").unwrap();
    fs::write(legacy.join("settings.tmp"), b"interrupted write").unwrap();
    fs::create_dir(legacy.join("received")).unwrap();
    fs::create_dir(legacy.join(DEVICE_DATA_DIR)).unwrap();
    fs::write(
        legacy.join(DEVICE_DATA_DIR).join("remote_sample.plist"),
        b"sample pairing data",
    )
    .unwrap();

    assert_eq!(
        upgrade::prepare(target.clone(), legacy.clone()).unwrap(),
        target
    );
    let backup = legacy.with_extension("before-rust-upgrade");
    assert!(!legacy.exists());
    assert_eq!(
        fs::read(target.join("history.sqlite3")).unwrap(),
        fs::read(backup.join("history.sqlite3")).unwrap()
    );
    assert_eq!(
        fs::read(target.join("settings.json")).unwrap(),
        br#"{"pull":false}"#
    );
    assert_eq!(
        fs::read(target.join(DEVICE_DATA_DIR).join("remote_sample.plist")).unwrap(),
        b"sample pairing data"
    );
    assert!(!target.join("received").exists());
    assert!(!target.join("settings.tmp").exists());
}

#[test]
fn installed_upgrade_preserves_existing_account_data() {
    let dir = TestDir::new();
    let legacy = dir.path().join(LEGACY_DIR_NAME);
    let target = dir.path().join("app").join("data").join("user");
    ensure_data_dir(&legacy).unwrap();
    ensure_data_dir(&target).unwrap();
    fs::write(legacy.join("settings.json"), b"legacy").unwrap();
    fs::write(target.join("settings.json"), b"installed").unwrap();

    assert_eq!(
        upgrade::prepare(target.clone(), legacy.clone()).unwrap(),
        target
    );
    assert_eq!(
        fs::read(target.join("settings.json")).unwrap(),
        b"installed"
    );
    assert_eq!(fs::read(legacy.join("settings.json")).unwrap(), b"legacy");
}

#[test]
fn interrupted_installed_upgrade_uses_legacy_and_discards_partial_copy() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = TestDir::new();
    let legacy = dir.path().join(LEGACY_DIR_NAME);
    let parent = dir.path().join("data");
    let target = parent.join("user");
    ensure_data_dir(&legacy).unwrap();
    fs::create_dir(&parent).unwrap();
    let source = legacy.join("history.sqlite3");
    fs::write(&source, b"locked sample").unwrap();
    let locked = fs::File::options()
        .write(true)
        .share_mode(0)
        .open(&source)
        .unwrap();

    assert_eq!(
        upgrade::prepare(target.clone(), legacy.clone()).unwrap(),
        legacy
    );
    assert!(!target.exists());
    assert_eq!(fs::read_dir(parent).unwrap().count(), 0);
    drop(locked);
    assert_eq!(fs::read(source).unwrap(), b"locked sample");
}
