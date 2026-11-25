use std::time::{Duration, Instant};

use copiago_core::{
    DeviceInfo, diagnostic,
    i18n::{Message, message},
};
use copiago_device::{ConnState, DeviceEvent};
use copiago_ui::{
    components::{ProgressState, Status},
    history::{HistoryStatus, StatusTone},
    settings_page::{ConnectionPhase, DriverState},
};
use copiago_win::tray::{NotificationKind, TrayIconState};
use windows_sys::Win32::{Foundation::SYSTEMTIME, System::SystemInformation::GetLocalTime};

use super::{Host, Screen};

const ICON_GRACE: Duration = Duration::from_secs(30);
const DISCONNECT_NOTICE: Duration = Duration::from_secs(300);
const PROGRESS_COMPLETE: Duration = Duration::from_millis(1560);

#[derive(Default)]
pub(super) struct ConnectionFeedback {
    pending_device: Option<DeviceInfo>,
    disconnected_at: Option<Instant>,
    last_connected: Option<String>,
    notice_sent: bool,
    awaiting_wireless: bool,
    just_forgot: bool,
    /// Diagnosis for the current run of "Apple service unavailable" statuses. Background
    /// reconnects repeat that status every few seconds; the check runs once per run.
    service_check: Option<(DriverState, Option<Message>)>,
    complete_until: Option<Instant>,
    icon_due: Option<Instant>,
}

impl ConnectionFeedback {
    pub fn deadline(&self, phase: ConnectionPhase) -> Option<Instant> {
        let notice = self
            .disconnected_at
            .filter(|_| !self.notice_sent && !phase.can_continue())
            .map(|at| at + DISCONNECT_NOTICE);
        [self.icon_due, notice, self.complete_until]
            .into_iter()
            .flatten()
            .min()
    }
}

impl Host<'_> {
    pub(super) fn device_event(&mut self, event: DeviceEvent, now: Instant) {
        if self.settings.forgetting() && !matches!(event, DeviceEvent::ForgetResult(_)) {
            return;
        }
        match event {
            DeviceEvent::Status(state, detail) => self.connection_status(state, detail, now),
            DeviceEvent::DeviceInfo(info) => {
                let mut connection = self.settings.connection().clone();
                if connection.device.is_some() {
                    connection.device = Some(info);
                    self.settings.set_connection(connection);
                } else {
                    self.connection.pending_device = Some(info);
                }
            }
            DeviceEvent::PairingPrompt(message) => {
                self.connection_status(ConnState::Pairing, message, now);
            }
            DeviceEvent::DeveloperServicesMissing { wireless } => {
                diagnostic::event(format_args!(
                    "event=developer_services_missing wireless={wireless}"
                ));
            }
            DeviceEvent::SetupProgress(stage, received) => {
                let mut connection = self.settings.connection().clone();
                connection.progress = Some((stage, received));
                self.settings.set_connection(connection);
                if let Some(total) = received.total_bytes {
                    diagnostic::event(format_args!(
                        "event=setup_progress stage={stage:?} bytes={} total={total}",
                        received.received_bytes
                    ));
                } else {
                    diagnostic::event(format_args!("event=setup_progress stage={stage:?}"));
                }
            }
            DeviceEvent::Clip {
                clip,
                while_disconnected,
            } => self.on_phone_copy(clip, while_disconnected),
            DeviceEvent::Unsupported { kind } => {
                self.clear_waiting_offer();
                self.set_status(
                    message!(
                        "iPhone 上复制了{kind}，目前只同步文字、图片和「文件」App 里的文件",
                        kind = kind
                    ),
                    StatusTone::Muted,
                );
            }
            DeviceEvent::FileOffer(offer) => self.on_file_offer(offer),
            DeviceEvent::FileProgress(id, progress) => self.on_file_progress(id, progress),
            DeviceEvent::FileResult(id, result) => self.on_file_result(id, result),
            DeviceEvent::PushResult(id, result) => self.on_push_result(id, result),
            DeviceEvent::ForgetResult(success) => self.finish_forget(success, now),
            DeviceEvent::Error(message) => {
                self.notify(&message, true, NotificationKind::Standard);
                self.set_status(message, StatusTone::Warning);
            }
        }
    }

    fn connection_status(&mut self, state: ConnState, detail: Message, now: Instant) {
        let detail = self.network_hint(detail);
        let mut connection = self.settings.connection().clone();
        let previous = connection.phase;
        let phase = connection_phase(state);
        connection.phase = phase;
        connection.progress = None;
        let mut show_guide = false;
        let repeated = connection.detail.as_ref() == Some(&detail);
        let mut detail = detail;
        let service_failed = detail == Message::new(copiago_device::APPLE_SERVICE_UNAVAILABLE);
        // A user-started attempt deserves a fresh check; background repeats reuse it.
        if !service_failed || previous.busy() {
            self.connection.service_check = None;
        }
        if service_failed && self.driver_install.is_none() {
            let fresh = self.connection.service_check.is_none();
            // The worker reached this state because the device list could not be read,
            // so a listening port alone is not a pass. The check can take 300 ms.
            let (driver, notice) = self
                .connection
                .service_check
                .get_or_insert_with(|| {
                    let check = super::driver::diagnose(true);
                    (check.state, check.notice)
                })
                .clone();
            if connection.device.is_none() || state == ConnState::Hold {
                if fresh {
                    // An explicit retry of a saved phone can land here after upgrading.
                    // Use the same repair controls; restore the device card on connection.
                    if let Some(device) = connection.device.take() {
                        self.connection.pending_device = Some(device);
                        connection.first_connection = false;
                        show_guide = self.popup.visible() && self.popup.screen() != Screen::Guide;
                    }
                    connection.driver = driver;
                    self.settings.set_notice(notice);
                }
            } else if notice.is_some() {
                // A paired phone reconnecting in the background. The repair controls live
                // in the guide, which "Continue" opens through the same Hold path.
                detail = message!(
                    "电脑上的 Apple 设备连接服务不可用，暂时无法通过数据线连接（不影响 Wi-Fi 连接）。需要修复时，请在设置的 iPhone 卡片上点「继续连接」，再按提示操作。"
                );
            }
        }
        if self.driver_install.is_none()
            && detail == Message::new(copiago_device::NO_USB_IPHONE)
            && !repeated
        {
            // "No iPhone" covers a charge-only cable, a missing Apple driver and a locked
            // phone alike. What Windows sees on USB tells them apart.
            let (driver, notice) = super::driver::explain_missing_iphone();
            if driver != copiago_ui::settings_page::DriverState::Ready {
                connection.driver = driver;
            }
            self.settings.set_notice(Some(notice));
        }
        if self.connection.just_forgot && state == ConnState::Setup {
            connection.detail = Some(message!("已忘记 iPhone。请用数据线连接后重新配对"));
        } else {
            self.connection.just_forgot = false;
            connection.detail = Some(detail.clone());
        }

        if let ConnectionPhase::Connected { wireless } = phase {
            connection.driver = copiago_ui::settings_page::DriverState::Ready;
            self.clipboard_sync.connected(true);
            let transport = if wireless { "wifi" } else { "usb" };
            diagnostic::event(format_args!("event=connected transport={transport}"));
            if previous.busy() {
                self.connection.complete_until = Some(now + PROGRESS_COMPLETE);
            }
            if self.connection.notice_sent {
                self.notify(
                    &message!("已重新连接 iPhone"),
                    false,
                    NotificationKind::Standard,
                );
            }
            self.last_alert = None;
            self.connection.notice_sent = false;
            self.connection.disconnected_at = None;
            self.connection.icon_due = None;
            if connection.device.is_none() {
                connection.device = Some(self.connection.pending_device.take().unwrap_or_default());
                connection.first_connection = true;
                self.connection.awaiting_wireless = !wireless;
                let message = Message::new(if wireless {
                    "iPhone 已连接成功。复制即可同步到电脑。"
                } else {
                    "iPhone 已连接成功。现在可以拔掉数据线，之后在同一 Wi-Fi 下自动连接。"
                });
                self.notify(&message, false, NotificationKind::Onboarding);
                show_guide = !self.popup.visible();
            } else if wireless && self.connection.awaiting_wireless {
                self.connection.awaiting_wireless = false;
                self.notify(
                    &message!("已通过 Wi-Fi 连接 iPhone，在手机上复制即可同步到电脑"),
                    false,
                    NotificationKind::Onboarding,
                );
            }
        } else {
            self.clipboard_sync.connected(false);
            if matches!(previous, ConnectionPhase::Connected { .. }) {
                self.connection.last_connected = Some(local_minute());
                self.connection.disconnected_at = Some(now);
                self.connection.icon_due = Some(now + ICON_GRACE);
            }
            if phase == ConnectionPhase::Error
                || (phase.can_continue() && connection.device.is_some())
            {
                self.notify(&detail, true, NotificationKind::Standard);
            }
            self.connection.complete_until = None;
            if !matches!(
                phase,
                ConnectionPhase::Connecting | ConnectionPhase::Pairing
            ) {
                self.clear_waiting_offer();
            }
            diagnostic::event(format_args!("event=status state={state:?}"));
        }

        let tone = match phase {
            ConnectionPhase::Connected { .. } => StatusTone::Success,
            ConnectionPhase::Waiting
            | ConnectionPhase::DeveloperMode
            | ConnectionPhase::RetryRequired
            | ConnectionPhase::Error => StatusTone::Warning,
            ConnectionPhase::Setup if detail != message!("连接并信任你的 iPhone") => {
                StatusTone::Warning
            }
            _ => StatusTone::Muted,
        };
        let message = if matches!(
            state,
            ConnState::Connecting | ConnState::Waiting | ConnState::Error
        ) && let Some(time) = &self.connection.last_connected
        {
            message!("{status}（上次连接 {time}）", status = detail, time = time)
        } else {
            connection.detail.clone().unwrap_or(detail)
        };
        self.set_status(message, tone);
        self.settings.set_connection(connection);
        if show_guide {
            self.show(Screen::Guide, now);
        }
    }

    pub(super) fn connection_tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self.connection.icon_due.is_some_and(|at| now >= at) {
            self.connection.icon_due = None;
            changed = true;
        }
        if self.connection.complete_until.is_some_and(|at| now >= at) {
            self.connection.complete_until = None;
            changed = true;
        }
        if !self.connection.notice_sent
            && !self.settings.connection().phase.can_continue()
            && self
                .connection
                .disconnected_at
                .is_some_and(|at| now >= at + DISCONNECT_NOTICE)
        {
            self.connection.notice_sent = true;
            self.notify(
                &message!(
                    "iPhone 已断开 {minutes} 分钟，正在自动重连。请确认手机已解锁并在同一局域网",
                    minutes = DISCONNECT_NOTICE.as_secs() / 60
                ),
                false,
                NotificationKind::Standard,
            );
        }
        changed
    }

    pub(super) fn publish(&mut self) {
        let connection = self.settings.connection();
        let phase = connection.phase;
        let name = connection.device.as_ref().map(device_title).unwrap_or("");
        let detail = self
            .history_notice
            .as_ref()
            .map(|(detail, _)| detail.clone())
            .or_else(|| connection.detail.clone());
        let language = self.settings.language();
        let connected = matches!(phase, ConnectionPhase::Connected { .. });
        let icon = if connected {
            TrayIconState::Connected
        } else if phase.can_continue() || phase == ConnectionPhase::Error {
            TrayIconState::Error
        } else if self.connection.icon_due.is_some() {
            TrayIconState::Connected
        } else {
            TrayIconState::Disconnected
        };
        self.tray.set_icon_state(icon);
        let default = connection.default_detail();
        let tooltip = if matches!(
            connection.progress,
            Some((copiago_core::ddi::DdiStage::Download, _))
        ) {
            message!("正在下载 iPhone 开发组件…").render(language)
        } else {
            detail.as_ref().unwrap_or(&default).render(language)
        };
        let mut title = String::from("CopiaGo");
        if connected && !name.is_empty() {
            title.push_str(" · ");
            title.push_str(name);
        }
        if !tooltip.is_empty() {
            title.push_str(" · ");
            title.push_str(&tooltip);
        }
        self.tray.set_tooltip(&title);
        self.tray.set_has_latest(self.clipboard_sync.has_pending());
        let progress = if self.connection.complete_until.is_some() {
            ProgressState::Complete
        } else if phase.busy() {
            connection
                .progress
                .and_then(|(_, progress)| {
                    let total = progress.total_bytes.filter(|&size| size != 0)?;
                    let fraction = progress.received_bytes.min(total) as f32 / total as f32;
                    Some(ProgressState::Determinate(fraction))
                })
                .unwrap_or(ProgressState::Indeterminate)
        } else {
            ProgressState::Hidden
        };
        self.history_page.set_status(HistoryStatus {
            connection: match phase {
                ConnectionPhase::Connected { .. } => Status::Connected,
                ConnectionPhase::Pairing | ConnectionPhase::Connecting | ConnectionPhase::Setup => {
                    Status::Connecting
                }
                _ => Status::Disconnected,
            },
            device_name: name.to_owned(),
            detail,
            tone: self
                .history_notice
                .as_ref()
                .map_or(StatusTone::Muted, |(_, tone)| *tone),
            progress,
            can_retry: connection.can_retry(),
        });
        self.history_page
            .set_offer(self.offer.presentation(language));
    }

    /// While a paired phone is unreachable, names an active VPN or proxy adapter, the most
    /// common PC-side reason for a phone that never reappears on Wi-Fi.
    fn network_hint(&self, detail: Message) -> Message {
        if detail != Message::new(copiago_device::WAITING_FOR_IPHONE) {
            return detail;
        }
        let Some(adapter) = super::network::vpn_adapter() else {
            return detail;
        };
        let hint = message!(
            "等待 iPhone 连接。电脑正在使用 VPN 或代理虚拟网卡「{adapter}」，它可能拦截局域网连接；如果一直连不上，请暂时关闭它或开启局域网直连，并确认手机和电脑在同一 Wi-Fi。",
            adapter = adapter,
        );
        if self.settings.connection().detail.as_ref() != Some(&hint) {
            // The adapter name is user-chosen, so only record that one was found.
            diagnostic::event(format_args!("event=vpn_adapter_active"));
        }
        hint
    }

    pub(super) fn reset_connection_feedback(&mut self) {
        self.connection = ConnectionFeedback {
            just_forgot: true,
            ..ConnectionFeedback::default()
        };
        self.last_alert = None;
    }
}

fn connection_phase(state: ConnState) -> ConnectionPhase {
    match state {
        ConnState::Idle | ConnState::Setup => ConnectionPhase::Setup,
        ConnState::Pairing => ConnectionPhase::Pairing,
        ConnState::Waiting => ConnectionPhase::Waiting,
        ConnState::Connecting => ConnectionPhase::Connecting,
        ConnState::Developer => ConnectionPhase::DeveloperMode,
        ConnState::Connected { wireless } => ConnectionPhase::Connected { wireless },
        ConnState::Hold => ConnectionPhase::RetryRequired,
        ConnState::Unsupported | ConnState::Error => ConnectionPhase::Error,
    }
}

fn device_title(device: &DeviceInfo) -> &str {
    [&*device.name, &*device.model]
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or("iPhone")
}

fn local_minute() -> String {
    let mut time = std::mem::MaybeUninit::<SYSTEMTIME>::uninit();
    // SAFETY: GetLocalTime initializes the supplied SYSTEMTIME.
    let time = unsafe {
        GetLocalTime(time.as_mut_ptr());
        time.assume_init()
    };
    format!("{:02}:{:02}", time.wHour, time.wMinute)
}
