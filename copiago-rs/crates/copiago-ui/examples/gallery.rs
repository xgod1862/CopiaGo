//! CopiaGo components with synthetic data.
//! Click the status button to cycle connection and progress states. Scroll
//! the rows, insert/delete an item, and click the copy or feedback button.
//! --probe plays the interactions automatically without activating the window.

#[cfg(not(windows))]
fn main() {
    eprintln!("This example requires Windows");
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    gallery::run()
}

#[cfg(windows)]
mod gallery {
    use std::{env, time::Instant};

    use copiago_ui::{
        animation::{Animator, MotionPolicy},
        components::{
            Button, ButtonStyle, Interaction, ItemAction, ItemActions, ItemContent, ItemHit,
            ItemKind, ItemLayout, LIGHT_THEME, ListItem, ProgressBar, ProgressState, Rect, Removal,
            ScrollLayout, ScrollView, Status, StatusPill, Toast, Toggle,
        },
        composition::{
            CompositionThread, Font, POPUP_SETTINGS_CHANGED, PopupBounds, PopupHost, PopupScene,
            TextStyle, rgb, rounded_rectangle,
        },
    };
    use windows::{
        Win32::{
            Foundation::{E_FAIL, E_INVALIDARG, HWND, LPARAM, POINT},
            Graphics::Gdi::ScreenToClient,
            UI::{
                HiDpi::{
                    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                    GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
                },
                Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE},
                WindowsAndMessaging::{
                    DispatchMessageW, GetForegroundWindow, GetMessageW, KillTimer, MSG, SetTimer,
                    TranslateMessage, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
                    WM_MOUSEWHEEL, WM_TIMER,
                },
            },
        },
        core::{Error, Interface, Result},
    };
    use windows_numerics::{Vector2, Vector3};

    const CARD: PopupBounds = PopupBounds::new(392.0, 498.0, 24.0);
    const VIEWPORT: Rect = Rect::new(0.0, 128.0, CARD.width, 300.0);
    const CAPACITY: usize = 6;
    const TIMER: usize = 1;
    const WM_MOUSELEAVE: u32 = 0x02A3;
    const STATES: [(Status, &str, ProgressState); 5] = [
        (Status::Disconnected, "未连接", ProgressState::Hidden),
        (
            Status::Connecting,
            "正在连接 iPhone",
            ProgressState::Indeterminate,
        ),
        (
            Status::Connecting,
            "正在准备",
            ProgressState::Determinate(0.35),
        ),
        (
            Status::Connecting,
            "正在验证",
            ProgressState::Determinate(0.80),
        ),
        (
            Status::Connected,
            "已连接 · iPhone",
            ProgressState::Complete,
        ),
    ];

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Target {
        Button(usize),
        Toggle,
        Row(u32),
        Action(u32, ItemAction),
    }

    struct Gallery {
        animator: Animator,
        status: StatusPill,
        progress: ProgressBar,
        toast: Toast,
        buttons: [Button; 5],
        toggle: Toggle,
        scroll: ScrollView,
        rows: Vec<(u32, ListItem)>,
        status_step: usize,
        next_id: u32,
        hovered: Option<Target>,
        pressed: Option<Target>,
    }

    impl Gallery {
        fn build(scene: &PopupScene, policy: MotionPolicy) -> Result<Self> {
            let theme = &LIGHT_THEME;
            let animator = Animator::new(scene.compositor(), policy)?;
            label(
                scene,
                "CopiaGo 组件展示",
                18.0,
                21.0,
                18.0,
                Font::Semibold,
                185.0,
            )?;
            label(scene, "自动接收", 254.0, 69.0, 12.5, Font::Regular, 72.0)?;
            let status = StatusPill::new(
                scene,
                &animator,
                Vector2::new(374.0, 20.0),
                166.0,
                STATES[0].0,
                STATES[0].1,
                theme,
            )?;
            let progress = ProgressBar::new(scene, Vector2::new(18.0, 110.0), 356.0, theme)?;
            let toast = Toast::new(scene, Vector2::new(196.0, 393.0), 320.0, theme)?;
            let buttons = [
                Button::text(
                    scene,
                    Rect::new(18.0, 62.0, 107.0, 34.0),
                    ButtonStyle::Primary,
                    "切换状态",
                    theme,
                )?,
                Button::text(
                    scene,
                    Rect::new(133.0, 62.0, 106.0, 34.0),
                    ButtonStyle::Neutral,
                    "复制反馈",
                    theme,
                )?,
                Button::text(
                    scene,
                    Rect::new(18.0, 446.0, 96.0, 34.0),
                    ButtonStyle::Primary,
                    "插入条目",
                    theme,
                )?,
                Button::text(
                    scene,
                    Rect::new(122.0, 446.0, 105.0, 34.0),
                    ButtonStyle::Danger,
                    "删除首条",
                    theme,
                )?,
                Button::text(
                    scene,
                    Rect::new(308.0, 446.0, 66.0, 34.0),
                    ButtonStyle::Neutral,
                    "关闭",
                    theme,
                )?,
            ];
            let toggle = Toggle::new(
                scene,
                Vector2::new(330.0, 66.0),
                Rect::new(251.0, 62.0, 123.0, 34.0),
                false,
                true,
                theme,
            )?;
            let mut scroll = ScrollView::new(scene, ScrollLayout::new(VIEWPORT, 0.0)?, theme)?;
            let mut rows = Vec::new();
            let mut y = 4.0;
            for id in 0..CAPACITY as u32 {
                let row = make_row(scene, id, y)?;
                scroll.content().Children()?.InsertAtTop(row.visual())?;
                y += row.layout().height + ItemLayout::GAP;
                rows.push((id, row));
            }
            scroll.set_content_height(y, Instant::now())?;

            let children = scene.content().Children()?;
            children.InsertAtTop(status.visual())?;
            children.InsertAtTop(progress.visual())?;
            children.InsertAtTop(toggle.visual())?;
            for button in &buttons {
                children.InsertAtTop(button.visual())?;
            }
            children.InsertAtTop(scroll.visual())?;
            children.InsertAtTop(toast.visual())?;

            Ok(Self {
                animator,
                status,
                progress,
                toast,
                buttons,
                toggle,
                scroll,
                rows,
                status_step: 0,
                next_id: CAPACITY as u32,
                hovered: None,
                pressed: None,
            })
        }

        fn hit(&self, point: Vector2, now: Instant) -> Option<Target> {
            if let Some(index) = self
                .buttons
                .iter()
                .position(|button| button.hit_test(point))
            {
                return Some(Target::Button(index));
            }
            if self.toggle.hit_test(point) {
                return Some(Target::Toggle);
            }
            self.rows.iter().find_map(|(id, row)| {
                self.scroll.hit_item(point, row, now).map(|hit| match hit {
                    ItemHit::Body => Target::Row(*id),
                    ItemHit::Action(action) => Target::Action(*id, action),
                })
            })
        }

        fn interaction(&self, target: Target) -> Interaction {
            if self.hovered != Some(target) {
                Interaction::Idle
            } else if self.pressed == Some(target) {
                Interaction::Pressed
            } else {
                Interaction::Hovered
            }
        }

        fn apply_pointer(&mut self) -> Result<()> {
            for index in 0..self.buttons.len() {
                let state = self.interaction(Target::Button(index));
                self.buttons[index].set_interaction(&self.animator, state)?;
            }
            let hovered = self.hovered;
            let pressed = self.pressed;
            for (id, row) in &mut self.rows {
                let state = if hovered == Some(Target::Row(*id)) {
                    if hovered == pressed {
                        Interaction::Pressed
                    } else {
                        Interaction::Hovered
                    }
                } else {
                    Interaction::Idle
                };
                row.set_interaction(&self.animator, state)?;
                let action = match hovered {
                    Some(Target::Action(row_id, action)) if row_id == *id => Some((
                        action,
                        if hovered == pressed {
                            Interaction::Pressed
                        } else {
                            Interaction::Hovered
                        },
                    )),
                    _ => None,
                };
                row.set_action_interaction(&self.animator, action)?;
            }
            Ok(())
        }

        fn pointer(
            &mut self,
            message: u32,
            point: Vector2,
            now: Instant,
        ) -> Result<Option<Target>> {
            self.hovered = if message == WM_MOUSELEAVE {
                None
            } else {
                self.hit(point, now)
            };
            let activated = match message {
                WM_LBUTTONDOWN => {
                    self.pressed = self.hovered;
                    None
                }
                WM_LBUTTONUP => self
                    .pressed
                    .take()
                    .filter(|target| Some(*target) == self.hovered),
                WM_MOUSELEAVE => {
                    self.pressed = None;
                    None
                }
                _ => None,
            };
            self.apply_pointer()?;
            Ok(activated)
        }

        fn activate(&mut self, scene: &PopupScene, target: Target, now: Instant) -> Result<bool> {
            match target {
                Target::Button(0) => {
                    self.status_step = (self.status_step + 1) % STATES.len();
                    self.show_status(scene)?;
                }
                Target::Button(1) => {
                    self.toast
                        .show(scene, &self.animator, "已复制", Some('\u{E73E}'))?;
                    self.load_thumbnail(scene)?;
                }
                Target::Button(2) => self.insert(scene, now)?,
                Target::Button(3) if !self.rows.is_empty() => self.remove(0, now)?,
                Target::Button(4) => return Ok(true),
                Target::Toggle => {
                    self.toggle
                        .set_state(&self.animator, !self.toggle.checked(), true)?
                }
                Target::Action(id, action) => {
                    if let Some(index) = self.rows.iter().position(|(item_id, _)| *item_id == id) {
                        match action {
                            ItemAction::Copy => {
                                self.rows[index].1.set_copied(scene, true)?;
                                self.toast.show(
                                    scene,
                                    &self.animator,
                                    "已复制",
                                    Some('\u{E73E}'),
                                )?;
                            }
                            ItemAction::Send => {
                                self.toast.show(
                                    scene,
                                    &self.animator,
                                    "已推送到 iPhone",
                                    Some('\u{E73E}'),
                                )?;
                            }
                            ItemAction::Delete => self.remove(index, now)?,
                        }
                    }
                }
                _ => {}
            }
            Ok(false)
        }

        fn show_status(&mut self, scene: &PopupScene) -> Result<()> {
            let (status, label, progress) = STATES[self.status_step];
            self.status
                .set_status(scene, &self.animator, status, label)?;
            self.progress.set_state(&self.animator, progress)
        }

        fn sync_motion(
            &mut self,
            scene: &PopupScene,
            policy: MotionPolicy,
            now: Instant,
        ) -> Result<()> {
            if self.animator.policy() != policy {
                self.animator.set_policy(policy);
                self.show_status(scene)?;
                self.toggle
                    .set_state(&self.animator, self.toggle.checked(), true)?;
                self.scroll
                    .scroll_to(&self.animator, self.scroll.target_offset(), now)?;
                self.reflow(now)?;
                self.apply_pointer()?;
                self.toast.dismiss()?;
            }
            Ok(())
        }

        fn insert(&mut self, scene: &PopupScene, now: Instant) -> Result<()> {
            let mut item = make_row(scene, self.next_id, 4.0)?;
            self.scroll
                .content()
                .Children()?
                .InsertAtTop(item.visual())?;
            item.animate_in(&self.animator, now)?;
            self.rows.insert(0, (self.next_id, item));
            self.next_id += 1;
            if self.rows.len() > CAPACITY {
                let (_, row) = self.rows.pop().expect("row added above");
                row.remove(&self.animator, self.scroll.content(), Removal::Fade)?;
            }
            self.reflow(now)
        }

        fn remove(&mut self, index: usize, now: Instant) -> Result<()> {
            let (_, row) = self.rows.remove(index);
            row.remove(&self.animator, self.scroll.content(), Removal::Collapse)?;
            self.reflow(now)
        }

        fn reflow(&mut self, now: Instant) -> Result<()> {
            let mut y = 4.0;
            for (_, row) in &mut self.rows {
                row.move_to(&self.animator, Vector2::new(18.0, y), now)?;
                y += row.layout().height + ItemLayout::GAP;
            }
            self.scroll.set_content_height(y, now)
        }

        fn load_thumbnail(&mut self, scene: &PopupScene) -> Result<()> {
            if let Some((_, image)) = self
                .rows
                .iter_mut()
                .find(|(_, row)| row.layout().kind == ItemKind::Image)
            {
                let thumb = rounded_rectangle(
                    scene.compositor(),
                    Vector2::new(56.0, 36.0),
                    10.0,
                    rgb(61, 90, 138),
                )?;
                image.set_thumbnail(&self.animator, Some(&thumb.visual.cast()?))?;
            }
            Ok(())
        }

        fn probe_click(
            &mut self,
            scene: &PopupScene,
            point: Vector2,
            expected: Target,
            now: Instant,
        ) -> Result<()> {
            assert_eq!(self.hit(point, now), Some(expected));
            self.pointer(WM_LBUTTONDOWN, point, now)?;
            assert_eq!(self.pointer(WM_LBUTTONUP, point, now)?, Some(expected));
            assert!(!self.activate(scene, expected, now)?);
            Ok(())
        }

        fn probe(
            &mut self,
            scene: &PopupScene,
            step: usize,
            now: Instant,
        ) -> Result<(&'static str, u32)> {
            let state = Vector2::new(40.0, 79.0);
            let result = match step {
                0 => {
                    self.pointer(WM_MOUSEMOVE, state, now)?;
                    ("hover", 230)
                }
                1 => {
                    self.pointer(WM_LBUTTONDOWN, state, now)?;
                    ("press", 200)
                }
                2 => {
                    assert_eq!(
                        self.pointer(WM_LBUTTONUP, state, now)?,
                        Some(Target::Button(0))
                    );
                    self.activate(scene, Target::Button(0), now)?;
                    ("indeterminate", 1450)
                }
                3 => {
                    self.probe_click(scene, state, Target::Button(0), now)?;
                    ("determinate_35", 850)
                }
                4 => {
                    self.probe_click(scene, state, Target::Button(0), now)?;
                    ("determinate_80", 850)
                }
                5 => {
                    self.probe_click(scene, state, Target::Button(0), now)?;
                    self.probe_click(scene, Vector2::new(266.0, 79.0), Target::Toggle, now)?;
                    assert!(self.toggle.checked());
                    ("complete_toggle", 1800)
                }
                6 => {
                    assert_eq!(self.status.visual().Children()?.Count()?, 3);
                    let copy = Vector2::new(345.0, 155.0);
                    self.probe_click(scene, copy, Target::Action(0, ItemAction::Copy), now)?;
                    self.load_thumbnail(scene)?;
                    ("feedback_thumbnail", 1850)
                }
                7 => {
                    self.pointer(WM_LBUTTONDOWN, Vector2::new(345.0, 155.0), now)?;
                    self.scroll.scroll_wheel(&self.animator, -150, now)?;
                    ("scroll", 525)
                }
                8 => {
                    assert_eq!(self.scroll.offset_at(now), 70.0);
                    assert!(
                        self.pointer(WM_LBUTTONUP, Vector2::new(345.0, 155.0), now)?
                            .is_none()
                    );
                    self.scroll.scroll_wheel(&self.animator, 150, now)?;
                    ("return_scroll", 500)
                }
                9 => {
                    self.probe_click(scene, Vector2::new(40.0, 464.0), Target::Button(2), now)?;
                    ("insert_evict", 575)
                }
                10 => {
                    assert_eq!(
                        self.scroll.content().Children()?.Count()? as usize,
                        CAPACITY
                    );
                    self.probe_click(scene, Vector2::new(150.0, 464.0), Target::Button(3), now)?;
                    ("delete", 575)
                }
                _ => {
                    assert_eq!(
                        self.scroll.content().Children()?.Count()? as usize,
                        CAPACITY - 1
                    );
                    self.probe_click(scene, state, Target::Button(0), now)?;
                    ("disconnected_again", 500)
                }
            };
            Ok(result)
        }
    }

    fn make_row(scene: &PopupScene, id: u32, y: f32) -> Result<ListItem> {
        let (kind, title, detail): (ItemKind, &[&str], &str) = match id % 5 {
            0 => (
                ItemKind::Text,
                &["会议纪要与产品评审", "带上新版交互稿"],
                "刚刚 · 文字",
            ),
            1 => (ItemKind::Image, &["图片"], "10 秒前 · PNG"),
            2 => (
                ItemKind::Link,
                &["github.com/xgod1862/CopiaGo"],
                "1 分钟前 · 链接",
            ),
            3 => (ItemKind::File, &["设计评审.pdf"], "3 分钟前 · 2.4 MB"),
            _ => (ItemKind::Text, &["明天上午发更新说明"], "5 分钟前 · 文字"),
        };
        let (actions, height) = if id == 0 {
            (
                ItemActions::CopySendDelete,
                ItemLayout::selected_text_height(title.len()),
            )
        } else {
            (ItemActions::None, ItemLayout::ROW_HEIGHT)
        };
        ListItem::new(
            scene,
            Vector2::new(18.0, y),
            ItemLayout::new(356.0, height, kind, actions),
            ItemContent {
                title,
                detail,
                emphasized_detail: false,
                copied: false,
            },
            &LIGHT_THEME,
        )
    }

    fn label(
        scene: &PopupScene,
        label: &str,
        x: f32,
        y: f32,
        size: f32,
        font: Font,
        width: f32,
    ) -> Result<()> {
        let label = scene.text(
            label,
            TextStyle {
                font,
                size,
                max_width: width,
                color: LIGHT_THEME.ink,
            },
        )?;
        label.visual.SetOffset(Vector3::new(x, y, 0.0))?;
        scene.content().Children()?.InsertAtTop(&label.visual)
    }

    struct Options {
        position: POINT,
        probe: bool,
        reduced: bool,
    }

    impl Options {
        fn parse() -> Result<Self> {
            let mut options = Self {
                position: POINT { x: 72, y: 72 },
                probe: false,
                reduced: false,
            };
            let invalid = || {
                Error::new(
                    E_INVALIDARG,
                    "Usage: gallery [--position X Y] [--probe] [--reduced-motion]",
                )
            };
            let mut args = env::args().skip(1);
            while let Some(argument) = args.next() {
                match argument.as_str() {
                    "--probe" => options.probe = true,
                    "--reduced-motion" => options.reduced = true,
                    "--position" => {
                        let mut coord = || {
                            args.next()
                                .and_then(|value| value.parse().ok())
                                .ok_or_else(invalid)
                        };
                        options.position = POINT {
                            x: coord()?,
                            y: coord()?,
                        };
                    }
                    _ => return Err(invalid()),
                }
            }
            Ok(options)
        }

        fn policy(&self) -> MotionPolicy {
            if self.reduced {
                MotionPolicy::Reduced
            } else {
                MotionPolicy::system()
            }
        }
    }

    pub(super) fn run() -> Result<()> {
        let options = Options::parse()?;
        // SAFETY: DPI awareness is set before creating any windows on this thread.
        unsafe {
            let context = DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2;
            if !AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), context).as_bool() {
                SetProcessDpiAwarenessContext(context)?;
            }
        }
        let start = Instant::now();
        let thread = CompositionThread::new()?;
        let mut popup = PopupHost::new(&thread)?;
        // SAFETY: Reading the foreground HWND leaves its ownership and focus unchanged.
        let foreground = unsafe { GetForegroundWindow() };
        let mut gallery = None;
        popup.show(options.position, CARD, false, |scene| {
            gallery = Some(Gallery::build(scene, options.policy())?);
            Ok(())
        })?;
        let hwnd = popup.hwnd();
        if options.probe {
            let scene = popup.scene().ok_or_else(|| Error::from_hresult(E_FAIL))?;
            println!(
                "phase=show dpi={} policy={:?} x={} y={} elapsed_ms={}",
                scene.dpi(),
                options.policy(),
                options.position.x,
                options.position.y,
                start.elapsed().as_millis(),
            );
            arm(hwnd, 450)?;
        }
        let mut step = 0;
        let mut tracking = false;
        let mut message = MSG::default();
        loop {
            // SAFETY: GetMessage writes to this stack MSG on the window's owner thread.
            let received = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
            match received {
                -1 => return Err(Error::from_win32()),
                0 => break,
                _ => {}
            }
            if popup.handle_message(&message, |scene| {
                gallery = Some(Gallery::build(scene, options.policy())?);
                Ok(())
            })? {
                if popup.scene().is_none() {
                    break;
                }
                continue;
            }
            if message.hwnd == hwnd {
                let now = Instant::now();
                if options.probe && message.message == WM_TIMER && message.wParam.0 == TIMER {
                    if step == 12 {
                        gallery = None;
                        popup.hide();
                        // SAFETY: This is a read of the foreground window for the non-activation check.
                        let unchanged = unsafe { GetForegroundWindow() } == foreground;
                        println!(
                            "phase=hidden elapsed_ms={} foreground_unchanged={unchanged}",
                            start.elapsed().as_millis()
                        );
                        arm(hwnd, 3500)?;
                    } else if step > 12 {
                        break;
                    } else if let (Some(gallery), Some(scene)) = (&mut gallery, popup.scene()) {
                        let (phase, interval) = gallery.probe(scene, step, now)?;
                        println!("phase={phase} elapsed_ms={}", start.elapsed().as_millis());
                        arm(hwnd, interval)?;
                    }
                    step += 1;
                    continue;
                }
                if let (Some(gallery), Some(scene)) = (&mut gallery, popup.scene()) {
                    match message.message {
                        POPUP_SETTINGS_CHANGED => {
                            gallery.sync_motion(scene, options.policy(), now)?
                        }
                        WM_KEYDOWN if message.wParam.0 == usize::from(VK_ESCAPE.0) => break,
                        WM_MOUSEWHEEL if !options.probe => {
                            let mut point = mouse_point(message.lParam);
                            // SAFETY: The popup's HWND is live and the POINT is writable.
                            if unsafe { ScreenToClient(hwnd, &mut point) }.as_bool()
                                && VIEWPORT.contains(scene.client_to_dips(point))
                            {
                                gallery.scroll.scroll_wheel(
                                    &gallery.animator,
                                    (message.wParam.0 >> 16) as i16,
                                    now,
                                )?;
                            }
                        }
                        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP
                            if !options.probe =>
                        {
                            if message.message == WM_MOUSEMOVE && !tracking {
                                let mut request = TRACKMOUSEEVENT {
                                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                                    dwFlags: TME_LEAVE,
                                    hwndTrack: hwnd,
                                    dwHoverTime: 0,
                                };
                                // SAFETY: The request is writable and references our popup window.
                                unsafe { TrackMouseEvent(&mut request)? };
                                tracking = true;
                            } else if message.message == WM_MOUSELEAVE {
                                tracking = false;
                            }
                            let location = scene.client_to_dips(mouse_point(message.lParam));
                            if let Some(target) = gallery.pointer(message.message, location, now)?
                                && gallery.activate(scene, target, now)?
                            {
                                break;
                            }
                        }
                        _ => {
                            dispatch(&message);
                        }
                    }
                    continue;
                }
            }
            dispatch(&message);
        }
        if options.probe {
            // SAFETY: This timer belongs to our still-live window.
            unsafe { KillTimer(Some(hwnd), TIMER)? };
        }
        Ok(())
    }

    fn mouse_point(position: LPARAM) -> POINT {
        POINT {
            x: i32::from(position.0 as i16),
            y: i32::from((position.0 >> 16) as i16),
        }
    }

    fn arm(hwnd: HWND, interval: u32) -> Result<()> {
        // SAFETY: The timer is associated with our live HWND on this thread.
        if unsafe { SetTimer(Some(hwnd), TIMER, interval, None) } == 0 {
            return Err(Error::from_win32());
        }
        Ok(())
    }

    fn dispatch(message: &MSG) {
        // SAFETY: Both functions process a message retrieved from the current thread's queue.
        unsafe {
            let _ = TranslateMessage(message);
            DispatchMessageW(message);
        }
    }
}
