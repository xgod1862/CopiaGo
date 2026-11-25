//! Connection and settings state for the popup. The application executes platform and
//! device effects after activation and feeds their results back to this state.

use std::{
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

use copiago_core::{
    DeviceInfo,
    ddi::{DdiStage, Progress},
    i18n::{Language, LanguagePreference, Message, message},
    settings::{FILE_ASK_MB_CHOICES, PULL_TYPES, PUSH_TYPES, Settings, SettingsChange, SyncKind},
};

mod view;
pub use view::{ControlInfo, ControlRole, SettingsLayout, SettingsScreen, SettingsView};

const FORGET_WINDOW: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingSwitch {
    Pull,
    Push,
    History,
    PcHistory,
    Startup,
    Notifications,
    UpdateCheck,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncDirection {
    ToPc,
    ToPhone,
}

impl SyncDirection {
    pub fn choices(self) -> &'static [SyncKind] {
        match self {
            Self::ToPc => &PULL_TYPES,
            Self::ToPhone => &PUSH_TYPES,
        }
    }

    pub fn selected(self, settings: &Settings) -> &[SyncKind] {
        match self {
            Self::ToPc => &settings.pull_types,
            Self::ToPhone => &settings.push_types,
        }
    }
}

/// A control's stable identity and the action sent by input or accessibility.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsAction {
    Toggle(SettingSwitch),
    ChooseLanguage(LanguagePreference),
    ToggleType(SyncDirection, SyncKind),
    ChooseFileAskMb(u8),
    RequestPair,
    Retry,
    ForgetDevice,
    InstallDriver,
    OpenAppleDevicesStore,
    FinishPairing,
    OpenUpdate,
    /// Copy a diagnostic report to the clipboard without syncing it to the iPhone.
    CopyDiagnostics,
    /// Copy the report and open a prefilled GitHub issue form.
    ReportIssue,
    OpenSettings,
    Back,
    Close,
}

/// Work for the host after a successful activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsEffect {
    /// The new setting is already on disk. Apply any live effects to the tray,
    /// clipboard watcher, history, or update checker using the updated Settings.
    SettingChanged(SettingsChange),
    /// Use the host's Run registry adapter, then call complete_startup.
    SetStartupEnabled(bool),
    RequestPair,
    Retry,
    /// Send the forget command. Call complete_forget when it has succeeded or failed.
    ForgetDevice,
    InstallDriver,
    OpenAppleDevicesStore,
    ShowHistory,
    ShowPairingGuide,
    OpenUpdate,
    CopyDiagnostics,
    ReportIssue,
    ShowSettings,
    Close,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StartupState {
    #[default]
    Unavailable,
    Available {
        enabled: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DriverState {
    #[default]
    Ready,
    Missing,
    /// Installed support is unreachable or failed to enumerate devices.
    ServiceUnavailable,
    /// Windows sees the phone on USB, but Apple's USB driver is not working for it.
    UsbDriver,
    /// Only the Microsoft Store edition of Apple Devices is installed and its device
    /// process is not running or not answering. Opening the app starts it again.
    AppleDevicesApp,
    Installing,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConnectionPhase {
    #[default]
    Setup,
    Pairing,
    Connecting,
    Waiting,
    DeveloperMode,
    RetryRequired,
    Connected {
        wireless: bool,
    },
    Error,
}

impl ConnectionPhase {
    pub fn busy(self) -> bool {
        matches!(self, Self::Pairing | Self::Connecting)
    }

    pub fn can_continue(self) -> bool {
        matches!(self, Self::DeveloperMode | Self::RetryRequired)
    }
}

/// Snapshot supplied by the host from device status, prompts, and setup progress.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConnectionState {
    pub phase: ConnectionPhase,
    /// Defer translation until rendering so a visible prompt follows the language setting.
    pub detail: Option<Message>,
    /// Set during the current busy phase and clear when the operation moves on.
    pub progress: Option<(DdiStage, Progress)>,
    /// Some (possibly with empty fields) after a device is paired. On the first
    /// setup, supply this and first_connection together when the connection succeeds.
    pub device: Option<DeviceInfo>,
    pub first_connection: bool,
    pub driver: DriverState,
}

impl ConnectionState {
    /// A saved pairing can have an unfinished first connection after an upgrade.
    /// Let the user start interactive recovery while ordinary reconnects continue.
    pub fn can_retry(&self) -> bool {
        self.phase.can_continue()
            || (self.device.is_some() && self.phase == ConnectionPhase::Waiting)
    }

    /// Stuck states in which the guide offers to copy diagnostics or report an issue.
    /// The first prompt to connect a phone is not stuck; a changed Setup detail is.
    pub fn needs_help(&self) -> bool {
        if self.phase.busy() || self.driver == DriverState::Installing {
            return false;
        }
        let failed_setup = self.phase == ConnectionPhase::Setup
            && self
                .detail
                .as_ref()
                .is_some_and(|detail| *detail != self.default_detail());
        failed_setup
            || self.driver != DriverState::Ready
            || matches!(
                self.phase,
                ConnectionPhase::Waiting
                    | ConnectionPhase::DeveloperMode
                    | ConnectionPhase::RetryRequired
                    | ConnectionPhase::Error
            )
    }

    pub fn pairing_complete(&self) -> bool {
        self.first_connection && self.device.is_some()
    }

    pub fn show_guide(&self) -> bool {
        self.device.is_none() || self.pairing_complete() || self.repairing_paired()
    }

    /// A paired phone whose explicit retry stopped on the Apple service or driver. The
    /// guide holds the repair controls, while the device card and its Forget action stay
    /// available in Settings.
    pub fn repairing_paired(&self) -> bool {
        self.device.is_some()
            && self.phase == ConnectionPhase::RetryRequired
            && self.driver != DriverState::Ready
    }

    pub fn default_detail(&self) -> Message {
        if let Some((stage, _)) = self.progress
            && self.phase.busy()
        {
            return Message::new(match stage {
                DdiStage::Download => "正在下载 iPhone 开发组件…",
                DdiStage::Sign => "正在请求 Apple 签名…",
                DdiStage::Install => "正在将开发者镜像安装到 iPhone…",
            });
        }
        Message::new(match self.phase {
            ConnectionPhase::Setup => "连接并信任你的 iPhone",
            ConnectionPhase::Pairing | ConnectionPhase::Connecting => "正在连接 iPhone…",
            ConnectionPhase::Connected { wireless: true } => "已连接（无线）",
            ConnectionPhase::Connected { wireless: false } => {
                "已连接（USB） · 可以拔掉数据线，之后将通过无线连接"
            }
            ConnectionPhase::DeveloperMode => {
                "需要开启开发者模式：在 iPhone「设置 → 隐私与安全性 → 开发者模式」中开启并重启手机，解锁后点「继续连接」"
            }
            ConnectionPhase::RetryRequired => {
                "连接未完成：请保持 iPhone 解锁并连着数据线，然后点「继续连接」"
            }
            ConnectionPhase::Waiting | ConnectionPhase::Error => "未连接",
        })
    }
}

/// State lasting across popup hides and scene rebuilds.
pub struct SettingsState {
    data_dir: PathBuf,
    settings: Settings,
    version: String,
    available_version: Option<String>,
    connection: ConnectionState,
    startup: StartupState,
    retry_hint: Option<Message>,
    diagnosis: Option<Message>,
    repair_error: Option<Message>,
    notice: Option<Message>,
    forget_until: Option<Instant>,
    forgetting: bool,
    after_driver: Option<SettingsAction>,
}

impl SettingsState {
    /// Load from the directory already resolved by the application.
    pub fn load(data_dir: impl Into<PathBuf>, version: impl Into<String>) -> Self {
        let data_dir = data_dir.into();
        Self {
            settings: Settings::load(&data_dir),
            data_dir,
            version: version.into(),
            available_version: None,
            connection: ConnectionState::default(),
            startup: StartupState::default(),
            retry_hint: None,
            diagnosis: None,
            repair_error: None,
            notice: None,
            forget_until: None,
            forgetting: false,
            after_driver: None,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Re-read after a history-page action or another host-owned settings write.
    pub fn reload(&mut self) {
        self.settings = Settings::load(&self.data_dir);
        if !self.settings.update_check {
            self.available_version = None;
        }
    }

    pub fn language(&self) -> Language {
        self.settings.language.resolve()
    }

    pub fn forgetting(&self) -> bool {
        self.forgetting
    }

    pub fn connection(&self) -> &ConnectionState {
        &self.connection
    }

    pub fn set_connection(&mut self, connection: ConnectionState) {
        if self.connection.device != connection.device {
            self.cancel_confirmation();
        }
        match connection.phase {
            ConnectionPhase::Waiting if connection.device.is_some() => {
                self.retry_hint = connection.detail.clone();
            }
            ConnectionPhase::Connecting if connection.device.is_some() => {}
            _ => self.retry_hint = None,
        }
        if matches!(connection.phase, ConnectionPhase::Connected { .. }) {
            self.diagnosis = None;
            self.repair_error = None;
        }
        self.connection = connection;
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn available_version(&self) -> Option<&str> {
        self.available_version.as_deref()
    }

    pub fn set_available_version(&mut self, version: Option<String>) {
        // A result from a cancelled check must not bring the update link back.
        self.available_version = version.filter(|_| self.settings.update_check);
    }

    pub fn notice(&self) -> Option<&Message> {
        self.notice.as_ref()
    }

    pub fn retry_hint(&self) -> Option<&Message> {
        self.retry_hint.as_ref()
    }

    pub fn diagnosis(&self) -> Option<&Message> {
        self.diagnosis.as_ref()
    }

    pub fn set_diagnosis(&mut self, notice: Option<Message>) {
        if self.diagnosis != notice {
            self.repair_error = None;
        }
        self.diagnosis = notice;
    }

    pub fn repair_error(&self) -> Option<&Message> {
        self.repair_error.as_ref()
    }

    pub fn set_notice(&mut self, notice: Option<Message>) {
        self.notice = notice;
    }

    /// Consume the connection action requested with this job. Forgetting a phone revokes it.
    pub fn complete_driver(
        &mut self,
        driver: DriverState,
        notice: Message,
    ) -> Option<SettingsAction> {
        self.connection.driver = driver;
        self.notice = None;
        if driver == DriverState::Ready {
            self.diagnosis = None;
            self.repair_error = None;
            self.notice = Some(notice);
        } else {
            self.repair_error = Some(notice);
        }
        self.after_driver
            .take()
            .filter(|action| driver == DriverState::Ready && self.can_activate(*action))
    }

    pub fn startup(&self) -> StartupState {
        self.startup
    }

    pub fn set_startup(&mut self, state: StartupState) {
        self.startup = state;
    }

    /// After a failed write, the host can supply the registry state it read
    /// back. Passing the attempted error preserves the last known value.
    pub fn complete_startup(&mut self, result: io::Result<StartupState>) {
        match result {
            Ok(state) => self.startup = state,
            Err(_) => self.notice = Some(message!("无法更改开机启动")),
        }
    }

    /// Remove the device card only after the device worker confirms deletion.
    /// The host opens the pairing guide on success.
    pub fn complete_forget(&mut self, success: bool) {
        if !std::mem::take(&mut self.forgetting) {
            return;
        }
        self.cancel_confirmation();
        if success {
            self.after_driver = None;
            self.retry_hint = None;
            self.diagnosis = None;
            self.repair_error = None;
            self.connection = ConnectionState {
                detail: Some(message!("已忘记 iPhone。请用数据线连接后重新配对")),
                driver: self.connection.driver,
                ..ConnectionState::default()
            };
            self.notice = None;
        } else {
            self.notice = Some(message!("无法删除配对记录"));
        }
    }

    pub fn switch(&self, setting: SettingSwitch) -> (bool, bool) {
        let checked = match setting {
            SettingSwitch::Pull => self.settings.pull,
            SettingSwitch::Push => self.settings.push,
            SettingSwitch::History => self.settings.history,
            SettingSwitch::PcHistory => self.settings.pc_history,
            SettingSwitch::Startup => {
                return match self.startup {
                    StartupState::Unavailable => (false, false),
                    StartupState::Available { enabled } => (enabled, true),
                };
            }
            SettingSwitch::Notifications => self.settings.notifications,
            SettingSwitch::UpdateCheck => self.settings.update_check,
        };
        (checked, true)
    }

    pub fn confirmation_deadline(&self) -> Option<Instant> {
        self.forget_until
    }

    pub fn confirming_forget(&self, now: Instant) -> bool {
        self.forget_until.is_some_and(|until| now < until)
    }

    /// Call from the one-shot timer for the displayed confirmation.
    pub fn expire_confirmation(&mut self, now: Instant) -> bool {
        let expired = self.forget_until.is_some_and(|until| now >= until);
        if expired {
            self.cancel_confirmation();
        }
        expired
    }

    /// Call when hiding the popup or navigating away without a page action.
    pub fn cancel_confirmation(&mut self) {
        self.forget_until = None;
    }

    pub fn can_activate(&self, action: SettingsAction) -> bool {
        match action {
            SettingsAction::Toggle(SettingSwitch::Startup) => {
                matches!(self.startup, StartupState::Available { .. })
            }
            SettingsAction::ChooseLanguage(preference) => preference != self.settings.language,
            SettingsAction::ChooseFileAskMb(value) => {
                FILE_ASK_MB_CHOICES.contains(&value) && value != self.settings.file_ask_mb
            }
            SettingsAction::ToggleType(direction, kind) => {
                let selected = direction.selected(&self.settings);
                direction.choices().contains(&kind)
                    && (selected.len() > 1 || !selected.contains(&kind))
            }
            SettingsAction::RequestPair => {
                !self.forgetting
                    && self.connection.driver != DriverState::Installing
                    && self.connection.device.is_none()
                    && !self.connection.phase.busy()
                    && !self.connection.phase.can_continue()
            }
            SettingsAction::Retry => {
                !self.forgetting
                    && self.connection.driver != DriverState::Installing
                    && self.connection.can_retry()
            }
            SettingsAction::ForgetDevice => self.connection.device.is_some() && !self.forgetting,
            SettingsAction::InstallDriver => {
                !self.forgetting
                    && !self.connection.phase.busy()
                    && matches!(
                        self.connection.driver,
                        DriverState::Missing
                            | DriverState::AppleDevicesApp
                            | DriverState::ServiceUnavailable
                            | DriverState::UsbDriver
                            | DriverState::Failed
                    )
            }
            SettingsAction::OpenAppleDevicesStore => matches!(
                self.connection.driver,
                DriverState::Missing
                    | DriverState::Failed
                    | DriverState::ServiceUnavailable
                    | DriverState::UsbDriver
            ),
            SettingsAction::FinishPairing => self.connection.pairing_complete(),
            SettingsAction::OpenUpdate => self.available_version.is_some(),
            _ => true,
        }
    }

    /// Apply an action on the UI thread. A failed write leaves the previous
    /// settings effective and sets a localized notice for the next render.
    pub fn activate(&mut self, action: SettingsAction, now: Instant) -> Option<SettingsEffect> {
        if !self.can_activate(action) {
            return None;
        }
        self.notice = None;
        Some(match action {
            SettingsAction::Toggle(SettingSwitch::Startup) => {
                SettingsEffect::SetStartupEnabled(!self.switch(SettingSwitch::Startup).0)
            }
            SettingsAction::Toggle(setting) => {
                let value = !self.switch(setting).0;
                let change = match setting {
                    SettingSwitch::Pull => SettingsChange::Pull(value),
                    SettingSwitch::Push => SettingsChange::Push(value),
                    SettingSwitch::History => SettingsChange::History(value),
                    SettingSwitch::PcHistory => SettingsChange::PcHistory(value),
                    SettingSwitch::Notifications => SettingsChange::Notifications(value),
                    SettingSwitch::UpdateCheck => SettingsChange::UpdateCheck(value),
                    SettingSwitch::Startup => unreachable!("startup is handled by the host"),
                };
                return self.save(change);
            }
            SettingsAction::ChooseLanguage(value) => {
                return self.save(SettingsChange::Language(value));
            }
            SettingsAction::ChooseFileAskMb(value) => {
                return self.save(SettingsChange::FileAskMb(value));
            }
            SettingsAction::ToggleType(direction, kind) => {
                let current = direction.selected(&self.settings);
                let types = direction
                    .choices()
                    .iter()
                    .copied()
                    .filter(|value| current.contains(value) != (*value == kind))
                    .collect();
                return self.save(match direction {
                    SyncDirection::ToPc => SettingsChange::PullTypes(types),
                    SyncDirection::ToPhone => SettingsChange::PushTypes(types),
                });
            }
            SettingsAction::RequestPair => {
                self.start_connection(ConnectionPhase::Pairing);
                SettingsEffect::RequestPair
            }
            SettingsAction::Retry => {
                self.start_connection(ConnectionPhase::Connecting);
                SettingsEffect::Retry
            }
            SettingsAction::ForgetDevice => {
                if self.confirming_forget(now) {
                    self.cancel_confirmation();
                    self.forgetting = true;
                    SettingsEffect::ForgetDevice
                } else {
                    self.forget_until = now.checked_add(FORGET_WINDOW);
                    return None;
                }
            }
            SettingsAction::InstallDriver => {
                self.repair_error = None;
                self.after_driver = Some(
                    if self.connection.device.is_some() || self.connection.phase.can_continue() {
                        SettingsAction::Retry
                    } else {
                        SettingsAction::RequestPair
                    },
                );
                let notice = match self.connection.driver {
                    DriverState::ServiceUnavailable => {
                        message!("正在修复 Apple 连接服务，请完成 Windows 管理员授权。")
                    }
                    DriverState::UsbDriver => {
                        message!("正在修复 Apple USB 驱动，请完成 Windows 管理员授权。")
                    }
                    DriverState::AppleDevicesApp => message!("正在打开 Apple Devices…"),
                    _ => message!("正在安装 Apple 连接组件，请完成 Windows 提示。"),
                };
                self.connection.driver = DriverState::Installing;
                self.notice = Some(notice);
                SettingsEffect::InstallDriver
            }
            SettingsAction::OpenAppleDevicesStore => SettingsEffect::OpenAppleDevicesStore,
            SettingsAction::FinishPairing => {
                self.connection.first_connection = false;
                self.cancel_confirmation();
                SettingsEffect::ShowHistory
            }
            SettingsAction::OpenUpdate => SettingsEffect::OpenUpdate,
            SettingsAction::CopyDiagnostics => SettingsEffect::CopyDiagnostics,
            SettingsAction::ReportIssue => SettingsEffect::ReportIssue,
            SettingsAction::OpenSettings => {
                self.cancel_confirmation();
                SettingsEffect::ShowSettings
            }
            SettingsAction::Back => {
                self.cancel_confirmation();
                if self.connection.show_guide() {
                    SettingsEffect::ShowPairingGuide
                } else {
                    SettingsEffect::ShowHistory
                }
            }
            SettingsAction::Close => {
                self.cancel_confirmation();
                SettingsEffect::Close
            }
        })
    }

    fn start_connection(&mut self, phase: ConnectionPhase) {
        self.diagnosis = None;
        self.repair_error = None;
        self.connection.phase = phase;
        self.connection.detail = None;
        self.connection.progress = None;
    }

    fn save(&mut self, change: SettingsChange) -> Option<SettingsEffect> {
        match Settings::update(&self.data_dir, change.clone()) {
            Ok(settings) => {
                self.settings = settings;
                if !self.settings.update_check {
                    self.available_version = None;
                }
                Some(SettingsEffect::SettingChanged(change))
            }
            Err(_) => {
                self.notice = Some(Message::new(match change {
                    SettingsChange::Language(_) => "无法保存语言设置",
                    SettingsChange::History(_) | SettingsChange::PcHistory(_) => "无法保存历史设置",
                    SettingsChange::Notifications(_) => "无法保存通知设置",
                    SettingsChange::UpdateCheck(_) => "无法保存更新设置",
                    _ => "无法保存同步设置",
                }));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests;
