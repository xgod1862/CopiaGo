//! History popup driven by synthetic previews. No database, clipboard or phone.
//! Usage: history_page [phone|pc|offer|progress|retry|off|many] [left] [top] [--timed|--probe]

#[cfg(not(windows))]
fn main() {
    eprintln!("This example requires Windows");
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    example::run()
}

#[cfg(windows)]
mod example {
    use std::{
        env,
        time::{Duration, Instant},
    };

    use chrono::{Datelike, Days, Local, NaiveDate};
    use copiago_core::{
        history::{Entry, Source},
        i18n::{Language, message},
    };
    use copiago_ui::{
        animation::MotionPolicy,
        components::{ProgressState, Status},
        composition::{CompositionThread, POPUP_SETTINGS_CHANGED, PopupHost, PopupScene},
        history::{
            HistoryAction, HistoryLayout, HistoryList, HistoryOffer, HistoryPage, HistoryStatus,
            HistoryTarget, HistoryView, StatusTone, ThumbnailPixels, ThumbnailRequest,
        },
    };
    use copiago_win::image::decode_scaled;
    use windows::{
        Win32::{
            Foundation::{E_FAIL, HWND, LPARAM, POINT, WPARAM},
            UI::{
                HiDpi::{
                    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                    GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
                },
                Input::KeyboardAndMouse::{
                    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DELETE, VK_DOWN, VK_ESCAPE,
                    VK_LEFT, VK_RETURN, VK_RIGHT, VK_UP,
                },
                WindowsAndMessaging::{
                    DispatchMessageW, GetMessageW, GetWindowRect, MSG, PostMessageW, SendMessageW,
                    SetTimer, TranslateMessage, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK,
                    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_SETFOCUS,
                    WM_TIMER,
                },
            },
        },
        core::{Error, Result},
    };
    use windows_numerics::Vector2;

    const TIMER: usize = 1;
    const WM_MOUSELEAVE: u32 = 0x02A3;
    const PREVIEW: &[u8] = include_bytes!("assets/history-preview.png");
    // A full-size picture for the image card, decoded like an original from history.
    const ORIGINAL: &[u8] = include_bytes!("assets/history-original.png");

    /// COPIAGO_SHOT_LANG=en renders the English UI with English sample data.
    fn language() -> Language {
        if env::var_os("COPIAGO_SHOT_LANG").is_some_and(|value| value == "en") {
            Language::En
        } else {
            Language::ZhCn
        }
    }

    fn t(zh: &'static str, en: &'static str) -> &'static str {
        if language() == Language::En { en } else { zh }
    }

    fn fake_page(sample: &str) -> (HistoryPage, [HistoryList; 2]) {
        let today = Local::now().date_naive();
        let date = |days| today.checked_sub_days(Days::new(days)).unwrap();
        let old = NaiveDate::from_ymd_opt(today.year() - 1, 11, 8).unwrap();
        let entry = |id, created: NaiveDate, time: &str, kind: &str, preview: &[u8]| Entry {
            id,
            created: format!("{created}T{time}"),
            kind: kind.to_owned(),
            preview: preview.to_owned(),
            size: preview.len() as u64 * 40,
            chars: (kind == "text").then(|| String::from_utf8_lossy(preview).chars().count()),
        };
        let phone = vec![
            entry(
                6,
                today,
                "14:23",
                "text",
                t(
                    "会议改到周四下午三点，请提前十分钟到五楼会议室，带上评审材料和项目进度说明。",
                    "The review moved to Thursday at 3 PM. Please arrive ten minutes early with the project notes.",
                )
                .as_bytes(),
            ),
            entry(5, today, "13:08", "image", PREVIEW),
            entry(
                4,
                date(1),
                "20:45",
                "text",
                b"https://example.com/docs/copiago/history-and-clipboard-sync",
            ),
            entry(
                3,
                date(4),
                "16:30",
                "file",
                t("项目交付清单.pdf\n1394606", "Delivery checklist.pdf\n1394606").as_bytes(),
            ),
            entry(2, old, "09:19", "text", t("跨年保留的示例条目", "An item kept from last year").as_bytes()),
        ];
        let pc = vec![
            entry(12, today, "15:04", "text", b"Release checklist: verify the portable build, inspect the release notes, and update the sample"),
            entry(11, date(1), "17:42", "image", PREVIEW),
            entry(10, date(7), "16:56", "text", t("从电脑复制的项目备注", "Project notes copied on the PC").as_bytes()),
        ];
        let mut page = HistoryPage::new(language());
        page.set_status(HistoryStatus {
            connection: Status::Connected,
            device_name: t("示例 iPhone", "iPhone").into(),
            detail: Some(message!("已连接（无线）")),
            tone: StatusTone::Success,
            ..HistoryStatus::default()
        });
        let lists = match sample {
            "off" => [HistoryList::default(), HistoryList::default()],
            "many" => {
                let images = (1..=50)
                    .rev()
                    .map(|id| entry(id, today, "13:08", "image", PREVIEW))
                    .collect();
                [
                    HistoryList {
                        enabled: true,
                        entries: images,
                    },
                    HistoryList::default(),
                ]
            }
            _ => [
                HistoryList {
                    enabled: true,
                    entries: phone,
                },
                HistoryList {
                    enabled: true,
                    entries: pc,
                },
            ],
        };
        page.set_entries(snapshot(&lists[0]), snapshot(&lists[1]));
        match sample {
            "pc" => {
                page.activate(HistoryTarget::Tab(Source::Pc), Instant::now());
            }
            "offer" => {
                page.set_offer(Some(HistoryOffer {
                    id: 7,
                    name: "演示资料.pptx".into(),
                    metadata: "PPTX · 24.0 MB".into(),
                    reason: message!("超过 {limit} MB，要同步到电脑吗？", limit = 20),
                    busy: false,
                }));
                page.set_status(HistoryStatus {
                    connection: Status::Connected,
                    device_name: "示例 iPhone".into(),
                    detail: Some(message!(
                        "收到文件「{name}」{size}，等你选择是否同步",
                        name = "演示资料.pptx",
                        size = "24.0 MB"
                    )),
                    ..HistoryStatus::default()
                });
            }
            "progress" => page.set_status(HistoryStatus {
                detail: Some(message!("正在准备 iPhone 开发组件…")),
                progress: ProgressState::Determinate(0.56),
                ..HistoryStatus::default()
            }),
            "retry" => page.set_status(HistoryStatus {
                connection: Status::Disconnected,
                detail: Some(message!(
                    "下载 iPhone 开发组件失败。已下载的部分会保留，请检查电脑网络后点「继续连接」。"
                )),
                tone: StatusTone::Warning,
                can_retry: true,
                ..HistoryStatus::default()
            }),
            _ => {}
        }
        (page, lists)
    }

    fn snapshot(list: &HistoryList) -> HistoryList {
        HistoryList {
            enabled: list.enabled,
            entries: list
                .entries
                .iter()
                .map(|entry| Entry {
                    id: entry.id,
                    created: entry.created.clone(),
                    kind: entry.kind.clone(),
                    preview: entry.preview.clone(),
                    size: entry.size,
                    chars: entry.chars,
                })
                .collect(),
        }
    }

    fn thumbnail(request: ThumbnailRequest<'_>) -> Option<ThumbnailPixels> {
        let picture = decode_scaled(ORIGINAL, request.width, request.height)
            .or_else(|_| decode_scaled(request.preview.unwrap_or(PREVIEW), 88, 56))
            .ok()?;
        let (width, height) = picture.dimensions();
        Some(ThumbnailPixels {
            width,
            height,
            bgra: picture.pixels().to_vec(),
        })
    }

    struct Driver {
        page: HistoryPage,
        lists: [HistoryList; 2],
        layout: HistoryLayout,
        view: Option<HistoryView>,
        policy: MotionPolicy,
        pressed: Option<HistoryTarget>,
        hovered: Option<HistoryTarget>,
    }

    impl Driver {
        fn mount(&mut self, scene: &PopupScene) -> Result<()> {
            let now = Instant::now();
            let mut view = HistoryView::mount(scene, &self.page, &self.layout, self.policy, now)?;
            view.refresh_thumbnails(scene, &self.page, now, thumbnail)?;
            self.view = Some(view);
            Ok(())
        }

        fn activate(
            &mut self,
            scene: &PopupScene,
            target: HistoryTarget,
            now: Instant,
        ) -> Result<()> {
            let before = self.page.selected();
            let action = self.page.activate(target, now);
            if let Some(action) = action {
                self.apply(action);
            }
            self.refresh(scene, now)?;
            let view = self.view.as_mut().expect("history example is visible");
            if before != self.page.selected()
                && let Some(id) = self.page.selected()
            {
                view.reveal(id, now)?;
            }
            if let Some(action) = action {
                view.show_feedback(scene, &self.page, action)?;
            }
            Ok(())
        }

        fn apply(&mut self, action: HistoryAction) {
            // Simulate the host acknowledgment with in-memory test data only.
            println!("{action:?}");
            let index = |source| usize::from(source == Source::Pc);
            match action {
                HistoryAction::Copy(id) => self.page.copied(id),
                HistoryAction::Send(id) => self.page.sent(id),
                HistoryAction::Delete(id) => {
                    self.lists[index(id.source)]
                        .entries
                        .retain(|entry| entry.id != id.id);
                    self.page
                        .set_entries(snapshot(&self.lists[0]), snapshot(&self.lists[1]));
                }
                HistoryAction::Clear(source) => {
                    self.lists[index(source)].entries.clear();
                    self.page
                        .set_entries(snapshot(&self.lists[0]), snapshot(&self.lists[1]));
                }
                HistoryAction::Enable(source) => {
                    self.lists[index(source)].enabled = true;
                    self.page
                        .set_entries(snapshot(&self.lists[0]), snapshot(&self.lists[1]));
                }
                HistoryAction::RespondToOffer { accept, .. } => {
                    self.page.set_offer(None);
                    self.page.set_status(HistoryStatus {
                        connection: Status::Connected,
                        device_name: "示例 iPhone".into(),
                        detail: Some(if accept {
                            message!("已同步文件「{name}」", name = "演示资料.pptx")
                        } else {
                            message!("已忽略「{name}」", name = "演示资料.pptx")
                        }),
                        ..HistoryStatus::default()
                    });
                }
                _ => {}
            }
        }

        fn refresh(&mut self, scene: &PopupScene, now: Instant) -> Result<()> {
            self.layout = self.page.layout()?;
            let view = self.view.as_mut().expect("history example is visible");
            view.refresh(scene, &self.page, &self.layout, now)?;
            view.refresh_thumbnails(scene, &self.page, now, thumbnail)
        }

        fn input(&mut self, scene: &PopupScene, msg: MSG) -> Result<()> {
            let now = Instant::now();
            let point = POINT {
                x: msg.lParam.0 as i16 as i32,
                y: (msg.lParam.0 >> 16) as i16 as i32,
            };
            let view = self.view.as_mut().expect("history example is visible");
            match msg.message {
                WM_MOUSEWHEEL => view.scroll_wheel((msg.wParam.0 >> 16) as i16, now)?,
                WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    if msg.message == WM_MOUSEMOVE {
                        // SAFETY: The live input window owns this leave notification.
                        unsafe {
                            TrackMouseEvent(&mut TRACKMOUSEEVENT {
                                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                                dwFlags: TME_LEAVE,
                                hwndTrack: msg.hwnd,
                                dwHoverTime: 0,
                            })?;
                        }
                    }
                    let hit = view.hit(scene.client_to_dips(point), now);
                    self.hovered = hit;
                    let target = match msg.message {
                        WM_LBUTTONDOWN => {
                            self.pressed = hit;
                            hit.filter(|target| matches!(target, HistoryTarget::Row(_)))
                        }
                        WM_LBUTTONUP => {
                            let pressed = self.pressed.take();
                            hit.filter(|hit| pressed == Some(*hit))
                                .filter(|target| !matches!(target, HistoryTarget::Row(_)))
                        }
                        WM_LBUTTONDBLCLK => hit.and_then(|target| match target {
                            HistoryTarget::Row(id) => {
                                Some(HistoryTarget::Action(HistoryAction::Copy(id)))
                            }
                            _ => {
                                self.pressed = hit;
                                None
                            }
                        }),
                        _ => None,
                    };
                    if let Some(target) = target {
                        self.activate(scene, target, now)?;
                    }
                }
                WM_MOUSELEAVE => self.hovered = None,
                WM_KEYDOWN => {
                    let key = msg.wParam.0 as u16;
                    let target = match key {
                        key if key == VK_LEFT.0 => Some(HistoryTarget::Tab(Source::Phone)),
                        key if key == VK_RIGHT.0 => Some(HistoryTarget::Tab(Source::Pc)),
                        key if key == VK_RETURN.0 => self
                            .page
                            .selected()
                            .map(|id| HistoryTarget::Action(HistoryAction::Copy(id))),
                        key if key == VK_DELETE.0 => self
                            .page
                            .selected()
                            .map(|id| HistoryTarget::Action(HistoryAction::Delete(id))),
                        key if key == VK_UP.0 || key == VK_DOWN.0 => {
                            self.page
                                .move_selection(if key == VK_UP.0 { -1 } else { 1 });
                            self.page.selected().map(HistoryTarget::Row)
                        }
                        _ => None,
                    };
                    if let Some(target) = target {
                        self.activate(scene, target, now)?;
                        if let HistoryTarget::Row(id) = target {
                            self.view.as_mut().unwrap().reveal(id, now)?;
                        }
                    }
                }
                POPUP_SETTINGS_CHANGED => {
                    self.policy = MotionPolicy::system();
                    view.set_motion_policy(self.policy);
                    self.refresh(scene, now)?;
                }
                WM_TIMER => {
                    if self.page.expire_confirmation(now) {
                        self.refresh(scene, now)?;
                    } else {
                        view.refresh_thumbnails(scene, &self.page, now, thumbnail)?;
                    }
                }
                _ => {}
            }
            self.view
                .as_mut()
                .unwrap()
                .set_interaction(self.hovered, self.pressed, Instant::now())
        }
    }

    pub(super) fn run() -> Result<()> {
        // SAFETY: Set DPI awareness before creating the example windows.
        unsafe {
            if let Err(error) =
                SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
                && !AreDpiAwarenessContextsEqual(
                    GetThreadDpiAwarenessContext(),
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                )
                .as_bool()
            {
                return Err(error);
            }
        }
        let mut args = env::args().skip(1);
        let sample = args.next().unwrap_or_else(|| "phone".into());
        let position = POINT {
            x: args.next().and_then(|arg| arg.parse().ok()).unwrap_or(160),
            y: args.next().and_then(|arg| arg.parse().ok()).unwrap_or(120),
        };
        let mode = args.next();
        let probe = mode.as_deref() == Some("--probe");
        let timed = mode.as_deref() == Some("--timed");
        let (page, lists) = fake_page(if probe { "many" } else { &sample });
        let mut driver = Driver {
            layout: page.layout()?,
            page,
            lists,
            view: None,
            hovered: None,
            pressed: None,
            policy: if timed || probe {
                MotionPolicy::Reduced
            } else {
                MotionPolicy::system()
            },
        };
        let thread = CompositionThread::new()?;
        let mut host = PopupHost::new(&thread)?;
        let started = Instant::now();
        host.show(position, driver.layout.bounds(), false, |scene| {
            driver.mount(scene)
        })?;
        let elapsed = started.elapsed().as_millis();
        println!("build_ms={elapsed} dpi={}", host.scene().unwrap().dpi());
        let hwnd = host.hwnd();
        let mut rect = Default::default();
        // SAFETY: The timer belongs to this input window and the rectangle is initialized.
        unsafe {
            GetWindowRect(hwnd, &mut rect)?;
            if SetTimer(Some(hwnd), TIMER, 32, None) == 0 {
                return Err(Error::from_win32());
            }
        }
        println!(
            "card={},{},{},{}",
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top
        );
        assert_probe_thumbnails(&host, &mut driver, probe)?;
        let started = Instant::now();
        let mut stage = 0;
        let mut msg = MSG::default();
        loop {
            // SAFETY: The message buffer is valid on the UI thread for this queue.
            let read = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
            if read < 0 {
                return Err(Error::from_win32());
            }
            if read == 0 {
                break;
            }
            if host.handle_message(&msg, |scene| driver.mount(scene))? && host.scene().is_none() {
                driver.view = None;
                if !probe && !timed {
                    break;
                }
            }
            if msg.hwnd == hwnd {
                if let Some(scene) = host.scene() {
                    driver.input(scene, msg)?;
                }
                if msg.message == WM_TIMER
                    && probe
                    && advance_probe(
                        &mut host,
                        &mut driver,
                        position,
                        started.elapsed(),
                        &mut stage,
                    )?
                {
                    break;
                }
                if msg.message == WM_TIMER
                    && timed
                    && host.scene().is_some()
                    && started.elapsed() >= Duration::from_secs(9)
                {
                    driver.view = None;
                    host.hide();
                    println!("hidden");
                }
                if timed && started.elapsed() >= Duration::from_secs(14) {
                    break;
                }
            }
            // SAFETY: The message was read from the current thread's queue.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }

    fn assert_probe_thumbnails(
        host: &PopupHost<'_>,
        driver: &mut Driver,
        probe: bool,
    ) -> Result<()> {
        if !probe {
            return Ok(());
        }
        let scene = host.scene().unwrap();
        let view = driver.view.as_mut().unwrap();
        let now = Instant::now();
        let point = Vector2::new(344.0, driver.layout.viewport().y + 32.0);
        assert_eq!(
            view.hit(point, now),
            driver
                .page
                .selected()
                .map(|id| HistoryTarget::Action(HistoryAction::Copy(id)))
        );
        view.restore_scroll(1000.0, now)?;
        let mut decoded = 0;
        view.refresh_thumbnails(scene, &driver.page, now, |request| {
            decoded += 1;
            thumbnail(request)
        })?;
        assert!((1..=6).contains(&decoded));
        println!("decoded_on_jump={decoded}");
        let previous = driver.page.selected().unwrap();
        view.restore_scroll(0.0, now)?;
        let delete = view.hit(Vector2::new(306.0, point.Y), now);
        assert_eq!(
            delete,
            Some(HistoryTarget::Action(HistoryAction::Delete(previous)))
        );
        driver.activate(scene, delete.unwrap(), now)?;
        let current = driver.page.selected().unwrap();
        assert_eq!(current.id, previous.id - 1);
        assert_eq!(
            driver.view.as_ref().unwrap().hit(point, now),
            Some(HistoryTarget::Action(HistoryAction::Copy(current)))
        );
        println!("delete_reflow_id=true");
        Ok(())
    }

    fn advance_probe(
        host: &mut PopupHost<'_>,
        driver: &mut Driver,
        position: POINT,
        elapsed: Duration,
        stage: &mut u8,
    ) -> Result<bool> {
        let window: HWND = host.hwnd();
        let visible = host.scene().is_some();
        match (*stage, elapsed.as_millis()) {
            (0, 80..) => send_focus(window, WM_KILLFOCUS),
            (1, 140..) => {
                assert!(visible, "dismissed before 150 ms");
                send_focus(window, WM_SETFOCUS);
            }
            (2, 400..) => {
                assert!(visible, "renewed focus did not cancel dismissal");
                send_focus(window, WM_KILLFOCUS);
            }
            (3, 700..) => {
                assert!(!visible, "focus loss did not dismiss");
                host.set_dismiss_on_focus_loss(false);
                host.show(position, driver.layout.bounds(), false, |scene| {
                    driver.mount(scene)
                })?;
                send_focus(window, WM_KILLFOCUS);
            }
            (4, 1000..) => {
                assert!(visible, "focus-loss dismissal was disabled");
                // SAFETY: Queue a synthetic Esc for this example's input window only.
                unsafe {
                    PostMessageW(
                        Some(window),
                        WM_KEYDOWN,
                        WPARAM(usize::from(VK_ESCAPE.0)),
                        LPARAM(0),
                    )?
                };
            }
            (5, 1200..) => {
                if visible {
                    return Err(Error::new(E_FAIL, "Esc did not dismiss"));
                }
                println!("focus_delay=true refocus_cancel=true opt_out=true escape=true");
                return Ok(true);
            }
            _ => return Ok(false),
        }
        *stage += 1;
        Ok(false)
    }

    fn send_focus(window: HWND, message: u32) {
        // SAFETY: Only notify our own input window, without changing actual focus.
        unsafe {
            SendMessageW(window, message, Some(WPARAM(0)), Some(LPARAM(0)));
        }
    }
}
