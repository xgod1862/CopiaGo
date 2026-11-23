//! Shows a blank popup card. Click the card and press Escape to close it.
//! Pass --probe to exercise simulated DPI changes, hide, and reopen.

#[cfg(not(windows))]
fn main() {
    eprintln!("This example requires Windows");
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    demo::run()
}

#[cfg(windows)]
mod demo {
    use std::{env, time::Instant};

    use copiago_ui::composition::{
        CompositionThread, Font, PopupBounds, PopupHost, PopupScene, TextStyle, rgb,
    };
    use windows::{
        Win32::{
            Foundation::{E_FAIL, E_INVALIDARG, HWND, LPARAM, POINT, RECT, WPARAM},
            UI::{
                HiDpi::{
                    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                    GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
                },
                Input::KeyboardAndMouse::VK_ESCAPE,
                WindowsAndMessaging::{
                    DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowRect, KillTimer,
                    MSG, SendMessageW, SetTimer, TranslateMessage, WM_DPICHANGED, WM_KEYDOWN,
                    WM_TIMER,
                },
            },
        },
        core::{Error, Result},
    };
    use windows_numerics::Vector3;

    const CARD: PopupBounds = PopupBounds::new(392.0, 452.0, 24.0);
    const PROBE_TIMER: usize = 1;

    pub(super) fn run() -> Result<()> {
        let (probe, position) = options()?;
        // SAFETY: Set DPI awareness before creating windows. A Per-Monitor V2
        // manifest may already have set the expected awareness context.
        unsafe {
            let context = DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2;
            if !AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), context).as_bool() {
                SetProcessDpiAwarenessContext(context)?;
            }
        }
        let started = Instant::now();
        let thread = CompositionThread::new()?;
        let mut popup = PopupHost::new(&thread)?;
        let build = |scene: &PopupScene| {
            if probe {
                draw_probe(scene)?;
            }
            Ok(())
        };
        // SAFETY: Reading the foreground HWND neither uses nor changes its ownership.
        let foreground = unsafe { GetForegroundWindow() };
        popup.show(position, CARD, false, build)?;
        if probe {
            report(&popup, "show", started, foreground)?;
            arm_timer(popup.hwnd(), 2400)?;
        }
        let mut message = MSG::default();
        let mut step = 0;
        loop {
            // SAFETY: The message buffer is writable and is used on its owner thread.
            let received = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
            match received {
                -1 => return Err(Error::from_win32()),
                0 => break,
                _ => {}
            }
            if popup.handle_message(&message, build)? {
                if popup.scene().is_none() {
                    if !probe {
                        break;
                    }
                } else if probe {
                    report(&popup, "simulated-dpi", started, foreground)?;
                }
            } else if message.hwnd == popup.hwnd()
                && message.message == WM_KEYDOWN
                && message.wParam.0 == usize::from(VK_ESCAPE.0)
            {
                break;
            } else if probe
                && message.hwnd == popup.hwnd()
                && message.message == WM_TIMER
                && message.wParam.0 == PROBE_TIMER
            {
                match step {
                    0 => simulate_dpi(popup.hwnd(), position, 144)?,
                    1 => simulate_dpi(popup.hwnd(), position, 192)?,
                    2 => {
                        popup.hide();
                        assert!(popup.scene().is_none());
                        println!("phase=hidden elapsed_ms={}", started.elapsed().as_millis());
                        arm_timer(popup.hwnd(), 12000)?;
                    }
                    3 => {
                        popup.show(position, CARD, false, build)?;
                        report(&popup, "reopen", started, foreground)?;
                        arm_timer(popup.hwnd(), 2400)?;
                    }
                    _ => break,
                }
                step += 1;
            } else {
                // SAFETY: The current message came from this thread's queue.
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        if probe {
            // SAFETY: The timer and live window belong to this thread.
            unsafe { KillTimer(Some(popup.hwnd()), PROBE_TIMER)? };
        }
        Ok(())
    }

    fn draw_probe(scene: &PopupScene) -> Result<()> {
        let icon = scene.icon('\u{E77B}', 24.0, rgb(15, 110, 90))?;
        icon.visual.SetOffset(Vector3::new(19.0, 20.0, 0.0))?;
        let text = scene.text(
            "CopiaGo · 渲染测试",
            TextStyle {
                font: Font::Semibold,
                size: 20.0,
                color: rgb(23, 32, 43),
                max_width: 300.0,
            },
        )?;
        text.visual.SetOffset(Vector3::new(53.0, 20.0, 0.0))?;
        let children = scene.content().Children()?;
        children.InsertAtTop(&icon.visual)?;
        children.InsertAtTop(&text.visual)?;
        Ok(())
    }

    fn options() -> Result<(bool, POINT)> {
        let mut probe = false;
        let mut position = POINT { x: 72, y: 72 };
        let invalid = || {
            Error::new(
                E_INVALIDARG,
                "Usage: composition_host [--probe] [--position X Y]",
            )
        };
        let mut arguments = env::args().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--probe" => probe = true,
                "--position" => {
                    let mut coordinate = || {
                        arguments
                            .next()
                            .and_then(|value| value.parse::<i32>().ok())
                            .ok_or_else(invalid)
                    };
                    position.x = coordinate()?;
                    position.y = coordinate()?;
                }
                _ => return Err(invalid()),
            }
        }
        Ok((probe, position))
    }

    fn arm_timer(hwnd: HWND, ms: u32) -> Result<()> {
        // SAFETY: The example owns the HWND and processes its timer messages on this thread.
        if unsafe { SetTimer(Some(hwnd), PROBE_TIMER, ms, None) } == 0 {
            return Err(Error::from_win32());
        }
        Ok(())
    }

    fn simulate_dpi(hwnd: HWND, position: POINT, dpi: u32) -> Result<()> {
        let (width, height) = CARD.pixel_size(dpi)?;
        let suggested = RECT {
            left: position.x,
            top: position.y,
            right: position.x + width,
            bottom: position.y + height,
        };
        // SAFETY: This same-thread SendMessageW returns before the stack RECT is dropped.
        unsafe {
            SendMessageW(
                hwnd,
                WM_DPICHANGED,
                Some(WPARAM(((dpi << 16) | dpi) as usize)),
                Some(LPARAM(std::ptr::from_ref(&suggested) as isize)),
            );
        }
        Ok(())
    }

    fn report(
        popup: &PopupHost<'_>,
        phase: &str,
        started: Instant,
        foreground: HWND,
    ) -> Result<()> {
        let mut rect = RECT::default();
        // SAFETY: The popup HWND is live and the RECT is writable.
        let foreground_unchanged = unsafe {
            GetWindowRect(popup.hwnd(), &mut rect)?;
            GetForegroundWindow() == foreground
        };
        let dpi = popup
            .scene()
            .ok_or_else(|| Error::from_hresult(E_FAIL))?
            .dpi();
        let actual = (rect.right - rect.left, rect.bottom - rect.top);
        assert_eq!(actual, CARD.pixel_size(dpi)?);
        println!(
            "phase={phase} dpi={dpi} card={}x{} at={},{} hwnd={:#x} elapsed_ms={} foreground_unchanged={foreground_unchanged}",
            actual.0,
            actual.1,
            rect.left,
            rect.top,
            popup.hwnd().0.addr(),
            started.elapsed().as_millis()
        );
        Ok(())
    }
}
