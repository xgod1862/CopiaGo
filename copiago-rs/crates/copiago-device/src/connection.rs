//! Transport policy and lifecycle of the device worker.

use std::{
    cell::Cell,
    future::{Future, pending},
    ops::ControlFlow,
    path::Path,
    time::Duration,
};

use copiago_core::{
    clip::Clip,
    ddi::DdiStage,
    i18n::{Language, LanguagePreference, Message, message},
};
use idevice::usbmuxd::UsbmuxdAddr;
use tokio::{sync::mpsc, time::sleep};

use crate::{
    ConnState, DeviceCommand, DeviceEvent, PasteboardError, UsbProbeError, WifiProbeError,
    pasteboard::PasteboardState, usb::listen_usb_detach, wifi::PairedDevice,
};

mod native;
mod push_queue;
#[cfg(test)]
mod tests;

use push_queue::PushQueue;

const RETRY_DELAY: Duration = Duration::from_secs(4);
const MULTIPLE_RECORDS_DELAY: Duration = Duration::from_secs(5);
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

/// Language and data persistence for the paired-phone worker.
#[derive(Clone, Copy, Debug)]
pub struct DeviceRunOptions {
    pub language: Language,
    /// Saves device details and migrates the RemotePairing identifier when needed.
    /// Enabled by default for the desktop host.
    pub persist_device_data: bool,
}

impl Default for DeviceRunOptions {
    fn default() -> Self {
        Self {
            language: LanguagePreference::System.resolve(),
            persist_device_data: true,
        }
    }
}

/// Runs the paired-phone worker on a Tokio runtime with I/O and time enabled.
///
/// Pass the CopiaGo data root. The worker owns one session at a time and resumes
/// discovery after a transport failure, desktop resume or network change.
/// Shutdown, a closed command channel, or a callback break stops the worker.
/// Pairing, persistent forgetting and file downloads are handled outside this worker.
/// Pending pushes are processed in order. If their byte budget is exhausted,
/// new pushes receive a `PushResult` with `PasteboardError::PushBusy`.
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
    async fn usb_id(&self, device: &PairedDevice) -> Result<Option<u32>, UsbProbeError>;
    async fn session(
        &self,
        device: &PairedDevice,
        transport: Transport,
        clipboard: &mut PasteboardState,
        pushes: &PushQueue,
        emit: impl Fn(DeviceEvent) -> ControlFlow<()>,
    ) -> Result<(), Failure>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Failure {
    Wifi(WifiProbeError),
    Usb(UsbProbeError),
    Session,
    Setup(DdiStage),
}

impl Failure {
    fn allows_usb(self) -> bool {
        !matches!(self, Self::Setup(DdiStage::Download | DdiStage::Sign))
    }

    fn after_usb(self, usb: Self) -> Self {
        if self == Self::Setup(DdiStage::Install)
            && !matches!(
                usb,
                Self::Setup(_) | Self::Usb(UsbProbeError::DeveloperModeRequired)
            )
        {
            return self;
        }
        usb
    }

    fn wait(self, first_setup: bool) -> Wait {
        let (mode, state, message) = match self {
            Self::Usb(UsbProbeError::DeveloperModeRequired) => (
                WaitMode::User,
                ConnState::Developer,
                message!(
                    "需要开启开发者模式：在 iPhone「设置 → 隐私与安全性 → 开发者模式」中开启并重启手机，解锁后点「继续连接」"
                ),
            ),
            Self::Setup(stage) => (
                WaitMode::User,
                ConnState::Hold,
                Message::new(match stage {
                    DdiStage::Download => {
                        "下载 iPhone 开发组件失败。已下载的部分会保留，请检查电脑网络后点「继续连接」。"
                    }
                    DdiStage::Sign => "Apple 签名失败。请稍后重试。",
                    DdiStage::Install => {
                        "无法在 iPhone 上安装开发组件。请保持手机解锁并连着数据线后重试。"
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

#[derive(Clone, Copy)]
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
    Reconnect,
    UsbDetached,
}

#[derive(Default)]
struct DeviceState {
    serial: Option<String>,
    clipboard: PasteboardState,
    first_setup: bool,
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
            Err(Interrupted::Forget) => state.forget(),
            Err(Interrupted::Stop) => return,
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
    let wait = match backend.paired_device() {
        Ok(device) => {
            state.select(&device);
            let error = connect_cycle(backend, inputs, emit, state, &device).await?;
            error.wait(state.first_setup)
        }
        Err(WifiProbeError::NoPairingRecord) => {
            if state.serial.is_none() {
                state.first_setup = true;
            }
            Wait {
                mode: WaitMode::Setup,
                state: ConnState::Setup,
                message: message!("连接并信任你的 iPhone"),
            }
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
    emit_event(emit, DeviceEvent::Status(wait.state, wait.message))?;
    inputs
        .drive(wait.mode.timer(), Phase::Waiting(wait.mode), emit)
        .await
}

async fn connect_cycle(
    backend: &impl Backend,
    inputs: &mut Inputs<'_, impl HostSignals>,
    emit: &impl Fn(DeviceEvent) -> ControlFlow<()>,
    state: &mut DeviceState,
    device: &PairedDevice,
) -> Result<Failure, Interrupted> {
    let wifi = attempt(backend, inputs, emit, state, device, Transport::Wifi).await?;
    if !wifi.allows_usb() {
        return Ok(wifi);
    }
    match inputs
        .drive(backend.usb_id(device), Phase::Lookup, emit)
        .await?
    {
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
        outstanding_pushes: Cell::new(0),
        pushes: PushQueue::new(MAX_QUEUED_PUSH_BYTES),
    };
    let session = backend.session(
        device,
        transport,
        &mut state.clipboard,
        &active.pushes,
        |event| {
            match &event {
                DeviceEvent::Status(ConnState::Connected { .. }, _) => active.ready.set(true),
                DeviceEvent::PushResult(_) => active
                    .outstanding_pushes
                    .set(active.outstanding_pushes.get() - 1),
                _ => {}
            }
            emit(event)
        },
    );
    let result = inputs.drive(session, Phase::Session(&active), emit).await;
    if active.ready.get() {
        state.first_setup = false;
    }
    let ended = match result {
        Ok(Err(error)) => Ok(error),
        Err(Interrupted::UsbDetached) => Ok(Failure::Session),
        Ok(Ok(())) | Err(Interrupted::Stop) => return Err(Interrupted::Stop),
        Err(other) => Err(other),
    };
    for _ in 0..active.outstanding_pushes.get() {
        emit_event(
            emit,
            DeviceEvent::PushResult(Err(PasteboardError::Disconnected)),
        )?;
    }
    ended
}

struct ActiveSession {
    transport: Transport,
    ready: Cell<bool>,
    outstanding_pushes: Cell<usize>,
    pushes: PushQueue,
}

impl ActiveSession {
    fn queue_push(&self, clip: Clip) -> Result<(), PasteboardError> {
        if !self.ready.get() {
            return Err(PasteboardError::Disconnected);
        }
        self.pushes.send(clip)?;
        self.outstanding_pushes
            .set(self.outstanding_pushes.get() + 1);
        Ok(())
    }
}

enum Phase<'a> {
    Lookup,
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
            Input::Command(DeviceCommand::Push(clip)) => {
                let result = match self {
                    Self::Session(active) => active.queue_push(clip),
                    _ => Err(PasteboardError::Disconnected),
                };
                if result.is_err() {
                    emit_event(emit, DeviceEvent::PushResult(result))?;
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
