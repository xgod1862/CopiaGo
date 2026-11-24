//! Present the live history, settings and pairing pages on the Win32 UI thread.

use std::{
    cell::OnceCell,
    time::{Duration, Instant},
};

use copiago_core::{history::Source, tr};
use copiago_ui::{
    animation::{Animator, MotionPolicy},
    components::Rect,
    composition::{CompositionThread, POPUP_SETTINGS_CHANGED, PopupBounds, PopupHost, PopupScene},
    focus::{FocusElement, FocusEvent, FocusId, FocusRole},
    history::{
        HistoryAction, HistoryLayout, HistoryPage, HistoryTarget, HistoryView, ThumbnailPixels,
        ThumbnailRequest,
    },
    page_transition::{PageDirection, transition_pages},
    settings_page::{ControlRole, SettingsAction, SettingsScreen, SettingsState, SettingsView},
};
use copiago_win::{image::decode_scaled, tray::Tray};
use windows::{
    UI::Composition::ContainerVisual,
    Win32::{
        Foundation::{HWND, LPARAM, POINT, RECT, WPARAM},
        UI::{
            Input::KeyboardAndMouse::{
                GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_DELETE,
                VK_DOWN, VK_LEFT, VK_RETURN, VK_RIGHT, VK_UP,
            },
            WindowsAndMessaging::{
                MSG, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
                WM_MOUSEWHEEL,
            },
        },
    },
    core::Result,
};
use windows_numerics::Vector2;

const WM_MOUSELEAVE: u32 = 0x02A3;
const SCROLL_FRAMES: Duration = Duration::from_millis(440);
const FRAME_INTERVAL: Duration = Duration::from_millis(32);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Screen {
    History,
    Settings,
    Guide,
}

impl Screen {
    fn settings(self) -> SettingsScreen {
        match self {
            Self::Settings => SettingsScreen::Settings,
            Self::Guide => SettingsScreen::PairingGuide,
            Self::History => unreachable!("history has its own view"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    History(HistoryTarget),
    Settings(SettingsAction),
}

pub(super) enum Input {
    Dispatch,
    Handled,
    Target(Target),
    MoveSelection(isize),
}

pub(super) struct Models<'a> {
    pub history: &'a HistoryPage,
    /// The encrypted store, for decoding full-size history images.
    pub store: &'a copiago_core::history::History,
    pub settings: &'a SettingsState,
}

pub(super) struct Popup<'a> {
    thread: &'a OnceCell<CompositionThread>,
    host: Option<PopupHost<'a>>,
    view: Option<PageView>,
    screen: Screen,
    viewport: Option<Rect>,
    policy: MotionPolicy,
    pressed: Option<Target>,
    point: Option<Vector2>,
    targets: Vec<(FocusId, Target)>,
    next_focus: u64,
    sample_until: Option<Instant>,
    next_frame: Option<Instant>,
}

impl<'a> Popup<'a> {
    pub fn new(thread: &'a OnceCell<CompositionThread>) -> Self {
        Self {
            thread,
            host: None,
            view: None,
            screen: Screen::History,
            viewport: None,
            policy: MotionPolicy::system(),
            pressed: None,
            point: None,
            targets: Vec::new(),
            next_focus: 0,
            sample_until: None,
            next_frame: None,
        }
    }

    pub fn visible(&self) -> bool {
        self.view.is_some() && self.closing_until().is_none()
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    pub fn show(
        &mut self,
        tray: &Tray,
        screen: Screen,
        activate: bool,
        models: Models<'_>,
        now: Instant,
    ) -> Result<()> {
        if self.visible() {
            if self.screen == screen {
                self.clear_pointer();
                if let Some(view) = &mut self.view {
                    view.restore_scroll(0.0, now)?;
                }
                self.render(models, now)?;
            } else {
                self.navigate(screen, models, now)?;
            }
            if activate && let Some(host) = &self.host {
                host.activate()?;
            }
            return Ok(());
        }
        self.hide();
        self.screen = screen;
        self.policy = MotionPolicy::system();
        self.clear_pointer();
        let layout = Layout::new(screen, models.history)?;
        self.viewport = layout.viewport();
        if self.thread.get().is_none() {
            let _ = self.thread.set(CompositionThread::new()?);
        }
        let thread = self.thread.get().unwrap();
        let host = match &mut self.host {
            Some(host) => host,
            empty => {
                let mut host = PopupHost::new(thread)?;
                host.set_animate_dismissal(true);
                empty.insert(host)
            }
        };
        host.set_dismiss_on_focus_loss(screen != Screen::Guide);
        let anchor = tray.icon_rect().ok().map(|rect| RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        });
        host.show_at_tray(anchor, None, layout.bounds(), activate, |scene| {
            self.view = Some(PageView::new(
                scene,
                screen,
                &layout,
                &models,
                self.policy,
                now,
            )?);
            attach(scene, self.view.as_ref().unwrap())
        })?;
        self.publish_and_decode(models, now)
    }

    pub fn navigate(&mut self, screen: Screen, models: Models<'_>, now: Instant) -> Result<()> {
        if !self.visible() || self.screen == screen {
            return Ok(());
        }
        let direction = if screen == Screen::Settings {
            PageDirection::Forward
        } else {
            PageDirection::Back
        };
        self.clear_pointer();
        let host = self.host.as_mut().unwrap();
        let previous = self.view.as_ref().unwrap();
        let layout = Layout::new(screen, models.history)?;
        host.resize_at_tray(layout.bounds())?;
        host.set_dismiss_on_focus_loss(screen != Screen::Guide);
        let scene = host.scene().unwrap();
        let view = PageView::new(scene, screen, &layout, &models, self.policy, now)?;
        transition_pages(
            &view.animator,
            scene.content(),
            previous.visual(),
            view.visual(),
            direction,
        )?;
        self.view = Some(view);
        self.screen = screen;
        self.viewport = layout.viewport();
        self.publish_and_decode(models, now)
    }

    pub fn render(&mut self, models: Models<'_>, now: Instant) -> Result<()> {
        if !self.visible() {
            return Ok(());
        }
        let layout = Layout::new(self.screen, models.history)?;
        let host = self.host.as_mut().unwrap();
        let view = self.view.as_mut().unwrap();
        if self.viewport != layout.viewport() {
            let scroll = view
                .history()
                .map_or(0.0, |history| history.scroll_offset());
            host.rebuild_at_tray(layout.bounds(), |scene| {
                *view = PageView::new(scene, self.screen, &layout, &models, self.policy, now)?;
                if let Some(history) = view.history() {
                    history.restore_scroll(scroll, now)?;
                }
                attach(scene, view)
            })?;
            self.viewport = layout.viewport();
        } else {
            view.refresh(host.scene().unwrap(), &layout, &models, self.policy, now)?;
        }
        self.update_pointer(now)?;
        self.schedule_frames(now);
        self.publish_and_decode(models, now)
    }

    pub fn feedback(&mut self, page: &HistoryPage, action: HistoryAction) -> Result<()> {
        if self.visible()
            && let Some(scene) = self.host.as_ref().and_then(PopupHost::scene)
            && let Some(view) = self.view.as_mut().and_then(PageView::history)
        {
            view.show_feedback(scene, page, action)?;
        }
        Ok(())
    }

    pub fn close(&mut self, now: Instant) -> Result<()> {
        if let Some(host) = &mut self.host {
            host.request_hide(now)?;
            if host.scene().is_none() {
                self.hide();
            }
        }
        Ok(())
    }

    pub fn hide(&mut self) {
        self.view = None;
        self.targets.clear();
        self.sample_until = None;
        self.next_frame = None;
        self.clear_pointer();
        if let Some(host) = &mut self.host {
            host.hide();
        }
    }

    pub fn closing_until(&self) -> Option<Instant> {
        self.host.as_ref().and_then(PopupHost::closing_until)
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.closing_until().or(self.next_frame)
    }

    pub fn tick(&mut self, models: Models<'_>, now: Instant) -> Result<()> {
        if let Some(deadline) = self.closing_until() {
            if now >= deadline {
                self.hide();
            }
        } else if self.next_frame.is_some_and(|frame| now >= frame) {
            self.publish_and_decode(models, now)?;
            self.next_frame = self
                .sample_until
                .filter(|end| now < *end)
                .map(|end| (now + FRAME_INTERVAL).min(end));
        }
        Ok(())
    }

    pub fn handle(
        &mut self,
        message: &windows_sys::Win32::UI::WindowsAndMessaging::MSG,
        models: Models<'_>,
        now: Instant,
    ) -> Result<Input> {
        // Do not borrow the view or initialize Composition for unrelated messages.
        let Some(host) = self
            .host
            .as_mut()
            .filter(|host| host.hwnd().0 == message.hwnd)
        else {
            return Ok(Input::Dispatch);
        };
        let message = MSG {
            hwnd: HWND(message.hwnd),
            message: message.message,
            wParam: WPARAM(message.wParam),
            lParam: LPARAM(message.lParam),
            pt: POINT {
                x: message.pt.x,
                y: message.pt.y,
            },
            time: message.time,
        };
        let view = &mut self.view;
        if host.handle_message(&message, |scene| {
            let layout = Layout::new(self.screen, models.history)?;
            let scroll = view.as_ref().map_or(0.0, PageView::scroll_offset);
            let mut next = PageView::new(scene, self.screen, &layout, &models, self.policy, now)?;
            next.restore_scroll(scroll, now)?;
            attach(scene, &next)?;
            *view = Some(next);
            Ok(())
        })? {
            if host.scene().is_none() {
                self.hide();
            } else if host.closing_until().is_none() {
                self.publish_and_decode(models, now)?;
            }
            return Ok(Input::Handled);
        }
        if !self.visible() {
            return Ok(Input::Dispatch);
        }
        let host = self.host.as_ref().unwrap();
        let view = self.view.as_mut().unwrap();
        if let Some(event) = host.handle_focus_message(&message)? {
            return match event {
                FocusEvent::Changed(Some(id)) => {
                    if let Some(target) = self.target(id) {
                        self.reveal(target, models, now)?;
                    }
                    Ok(Input::Handled)
                }
                FocusEvent::Activated(id) => {
                    let action = self.target(id);
                    host.notify_invoked(id);
                    let target = action.map(|action| match action {
                        Target::History(HistoryTarget::Row(id))
                            if message.message == WM_KEYDOWN
                                && message.wParam.0 == usize::from(VK_RETURN.0) =>
                        {
                            Target::History(HistoryTarget::Action(HistoryAction::Copy(id)))
                        }
                        _ => action,
                    });
                    Ok(target.map_or(Input::Handled, Input::Target))
                }
                _ => Ok(Input::Handled),
            };
        }
        match message.message {
            POPUP_SETTINGS_CHANGED => {
                self.policy = MotionPolicy::system();
                self.render(models, now)?;
                Ok(Input::Handled)
            }
            WM_KEYDOWN => Ok(key_input(&message, self.screen, &models)),
            WM_MOUSEWHEEL => {
                view.scroll((message.wParam.0 >> 16) as i16, now)?;
                self.schedule_frames(now);
                self.publish_and_decode(models, now)?;
                Ok(Input::Handled)
            }
            WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                self.handle_pointer_message(&message, now)
            }
            WM_MOUSELEAVE => {
                self.clear_pointer();
                self.update_pointer(now)?;
                Ok(Input::Handled)
            }
            _ => Ok(Input::Dispatch),
        }
    }

    fn handle_pointer_message(&mut self, message: &MSG, now: Instant) -> Result<Input> {
        let host = self.host.as_ref().unwrap();
        let view = self.view.as_ref().unwrap();
        if self.point.is_none() {
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: host.hwnd(),
                dwHoverTime: 0,
            };
            // SAFETY: The structure and the owning HWND are valid for the call.
            let _ = unsafe { TrackMouseEvent(&mut track) };
        }
        let point = host.scene().unwrap().client_to_dips(POINT {
            x: i32::from(message.lParam.0 as i16),
            y: i32::from((message.lParam.0 >> 16) as i16),
        });
        self.point = Some(point);
        let target = view.hit(point, now);
        let action = match message.message {
            WM_LBUTTONDBLCLK if matches!(target, Some(Target::History(HistoryTarget::Row(_)))) => {
                let Some(Target::History(HistoryTarget::Row(id))) = target else {
                    unreachable!()
                };
                self.pressed = None;
                Some(Target::History(HistoryTarget::Action(HistoryAction::Copy(
                    id,
                ))))
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                self.pressed = target;
                None
            }
            WM_LBUTTONUP => self
                .pressed
                .take()
                .filter(|pressed| Some(*pressed) == target),
            _ => None,
        };
        if let Some(action) = action {
            let focus = self
                .targets
                .iter()
                .find(|(_, target)| *target == action)
                .map(|(id, _)| *id);
            if let Some(id) = focus {
                host.set_focused_element(Some(id))?;
                host.notify_invoked(id);
            }
        }
        self.update_pointer(now)?;
        Ok(action.map_or(Input::Handled, Input::Target))
    }

    pub fn reveal(&mut self, target: Target, models: Models<'_>, now: Instant) -> Result<()> {
        if let Some(view) = self.view.as_mut() {
            match (&mut view.body, target) {
                (Body::History(view), Target::History(target)) => {
                    view.reveal_control(target, now)?
                }
                (Body::Settings(settings), Target::Settings(action)) => {
                    settings.reveal(&view.animator, action, now)?;
                }
                _ => {}
            }
            self.schedule_frames(now);
            self.publish_and_decode(models, now)?;
        }
        Ok(())
    }

    pub fn focus(&self, target: Target) -> Result<()> {
        if let Some(host) = &self.host
            && let Some((id, _)) = self.targets.iter().find(|(_, action)| *action == target)
        {
            host.set_focused_element(Some(*id))?;
        }
        Ok(())
    }

    fn clear_pointer(&mut self) {
        self.point = None;
        self.pressed = None;
    }

    fn update_pointer(&mut self, now: Instant) -> Result<()> {
        if let Some(view) = self.view.as_mut() {
            let point = self.point;
            match &mut view.body {
                Body::History(history) => {
                    let hovered = point.and_then(|point| history.hit(point, now));
                    let pressed = match self.pressed {
                        Some(Target::History(target)) => Some(target),
                        _ => None,
                    };
                    history.set_interaction(hovered, pressed, now)?;
                }
                Body::Settings(settings) => {
                    let pressed = match self.pressed {
                        Some(Target::Settings(action)) => Some(action),
                        _ => None,
                    };
                    settings.set_pointer(&view.animator, point, pressed, now)?;
                }
            }
        }
        Ok(())
    }

    fn schedule_frames(&mut self, now: Instant) {
        if self.policy.allows_motion() {
            self.sample_until = Some(now + SCROLL_FRAMES);
            self.next_frame = Some(now + FRAME_INTERVAL);
        }
    }

    fn target(&self, id: FocusId) -> Option<Target> {
        self.targets
            .iter()
            .find(|(key, _)| id == *key)
            .map(|(_, target)| *target)
    }

    fn publish_and_decode(&mut self, models: Models<'_>, now: Instant) -> Result<()> {
        let host = self.host.as_ref().unwrap();
        let scene = host.scene().unwrap();
        let view = self.view.as_mut().unwrap();
        let mut previous = std::mem::take(&mut self.targets);
        let mut id_for = |target| {
            let id = if let Some(index) = previous.iter().position(|(_, action)| *action == target)
            {
                previous.swap_remove(index).0
            } else {
                self.next_focus += 1;
                FocusId(self.next_focus)
            };
            self.targets.push((id, target));
            id
        };
        let mut elements = Vec::new();
        match &mut view.body {
            Body::Settings(view) => {
                for control in view.controls(now) {
                    let role = match control.role {
                        ControlRole::Button => FocusRole::Button,
                        ControlRole::Toggle { checked } => FocusRole::Toggle { checked },
                        ControlRole::Choice { selected } => FocusRole::Toggle { checked: selected },
                    };
                    elements.push(FocusElement {
                        id: id_for(Target::Settings(control.action)),
                        parent: None,
                        name: control.label.to_owned(),
                        role,
                        bounds: control.bounds,
                        enabled: control.enabled,
                    });
                }
            }
            Body::History(view) => {
                view.refresh_thumbnails(scene, models.history, now, |request| {
                    thumbnail(models.store, request)
                })?;
                let controls = view.controls(models.history, now);
                let list = FocusId(0);
                let mut last_row = None;
                let mut list_published = false;
                for control in controls {
                    let id = id_for(Target::History(control.target));
                    let (role, parent) = match control.target {
                        HistoryTarget::Row(_) => {
                            if !list_published {
                                list_published = true;
                                elements.push(FocusElement {
                                    id: list,
                                    parent: None,
                                    name: tr!(models.settings.language(), "剪贴板历史")
                                        .into_owned(),
                                    role: FocusRole::List,
                                    bounds: self.viewport,
                                    enabled: true,
                                });
                            }
                            last_row = Some(id);
                            (FocusRole::ListItem, Some(list))
                        }
                        HistoryTarget::Action(
                            HistoryAction::Copy(_)
                            | HistoryAction::Send(_)
                            | HistoryAction::Delete(_),
                        ) => (FocusRole::Button, last_row),
                        _ => (FocusRole::Button, None),
                    };
                    elements.push(FocusElement {
                        id,
                        parent,
                        name: control.label,
                        role,
                        bounds: control.bounds,
                        enabled: true,
                    });
                }
            }
        }
        host.set_focus_elements(elements)
    }
}

impl Drop for Popup<'_> {
    fn drop(&mut self) {
        self.hide();
    }
}

fn key_input(message: &MSG, screen: Screen, models: &Models<'_>) -> Input {
    let key = message.wParam.0 as u16;
    let target = match screen {
        Screen::History => match key {
            key if key == VK_UP.0 => return Input::MoveSelection(-1),
            key if key == VK_DOWN.0 => return Input::MoveSelection(1),
            key if key == VK_LEFT.0 => Some(HistoryTarget::Tab(Source::Phone)),
            key if key == VK_RIGHT.0 => Some(HistoryTarget::Tab(Source::Pc)),
            key if key == VK_DELETE.0 => models
                .history
                .selected()
                .map(|id| HistoryTarget::Action(HistoryAction::Delete(id))),
            key if key == VK_RETURN.0 || (key == u16::from(b'C') && control_down()) => models
                .history
                .selected()
                .map(|id| HistoryTarget::Action(HistoryAction::Copy(id))),
            _ => None,
        }
        .map(Target::History),
        Screen::Guide if key == VK_RETURN.0 => {
            let connection = models.settings.connection();
            Some(Target::Settings(if connection.pairing_complete() {
                SettingsAction::FinishPairing
            } else if connection.phase.can_continue() {
                SettingsAction::Retry
            } else {
                SettingsAction::RequestPair
            }))
        }
        _ => None,
    };
    target.map_or(Input::Dispatch, Input::Target)
}

fn control_down() -> bool {
    // SAFETY: GetKeyState reads the calling thread's event queue keyboard state.
    (unsafe { GetKeyState(i32::from(VK_CONTROL.0)) }) < 0
}

/// Decode the original image to cover the row's slot, falling back to the stored preview.
fn thumbnail(
    store: &copiago_core::history::History,
    request: ThumbnailRequest<'_>,
) -> Option<ThumbnailPixels> {
    let original = match store.get(request.id.id) {
        Ok(Some(copiago_core::history::StoredClip::Image(bytes))) => {
            decode_scaled(&bytes, request.width, request.height).ok()
        }
        _ => None,
    };
    let picture = original.or_else(|| decode_scaled(request.preview?, 88, 56).ok())?;
    let (width, height) = picture.dimensions();
    Some(ThumbnailPixels {
        width,
        height,
        bgra: picture.pixels().to_vec(),
    })
}

enum Layout {
    History(Box<HistoryLayout>),
    Settings,
}

impl Layout {
    fn new(screen: Screen, page: &HistoryPage) -> Result<Self> {
        Ok(if screen == Screen::History {
            Self::History(Box::new(page.layout()?))
        } else {
            Self::Settings
        })
    }

    fn bounds(&self) -> PopupBounds {
        match self {
            Self::History(layout) => layout.bounds(),
            Self::Settings => SettingsView::BOUNDS,
        }
    }

    fn viewport(&self) -> Option<Rect> {
        match self {
            Self::History(layout) => Some(layout.viewport()),
            Self::Settings => None,
        }
    }
}

enum Body {
    History(Box<HistoryView>),
    Settings(Box<SettingsView>),
}

struct PageView {
    animator: Animator,
    body: Body,
}

impl PageView {
    fn new(
        scene: &PopupScene,
        screen: Screen,
        layout: &Layout,
        models: &Models<'_>,
        policy: MotionPolicy,
        now: Instant,
    ) -> Result<Self> {
        let animator = Animator::new(scene.compositor(), policy)?;
        let body = match layout {
            Layout::History(layout) => Body::History(Box::new(HistoryView::new(
                scene,
                models.history,
                layout,
                policy,
                now,
            )?)),
            Layout::Settings => Body::Settings(Box::new(SettingsView::new(
                scene,
                &animator,
                models.settings,
                screen.settings(),
                now,
            )?)),
        };
        Ok(Self { animator, body })
    }

    fn visual(&self) -> &ContainerVisual {
        match &self.body {
            Body::History(view) => view.visual(),
            Body::Settings(view) => view.visual(),
        }
    }

    fn history(&mut self) -> Option<&mut HistoryView> {
        match &mut self.body {
            Body::History(view) => Some(view),
            _ => None,
        }
    }

    fn refresh(
        &mut self,
        scene: &PopupScene,
        layout: &Layout,
        models: &Models<'_>,
        policy: MotionPolicy,
        now: Instant,
    ) -> Result<()> {
        self.animator.set_policy(policy);
        match (&mut self.body, layout) {
            (Body::History(view), Layout::History(layout)) => {
                view.set_motion_policy(policy);
                view.refresh(scene, models.history, layout, now)
            }
            (Body::Settings(view), Layout::Settings) => {
                view.update(scene, &self.animator, models.settings, now)
            }
            _ => unreachable!("layout belongs to the mounted page"),
        }
    }

    fn hit(&self, point: Vector2, now: Instant) -> Option<Target> {
        match &self.body {
            Body::History(view) => view.hit(point, now).map(Target::History),
            Body::Settings(view) => view.hit_test(point, now).map(Target::Settings),
        }
    }

    fn scroll_offset(&self) -> f32 {
        match &self.body {
            Body::History(view) => view.scroll_offset(),
            Body::Settings(view) => view.scroll_offset(),
        }
    }

    fn restore_scroll(&mut self, offset: f32, now: Instant) -> Result<()> {
        match &mut self.body {
            Body::History(view) => view.restore_scroll(offset, now),
            Body::Settings(view) => view.restore_scroll(offset, now),
        }
    }

    fn scroll(&mut self, delta: i16, now: Instant) -> Result<()> {
        match &mut self.body {
            Body::History(view) => view.scroll_wheel(delta, now),
            Body::Settings(view) => view.scroll_wheel(&self.animator, delta, now),
        }
    }
}

fn attach(scene: &PopupScene, view: &PageView) -> Result<()> {
    scene.content().Children()?.InsertAtTop(view.visual())
}
