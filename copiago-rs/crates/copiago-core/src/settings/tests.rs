use std::env;
use std::fs::OpenOptions;
use std::os::windows::fs::OpenOptionsExt;
use std::sync::Barrier;
use std::thread;

use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

use super::*;
use crate::paths::RunMode;
use crate::test_support::TestDir;

#[test]
fn defaults_for_missing_empty_or_invalid_documents() {
    let dir = TestDir::new();
    let data = dir.path().join("data");
    let defaults = Settings {
        language: LanguagePreference::System,
        history: false,
        pc_history: false,
        pull: true,
        push: false,
        pull_types: vec![SyncKind::Text, SyncKind::Image, SyncKind::File],
        push_types: vec![SyncKind::Text, SyncKind::Image],
        file_ask_mb: 20,
        notifications: true,
        update_check: true,
    };
    assert_eq!(Settings::default(), defaults);
    assert_eq!(Settings::load(&data), defaults);
    assert_eq!(
        Settings::read(&data).unwrap_err().kind(),
        ErrorKind::NotFound
    );
    assert!(!data.exists());

    let path = dir.path().join(FILE_NAME);
    fs::write(&path, b"{}").unwrap();
    assert_eq!(Settings::read(dir.path()).unwrap(), defaults);
    for contents in [b"{".as_slice(), b"[]", b"null", b"{\"a\":\"\xff\"}"] {
        fs::write(&path, contents).unwrap();
        assert_eq!(Settings::load(dir.path()), defaults);
        assert_eq!(
            Settings::read(dir.path()).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&path).unwrap(), contents);
    }
}

#[test]
fn illegal_values_fall_back_individually() {
    let dir = TestDir::new();
    fs::write(
        dir.path().join(FILE_NAME),
        br#"{
            "language": "unknown",
            "history": 1,
            "pc_history": "true",
            "pull": 0,
            "push": true,
            "pull_types": "file",
            "push_types": ["file", "unknown", true, null],
            "file_ask_mb": false,
            "notifications": null,
            "update_check": "false"
        }"#,
    )
    .unwrap();
    assert_eq!(
        Settings::read(dir.path()).unwrap(),
        Settings {
            push: true,
            ..Settings::default()
        }
    );
}

#[test]
fn sync_types_are_filtered_deduplicated_and_ordered() {
    let dir = TestDir::new();
    let path = dir.path().join(FILE_NAME);
    fs::write(
        &path,
        br#"{
            "pull_types": ["file", "unknown", "image", "file", {}],
            "push_types": ["file", "te\u0078t", false, 1e400]
        }"#,
    )
    .unwrap();
    let settings = Settings::read(dir.path()).unwrap();
    assert_eq!(settings.pull_types, [SyncKind::Image, SyncKind::File]);
    assert_eq!(settings.push_types, [SyncKind::Text]);

    fs::write(&path, br#"{"pull_types": [], "push_types": []}"#).unwrap();
    assert_eq!(Settings::read(dir.path()).unwrap(), Settings::default());
    let settings = Settings::update(
        dir.path(),
        SettingsChange::PushTypes(vec![SyncKind::Image, SyncKind::File, SyncKind::Image]),
    )
    .unwrap();
    assert_eq!(settings.push_types, [SyncKind::Image]);
    let raw = read_document(&path).unwrap();
    assert_eq!(raw["push_types"].get(), r#"["image"]"#);
}

#[test]
fn file_ask_mb_accepts_only_the_supported_numeric_values() {
    let dir = TestDir::new();
    let path = dir.path().join(FILE_NAME);
    for (raw, expected) in [
        ("10", 10),
        ("20", 20),
        ("50", 50),
        ("0", 0),
        ("10.0", 10),
        ("5e1", 50),
        ("-0.0", 0),
        ("false", 20),
        ("true", 20),
        (r#""0""#, 20),
        ("null", 20),
        ("-10", 20),
        ("50.1", 20),
        ("1e400", 20),
    ] {
        fs::write(&path, format!(r#"{{"file_ask_mb":{raw}}}"#)).unwrap();
        assert_eq!(Settings::read(dir.path()).unwrap().file_ask_mb, expected);
    }
    let before = fs::read(&path).unwrap();
    let error = Settings::update(dir.path(), SettingsChange::FileAskMb(1)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn settings_updates_round_trip_all_keys() {
    let dir = TestDir::new();
    let data = dir.path().join("data");
    let mut updated = Settings::default();
    for change in [
        SettingsChange::Language(LanguagePreference::ZhCn),
        SettingsChange::History(true),
        SettingsChange::PcHistory(true),
        SettingsChange::Pull(false),
        SettingsChange::Push(true),
        SettingsChange::PullTypes(vec![SyncKind::File, SyncKind::Text, SyncKind::Text]),
        SettingsChange::PushTypes(vec![SyncKind::Image]),
        SettingsChange::FileAskMb(0),
        SettingsChange::Notifications(false),
        SettingsChange::UpdateCheck(false),
    ] {
        updated = Settings::update(&data, change).unwrap();
    }
    let expected = Settings {
        language: LanguagePreference::ZhCn,
        history: true,
        pc_history: true,
        pull: false,
        push: true,
        pull_types: vec![SyncKind::Text, SyncKind::File],
        push_types: vec![SyncKind::Image],
        file_ask_mb: 0,
        notifications: false,
        update_check: false,
    };
    assert_eq!(updated, expected);
    assert_eq!(Settings::read(&data).unwrap(), expected);
    let stored: serde_json::Value =
        serde_json::from_slice(&fs::read(data.join(FILE_NAME)).unwrap()).unwrap();
    assert_eq!(
        stored,
        serde_json::json!({
            "language": "zh-CN",
            "history": true,
            "pc_history": true,
            "pull": false,
            "push": true,
            "pull_types": ["text", "file"],
            "push_types": ["image"],
            "file_ask_mb": 0,
            "notifications": false,
            "update_check": false,
        })
    );
}

#[test]
fn update_preserves_untouched_json_values() {
    let dir = TestDir::new();
    let path = dir.path().join(FILE_NAME);
    let future = r#"{ "data": [999999999999999999999999, 1e400, true, "测试"] }"#;
    fs::write(
        &path,
        format!(r#"{{ "language": "system", "history": true, "file_ask_mb": "invalid", "future": {future} }}"#),
    )
    .unwrap();
    let settings =
        Settings::update(dir.path(), SettingsChange::Language(LanguagePreference::En)).unwrap();
    let raw = read_document(&path).unwrap();
    assert_eq!(raw.len(), 4);
    assert_eq!(raw["future"].get(), future);
    assert_eq!(raw["file_ask_mb"].get(), r#""invalid""#);
    assert_eq!(raw["history"].get(), "true");
    assert_eq!(raw["language"].get(), r#""en""#);
    assert_eq!(settings.file_ask_mb, 20);
}

#[test]
fn update_replaces_a_malformed_document() {
    let dir = TestDir::new();
    let path = dir.path().join(FILE_NAME);
    fs::write(&path, br#"{"history":"#).unwrap();
    Settings::update(dir.path(), SettingsChange::History(true)).unwrap();
    let raw = read_document(&path).unwrap();
    assert_eq!(raw.len(), 1);
    assert_eq!(raw["history"].get(), "true");
}

#[test]
fn failed_atomic_update_preserves_the_file_and_cleans_up() {
    let dir = TestDir::new();
    let path = dir.path().join(FILE_NAME);
    let before = br#"{"notifications":true,"future":"keep"}"#;
    fs::write(&path, before).unwrap();
    let reader = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&path)
        .unwrap();

    assert!(Settings::update(dir.path(), SettingsChange::Notifications(false)).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);

    drop(reader);
    Settings::update(dir.path(), SettingsChange::Notifications(false)).unwrap();
    assert!(!Settings::read(dir.path()).unwrap().notifications);
    assert_eq!(read_document(&path).unwrap()["future"].get(), r#""keep""#);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn simultaneous_updates_to_different_keys_are_retained() {
    let dir = TestDir::new();
    let barrier = Barrier::new(2);
    thread::scope(|thread| {
        for change in [SettingsChange::History(true), SettingsChange::Pull(false)] {
            let barrier = &barrier;
            let data = dir.path();
            thread.spawn(move || {
                barrier.wait();
                Settings::update(data, change).unwrap();
            });
        }
    });
    let settings = Settings::read(dir.path()).unwrap();
    assert!(settings.history);
    assert!(!settings.pull);
}

#[test]
#[ignore = "requires ICB_DATA_DIR to point to an existing Python installation's data folder"]
fn reads_installed_python_settings() {
    assert!(
        env::var_os("ICB_DATA_DIR").is_some_and(|path| !path.is_empty()),
        "set ICB_DATA_DIR to the installed user data directory"
    );
    let data = paths::data_dir(RunMode::Packaged).expect("resolve ICB_DATA_DIR");
    let path = data.join(FILE_NAME);
    let before = fs::read(&path).expect("read installed settings.json");
    assert!(
        Settings::read(&data).is_ok(),
        "installed settings.json did not parse"
    );
    assert!(
        fs::read(path).is_ok_and(|after| after == before),
        "installed settings.json changed during verification"
    );
}

#[test]
fn a_utf8_bom_written_by_windows_powershell_is_accepted() {
    let dir = TestDir::new();
    fs::write(
        dir.path().join(FILE_NAME),
        "\u{feff}{\"push\":true,\"file_ask_mb\":0}",
    )
    .unwrap();
    let settings = Settings::read(dir.path()).unwrap();
    assert!(settings.push);
    assert_eq!(settings.file_ask_mb, 0);
}
