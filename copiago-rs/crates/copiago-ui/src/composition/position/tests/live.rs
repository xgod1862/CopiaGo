use std::time::{Duration, Instant};

use copiago_win::{
    instance::Instance,
    tray::{Tray, TrayMenuLabels},
};
use windows::{
    Win32::{
        Foundation::{LPARAM, WPARAM},
        Graphics::Gdi::{EnumDisplayMonitors, HDC},
        UI::{
            HiDpi::{
                AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                DispatchMessageW, GetForegroundWindow, IsWindowVisible, MSG, MWMO_INPUTAVAILABLE,
                MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_ALLINPUT, SendMessageW,
                TranslateMessage, WM_APP, WM_DPICHANGED,
            },
        },
    },
    core::BOOL,
};

use super::{CARD, card_box, rect};
use crate::composition::position::*;
use crate::composition::{
    CompositionThread, Font, PopupBounds, PopupHost, PopupScene, TextStyle, rgb,
};

#[test]
#[ignore = "requires Explorer, Windows Composition and two interactive monitors"]
fn live_shell_anchor_and_secondary_window() -> Result<()> {
    let per_monitor = DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2;
    // SAFETY: Set the test process's DPI context before creating any windows.
    unsafe {
        if !AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), per_monitor).as_bool() {
            SetProcessDpiAwarenessContext(per_monitor)?;
        }
    }
    let thread = CompositionThread::new()?;
    let mut popup = PopupHost::new(&thread)?;
    let primary = TrayPlacement::resolve(None, Some(POINT::default()))?;
    let secondary = displays()
        .into_iter()
        .find(|&monitor| monitor != primary.monitor)
        .expect("Connect a secondary monitor before running this test");
    let secondary = TrayPlacement::on_monitor(secondary, None)?;
    for (name, placement) in [("primary", primary), ("secondary", secondary)] {
        let dpi = popup.windows.dpi_at(placement.dpi_sample())?;
        println!(
            "monitor={name} work={:?} dpi={dpi} edge={:?}",
            placement.work, placement.edge
        );
    }

    let instance =
        Instance::acquire_isolated(&format!("PopupPosition-{}", std::process::id()), false)
            .unwrap()
            .unwrap();
    let tray = Tray::new(
        &instance,
        popup.hwnd().0,
        WM_APP + 90,
        TrayMenuLabels {
            history: "History",
            use_latest: "Use latest",
            settings: "Settings",
            exit: "Exit",
        },
    )
    .unwrap();
    pump(&mut popup, Duration::from_millis(200))?;
    let icon = tray
        .icon_rect()
        .expect("Explorer returns a real tray rectangle");
    let icon = rect(icon.left, icon.top, icon.right, icon.bottom);
    assert!(valid(icon));
    // SAFETY: This rectangle is readable and contains native screen pixels.
    let icon_monitor = unsafe { MonitorFromRect(&icon, MONITOR_DEFAULTTONULL) };
    assert!(!icon_monitor.is_invalid());
    let opposite = if icon_monitor == primary.monitor {
        secondary
    } else {
        primary
    };
    let shell_anchor = TrayPlacement::resolve(Some(icon), Some(opposite.dpi_sample()))?;
    assert_eq!(shell_anchor.monitor, icon_monitor);
    let shell_dpi = popup.windows.dpi_at(shell_anchor.dpi_sample())?;
    let shell_target = card_box(shell_anchor.place(CARD, shell_dpi)?);
    assert_eq!(
        intersection(shell_target, shell_anchor.work),
        Some(shell_target)
    );
    println!(
        "shell icon={icon:?} target={shell_target:?} edge={:?}",
        shell_anchor.edge
    );

    let invalid = rect(0, 0, 0, 96);
    let offscreen = rect(-500_000, -500_000, -499_900, -499_900);
    for icon in [None, Some(invalid), Some(offscreen)] {
        let fallback = TrayPlacement::resolve(icon, Some(secondary.dpi_sample()))?;
        assert_eq!(fallback.monitor, secondary.monitor);
    }

    let icon = (icon_monitor == secondary.monitor).then_some(icon);
    let secondary = TrayPlacement::on_monitor(secondary.monitor, icon)?;
    // SAFETY: Read the foreground window without activating any window.
    let foreground = unsafe { GetForegroundWindow() };
    let start = Instant::now();
    popup.show_at_tray(icon, Some(secondary.dpi_sample()), CARD, false, draw_card)?;
    let show_ms = start.elapsed().as_secs_f64() * 1_000.0;
    // SAFETY: These queries use live windows and don't alter foreground focus.
    unsafe {
        assert_eq!(GetForegroundWindow(), foreground);
        assert!(IsWindowVisible(popup.hwnd()).as_bool());
    }
    let dpi = popup.scene().unwrap().dpi();
    assert_window(&popup, secondary.place(CARD, dpi)?)?;
    println!(
        "phase=visible pid={} show_ms={show_ms:.3} dpi={dpi} policy={:?}",
        std::process::id(),
        MotionPolicy::system()
    );
    pump(&mut popup, Duration::from_secs(3))?;

    let taller = PopupBounds::new(CARD.width, CARD.height + 26.0, CARD.radius);
    popup.rebuild_at_tray(taller, draw_card)?;
    assert_window(&popup, secondary.place(taller, dpi)?)?;
    let scene = popup.scene().unwrap();
    secondary.animate_in(scene, MotionPolicy::Reduced)?;
    assert_eq!(scene.motion_root().Offset()?, Vector3::default());
    assert_eq!(scene.motion_root().Scale()?, Vector3::new(1.0, 1.0, 1.0));
    assert_eq!(scene.motion_root().Opacity()?, 1.0);
    for simulated in [144_usize, 192] {
        // SAFETY: The popup belongs to this thread and reads the suggested
        // rectangle during this synchronous DPI message.
        unsafe {
            SendMessageW(
                popup.hwnd(),
                WM_DPICHANGED,
                Some(WPARAM((simulated << 16) | simulated)),
                Some(LPARAM(std::ptr::from_ref(&primary.work) as isize)),
            );
        }
        pump(&mut popup, Duration::from_millis(120))?;
        let expected = secondary.place(taller, simulated as u32)?;
        assert_eq!(popup.scene().unwrap().dpi(), simulated as u32);
        assert_window(&popup, expected)?;
        println!("simulated dpi={simulated} card={:?}", card_box(expected));
    }
    popup.hide();
    assert!(popup.scene().is_none());
    // SAFETY: The hidden HWND is still owned by the popup.
    assert!(!unsafe { IsWindowVisible(popup.hwnd()) }.as_bool());
    println!("phase=hidden pid={}", std::process::id());
    pump(&mut popup, Duration::from_secs(3))
}

fn displays() -> Vec<HMONITOR> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        output: LPARAM,
    ) -> BOOL {
        // SAFETY: EnumDisplayMonitors synchronously provides the Vec pointer given below.
        unsafe { &mut *(output.0 as *mut Vec<HMONITOR>) }.push(monitor);
        BOOL(1)
    }
    let mut monitors = Vec::new();
    // SAFETY: The callback is valid and the vector remains live during enumeration.
    let result = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(std::ptr::from_mut(&mut monitors) as isize),
        )
    };
    assert!(result.as_bool());
    monitors
}

#[test]
#[ignore = "requires a Windows desktop with Composition"]
fn native_card_keeps_its_taskbar_corner_when_height_changes() -> Result<()> {
    let thread = CompositionThread::new()?;
    let mut popup = PopupHost::new(&thread)?;
    let desktop = TrayPlacement::resolve(None, Some(POINT::default()))?;
    let screen = rect(0, 0, 1280, 800);
    let dpi = 120;
    let gap = 15;
    for (edge, bar) in [
        (Edge::Top, rect(0, 0, 1280, 48)),
        (Edge::Bottom, rect(0, 752, 1280, 800)),
        (Edge::Left, rect(0, 0, 48, 800)),
        (Edge::Right, rect(1232, 0, 1280, 800)),
    ] {
        let detected = Edge::from_bar(screen, bar).unwrap();
        let tray =
            TrayPlacement::on_display(desktop.monitor, screen, screen, Some((detected, bar)), None);
        assert_eq!(tray.edge, edge);
        popup.present(tray.place(CARD, dpi)?, Some(tray), false, draw_card)?;
        for height in [CARD.height, 600.0, 320.0] {
            let bounds = PopupBounds { height, ..CARD };
            popup.resize_at_tray(bounds)?;
            let mut actual = RECT::default();
            // SAFETY: This test owns the active input window and output RECT.
            unsafe { GetWindowRect(popup.hwnd(), &mut actual)? };
            let work = tray.work;
            assert_eq!(intersection(work, actual), Some(actual));
            assert_eq!(
                if edge == Edge::Top {
                    actual.top - work.top
                } else {
                    work.bottom - actual.bottom
                },
                gap
            );
            assert_eq!(
                if edge == Edge::Left {
                    actual.left - work.left
                } else {
                    work.right - actual.right
                },
                gap
            );
            if height == CARD.height {
                println!("taskbar={edge:?} dpi={dpi} card={actual:?}");
            }
        }
        popup.hide();
    }
    Ok(())
}

#[test]
#[ignore = "requires a Windows desktop with Composition"]
fn small_work_areas_keep_real_windows_and_recovery_actions_reachable() -> Result<()> {
    use crate::animation::Animator;
    use crate::components::{ProgressState, Rect};
    use crate::history::{
        HistoryAction, HistoryPage, HistoryStatus, HistoryTarget, HistoryView, StatusTone,
    };
    use crate::settings_page::{
        ConnectionState, DriverState, SettingsAction, SettingsLayout, SettingsScreen,
        SettingsState, SettingsView,
    };
    use copiago_core::i18n::{Language, message};
    use windows_numerics::Vector2;

    let thread = CompositionThread::new()?;
    let mut popup = PopupHost::new(&thread)?;
    let desktop = TrayPlacement::resolve(None, Some(POINT::default()))?;
    let now = Instant::now();
    let warning = message!(
        "端口 27015 被「{program}」占用，Apple 连接服务无法工作。请退出该程序（或在任务管理器中结束它），然后点「修复连接服务」。",
        program = "example-process-".repeat(8)
    );
    let mut state = SettingsState::load(
        std::env::temp_dir().join("copiago-popup-layout-read-only"),
        "test",
    );
    state.set_connection(ConnectionState {
        driver: DriverState::ServiceUnavailable,
        ..Default::default()
    });
    state.set_diagnosis(Some(warning.clone()));
    state.complete_driver(DriverState::ServiceUnavailable, message!("Apple 连接服务仍不可用。请重启电脑后再试；仍不行请点「手动下载安装」重新安装 Apple 组件。"));
    state.set_notice(Some(message!(
        "诊断信息已复制。可以直接粘贴发给开发者，或点「反馈问题」提交到 GitHub。"
    )));
    let mut page = HistoryPage::new(Language::En);
    page.set_status(HistoryStatus {
        detail: Some(warning),
        tone: StatusTone::Warning,
        progress: ProgressState::Waiting,
        can_retry: true,
        ..Default::default()
    });
    page.set_update(Some("2.0".into()));

    // Supply physical work areas to the real presenter without changing any display settings.
    for (width, height, dpi) in [(1280, 720, 144), (1024, 600, 192), (1280, 480, 288)] {
        let work = rect(0, 0, width, height - 48);
        let tray = TrayPlacement::on_display(desktop.monitor, work, work, None, None);
        let limit = tray.available_height(dpi);
        for screen in [SettingsScreen::Settings, SettingsScreen::PairingGuide] {
            let layout = SettingsLayout::measure(&state, screen, limit)?;
            let bounds = layout.bounds();
            let card = tray.place(bounds, dpi)?;
            assert_eq!(card.bounds.height, bounds.height);
            assert_eq!(intersection(work, card_box(card)), Some(card_box(card)));
            popup.present(card, None, false, |scene| {
                let animator = Animator::new(scene.compositor(), MotionPolicy::Reduced)?;
                let mut view =
                    SettingsView::with_layout(scene, &animator, &state, screen, now, &layout)?;
                scene.content().Children()?.InsertAtTop(view.visual())?;
                let actions: &[_] = match screen {
                    SettingsScreen::PairingGuide => &[
                        SettingsAction::InstallDriver,
                        SettingsAction::CopyDiagnostics,
                        SettingsAction::RequestPair,
                    ],
                    SettingsScreen::Settings => &[
                        SettingsAction::CopyDiagnostics,
                        SettingsAction::ReportIssue,
                        SettingsAction::Back,
                    ],
                };
                for &action in actions {
                    view.reveal(&animator, action, now)?;
                    let area = view
                        .controls(now)
                        .find(|c| c.enabled && c.action == action)
                        .and_then(|c| c.bounds)
                        .unwrap();
                    assert!(Rect::new(0.0, 0.0, bounds.width, bounds.height).contains_rect(area));
                    assert_eq!(
                        view.hit_test(
                            Vector2::new(area.x + area.width / 2.0, area.y + area.height / 2.0),
                            now
                        ),
                        Some(action)
                    );
                }
                Ok(())
            })?;
            assert_window(&popup, card)?;
            popup.hide();
        }
        let layout = page.layout_with_limit(limit)?;
        let bounds = layout.bounds();
        let card = tray.place(bounds, dpi)?;
        assert_eq!(bounds.height, card.bounds.height);
        assert_eq!(intersection(work, card_box(card)), Some(card_box(card)));
        popup.present(card, Some(tray), false, |scene| {
            let mut view = HistoryView::mount(scene, &page, &layout, MotionPolicy::Reduced, now)?;
            for action in [
                HistoryAction::Retry,
                HistoryAction::OpenUpdate,
                HistoryAction::OpenSettings,
            ] {
                let target = HistoryTarget::Action(action);
                view.reveal_control(target, now)?;
                let area = view
                    .controls(&page, now)
                    .into_iter()
                    .find(|c| c.target == target)
                    .and_then(|c| c.bounds)
                    .unwrap();
                assert!(Rect::new(0.0, 0.0, bounds.width, bounds.height).contains_rect(area));
                assert_eq!(
                    view.hit(
                        Vector2::new(area.x + area.width / 2.0, area.y + area.height / 2.0),
                        now
                    ),
                    Some(target)
                );
            }
            Ok(())
        })?;
        assert_window(&popup, card)?;
        popup.resize_at_tray(PopupBounds {
            height: 1200.0,
            ..bounds
        })?;
        assert_window(
            &popup,
            tray.place(
                PopupBounds {
                    height: limit,
                    ..bounds
                },
                dpi,
            )?,
        )?;
        // A larger work area must reposition the same-height window immediately.
        popup.available_tray_height(None)?;
        assert_window(
            &popup,
            desktop.place(
                PopupBounds {
                    height: limit,
                    ..bounds
                },
                dpi,
            )?,
        )?;
        println!(
            "screen={width}x{height} dpi={dpi} work_height={} limit_dip={limit} history_height_px={} bottom_px={}",
            work.bottom,
            card.height,
            card_box(card).bottom
        );
        popup.hide();
    }
    Ok(())
}

fn draw_card(scene: &PopupScene) -> Result<()> {
    let label = scene.text(
        "CopiaGo · 托盘弹窗定位验证",
        TextStyle {
            font: Font::Semibold,
            size: 20.0,
            color: rgb(23, 32, 43),
            max_width: 350.0,
        },
    )?;
    label.visual.SetOffset(Vector3::new(18.0, 18.0, 0.0))?;
    scene.content().Children()?.InsertAtTop(&label.visual)
}

fn assert_window(popup: &PopupHost<'_>, expected: Placement) -> Result<()> {
    let mut actual = RECT::default();
    // SAFETY: The popup window is live and the rectangle is a writable output.
    unsafe { GetWindowRect(popup.hwnd(), &mut actual)? };
    assert_eq!(actual, card_box(expected));
    Ok(())
}

fn pump(popup: &mut PopupHost<'_>, time: Duration) -> Result<()> {
    let deadline = Instant::now() + time;
    let mut message = MSG::default();
    while Instant::now() < deadline {
        // SAFETY: This message buffer belongs to the current UI thread.
        if unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            if !popup.handle_message(&message, draw_card)? {
                // SAFETY: This thread retrieved the message from its own queue above.
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        } else {
            // SAFETY: Wait for messages on this thread without passing any handles.
            unsafe { MsgWaitForMultipleObjectsEx(None, 10, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        }
    }
    Ok(())
}
