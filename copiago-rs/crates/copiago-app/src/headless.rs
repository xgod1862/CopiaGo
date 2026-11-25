//! Connect the Win32 main loop to the device thread for console operation.

mod files;

use std::{
    fmt,
    io::{self, IsTerminal, Write},
    sync::mpsc,
    time::Instant,
};

use copiago_core::{
    clip::{Clip, FileOffer},
    files::{self as file_storage, Confirmation, OfferDecision, ReceivedFile},
    paths,
    settings::Settings,
};
use copiago_device::{
    ConnState, DeviceCommand, DeviceEvent, DeviceRunOptions, FileTransferError, HostSignal,
    PasteboardError,
};
use copiago_win::clipboard::Clipboard;
use tokio::sync::mpsc as tokio_mpsc;

use self::files::{Answer, PendingOffer};
use crate::{
    VERSION,
    clipboard::{ClipboardSync, PhoneClip, PushQueueError},
    launch::Launch,
    window::{ConsoleControl, NetworkNotifications, PowerNotifications, Window, WindowEvent},
    worker::DeviceWorker,
};

pub(super) fn run(launch: Launch) -> bool {
    let log = Log(Instant::now());
    match start(launch, &log) {
        Ok(()) => true,
        Err(error) => {
            log.event("fatal", format_args!("{error}"));
            false
        }
    }
}

fn start(launch: Launch, log: &Log) -> Result<(), HostError> {
    let data_dir = launch
        .data_dir(true)
        .map_err(|error| HostError::os("resolve_data_dir", error))?;
    paths::ensure_data_dir(&data_dir).map_err(|error| HostError::os("prepare_data_dir", error))?;
    copiago_core::diagnostic::init(&data_dir, VERSION);
    copiago_core::diagnostic::install_crash_hook(&data_dir, VERSION);
    if let Err(error) = file_storage::cleanup(&data_dir) {
        log.event(
            "file_cleanup_failed",
            format_args!("{}", HostError::os("cleanup_received", error)),
        );
    }
    let settings = Settings::load(&data_dir);
    let options = DeviceRunOptions {
        language: settings.language.resolve(),
        ..DeviceRunOptions::default()
    };

    let window = Window::new().map_err(|error| HostError::os("create_window", error))?;
    let clipboard =
        Clipboard::new(window.handle()).map_err(|_| HostError::new("listen_clipboard"))?;
    let _console = log.registration("console_control", ConsoleControl::register(&window));
    let file_answers = if io::stdin().is_terminal() {
        log.registration("file_confirmation", files::read_answers(window.waker()))
    } else {
        None
    };
    let _power = log.registration("power_notifications", PowerNotifications::register(&window));
    let _network = log.registration(
        "network_notifications",
        NetworkNotifications::register(&window),
    );
    log.event(
        "ready",
        format_args!("pull={} push={}", settings.pull, settings.push),
    );

    let sync = ClipboardSync::new(settings, clipboard.sequence_number());
    let worker = DeviceWorker::start(data_dir, options, window.waker())
        .map_err(|error| HostError::os("start_device_thread", error))?;

    let mut host = Host {
        clipboard,
        sync,
        worker: &worker,
        file_answers,
        pending_offer: PendingOffer::default(),
        log,
    };
    let result = host.message_loop(&window);
    worker
        .shutdown()
        .map_err(|error| HostError::os("stop_device_thread", error))?;
    result?;
    log.event("stopped", format_args!(""));
    Ok(())
}

struct Host<'a> {
    clipboard: Clipboard,
    sync: ClipboardSync,
    worker: &'a DeviceWorker,
    file_answers: Option<mpsc::Receiver<Answer>>,
    pending_offer: PendingOffer,
    log: &'a Log,
}

impl Host<'_> {
    fn message_loop(&mut self, window: &Window) -> Result<(), HostError> {
        while let Some(event) = window
            .next_event()
            .map_err(|error| HostError::os("get_message", error))?
        {
            match event {
                WindowEvent::Clipboard => self.on_windows_clipboard(),
                WindowEvent::Device => {
                    if let Ok(event) = self.worker.events.try_recv() {
                        self.on_device_event(event);
                    }
                }
                WindowEvent::Resume => {
                    window.cancel_network_timer();
                    self.signal(HostSignal::Resumed);
                }
                WindowEvent::NetworkChanged => {
                    if !window.debounce_network_change() {
                        self.signal(HostSignal::NetworkChanged);
                    }
                }
                WindowEvent::NetworkSettled => {
                    if window.cancel_network_timer() {
                        self.signal(HostSignal::NetworkChanged);
                    }
                }
                WindowEvent::WorkerExited => return Err(HostError::new("device_thread_exited")),
                WindowEvent::Close | WindowEvent::Tray(_) | WindowEvent::Update => {}
                WindowEvent::FileAnswer => {
                    if let Some(answer) = self
                        .file_answers
                        .as_ref()
                        .and_then(|answers| answers.try_recv().ok())
                    {
                        self.on_file_answer(answer);
                    }
                }
            }
        }
        Ok(())
    }

    fn signal(&mut self, signal: HostSignal) {
        if self.worker.signals.try_send(signal).is_ok() {
            self.sync.connected(false);
            self.pending_offer.clear();
            let event = match signal {
                HostSignal::Resumed => "resume",
                HostSignal::NetworkChanged => "network_changed",
            };
            self.log.event(event, format_args!(""));
        }
    }

    fn on_windows_clipboard(&mut self) {
        if !self
            .sync
            .should_read_windows(self.clipboard.sequence_number(), false)
        {
            return;
        }
        let read = match self.clipboard.read() {
            Ok(read) => read,
            Err(error) => {
                self.log
                    .event("clipboard_read_failed", format_args!("{error}"));
                return;
            }
        };
        let Some((clip, kind)) = self.sync.read_windows(read) else {
            return;
        };
        match self
            .sync
            .queue_push(clip, None, false, &self.worker.commands)
        {
            Ok(true) => self
                .log
                .event("push_queued", format_args!("kind={}", kind.as_str())),
            Ok(false) => {}
            Err(error) => {
                let reason = match error {
                    PushQueueError::Full => "queue_full",
                    PushQueueError::Closed => "queue_closed",
                };
                self.log
                    .event("push_failed", format_args!("reason={reason}"));
            }
        }
    }

    fn on_device_event(&mut self, event: DeviceEvent) {
        match event {
            DeviceEvent::Status(ConnState::Connected { wireless }, _) => {
                self.sync.connected(true);
                let transport = if wireless { "wifi" } else { "usb" };
                self.log
                    .event("connected", format_args!("transport={transport}"));
            }
            DeviceEvent::Status(state, _) => {
                self.sync.connected(false);
                self.pending_offer.clear();
                self.log.event("status", format_args!("state={state:?}"));
            }
            DeviceEvent::Clip {
                clip,
                while_disconnected,
            } => {
                self.pending_offer.clear();
                self.on_phone_clipboard(clip, while_disconnected);
            }
            DeviceEvent::PushResult(_, Ok(())) => self.log.event("push_complete", format_args!("")),
            DeviceEvent::PushResult(_, Err(error)) => {
                if error == PasteboardError::PushBusy {
                    self.sync.allow_resend();
                }
                self.log
                    .event("push_failed", format_args!("reason={error}"));
            }
            DeviceEvent::FileOffer(offer) => self.on_file_offer(offer),
            DeviceEvent::FileProgress(id, progress) => self.log.event(
                "file_progress",
                format_args!(
                    "id={id} bytes={} total={}",
                    progress.received_bytes, progress.total_bytes
                ),
            ),
            DeviceEvent::FileResult(_, result) => self.on_file_result(result),
            DeviceEvent::Unsupported { .. } => {
                self.pending_offer.clear();
                self.log.event("unsupported", format_args!(""));
            }
            DeviceEvent::DeviceInfo(_) => self.log.event("device_info", format_args!("")),
            DeviceEvent::DeveloperServicesMissing { wireless } => {
                let transport = if wireless { "wifi" } else { "usb" };
                self.log.event(
                    "developer_services_missing",
                    format_args!("transport={transport}"),
                );
            }
            DeviceEvent::SetupProgress(stage, _) => {
                self.log
                    .event("setup_progress", format_args!("stage={stage:?}"));
            }
            DeviceEvent::PairingPrompt(_) => self.log.event("pairing", format_args!("")),
            DeviceEvent::Error(_) => self.log.event("device_error", format_args!("")),
            DeviceEvent::ForgetResult(ok) => self.log.event("forget", format_args!("success={ok}")),
        }
    }

    fn on_file_offer(&mut self, offer: FileOffer) {
        self.pending_offer.clear();
        let bytes = offer.size;
        match self.sync.decide_offer(bytes) {
            OfferDecision::Ignore => {}
            OfferDecision::TooLarge => self.log.event(
                "file_failed",
                format_args!(
                    "reason=too_large bytes={bytes} limit={}",
                    file_storage::MAX_FILE_BYTES
                ),
            ),
            OfferDecision::Receive => {
                let id = self.pending_offer.allocate_id();
                self.fetch_offer(offer, id);
            }
            OfferDecision::Confirm(reason) => {
                let id = self.pending_offer.set(offer);
                let reason = match reason {
                    Confirmation::AutoSyncOff => "auto_sync_off",
                    Confirmation::UnknownSize => "unknown_size",
                    Confirmation::OverThreshold => "over_threshold",
                };
                let interactive = self.file_answers.is_some();
                self.log.event(
                    "file_waiting",
                    format_args!("id={id} bytes={bytes} reason={reason} interactive={interactive}"),
                );
                if interactive {
                    self.log.event(
                        "file_confirmation",
                        format_args!("accept {id} / ignore {id}"),
                    );
                }
            }
        }
    }

    fn on_file_answer(&mut self, answer: Answer) {
        let Some(offer) = self.pending_offer.take(answer.id) else {
            self.on_file_result(Err(FileTransferError::OfferExpired));
            return;
        };
        if answer.accept {
            self.fetch_offer(offer, answer.id);
        } else {
            self.log
                .event("file_ignored", format_args!("id={}", answer.id));
        }
    }

    fn fetch_offer(&self, offer: FileOffer, id: u64) {
        let bytes = offer.size;
        match self
            .worker
            .commands
            .try_send(DeviceCommand::FetchOffer(offer, id))
        {
            Ok(()) => self
                .log
                .event("file_requested", format_args!("id={id} bytes={bytes}")),
            Err(tokio_mpsc::error::TrySendError::Full(_)) => {
                self.log
                    .event("file_failed", format_args!("reason=queue_full"));
            }
            Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
                self.log
                    .event("file_failed", format_args!("reason=disconnected"));
            }
        }
    }

    fn on_file_result(&mut self, result: Result<ReceivedFile, FileTransferError>) {
        match result {
            Ok(file) => match self.clipboard.write_files(&[file.path().to_path_buf()]) {
                Ok(()) => {
                    self.sync.mark_written(self.clipboard.sequence_number());
                    let bytes = file.bytes();
                    file.keep();
                    self.log
                        .event("file_complete", format_args!("bytes={bytes}"));
                }
                Err(error) => self.log.event(
                    "file_failed",
                    format_args!("reason=clipboard error={error}"),
                ),
            },
            Err(error) => {
                let reason = match error {
                    FileTransferError::OfferExpired => "offer_expired",
                    FileTransferError::TooLarge => "too_large",
                    FileTransferError::Disconnected => "disconnected",
                    _ => "transfer_failed",
                };
                self.log
                    .event("file_failed", format_args!("reason={reason} error={error}"));
            }
        }
    }

    fn on_phone_clipboard(&mut self, clip: Clip, while_disconnected: bool) {
        match self
            .sync
            .receive_phone(clip, while_disconnected, self.clipboard.sequence_number())
        {
            PhoneClip::Ignore => {}
            PhoneClip::Hold(kind) => {
                self.log.event(
                    "pull_held",
                    format_args!(
                        "kind={} while_disconnected={while_disconnected}",
                        kind.as_str()
                    ),
                );
            }
            PhoneClip::Write(clip, kind) => {
                let result = match clip {
                    Clip::Text(text) => self.clipboard.write_text(&text),
                    Clip::Image { bytes, .. } => self.clipboard.write_image(&bytes),
                    Clip::File(_) => unreachable!("the headless host only writes text and images"),
                };
                match result {
                    Ok(()) => {
                        self.sync.mark_written(self.clipboard.sequence_number());
                        self.log.event(
                            "pull_complete",
                            format_args!(
                                "kind={} while_disconnected={while_disconnected}",
                                kind.as_str()
                            ),
                        );
                    }
                    Err(error) => self.log.event(
                        "pull_failed",
                        format_args!("kind={} reason={error}", kind.as_str()),
                    ),
                }
            }
        }
    }
}

struct HostError {
    action: &'static str,
    code: Option<i32>,
}

impl HostError {
    fn new(action: &'static str) -> Self {
        Self { action, code: None }
    }

    fn os(action: &'static str, error: io::Error) -> Self {
        Self {
            action,
            code: error.raw_os_error(),
        }
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "action={}", self.action)?;
        if let Some(code) = self.code {
            write!(f, " os_code={code}")?;
        }
        Ok(())
    }
}

struct Log(Instant);

impl Log {
    fn event(&self, event: &str, details: fmt::Arguments<'_>) {
        copiago_core::diagnostic::event(format_args!("event={event} {details}"));
        let mut out = io::stdout().lock();
        let _ = writeln!(
            out,
            "elapsed_ms={} event={event} {details}",
            self.0.elapsed().as_millis()
        );
        let _ = out.flush();
    }

    fn registration<T>(&self, action: &'static str, result: io::Result<T>) -> Option<T> {
        match result {
            Ok(registration) => {
                self.event("registered", format_args!("action={action}"));
                Some(registration)
            }
            Err(error) => {
                self.event(
                    "registration_failed",
                    format_args!("{}", HostError::os(action, error)),
                );
                None
            }
        }
    }
}
