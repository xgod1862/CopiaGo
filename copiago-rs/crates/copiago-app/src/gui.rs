//! The CopiaGo notification-area application.

mod actions;
mod driver;
mod popup;
mod status;
mod sync;

use std::{cell::OnceCell, io, path::PathBuf, time::Instant};

use copiago_core::{
    DeviceInfo, diagnostic, files,
    history::History,
    i18n::{Language, Message, tr},
    paths,
};
use copiago_device::{DeviceRunOptions, HostSignal, has_pairing_records};
use copiago_ui::{
    history::{HistoryAction, HistoryPage, HistoryTarget, StatusTone},
    settings_page::{ConnectionState, SettingsState, StartupState},
};
use copiago_win::{
    clipboard::Clipboard,
    startup,
    tray::{NotificationKind, Tray, TrayAction, TrayMenuLabels},
    update::UpdateCheck,
};

use self::{
    driver::DriverInstall,
    popup::{Input, Models, Popup, Screen, Target},
    status::ConnectionFeedback,
    sync::{FileUi, PushTracker},
};
use crate::{
    VERSION,
    clipboard::ClipboardSync,
    launch::Launch,
    window::{
        self, DesktopMessage, NetworkNotifications, PowerNotifications, WM_APP_TRAY, Window,
        WindowEvent,
    },
    worker::DeviceWorker,
};

pub(super) fn run(launch: Launch, minimized: bool) -> bool {
    match start(launch, minimized) {
        Ok(()) => true,
        Err(error) => {
            report_error("desktop", &error);
            false
        }
    }
}

fn start(launch: Launch, minimized: bool) -> io::Result<()> {
    diagnostic::event(format_args!("event=start mode=tray version={VERSION}"));
    let data_dir = launch.data_dir(false)?;
    paths::ensure_data_dir(&data_dir)?;
    diagnostic::install_crash_hook(&data_dir, VERSION);
    if let Err(error) = files::cleanup(&data_dir) {
        report_error("cleanup_received", &error);
    }

    let paired = has_pairing_records(&data_dir);
    let mut settings = SettingsState::load(&data_dir, VERSION);
    let language = settings.language();
    settings.set_connection(ConnectionState {
        device: paired.then(|| DeviceInfo::load(&data_dir)),
        driver: if paired {
            Default::default()
        } else {
            driver::detect()
        },
        ..ConnectionState::default()
    });
    if !launch.isolated
        && let Some(startup) = register("read_startup", read_startup())
    {
        settings.set_startup(startup);
    }
    let window = Window::new_gui()?;
    let clipboard = Clipboard::new(window.handle()).map_err(io::Error::other)?;
    let tray = Tray::new(
        &launch.instance,
        window.handle(),
        WM_APP_TRAY,
        menu_labels(language),
    )?;
    tray.set_notifications_enabled(settings.settings().notifications);
    let _power = register("power_notifications", PowerNotifications::register(&window));
    let _network = register(
        "network_notifications",
        NetworkNotifications::register(&window),
    );

    let device_options = DeviceRunOptions {
        language,
        follow_saved_language: true,
        ..DeviceRunOptions::default()
    };
    let worker = DeviceWorker::start(data_dir.clone(), device_options, window.waker())?;
    let waker = window.waker();
    let update = UpdateCheck::start(VERSION, settings.settings().update_check, move || {
        waker.update()
    });
    let composition = OnceCell::new();
    let mut host = Host {
        history: History::new(&data_dir),
        data_dir,
        clipboard_sync: ClipboardSync::new(
            settings.settings().clone(),
            clipboard.sequence_number(),
        ),
        clipboard,
        tray,
        update,
        history_page: HistoryPage::new(language),
        settings,
        worker: &worker,
        popup: Popup::new(&composition),
        connection: ConnectionFeedback::default(),
        driver_install: None,
        history_notice: None,
        offer: FileUi::default(),
        pushes: PushTracker::default(),
        last_alert: None,
        copy_hide_at: None,
        activate: launch.activate,
        isolated: launch.isolated,
    };
    host.publish();
    diagnostic::event(format_args!("event=ready paired={paired}"));
    if !minimized || !paired {
        host.show(Screen::History, Instant::now());
    }

    let result = host.message_loop(&window, launch.instance.activation_handle());
    // Stop clipboard listening and unpublish the icon before joining the worker.
    drop(host);
    worker.shutdown()?;
    result?;
    diagnostic::event(format_args!("event=stopped"));
    Ok(())
}

struct Host<'a> {
    data_dir: PathBuf,
    clipboard: Clipboard,
    clipboard_sync: ClipboardSync,
    tray: Tray,
    update: UpdateCheck,
    settings: SettingsState,
    history: History,
    history_page: HistoryPage,
    worker: &'a DeviceWorker,
    popup: Popup<'a>,
    connection: ConnectionFeedback,
    offer: FileUi,
    pushes: PushTracker,
    driver_install: Option<DriverInstall>,
    history_notice: Option<(Message, StatusTone)>,
    last_alert: Option<Message>,
    copy_hide_at: Option<Instant>,
    activate: bool,
    isolated: bool,
}

impl Host<'_> {
    fn message_loop(
        &mut self,
        window: &Window,
        activation: windows_sys::Win32::Foundation::HANDLE,
    ) -> io::Result<()> {
        loop {
            let now = Instant::now();
            self.tick(now);
            if let Some(event) = window.take_event() {
                match event {
                    WindowEvent::Tray(TrayAction::Exit) | WindowEvent::Close => break,
                    WindowEvent::Tray(action) => self.tray_action(action, now),
                    WindowEvent::Clipboard => self.on_windows_copy(now),
                    WindowEvent::Device => {
                        let mut changed = false;
                        while let Ok(event) = self.worker.events.try_recv() {
                            self.device_event(event, now);
                            changed = true;
                        }
                        if changed {
                            self.redraw(now);
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
                    WindowEvent::WorkerExited => {
                        return Err(io::Error::other("device thread exited"));
                    }
                    WindowEvent::Update => self.show_release(now),
                    WindowEvent::FileAnswer => {}
                }
                continue;
            }

            match window::next_message(Some(activation), self.deadline())? {
                DesktopMessage::Activate => self.show(Screen::History, Instant::now()),
                DesktopMessage::Quit => break,
                DesktopMessage::Timeout => {}
                DesktopMessage::Native(message) => {
                    let now = Instant::now();
                    let result = self.popup.handle(
                        &message,
                        Models {
                            history: &self.history_page,
                            store: &self.history,
                            settings: &self.settings,
                        },
                        now,
                    );
                    match result {
                        Ok(Input::Dispatch) => window::dispatch(&message),
                        Ok(Input::Target(target)) => self.activate_target(target, now),
                        Ok(Input::MoveSelection(step)) => self.move_selection(step, now),
                        Ok(Input::Handled) => {
                            if !self.popup.visible() {
                                self.cancel_popup_confirmations();
                            }
                        }
                        Err(error) => self.render_failed(error),
                    }
                }
            }
        }
        Ok(())
    }

    fn tray_action(&mut self, action: TrayAction, now: Instant) {
        match action {
            TrayAction::OpenHistory => self.show(Screen::History, now),
            TrayAction::OpenSettings => self.show(Screen::Settings, now),
            TrayAction::UseLatest => {
                self.use_latest();
                self.redraw(now);
            }
            TrayAction::Exit => unreachable!("handled by the message loop"),
        }
    }

    fn show(&mut self, screen: Screen, now: Instant) {
        let screen = if screen == Screen::History && self.settings.connection().show_guide() {
            Screen::Guide
        } else {
            screen
        };
        self.cancel_popup_confirmations();
        self.history_page.reset_for_show();
        self.refresh_entries();
        if screen == Screen::Guide
            && self.driver_install.is_none()
            && self.settings.connection().device.is_none()
        {
            let mut connection = self.settings.connection().clone();
            let driver = driver::detect();
            // A port can listen while enumeration fails. Keep that diagnosis
            // until a connection succeeds or an explicit repair is verified.
            if !matches!(connection.driver,
                copiago_ui::settings_page::DriverState::Failed
                    | copiago_ui::settings_page::DriverState::ServiceUnavailable
            ) {
                connection.driver = driver;
                self.settings.set_connection(connection);
            }
        }
        self.publish();
        let result = self.popup.show(
            &self.tray,
            screen,
            self.activate,
            Models {
                history: &self.history_page,
                store: &self.history,
                settings: &self.settings,
            },
            now,
        );
        if let Err(error) = result {
            self.render_failed(error);
        } else {
            let page = match screen {
                Screen::History => "history",
                Screen::Settings => "settings",
                Screen::Guide => "pairing",
            };
            diagnostic::event(format_args!("event=popup_shown page={page}"));
        }
    }

    fn navigate(&mut self, screen: Screen, now: Instant) {
        self.cancel_popup_confirmations();
        self.publish();
        let result = self.popup.navigate(
            screen,
            Models {
                history: &self.history_page,
                store: &self.history,
                settings: &self.settings,
            },
            now,
        );
        if let Err(error) = result {
            self.render_failed(error);
        }
    }

    fn move_selection(&mut self, step: isize, now: Instant) {
        self.history_page.move_selection(step);
        self.redraw(now);
        if self.popup.visible()
            && let Some(id) = self.history_page.selected()
        {
            let target = Target::History(HistoryTarget::Row(id));
            let result = self.popup.focus(target).and_then(|()| {
                self.popup.reveal(
                    target,
                    Models {
                        history: &self.history_page,
                        store: &self.history,
                        settings: &self.settings,
                    },
                    now,
                )
            });
            if let Err(error) = result {
                self.render_failed(error);
            }
        }
    }

    fn cancel_popup_confirmations(&mut self) {
        self.settings.cancel_confirmation();
        self.history_page.cancel_confirmation();
        self.copy_hide_at = None;
    }

    fn close_popup(&mut self, now: Instant) {
        self.cancel_popup_confirmations();
        if let Err(error) = self.popup.close(now) {
            self.render_failed(error);
        }
    }

    fn redraw(&mut self, now: Instant) {
        self.publish();
        let result = self.popup.render(
            Models {
                history: &self.history_page,
                store: &self.history,
                settings: &self.settings,
            },
            now,
        );
        if let Err(error) = result {
            self.render_failed(error);
        }
    }

    fn render_failed(&mut self, error: windows::core::Error) {
        diagnostic::event(format_args!("event=ui_error code={:#010x}", error.code().0));
        self.popup.hide();
        self.cancel_popup_confirmations();
    }

    fn deadline(&self) -> Option<Instant> {
        [
            self.popup.deadline(),
            self.settings.confirmation_deadline(),
            self.history_page.clear_deadline(),
            self.copy_hide_at,
            self.connection.deadline(self.settings.connection().phase),
            self.driver_install.as_ref().map(DriverInstall::deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn tick(&mut self, now: Instant) {
        let mut changed = self.settings.expire_confirmation(now)
            | self.history_page.expire_confirmation(now)
            | self.connection_tick(now);
        if let Some((state, notice)) = self.driver_install.as_mut().and_then(|job| job.poll(now)) {
            self.driver_install = None;
            self.set_driver_result(state, notice);
            changed = true;
        }
        if self.copy_hide_at.is_some_and(|deadline| now >= deadline) {
            self.close_popup(now);
        }
        if changed {
            self.redraw(now);
        }
        let was_closing = self.popup.closing_until().is_some();
        let result = self.popup.tick(
            Models {
                history: &self.history_page,
                store: &self.history,
                settings: &self.settings,
            },
            now,
        );
        if let Err(error) = result {
            self.render_failed(error);
        }
        if was_closing && self.popup.closing_until().is_none() {
            diagnostic::event(format_args!("event=popup_hidden"));
            release_memory();
        }
    }

    fn signal(&mut self, signal: HostSignal) {
        if self.worker.signals.try_send(signal).is_ok() {
            self.clipboard_sync.connected(false);
            diagnostic::event(format_args!("event=host_signal signal={signal:?}"));
        }
    }

    fn set_status(&mut self, message: Message, tone: StatusTone) {
        self.history_notice = Some((message, tone));
    }

    fn notify(&mut self, message: &Message, alert: bool, kind: NotificationKind) {
        if (alert && self.last_alert.as_ref() == Some(message)) || (self.isolated && !self.activate)
        {
            return;
        }
        if alert {
            self.last_alert = Some(message.clone());
        }
        self.tray
            .notify(&message.render(self.settings.language()), kind);
    }

    fn show_release(&mut self, now: Instant) {
        let version = self
            .update
            .release()
            .map(|release| release.version().to_owned());
        self.settings.set_available_version(version.clone());
        self.history_page.set_update(version);
        self.redraw(now);
    }

    fn feedback(&mut self, action: HistoryAction) {
        if let Err(error) = self.popup.feedback(&self.history_page, action) {
            self.render_failed(error);
        }
    }
}

fn menu_labels(language: Language) -> TrayMenuLabels<'static> {
    let label = |source: &'static str| match tr!(language, source) {
        std::borrow::Cow::Borrowed(label) => label,
        std::borrow::Cow::Owned(_) => unreachable!("menu labels do not use substitution"),
    };
    TrayMenuLabels {
        history: label("剪贴板历史"),
        use_latest: label("使用 iPhone 上最新复制的内容"),
        settings: label("连接与设置"),
        exit: label("退出"),
    }
}

fn read_startup() -> io::Result<StartupState> {
    startup::startup_enabled().map(|enabled| StartupState::Available { enabled })
}

fn register<T>(action: &'static str, result: io::Result<T>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            report_error(action, &error);
            None
        }
    }
}

pub(super) fn report_error(action: &'static str, error: &io::Error) {
    diagnostic::event(format_args!(
        "event=host_error action={action} os_code={:?} kind={:?}",
        error.raw_os_error(),
        error.kind()
    ));
}

/// Hand memory used by the panel back to Windows once it is hidden. Rendering and image
/// decoding leave freed heap blocks committed, and the tray then sits on pages it no longer
/// needs. Both calls are advisory; failures change nothing.
fn release_memory() {
    use windows_sys::Win32::System::{
        Memory::{GetProcessHeap, HeapCompact},
        Threading::{GetCurrentProcess, SetProcessWorkingSetSize},
    };
    // SAFETY: The process heap and current-process pseudo handle are always valid. Passing
    // usize::MAX for both sizes asks Windows to trim the working set.
    unsafe {
        HeapCompact(GetProcessHeap(), 0);
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}
