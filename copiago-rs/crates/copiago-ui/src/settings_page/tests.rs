use std::{
    env, fs,
    path::Path,
    sync::atomic::{AtomicU32, Ordering},
};

use super::*;
use copiago_core::paths;

static NEXT_FIXTURE: AtomicU32 = AtomicU32::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let parent = env::temp_dir();
        let directory = parent.join(format!(
            "copiago-settings-ui-test-{}-{sequence}",
            std::process::id()
        ));
        assert!(!directory.exists());
        paths::ensure_data_dir(&directory).expect("private clipboard settings fixture");
        Self(directory)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // All immediate children are files created by these tests.
        let _ = fs::remove_file(self.0.join("settings.json"));
        let _ = fs::remove_file(self.0.join("blocked"));
        let _ = fs::remove_dir(&self.0);
    }
}

#[test]
fn writes_each_json_setting_using_the_python_schema() {
    let fixture = Fixture::new();
    let path = fixture.path();
    fs::write(
        path.join("settings.json"),
        br#"{"legacy_option":{"keep":true}}"#,
    )
    .unwrap();
    let now = Instant::now();
    let mut state = SettingsState::load(path, "2.0.0-beta");
    for (action, change) in [
        (
            SettingsAction::Toggle(SettingSwitch::Pull),
            SettingsChange::Pull(false),
        ),
        (
            SettingsAction::Toggle(SettingSwitch::Push),
            SettingsChange::Push(true),
        ),
        (
            SettingsAction::Toggle(SettingSwitch::History),
            SettingsChange::History(true),
        ),
        (
            SettingsAction::Toggle(SettingSwitch::PcHistory),
            SettingsChange::PcHistory(true),
        ),
        (
            SettingsAction::Toggle(SettingSwitch::Notifications),
            SettingsChange::Notifications(false),
        ),
        (
            SettingsAction::Toggle(SettingSwitch::UpdateCheck),
            SettingsChange::UpdateCheck(false),
        ),
        (
            SettingsAction::ToggleType(SyncDirection::ToPc, SyncKind::File),
            SettingsChange::PullTypes(vec![SyncKind::Text, SyncKind::Image]),
        ),
        (
            SettingsAction::ToggleType(SyncDirection::ToPhone, SyncKind::Image),
            SettingsChange::PushTypes(vec![SyncKind::Text]),
        ),
        (
            SettingsAction::ChooseFileAskMb(0),
            SettingsChange::FileAskMb(0),
        ),
        (
            SettingsAction::ChooseLanguage(LanguagePreference::ZhCn),
            SettingsChange::Language(LanguagePreference::ZhCn),
        ),
    ] {
        assert_eq!(
            state.activate(action, now),
            Some(SettingsEffect::SettingChanged(change))
        );
        assert_eq!(state.settings(), &Settings::read(path).unwrap());
    }
    assert_eq!(
        state.settings(),
        &Settings {
            language: LanguagePreference::ZhCn,
            history: true,
            pc_history: true,
            pull: false,
            push: true,
            pull_types: vec![SyncKind::Text, SyncKind::Image],
            push_types: vec![SyncKind::Text],
            file_ask_mb: 0,
            notifications: false,
            update_check: false,
        }
    );
    assert!(
        fs::read_to_string(path.join("settings.json"))
            .unwrap()
            .contains(r#""legacy_option":{"keep":true}"#)
    );
    assert_eq!(state.settings(), SettingsState::load(path, "").settings());

    for value in [10, 20, 50, 0] {
        assert_eq!(
            state.activate(SettingsAction::ChooseFileAskMb(value), now),
            Some(SettingsEffect::SettingChanged(SettingsChange::FileAskMb(
                value
            ))),
        );
    }
    for preference in [LanguagePreference::En, LanguagePreference::System] {
        assert!(
            state
                .activate(SettingsAction::ChooseLanguage(preference), now)
                .is_some()
        );
        assert_eq!(state.language(), preference.resolve());
    }
}

#[test]
fn cannot_turn_off_the_last_type_or_select_an_invalid_choice() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let now = Instant::now();
    Settings::update(path, SettingsChange::PullTypes(vec![SyncKind::File])).unwrap();
    Settings::update(path, SettingsChange::PushTypes(vec![SyncKind::Image])).unwrap();
    let mut state = SettingsState::load(path, "");
    for action in [
        SettingsAction::ToggleType(SyncDirection::ToPc, SyncKind::File),
        SettingsAction::ToggleType(SyncDirection::ToPhone, SyncKind::Image),
        SettingsAction::ToggleType(SyncDirection::ToPhone, SyncKind::File),
        SettingsAction::ChooseFileAskMb(30),
        SettingsAction::ChooseFileAskMb(20),
        SettingsAction::ChooseLanguage(LanguagePreference::System),
    ] {
        assert!(!state.can_activate(action));
        assert_eq!(state.activate(action, now), None);
    }
    assert_eq!(state.settings().pull_types, vec![SyncKind::File]);
    assert_eq!(state.settings().push_types, vec![SyncKind::Image]);
    assert_eq!(&Settings::read(path).unwrap(), state.settings());
    assert!(
        state
            .activate(
                SettingsAction::ToggleType(SyncDirection::ToPhone, SyncKind::Text),
                now
            )
            .is_some()
    );
}

#[test]
fn failed_writes_preserve_the_previous_setting() {
    let fixture = Fixture::new();
    let bad_directory = fixture.path().join("blocked");
    fs::write(&bad_directory, "").unwrap();
    let mut state = SettingsState::load(bad_directory, "");
    let now = Instant::now();
    assert_eq!(
        state.activate(
            SettingsAction::ChooseLanguage(LanguagePreference::ZhCn),
            now
        ),
        None
    );
    assert_eq!(state.settings().language, LanguagePreference::System);
    assert_eq!(
        state.notice().unwrap().render(Language::En),
        "Could not save the language preference."
    );
    assert_eq!(
        state.activate(SettingsAction::Toggle(SettingSwitch::History), now),
        None
    );
    assert!(!state.settings().history);
}

#[test]
fn visible_progress_message_uses_the_new_language_immediately() {
    let fixture = Fixture::new();
    let mut state = SettingsState::load(fixture.path(), "");
    let now = Instant::now();
    state.set_connection(ConnectionState {
        phase: ConnectionPhase::Connecting,
        detail: Some(message!(
            "正在下载文件 {index}/{count} · {received} / {total}",
            index = 2,
            count = 4,
            received = "10 MB",
            total = "20 MB",
        )),
        ..ConnectionState::default()
    });
    state.activate(
        SettingsAction::ChooseLanguage(LanguagePreference::ZhCn),
        now,
    );
    let localized_detail = |state: &SettingsState| {
        state
            .connection()
            .detail
            .as_ref()
            .unwrap()
            .render(state.language())
    };
    assert_eq!(localized_detail(&state), "正在下载文件 2/4 · 10 MB / 20 MB");
    state.activate(SettingsAction::ChooseLanguage(LanguagePreference::En), now);
    assert_eq!(
        localized_detail(&state),
        "Downloading file 2/4 · 10 MB / 20 MB"
    );
    assert_eq!(
        Settings::read(fixture.path()).unwrap().language,
        LanguagePreference::En
    );
}

#[test]
fn forget_requires_another_click_in_three_seconds_and_waits_for_worker() {
    let fixture = Fixture::new();
    let mut state = SettingsState::load(fixture.path(), "");
    assert!(!state.can_activate(SettingsAction::ForgetDevice));
    state.set_connection(ConnectionState {
        device: Some(DeviceInfo::default()),
        ..ConnectionState::default()
    });
    let now = Instant::now();
    let forget = SettingsAction::ForgetDevice;
    assert_eq!(state.activate(forget, now), None);
    let deadline = state.confirmation_deadline().unwrap();
    assert_eq!(deadline, now + Duration::from_secs(3));
    assert!(!state.expire_confirmation(now));
    assert!(state.expire_confirmation(deadline));
    assert!(!state.confirming_forget(deadline));
    assert_eq!(state.activate(forget, deadline), None);
    assert_eq!(
        state.activate(SettingsAction::Back, deadline),
        Some(SettingsEffect::ShowHistory)
    );
    assert_eq!(state.activate(forget, deadline), None);
    assert_eq!(
        state.activate(forget, deadline + Duration::from_millis(2999)),
        Some(SettingsEffect::ForgetDevice)
    );
    assert!(!state.can_activate(forget));
    assert!(state.connection().device.is_some());
    state.complete_forget(false);
    assert!(state.can_activate(forget));
    assert!(state.notice().is_some());
    state.activate(forget, deadline);
    assert_eq!(
        state.activate(forget, deadline),
        Some(SettingsEffect::ForgetDevice)
    );
    state.complete_forget(true);
    assert!(state.connection().show_guide());
    assert_eq!(state.connection().phase, ConnectionPhase::Setup);
    assert_eq!(
        state.activate(SettingsAction::Back, now),
        Some(SettingsEffect::ShowPairingGuide)
    );
}

#[test]
fn startup_and_update_feed_back_through_host_without_changing_actual_registry() {
    let fixture = Fixture::new();
    let mut state = SettingsState::load(fixture.path(), "");
    let now = Instant::now();
    let startup = SettingsAction::Toggle(SettingSwitch::Startup);
    assert_eq!(state.activate(startup, now), None);
    state.set_startup(StartupState::Available { enabled: false });
    assert_eq!(
        state.activate(startup, now),
        Some(SettingsEffect::SetStartupEnabled(true))
    );
    state.complete_startup(Err(io::Error::other("fixture")));
    assert_eq!(state.switch(SettingSwitch::Startup), (false, true));
    assert!(state.notice().is_some());
    state.complete_startup(Ok(StartupState::Available { enabled: true }));
    assert_eq!(
        state.activate(startup, now),
        Some(SettingsEffect::SetStartupEnabled(false))
    );
    assert!(!fixture.path().join("settings.json").exists());

    state.set_available_version(Some("2.0.1".into()));
    assert_eq!(
        state.activate(SettingsAction::OpenUpdate, now),
        Some(SettingsEffect::OpenUpdate)
    );
    state.activate(SettingsAction::Toggle(SettingSwitch::UpdateCheck), now);
    state.set_available_version(Some("2.0.1".into()));
    assert!(state.available_version().is_none());
    assert_eq!(state.activate(SettingsAction::OpenUpdate, now), None);
}

#[test]
fn waiting_with_an_existing_pairing_offers_interactive_recovery() {
    let fixture = Fixture::new();
    let mut state = SettingsState::load(fixture.path(), "");
    state.set_connection(ConnectionState {
        phase: ConnectionPhase::Waiting,
        device: Some(DeviceInfo::default()),
        ..ConnectionState::default()
    });
    assert_eq!(
        state.activate(SettingsAction::Retry, Instant::now()),
        Some(SettingsEffect::Retry)
    );
    assert_eq!(state.connection().phase, ConnectionPhase::Connecting);
}

#[test]
fn issues_only_available_pair_retry_and_driver_actions() {
    let fixture = Fixture::new();
    let mut state = SettingsState::load(fixture.path(), "");
    let now = Instant::now();
    assert_eq!(
        state.activate(SettingsAction::RequestPair, now),
        Some(SettingsEffect::RequestPair)
    );
    assert_eq!(state.connection().phase, ConnectionPhase::Pairing);
    assert_eq!(state.activate(SettingsAction::RequestPair, now), None);
    state.set_connection(ConnectionState {
        phase: ConnectionPhase::DeveloperMode,
        driver: DriverState::Failed,
        ..ConnectionState::default()
    });
    assert_eq!(
        state.activate(SettingsAction::OpenAppleDevicesStore, now),
        Some(SettingsEffect::OpenAppleDevicesStore)
    );
    assert_eq!(
        state.activate(SettingsAction::InstallDriver, now),
        Some(SettingsEffect::InstallDriver)
    );
    assert_eq!(state.activate(SettingsAction::InstallDriver, now), None);
    assert_eq!(state.activate(SettingsAction::Retry, now), None);
    let mut connection = state.connection().clone();
    connection.driver = DriverState::Ready;
    state.set_connection(connection);
    assert_eq!(
        state.activate(SettingsAction::Retry, now),
        Some(SettingsEffect::Retry)
    );
    assert_eq!(state.connection().phase, ConnectionPhase::Connecting);
    state.set_connection(ConnectionState {
        phase: ConnectionPhase::Connected { wireless: true },
        device: Some(DeviceInfo::default()),
        first_connection: true,
        ..ConnectionState::default()
    });
    assert_eq!(
        state.activate(SettingsAction::FinishPairing, now),
        Some(SettingsEffect::ShowHistory)
    );
    assert!(!state.connection().show_guide());
    assert_eq!(state.activate(SettingsAction::FinishPairing, now), None);
}

#[test]
fn unavailable_service_offers_repair_and_store_without_parallel_pairing() {
    let fixture = Fixture::new();
    let mut state = SettingsState::load(fixture.path(), "");
    let now = Instant::now();
    state.set_connection(ConnectionState {
        driver: DriverState::ServiceUnavailable,
        ..ConnectionState::default()
    });
    assert_eq!(
        state.activate(SettingsAction::OpenAppleDevicesStore, now),
        Some(SettingsEffect::OpenAppleDevicesStore)
    );
    assert_eq!(
        state.activate(SettingsAction::InstallDriver, now),
        Some(SettingsEffect::InstallDriver)
    );
    assert_eq!(state.connection().driver, DriverState::Installing);
    assert_eq!(state.activate(SettingsAction::RequestPair, now), None);
    assert_eq!(state.activate(SettingsAction::InstallDriver, now), None);
}

#[test]
fn help_is_offered_only_while_stuck() {
    let fixture = Fixture::new();
    let mut state = SettingsState::load(fixture.path(), "");
    let now = Instant::now();
    assert!(!state.connection().needs_help());
    for phase in [
        ConnectionPhase::Waiting,
        ConnectionPhase::DeveloperMode,
        ConnectionPhase::RetryRequired,
        ConnectionPhase::Error,
    ] {
        state.set_connection(ConnectionState {
            phase,
            ..ConnectionState::default()
        });
        assert!(state.connection().needs_help(), "{phase:?}");
    }
    state.set_connection(ConnectionState {
        phase: ConnectionPhase::Connecting,
        driver: DriverState::ServiceUnavailable,
        ..ConnectionState::default()
    });
    assert!(!state.connection().needs_help());
    state.set_connection(ConnectionState {
        driver: DriverState::ServiceUnavailable,
        ..ConnectionState::default()
    });
    assert!(state.connection().needs_help());
    state.set_connection(ConnectionState {
        detail: Some(message!("请先解锁 iPhone，再点「连接 iPhone」")),
        ..ConnectionState::default()
    });
    assert!(state.connection().needs_help());
    assert_eq!(
        state.activate(SettingsAction::CopyDiagnostics, now),
        Some(SettingsEffect::CopyDiagnostics)
    );
    assert_eq!(
        state.activate(SettingsAction::ReportIssue, now),
        Some(SettingsEffect::ReportIssue)
    );
}
