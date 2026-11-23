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
pub use view::{ControlInfo, ControlRole, SettingsScreen, SettingsView};

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
    pub fn pairing_complete(&self) -> bool {
        self.first_connection && self.device.is_some()
    }

    pub fn show_guide(&self) -> bool {
        self.device.is_none() || self.pairing_complete()
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
    notice: Option<Message>,
    forget_until: Option<Instant>,
    forgetting: bool,
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
            notice: None,
            forget_until: None,
            forgetting: false,
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

    pub fn set_notice(&mut self, notice: Option<Message>) {
        self.notice = notice;
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
                    && self.connection.device.is_none()
                    && !self.connection.phase.busy()
                    && !self.connection.phase.can_continue()
            }
            SettingsAction::Retry => !self.forgetting && self.connection.phase.can_continue(),
            SettingsAction::ForgetDevice => self.connection.device.is_some() && !self.forgetting,
            SettingsAction::InstallDriver => {
                matches!(
                    self.connection.driver,
                    DriverState::Missing | DriverState::Failed
                )
            }
            SettingsAction::OpenAppleDevicesStore => self.connection.driver == DriverState::Failed,
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
                self.connection.driver = DriverState::Installing;
                self.notice = Some(message!("正在安装 Apple 连接组件，请完成 Windows 提示。"));
                SettingsEffect::InstallDriver
            }
            SettingsAction::OpenAppleDevicesStore => SettingsEffect::OpenAppleDevicesStore,
            SettingsAction::FinishPairing => {
                self.connection.first_connection = false;
                self.cancel_confirmation();
                SettingsEffect::ShowHistory
            }
            SettingsAction::OpenUpdate => SettingsEffect::OpenUpdate,
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
