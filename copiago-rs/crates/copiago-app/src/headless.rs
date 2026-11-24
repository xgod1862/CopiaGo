//! Connect the Win32 main loop to the device thread for console operation.

mod clipboard;
mod window;

use std::{
    fmt,
    io::{self, Write},
    ops::ControlFlow,
    sync::mpsc,
    thread,
    time::Instant,
};

use copiago_core::{
    clip::Clip,
    paths::{self, RunMode},
    settings::Settings,
};
use copiago_device::{
    ConnState, DeviceCommand, DeviceEvent, DeviceRunOptions, HostSignal, run_device,
};
use copiago_win::clipboard::Clipboard;
use tokio::sync::mpsc as tokio_mpsc;

use self::{
    clipboard::{ClipboardSync, PhoneClip, PushQueueError},
    window::{ConsoleControl, NetworkNotifications, PowerNotifications, Window, WindowEvent},
};

pub(super) fn run() -> bool {
    let log = Log(Instant::now());
    match start(&log) {
        Ok(()) => true,
        Err(error) => {
            log.event("fatal", format_args!("{error}"));
            false
        }
    }
}

fn start(log: &Log) -> Result<(), HostError> {
    let data_dir = paths::data_dir(RunMode::Development)
        .map_err(|error| HostError::os("resolve_data_dir", error))?;
    paths::ensure_data_dir(&data_dir).map_err(|error| HostError::os("prepare_data_dir", error))?;
    let settings = Settings::load(&data_dir);
    let options = DeviceRunOptions {
        language: settings.language.resolve(),
        persist_device_data: true,
    };

    let window = Window::new().map_err(|error| HostError::os("create_window", error))?;
    let clipboard =
        Clipboard::new(window.handle()).map_err(|_| HostError::new("listen_clipboard"))?;
    let _console = log.registration("console_control", ConsoleControl::register(&window));
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
    let (commands, mut receive_commands) = tokio_mpsc::channel(4);
    let (signals, receive_signals) = tokio_mpsc::channel(2);
    let (send_events, events) = mpsc::channel();
    let waker = window.waker();
    let worker = thread::Builder::new()
        .name("copiago-device".to_owned())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map(|runtime| {
                    runtime.block_on(run_device(
                        &data_dir,
                        options,
                        &mut receive_commands,
                        receive_signals,
                        |event| {
                            if send_events.send(event).is_err() || !waker.device_event() {
                                return ControlFlow::Break(());
                            }
                            ControlFlow::Continue(())
                        },
                    ));
                });
            waker.worker_exited();
            result
        })
        .map_err(|error| HostError::os("start_device_thread", error))?;

    let mut host = Host {
        clipboard,
        sync,
        commands,
        signals,
        events,
        log,
    };
    let result = host.message_loop(&window);
    let _ = host.commands.try_send(DeviceCommand::Shutdown);
    drop(host);
    worker
        .join()
        .map_err(|_| HostError::new("device_thread_panicked"))?
        .map_err(|error| HostError::os("start_device_runtime", error))?;
    result?;
    log.event("stopped", format_args!(""));
    Ok(())
}

struct Host<'a> {
    clipboard: Clipboard,
    sync: ClipboardSync,
    commands: tokio_mpsc::Sender<DeviceCommand>,
    signals: tokio_mpsc::Sender<HostSignal>,
    events: mpsc::Receiver<DeviceEvent>,
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
                    if let Ok(event) = self.events.try_recv() {
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
            }
        }
        Ok(())
    }

    fn signal(&mut self, signal: HostSignal) {
        if self.signals.try_send(signal).is_ok() {
            self.sync.connected(false);
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
            .should_read_windows(self.clipboard.sequence_number())
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
        match self.sync.queue_push(read, &self.commands) {
            Ok(Some(kind)) => self
                .log
                .event("push_queued", format_args!("kind={}", kind.as_str())),
            Ok(None) => {}
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
                self.log.event("status", format_args!("state={state:?}"));
            }
            DeviceEvent::Clip {
                clip,
                while_disconnected,
            } => {
                self.on_phone_clipboard(clip, while_disconnected);
            }
            DeviceEvent::PushResult(Ok(())) => self.log.event("push_complete", format_args!("")),
            DeviceEvent::PushResult(Err(error)) => {
                self.log
                    .event("push_failed", format_args!("reason={error}"));
            }
            DeviceEvent::FileOffer(_) if self.sync.offers_files() => {
                self.log.event("file_offered", format_args!("kind=file"));
            }
            DeviceEvent::FileOffer(_) => {}
            DeviceEvent::Unsupported { .. } => self.log.event("unsupported", format_args!("")),
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
