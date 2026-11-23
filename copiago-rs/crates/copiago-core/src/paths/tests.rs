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
