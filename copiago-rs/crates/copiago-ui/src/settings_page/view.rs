use std::time::{Duration, Instant};

use copiago_core::{
    DeviceInfo,
    i18n::{Language, LanguagePreference, tr},
    settings::SyncKind,
};
use windows::{
    UI::{Color, Composition::ContainerVisual},
    core::Result,
};
use windows_numerics::Vector2;

use super::{
    ConnectionPhase, DriverState, SettingSwitch, SettingsAction, SettingsState, StartupState,
};
use crate::{
    animation::{Animator, MotionPolicy, Tween},
    components::{
        Button, Interaction, LIGHT_THEME as THEME, LiveLabel, ProgressBar, ProgressState, Rect,
        ScrollLayout, ScrollView, Status, StatusPill, Toggle, center, holder, xy,
    },
    composition::{Font, PopupBounds, PopupScene, RoundedRectangle, TextStyle, rounded_rectangle},
};

mod feedback;
pub use feedback::SettingsLayout;
mod form;
mod guide;

const WIDTH: f32 = 392.0;
const HEIGHT: f32 = 478.0;
const PAD: f32 = 18.0;
const INNER: f32 = WIDTH - 2.0 * PAD;

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
    geometry: SettingsLayout,
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
        let geometry = SettingsLayout::measure(state, screen, scene.content().Size()?.Y)?;
        Self::with_layout(scene, animator, state, screen, now, &geometry)
    }

    pub fn with_layout(
        scene: &PopupScene,
        animator: &Animator,
        state: &SettingsState,
        screen: SettingsScreen,
        now: Instant,
        geometry: &SettingsLayout,
    ) -> Result<Self> {
        let visual = holder(scene, Rect::new(0.0, 0.0, WIDTH, geometry.height))?;
        let layout = LayoutKey::new(state, screen, now);
        let body = Body::new(scene, animator, state, &layout, geometry)?;
        visual.Children()?.InsertAtTop(&body.visual)?;
        let mut view = Self {
            visual,
            screen,
            body,
            layout,
            geometry: geometry.clone(),
            was_busy: false,
            progress_complete_until: None,
            policy: animator.policy(),
        };
        view.update_layout(scene, animator, state, now, geometry)?;
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
        let geometry = SettingsLayout::measure(state, self.screen, scene.content().Size()?.Y)?;
        self.update_layout(scene, animator, state, now, &geometry)
    }

    pub fn update_layout(
        &mut self,
        scene: &PopupScene,
        animator: &Animator,
        state: &SettingsState,
        now: Instant,
        geometry: &SettingsLayout,
    ) -> Result<()> {
        let next_layout = LayoutKey::new(state, self.screen, now);
        if next_layout == self.layout
            && geometry != &self.geometry
            && geometry.same_frame(&self.geometry)
        {
            // Progress text only: rewrite the lines and keep the page, its progress bar
            // animation and its scroll position.
            for (label, (line, _)) in self.body.live_lines.iter_mut().zip(&geometry.lines) {
                label.set(scene, line)?;
            }
            if let (Some(label), Some(activity)) =
                (self.body.live_activity.as_mut(), geometry.activity())
            {
                label.set(scene, activity)?;
            }
            self.geometry = geometry.clone();
        } else if next_layout != self.layout || geometry != &self.geometry {
            let mut next = Body::new(scene, animator, state, &next_layout, geometry)?;
            let offset = self.body.scroll.offset_at(now);
            if offset > 0.0
                && !(geometry.inline
                    && (geometry.lines != self.geometry.lines || !self.geometry.inline))
            {
                let snap = Animator::new(scene.compositor(), MotionPolicy::Reduced)?;
                next.scroll.scroll_to(&snap, offset, now)?;
            }
            self.visual.SetSize(Vector2::new(WIDTH, geometry.height))?;
            // Crossfade only when the status and feedback area changes in place. A click on
            // a choice (language, synced types, size limit) rebuilds the same page with one
            // control changed; fading the whole page for that reads as a flicker. A language
            // change replaces every label at once, so it swaps instantly as well.
            let feedback_changed = geometry != &self.geometry;
            if feedback_changed && next_layout.language == self.layout.language {
                crate::page_transition::crossfade(
                    animator,
                    &self.visual,
                    &self.body.visual,
                    &next.visual,
                )?;
            } else {
                let children = self.visual.Children()?;
                children.InsertAtTop(&next.visual)?;
                children.Remove(&self.body.visual)?;
            }
            self.geometry = geometry.clone();
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

    pub fn scroll_offset(&self) -> f32 {
        self.body.scroll.target_offset()
    }

    pub fn restore_scroll(&mut self, offset: f32, now: Instant) -> Result<()> {
        self.body.scroll.jump_to(offset, now)
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
    },
    Guide {
        phase: ConnectionPhase,
        complete: bool,
    },
}

impl LayoutKey {
    fn new(state: &SettingsState, screen: SettingsScreen, now: Instant) -> Self {
        let settings = state.settings();
        let connection = state.connection();
        let content = match screen {
            SettingsScreen::Settings => ContentKey::Settings {
                device: connection.device.clone(),
                retry: connection.can_retry().then_some(connection.phase),
                confirming: state.confirming_forget(now),
                forget_enabled: state.can_activate(SettingsAction::ForgetDevice),
                startup_available: state.startup() != StartupState::Unavailable,
                language_choice: settings.language,
                pull_types: settings.pull_types.clone(),
                push_types: settings.push_types.clone(),
                file_ask_mb: settings.file_ask_mb,
                update: state.available_version().map(str::to_owned),
            },
            SettingsScreen::PairingGuide => ContentKey::Guide {
                phase: connection.phase,
                complete: connection.pairing_complete(),
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
    height: f32,
    scroll_all: bool,
    progress: Option<ProgressBar>,
    pill: Option<StatusPill>,
    live_lines: Vec<LiveLabel>,
    live_activity: Option<LiveLabel>,
}

impl Body {
    fn new(
        scene: &PopupScene,
        animator: &Animator,
        state: &SettingsState,
        key: &LayoutKey,
        geometry: &SettingsLayout,
    ) -> Result<Self> {
        let viewport = geometry.viewport;
        let mut body = Self {
            visual: holder(scene, Rect::new(0.0, 0.0, WIDTH, geometry.height))?,
            scroll: ScrollView::new(scene, ScrollLayout::new(viewport, 0.0)?, &THEME)?,
            controls: Vec::new(),
            height: geometry.height,
            scroll_all: geometry.scroll_all,
            progress: None,
            pill: None,
            live_lines: Vec::new(),
            live_activity: None,
        };
        body.visual.Children()?.InsertAtTop(body.scroll.visual())?;
        let mut build = Builder {
            measure: crate::composition::TextMeasurer::new()?,
            scene,
            animator,
            state,
            language: key.language,
            body: &mut body,
        };
        let start = build.feedback(geometry)?;
        let bottom = match key.content {
            ContentKey::Settings { confirming, .. } => build.settings(confirming, start)?,
            ContentKey::Guide { complete, .. } => build.guide(complete, start)?,
        };
        body.scroll
            .set_content_height(bottom + 8.0, Instant::now())?;
        Ok(body)
    }
}

struct Builder<'a> {
    measure: crate::composition::TextMeasurer,
    scene: &'a PopupScene,
    animator: &'a Animator,
    state: &'a SettingsState,
    language: Language,
    body: &'a mut Body,
}

impl Builder<'_> {
    fn target(&self, scrolled: bool) -> &ContainerVisual {
        if scrolled || self.body.scroll_all {
            self.body.scroll.content()
        } else {
            &self.body.visual
        }
    }

    fn add_button(
        &mut self,
        button: Button,
        label: String,
        action: SettingsAction,
        scrolled: bool,
        role: ControlRole,
    ) -> Result<()> {
        let button = button.with_enabled(self.state.can_activate(action))?;
        self.target(scrolled)
            .Children()?
            .InsertAtTop(button.visual())?;
        self.body.controls.push(Control {
            action,
            label,
            scrolled: scrolled || self.body.scroll_all,
            input: Input::Button { button, role },
        });
        Ok(())
    }

    fn add_progress(&mut self, y: f32, scrolled: bool) -> Result<()> {
        let bar = ProgressBar::new(
            self.scene,
            Vector2::new(PAD + 12.0, y),
            INNER - 24.0,
            &THEME,
        )?;
        self.target(scrolled)
            .Children()?
            .InsertAtTop(bar.visual())?;
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
    connection.pairing_complete()
        || match connection.phase {
            ConnectionPhase::DeveloperMode => true,
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
    } else if connection.phase == ConnectionPhase::Waiting {
        ProgressState::Waiting
    } else if complete {
        ProgressState::Complete
    } else {
        ProgressState::Hidden
    }
}
