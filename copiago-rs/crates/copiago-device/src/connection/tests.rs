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
const INSTALL: Failure = Failure::Setup(DdiStage::Install);
const SIGN: Failure = Failure::Setup(DdiStage::Sign);
const DOWNLOAD: Failure = Failure::Setup(DdiStage::Download);

#[derive(Clone, Copy)]
enum Session {
    Failure(Failure),
    Connected,
    Opening,
    SlowPull,
    SlowSet,
}

#[derive(Debug, PartialEq, Eq)]
enum Call {
    Scan,
    UsbList,
    Attempt(Transport),
}

struct FakePhone {
    record: Cell<Result<&'static str, WifiProbeError>>,
    attached: Cell<Option<&'static str>>,
    sessions: RefCell<VecDeque<Session>>,
    calls: RefCell<Vec<(Instant, Call)>>,
    request_started: Notify,
    release_request: Notify,
    pushed: RefCell<Vec<Clip>>,
}

impl FakePhone {
    fn new(attached: Option<&'static str>, sessions: impl IntoIterator<Item = Session>) -> Self {
        Self {
            record: Cell::new(Ok(PHONE)),
            attached: Cell::new(attached),
            sessions: RefCell::new(sessions.into_iter().collect()),
            calls: RefCell::new(Vec::new()),
            request_started: Notify::new(),
            release_request: Notify::new(),
            pushed: RefCell::new(Vec::new()),
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

    async fn usb_id(&self, device: &PairedDevice) -> Result<Option<u32>, UsbProbeError> {
        self.record_call(Call::UsbList);
        Ok((self.attached.get() == Some(device.serial.as_str())).then_some(USB_ID))
    }

    async fn session(
        &self,
        _device: &PairedDevice,
        transport: Transport,
        _clipboard: &mut PasteboardState,
        pushes: &PushQueue,
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
            Session::Connected | Session::SlowPull | Session::SlowSet => {
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
                    let clip = pushes.recv().await;
                    self.pushed.borrow_mut().push(clip);
                    if std::mem::take(&mut slow_set) {
                        self.pause_request().await;
                    }
                    if emit(DeviceEvent::PushResult(Ok(()))).is_break() {
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
        let expected_lookup = matches!(wifi, F(error) if error.allows_usb());
        assert_eq!(
            phone
                .calls
                .borrow()
                .iter()
                .any(|(_, call)| *call == Call::UsbList),
            expected_lookup,
            "{name}",
        );
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

fn rig<'a>(
    commands: &'a mut mpsc::Receiver<DeviceCommand>,
    signals: mpsc::Receiver<HostSignal>,
    detached: Option<mpsc::Receiver<u32>>,
) -> Inputs<'a, mpsc::Receiver<HostSignal>> {
    Inputs {
        commands,
        signals: Some(signals),
        detached,
    }
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

        phone.record.set(Err(WifiProbeError::NoPairingRecord));
        send.send(DeviceCommand::Forget).await.unwrap();
        status(&mut statuses, ConnState::Setup).await;
        send.send(DeviceCommand::Shutdown).await.unwrap();
    };
    tokio::join!(run(&phone, &mut inputs, &emit), verify);
    assert_eq!(phone.attempts(), [Transport::Wifi; 3]);
    assert!(
        !phone
            .calls
            .borrow()
            .iter()
            .any(|(_, call)| *call == Call::UsbList)
    );
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
    send.try_send(DeviceCommand::Push(Clip::Text("sample".into())))
        .unwrap();
    let mut inputs = Inputs {
        commands: &mut commands,
        signals: Some(()),
        detached: None,
    };
    run(&phone, &mut inputs, &|event| {
        if let DeviceEvent::PushResult(result) = event {
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
        DeviceEvent::PushResult(reply) => {
            result.send(reply).unwrap();
            ControlFlow::Continue(())
        }
        event => emit_status(event),
    };
    let verify = async {
        status(&mut statuses, ConnState::Connecting).await;
        status(&mut statuses, ConnState::Connected { wireless: true }).await;
        for text in ["First copy", "Second copy"] {
            send.try_send(DeviceCommand::Push(Clip::Text(text.into())))
                .unwrap();
        }
        sleep(Duration::from_secs(1)).await;
        assert!(phone.pushed.borrow().is_empty());
        phone.release_request.notify_one();
        for _ in 0..2 {
            assert_eq!(
                timeout(Duration::from_secs(1), results.recv())
                    .await
                    .unwrap(),
                Some(Ok(())),
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
            DeviceEvent::PushResult(reply) => {
                result.send(reply).unwrap();
                ControlFlow::Continue(())
            }
            event => emit_status(event),
        };
        let verify = async {
            status(&mut statuses, ConnState::Connecting).await;
            status(&mut statuses, ConnState::Connected { wireless: true }).await;
            send.send(DeviceCommand::Push(Clip::Text("In flight".into())))
                .await
                .unwrap();
            timeout(Duration::from_secs(1), phone.request_started.notified())
                .await
                .unwrap();
            for text in ["First waiting copy", "Second waiting copy"] {
                send.try_send(DeviceCommand::Push(Clip::Text(text.into())))
                    .unwrap();
            }
            // Give the worker a chance to queue the copies before interrupting.
            sleep(Duration::from_secs(1)).await;
            assert!(results.try_recv().is_err());
            let interrupted = Instant::now();
            if forget {
                phone.record.set(Err(WifiProbeError::NoPairingRecord));
                send.send(DeviceCommand::Forget).await.unwrap();
                status(&mut statuses, ConnState::Setup).await;
            } else {
                wake.send(HostSignal::Resumed).await.unwrap();
                status(&mut statuses, ConnState::Connecting).await;
                status(&mut statuses, ConnState::Connected { wireless: true }).await;
            }
            assert_eq!(Instant::now(), interrupted);
            for _ in 0..3 {
                assert_eq!(results.try_recv(), Ok(Err(PasteboardError::Disconnected)),);
            }
            assert!(results.try_recv().is_err());
            send.send(DeviceCommand::Shutdown).await.unwrap();
        };
        tokio::join!(run(&phone, &mut inputs, &emit), verify);
        assert!(phone.pushed.into_inner() == [Clip::Text("In flight".into())]);
    }
}
