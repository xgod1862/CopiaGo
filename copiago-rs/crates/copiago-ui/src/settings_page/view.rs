use std::time::{Duration, Instant};

use copiago_core::{
    DeviceInfo,
    i18n::{Language, LanguagePreference, tr},
    settings::SyncKind,
};
use windows::{
    UI::{
        Color,
        Composition::{ContainerVisual, SpriteVisual},
    },
    core::Result,
};
use windows_numerics::Vector2;

use super::{
    ConnectionPhase, DriverState, SettingSwitch, SettingsAction, SettingsState, StartupState,
};
use crate::{
    animation::{Animator, MotionPolicy, Tween},
    components::{
        Button, Interaction, LIGHT_THEME as THEME, ProgressBar, ProgressState, Rect, ScrollLayout,
        ScrollView, Status, StatusPill, Toggle, center, holder, xy,
    },
    composition::{Font, PopupBounds, PopupScene, RoundedRectangle, TextStyle, rounded_rectangle},
};

mod form;
mod guide;

const WIDTH: f32 = 392.0;
const HEIGHT: f32 = 478.0;
const PAD: f32 = 18.0;
const INNER: f32 = WIDTH - 2.0 * PAD;
const PANEL: Rect = Rect::new(0.0, 0.0, WIDTH, HEIGHT);
const SETTINGS_VIEWPORT: Rect = Rect::new(0.0, 64.0, WIDTH, 352.0);
const GUIDE_VIEWPORT: Rect = Rect::new(0.0, 64.0, WIDTH, 339.0);
const DRIVER_VIEWPORT: Rect = Rect::new(0.0, 64.0, WIDTH, 179.0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsScreen {
    Settings,
    PairingGuide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlRole {
    Button,
    Toggle { checked: bool },
    Choice { selected: bool },
}

#[derive(Clone, Copy, Debug)]
pub struct ControlInfo<'a> {
    /// An identity that remains stable when the view changes language or scrolls.
    pub action: SettingsAction,
    pub label: &'a str,
    pub role: ControlRole,
    pub enabled: bool,
    /// Clipped bounds in popup DIPs. Offscreen controls remain in traversal order.
    pub bounds: Option<Rect>,
}

/// Visuals for a visible scene. Rebuild after a DPI change and drop with that
/// scene. Keep SettingsState in the host while the popup is hidden.
pub struct SettingsView {
    visual: ContainerVisual,
    screen: SettingsScreen,
    body: Body,
    layout: LayoutKey,
    was_busy: bool,
    progress_complete_until: Option<Instant>,
    policy: MotionPolicy,
}

impl SettingsView {
    pub const BOUNDS: PopupBounds = PopupBounds::new(WIDTH, HEIGHT, 24.0);

    pub fn new(
        scene: &PopupScene,
        animator: &Animator,
        state: &SettingsState,
        screen: SettingsScreen,
        now: Instant,
    ) -> Result<Self> {
        let visual = holder(scene, PANEL)?;
        let layout = LayoutKey::new(state, screen, now);
        let body = Body::new(scene, animator, state, &layout)?;
        visual.Children()?.InsertAtTop(&body.visual)?;
        let mut view = Self {
            visual,
            screen,
            body,
            layout,
            was_busy: false,
            progress_complete_until: None,
            policy: animator.policy(),
        };
        view.update(scene, animator, state, now)?;
        Ok(view)
    }

    pub fn visual(&self) -> &ContainerVisual {
        &self.visual
    }

    /// Reapply after state changes, after the confirmation deadline and when
    /// reduced motion changes. Progress and switches update in their current visuals.
    pub fn update(
        &mut self,
        scene: &PopupScene,
        animator: &Animator,
        state: &SettingsState,
        now: Instant,
    ) -> Result<()> {
        let next_layout = LayoutKey::new(state, self.screen, now);
        if next_layout != self.layout {
            let mut next = Body::new(scene, animator, state, &next_layout)?;
            let offset = self.body.scroll.offset_at(now);
            if offset > 0.0 {
                let snap = Animator::new(scene.compositor(), MotionPolicy::Reduced)?;
                next.scroll.scroll_to(&snap, offset, now)?;
            }
            let children = self.visual.Children()?;
            children.InsertAtTop(&next.visual)?;
            children.Remove(&self.body.visual)?;
            self.body = next;
            self.layout = next_layout;
        }
        if self.policy != animator.policy() && !animator.policy().allows_motion() {
            let target = self.body.scroll.target_offset();
            self.body.scroll.scroll_to(animator, target, now)?;
        }
        self.policy = animator.policy();

        let connection = state.connection();
        let status = pill_status(connection.phase);
        let language = self.layout.language;
        if let Some(pill) = self.body.pill.as_mut() {
            pill.set_status(scene, animator, status, &pill_label(language, status))?;
        }
        let notice = state.notice().map(|notice| notice.render(language));
        if let Some(slot) = self.body.notice.as_mut() {
            slot.set(
                scene,
                notice.as_deref().unwrap_or(""),
                THEME.disconnected.text,
            )?;
        }
        if let Some(slot) = self.body.detail.as_mut() {
            let default = connection.default_detail();
            let detail = connection.detail.as_ref().unwrap_or(&default);
            let display = detail.render(language);
            let color = status.colors(&THEME).text;
            match self.screen {
                SettingsScreen::Settings => slot.set(scene, &display, color)?,
                SettingsScreen::PairingGuide => {
                    if let Some(notice) = notice {
                        slot.set(scene, &notice, THEME.disconnected.text)?;
                    } else if !guide_detail_visible(state) {
                        slot.set(scene, "", color)?;
                    } else {
                        let color = if connection.phase == ConnectionPhase::Setup {
                            THEME.disconnected.text
                        } else {
                            color
                        };
                        slot.set(scene, &display, color)?;
                    }
                }
            }
        }
        if !matches!(connection.phase, ConnectionPhase::Connected { .. }) {
            self.progress_complete_until = None;
        } else if self.was_busy {
            self.progress_complete_until = now.checked_add(Duration::from_millis(1560));
        }
        self.was_busy = connection.phase.busy();
        if let Some(bar) = self.body.progress.as_mut() {
            let complete = self
                .progress_complete_until
                .is_some_and(|until| now < until);
            bar.set_state(animator, progress_state(state, complete))?;
        }
        for control in &mut self.body.controls {
            if let SettingsAction::Toggle(setting) = control.action
                && let Input::Toggle { toggle, .. } = &mut control.input
            {
                let (checked, enabled) = state.switch(setting);
                toggle.set_state(animator, checked, enabled)?;
            }
        }
        Ok(())
    }

    pub fn controls(&self, now: Instant) -> impl Iterator<Item = ControlInfo<'_>> {
        self.body.controls.iter().map(move |control| ControlInfo {
            action: control.action,
            label: &control.label,
            role: control.role(),
            enabled: control.enabled(),
            bounds: control.visible_bounds(&self.body.scroll, now),
        })
    }

    pub fn hit_test(&self, point: Vector2, now: Instant) -> Option<SettingsAction> {
        self.controls(now)
            .find(|control| {
                control.enabled && control.bounds.is_some_and(|area| area.contains(point))
            })
            .map(|control| control.action)
    }

    pub fn set_pointer(
        &mut self,
        animator: &Animator,
        point: Option<Vector2>,
        pressed: Option<SettingsAction>,
        now: Instant,
    ) -> Result<()> {
        let hover = point.and_then(|point| self.hit_test(point, now));
        for control in &mut self.body.controls {
            let interaction = if hover == Some(control.action) {
                if hover == pressed {
                    Interaction::Pressed
                } else {
                    Interaction::Hovered
                }
            } else {
                Interaction::Idle
            };
            control.set_interaction(animator, interaction)?;
        }
        Ok(())
    }

    pub fn scroll_wheel(&mut self, animator: &Animator, delta: i16, now: Instant) -> Result<()> {
        self.body.scroll.scroll_wheel(animator, delta, now)
    }

    pub fn reveal(
        &mut self,
        animator: &Animator,
        action: SettingsAction,
        now: Instant,
    ) -> Result<()> {
        if let Some(control) = self
            .body
            .controls
            .iter()
            .find(|control| control.scrolled && control.action == action && control.enabled())
        {
            self.body.scroll.reveal(animator, control.bounds(), now)?;
        }
        Ok(())
    }
}

#[derive(PartialEq, Eq)]
struct LayoutKey {
    language: Language,
    content: ContentKey,
}

#[derive(PartialEq, Eq)]
enum ContentKey {
    Settings {
        device: Option<DeviceInfo>,
        retry: Option<ConnectionPhase>,
        confirming: bool,
        forget_enabled: bool,
        startup_available: bool,
        language_choice: LanguagePreference,
        pull_types: Vec<SyncKind>,
        push_types: Vec<SyncKind>,
        file_ask_mb: u8,
        update: Option<String>,
        notice: bool,
    },
    Guide {
        phase: ConnectionPhase,
        complete: bool,
        detail_visible: bool,
        driver: DriverState,
    },
}

impl LayoutKey {
    fn new(state: &SettingsState, screen: SettingsScreen, now: Instant) -> Self {
        let settings = state.settings();
        let connection = state.connection();
        let content = match screen {
            SettingsScreen::Settings => ContentKey::Settings {
                device: connection.device.clone(),
                retry: connection.phase.can_continue().then_some(connection.phase),
                confirming: state.confirming_forget(now),
                forget_enabled: state.can_activate(SettingsAction::ForgetDevice),
                startup_available: state.startup() != StartupState::Unavailable,
                language_choice: settings.language,
                pull_types: settings.pull_types.clone(),
                push_types: settings.push_types.clone(),
                file_ask_mb: settings.file_ask_mb,
                update: state.available_version().map(str::to_owned),
                notice: state.notice().is_some(),
            },
            SettingsScreen::PairingGuide => ContentKey::Guide {
                phase: connection.phase,
                complete: connection.pairing_complete(),
                detail_visible: guide_detail_visible(state),
                driver: connection.driver,
            },
        };
        Self {
            language: state.language(),
            content,
        }
    }
}

struct Body {
    visual: ContainerVisual,
    scroll: ScrollView,
    controls: Vec<Control>,
    detail: Option<TextSlot>,
    notice: Option<TextSlot>,
    progress: Option<ProgressBar>,
    pill: Option<StatusPill>,
}

impl Body {
    fn new(
        scene: &PopupScene,
        animator: &Animator,
        state: &SettingsState,
        key: &LayoutKey,
    ) -> Result<Self> {
        let viewport = match key.content {
            ContentKey::Settings { .. } => SETTINGS_VIEWPORT,
            ContentKey::Guide {
                complete: false,
                driver: DriverState::Missing | DriverState::Installing | DriverState::Failed,
                ..
            } => DRIVER_VIEWPORT,
            ContentKey::Guide { .. } => GUIDE_VIEWPORT,
        };
        let mut body = Self {
            visual: holder(scene, PANEL)?,
            scroll: ScrollView::new(scene, ScrollLayout::new(viewport, 0.0)?, &THEME)?,
            controls: Vec::new(),
            detail: None,
            notice: None,
            progress: None,
            pill: None,
        };
        body.visual.Children()?.InsertAtTop(body.scroll.visual())?;
        let mut build = Builder {
            scene,
            animator,
            state,
            language: key.language,
            body: &mut body,
        };
        let bottom = match key.content {
            ContentKey::Settings { confirming, .. } => build.settings(confirming)?,
            ContentKey::Guide { complete, .. } => build.guide(complete)?,
        };
        body.scroll
            .set_content_height(bottom + 8.0, Instant::now())?;
        Ok(body)
    }
}

struct Builder<'a> {
    scene: &'a PopupScene,
    animator: &'a Animator,
    state: &'a SettingsState,
    language: Language,
    body: &'a mut Body,
}

impl Builder<'_> {
    fn target(&self, scrolled: bool) -> &ContainerVisual {
        if scrolled {
            self.body.scroll.content()
        } else {
            &self.body.visual
        }
    }

    fn add_button(
        &mut self,
        mut button: Button,
        label: String,
        action: SettingsAction,
        scrolled: bool,
        role: ControlRole,
    ) -> Result<()> {
        button.set_enabled(self.animator, self.state.can_activate(action))?;
        self.target(scrolled)
            .Children()?
            .InsertAtTop(button.visual())?;
        self.body.controls.push(Control {
            action,
            label,
            scrolled,
            input: Input::Button { button, role },
        });
        Ok(())
    }

    fn add_progress(&mut self, y: f32) -> Result<()> {
        let bar = ProgressBar::new(
            self.scene,
            Vector2::new(PAD + 12.0, y),
            INNER - 24.0,
            &THEME,
        )?;
        self.target(true).Children()?.InsertAtTop(bar.visual())?;
        self.body.progress = Some(bar);
        Ok(())
    }
}

struct Control {
    action: SettingsAction,
    label: String,
    scrolled: bool,
    input: Input,
}

enum Input {
    Button {
        button: Button,
        role: ControlRole,
    },
    Toggle {
        toggle: Toggle,
        hover: RoundedRectangle,
        hovered: bool,
        policy: MotionPolicy,
    },
}

impl Control {
    fn role(&self) -> ControlRole {
        match &self.input {
            Input::Button { role, .. } => *role,
            Input::Toggle { toggle, .. } => ControlRole::Toggle {
                checked: toggle.checked(),
            },
        }
    }

    fn enabled(&self) -> bool {
        match &self.input {
            Input::Button { button, .. } => button.enabled(),
            Input::Toggle { toggle, .. } => toggle.enabled(),
        }
    }

    fn bounds(&self) -> Rect {
        match &self.input {
            Input::Button { button, .. } => button.bounds(),
            Input::Toggle { toggle, .. } => toggle.bounds(),
        }
    }

    fn visible_bounds(&self, scroll: &ScrollView, now: Instant) -> Option<Rect> {
        let bounds = self.bounds();
        if self.scrolled {
            scroll
                .layout()
                .visible_bounds(bounds, scroll.offset_at(now))
        } else {
            Some(bounds)
        }
    }

    fn set_interaction(&mut self, animator: &Animator, interaction: Interaction) -> Result<()> {
        match &mut self.input {
            Input::Button { button, .. } => button.set_interaction(animator, interaction),
            Input::Toggle {
                hover,
                toggle,
                hovered,
                policy,
            } => {
                let active = toggle.enabled() && interaction != Interaction::Idle;
                if active != *hovered || animator.policy() != *policy {
                    let color = if active {
                        THEME.card_hover
                    } else {
                        transparent(THEME.card_hover)
                    };
                    let timing = Tween::new(if active { 120 } else { 180 });
                    animator.animate_color(&hover.brush, color, timing)?;
                    *hovered = active;
                    *policy = animator.policy();
                }
                Ok(())
            }
        }
    }
}

struct TextSlot {
    visual: ContainerVisual,
    bounds: Rect,
    lines: u8,
    value: String,
    color: Color,
    label: Option<SpriteVisual>,
}

impl TextSlot {
    fn new(scene: &PopupScene, parent: &ContainerVisual, bounds: Rect, lines: u8) -> Result<Self> {
        let visual = holder(scene, bounds)?;
        parent.Children()?.InsertAtTop(&visual)?;
        Ok(Self {
            visual,
            bounds,
            lines,
            value: String::new(),
            color: THEME.muted,
            label: None,
        })
    }

    fn set(&mut self, scene: &PopupScene, content: &str, color: Color) -> Result<()> {
        if self.value == content && self.color == color {
            return Ok(());
        }
        let children = self.visual.Children()?;
        if let Some(label) = self.label.take() {
            children.Remove(&label)?;
        }
        if !content.is_empty() {
            let style = TextStyle {
                font: Font::Regular,
                size: 12.0,
                max_width: self.bounds.width,
                color,
            };
            let surface = scene.wrapped_text(content, style, self.lines)?;
            children.InsertAtTop(&surface.visual)?;
            self.label = Some(surface.visual);
        }
        self.value.clear();
        self.value.push_str(content);
        self.color = color;
        Ok(())
    }
}

fn label(
    scene: &PopupScene,
    parent: &ContainerVisual,
    value: &str,
    bounds: Rect,
    font: Font,
    size: f32,
    color: Color,
) -> Result<()> {
    let surface = scene.text(
        value,
        TextStyle {
            font,
            size,
            max_width: bounds.width,
            color,
        },
    )?;
    surface.visual.SetOffset(xy(
        bounds.x,
        bounds.y + (bounds.height - surface.size.Y) / 2.0,
    ))?;
    parent.Children()?.InsertAtTop(&surface.visual)
}

fn shape(scene: &PopupScene, bounds: Rect, radius: f32, color: Color) -> Result<RoundedRectangle> {
    let shape = rounded_rectangle(scene.compositor(), bounds.size(), radius, color)?;
    shape.visual.SetOffset(xy(bounds.x, bounds.y))?;
    Ok(shape)
}

fn background(
    scene: &PopupScene,
    parent: &ContainerVisual,
    bounds: Rect,
    radius: f32,
    color: Color,
) -> Result<()> {
    parent
        .Children()?
        .InsertAtTop(&shape(scene, bounds, radius, color)?.visual)
}

fn icon(
    scene: &PopupScene,
    parent: &ContainerVisual,
    bounds: Rect,
    glyph: char,
    size: f32,
    color: Color,
) -> Result<()> {
    let surface = scene.icon(glyph, size, color)?;
    center(&surface, bounds)?;
    parent.Children()?.InsertAtTop(&surface.visual)
}

fn transparent(color: Color) -> Color {
    Color { A: 0, ..color }
}

fn pill_status(phase: ConnectionPhase) -> Status {
    match phase {
        ConnectionPhase::Connected { .. } => Status::Connected,
        ConnectionPhase::Waiting
        | ConnectionPhase::DeveloperMode
        | ConnectionPhase::RetryRequired
        | ConnectionPhase::Error => Status::Disconnected,
        _ => Status::Connecting,
    }
}

fn pill_label(language: Language, status: Status) -> std::borrow::Cow<'static, str> {
    tr!(
        language,
        match status {
            Status::Connected => "已连接",
            Status::Connecting => "连接中",
            Status::Disconnected => "未连接",
        }
    )
}

fn guide_detail_visible(state: &SettingsState) -> bool {
    let connection = state.connection();
    state.notice().is_some()
        || connection.pairing_complete()
        || match connection.phase {
            ConnectionPhase::DeveloperMode => false,
            ConnectionPhase::Setup => connection
                .detail
                .as_ref()
                .is_some_and(|detail| *detail != connection.default_detail()),
            _ => true,
        }
}

fn progress_state(state: &SettingsState, complete: bool) -> ProgressState {
    let connection = state.connection();
    if connection.phase.busy() {
        let fraction = connection.progress.and_then(|(_, progress)| {
            let total = progress.total_bytes.filter(|total| *total > 0)?;
            Some(progress.received_bytes.min(total) as f32 / total as f32)
        });
        fraction.map_or(ProgressState::Indeterminate, ProgressState::Determinate)
    } else if complete {
        ProgressState::Complete
    } else {
        ProgressState::Hidden
    }
}
