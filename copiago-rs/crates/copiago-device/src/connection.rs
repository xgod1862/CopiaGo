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
    /// Allows automatic image downloads and mounting when persistent device data is enabled.
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
    async fn usb_id(&self, device: &PairedDevice) -> Result<Option<u32>, UsbProbeError>;
    /// Told before each Wi-Fi attempt whether the paired phone is also on USB.
    fn set_cable_attached(&self, _attached: bool) {}
    /// Resolves when the paired phone reappears on the network. The default never does.
    async fn advertisement(&self, _device: &PairedDevice) {
        pending::<()>().await;
    }
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
    DeveloperModeRequired,
    Setup { stage: DdiStage, timed_out: bool },
}

impl Failure {
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
            Self::Setup {
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

    fn wait(self, first_setup: bool) -> Wait {
        let (mode, state, message) = match self {
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
            _ if first_setup => (
                WaitMode::User,
                ConnState::Hold,
                message!("连接未完成：请保持 iPhone 解锁并连着数据线，然后点「继续连接」"),
            ),
            _ => (
                WaitMode::Reconnect,
                ConnState::Waiting,
                Message::new(match self {
                    Self::Usb(UsbProbeError::Usbmuxd) => "等待 Apple 设备连接服务或 iPhone…",
                    Self::Usb(UsbProbeError::DeviceLocked) => {
                        "请先解锁 iPhone，再点「连接 iPhone」"
                    }
                    _ => "等待 iPhone 连接；请检查解锁、信任及同一局域网",
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
    match cable {
        Ok(Some(id)) => {
            let usb = attempt(backend, inputs, emit, state, device, Transport::Usb(id)).await?;
            Ok(wifi.after_usb(usb))
        }
        Ok(None) => Ok(wifi),
        Err(error) => Ok(wifi.after_usb(Failure::Usb(error))),
    }
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
