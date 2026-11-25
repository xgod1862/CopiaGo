use std::{cell::RefCell, collections::VecDeque, path::PathBuf};

use copiago_core::clip::{Clip, PasteboardChange};
use tokio::{
    sync::{Notify, mpsc},
    time::{Instant, timeout},
};

use super::*;

const PHONE: &str = "paired";
const OTHER: &str = "other";
const USB_ID: u32 = 7;
const OFFLINE: Failure = Failure::Wifi(WifiProbeError::NoEndpoint);
const USB_MODE: Failure = Failure::Usb(UsbProbeError::DeveloperModeRequired);
const INSTALL: Failure = Failure::setup(DdiStage::Install);
const SIGN: Failure = Failure::setup(DdiStage::Sign);
const DOWNLOAD: Failure = Failure::setup(DdiStage::Download);

#[derive(Clone, Copy)]
enum Session {
    Failure(Failure),
    Connected,
    Opening,
    SlowPull,
    SlowSet,
    SlowFetch,
}

#[derive(Debug, PartialEq, Eq)]
enum Call {
    Scan,
    RenewTrust(String),
    UsbList,
    Reveal(String, u32),
    Pair,
    Forget,
    Attempt(Transport),
}

#[derive(Clone, Copy)]
enum Pairing {
    Success,
    Failure(PairingError),
    WaitingForTrust,
}

struct FakePhone {
    renew_result: Cell<Result<(), PairingError>>,
    record: Cell<Result<&'static str, WifiProbeError>>,
    attached: Cell<Option<&'static str>>,
    sessions: RefCell<VecDeque<Session>>,
    calls: RefCell<Vec<(Instant, Call)>>,
    pairings: RefCell<VecDeque<Pairing>>,
    request_started: Notify,
    reveal_error: Cell<Option<UsbProbeError>>,
    release_request: Notify,
    pushed: RefCell<Vec<Clip>>,
    fetched: RefCell<Vec<FileOffer>>,
    /// Delay after which the phone announces itself while the worker waits.
    advertises_after: Cell<Option<Duration>>,
}

impl FakePhone {
    fn new(attached: Option<&'static str>, sessions: impl IntoIterator<Item = Session>) -> Self {
        Self {
            renew_result: Cell::new(Ok(())),
            record: Cell::new(Ok(PHONE)),
            attached: Cell::new(attached),
            sessions: RefCell::new(sessions.into_iter().collect()),
            calls: RefCell::new(Vec::new()),
            pairings: RefCell::new(VecDeque::new()),
            request_started: Notify::new(),
            reveal_error: Cell::new(None),
            release_request: Notify::new(),
            pushed: RefCell::new(Vec::new()),
            fetched: RefCell::new(Vec::new()),
            advertises_after: Cell::new(None),
        }
    }

    fn record_call(&self, call: Call) {
        self.calls.borrow_mut().push((Instant::now(), call));
    }

    async fn pause_request(&self) {
        self.request_started.notify_one();
        self.release_request.notified().await;
    }

    fn attempts(&self) -> Vec<Transport> {
        self.calls
            .borrow()
            .iter()
            .filter_map(|(_, call)| match call {
                Call::Attempt(transport) => Some(*transport),
                _ => None,
            })
            .collect()
    }
}

impl Backend for FakePhone {
    fn paired_device(&self) -> Result<PairedDevice, WifiProbeError> {
        self.record_call(Call::Scan);
        self.record.get().map(record)
    }

    async fn pair(
        &self,
        prompt: impl Fn(Message) -> ControlFlow<()>,
    ) -> Result<DeviceInfo, PairingError> {
        self.record_call(Call::Pair);
        if prompt(message!(
            "正在等待 iPhone 信任此电脑。请解锁手机，按提示点「信任」并输入锁屏密码。"
        ))
        .is_break()
        {
            return Err(PairingError::Stopped);
        }
        let result = self
            .pairings
            .borrow_mut()
            .pop_front()
            .expect("extra pairing");
        match result {
            Pairing::Success => {
                self.record.set(Ok(PHONE));
                Ok(DeviceInfo {
                    model: "iPhone".into(),
                    ..DeviceInfo::default()
                })
            }
            Pairing::Failure(error) => Err(error),
            Pairing::WaitingForTrust => pending().await,
        }
    }

    fn forget(&self) -> std::io::Result<()> {
        self.record_call(Call::Forget);
        self.record.set(Err(WifiProbeError::NoPairingRecord));
        Ok(())
    }

    async fn usb_id(&self, device: &PairedDevice) -> Result<Option<u32>, UsbProbeError> {
        self.record_call(Call::UsbList);
        Ok((self.attached.get() == Some(device.serial.as_str())).then_some(USB_ID))
    }

    async fn advertisement(&self, _device: &PairedDevice) {
        match self.advertises_after.get() {
            Some(delay) => sleep(delay).await,
            None => pending().await,
        }
    }

    async fn renew_usb_trust(&self, device: &PairedDevice) -> Result<(), PairingError> {
        self.record_call(Call::RenewTrust(device.serial.clone()));
        self.renew_result.get()
    }

    async fn reveal_developer_menu(
        &self,
        device: &PairedDevice,
        id: u32,
    ) -> Result<(), UsbProbeError> {
        self.record_call(Call::Reveal(device.serial.clone(), id));
        self.reveal_error.get().map_or(Ok(()), Err)
    }

    async fn session(
        &self,
        _device: &PairedDevice,
        transport: Transport,
        _clipboard: &mut PasteboardState,
        commands: &ClipboardQueue,
        emit: impl Fn(DeviceEvent) -> ControlFlow<()>,
    ) -> Result<(), Failure> {
        self.record_call(Call::Attempt(transport));
        let session = self
            .sessions
            .borrow_mut()
            .pop_front()
            .expect("extra connection");
        match session {
            Session::Failure(error) => Err(error),
            Session::Connected | Session::SlowPull | Session::SlowSet | Session::SlowFetch => {
                let connected = ConnState::Connected {
                    wireless: transport == Transport::Wifi,
                };
                if emit(DeviceEvent::Status(connected, message!(""))).is_break() {
                    return Ok(());
                }
                if matches!(session, Session::Connected) {
                    return pending().await;
                }
                if matches!(session, Session::SlowPull) {
                    self.pause_request().await;
                }
                let mut slow_set = matches!(session, Session::SlowSet);
                loop {
                    let reply = match commands.recv().await {
                        DeviceCommand::Push(clip, id) => {
                            self.pushed.borrow_mut().push(clip);
                            if std::mem::take(&mut slow_set) {
                                self.pause_request().await;
                            }
                            DeviceEvent::PushResult(id, Ok(()))
                        }
                        DeviceCommand::FetchOffer(offer, id) => {
                            self.fetched.borrow_mut().push(offer);
                            self.pause_request().await;
                            DeviceEvent::FileResult(id, Err(FileTransferError::OfferExpired))
                        }
                        _ => unreachable!("a clipboard queue only contains clipboard commands"),
                    };
                    if emit(reply).is_break() {
                        return Ok(());
                    }
                }
            }
            Session::Opening => pending().await,
        }
    }
}

fn record(serial: &str) -> PairedDevice {
    PairedDevice {
        serial: serial.into(),
        path: PathBuf::new(),
    }
}

#[tokio::test(start_paused = true)]
async fn connection_policy_by_failure_stage() {
    use Session::{Connected, Failure as F};
    use Transport::{Usb, Wifi};

    let wifi_ready = ConnState::Connected { wireless: true };
    let usb_ready = ConnState::Connected { wireless: false };
    let cases = [
        (
            "Wi-Fi even with a cable",
            Some(PHONE),
            Connected,
            None,
            wifi_ready,
            None,
        ),
        (
            "same-device fallback",
            Some(PHONE),
            F(OFFLINE),
            Some(Connected),
            usb_ready,
            None,
        ),
        (
            "unrelated USB phone",
            Some(OTHER),
            F(OFFLINE),
            None,
            ConnState::Waiting,
            Some(OFFLINE),
        ),
        (
            "download skips USB",
            Some(PHONE),
            F(DOWNLOAD),
            None,
            ConnState::Hold,
            Some(DOWNLOAD),
        ),
        (
            "signing skips USB",
            Some(PHONE),
            F(SIGN),
            None,
            ConnState::Hold,
            Some(SIGN),
        ),
        (
            "install permits USB",
            Some(PHONE),
            F(INSTALL),
            Some(Connected),
            usb_ready,
            None,
        ),
        (
            "USB preserves install",
            Some(PHONE),
            F(INSTALL),
            Some(F(Failure::Session)),
            ConnState::Hold,
            Some(INSTALL),
        ),
        (
            "USB developer mode wins",
            Some(PHONE),
            F(INSTALL),
            Some(F(USB_MODE)),
            ConnState::Developer,
            Some(USB_MODE),
        ),
        (
            "Wi-Fi detects Developer Mode before the USB tunnel is available",
            Some(PHONE),
            F(Failure::DeveloperModeRequired),
            None,
            ConnState::Developer,
            Some(Failure::DeveloperModeRequired),
        ),
        (
            "do not reveal the menu on another USB phone",
            Some(OTHER),
            F(Failure::DeveloperModeRequired),
            None,
            ConnState::Hold,
            Some(Failure::DeveloperMenuUnavailable),
        ),
        (
            "USB setup error wins",
            Some(PHONE),
            F(INSTALL),
            Some(F(SIGN)),
            ConnState::Hold,
            Some(SIGN),
        ),
    ];
    for (name, attached, wifi, usb, expected, message_from) in cases {
        let phone = FakePhone::new(attached, std::iter::once(wifi).chain(usb));
        let (_send, mut commands) = mpsc::channel(8);
        let mut inputs = Inputs {
            commands: &mut commands,
            signals: Some(()),
            detached: None,
        };
        let seen = Cell::new(false);
        run(&phone, &mut inputs, &|event| {
            if let DeviceEvent::Status(state, text) = event
                && state != ConnState::Connecting
            {
                assert_eq!(state, expected, "{name}");
                if let Some(failure) = message_from {
                    assert_eq!(text, failure.wait(false).message, "{name}");
                }
                seen.set(true);
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        })
        .await;
        assert!(seen.get(), "{name}");
        let mut expected_attempts = vec![Wifi];
        if usb.is_some() {
            expected_attempts.push(Usb(USB_ID));
        }
        assert_eq!(phone.attempts(), expected_attempts, "{name}");
        // The cable is checked before Wi-Fi so a silent Wi-Fi side can be skipped quickly.
        let calls = phone.calls.borrow();
        let lookup = calls.iter().position(|(_, call)| *call == Call::UsbList);
        let first_attempt = calls
            .iter()
            .position(|(_, call)| matches!(call, Call::Attempt(_)));
        assert!(lookup.is_some() && lookup < first_attempt, "{name}");
        let reveals: Vec<_> = calls
            .iter()
            .filter_map(|(_, call)| match call {
                Call::Reveal(serial, id) => Some((serial.clone(), *id)),
                _ => None,
            })
            .collect();
        let expected_reveals = if expected == ConnState::Developer {
            vec![(PHONE.to_owned(), USB_ID)]
        } else {
            vec![]
        };
        assert_eq!(reveals, expected_reveals, "{name}");
    }
}

#[tokio::test(start_paused = true)]
async fn ordinary_and_multiple_record_retries_use_fixed_delays() {
    use Session::Failure as F;
    for (records, repeating_status, seconds) in [
        (Ok(PHONE), ConnState::Waiting, 4),
        (
            Err(WifiProbeError::MultiplePairingRecords),
            ConnState::Error,
            5,
        ),
    ] {
        let phone = FakePhone::new(None, [F(OFFLINE), F(OFFLINE)]);
        phone.record.set(records);
        let (_send, mut commands) = mpsc::channel(8);
        let mut inputs = Inputs {
            commands: &mut commands,
            signals: Some(()),
            detached: None,
        };
        let first = Cell::new(None);
        run(&phone, &mut inputs, &|event| {
            if matches!(event, DeviceEvent::Status(state, _) if state == repeating_status) {
                let now = Instant::now();
                if let Some(previous) = first.get() {
                    assert_eq!(now - previous, Duration::from_secs(seconds));
                    return ControlFlow::Break(());
                }
                first.set(Some(now));
            }
            ControlFlow::Continue(())
        })
        .await;
        assert_eq!(phone.attempts().len(), if records.is_ok() { 2 } else { 0 });
    }
}

fn rig<S: HostSignals>(
    commands: &mut mpsc::Receiver<DeviceCommand>,
    signals: S,
    detached: Option<mpsc::Receiver<u32>>,
) -> Inputs<'_, S> {
    Inputs {
        commands,
        signals: Some(signals),
        detached,
    }
}

#[tokio::test(start_paused = true)]
async fn developer_menu_failure_waits_for_user_and_retries_the_same_phone() {
    let phone = FakePhone::new(
        Some(PHONE),
        [Session::Failure(Failure::DeveloperModeRequired); 2],
    );
    phone.reveal_error.set(Some(UsbProbeError::Timeout));
    let (send, mut commands) = mpsc::channel(4);
    let mut inputs = rig(&mut commands, (), None);
    let (events, mut statuses) = mpsc::unbounded_channel();
    let emit = collect_status(events);
    let verify = async {
        status(&mut statuses, ConnState::Connecting).await;
        status(&mut statuses, ConnState::Hold).await;
        assert!(timeout(RETRY_DELAY * 2, statuses.recv()).await.is_err());
        phone.reveal_error.set(None);
        send.send(DeviceCommand::Retry).await.unwrap();
        status(&mut statuses, ConnState::Connecting).await;
        status(&mut statuses, ConnState::Developer).await;
        send.send(DeviceCommand::Shutdown).await.unwrap();
    };
    tokio::join!(run(&phone, &mut inputs, &emit), verify);
    let expected = Call::Reveal(PHONE.to_owned(), USB_ID);
    assert_eq!(
        phone
            .calls
            .borrow()
            .iter()
            .filter(|(_, call)| call == &expected)
            .count(),
        2
    );
}

async fn status(events: &mut mpsc::UnboundedReceiver<ConnState>, expected: ConnState) {
    let actual = timeout(Duration::from_secs(6), events.recv())
        .await
        .unwrap();
    assert_eq!(actual, Some(expected));
}

fn collect_status(
    events: mpsc::UnboundedSender<ConnState>,
) -> impl Fn(DeviceEvent) -> ControlFlow<()> {
    move |event| {
        if let DeviceEvent::Status(state, _) = event {
            events.send(state).unwrap();
        }
        ControlFlow::Continue(())
    }
}

#[tokio::test(start_paused = true)]
async fn setup_and_developer_failures_stay_visible_until_user_retry() {
    for (first_setup, failure, held) in [
        (true, OFFLINE, ConnState::Hold),
        (false, INSTALL, ConnState::Hold),
        (false, USB_MODE, ConnState::Developer),
    ] {
        let sessions = if failure == USB_MODE {
            vec![
                Session::Failure(OFFLINE),
                Session::Failure(USB_MODE),
                Session::Connected,
            ]
        } else {
            vec![Session::Failure(failure), Session::Connected]
        };
        let phone = FakePhone::new((failure == USB_MODE).then_some(PHONE), sessions);
        if first_setup {
            phone.record.set(Err(WifiProbeError::NoPairingRecord));
        }
        let (send, mut commands) = mpsc::channel(8);
        let (wake, signals) = mpsc::channel(8);
        let (events, mut statuses) = mpsc::unbounded_channel();
        let mut inputs = rig(&mut commands, signals, None);
        let emit = collect_status(events);
        let verify = async {
            if first_setup {
                status(&mut statuses, ConnState::Setup).await;
                phone.record.set(Ok(PHONE));
                send.send(DeviceCommand::RequestPair).await.unwrap();
            }
            status(&mut statuses, ConnState::Connecting).await;
            if failure == USB_MODE {
                status(&mut statuses, ConnState::Connecting).await;
            }
            status(&mut statuses, held).await;
            wake.send(HostSignal::Resumed).await.unwrap();
            assert!(
                timeout(Duration::from_secs(12), statuses.recv())
                    .await
                    .is_err()
            );
            send.send(DeviceCommand::Retry).await.unwrap();
            status(&mut statuses, ConnState::Connecting).await;
            status(&mut statuses, ConnState::Connected { wireless: true }).await;
            send.send(DeviceCommand::Shutdown).await.unwrap();
        };
        tokio::join!(run(&phone, &mut inputs, &emit), verify);
    }
}

#[tokio::test(start_paused = true)]
async fn first_usb_timeout_retries_once_then_shows_the_actual_problem() {
    let timed_out = Failure::Usb(UsbProbeError::Timeout);
    let phone = FakePhone::new(
        Some(PHONE),
        [OFFLINE, timed_out, timed_out].map(Session::Failure),
    );
    let (_send, mut commands) = mpsc::channel(4);
    let mut inputs = rig(&mut commands, (), None);
    let mut state = DeviceState {
        first_setup: true,
        ..DeviceState::default()
    };
    let error = connect_cycle(
        &phone,
        &mut inputs,
        &|_| ControlFlow::Continue(()),
        &mut state,
        &record(PHONE),
    )
    .await;
    let Ok(error) = error else { panic!("setup interrupted") };
    assert_eq!(
        phone.attempts(),
        [Transport::Wifi, Transport::Usb(USB_ID), Transport::Usb(USB_ID)]
    );
    assert_eq!(
        error.wait(true).message,
        message!("USB 连接超时。请将 iPhone 直接接到电脑的 USB 口，然后点「继续连接」。")
    );
}

#[tokio::test(start_paused = true)]
async fn usb_trust_recovery_is_scoped_to_the_first_setup_and_the_same_phone() {
    let stale = Failure::Usb(UsbProbeError::TrustRequired);
    for (first_setup, renewal, replacement) in [
        (true, Ok(()), PHONE),
        (true, Ok(()), OTHER),
        (true, Err(PairingError::Denied), PHONE),
        (false, Ok(()), PHONE),
    ] {
        let phone = FakePhone::new(Some(PHONE), [Session::Failure(stale), Session::Connected]);
        phone.renew_result.set(renewal);
        let (_send, mut commands) = mpsc::channel(4);
        let mut inputs = rig(&mut commands, (), None);
        let mut state = DeviceState {
            first_setup,
            ..DeviceState::default()
        };
        let connected = Cell::new(false);
        let emit = |event| {
            if let DeviceEvent::Status(status, detail) = event {
                if detail == message!("正在重新请求 USB 信任。请在这台 iPhone 上点「信任」并输入锁屏密码。") {
                    phone.attached.set(Some(replacement));
                }
                if status == (ConnState::Connected { wireless: false }) {
                    connected.set(true);
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        };
        let mut id = USB_ID;
        let result = attempt_usb(
            &phone, &mut inputs, &emit, &mut state, &record(PHONE), &mut id,
        )
        .await;
        let should_connect = first_setup && renewal.is_ok() && replacement == PHONE;
        assert_eq!(connected.get(), should_connect);
        assert_eq!(phone.attempts().len(), if should_connect { 2 } else { 1 });
        let calls = phone.calls.borrow();
        assert_eq!(
            calls.iter()
                .filter(|(_, call)| matches!(call, Call::RenewTrust(serial) if serial == PHONE))
                .count(),
            if first_setup { 1 } else { 0 },
        );
        assert!(!calls.iter().any(|(_, call)| matches!(call, Call::Forget | Call::Pair)));
        if !first_setup {
            assert_eq!(result.ok(), Some(stale));
        } else if renewal.is_err() {
            assert_eq!(result.ok(), Some(Failure::Trust(PairingError::Denied)));
        } else if replacement != PHONE {
            assert_eq!(result.ok(), Some(Failure::Usb(UsbProbeError::NoUsbDevice)));
        }
    }
}

#[test]
fn first_setup_keeps_actionable_causes() {
    for (error, expected) in [
        (Failure::Usb(UsbProbeError::DeviceLocked), "iPhone 已锁定。请解锁并停留在主屏幕，然后点「继续连接」。"),
        (Failure::Usb(UsbProbeError::Usbmuxd), "等待 Apple 设备连接服务或 iPhone…"),
        (OFFLINE, "无线未找到这台 iPhone，USB 也未检测到它。请重新插紧数据线或换一个电脑 USB 口。"),
    ] {
        assert_eq!(error.wait(true).message, Message::new(expected));
    }
    assert_eq!(Failure::DeveloperModeRequired.after_usb(Failure::Usb(UsbProbeError::Timeout)), Failure::DeveloperModeRequired);
}

#[tokio::test(start_paused = true)]
async fn explicit_retry_recovers_an_unfinished_pairing_after_restart() {
    let stale = Session::Failure(Failure::Usb(UsbProbeError::TrustRequired));
    let phone = FakePhone::new(
        Some(PHONE),
        [Session::Failure(OFFLINE), stale, Session::Failure(OFFLINE), stale, Session::Connected],
    );
    let (send, mut commands) = mpsc::channel(4);
    let mut inputs = rig(&mut commands, (), None);
    let requested_retry = Cell::new(false);
    let connected = Cell::new(false);
    let emit = |event| {
        if let DeviceEvent::Status(status, _) = event {
            if status == ConnState::Waiting {
                assert!(!requested_retry.replace(true), "retry was ignored");
                send.try_send(DeviceCommand::Retry).unwrap();
            }
            if status == (ConnState::Connected { wireless: false }) {
                connected.set(true);
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    };
    run(&phone, &mut inputs, &emit).await;
    assert!(connected.get());
    assert!(requested_retry.get());
}

#[tokio::test(start_paused = true)]
async fn pairing_retries_on_request_and_success_connects_over_wifi_first() {
    let phone = FakePhone::new(Some(PHONE), [Session::Connected]);
    phone.record.set(Err(WifiProbeError::NoPairingRecord));
    phone
        .pairings
        .borrow_mut()
        .extend([Pairing::Failure(PairingError::Denied), Pairing::Success]);
    let (send, mut commands) = mpsc::channel(8);
    let (events, mut statuses) = mpsc::unbounded_channel();
    let mut inputs = rig(&mut commands, (), None);
    let emit = |event| {
        let feedback = match event {
            DeviceEvent::Status(state, message) => Some((state, message)),
            DeviceEvent::PairingPrompt(message) => Some((ConnState::Pairing, message)),
            DeviceEvent::DeviceInfo(info) => {
                assert_eq!(info.model, "iPhone");
                None
            }
            _ => None,
        };
        if let Some(feedback) = feedback {
            events.send(feedback).unwrap();
        }
        ControlFlow::Continue(())
    };
    let verify = async {
        assert_eq!(feedback(&mut statuses).await.0, ConnState::Setup);
        for outcome in [ConnState::Setup, ConnState::Connecting] {
            send.send(DeviceCommand::RequestPair).await.unwrap();
            let search = message!("正在查找通过 USB 连接的 iPhone…");
            assert_eq!(
                feedback(&mut statuses).await,
                (ConnState::Pairing, search.clone())
            );
            assert_eq!(feedback(&mut statuses).await, (ConnState::Pairing, search));
            assert_eq!(
                feedback(&mut statuses).await,
                (
                    ConnState::Pairing,
                    message!(
                        "正在等待 iPhone 信任此电脑。请解锁手机，按提示点「信任」并输入锁屏密码。"
                    )
                )
            );
            let (state, prompt) = feedback(&mut statuses).await;
            assert_eq!(state, outcome);
            if state == ConnState::Setup {
                assert_eq!(prompt, PairingError::Denied.message());
                assert!(
                    timeout(Duration::from_secs(12), statuses.recv())
                        .await
                        .is_err()
                );
            }
        }
        assert_eq!(
            feedback(&mut statuses).await.0,
            ConnState::Connected { wireless: true }
        );
        send.send(DeviceCommand::Shutdown).await.unwrap();
    };
    tokio::join!(run(&phone, &mut inputs, &emit), verify);
    assert_eq!(phone.attempts(), [Transport::Wifi]);
}

async fn feedback(
    events: &mut mpsc::UnboundedReceiver<(ConnState, Message)>,
) -> (ConnState, Message) {
    timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn wifi_and_usb_remain_idle_while_waiting_for_trust_and_after_timeout() {
    let phone = FakePhone::new(Some(PHONE), []);
    phone.record.set(Err(WifiProbeError::NoPairingRecord));
    phone
        .pairings
        .borrow_mut()
        .push_back(Pairing::WaitingForTrust);
    let (send, mut commands) = mpsc::channel(8);
    let (events, mut statuses) = mpsc::unbounded_channel();
    let mut inputs = rig(&mut commands, (), None);
    let emit = |event| {
        if let DeviceEvent::Status(state, message) = event {
            events.send((state, message)).unwrap();
        }
        ControlFlow::Continue(())
    };
    let verify = async {
        assert_eq!(feedback(&mut statuses).await.0, ConnState::Setup);
        send.send(DeviceCommand::RequestPair).await.unwrap();
        assert_eq!(feedback(&mut statuses).await.0, ConnState::Pairing);
        let started = Instant::now();
        let before_timeout = PAIRING_TIMEOUT - Duration::from_secs(1);
        assert!(timeout(before_timeout, statuses.recv()).await.is_err());
        let after = timeout(Duration::from_secs(2), statuses.recv())
            .await
            .unwrap();
        assert_eq!(
            after,
            Some((ConnState::Setup, PairingError::Pending.message()))
        );
        assert_eq!(started.elapsed(), PAIRING_TIMEOUT);
        assert!(
            timeout(Duration::from_secs(12), statuses.recv())
                .await
                .is_err()
        );
        send.send(DeviceCommand::Shutdown).await.unwrap();
    };
    tokio::join!(run(&phone, &mut inputs, &emit), verify);
    assert!(phone.attempts().is_empty());
}

#[tokio::test(start_paused = true)]
async fn forget_cancels_pairing_before_processing_the_next_request() {
    let phone = FakePhone::new(Some(PHONE), [Session::Connected]);
    phone.record.set(Err(WifiProbeError::NoPairingRecord));
    phone
        .pairings
        .borrow_mut()
        .extend([Pairing::WaitingForTrust, Pairing::Success]);
    let (send, mut commands) = mpsc::channel(8);
    let (events, mut statuses) = mpsc::unbounded_channel();
    let mut inputs = rig(&mut commands, (), None);
    let emit = collect_status(events);
    let verify = async {
        status(&mut statuses, ConnState::Setup).await;
        send.send(DeviceCommand::RequestPair).await.unwrap();
        status(&mut statuses, ConnState::Pairing).await;
        tokio::task::yield_now().await;
        send.send(DeviceCommand::Forget).await.unwrap();
        status(&mut statuses, ConnState::Setup).await;
        send.send(DeviceCommand::RequestPair).await.unwrap();
        status(&mut statuses, ConnState::Pairing).await;
        status(&mut statuses, ConnState::Connecting).await;
        status(&mut statuses, ConnState::Connected { wireless: true }).await;
        send.send(DeviceCommand::Shutdown).await.unwrap();
    };
    tokio::join!(run(&phone, &mut inputs, &emit), verify);
    let calls = phone.calls.into_inner();
    let changes: Vec<_> = calls
        .into_iter()
        .map(|(_, call)| call)
        .filter(|call| matches!(call, Call::Pair | Call::Forget))
        .collect();
    assert_eq!(changes, [Call::Pair, Call::Forget, Call::Pair]);
}

#[tokio::test(start_paused = true)]
async fn usb_detach_retries_wifi_and_healthy_wifi_ignores_cable_changes() {
    let phone = FakePhone::new(
        Some(PHONE),
        [
            Session::Failure(OFFLINE),
            Session::Connected,
            Session::Connected,
        ],
    );
    let (send, mut commands) = mpsc::channel(8);
    let (wake, signals) = mpsc::channel(8);
    let (detach, detached) = mpsc::channel(8);
    let (events, mut statuses) = mpsc::unbounded_channel();
    let mut inputs = rig(&mut commands, signals, Some(detached));
    let emit = collect_status(events);
    let verify = async {
        status(&mut statuses, ConnState::Connecting).await;
        status(&mut statuses, ConnState::Connecting).await;
        status(&mut statuses, ConnState::Connected { wireless: false }).await;
        detach.send(USB_ID + 1).await.unwrap();
        assert!(
            timeout(Duration::from_secs(1), statuses.recv())
                .await
                .is_err()
        );

        phone.attached.set(None);
        let disconnected = Instant::now();
        detach.send(USB_ID).await.unwrap();
        status(&mut statuses, ConnState::Waiting).await;
        status(&mut statuses, ConnState::Connecting).await;
        assert_eq!(disconnected.elapsed(), Duration::from_secs(4));
        status(&mut statuses, ConnState::Connected { wireless: true }).await;

        phone.attached.set(Some(PHONE));
        assert!(
            timeout(Duration::from_secs(1), statuses.recv())
                .await
                .is_err()
        );
        phone.attached.set(None);
        detach.send(USB_ID).await.unwrap();
        assert!(
            timeout(Duration::from_secs(1), statuses.recv())
                .await
                .is_err()
        );
        drop(wake);
        drop(send);
    };
    tokio::join!(run(&phone, &mut inputs, &emit), verify);
    assert_eq!(
        phone.attempts(),
        [Transport::Wifi, Transport::Usb(USB_ID), Transport::Wifi]
    );
}

#[tokio::test(start_paused = true)]
async fn host_reconnect_and_forget_interrupt_without_usb_fallback() {
    let phone = FakePhone::new(
        Some(PHONE),
        [Session::Opening, Session::Connected, Session::Connected],
    );
    let (send, mut commands) = mpsc::channel(8);
    let (wake, signals) = mpsc::channel(8);
    let (events, mut statuses) = mpsc::unbounded_channel();
    let mut inputs = rig(&mut commands, signals, None);
    let emit = collect_status(events);
    let verify = async {
        status(&mut statuses, ConnState::Connecting).await;
        sleep(Duration::from_secs(1)).await;
        wake.send(HostSignal::Resumed).await.unwrap();
        status(&mut statuses, ConnState::Connecting).await;
        status(&mut statuses, ConnState::Connected { wireless: true }).await;
        wake.send(HostSignal::NetworkChanged).await.unwrap();
        status(&mut statuses, ConnState::Connecting).await;
        status(&mut statuses, ConnState::Connected { wireless: true }).await;

        send.send(DeviceCommand::Forget).await.unwrap();
        status(&mut statuses, ConnState::Setup).await;
        send.send(DeviceCommand::Shutdown).await.unwrap();
    };
    tokio::join!(run(&phone, &mut inputs, &emit), verify);
    // The cable is looked up, but a working Wi-Fi session never falls back to it.
    assert_eq!(phone.attempts(), [Transport::Wifi; 3]);
}

#[test]
fn clipboard_state_survives_same_device_and_resets_on_switch_or_forget() {
    let mut state = DeviceState::default();
    let change = |count| Some(PasteboardChange { nonce: None, count });
    state.select(&record(PHONE));
    assert!(!state.clipboard.reconnect(change(1)));
    state.select(&record(PHONE));
    assert!(state.clipboard.reconnect(change(2)));
    state.select(&record(OTHER));
    assert!(!state.clipboard.reconnect(change(2)));
    state.forget();
    state.select(&record(OTHER));
    assert!(!state.clipboard.reconnect(change(3)));
}

#[tokio::test]
async fn offline_push_is_reported_without_a_device_session() {
    let phone = FakePhone::new(None, []);
    phone.record.set(Err(WifiProbeError::NoPairingRecord));
    let (send, mut commands) = mpsc::channel(8);
    send.try_send(DeviceCommand::Push(Clip::Text("sample".into()), None))
        .unwrap();
    let mut inputs = Inputs {
        commands: &mut commands,
        signals: Some(()),
        detached: None,
    };
    run(&phone, &mut inputs, &|event| {
        if let DeviceEvent::PushResult(_, result) = event {
            assert_eq!(result, Err(PasteboardError::Disconnected));
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    })
    .await;
    assert!(phone.attempts().is_empty());
}

#[tokio::test(start_paused = true)]
async fn pushes_queued_during_a_slow_pull_are_delivered_fifo() {
    let phone = FakePhone::new(None, [Session::SlowPull]);
    let (send, mut commands) = mpsc::channel(4);
    let (events, mut statuses) = mpsc::unbounded_channel();
    let (result, mut results) = mpsc::unbounded_channel();
    let mut inputs = Inputs {
        commands: &mut commands,
        signals: Some(()),
        detached: None,
    };
    let emit_status = collect_status(events);
    let emit = |event| match event {
        DeviceEvent::PushResult(id, reply) => {
            result.send((id, reply)).unwrap();
            ControlFlow::Continue(())
        }
        event => emit_status(event),
    };
    let verify = async {
        status(&mut statuses, ConnState::Connecting).await;
        status(&mut statuses, ConnState::Connected { wireless: true }).await;
        for (id, text) in [(17, "First copy"), (18, "Second copy")] {
            send.try_send(DeviceCommand::Push(Clip::Text(text.into()), Some(id)))
                .unwrap();
        }
        sleep(Duration::from_secs(1)).await;
        assert!(phone.pushed.borrow().is_empty());
        phone.release_request.notify_one();
        for id in [17, 18] {
            assert_eq!(
                timeout(Duration::from_secs(1), results.recv())
                    .await
                    .unwrap(),
                Some((Some(id), Ok(()))),
            );
        }
        send.send(DeviceCommand::Shutdown).await.unwrap();
    };
    tokio::join!(run(&phone, &mut inputs, &emit), verify);
    assert!(
        phone.pushed.into_inner()
            == [
                Clip::Text("First copy".into()),
                Clip::Text("Second copy".into()),
            ]
    );
}

#[tokio::test(start_paused = true)]
async fn resume_or_forget_interrupts_a_slow_set_and_fails_outstanding_pushes() {
    for forget in [false, true] {
        let phone = FakePhone::new(None, [Session::SlowSet, Session::Connected]);
        let (send, mut commands) = mpsc::channel(4);
        let (wake, signals) = mpsc::channel(1);
        let (events, mut statuses) = mpsc::unbounded_channel();
        let (result, mut results) = mpsc::unbounded_channel();
        let mut inputs = rig(&mut commands, signals, None);
        let emit_status = collect_status(events);
        let emit = |event| match event {
            DeviceEvent::PushResult(id, reply) => {
                result.send((id, reply)).unwrap();
                ControlFlow::Continue(())
            }
            event => emit_status(event),
        };
        let verify = async {
            status(&mut statuses, ConnState::Connecting).await;
            status(&mut statuses, ConnState::Connected { wireless: true }).await;
            send.send(DeviceCommand::Push(Clip::Text("In flight".into()), Some(1)))
                .await
                .unwrap();
            timeout(Duration::from_secs(1), phone.request_started.notified())
                .await
                .unwrap();
            for (id, text) in [(2, "First waiting copy"), (3, "Second waiting copy")] {
                send.try_send(DeviceCommand::Push(Clip::Text(text.into()), Some(id)))
                    .unwrap();
            }
            // Give the worker a chance to queue the copies before interrupting.
            sleep(Duration::from_secs(1)).await;
            assert!(results.try_recv().is_err());
            let interrupted = Instant::now();
            if forget {
                send.send(DeviceCommand::Forget).await.unwrap();
                status(&mut statuses, ConnState::Setup).await;
            } else {
                wake.send(HostSignal::Resumed).await.unwrap();
                status(&mut statuses, ConnState::Connecting).await;
                status(&mut statuses, ConnState::Connected { wireless: true }).await;
            }
            assert_eq!(Instant::now(), interrupted);
            for id in 1..=3 {
                assert_eq!(
                    results.try_recv(),
                    Ok((Some(id), Err(PasteboardError::Disconnected)))
                );
            }
            assert!(results.try_recv().is_err());
            send.send(DeviceCommand::Shutdown).await.unwrap();
        };
        tokio::join!(run(&phone, &mut inputs, &emit), verify);
        assert!(phone.pushed.into_inner() == [Clip::Text("In flight".into())]);
    }
}

#[tokio::test(start_paused = true)]
async fn file_commands_reach_the_connected_session_and_do_not_survive_its_end() {
    let phone = FakePhone::new(None, [Session::SlowFetch, Session::Connected]);
    let (send, mut commands) = mpsc::channel(4);
    let (wake, signals) = mpsc::channel(1);
    let (events, mut statuses) = mpsc::unbounded_channel();
    let (result, mut results) = mpsc::unbounded_channel();
    let mut inputs = rig(&mut commands, signals, None);
    let emit_status = collect_status(events);
    let emit = |event| match event {
        DeviceEvent::FileResult(id, reply) => {
            result.send((id, reply.err().unwrap())).unwrap();
            ControlFlow::Continue(())
        }
        event => emit_status(event),
    };
    let offer = FileOffer {
        name: "sample.pdf".to_owned(),
        uti: "com.adobe.pdf".to_owned(),
        size: 123,
        change: None,
        data: None,
    };
    let verify = async {
        status(&mut statuses, ConnState::Connecting).await;
        status(&mut statuses, ConnState::Connected { wireless: true }).await;
        send.send(DeviceCommand::FetchOffer(offer.clone(), 1))
            .await
            .unwrap();
        timeout(Duration::from_secs(1), phone.request_started.notified())
            .await
            .unwrap();
        assert!(phone.fetched.borrow().as_slice() == [offer.clone()]);
        send.send(DeviceCommand::FetchOffer(offer.clone(), 2))
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), results.recv())
                .await
                .unwrap(),
            Some((2, FileTransferError::Busy)),
        );
        phone.release_request.notify_one();
        assert_eq!(
            timeout(Duration::from_secs(1), results.recv())
                .await
                .unwrap(),
            Some((1, FileTransferError::OfferExpired)),
        );
        // A valid result releases the request slot. Cancelling the next request
        // reports a disconnection and does not requeue it on the next session.
        send.send(DeviceCommand::FetchOffer(offer.clone(), 3))
            .await
            .unwrap();
        timeout(Duration::from_secs(1), phone.request_started.notified())
            .await
            .unwrap();
        wake.send(HostSignal::Resumed).await.unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), results.recv())
                .await
                .unwrap(),
            Some((3, FileTransferError::Disconnected)),
        );
        status(&mut statuses, ConnState::Connecting).await;
        status(&mut statuses, ConnState::Connected { wireless: true }).await;
        send.send(DeviceCommand::Shutdown).await.unwrap();
    };
    tokio::join!(run(&phone, &mut inputs, &emit), verify);
    assert!(phone.fetched.into_inner() == [offer.clone(), offer]);
}

/// Seconds between consecutive `Waiting` statuses while the phone stays away.
async fn waiting_gaps(phone: &FakePhone, count: usize) -> Vec<u64> {
    let (_send, mut commands) = mpsc::channel(8);
    let mut inputs = Inputs {
        commands: &mut commands,
        signals: Some(()),
        detached: None,
    };
    let times = RefCell::new(Vec::new());
    run(phone, &mut inputs, &|event| {
        if matches!(event, DeviceEvent::Status(ConnState::Waiting, _)) {
            let mut times = times.borrow_mut();
            times.push(Instant::now());
            if times.len() > count {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    })
    .await;
    times
        .into_inner()
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).as_secs())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn an_absent_phone_is_polled_slowly_after_the_fast_retries() {
    let misses = FAST_RETRIES as usize + 2;
    let phone = FakePhone::new(None, (0..misses + 1).map(|_| Session::Failure(OFFLINE)));
    let gaps = waiting_gaps(&phone, misses).await;
    let mut expected = vec![RETRY_DELAY.as_secs(); FAST_RETRIES as usize];
    expected.extend([IDLE_RETRY.as_secs(); 2]);
    assert_eq!(gaps, expected);
}

#[tokio::test(start_paused = true)]
async fn an_advertisement_ends_the_slow_wait_early() {
    let misses = FAST_RETRIES as usize + 1;
    let phone = FakePhone::new(None, (0..misses + 1).map(|_| Session::Failure(OFFLINE)));
    // Listening starts after the minimum delay; the announcement then ends the 30 s wait.
    phone.advertises_after.set(Some(Duration::from_secs(10)));
    let gaps = waiting_gaps(&phone, misses).await;
    assert_eq!(gaps.last(), Some(&(RETRY_DELAY.as_secs() + 10)));
}
