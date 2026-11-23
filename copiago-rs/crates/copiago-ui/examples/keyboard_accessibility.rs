//! Standalone synthetic controls for keyboard and UI Automation verification.
//! Click the card to focus it. Tab/Shift+Tab move focus, Enter/Space activate.
//! --probe starts a separate UIA client process without activating the card.

#[cfg(not(windows))]
fn main() {
    eprintln!("This example requires Windows");
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    server::run()
}

#[cfg(windows)]
#[path = "keyboard_accessibility/uia_client.rs"]
mod uia_client;

#[cfg(windows)]
mod server {
    use std::{
        env,
        process::Command,
        time::{Duration, Instant},
    };

    use copiago_ui::{
        animation::{Animator, MotionPolicy},
        components::{
            Button, ButtonStyle, ItemActions, ItemContent, ItemKind, ItemLayout, LIGHT_THEME,
            ListItem, Rect, Toggle,
        },
        composition::{
            CompositionThread, Font, POPUP_SETTINGS_CHANGED, PopupBounds, PopupHost, PopupScene,
            TextStyle,
        },
        focus::{FocusElement, FocusEvent, FocusId, FocusRole},
    };
    use windows::{
        Win32::{
            Foundation::{E_FAIL, E_INVALIDARG, HWND, LPARAM, POINT, WPARAM},
            UI::{
                HiDpi::{
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
                },
                Input::KeyboardAndMouse::VK_ESCAPE,
                WindowsAndMessaging::{
                    DispatchMessageW, GetForegroundWindow, GetMessageW, MSG, SendMessageW,
                    SetTimer, TranslateMessage, WM_KEYDOWN, WM_SETTINGCHANGE, WM_TIMER,
                },
            },
        },
        core::{Error, Result},
    };
    use windows_numerics::{Vector2, Vector3};

    pub(super) const BUTTON_NAME: &str = "启用选项";
    pub(super) const DISABLED_NAME: &str = "不可用";
    pub(super) const TOGGLE_NAME: &str = "示例选项";
    pub(super) const LIST_NAME: &str = "示例历史";
    pub(super) const ITEM_NAME: &str = "示例条目";
    const BUTTON: FocusId = FocusId(1);
    const DISABLED: FocusId = FocusId(2);
    const TOGGLE: FocusId = FocusId(3);
    const LIST: FocusId = FocusId(4);
    const ITEM: FocusId = FocusId(5);
    const BUTTON_BOUNDS: Rect = Rect::new(20.0, 20.0, 116.0, 36.0);
    const DISABLED_BOUNDS: Rect = Rect::new(144.0, 20.0, 102.0, 36.0);
    const TOGGLE_BOUNDS: Rect = Rect::new(20.0, 68.0, 226.0, 36.0);
    const ITEM_BOUNDS: Rect = Rect::new(20.0, 118.0, 226.0, 60.0);

    struct Demo {
        animator: Animator,
        toggle: Toggle,
        item: Option<ListItem>,
    }

    impl Demo {
        fn build(
            scene: &PopupScene,
            policy: MotionPolicy,
            checked: bool,
            has_item: bool,
        ) -> Result<Self> {
            let children = scene.content().Children()?;
            let animator = Animator::new(scene.compositor(), policy)?;
            let button = Button::text(
                scene,
                BUTTON_BOUNDS,
                ButtonStyle::Primary,
                BUTTON_NAME,
                &LIGHT_THEME,
            )?;
            let mut disabled = Button::text(
                scene,
                DISABLED_BOUNDS,
                ButtonStyle::Neutral,
                DISABLED_NAME,
                &LIGHT_THEME,
            )?;
            disabled.set_enabled(&animator, false)?;
            children.InsertAtTop(button.visual())?;
            children.InsertAtTop(disabled.visual())?;

            let toggle = Toggle::new(
                scene,
                Vector2::new(198.0, 73.0),
                TOGGLE_BOUNDS,
                checked,
                true,
                &LIGHT_THEME,
            )?;
            children.InsertAtTop(toggle.visual())?;
            let label = scene.text(
                TOGGLE_NAME,
                TextStyle {
                    font: Font::Regular,
                    size: 14.0,
                    max_width: 150.0,
                    color: LIGHT_THEME.ink,
                },
            )?;
            label.visual.SetOffset(Vector3::new(30.0, 77.0, 0.0))?;
            children.InsertAtTop(&label.visual)?;

            let item = if has_item {
                let item = ListItem::new(
                    scene,
                    ITEM_BOUNDS.origin(),
                    ItemLayout::new(
                        ITEM_BOUNDS.width,
                        ITEM_BOUNDS.height,
                        ItemKind::Text,
                        ItemActions::None,
                    ),
                    ItemContent {
                        title: &[ITEM_NAME],
                        detail: "Enter / Space",
                        emphasized_detail: false,
                        copied: false,
                    },
                    &LIGHT_THEME,
                )?;
                children.InsertAtTop(item.visual())?;
                Some(item)
            } else {
                None
            };
            Ok(Self {
                animator,
                toggle,
                item,
            })
        }

        fn publish(&self, popup: &PopupHost<'_>) -> Result<()> {
            let element = |id, role, name: &str, bounds, enabled| FocusElement {
                id,
                parent: (id == ITEM).then_some(LIST),
                name: name.to_owned(),
                role,
                bounds: Some(bounds),
                enabled,
            };
            let mut nodes = vec![
                element(BUTTON, FocusRole::Button, BUTTON_NAME, BUTTON_BOUNDS, true),
                element(
                    DISABLED,
                    FocusRole::Button,
                    DISABLED_NAME,
                    DISABLED_BOUNDS,
                    false,
                ),
                element(
                    TOGGLE,
                    FocusRole::Toggle {
                        checked: self.toggle.checked(),
                    },
                    TOGGLE_NAME,
                    TOGGLE_BOUNDS,
                    true,
                ),
                element(LIST, FocusRole::List, LIST_NAME, ITEM_BOUNDS, true),
            ];
            if self.item.is_some() {
                nodes.push(element(
                    ITEM,
                    FocusRole::ListItem,
                    ITEM_NAME,
                    ITEM_BOUNDS,
                    true,
                ));
            }
            popup.set_focus_elements(nodes)
        }

        fn activate(&mut self, popup: &PopupHost<'_>, id: FocusId) -> Result<()> {
            popup.notify_invoked(id);
            match id {
                BUTTON => self.toggle.set_state(&self.animator, true, true)?,
                TOGGLE => self
                    .toggle
                    .set_state(&self.animator, !self.toggle.checked(), true)?,
                ITEM => {
                    if let (Some(item), Some(scene)) = (self.item.take(), popup.scene()) {
                        scene.content().Children()?.Remove(item.visual())?;
                    }
                }
                _ => {}
            }
            self.publish(popup)
        }
    }

    pub(super) fn run() -> Result<()> {
        let mut probe = false;
        let mut reduced = false;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--probe" => probe = true,
                "--reduced-motion" => reduced = true,
                "--client" => {
                    let handle = args
                        .next()
                        .and_then(|arg| arg.parse::<usize>().ok())
                        .ok_or_else(invalid_args)?;
                    return super::uia_client::query(HWND(handle as *mut _));
                }
                _ => return Err(invalid_args()),
            }
        }
        let motion = || {
            if reduced {
                MotionPolicy::Reduced
            } else {
                MotionPolicy::system()
            }
        };
        // SAFETY: Set DPI awareness before creating a window or initializing Composition.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)? };
        let started = Instant::now();
        let thread = CompositionThread::new()?;
        let mut popup = PopupHost::new(&thread)?;
        let mut demo = None;
        let foreground = foreground_window();
        let mut rebuild = |scene: &PopupScene| {
            demo = Some(Demo::build(scene, motion(), false, true)?);
            Ok(())
        };
        popup.show(
            POINT { x: 72, y: 72 },
            PopupBounds::new(266.0, 198.0, 24.0),
            false,
            &mut rebuild,
        )?;
        demo.as_ref().unwrap().publish(&popup)?;

        let mut client = if probe {
            println!(
                "phase=visible elapsed_ms={} motion={:?}",
                started.elapsed().as_millis(),
                motion()
            );
            // SAFETY: The demo owns this HWND. Its timer and settings message target only this window.
            unsafe {
                if SetTimer(Some(popup.hwnd()), 1, 25, None) == 0 {
                    return Err(Error::from_win32());
                }
                SendMessageW(
                    popup.hwnd(),
                    WM_SETTINGCHANGE,
                    Some(WPARAM(0)),
                    Some(LPARAM(0)),
                );
            }
            let executable =
                env::current_exe().map_err(|error| Error::new(E_FAIL, error.to_string()))?;
            Some(
                Command::new(executable)
                    .args(["--client", &popup.hwnd().0.addr().to_string()])
                    .spawn()
                    .map_err(|error| Error::new(E_FAIL, error.to_string()))?,
            )
        } else {
            None
        };

        let mut confirmed = None;
        let mut hidden = None;
        let mut message = MSG::default();
        loop {
            // SAFETY: The message buffer belongs to the UI thread.
            match unsafe { GetMessageW(&mut message, None, 0, 0) }.0 {
                -1 => return Err(Error::from_win32()),
                0 => break,
                _ => {}
            }
            if let Some(event) = popup.handle_focus_message(&message)? {
                if let FocusEvent::Activated(id) = event {
                    demo.as_mut().unwrap().activate(&popup, id)?;
                }
                continue;
            }
            if popup.scene().is_some() {
                let visible = demo.as_ref().unwrap();
                let (checked, has_item) = (visible.toggle.checked(), visible.item.is_some());
                if popup.handle_message(&message, |scene| {
                    demo = Some(Demo::build(scene, motion(), checked, has_item)?);
                    Ok(())
                })? {
                    if popup.scene().is_none() {
                        break;
                    }
                    demo.as_ref().unwrap().publish(&popup)?;
                    continue;
                }
            }
            if message.hwnd == popup.hwnd() {
                match message.message {
                    WM_KEYDOWN if message.wParam.0 == usize::from(VK_ESCAPE.0) => break,
                    POPUP_SETTINGS_CHANGED => {
                        let visible = demo.as_mut().unwrap();
                        let preference = motion();
                        visible.animator.set_policy(preference);
                        visible.toggle.set_state(
                            &visible.animator,
                            visible.toggle.checked(),
                            true,
                        )?;
                        if probe {
                            println!(
                                "phase=settings motion={preference:?} duration_ms={}",
                                preference.duration_ms(180)
                            );
                        }
                        continue;
                    }
                    WM_TIMER if probe => {
                        if let Some(child) = client.as_mut()
                            && let Some(status) = child
                                .try_wait()
                                .map_err(|error| Error::new(E_FAIL, error.to_string()))?
                        {
                            if !status.success() {
                                return Err(Error::new(E_FAIL, "UI Automation client failed"));
                            }
                            println!(
                                "phase=verified elapsed_ms={} foreground_unchanged={}",
                                started.elapsed().as_millis(),
                                foreground_window() == foreground
                            );
                            confirmed = Some(Instant::now());
                            client = None;
                        }
                        if confirmed.is_some_and(|time| time.elapsed() >= Duration::from_secs(2))
                            && hidden.is_none()
                        {
                            drop(demo.take());
                            popup.hide();
                            hidden = Some(Instant::now());
                            println!("phase=hidden elapsed_ms={}", started.elapsed().as_millis());
                        }
                        if hidden.is_some_and(|time| time.elapsed() >= Duration::from_secs(2)) {
                            break;
                        }
                        if started.elapsed() > Duration::from_secs(20) {
                            if let Some(child) = client.as_mut() {
                                let _ = child.kill();
                                let _ = child.wait();
                            }
                            return Err(Error::new(E_FAIL, "UI Automation probe timed out"));
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            // SAFETY: Dispatch a message just retrieved from this queue.
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }

    fn foreground_window() -> HWND {
        // SAFETY: A read-only observation of the current foreground HWND.
        unsafe { GetForegroundWindow() }
    }

    fn invalid_args() -> Error {
        Error::new(
            E_INVALIDARG,
            "Usage: keyboard_accessibility [--probe] [--reduced-motion]",
        )
    }
}
