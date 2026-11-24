//! Isolated native settings and pairing preview. Set ICB_DATA_DIR to a fresh
//! path named copiago-ui04-preview-* under TEMP. Use --probe for scripted states,
//! --reduced to force reduced motion, and optional left/top screen coordinates.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    preview::run()
}

#[cfg(windows)]
mod preview {
    use std::{env, fs, path::PathBuf, time::Instant};

    use copiago_core::{
        DeviceInfo,
        ddi::{DdiStage, Progress},
        i18n::{LanguagePreference, message},
        paths,
    };
    use copiago_ui::{
        animation::{Animator, MotionPolicy},
        composition::{CompositionThread, POPUP_SETTINGS_CHANGED, PopupHost, PopupScene},
        page_transition::{PageDirection, transition_pages},
        settings_page::{
            ConnectionPhase, ConnectionState, DriverState, SettingSwitch, SettingsAction,
            SettingsEffect, SettingsScreen, SettingsState, SettingsView, StartupState,
        },
    };
    use windows::{
        UI::Composition::ContainerVisual,
        Win32::{
            Foundation::{E_FAIL, E_INVALIDARG, HWND, LPARAM, POINT},
            Graphics::Gdi::ScreenToClient,
            UI::{
                HiDpi::{
                    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                    GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
                },
                WindowsAndMessaging::{
                    DispatchMessageW, GetMessageW, MSG, SetTimer, TranslateMessage, WM_KEYDOWN,
                    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_TIMER,
                },
            },
        },
        core::{Error, Result},
    };
    use windows_numerics::Vector2;

    const TIMER: usize = 1;

    struct Preview {
        animator: Animator,
        layer: ContainerVisual,
        view: SettingsView,
        pressed: Option<SettingsAction>,
    }

    impl Preview {
        fn new(
            scene: &PopupScene,
            policy: MotionPolicy,
            state: &SettingsState,
            screen: SettingsScreen,
        ) -> Result<Self> {
            let animator = Animator::new(scene.compositor(), policy)?;
            let layer = scene.compositor().CreateContainerVisual()?;
            layer.SetSize(Vector2::new(
                SettingsView::BOUNDS.width,
                SettingsView::BOUNDS.height,
            ))?;
            layer.SetClip(&scene.compositor().CreateInsetClip()?)?;
            scene.content().Children()?.InsertAtTop(&layer)?;
            let view = SettingsView::new(scene, &animator, state, screen, Instant::now())?;
            layer.Children()?.InsertAtTop(view.visual())?;
            Ok(Self {
                animator,
                layer,
                view,
                pressed: None,
            })
        }

        fn navigate(
            &mut self,
            scene: &PopupScene,
            state: &SettingsState,
            screen: SettingsScreen,
            direction: PageDirection,
        ) -> Result<()> {
            let now = Instant::now();
            let view = SettingsView::new(scene, &self.animator, state, screen, now)?;
            transition_pages(
                &self.animator,
                &self.layer,
                self.view.visual(),
                view.visual(),
                direction,
            )?;
            self.view = view;
            self.pressed = None;
            Ok(())
        }

        fn click(
            &mut self,
            scene: &PopupScene,
            state: &mut SettingsState,
            action: SettingsAction,
        ) -> Result<Option<SettingsEffect>> {
            let now = Instant::now();
            let snap = Animator::new(scene.compositor(), MotionPolicy::Reduced)?;
            self.view.reveal(&snap, action, now)?;
            let control = self
                .view
                .controls(now)
                .find(|control| control.action == action)
                .ok_or_else(|| Error::new(E_FAIL, format!("Missing control {action:?}")))?;
            let area = control
                .bounds
                .ok_or_else(|| Error::new(E_FAIL, format!("Offscreen control {action:?}")))?;
            let point = Vector2::new(area.x + area.width / 2.0, area.y + area.height / 2.0);
            assert_eq!(self.view.hit_test(point, now), Some(action));
            self.view
                .set_pointer(&self.animator, Some(point), Some(action), now)?;
            let effect = state.activate(action, now);
            self.view.update(scene, &self.animator, state, now)?;
            Ok(effect)
        }
    }

    pub(super) fn run() -> Result<()> {
        let mut args = env::args().skip(1).collect::<Vec<_>>();
        let probe = flag(&mut args, "--probe");
        let reduced = flag(&mut args, "--reduced");
        let mut coordinates = args.into_iter().map(|arg| {
            arg.parse::<i32>()
                .map_err(|_| Error::from_hresult(E_INVALIDARG))
        });
        let position = POINT {
            x: coordinates.next().transpose()?.unwrap_or(80),
            y: coordinates.next().transpose()?.unwrap_or(80),
        };
        let data = fixture_dir()?;
        let mut state = SettingsState::load(data, "2.0.0-beta");
        if probe {
            state.activate(
                SettingsAction::ChooseLanguage(LanguagePreference::ZhCn),
                Instant::now(),
            );
        }
        state.set_startup(StartupState::Available { enabled: false });
        state.set_available_version(Some("2.0.1".into()));
        state.set_connection(ConnectionState {
            phase: ConnectionPhase::Connected { wireless: true },
            device: Some(DeviceInfo {
                name: "测试 iPhone".into(),
                model: "iPhone 15 Pro Max".into(),
                os_version: "27.2.1".into(),
            }),
            ..ConnectionState::default()
        });

        // SAFETY: Set the DPI awareness for this example before creating its first window.
        unsafe {
            let context = DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2;
            if !AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), context).as_bool() {
                SetProcessDpiAwarenessContext(context)?;
            }
        }
        let policy = || {
            if reduced {
                MotionPolicy::Reduced
            } else {
                MotionPolicy::system()
            }
        };
        let started = Instant::now();
        let thread = CompositionThread::new()?;
        let mut popup = PopupHost::new(&thread)?;
        let mut live = None;
        let mut screen = SettingsScreen::Settings;
        popup.show(position, SettingsView::BOUNDS, !probe, |scene| {
            live = Some(Preview::new(scene, policy(), &state, screen)?);
            Ok(())
        })?;
        println!(
            "phase=zh_settings show_ms={}",
            started.elapsed().as_millis()
        );
        let hwnd = popup.hwnd();
        arm(hwnd, if probe { 1800 } else { 100 })?;
        let mut message = MSG::default();
        let mut step = 0;
        loop {
            // SAFETY: This thread owns the window and the message buffer is writable.
            match unsafe { GetMessageW(&mut message, None, 0, 0) }.0 {
                -1 => return Err(Error::from_win32()),
                0 => break,
                _ => {}
            }
            if popup.handle_message(&message, |scene| {
                live = Some(Preview::new(scene, policy(), &state, screen)?);
                Ok(())
            })? {
                if popup.scene().is_none() {
                    break;
                }
                continue;
            }
            if message.hwnd == hwnd && message.message == WM_TIMER && message.wParam.0 == TIMER {
                if !probe {
                    if state.expire_confirmation(Instant::now())
                        && let (Some(scene), Some(live)) = (popup.scene(), live.as_mut())
                    {
                        live.view
                            .update(scene, &live.animator, &state, Instant::now())?;
                    }
                    continue;
                }
                let (Some(scene), Some(preview)) = (popup.scene(), live.as_mut()) else {
                    break;
                };
                if step == 17 {
                    live = None;
                    popup.hide();
                    println!("phase=hidden");
                    arm(hwnd, 4000)?;
                } else {
                    run_step(scene, preview, &mut state, &mut screen, step)?;
                    arm(hwnd, if step == 2 { 3200 } else { 1200 })?;
                }
                step += 1;
                continue;
            }
            if let (Some(scene), Some(live)) = (popup.scene(), live.as_mut())
                && message.hwnd == hwnd
            {
                let now = Instant::now();
                let mut point = decode_point(message.lParam);
                match message.message {
                    WM_KEYDOWN if message.wParam.0 == 0x1B => break,
                    POPUP_SETTINGS_CHANGED => {
                        live.animator.set_policy(policy());
                        live.view.update(scene, &live.animator, &state, now)?;
                    }
                    WM_MOUSEWHEEL => {
                        // SAFETY: hwnd is live and point is a writable screen POINT.
                        if unsafe { ScreenToClient(hwnd, &mut point) }.as_bool() {
                            let delta = (message.wParam.0 >> 16) as i16;
                            live.view.scroll_wheel(&live.animator, delta, now)?;
                        }
                    }
                    WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP => {
                        let point = scene.client_to_dips(point);
                        let hover = live.view.hit_test(point, now);
                        if message.message == WM_LBUTTONDOWN {
                            live.pressed = hover;
                        }
                        if message.message == WM_LBUTTONUP
                            && let Some(action) = live
                                .pressed
                                .take()
                                .filter(|pressed| Some(*pressed) == hover)
                            && !handle_action(scene, live, &mut state, &mut screen, action, now)?
                        {
                            break;
                        }
                        live.view
                            .set_pointer(&live.animator, Some(point), live.pressed, now)?;
                    }
                    _ => {}
                }
            }
            // SAFETY: Translate and dispatch the message obtained from the current queue.
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }

    fn handle_action(
        scene: &PopupScene,
        live: &mut Preview,
        state: &mut SettingsState,
        screen: &mut SettingsScreen,
        action: SettingsAction,
        now: Instant,
    ) -> Result<bool> {
        let effect = state.activate(action, now);
        match effect {
            Some(SettingsEffect::SetStartupEnabled(enabled)) => {
                state.complete_startup(Ok(StartupState::Available { enabled }))
            }
            Some(SettingsEffect::ForgetDevice) => {
                state.complete_forget(true);
                *screen = SettingsScreen::PairingGuide;
                live.navigate(scene, state, *screen, PageDirection::Back)?;
            }
            Some(SettingsEffect::ShowSettings) => {
                *screen = SettingsScreen::Settings;
                live.navigate(scene, state, *screen, PageDirection::Forward)?;
            }
            Some(SettingsEffect::ShowPairingGuide) => {
                *screen = SettingsScreen::PairingGuide;
                live.navigate(scene, state, *screen, PageDirection::Back)?;
            }
            Some(SettingsEffect::Close | SettingsEffect::ShowHistory) => return Ok(false),
            _ => {}
        }
        live.view.update(scene, &live.animator, state, now)?;
        Ok(true)
    }

    fn run_step(
        scene: &PopupScene,
        live: &mut Preview,
        state: &mut SettingsState,
        screen: &mut SettingsScreen,
        step: u32,
    ) -> Result<()> {
        let now = Instant::now();
        let phase = match step {
            0 => {
                live.click(
                    scene,
                    state,
                    SettingsAction::ChooseLanguage(LanguagePreference::En),
                )?;
                live.click(scene, state, SettingsAction::Toggle(SettingSwitch::Pull))?;
                assert_eq!(state.language(), LanguagePreference::En.resolve());
                "en_settings"
            }
            1 => {
                live.click(scene, state, SettingsAction::Toggle(SettingSwitch::Startup))?;
                state.complete_startup(Ok(StartupState::Available { enabled: true }));
                live.click(
                    scene,
                    state,
                    SettingsAction::Toggle(SettingSwitch::UpdateCheck),
                )?;
                assert!(state.available_version().is_none());
                "general_options"
            }
            2 => {
                assert_eq!(
                    live.click(scene, state, SettingsAction::ForgetDevice)?,
                    None
                );
                assert!(state.confirming_forget(now));
                "forget_arm"
            }
            3 => {
                assert!(state.expire_confirmation(now));
                assert!(!state.confirming_forget(now));
                "forget_expired"
            }
            4 => {
                assert_eq!(
                    live.click(scene, state, SettingsAction::ForgetDevice)?,
                    None
                );
                assert_eq!(
                    live.click(scene, state, SettingsAction::ForgetDevice)?,
                    Some(SettingsEffect::ForgetDevice)
                );
                state.complete_forget(true);
                *screen = SettingsScreen::PairingGuide;
                live.navigate(scene, state, *screen, PageDirection::Back)?;
                "guide"
            }
            5 => {
                assert_eq!(
                    live.click(scene, state, SettingsAction::RequestPair)?,
                    Some(SettingsEffect::RequestPair)
                );
                "trust"
            }
            6..=8 => {
                let stage =
                    [DdiStage::Download, DdiStage::Sign, DdiStage::Install][(step - 6) as usize];
                let progress = if stage == DdiStage::Download {
                    Progress {
                        received_bytes: 7,
                        total_bytes: Some(16),
                        bytes_per_second: 1,
                    }
                } else {
                    Progress::default()
                };
                state.set_connection(ConnectionState {
                    phase: ConnectionPhase::Connecting,
                    progress: Some((stage, progress)),
                    ..ConnectionState::default()
                });
                ["download", "sign", "install"][(step - 6) as usize]
            }
            9 => {
                state.set_connection(ConnectionState {
                    phase: ConnectionPhase::DeveloperMode,
                    ..ConnectionState::default()
                });
                "developer_mode"
            }
            10 => {
                state.set_connection(ConnectionState {
                    phase: ConnectionPhase::RetryRequired,
                    detail: Some(message!("下载 iPhone 开发组件失败。已下载的部分会保留，请检查电脑网络后点「继续连接」。")),
                    ..ConnectionState::default()
                });
                "retry"
            }
            11 => {
                assert_eq!(
                    live.click(scene, state, SettingsAction::Retry)?,
                    Some(SettingsEffect::Retry)
                );
                state.set_connection(ConnectionState {
                    driver: DriverState::Missing,
                    ..ConnectionState::default()
                });
                "driver_missing"
            }
            12 => {
                assert_eq!(
                    live.click(scene, state, SettingsAction::InstallDriver)?,
                    Some(SettingsEffect::InstallDriver)
                );
                "driver_installing"
            }
            13 => {
                state.set_connection(ConnectionState {
                    driver: DriverState::Failed,
                    ..ConnectionState::default()
                });
                live.view.update(scene, &live.animator, state, now)?;
                assert_eq!(
                    live.click(scene, state, SettingsAction::OpenAppleDevicesStore)?,
                    Some(SettingsEffect::OpenAppleDevicesStore)
                );
                state.set_notice(Some(message!(
                    "安装未完成。可以在 Microsoft Store 安装「Apple Devices」后重试。"
                )));
                "driver_failed"
            }
            15 => {
                assert_eq!(
                    live.click(scene, state, SettingsAction::OpenSettings)?,
                    Some(SettingsEffect::ShowSettings)
                );
                *screen = SettingsScreen::Settings;
                live.navigate(scene, state, *screen, PageDirection::Forward)?;
                "forward_transition"
            }
            16 => {
                assert_eq!(
                    live.click(scene, state, SettingsAction::Back)?,
                    Some(SettingsEffect::ShowPairingGuide)
                );
                *screen = SettingsScreen::PairingGuide;
                live.navigate(scene, state, *screen, PageDirection::Back)?;
                "back_transition"
            }
            _ => {
                state.set_notice(None);
                state.set_connection(ConnectionState {
                    phase: ConnectionPhase::Connecting,
                    ..ConnectionState::default()
                });
                live.view.update(scene, &live.animator, state, now)?;
                state.set_connection(ConnectionState {
                    phase: ConnectionPhase::Connected { wireless: true },
                    first_connection: true,
                    device: Some(DeviceInfo::default()),
                    ..ConnectionState::default()
                });
                "complete"
            }
        };
        live.view.update(scene, &live.animator, state, now)?;
        let buttons_that_can_be_used = live
            .view
            .controls(now)
            .filter(|control| control.enabled)
            .count();
        println!(
            "phase={phase} controls={buttons_that_can_be_used} update_ms={}",
            now.elapsed().as_millis()
        );
        Ok(())
    }

    fn fixture_dir() -> Result<PathBuf> {
        let invalid = || {
            Error::new(
                E_INVALIDARG,
                "Set ICB_DATA_DIR to a fresh copiago-ui04-preview-* directory inside TEMP",
            )
        };
        let path = PathBuf::from(env::var_os("ICB_DATA_DIR").ok_or_else(invalid)?);
        let parent = path
            .parent()
            .and_then(|parent| fs::canonicalize(parent).ok())
            .ok_or_else(invalid)?;
        let parent_matches = fs::canonicalize(env::temp_dir()).is_ok_and(|temp| temp == parent);
        let path_matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("copiago-ui04-preview-"));
        if !parent_matches || !path_matches || path.exists() {
            return Err(invalid());
        }
        paths::ensure_data_dir(&path).map_err(|error| Error::new(E_FAIL, error.to_string()))?;
        Ok(path)
    }

    fn flag(args: &mut Vec<String>, name: &str) -> bool {
        let count = args.len();
        args.retain(|arg| arg != name);
        args.len() != count
    }

    fn decode_point(value: LPARAM) -> POINT {
        POINT {
            x: (value.0 as i16).into(),
            y: ((value.0 >> 16) as i16).into(),
        }
    }

    fn arm(hwnd: HWND, interval: u32) -> Result<()> {
        // SAFETY: hwnd is owned by the message thread. WM_TIMER uses no callback.
        if unsafe { SetTimer(Some(hwnd), TIMER, interval, None) } == 0 {
            return Err(Error::from_win32());
        }
        Ok(())
    }
}
