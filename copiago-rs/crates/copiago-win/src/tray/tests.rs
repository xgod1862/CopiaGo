use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::{
    Graphics::Gdi::{BITMAP, DeleteObject, GetObjectW},
    UI::WindowsAndMessaging::{
        DispatchMessageW, GMDI_USEDISABLED, GetIconInfo, GetMenuDefaultItem, GetMenuItemCount,
        GetMenuItemID, GetMenuStringW, HICON, ICONINFO, MF_BYPOSITION, MSG, PM_REMOVE,
        PeekMessageW, SendMessageW,
    },
};

use super::*;

const ACTION_MESSAGE: u32 = WM_APP + 19;
const LABELS: TrayMenuLabels<'static> = TrayMenuLabels {
    history: "剪贴板历史",
    use_latest: "使用 iPhone 上最新复制的内容",
    settings: "连接与设置",
    exit: "退出",
};

#[derive(Default)]
struct Shell {
    unavailable: bool,
    legacy: bool,
    notices: Vec<Notice>,
}

struct Notice {
    message: NOTIFY_ICON_MESSAGE,
    flags: u32,
    icon_size: Option<(i32, i32)>,
    version: u32,
    tip: [u16; 128],
    info: [u16; 256],
}

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
}

pub(super) fn mock_notify(message: NOTIFY_ICON_MESSAGE, data: &NOTIFYICONDATAW) -> Option<bool> {
    SHELL.with_borrow_mut(|shell| {
        let shell = shell.as_mut()?;
        // SAFETY: icon_data initializes the union's single u32 slot to zero, or
        // add_icon sets it to uVersion before NIM_SETVERSION.
        let version = unsafe { data.Anonymous.uVersion };
        shell.notices.push(Notice {
            message,
            flags: data.uFlags,
            icon_size: (!data.hIcon.is_null()).then(|| icon_dimensions(data.hIcon)),
            version,
            tip: data.szTip,
            info: data.szInfo,
        });
        Some(!shell.unavailable && (message != NIM_SETVERSION || !shell.legacy))
    })
}

struct Mock;

impl Mock {
    fn install(shell: Shell) -> Self {
        SHELL.set(Some(shell));
        Self
    }

    fn change(apply: impl FnOnce(&mut Shell)) {
        SHELL.with_borrow_mut(|shell| apply(shell.as_mut().unwrap()));
    }

    fn notices() -> Vec<Notice> {
        SHELL.with_borrow_mut(|shell| std::mem::take(&mut shell.as_mut().unwrap().notices))
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        SHELL.set(None);
    }
}

struct HostWindow(HWND);

impl HostWindow {
    fn new() -> Self {
        // SAFETY: STATIC is a predefined class; the new hidden window has no parent or data.
        let window = unsafe {
            CreateWindowExW(
                0,
                w!("STATIC"),
                null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        };
        assert!(!window.is_null());
        Self(window)
    }

    fn action(&self) -> Option<TrayAction> {
        let event = take_message(self.0, ACTION_MESSAGE, ACTION_MESSAGE)?;
        TrayAction::from_wparam(event.wParam)
    }

    fn isolated_tray(&self) -> (Instance, Tray) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let namespace = format!("tray-{}-{now}", std::process::id());
        let instance = Instance::acquire_isolated(&namespace, false)
            .unwrap()
            .unwrap();
        let tray = Tray::new(&instance, self.0, ACTION_MESSAGE, LABELS).unwrap();
        (instance, tray)
    }
}

impl Drop for HostWindow {
    fn drop(&mut self) {
        // SAFETY: This test thread owns the hidden receiver.
        unsafe { DestroyWindow(self.0) };
    }
}

#[test]
fn taskbar_created_restores_current_appearance_after_failed_attempts() {
    let _mock = Mock::install(Shell::default());
    let host = HostWindow::new();
    let (_instance, tray) = host.isolated_tray();
    assert_eq!(operations(Mock::notices()), [NIM_ADD, NIM_SETVERSION]);
    tray.set_icon_state(TrayIconState::Connected);
    tray.set_tooltip("CopiaGo · 已连接");
    Mock::notices();

    Mock::change(|shell| shell.unavailable = true);
    taskbar_created(&tray);
    assert!(tray.state.callbacks.get().is_none());
    assert_eq!(tray.state.retries.get(), 10);
    send_timer(&tray, RETRY_TIMER);
    assert_eq!(tray.state.retries.get(), 9);
    let failed = Mock::notices();
    assert_eq!(operations(failed), [NIM_ADD, NIM_MODIFY, NIM_ADD]);

    Mock::change(|shell| shell.unavailable = false);
    send_timer(&tray, RETRY_TIMER);
    assert_eq!(tray.state.retries.get(), 0);
    assert!(matches!(
        tray.state.callbacks.get(),
        Some(CallbackFormat::Version4)
    ));
    let restored = Mock::notices();
    assert_eq!(restored.len(), 2);
    assert_eq!(restored[0].message, NIM_ADD);
    assert_eq!(
        restored[0].flags,
        NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP
    );
    assert_eq!(restored[0].tip, fit_utf16("CopiaGo · 已连接"));
    assert!(restored[0].icon_size.is_some());
    assert_eq!(restored[1].message, NIM_SETVERSION);
    assert_eq!(restored[1].version, NOTIFYICON_VERSION_4);
}

#[test]
fn tray_stops_after_ten_retries_and_restarts_on_a_shell_broadcast() {
    let _mock = Mock::install(Shell {
        unavailable: true,
        ..Shell::default()
    });
    let host = HostWindow::new();
    let (_instance, tray) = host.isolated_tray();
    for _ in 0..12 {
        send_timer(&tray, RETRY_TIMER);
    }
    assert_eq!(operations(Mock::notices()), [NIM_ADD; 11]);
    assert_eq!(tray.state.retries.get(), 0);

    Mock::change(|shell| shell.unavailable = false);
    taskbar_created(&tray);
    assert_eq!(operations(Mock::notices()), [NIM_ADD, NIM_SETVERSION]);
    drop(tray);
    assert_eq!(operations(Mock::notices()), [NIM_DELETE]);
}

#[test]
fn modern_and_legacy_shell_protocols_post_only_their_default_actions() {
    let _mock = Mock::install(Shell::default());
    let host = HostWindow::new();
    let (_instance, tray) = host.isolated_tray();
    let callback = |icon, message| {
        // SAFETY: The tray window is live and only integral callback data is sent.
        unsafe {
            SendMessageW(
                tray.hwnd(),
                WM_CALLBACK,
                0,
                ((icon << 16) | message) as LPARAM,
            )
        };
    };

    callback(ICON_ID, WM_LBUTTONUP);
    callback(ICON_ID + 1, NIN_SELECT);
    assert_eq!(host.action(), None);
    callback(ICON_ID, NIN_SELECT);
    callback(ICON_ID, NIN_KEYSELECT);
    assert_eq!(host.action(), Some(TrayAction::OpenHistory));
    assert_eq!(host.action(), Some(TrayAction::OpenHistory));
    assert_eq!(host.action(), None);

    Mock::change(|shell| shell.legacy = true);
    taskbar_created(&tray);
    // SAFETY: The legacy callback carries the icon ID in wParam.
    unsafe {
        SendMessageW(
            tray.hwnd(),
            WM_CALLBACK,
            ICON_ID as usize,
            WM_LBUTTONUP as LPARAM,
        )
    };
    assert_eq!(host.action(), Some(TrayAction::OpenHistory));
}

#[test]
fn native_menu_hides_unavailable_latest_item_and_preserves_the_default() {
    let labels: menu::Labels = LABELS.into();
    let empty = Menu::new(&labels, false).unwrap();
    let ready = Menu::new(&labels, true).unwrap();
    assert_eq!(
        menu_actions(&empty),
        [
            TrayAction::OpenHistory,
            TrayAction::OpenSettings,
            TrayAction::Exit
        ]
    );
    assert_eq!(
        menu_actions(&ready),
        [
            TrayAction::OpenHistory,
            TrayAction::UseLatest,
            TrayAction::OpenSettings,
            TrayAction::Exit,
        ]
    );

    let mut text = [0u16; 128];
    // SAFETY: The menu exists, and text is writable for its full length.
    let length = unsafe {
        GetMenuStringW(
            ready.0,
            1,
            text.as_mut_ptr(),
            text.len() as i32,
            MF_BYPOSITION,
        )
    };
    assert_eq!(
        String::from_utf16(&text[..length as usize]).unwrap(),
        LABELS.use_latest
    );
}

#[test]
fn balloon_preferences_and_utf16_lengths_match_the_shell_buffers() {
    let _mock = Mock::install(Shell::default());
    let host = HostWindow::new();
    let (_instance, tray) = host.isolated_tray();
    Mock::notices();
    tray.set_notifications_enabled(false);
    assert!(!tray.notify("测试", NotificationKind::Standard));
    assert!(tray.notify("首次连接", NotificationKind::Onboarding));
    tray.set_notifications_enabled(true);
    assert!(tray.notify(&"🙂".repeat(140), NotificationKind::Standard));
    let notices = Mock::notices();
    assert_eq!(notices.len(), 2);
    assert!(
        notices
            .iter()
            .all(|n| n.message == NIM_MODIFY && n.flags == NIF_INFO)
    );
    assert_eq!(notices[0].info, fit_utf16("首次连接"));
    assert_eq!(notices[1].info[254..], [0, 0]);
    assert_eq!(
        String::from_utf16(&notices[1].info[..254]).unwrap(),
        "🙂".repeat(127)
    );
    assert_eq!(fit_utf16::<4>("xy🙂"), [b'x' as u16, b'y' as u16, 0, 0]);
}

#[test]
fn dpi_message_replaces_icons_with_expected_native_pixel_sizes() {
    let _mock = Mock::install(Shell::default());
    let host = HostWindow::new();
    let (_instance, tray) = host.isolated_tray();
    for (dpi, state, size) in [
        (96, TrayIconState::Disconnected, 16),
        (144, TrayIconState::Connected, 24),
        (192, TrayIconState::Error, 32),
    ] {
        Mock::notices();
        let suggested = RECT {
            left: 10,
            top: 10,
            right: 11,
            bottom: 11,
        };
        // SAFETY: The window and suggested rectangle are live during the synchronous message.
        unsafe {
            SendMessageW(
                tray.hwnd(),
                WM_DPICHANGED,
                (dpi << 16) | dpi,
                (&suggested as *const RECT) as LPARAM,
            )
        };
        dispatch_tray_messages(&tray);
        tray.set_icon_state(state);
        assert_eq!(tray.state.dpi.get() as usize, dpi);
        assert_eq!(
            icon_dimensions(Icon::load(state, dpi as u32).unwrap().0),
            (size, size)
        );
        assert!(
            Mock::notices()
                .last()
                .is_none_or(|n| n.icon_size == Some((size, size)))
        );
    }
}

#[test]
#[ignore = "requires a real Explorer tray in the interactive Windows session"]
fn live_shell_restores_only_this_probe_icon() {
    let host = HostWindow::new();
    let start = Instant::now();
    let (_instance, tray) = host.isolated_tray();
    let ready = start.elapsed();
    assert!(tray.state.callbacks.get().is_some());
    let rect = tray.icon_rect().unwrap();
    assert!(rect.left < rect.right && rect.top < rect.bottom);
    assert!(shell_notify(NIM_DELETE, &icon_data(tray.hwnd())));
    assert!(!shell_notify(NIM_MODIFY, &icon_data(tray.hwnd())));
    assert!(tray.icon_rect().is_err());

    let restore = Instant::now();
    taskbar_created(&tray);
    let restored = restore.elapsed();
    assert!(tray.state.callbacks.get().is_some());
    assert!(shell_notify(NIM_MODIFY, &icon_data(tray.hwnd())));
    let rect = tray.icon_rect().unwrap();
    assert!(rect.left < rect.right && rect.top < rect.bottom);
    println!(
        "ready_ms={:.3} restore_ms={:.3}",
        ready.as_secs_f64() * 1000.0,
        restored.as_secs_f64() * 1000.0
    );
}

fn operations(notices: Vec<Notice>) -> Vec<NOTIFY_ICON_MESSAGE> {
    notices.into_iter().map(|notice| notice.message).collect()
}

fn send_timer(tray: &Tray, timer: usize) {
    // SAFETY: The message has integral parameters and targets only this tray HWND.
    unsafe { SendMessageW(tray.hwnd(), WM_TIMER, timer, 0) };
}

fn taskbar_created(tray: &Tray) {
    // SAFETY: Simulate the registered Explorer message on this HWND only.
    unsafe { SendMessageW(tray.hwnd(), tray.state.taskbar_created, 0, 0) };
    dispatch_tray_messages(tray);
}

fn dispatch_tray_messages(tray: &Tray) {
    while let Some(message) = take_message(tray.hwnd(), 0, 0) {
        // SAFETY: The message was retrieved from this UI thread.
        unsafe { DispatchMessageW(&message) };
    }
}

fn take_message(window: HWND, minimum: u32, maximum: u32) -> Option<MSG> {
    // SAFETY: All-zero MSG is valid before PeekMessageW writes its fields.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: message is writable, and the handle belongs to this thread.
    (unsafe { PeekMessageW(&mut message, window, minimum, maximum, PM_REMOVE) } != 0)
        .then_some(message)
}

fn menu_actions(menu: &Menu) -> Vec<TrayAction> {
    // SAFETY: These queries only require the live menu handle.
    unsafe {
        assert_eq!(
            GetMenuDefaultItem(menu.0, 0, GMDI_USEDISABLED),
            TrayAction::OpenHistory as u32
        );
        (0..GetMenuItemCount(menu.0))
            .map(|index| TrayAction::from_wparam(GetMenuItemID(menu.0, index) as usize).unwrap())
            .collect()
    }
}

fn icon_dimensions(icon: HICON) -> (i32, i32) {
    // SAFETY: ICONINFO is an output struct with zeroable fields.
    let mut info: ICONINFO = unsafe { std::mem::zeroed() };
    // SAFETY: The icon is live and info is writable.
    assert_ne!(unsafe { GetIconInfo(icon, &mut info) }, 0);
    // SAFETY: BITMAP is an output struct with zeroable fields.
    let mut bitmap: BITMAP = unsafe { std::mem::zeroed() };
    // SAFETY: GetIconInfo returned caller-owned bitmaps to query and release.
    let copied = unsafe {
        let copied = GetObjectW(
            info.hbmColor,
            size_of::<BITMAP>() as i32,
            (&mut bitmap as *mut BITMAP).cast(),
        );
        DeleteObject(info.hbmColor);
        DeleteObject(info.hbmMask);
        copied
    };
    assert_ne!(copied, 0);
    (bitmap.bmWidth, bitmap.bmHeight)
}
