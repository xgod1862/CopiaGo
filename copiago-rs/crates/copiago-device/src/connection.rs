//! Transport policy and lifecycle of the device worker.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::{Future, pending},
    ops::ControlFlow,
    path::Path,
    time::Duration,
};

use copiago_core::{
    DeviceInfo,
    clip::{Clip, FileOffer},
    ddi::DdiStage,
    diagnostic,
    i18n::{Language, LanguagePreference, Message, message},
};
use idevice::usbmuxd::UsbmuxdAddr;
use tokio::{
    sync::mpsc,
    time::{sleep, timeout},
};

use crate::{
    ConnState, DeviceCommand, DeviceEvent, FileTransferError, PasteboardError, UsbProbeError,
    WifiProbeError,
    pasteboard::PasteboardState,
    usb::{PairingError, listen_usb_detach},
    wifi::PairedDevice,
};

mod clipboard_queue;
mod native;
#[cfg(test)]
mod tests;

use clipboard_queue::ClipboardQueue;

/// Status while a paired phone is not reachable. The host recognizes it to add hints
/// about the PC's network, such as an active VPN adapter.
pub const WAITING_FOR_IPHONE: &str = "等待 iPhone 连接；请检查解锁、信任及同一局域网";
const RETRY_DELAY: Duration = Duration::from_secs(4);
// After this many consecutive misses the phone is probably away. Stop blind polling and
// wait for its network announcement, with a slow timer as a safety net.
const FAST_RETRIES: u32 = 6;
const IDLE_RETRY: Duration = Duration::from_secs(30);
const MULTIPLE_RECORDS_DELAY: Duration = Duration::from_secs(5);
const PAIRING_TIMEOUT: Duration = Duration::from_secs(120);
// Budget a maximum-size Windows PNG plus room for queue entries and small copies.
const MAX_QUEUED_PUSH_BYTES: usize = 64 * 1024 * 1024 + 64 * 1024;

/// Power and network notifications provided by the desktop host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostSignal {
    Resumed,
    NetworkChanged,
}

/// Asynchronous source of host notifications. Pending reads must be cancellation-safe.
pub trait HostSignals {
    /// Returns None when this source is closed.
    fn next_signal(&mut self) -> impl Future<Output = Option<HostSignal>> + Send;
}

impl HostSignals for mpsc::Receiver<HostSignal> {
    async fn next_signal(&mut self) -> Option<HostSignal> {
        self.recv().await
    }
}

impl HostSignals for () {
    async fn next_signal(&mut self) -> Option<HostSignal> {
        None
    }
}

/// Language and setup options for the device worker.
#[derive(Clone, Copy, Debug)]
pub struct DeviceRunOptions {
    pub language: Language,
    /// Format generated file names using the saved preference present when the offer arrives.
    pub follow_saved_language: bool,
    /// Allows pairing, forgetting, saving device details and migrating existing records.
    /// Set false for a read-only probe.
    pub persist_device_data: bool,
    /// Allows revealing the Developer Mode setting and preparing developer images
    /// when persistent device data is enabled.
    pub allow_developer_setup: bool,
}

impl Default for DeviceRunOptions {
    fn default() -> Self {
        Self {
            language: LanguagePreference::System.resolve(),
            follow_saved_language: false,
            persist_device_data: true,
            allow_developer_setup: true,
        }
    }
}

/// Runs the device worker on a Tokio runtime with I/O and time enabled.
///
/// Pass the CopiaGo data root. The worker owns one session at a time and resumes
/// discovery after a transport failure, desktop resume or network change.
/// Shutdown, a closed command channel, or a callback break stops the worker.
/// The worker handles USB pairing and forgetting in command order.
/// Pending pushes are processed in order. If their byte budget is exhausted,
/// new pushes receive a `PushResult` with `PasteboardError::PushBusy`.
/// A file download runs only on an explicit `FetchOffer` command.
pub async fn run_device(
    data_dir: &Path,
    options: DeviceRunOptions,
    commands: &mut mpsc::Receiver<DeviceCommand>,
    signals: impl HostSignals,
    emit: impl Fn(DeviceEvent) -> ControlFlow<()>,
) {
    let addr = UsbmuxdAddr::default();
    let backend = native::NativeDevice {
        data_dir,
        addr: &addr,
        options,
        cable_attached: Cell::new(false),
    };
    let (detached, events) = mpsc::channel(16);
    let mut inputs = Inputs {
        commands,
        signals: Some(signals),
        detached: Some(events),
    };
    tokio::select! {
        () = run(&backend, &mut inputs, &emit) => {}
        () = listen_usb_detach(&addr, detached) => {}
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Transport {
    Wifi,
    Usb(u32),
}

trait Backend {
    fn paired_device(&self) -> Result<PairedDevice, WifiProbeError>;
    async fn pair(
        &self,
        prompt: impl Fn(Message) -> ControlFlow<()>,
    ) -> Result<DeviceInfo, PairingError>;
    fn forget(&self) -> std::io::Result<()>;
    /// Request USB trust from this paired phone without replacing its wireless record.
    async fn renew_usb_trust(&self, device: &PairedDevice) -> Result<(), PairingError>;
    async fn usb_id(&self, device: &PairedDevice) -> Result<Option<u32>, UsbProbeError>;
    /// Told before each Wi-Fi attempt whether the paired phone is also on USB.
    fn set_cable_attached(&self, _attached: bool) {}
    /// Resolves when the paired phone reappears on the network. The default never does.
    async fn advertisement(&self, _device: &PairedDevice) {
        pending::<()>().await;
    }
    async fn reveal_developer_menu(
        &self,
        device: &PairedDevice,
        id: u32,
    ) -> Result<(), UsbProbeError>;
    async fn session(
        &self,
        device: &PairedDevice,
        transport: Transport,
        clipboard: &mut PasteboardState,
        commands: &ClipboardQueue,
        emit: impl Fn(DeviceEvent) -> ControlFlow<()>,
    ) -> Result<(), Failure>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Failure {
    Wifi(WifiProbeError),
    Usb(UsbProbeError),
    Session,
    Trust(PairingError),
    DeveloperModeRequired,
    DeveloperMenuUnavailable,
    Setup { stage: DdiStage, timed_out: bool },
}

impl Failure {
    fn requires_developer_mode(self) -> bool {
        matches!(
            self,
            Self::DeveloperModeRequired | Self::Usb(UsbProbeError::DeveloperModeRequired)
        )
    }

    const fn setup(stage: DdiStage) -> Self {
        Self::Setup {
            stage,
            timed_out: false,
        }
    }

    fn allows_usb(self) -> bool {
        !matches!(
            self,
            Self::Setup {
                stage: DdiStage::Download | DdiStage::Sign,
                ..
            }
        )
    }

    fn after_usb(self, usb: Self) -> Self {
        if matches!(
            self,
            Self::DeveloperModeRequired
                | Self::Setup {
                    stage: DdiStage::Install,
                    ..
                }
        ) && !matches!(
            usb,
            Self::Setup { .. }
                | Self::DeveloperModeRequired
                | Self::Usb(UsbProbeError::DeveloperModeRequired)
        ) {
            return self;
        }
        usb
    }

    /// Used after dedicated developer-mode and image errors have been handled.
    /// A Wi-Fi failure reaches this branch only when no matching USB phone was found.
    fn first_connection_hint(self) -> &'static str {
        match self {
            // This status also activates the host's Apple service repair controls.
            Self::Usb(UsbProbeError::Usbmuxd) => crate::usb::APPLE_SERVICE_UNAVAILABLE,
            Self::Usb(UsbProbeError::DeviceLocked) => {
                "iPhone 已锁定。请解锁并停留在主屏幕，然后点「继续连接」。"
            }
            Self::Usb(UsbProbeError::Timeout) => {
                "USB 连接超时。请把 iPhone 直接插在电脑机身的 USB 口（不要经过扩展坞或延长线），保持解锁，再点「继续连接」。"
            }
            Self::Usb(UsbProbeError::CoreDeviceProxy) => {
                "已找到 iPhone，但开发者通道未能打开。请解锁手机，重新插拔数据线后重试。"
            }
            Self::Usb(UsbProbeError::Rsd) => {
                "已连到 iPhone，但未能确认手机提供的服务。请保持解锁，重新插拔数据线后重试。"
            }
            Self::Usb(UsbProbeError::Lockdown) => {
                "已找到 iPhone，但 USB 会话未能建立。请重新插拔数据线并保持手机解锁。"
            }
            Self::Usb(UsbProbeError::NoUsbDevice) => {
                "USB 已检测不到这台 iPhone。请重新接好数据线，再点「继续连接」。"
            }
            Self::Usb(UsbProbeError::MultipleUsbDevices(_)) => "请暂时只连接一台 iPhone",
            Self::Trust(PairingError::Denied) => {
                "iPhone 拒绝了信任请求。请重新插拔数据线，点「继续连接」，在手机上点「信任」。"
            }
            Self::Trust(PairingError::Pending) => {
                "手机尚未确认信任。请在 iPhone 上点「信任」并输入密码，然后点「继续连接」。"
            }
            Self::Trust(_)
            | Self::Usb(UsbProbeError::PairingRecordUnavailable | UsbProbeError::TrustRequired) => {
                "USB 信任未能恢复。请解锁 iPhone，重新插拔数据线，再点「继续连接」。"
            }
            Self::Wifi(WifiProbeError::NoEndpoint | WifiProbeError::Discovery) => {
                "无线未找到这台 iPhone，USB 也未检测到它。请重新插紧数据线或换一个电脑 USB 口。"
            }
            Self::Wifi(_) => {
                "无线连接失败，USB 也未检测到这台 iPhone。请将手机直接接到电脑后重试。"
            }
            _ => {
                "与 iPhone 的连接中断。请插紧数据线并保持手机解锁，再点「继续连接」；反复断开时请换一个 USB 口或数据线。"
            }
        }
    }

    fn wait(self, first_setup: bool) -> Wait {
        let (mode, state, message) = match self {
            Self::DeveloperMenuUnavailable => (
                WaitMode::User,
                ConnState::Hold,
                Message::new(crate::usb::DEVELOPER_MENU_RETRY),
            ),
            Self::DeveloperModeRequired | Self::Usb(UsbProbeError::DeveloperModeRequired) => (
                WaitMode::User,
                ConnState::Developer,
                message!(
                    "需要开启开发者模式：在 iPhone「设置 → 隐私与安全性 → 开发者模式」中开启并重启手机，解锁后点「继续连接」"
                ),
            ),
            Self::Setup { stage, timed_out } => (
                WaitMode::User,
                ConnState::Hold,
                Message::new(match (stage, timed_out) {
                    (DdiStage::Download, false) => {
                        "下载 iPhone 开发组件失败。已下载的部分会保留，请检查电脑网络后点「继续连接」。"
                    }
                    (DdiStage::Download, true) => {
                        "下载 iPhone 开发组件超时。已下载的部分会保留，请检查电脑网络后点「继续连接」。"
                    }
                    (DdiStage::Sign, false) => "Apple 签名失败。请稍后重试。",
                    (DdiStage::Sign, true) => "请求 Apple 签名超时。请检查电脑网络后重试。",
                    (DdiStage::Install, false) => {
                        "无法在 iPhone 上安装开发组件。请保持手机解锁并连着数据线后重试。"
                    }
                    (DdiStage::Install, true) => {
                        "安装 iPhone 开发组件超时。请保持手机解锁并连着数据线后重试。"
                    }
                }),
            ),
            error if first_setup => (
                WaitMode::User,
                ConnState::Hold,
                Message::new(error.first_connection_hint()),
            ),
            _ => (
                WaitMode::Reconnect,
                ConnState::Waiting,
                Message::new(match self {
                    Self::Usb(UsbProbeError::Usbmuxd) => crate::usb::APPLE_SERVICE_UNAVAILABLE,
                    Self::Usb(UsbProbeError::DeviceLocked) => {
                        "请先解锁 iPhone，再点「连接 iPhone」"
                    }
                    _ => WAITING_FOR_IPHONE,
                }),
            ),
        };
        Wait {
            mode,
            state,
            message,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum WaitMode {
    Setup,
    User,
    Reconnect,
    MultipleRecords,
}

impl WaitMode {
    async fn timer(self) {
        match self {
            Self::Setup | Self::User => pending().await,
            Self::Reconnect => sleep(RETRY_DELAY).await,
            Self::MultipleRecords => sleep(MULTIPLE_RECORDS_DELAY).await,
        }
    }
}

struct Wait {
    mode: WaitMode,
    state: ConnState,
    message: Message,
}

enum Interrupted {
    Stop,
    Forget,
    Pair,
    UserRetry,
    Reconnect,
    UsbDetached,
}

#[derive(Default)]
struct DeviceState {
    serial: Option<String>,
    clipboard: PasteboardState,
    first_setup: bool,
    /// Consecutive reconnect waits since the last ready session.
    misses: u32,
}

impl DeviceState {
    fn select(&mut self, device: &PairedDevice) {
        if self.serial.as_deref() != Some(&device.serial) {
            self.serial = Some(device.serial.clone());
            self.clipboard = PasteboardState::default();
        }
    }

    fn forget(&mut self) {
        *self = Self {
            first_setup: true,
            ..Self::default()
        };
    }
}

async fn run(
    backend: &impl Backend,
    inputs: &mut Inputs<'_, impl HostSignals>,
    emit: &impl Fn(DeviceEvent) -> ControlFlow<()>,
) {
    let mut state = DeviceState::default();
    loop {
        match connect_or_wait(backend, inputs, emit, &mut state).await {
            Ok(()) | Err(Interrupted::Reconnect) => {}
            // Legacy records do not distinguish a completed first connection.
            // An explicit retry opts into USB recovery even after restarting the app.
            Err(Interrupted::UserRetry) => state.first_setup = true,
            Err(Interrupted::Forget) => {
                let success = backend.forget().is_ok();
                if success {
                    state.forget();
                }
                if emit(DeviceEvent::ForgetResult(success)).is_break() {
                    return;
                }
            }
            Err(Interrupted::Stop) => return,
            Err(Interrupted::Pair) => unreachable!("pair requests handled during setup"),
            Err(Interrupted::UsbDetached) => unreachable!("USB detach handled by its session"),
        }
    }
}

async fn connect_or_wait(
    backend: &impl Backend,
    inputs: &mut Inputs<'_, impl HostSignals>,
    emit: &impl Fn(DeviceEvent) -> ControlFlow<()>,
    state: &mut DeviceState,
) -> Result<(), Interrupted> {
    let pairing = backend.paired_device();
    let records = match &pairing {
        Ok(_) => "1",
        Err(WifiProbeError::NoPairingRecord) => "0",
        Err(WifiProbeError::MultiplePairingRecords) => "2+",
        Err(_) => "unavailable",
    };
    diagnostic::event(format_args!("paired_records={records}"));
    let mut paired = None;
    let wait = match pairing {
        Ok(device) => {
            state.select(&device);
            let error = connect_cycle(backend, inputs, emit, state, &device).await?;
            paired = Some(device);
            error.wait(state.first_setup)
        }
        Err(WifiProbeError::NoPairingRecord) => {
            state.forget();
            return setup(backend, inputs, emit).await;
        }
        Err(WifiProbeError::MultiplePairingRecords) => Wait {
            mode: WaitMode::MultipleRecords,
            state: ConnState::Error,
            message: message!("已配对多台 iPhone。请在设置中点「忘记此 iPhone」后重新配对"),
        },
        Err(error) => Wait {
            state: ConnState::Setup,
            ..Failure::Wifi(error).wait(false)
        },
    };
    diagnostic::event(format_args!(
        "event=wait mode={:?} state={:?}",
        wait.mode, wait.state
    ));
    emit_event(emit, DeviceEvent::Status(wait.state, wait.message))?;
    let idle = match (wait.mode, &paired) {
        (WaitMode::Reconnect, Some(device)) => {
            state.misses = state.misses.saturating_add(1);
            (state.misses > FAST_RETRIES).then_some(device)
        }
        _ => None,
    };
    let timer = async {
        let Some(device) = idle else {
            return wait.mode.timer().await;
        };
        sleep(RETRY_DELAY).await;
        tokio::select! {
            () = backend.advertisement(device) => {
                diagnostic::event(format_args!("event=advertisement_seen"));
            }
            () = sleep(IDLE_RETRY - RETRY_DELAY) => {}
        }
    };
    inputs.drive(timer, Phase::Waiting(wait.mode), emit).await
}

async fn setup(
    backend: &impl Backend,
    inputs: &mut Inputs<'_, impl HostSignals>,
    emit: &impl Fn(DeviceEvent) -> ControlFlow<()>,
) -> Result<(), Interrupted> {
    let mut message = message!("连接并信任你的 iPhone");
    loop {
        emit_event(emit, DeviceEvent::Status(ConnState::Setup, message))?;
        match inputs
            .drive(
                WaitMode::Setup.timer(),
                Phase::Waiting(WaitMode::Setup),
                emit,
            )
            .await
        {
            Err(Interrupted::Pair) => {}
            result => return result,
        }
        // A record may have appeared while setup waited. Preserve it and connect normally.
        if !matches!(
            backend.paired_device(),
            Err(WifiProbeError::NoPairingRecord)
        ) {
            return Ok(());
        }
        let search = message!("正在查找通过 USB 连接的 iPhone…");
        emit_event(
            emit,
            DeviceEvent::Status(ConnState::Pairing, search.clone()),
        )?;
        emit_event(emit, DeviceEvent::PairingPrompt(search))?;
        let operation = backend.pair(|prompt| emit(DeviceEvent::PairingPrompt(prompt)));
        let result = inputs
            .drive(timeout(PAIRING_TIMEOUT, operation), Phase::Pairing, emit)
            .await?
            .unwrap_or(Err(PairingError::Pending));
        match result {
            Ok(info) => return emit_event(emit, DeviceEvent::DeviceInfo(info)),
            Err(PairingError::Stopped) => return Err(Interrupted::Stop),
            Err(error) => message = error.message(),
        }
    }
}

async fn connect_cycle(
    backend: &impl Backend,
    inputs: &mut Inputs<'_, impl HostSignals>,
    emit: &impl Fn(DeviceEvent) -> ControlFlow<()>,
    state: &mut DeviceState,
    device: &PairedDevice,
) -> Result<Failure, Interrupted> {
    // Look for the cable first: with the phone on USB, a silent Wi-Fi side should not hold
    // up the fallback for the full discovery window.
    let cable = inputs
        .drive(backend.usb_id(device), Phase::Lookup, emit)
        .await?;
    backend.set_cable_attached(matches!(cable, Ok(Some(_))));
    let wifi = attempt(backend, inputs, emit, state, device, Transport::Wifi).await?;
    if !wifi.allows_usb() {
        return Ok(wifi);
    }
    let cable = match cable {
        Ok(Some(id)) => Ok(Some(id)),
        // The cable may have been plugged in during the Wi-Fi attempt.
        _ => {
            inputs
                .drive(backend.usb_id(device), Phase::Lookup, emit)
                .await?
        }
    };
    let mut id = match cable {
        Ok(Some(id)) => id,
        _ if wifi.requires_developer_mode() => return Ok(Failure::DeveloperMenuUnavailable),
        Ok(None) => return Ok(wifi),
        Err(error) => return Ok(wifi.after_usb(Failure::Usb(error))),
    };
    // Developer Mode is device-wide. Its USB settings service works without
    // bringing up another developer tunnel or attempting to mount an image.
    let failure = if wifi.requires_developer_mode() {
        wifi
    } else {
        let usb = attempt_usb(backend, inputs, emit, state, device, &mut id).await?;
        wifi.after_usb(usb)
    };
    if failure.requires_developer_mode() {
        let reveal = backend.reveal_developer_menu(device, id);
        if inputs.drive(reveal, Phase::Lookup, emit).await?.is_err() {
            return Ok(Failure::DeveloperMenuUnavailable);
        }
    }
    Ok(failure)
}

/// During first setup, recover once from stale USB trust or a transient tunnel
/// error. Return the current handle for any subsequent device-specific work.
async fn attempt_usb(
    backend: &impl Backend,
    inputs: &mut Inputs<'_, impl HostSignals>,
    emit: &impl Fn(DeviceEvent) -> ControlFlow<()>,
    state: &mut DeviceState,
    device: &PairedDevice,
    id: &mut u32,
) -> Result<Failure, Interrupted> {
    let failure = attempt(backend, inputs, emit, state, device, Transport::Usb(*id)).await?;
    // A background reconnect must never create a trust prompt. A session that
    // already became ready cleared first_setup, even if it has since disconnected.
    if !state.first_setup {
        return Ok(failure);
    }
    match failure {
        Failure::Usb(UsbProbeError::TrustRequired | UsbProbeError::PairingRecordUnavailable) => {
            emit_event(
                emit,
                DeviceEvent::Status(
                    ConnState::Connecting,
                    message!("正在重新请求 USB 信任。请在这台 iPhone 上点「信任」并输入锁屏密码。"),
                ),
            )?;
            let result = inputs
                .drive(
                    timeout(PAIRING_TIMEOUT, backend.renew_usb_trust(device)),
                    Phase::Pairing,
                    emit,
                )
                .await?
                .unwrap_or(Err(PairingError::Pending));
            diagnostic::event(format_args!("event=usb_trust_renewed result={result:?}"));
            match result {
                Ok(()) => {}
                Err(PairingError::Usb(error)) => return Ok(Failure::Usb(error)),
                Err(error) => return Ok(Failure::Trust(error)),
            }
        }
        Failure::Usb(
            UsbProbeError::Timeout
            | UsbProbeError::CoreDeviceProxy
            | UsbProbeError::Rsd
            | UsbProbeError::Lockdown,
        ) => {
            diagnostic::event(format_args!("event=usb_retry failure={failure:?}"));
            emit_event(
                emit,
                DeviceEvent::Status(
                    ConnState::Connecting,
                    message!("正在通过 USB 自动重试一次…"),
                ),
            )?;
            inputs
                .drive(sleep(RETRY_DELAY), Phase::Lookup, emit)
                .await?;
        }
        _ => return Ok(failure),
    }
    // A reconnect may assign a new USB handle. Resolve the same phone again,
    // then make exactly one attempt even when the second failure is transient.
    *id = match inputs
        .drive(backend.usb_id(device), Phase::Lookup, emit)
        .await?
    {
        Ok(Some(current)) => current,
        Ok(None) => return Ok(Failure::Usb(UsbProbeError::NoUsbDevice)),
        Err(error) => return Ok(Failure::Usb(error)),
    };
    attempt(backend, inputs, emit, state, device, Transport::Usb(*id)).await
}

async fn attempt(
    backend: &impl Backend,
    inputs: &mut Inputs<'_, impl HostSignals>,
    emit: &impl Fn(DeviceEvent) -> ControlFlow<()>,
    state: &mut DeviceState,
    device: &PairedDevice,
    transport: Transport,
) -> Result<Failure, Interrupted> {
    let connecting = Message::new(match transport {
        Transport::Wifi => "正在建立无线连接…",
        Transport::Usb(_) => "正在建立USB连接…",
    });
    emit_event(emit, DeviceEvent::Status(ConnState::Connecting, connecting))?;
    let active = ActiveSession {
        transport,
        ready: Cell::new(false),
        outstanding_pushes: RefCell::default(),
        outstanding_file: Cell::new(None),
        commands: ClipboardQueue::new(MAX_QUEUED_PUSH_BYTES),
    };
    let session = backend.session(
        device,
        transport,
        &mut state.clipboard,
        &active.commands,
        |event| {
            match &event {
                DeviceEvent::Status(ConnState::Connected { .. }, _) => active.ready.set(true),
                DeviceEvent::PushResult(id, _) => {
                    let queued = active.outstanding_pushes.borrow_mut().pop_front();
                    debug_assert_eq!(queued, Some(*id));
                }
                DeviceEvent::FileResult(id, _) => {
                    let queued = active.outstanding_file.take();
                    debug_assert_eq!(queued, Some(*id));
                }
                _ => {}
            }
            emit(event)
        },
    );
    let result = inputs.drive(session, Phase::Session(&active), emit).await;
    if active.ready.get() {
        state.first_setup = false;
        state.misses = 0;
    }
    let ended = match result {
        Ok(Err(error)) => Ok(error),
        Err(Interrupted::UsbDetached) => Ok(Failure::Session),
        Ok(Ok(())) | Err(Interrupted::Stop) => return Err(Interrupted::Stop),
        Err(other) => Err(other),
    };
    if let Ok(failure) = &ended {
        diagnostic::event(format_args!(
            "event=attempt_failed transport={transport:?} ready={} failure={failure:?}",
            active.ready.get()
        ));
    }
    for id in active.outstanding_pushes.into_inner() {
        emit_event(
            emit,
            DeviceEvent::PushResult(id, Err(PasteboardError::Disconnected)),
        )?;
    }
    if let Some(id) = active.outstanding_file.get() {
        emit_event(
            emit,
            DeviceEvent::FileResult(id, Err(FileTransferError::Disconnected)),
        )?;
    }
    ended
}

struct ActiveSession {
    transport: Transport,
    ready: Cell<bool>,
    outstanding_pushes: RefCell<VecDeque<Option<u64>>>,
    outstanding_file: Cell<Option<u64>>,
    commands: ClipboardQueue,
}

impl ActiveSession {
    fn queue_push(&self, clip: Clip, id: Option<u64>) -> Result<(), PasteboardError> {
        if !self.ready.get() {
            return Err(PasteboardError::Disconnected);
        }
        self.commands.send_push(clip, id)?;
        self.outstanding_pushes.borrow_mut().push_back(id);
        Ok(())
    }

    fn queue_file(&self, offer: FileOffer, id: u64) -> Result<(), FileTransferError> {
        if !self.ready.get() {
            return Err(FileTransferError::Disconnected);
        }
        if self.outstanding_file.get().is_some() {
            return Err(FileTransferError::Busy);
        }
        self.outstanding_file.set(Some(id));
        self.commands.send_offer(offer, id);
        Ok(())
    }
}

enum Phase<'a> {
    Lookup,
    Pairing,
    Session(&'a ActiveSession),
    Waiting(WaitMode),
}

enum Input {
    Command(DeviceCommand),
    Host,
    Detach(u32),
}

struct Inputs<'a, S> {
    commands: &'a mut mpsc::Receiver<DeviceCommand>,
    signals: Option<S>,
    detached: Option<mpsc::Receiver<u32>>,
}

impl<S: HostSignals> Inputs<'_, S> {
    async fn drive<T>(
        &mut self,
        operation: impl Future<Output = T>,
        phase: Phase<'_>,
        emit: &impl Fn(DeviceEvent) -> ControlFlow<()>,
    ) -> Result<T, Interrupted> {
        tokio::pin!(operation);
        loop {
            tokio::select! {
                biased;
                input = self.next() => phase.handle(input, emit)?,
                result = &mut operation => return Ok(result),
            };
        }
    }

    async fn next(&mut self) -> Input {
        loop {
            tokio::select! {
                biased;
                command = self.commands.recv() =>
                    return Input::Command(command.unwrap_or(DeviceCommand::Shutdown)),
                signal = async {
                    match &mut self.signals {
                        Some(source) => source.next_signal().await,
                        None => pending().await,
                    }
                } => match signal {
                    Some(HostSignal::Resumed | HostSignal::NetworkChanged) => return Input::Host,
                    None => self.signals = None,
                },
                event = async {
                    match &mut self.detached {
                        Some(events) => events.recv().await,
                        None => pending().await,
                    }
                } => match event {
                    Some(id) => return Input::Detach(id),
                    None => self.detached = None,
                },
            }
        }
    }
}

impl Phase<'_> {
    fn handle(
        &self,
        input: Input,
        emit: &impl Fn(DeviceEvent) -> ControlFlow<()>,
    ) -> Result<(), Interrupted> {
        match input {
            Input::Command(DeviceCommand::Shutdown) => return Err(Interrupted::Stop),
            Input::Command(DeviceCommand::Forget) => return Err(Interrupted::Forget),
            Input::Command(DeviceCommand::Retry)
                if matches!(
                    self,
                    Self::Waiting(WaitMode::Reconnect | WaitMode::User) | Self::Lookup
                ) || matches!(self, Self::Session(active) if !active.ready.get()) =>
            {
                return Err(Interrupted::UserRetry);
            }
            Input::Command(DeviceCommand::RequestPair)
                if matches!(self, Self::Waiting(WaitMode::Setup)) =>
            {
                return Err(Interrupted::Pair);
            }
            Input::Command(DeviceCommand::Retry | DeviceCommand::RequestPair)
                if matches!(self, Self::Waiting(WaitMode::Setup | WaitMode::User)) =>
            {
                return Err(Interrupted::Reconnect);
            }
            Input::Host
                if matches!(
                    self,
                    Self::Lookup | Self::Session(_) | Self::Waiting(WaitMode::Reconnect)
                ) =>
            {
                return Err(Interrupted::Reconnect);
            }
            Input::Detach(id) if matches!(self, Self::Session(active) if active.transport == Transport::Usb(id)) =>
            {
                return Err(Interrupted::UsbDetached);
            }
            Input::Command(DeviceCommand::Push(clip, id)) => {
                let result = match self {
                    Self::Session(active) => active.queue_push(clip, id),
                    _ => Err(PasteboardError::Disconnected),
                };
                if result.is_err() {
                    emit_event(emit, DeviceEvent::PushResult(id, result))?;
                }
            }
            Input::Command(DeviceCommand::FetchOffer(offer, id)) => {
                let result = match self {
                    Self::Session(active) => active.queue_file(offer, id),
                    _ => Err(FileTransferError::Disconnected),
                };
                if let Err(error) = result {
                    emit_event(emit, DeviceEvent::FileResult(id, Err(error)))?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn emit_event(
    emit: &impl Fn(DeviceEvent) -> ControlFlow<()>,
    event: DeviceEvent,
) -> Result<(), Interrupted> {
    match emit(event) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(()) => Err(Interrupted::Stop),
    }
}
