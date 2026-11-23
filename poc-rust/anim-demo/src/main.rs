//! CopiaGo popup animation demo.
//!
//! Visual tree: Windows.UI.Composition hosted in a plain Win32 window (desktop interop).
//! Text and icons: Direct2D + DirectWrite drawn into composition surfaces.
//! Every animation runs in the system compositor (DWM), so it stays smooth even when the
//! UI thread is busy, and follows the display's refresh rate.
//!
//! Keys: Space = hide/show (GPU resources are released while hidden), Esc = quit.
//! The scripted sequence loops: disconnected -> connecting (progress) -> connected ->
//! clips arrive -> push feedback -> disconnected. Hover and click work at any time.

#![windows_subsystem = "windows"]

mod gfx;
mod mem;

use std::cell::RefCell;

use windows::{
    Foundation::TimeSpan,
    System::DispatcherQueueController,
    UI::Color,
    UI::Composition::Desktop::DesktopWindowTarget,
    UI::Composition::*,
    Win32::Foundation::*,
    Win32::Graphics::Gdi::*,
    Win32::System::Com::*,
    Win32::System::LibraryLoader::GetModuleHandleW,
    Win32::System::WinRT::Composition::ICompositorDesktopInterop,
    Win32::System::WinRT::*,
    Win32::UI::HiDpi::*,
    Win32::UI::Input::KeyboardAndMouse::*,
    Win32::UI::WindowsAndMessaging::*,
    core::*,
};
use windows_numerics::{Vector2, Vector3};

use gfx::Gfx;

// Palette and metrics of the current Python popup (popup.py), in DIPs.
const WHITE: Color = rgb(255, 255, 255);
const INK: Color = rgb(23, 32, 43);
const MUTED: Color = rgb(93, 104, 117);
const CARD: Color = rgb(244, 246, 249);
const CARD_HOVER: Color = rgb(235, 239, 243);
const ACCENT: Color = rgb(15, 110, 90);
const ACCENT_HOVER: Color = rgb(11, 88, 72);
const TINT: Color = rgb(227, 241, 236);
const TRACK: Color = rgb(232, 236, 240);

const WIDTH: f32 = 392.0;
const HEIGHT: f32 = 452.0;
const RADIUS: f32 = 24.0;
const PAD: f32 = 18.0;
const MARGIN: f32 = 40.0; // room for the shadow around the card
const ROW: f32 = 60.0;
const GAP: f32 = 8.0;
const LIST_Y: f32 = 112.0;
const LIST_H: f32 = 4.0 * ROW + 3.0 * GAP;
const FOOTER_Y: f32 = HEIGHT - PAD - 36.0;
const MAX_ROWS: usize = 4;

const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_STEP: usize = 1;
const TIMER_MEM: usize = 2;
const TIMER_HIDE_DONE: usize = 3;
const TIMER_AUTOHIDE: usize = 4;
const HOTKEY_TOGGLE: i32 = 1;
const HOTKEY_QUIT: i32 = 2;

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { A: 255, R: r, G: g, B: b }
}

fn ms(v: i64) -> TimeSpan {
    TimeSpan { Duration: v * 10_000 }
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Warn,
    Busy,
    Ok,
}

impl Status {
    fn colors(self) -> (Color, Color, Color) {
        // (chip background, chip text, dot)
        match self {
            Status::Ok => (rgb(227, 241, 236), rgb(12, 90, 73), rgb(31, 157, 118)),
            Status::Busy => (rgb(238, 241, 244), MUTED, rgb(138, 151, 164)),
            Status::Warn => (rgb(251, 240, 220), rgb(138, 83, 0), rgb(208, 138, 26)),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Status::Ok => "已连接",
            Status::Busy => "连接中",
            Status::Warn => "未连接",
        }
    }
}

struct Row {
    visual: ContainerVisual,
    bg: CompositionColorBrush,
}

/// Everything that holds GPU resources. Dropped while the popup is hidden.
struct Scene {
    gfx: Gfx,
    layer: ContainerVisual,
    chip: ContainerVisual,
    chip_geom: CompositionRoundedRectangleGeometry,
    chip_brush: CompositionColorBrush,
    dot: ShapeVisual,
    dot_brush: CompositionColorBrush,
    chip_label: Option<Visual>,
    chip_label_old: Option<Visual>,
    message: Option<Visual>,
    message_old: Option<Visual>,
    message_holder: ContainerVisual,
    progress: ContainerVisual,
    progress_fill: ShapeVisual,
    list: ContainerVisual,
    rows: Vec<Row>,
    leaving: Vec<Visual>,
    button: ContainerVisual,
    button_brush: CompositionColorBrush,
    button_w: f32,
    toast: ContainerVisual,
    mem_holder: ContainerVisual,
    status: Status,
}

struct App {
    hwnd: HWND,
    s: f32, // DPI scale
    compositor: Compositor,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    scene: Option<Scene>,
    step: u32,
    clip_no: usize,
    hover_row: Option<usize>,
    hover_button: bool,
    pressed: bool,
    tracking: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn main() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _queue: DispatcherQueueController = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?;

        let instance = GetModuleHandleW(None)?;
        let class = w!("CopiaGoAnimDemo");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        });
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("CopiaGo 动画演示"),
            WS_POPUP,
            0,
            0,
            100,
            100,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let s = GetDpiForWindow(hwnd) as f32 / 96.0;
        let (w, h) = (((WIDTH + 2.0 * MARGIN) * s) as i32, ((HEIGHT + 2.0 * MARGIN) * s) as i32);
        let mut work = RECT::default();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))?;
        let gap = (12.0 * s) as i32 - (MARGIN * s) as i32;
        SetWindowPos(hwnd, Some(HWND_TOPMOST), work.right - w - gap, work.bottom - h - gap, w, h, SWP_NOACTIVATE)?;

        let compositor = Compositor::new()?;
        let interop: ICompositorDesktopInterop = compositor.cast()?;
        let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
        let root = compositor.CreateContainerVisual()?;
        target.SetRoot(&root)?;

        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                hwnd,
                s,
                compositor,
                _target: target,
                root,
                scene: None,
                step: 0,
                clip_no: 0,
                hover_row: None,
                hover_button: false,
                pressed: false,
                tracking: false,
            })
        });
        with_app(|app| app.show())?;
        SetTimer(Some(hwnd), TIMER_MEM, 1000, None);
        // A hidden window gets no key presses, so show/hide and quit also have global hotkeys.
        let _ = RegisterHotKey(Some(hwnd), HOTKEY_TOGGLE, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, VK_SPACE.0 as u32);
        let _ = RegisterHotKey(Some(hwnd), HOTKEY_QUIT, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, u32::from(b'Q'));
        // --hide-after N: hide once after N seconds (to measure the hidden, GPU-free state).
        let mut it = std::env::args().skip_while(|a| a != "--hide-after").skip(1);
        if let Some(secs) = it.next().and_then(|s| s.parse::<u32>().ok()) {
            SetTimer(Some(hwnd), TIMER_AUTOHIDE, secs * 1000, None);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        APP.with(|a| a.borrow_mut().take());
    }
    Ok(())
}

fn with_app(f: impl FnOnce(&mut App) -> Result<()>) -> Result<()> {
    APP.with(|a| match a.try_borrow_mut() {
        Ok(mut guard) => match guard.as_mut() {
            Some(app) => f(app),
            None => Ok(()),
        },
        Err(_) => Ok(()), // re-entered from a nested message; ignore
    })
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let x = (lp.0 & 0xffff) as i16 as f32;
    let y = ((lp.0 >> 16) & 0xffff) as i16 as f32;
    let r = match msg {
        WM_TIMER => with_app(|app| match wp.0 {
            TIMER_STEP => app.advance(),
            TIMER_MEM => app.update_memory(),
            TIMER_HIDE_DONE => app.finish_hide(),
            TIMER_AUTOHIDE => {
                unsafe {
                    let _ = KillTimer(Some(app.hwnd), TIMER_AUTOHIDE);
                }
                app.hide()
            }
            _ => Ok(()),
        }),
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            unsafe { PostQuitMessage(0) };
            Ok(())
        }
        WM_KEYDOWN if wp.0 == VK_SPACE.0 as usize => with_app(|app| if app.scene.is_some() { app.hide() } else { app.show() }),
        WM_HOTKEY if wp.0 as i32 == HOTKEY_TOGGLE => with_app(|app| if app.scene.is_some() { app.hide() } else { app.show() }),
        WM_HOTKEY if wp.0 as i32 == HOTKEY_QUIT => {
            unsafe { PostQuitMessage(0) };
            Ok(())
        }
        WM_MOUSEMOVE => with_app(|app| app.mouse_move(x, y)),
        WM_MOUSELEAVE => with_app(|app| {
            app.tracking = false;
            app.mouse_move(-1.0, -1.0)
        }),
        WM_LBUTTONDOWN => with_app(|app| app.mouse_down(x, y)),
        WM_LBUTTONUP => with_app(|app| app.mouse_up(x, y)),
        WM_NCHITTEST => {
            // Clicks on the transparent shadow margin fall through to the window below.
            let mut pt = POINT { x: x as i32, y: y as i32 };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut pt);
            }
            let inside = APP.with(|a| {
                a.try_borrow().ok().and_then(|g| g.as_ref().map(|app| app.in_card(pt.x as f32, pt.y as f32))).unwrap_or(true)
            });
            return LRESULT(if inside { HTCLIENT as isize } else { HTTRANSPARENT as isize });
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            Ok(())
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    };
    if let Err(e) = r {
        let text = HSTRING::from(format!("{e}"));
        unsafe { MessageBoxW(Some(hwnd), &text, w!("CopiaGo 动画演示"), MB_ICONERROR) };
        unsafe { PostQuitMessage(1) };
    }
    LRESULT(0)
}

impl App {
    fn px(&self, v: f32) -> f32 {
        v * self.s
    }
    fn v2(&self, x: f32, y: f32) -> Vector2 {
        Vector2::new(x * self.s, y * self.s)
    }
    fn v3(&self, x: f32, y: f32) -> Vector3 {
        Vector3::new(x * self.s, y * self.s, 0.0)
    }
    fn in_card(&self, x: f32, y: f32) -> bool {
        let (l, t) = (self.px(MARGIN), self.px(MARGIN));
        self.scene.is_some() && x >= l && y >= t && x < l + self.px(WIDTH) && y < t + self.px(HEIGHT)
    }
    /// Card-local DIP coordinates of a client point.
    fn local(&self, x: f32, y: f32) -> (f32, f32) {
        (x / self.s - MARGIN, y / self.s - MARGIN)
    }

    // ---------------------------------------------------------------- building blocks

    fn rounded(&self, w: f32, h: f32, r: f32, color: Color) -> Result<(ShapeVisual, CompositionRoundedRectangleGeometry, CompositionColorBrush)> {
        let c = &self.compositor;
        let geom = c.CreateRoundedRectangleGeometry()?;
        geom.SetSize(self.v2(w, h))?;
        geom.SetCornerRadius(self.v2(r, r))?;
        let brush = c.CreateColorBrushWithColor(color)?;
        let shape = c.CreateSpriteShapeWithGeometry(&geom)?;
        shape.SetFillBrush(&brush)?;
        let visual = c.CreateShapeVisual()?;
        visual.SetSize(self.v2(w, h))?;
        visual.Shapes()?.Append(&shape)?;
        Ok((visual, geom, brush))
    }

    fn cubic(&self) -> Result<CubicBezierEasingFunction> {
        // "Standard" fluent easing: fast out, gentle settle.
        self.compositor.CreateCubicBezierEasingFunction(Vector2::new(0.1, 0.9), Vector2::new(0.2, 1.0))
    }

    fn animate_scalar(&self, target: &CompositionObject, prop: &str, to: f32, dur: i64, delay: i64) -> Result<()> {
        let a = self.compositor.CreateScalarKeyFrameAnimation()?;
        a.InsertKeyFrameWithEasingFunction(1.0, to, &self.cubic()?)?;
        a.SetDuration(ms(dur))?;
        if delay > 0 {
            a.SetDelayTime(ms(delay))?;
        }
        target.StartAnimation(&HSTRING::from(prop), &a)
    }

    fn animate_color(&self, brush: &CompositionColorBrush, to: Color, dur: i64) -> Result<()> {
        let a = self.compositor.CreateColorKeyFrameAnimation()?;
        a.SetInterpolationColorSpace(CompositionColorSpace::Rgb)?;
        a.InsertKeyFrameWithEasingFunction(1.0, to, &self.cubic()?)?;
        a.SetDuration(ms(dur))?;
        brush.StartAnimation(h!("Color"), &a)
    }

    fn animate_v2(&self, target: &CompositionObject, prop: &str, to: Vector2, dur: i64) -> Result<()> {
        let a = self.compositor.CreateVector2KeyFrameAnimation()?;
        a.InsertKeyFrameWithEasingFunction(1.0, to, &self.cubic()?)?;
        a.SetDuration(ms(dur))?;
        target.StartAnimation(&HSTRING::from(prop), &a)
    }

    fn animate_v3(&self, target: &CompositionObject, prop: &str, to: Vector3, dur: i64) -> Result<()> {
        let a = self.compositor.CreateVector3KeyFrameAnimation()?;
        a.InsertKeyFrameWithEasingFunction(1.0, to, &self.cubic()?)?;
        a.SetDuration(ms(dur))?;
        target.StartAnimation(&HSTRING::from(prop), &a)
    }

    fn spring_v3(&self, target: &CompositionObject, prop: &str, to: Vector3, period: i64, damping: f32) -> Result<()> {
        // A Vector3 spring needs a boxed IReference<Vector3>, which windows-rs cannot create;
        // a scalar spring per channel ("Offset.X", "Scale.Y", …) behaves the same.
        for (channel, value) in [("X", to.X), ("Y", to.Y)] {
            let a = self.compositor.CreateSpringScalarAnimation()?;
            a.SetFinalValue(&windows::Foundation::PropertyValue::CreateSingle(value)?.cast::<windows::Foundation::IReference<f32>>()?)?;
            a.SetPeriod(ms(period))?;
            a.SetDampingRatio(damping)?;
            target.StartAnimation(&HSTRING::from(format!("{prop}.{channel}")), &a)?;
        }
        Ok(())
    }

    /// Offset changes on this visual animate by themselves from now on.
    fn implicit_offset(&self, v: &Visual, dur: i64) -> Result<()> {
        let c = &self.compositor;
        let a = c.CreateVector3KeyFrameAnimation()?;
        a.InsertExpressionKeyFrameWithEasingFunction(1.0, h!("this.FinalValue"), &self.cubic()?)?;
        a.SetDuration(ms(dur))?;
        a.SetTarget(h!("Offset"))?;
        let set = c.CreateImplicitAnimationCollection()?;
        set.Insert(h!("Offset"), &a)?;
        v.SetImplicitAnimations(&set)
    }

    // ---------------------------------------------------------------- scene

    fn build_scene(&self) -> Result<Scene> {
        let c = &self.compositor;
        let gfx = Gfx::new(c, self.s)?;

        // `layer` is the animated root of the card: [shadow layer, content].
        let layer = c.CreateContainerVisual()?;
        layer.SetSize(self.v2(WIDTH, HEIGHT))?;
        layer.SetOffset(self.v3(MARGIN, MARGIN))?;

        // The shadow lives on its own LayerVisual holding just the card silhouette. A shadow
        // set on a LayerVisual paints over that layer's own content here, so the real content
        // is a separate sibling drawn on top.
        if !std::env::args().any(|a| a == "--noshadow") {
            let shadow_layer = c.CreateLayerVisual()?;
            shadow_layer.SetSize(self.v2(WIDTH, HEIGHT))?;
            let (silhouette, _, _) = self.rounded(WIDTH, HEIGHT, RADIUS, WHITE)?;
            shadow_layer.Children()?.InsertAtTop(&silhouette)?;
            let shadow = c.CreateDropShadow()?;
            shadow.SetBlurRadius(self.px(28.0))?;
            shadow.SetOffset(self.v3(0.0, 10.0))?;
            shadow.SetColor(rgb(16, 24, 40))?;
            shadow.SetOpacity(0.20)?;
            shadow_layer.SetShadow(&shadow)?;
            layer.Children()?.InsertAtBottom(&shadow_layer)?;
        }
        let content = c.CreateContainerVisual()?;
        content.SetSize(self.v2(WIDTH, HEIGHT))?;
        layer.Children()?.InsertAtTop(&content)?;
        let kids = content.Children()?;

        let (frame, _, _) = self.rounded(WIDTH, HEIGHT, RADIUS, WHITE)?;
        kids.InsertAtBottom(&frame)?;

        // Header: app name + status chip
        let (title, _, _) = gfx.text(c, "CopiaGo", gfx::BOLD, 16.0, INK, 200.0)?;
        title.SetOffset(self.v3(PAD + 2.0, PAD))?;
        kids.InsertAtTop(&title)?;

        let chip = c.CreateContainerVisual()?;
        let (chip_bg, chip_geom, chip_brush) = self.rounded(80.0, 26.0, 13.0, Status::Warn.colors().0)?;
        chip_bg.SetSize(self.v2(200.0, 26.0))?;
        chip.Children()?.InsertAtBottom(&chip_bg)?;
        let (dot, _, dot_brush) = self.rounded(8.0, 8.0, 4.0, Status::Warn.colors().2)?;
        dot.SetOffset(self.v3(11.0, 9.0))?;
        dot.SetCenterPoint(self.v3(4.0, 4.0))?;
        chip.Children()?.InsertAtTop(&dot)?;
        kids.InsertAtTop(&chip)?;

        let message_holder = c.CreateContainerVisual()?;
        message_holder.SetOffset(self.v3(PAD + 2.0, PAD + 34.0))?;
        kids.InsertAtTop(&message_holder)?;

        // Progress bar
        let progress = c.CreateContainerVisual()?;
        progress.SetOffset(self.v3(PAD, PAD + 68.0))?;
        progress.SetOpacity(0.0)?;
        let (track, _, _) = self.rounded(WIDTH - 2.0 * PAD, 4.0, 2.0, TRACK)?;
        let (fill, _, _) = self.rounded(WIDTH - 2.0 * PAD, 4.0, 2.0, ACCENT)?;
        fill.SetScale(Vector3::new(0.0, 1.0, 1.0))?;
        progress.Children()?.InsertAtBottom(&track)?;
        progress.Children()?.InsertAtTop(&fill)?;
        kids.InsertAtTop(&progress)?;

        // History list, clipped so rows sliding out disappear at the edge.
        let list = c.CreateContainerVisual()?;
        list.SetOffset(self.v3(PAD, LIST_Y))?;
        list.SetSize(self.v2(WIDTH - 2.0 * PAD, LIST_H))?;
        list.SetClip(&c.CreateInsetClip()?)?;
        kids.InsertAtTop(&list)?;

        // Footer: memory readout + push button
        let mem_holder = c.CreateContainerVisual()?;
        mem_holder.SetOffset(self.v3(PAD + 2.0, FOOTER_Y + 9.0))?;
        kids.InsertAtTop(&mem_holder)?;

        let (label, lw, lh) = gfx.text(c, "推送到 iPhone", gfx::SEMIBOLD, 13.0, WHITE, 200.0)?;
        let button_w = lw / self.s + 36.0;
        let button = c.CreateContainerVisual()?;
        button.SetSize(self.v2(button_w, 36.0))?;
        button.SetOffset(self.v3(WIDTH - PAD - button_w, FOOTER_Y))?;
        button.SetCenterPoint(self.v3(button_w / 2.0, 18.0))?;
        let (button_bg, _, button_brush) = self.rounded(button_w, 36.0, 18.0, ACCENT)?;
        button.Children()?.InsertAtBottom(&button_bg)?;
        label.SetOffset(Vector3::new(self.px(18.0), (self.px(36.0) - lh) / 2.0, 0.0))?;
        button.Children()?.InsertAtTop(&label)?;
        kids.InsertAtTop(&button)?;

        // Toast (hidden until used)
        let toast = c.CreateContainerVisual()?;
        let (icon, iw, ih) = gfx.text(c, "\u{E73E}", gfx::ICON, 13.0, WHITE, 40.0)?;
        let (msg, mw, mh) = gfx.text(c, "已推送到 iPhone", gfx::SEMIBOLD, 13.0, WHITE, 200.0)?;
        let tw = (iw + mw) / self.s + 34.0;
        let (toast_bg, _, _) = self.rounded(tw, 34.0, 17.0, INK)?;
        toast.Children()?.InsertAtBottom(&toast_bg)?;
        icon.SetOffset(Vector3::new(self.px(14.0), (self.px(34.0) - ih) / 2.0, 0.0))?;
        msg.SetOffset(Vector3::new(self.px(20.0) + iw, (self.px(34.0) - mh) / 2.0, 0.0))?;
        toast.Children()?.InsertAtTop(&icon)?;
        toast.Children()?.InsertAtTop(&msg)?;
        toast.SetSize(self.v2(tw, 34.0))?;
        toast.SetOffset(self.v3((WIDTH - tw) / 2.0, FOOTER_Y - 48.0))?;
        toast.SetCenterPoint(self.v3(tw / 2.0, 17.0))?;
        toast.SetOpacity(0.0)?;
        kids.InsertAtTop(&toast)?;

        Ok(Scene {
            gfx,
            layer,
            chip,
            chip_geom,
            chip_brush,
            dot,
            dot_brush,
            chip_label: None,
            chip_label_old: None,
            message: None,
            message_old: None,
            message_holder,
            progress,
            progress_fill: fill,
            list,
            rows: Vec::new(),
            leaving: Vec::new(),
            button,
            button_brush,
            button_w,
            toast,
            mem_holder,
            status: Status::Warn,
        })
    }

    // ---------------------------------------------------------------- show / hide

    fn show(&mut self) -> Result<()> {
        let scene = self.build_scene()?;
        self.root.Children()?.InsertAtTop(&scene.layer)?;
        let layer: Visual = scene.layer.cast()?;
        self.scene = Some(scene);
        self.step = 0;
        self.set_status(Status::Warn, "连接并信任你的 iPhone", false)?;
        self.update_memory()?;

        // Rise from below with a soft spring, fade and scale in.
        layer.SetOpacity(0.0)?;
        layer.SetOffset(self.v3(MARGIN, MARGIN + 36.0))?;
        layer.SetCenterPoint(self.v3(WIDTH / 2.0, HEIGHT))?;
        layer.SetScale(Vector3::new(0.94, 0.94, 1.0))?;
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            let _ = SetForegroundWindow(self.hwnd);
        }
        let obj: CompositionObject = layer.cast()?;
        self.spring_v3(&obj, "Offset", self.v3(MARGIN, MARGIN), 70, 0.72)?;
        self.spring_v3(&obj, "Scale", Vector3::new(1.0, 1.0, 1.0), 70, 0.72)?;
        self.animate_scalar(&obj, "Opacity", 1.0, 220, 0)?;
        unsafe { SetTimer(Some(self.hwnd), TIMER_STEP, 1500, None) };
        Ok(())
    }

    fn hide(&mut self) -> Result<()> {
        let Some(scene) = &self.scene else { return Ok(()) };
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_STEP);
        }
        let obj: CompositionObject = scene.layer.cast()?;
        self.animate_v3(&obj, "Offset", self.v3(MARGIN, MARGIN + 24.0), 200)?;
        self.animate_v3(&obj, "Scale", Vector3::new(0.97, 0.97, 1.0), 200)?;
        self.animate_scalar(&obj, "Opacity", 0.0, 180, 0)?;
        unsafe { SetTimer(Some(self.hwnd), TIMER_HIDE_DONE, 240, None) };
        Ok(())
    }

    fn finish_hide(&mut self) -> Result<()> {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE_DONE);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.root.Children()?.RemoveAll()?;
        // Drops every visual, surface, the D2D device and the D3D device.
        self.scene = None;
        self.hover_row = None;
        self.hover_button = false;
        Ok(())
    }

    // ---------------------------------------------------------------- the scripted sequence

    fn advance(&mut self) -> Result<()> {
        self.step = if self.step >= 8 { 1 } else { self.step + 1 };
        match self.step {
            1 => {
                self.set_status(Status::Busy, "正在建立无线连接…", true)?;
                self.set_progress(Some(0.35))?;
            }
            2 => {
                self.set_message("正在验证剪贴板服务…")?;
                self.set_progress(Some(0.82))?;
            }
            3 => {
                self.set_progress(Some(1.0))?;
                self.set_status(Status::Ok, "已连接（无线）", true)?;
                self.set_progress(None)?;
            }
            4 => self.add_row()?,
            5 => self.add_row()?,
            6 => self.push_feedback()?,
            7 => self.add_row()?,
            8 => self.set_status(Status::Warn, "iPhone 已断开，正在重新连接…", true)?,
            _ => {}
        }
        Ok(())
    }

    fn set_status(&mut self, status: Status, message: &str, animate: bool) -> Result<()> {
        let s = self.s;
        let (bg, ink, dot_color) = status.colors();
        let compositor = self.compositor.clone();
        {
            let scene = self.scene.as_mut().unwrap();
            scene.status = status;
            // Chip label: cross-fade with a small vertical slide.
            if let Some(old) = scene.chip_label_old.take() {
                scene.chip.Children()?.Remove(&old)?;
            }
            let (label, lw, lh) = scene.gfx.text(&compositor, status.label(), gfx::SEMIBOLD, 12.0, ink, 120.0)?;
            label.SetOffset(Vector3::new(26.0 * s, (26.0 * s - lh) / 2.0 + if animate { 6.0 * s } else { 0.0 }, 0.0))?;
            label.SetOpacity(if animate { 0.0 } else { 1.0 })?;
            scene.chip.Children()?.InsertAtTop(&label)?;
            let chip_w = lw / s + 26.0 + 12.0;
            let new: Visual = label.cast()?;
            let old = scene.chip_label.replace(new.clone());
            scene.chip_label_old = old.clone();
            let chip = scene.chip.clone();
            let geom = scene.chip_geom.clone();
            let chip_brush = scene.chip_brush.clone();
            let dot_brush = scene.dot_brush.clone();
            let dot = scene.dot.clone();
            let chip_target = Vector3::new((WIDTH - PAD - chip_w) * s, PAD * s, 0.0);
            if animate {
                let new_obj: CompositionObject = new.cast()?;
                self.animate_scalar(&new_obj, "Opacity", 1.0, 260, 60)?;
                self.animate_v3(&new_obj, "Offset", Vector3::new(26.0 * s, (26.0 * s - lh) / 2.0, 0.0), 320)?;
                if let Some(old) = old {
                    let old_obj: CompositionObject = old.cast()?;
                    let o = old.Offset()?;
                    self.animate_scalar(&old_obj, "Opacity", 0.0, 160, 0)?;
                    self.animate_v3(&old_obj, "Offset", Vector3::new(o.X, o.Y - 6.0 * s, 0.0), 220)?;
                }
                // The chip morphs to the new width while staying right-aligned.
                self.animate_v2(&geom.cast()?, "Size", Vector2::new(chip_w * s, 26.0 * s), 320)?;
                self.animate_v3(&chip.cast()?, "Offset", chip_target, 320)?;
                self.animate_color(&chip_brush, bg, 320)?;
                self.animate_color(&dot_brush, dot_color, 320)?;
            } else {
                geom.SetSize(Vector2::new(chip_w * s, 26.0 * s))?;
                chip.SetOffset(chip_target)?;
                chip_brush.SetColor(bg)?;
                dot_brush.SetColor(dot_color)?;
            }
            // A breathing dot while connecting.
            if status == Status::Busy {
                let a = compositor.CreateScalarKeyFrameAnimation()?;
                a.InsertKeyFrame(0.0, 1.0)?;
                a.InsertKeyFrame(0.5, 0.25)?;
                a.InsertKeyFrame(1.0, 1.0)?;
                a.SetDuration(ms(1100))?;
                a.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
                dot.StartAnimation(h!("Opacity"), &a)?;
                let p = compositor.CreateVector3KeyFrameAnimation()?;
                p.InsertKeyFrame(0.0, Vector3::new(1.0, 1.0, 1.0))?;
                p.InsertKeyFrame(0.5, Vector3::new(1.35, 1.35, 1.0))?;
                p.InsertKeyFrame(1.0, Vector3::new(1.0, 1.0, 1.0))?;
                p.SetDuration(ms(1100))?;
                p.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
                dot.StartAnimation(h!("Scale"), &p)?;
            } else {
                dot.StopAnimation(h!("Opacity"))?;
                dot.StopAnimation(h!("Scale"))?;
                dot.SetOpacity(1.0)?;
                // A small "pop" when the state settles.
                dot.SetScale(Vector3::new(1.6, 1.6, 1.0))?;
                self.spring_v3(&dot.cast()?, "Scale", Vector3::new(1.0, 1.0, 1.0), 60, 0.45)?;
            }
        }
        if animate {
            self.set_message(message)
        } else {
            self.set_message_now(message)
        }
    }

    fn set_message_now(&mut self, text: &str) -> Result<()> {
        let compositor = self.compositor.clone();
        let scene = self.scene.as_mut().unwrap();
        let (v, _, _) = scene.gfx.text(&compositor, text, gfx::REGULAR, 13.0, MUTED, WIDTH - 2.0 * PAD)?;
        scene.message_holder.Children()?.InsertAtTop(&v)?;
        scene.message = Some(v.cast()?);
        Ok(())
    }

    fn set_message(&mut self, text: &str) -> Result<()> {
        let s = self.s;
        let compositor = self.compositor.clone();
        let (new, old) = {
            let scene = self.scene.as_mut().unwrap();
            if let Some(old) = scene.message_old.take() {
                scene.message_holder.Children()?.Remove(&old)?;
            }
            let (v, _, _) = scene.gfx.text(&compositor, text, gfx::REGULAR, 13.0, MUTED, WIDTH - 2.0 * PAD)?;
            v.SetOpacity(0.0)?;
            v.SetOffset(Vector3::new(0.0, 8.0 * s, 0.0))?;
            scene.message_holder.Children()?.InsertAtTop(&v)?;
            let new: Visual = v.cast()?;
            let old = scene.message.replace(new.clone());
            scene.message_old = old.clone();
            (new, old)
        };
        let n: CompositionObject = new.cast()?;
        self.animate_scalar(&n, "Opacity", 1.0, 260, 80)?;
        self.animate_v3(&n, "Offset", Vector3::new(0.0, 0.0, 0.0), 360)?;
        if let Some(old) = old {
            let o: CompositionObject = old.cast()?;
            self.animate_scalar(&o, "Opacity", 0.0, 160, 0)?;
            self.animate_v3(&o, "Offset", Vector3::new(0.0, -8.0 * s, 0.0), 240)?;
        }
        Ok(())
    }

    fn set_progress(&mut self, value: Option<f32>) -> Result<()> {
        let scene = self.scene.as_ref().unwrap();
        let bar: CompositionObject = scene.progress.cast()?;
        let fill: CompositionObject = scene.progress_fill.cast()?;
        match value {
            Some(v) => {
                self.animate_scalar(&bar, "Opacity", 1.0, 200, 0)?;
                self.animate_v3(&fill, "Scale", Vector3::new(v, 1.0, 1.0), 700)?;
            }
            None => {
                // Let the bar finish filling, then fade it out and reset.
                let a = self.compositor.CreateScalarKeyFrameAnimation()?;
                a.InsertKeyFrame(0.0, 1.0)?;
                a.InsertKeyFrame(0.6, 1.0)?;
                a.InsertKeyFrameWithEasingFunction(1.0, 0.0, &self.cubic()?)?;
                a.SetDuration(ms(900))?;
                bar.StartAnimation(h!("Opacity"), &a)?;
            }
        }
        Ok(())
    }

    fn add_row(&mut self) -> Result<()> {
        const CLIPS: [(&str, &str, &str); 3] = [
            ("\u{E8C8}", "会议纪要：周四 14:00 产品评审，带上新版交互稿", "来自 iPhone · 刚刚 · 文字"),
            ("\u{EB9F}", "截图 1179×2556", "来自 iPhone · 刚刚 · PNG 图片"),
            ("\u{E71B}", "https://github.com/xgod1862/CopiaGo/releases", "来自 iPhone · 刚刚 · 链接"),
        ];
        let (glyph, title, sub) = CLIPS[self.clip_no % CLIPS.len()];
        self.clip_no += 1;
        let s = self.s;
        let c = self.compositor.clone();
        let w = WIDTH - 2.0 * PAD;

        let row = c.CreateContainerVisual()?;
        row.SetSize(self.v2(w, ROW))?;
        let (bg, _, bg_brush) = self.rounded(w, ROW, 16.0, CARD)?;
        row.Children()?.InsertAtBottom(&bg)?;
        let (badge, _, _) = self.rounded(36.0, 36.0, 18.0, TINT)?;
        badge.SetOffset(self.v3(12.0, 12.0))?;
        row.Children()?.InsertAtTop(&badge)?;
        {
            let scene = self.scene.as_ref().unwrap();
            let (icon, iw, ih) = scene.gfx.text(&c, glyph, gfx::ICON, 16.0, ACCENT, 40.0)?;
            icon.SetOffset(Vector3::new((12.0 + 18.0) * s - iw / 2.0, (12.0 + 18.0) * s - ih / 2.0, 0.0))?;
            row.Children()?.InsertAtTop(&icon)?;
            let (t, _, _) = scene.gfx.text(&c, title, gfx::SEMIBOLD, 13.5, INK, w - 64.0)?;
            t.SetOffset(self.v3(58.0, 11.0))?;
            row.Children()?.InsertAtTop(&t)?;
            let (st, _, _) = scene.gfx.text(&c, sub, gfx::REGULAR, 11.5, MUTED, w - 64.0)?;
            st.SetOffset(self.v3(58.0, 33.0))?;
            row.Children()?.InsertAtTop(&st)?;
        }

        // New row starts slightly above and transparent; existing rows slide down.
        row.SetOffset(self.v3(0.0, -18.0))?;
        row.SetOpacity(0.0)?;
        row.SetCenterPoint(self.v3(w / 2.0, ROW / 2.0))?;
        row.SetScale(Vector3::new(0.97, 0.97, 1.0))?;
        let rv: Visual = row.cast()?;
        self.implicit_offset(&rv, 420)?;

        let scene = self.scene.as_mut().unwrap();
        for old in scene.leaving.drain(..) {
            scene.list.Children()?.Remove(&old)?;
        }
        scene.list.Children()?.InsertAtTop(&row)?;
        scene.rows.insert(0, Row { visual: row.clone(), bg: bg_brush });
        let mut fading = Vec::new();
        for (i, r) in scene.rows.iter().enumerate() {
            r.visual.SetOffset(Vector3::new(0.0, i as f32 * (ROW + GAP) * s, 0.0))?;
            if i >= MAX_ROWS {
                fading.push(r.visual.clone());
            }
        }
        while scene.rows.len() > MAX_ROWS {
            let r = scene.rows.pop().unwrap();
            scene.leaving.push(r.visual.cast()?);
        }
        let obj: CompositionObject = row.cast()?;
        self.animate_scalar(&obj, "Opacity", 1.0, 300, 60)?;
        self.spring_v3(&obj, "Scale", Vector3::new(1.0, 1.0, 1.0), 60, 0.7)?;
        for f in fading {
            self.animate_scalar(&f.cast()?, "Opacity", 0.0, 250, 0)?;
        }
        if let Some(i) = self.hover_row {
            self.hover_row = None;
            let _ = i;
        }
        Ok(())
    }

    fn push_feedback(&mut self) -> Result<()> {
        let scene = self.scene.as_ref().unwrap();
        let button: CompositionObject = scene.button.cast()?;
        let toast: CompositionObject = scene.toast.cast()?;
        // Press: quick squeeze then spring back.
        let press = self.compositor.CreateVector3KeyFrameAnimation()?;
        press.InsertKeyFrame(0.0, Vector3::new(1.0, 1.0, 1.0))?;
        press.InsertKeyFrame(0.25, Vector3::new(0.94, 0.94, 1.0))?;
        press.InsertKeyFrameWithEasingFunction(1.0, Vector3::new(1.0, 1.0, 1.0), &self.cubic()?)?;
        press.SetDuration(ms(380))?;
        button.StartAnimation(h!("Scale"), &press)?;
        // Toast: pop in with a spring, hold, fade out.
        scene.toast.SetScale(Vector3::new(0.8, 0.8, 1.0))?;
        self.spring_v3(&toast, "Scale", Vector3::new(1.0, 1.0, 1.0), 55, 0.55)?;
        let fade = self.compositor.CreateScalarKeyFrameAnimation()?;
        fade.InsertKeyFrame(0.0, 0.0)?;
        fade.InsertKeyFrame(0.1, 1.0)?;
        fade.InsertKeyFrame(0.82, 1.0)?;
        fade.InsertKeyFrameWithEasingFunction(1.0, 0.0, &self.cubic()?)?;
        fade.SetDuration(ms(1800))?;
        toast.StartAnimation(h!("Opacity"), &fade)?;
        Ok(())
    }

    // ---------------------------------------------------------------- pointer

    fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        let (lx, ly) = self.local(x, y);
        let scene = self.scene.as_ref()?;
        if lx < PAD || lx > WIDTH - PAD || ly < LIST_Y || ly > LIST_Y + LIST_H {
            return None;
        }
        let i = ((ly - LIST_Y) / (ROW + GAP)) as usize;
        let within = (ly - LIST_Y) - i as f32 * (ROW + GAP) <= ROW;
        (within && i < scene.rows.len()).then_some(i)
    }

    fn on_button(&self, x: f32, y: f32) -> bool {
        let (lx, ly) = self.local(x, y);
        let Some(scene) = &self.scene else { return false };
        lx >= WIDTH - PAD - scene.button_w && lx <= WIDTH - PAD && ly >= FOOTER_Y && ly <= FOOTER_Y + 36.0
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> Result<()> {
        if self.scene.is_none() {
            return Ok(());
        }
        if !self.tracking && x >= 0.0 {
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe { TrackMouseEvent(&mut t)? };
            self.tracking = true;
        }
        let row = self.row_at(x, y);
        if row != self.hover_row {
            let scene = self.scene.as_ref().unwrap();
            if let Some(r) = self.hover_row.and_then(|i| scene.rows.get(i)) {
                self.animate_color(&r.bg, CARD, 180)?;
            }
            if let Some(r) = row.and_then(|i| scene.rows.get(i)) {
                self.animate_color(&r.bg, CARD_HOVER, 120)?;
            }
            self.hover_row = row;
        }
        let on = self.on_button(x, y);
        if on != self.hover_button {
            let brush = self.scene.as_ref().unwrap().button_brush.clone();
            self.animate_color(&brush, if on { ACCENT_HOVER } else { ACCENT }, 150)?;
            self.hover_button = on;
        }
        Ok(())
    }

    fn mouse_down(&mut self, x: f32, y: f32) -> Result<()> {
        if self.on_button(x, y) {
            self.pressed = true;
            let b: CompositionObject = self.scene.as_ref().unwrap().button.cast()?;
            self.spring_v3(&b, "Scale", Vector3::new(0.94, 0.94, 1.0), 30, 0.9)?;
        } else if let Some(i) = self.row_at(x, y) {
            let v: CompositionObject = self.scene.as_ref().unwrap().rows[i].visual.cast()?;
            self.spring_v3(&v, "Scale", Vector3::new(0.98, 0.98, 1.0), 30, 0.9)?;
        }
        Ok(())
    }

    fn mouse_up(&mut self, x: f32, y: f32) -> Result<()> {
        if self.pressed {
            self.pressed = false;
            let b: CompositionObject = self.scene.as_ref().unwrap().button.cast()?;
            self.spring_v3(&b, "Scale", Vector3::new(1.0, 1.0, 1.0), 55, 0.5)?;
            if self.on_button(x, y) {
                self.push_feedback()?;
            }
        } else if let Some(i) = self.row_at(x, y) {
            let v: CompositionObject = self.scene.as_ref().unwrap().rows[i].visual.cast()?;
            self.spring_v3(&v, "Scale", Vector3::new(1.0, 1.0, 1.0), 55, 0.5)?;
            self.push_feedback()?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- memory readout

    fn update_memory(&mut self) -> Result<()> {
        let m = mem::sample();
        println!(
            "[mem] {} private WS {:.1} MB, private bytes {:.1} MB, WS {:.1} MB",
            if self.scene.is_some() { "shown " } else { "hidden" },
            m.private_ws,
            m.private_bytes,
            m.working_set
        );
        let compositor = self.compositor.clone();
        let Some(scene) = self.scene.as_mut() else { return Ok(()) };
        let text = format!("内存 {:.1} MB", m.private_ws);
        let (v, _, _) = scene.gfx.text(&compositor, &text, gfx::REGULAR, 11.5, MUTED, 160.0)?;
        scene.mem_holder.Children()?.RemoveAll()?;
        scene.mem_holder.Children()?.InsertAtTop(&v)?;
        Ok(())
    }
}
