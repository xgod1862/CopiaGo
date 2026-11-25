use std::time::{Duration, Instant};

use copiago_core::{diagnostic, history::Source, i18n::message, settings::SettingsChange};
use copiago_device::DeviceCommand;
use copiago_ui::{
    history::{HistoryAction, StatusTone},
    settings_page::{ConnectionState, DriverState, SettingSwitch, SettingsAction, SettingsEffect},
};
use copiago_win::startup;

use super::{Host, Screen, driver, popup::Target};

const COPY_HIDE_DELAY: Duration = Duration::from_millis(450);

impl Host<'_> {
    pub(super) fn activate_target(&mut self, target: Target, now: Instant) {
        match target {
            Target::History(target) => {
                let action = self.history_page.activate(target, now);
                if let Some(action) = action {
                    self.history_action(action, now);
                }
                self.redraw(now);
            }
            Target::Settings(action) => self.settings_action(action, now),
        }
    }

    fn history_action(&mut self, action: HistoryAction, now: Instant) {
        match action {
            HistoryAction::Copy(id) => match self.copy_from_history(id) {
                Ok(()) => {
                    self.history_page.copied(id);
                    self.copy_hide_at = Some(now + COPY_HIDE_DELAY);
                    self.feedback(action);
                }
                Err(_) => self.set_status(message!("无法复制这条历史"), StatusTone::Warning),
            },
            HistoryAction::Send(id) => self.send_from_history(id),
            HistoryAction::Delete(id) => {
                if self.history.remove(id.id).is_err() {
                    self.set_status(message!("无法删除这条历史"), StatusTone::Warning);
                } else {
                    self.refresh_entries();
                }
            }
            HistoryAction::Clear(source) => {
                if self.history.clear(Some(source)).is_err() {
                    self.set_status(message!("无法清空历史"), StatusTone::Warning);
                } else {
                    self.refresh_entries();
                }
            }
            HistoryAction::Enable(source) => {
                let setting = match source {
                    Source::Phone => SettingSwitch::History,
                    Source::Pc => SettingSwitch::PcHistory,
                };
                if !self.settings.switch(setting).0 {
                    self.settings_action(SettingsAction::Toggle(setting), now);
                }
            }
            HistoryAction::RespondToOffer { id, accept } => self.answer_file_offer(id, accept),
            HistoryAction::Retry => self.settings_action(SettingsAction::Retry, now),
            HistoryAction::OpenSettings => self.navigate(Screen::Settings, now),
            HistoryAction::OpenUpdate => self.open_update(),
        }
    }

    fn settings_action(&mut self, action: SettingsAction, now: Instant) {
        let connection = self.settings.connection().clone();
        let was_available = self.settings.notice().is_some();
        if let Some(effect) = self.settings.activate(action, now) {
            match effect {
                SettingsEffect::SettingChanged(change) => self.setting_changed(change, now),
                SettingsEffect::SetStartupEnabled(enabled) => {
                    let result =
                        startup::set_startup_enabled(enabled).and_then(|()| super::read_startup());
                    self.settings.complete_startup(result);
                }
                SettingsEffect::RequestPair => {
                    self.request_connection(DeviceCommand::RequestPair, connection)
                }
                SettingsEffect::Retry => self.request_connection(DeviceCommand::Retry, connection),
                SettingsEffect::ForgetDevice => self.begin_forget(),
                SettingsEffect::InstallDriver => self.install_driver(
                    now,
                    connection.driver == DriverState::ServiceUnavailable,
                ),
                SettingsEffect::OpenAppleDevicesStore => {
                    if driver::open_store().is_err() {
                        self.settings
                            .set_notice(Some(message!("无法打开 Microsoft Store")));
                    }
                }
                SettingsEffect::ShowHistory => {
                    self.navigate(Screen::History, now);
                    return;
                }
                SettingsEffect::ShowPairingGuide => {
                    self.navigate(Screen::Guide, now);
                    return;
                }
                SettingsEffect::ShowSettings => {
                    self.navigate(Screen::Settings, now);
                    return;
                }
                SettingsEffect::OpenUpdate => self.open_update(),
                SettingsEffect::Close => {
                    self.close_popup(now);
                    return;
                }
            }
        }
        if self.popup.screen() == Screen::History
            && let Some(notice) = self.settings.notice().filter(|_| !was_available).cloned()
        {
            self.set_status(notice, StatusTone::Warning);
        }
        self.redraw(now);
    }

    fn setting_changed(&mut self, change: SettingsChange, now: Instant) {
        self.clipboard_sync.update_settings(
            self.settings.settings().clone(),
            self.clipboard.sequence_number(),
        );
        match change {
            SettingsChange::Language(_) => {
                let language = self.settings.language();
                self.history_page.set_language(language);
                self.tray.set_menu_labels(super::menu_labels(language));
            }
            SettingsChange::History(_) | SettingsChange::PcHistory(_) => self.refresh_entries(),
            SettingsChange::Notifications(enabled) => self.tray.set_notifications_enabled(enabled),
            SettingsChange::UpdateCheck(enabled) => {
                self.update.set_enabled(enabled);
                self.show_release(now);
            }
            _ => {}
        }
    }

    fn request_connection(&mut self, command: DeviceCommand, previous: ConnectionState) {
        // Saved RemotePairing records can connect over Wi-Fi without the USB
        // service. Only gate a fresh USB pairing on the local service check.
        if previous.device.is_none() && !copiago_device::has_pairing_records(&self.data_dir) {
            let driver = driver::detect();
            if driver != DriverState::Ready {
                let mut connection = previous;
                connection.driver = driver;
                self.settings.set_connection(connection);
                self.settings.set_notice(Some(if driver == DriverState::Missing {
                    message!("尚未发现 Apple 连接组件，请先安装。")
                } else {
                    message!("Apple 连接服务不可用，请先点「修复连接服务」。")
                }));
                return;
            }
        }
        if self.worker.commands.try_send(command).is_ok() {
            let connection = self.settings.connection();
            self.set_status(connection.default_detail(), StatusTone::Muted);
            diagnostic::event(format_args!(
                "event=connection_requested phase={:?}",
                self.settings.connection().phase
            ));
        } else {
            self.settings.set_connection(previous);
            self.settings
                .set_notice(Some(message!("同步进程已停止，请重新打开应用")));
        }
    }

    fn begin_forget(&mut self) {
        if self
            .worker
            .commands
            .try_send(DeviceCommand::Forget)
            .is_err()
        {
            self.settings.complete_forget(false);
            self.set_status(message!("无法删除配对记录"), StatusTone::Warning);
        } else {
            self.clipboard_sync.connected(false);
        }
    }

    pub(super) fn finish_forget(&mut self, success: bool, now: Instant) {
        if !self.settings.forgetting() {
            return;
        }
        self.settings.complete_forget(success);
        diagnostic::event(format_args!("event=forget success={success}"));
        super::sync::retire_forgotten_session(
            &mut self.clipboard_sync,
            &mut self.offer,
            &mut self.pushes,
            success,
        );
        if success {
            self.reset_connection_feedback();
            let mut connection = self.settings.connection().clone();
            connection.driver = driver::detect();
            self.settings.set_connection(connection);
            self.set_status(
                message!("已忘记 iPhone。请用数据线连接后重新配对"),
                StatusTone::Muted,
            );
            self.navigate(Screen::Guide, now);
        } else {
            let message = message!("无法删除配对记录");
            self.set_status(message, StatusTone::Warning);
        }
    }

    fn install_driver(&mut self, now: Instant, repair: bool) {
        let install = if self.isolated {
            Err(std::io::ErrorKind::PermissionDenied.into())
        } else {
            driver::DriverInstall::start(now, repair)
        };
        match install {
            Ok(install) => self.driver_install = Some(install),
            Err(error) => {
                let (state, message) = if repair {
                    (
                        DriverState::ServiceUnavailable,
                        if error.raw_os_error() == Some(1223) {
                            message!("已取消管理员授权，未修复 Apple 连接服务。")
                        } else {
                            message!("无法启动服务修复。请在 Windows 服务中启动 Apple Mobile Device Service，或修复 Apple Devices。")
                        },
                    )
                } else {
                    driver::failed_to_run()
                };
                self.set_driver_result(state, message);
            }
        }
    }

    pub(super) fn set_driver_result(
        &mut self,
        state: DriverState,
        notice: copiago_core::i18n::Message,
    ) {
        let mut connection = self.settings.connection().clone();
        connection.driver = state;
        self.settings.set_connection(connection);
        self.settings.set_notice(Some(notice));
        // A successful repair only proves service reachability. Continue through
        // the normal worker so trust, device enumeration and pairing are verified.
        if state == DriverState::Ready && self.settings.connection().device.is_none() {
            let previous = self.settings.connection().clone();
            let action = if previous.phase.can_continue() {
                SettingsAction::Retry
            } else {
                SettingsAction::RequestPair
            };
            let command = match self.settings.activate(action, Instant::now()) {
                Some(SettingsEffect::Retry) => Some(DeviceCommand::Retry),
                Some(SettingsEffect::RequestPair) => Some(DeviceCommand::RequestPair),
                _ => None,
            };
            if let Some(command) = command {
                self.request_connection(command, previous);
            }
        }
    }

    fn open_update(&mut self) {
        if self
            .update
            .release()
            .is_some_and(|release| release.open().is_err())
        {
            let warning = message!("无法打开浏览器");
            self.settings.set_notice(Some(warning.clone()));
            self.set_status(warning, StatusTone::Warning);
        }
    }
}
