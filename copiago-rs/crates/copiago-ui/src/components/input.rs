use crate::{
    animation::{Animator, MotionPolicy, Spring, Tween},
    composition::{Font, PopupScene, RoundedRectangle, TextSurface, rgb, rounded_rectangle},
};

use super::{Rect, Theme, center, holder, text, theme::with_alpha, xy};
use windows::{
    UI::{
        Color,
        Composition::{ContainerVisual, SpriteVisual},
    },
    core::Result,
};
use windows_numerics::Vector2;

/// Input state from the caller's pointer, keyboard, or accessibility handler.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Interaction {
    #[default]
    Idle,
    Hovered,
    Pressed,
}

impl Interaction {
    pub(super) fn active(self) -> bool {
        self != Self::Idle
    }

    pub(super) fn tween(self) -> Tween {
        Tween::new(if self.active() { 120 } else { 180 })
    }

    pub(super) fn press(
        self,
        animator: &Animator,
        visual: &ContainerVisual,
        scale: f32,
    ) -> Result<()> {
        let target = if self == Self::Pressed && animator.policy().allows_motion() {
            scale
        } else {
            1.0
        };
        animator.spring_scale(visual, target, Spring::new(55, 0.82))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonStyle {
    Primary,
    Neutral,
    Danger,
    Tinted,
}

impl ButtonStyle {
    fn colors(self, theme: &Theme) -> ButtonColors {
        let (idle, hover, ink) = match self {
            Self::Primary => (theme.accent, theme.accent_hover, theme.white),
            Self::Neutral => (theme.control, theme.control_hover, theme.soft_ink),
            Self::Danger => (theme.control, theme.danger_tint, theme.danger),
            Self::Tinted => (theme.tint, theme.tint_hover, theme.accent),
        };
        let disabled = if self == Self::Primary {
            with_alpha(idle, 120)
        } else {
            idle
        };
        ButtonColors {
            idle,
            hover,
            ink,
            disabled,
        }
    }
}

struct ButtonColors {
    idle: Color,
    hover: Color,
    ink: Color,
    disabled: Color,
}

/// A button has stable logical bounds while its visual animates.
/// The caller owns activation, focus, and pointer capture.
pub struct Button {
    visual: ContainerVisual,
    fill: RoundedRectangle,
    label: SpriteVisual,
    bounds: Rect,
    colors: ButtonColors,
    enabled: bool,
    interaction: Interaction,
    policy: Option<MotionPolicy>,
}

impl Button {
    pub fn text(
        scene: &PopupScene,
        bounds: Rect,
        style: ButtonStyle,
        label: &str,
        theme: &Theme,
    ) -> Result<Self> {
        let (font, size) = if style == ButtonStyle::Primary {
            (Font::Semibold, 14.0)
        } else {
            (Font::Regular, 12.5)
        };
        let colors = style.colors(theme);
        let surface = text(scene, label, font, size, bounds.width - 16.0, colors.ink)?;
        Self::build(scene, bounds, bounds.height / 2.0, colors, surface)
    }

    pub(crate) fn choice(
        scene: &PopupScene,
        right_center: Vector2,
        label: &str,
        selected: bool,
        single: bool,
        theme: &Theme,
    ) -> Result<Self> {
        let colors = if selected {
            ButtonColors {
                idle: theme.tint,
                hover: theme.tint_hover,
                ink: theme.connected.text,
                disabled: theme.tint,
            }
        } else {
            ButtonColors {
                idle: theme.white,
                hover: theme.control_hover,
                ink: theme.muted,
                disabled: theme.white,
            }
        };
        let font = if selected {
            Font::Semibold
        } else {
            Font::Regular
        };
        let size = if single { 12.0 } else { 12.5 };
        let inset = if single { 16.0 } else { 30.0 };
        let surface = text(scene, label, font, size, 100.0, colors.ink)?;
        let width = surface.size.X.ceil() + inset;
        let bounds = Rect::new(right_center.X - width, right_center.Y - 14.0, width, 28.0);
        Self::build(scene, bounds, 14.0, colors, surface)
    }

    pub(crate) fn forget(
        scene: &PopupScene,
        bounds: Rect,
        label: &str,
        confirming: bool,
        theme: &Theme,
    ) -> Result<Self> {
        let idle = if confirming {
            theme.danger_tint
        } else {
            theme.white
        };
        let colors = ButtonColors {
            idle,
            hover: theme.danger_tint,
            ink: theme.danger,
            disabled: idle,
        };
        let surface = text(
            scene,
            label,
            Font::Regular,
            12.0,
            bounds.width - 16.0,
            colors.ink,
        )?;
        Self::build(scene, bounds, bounds.height / 2.0, colors, surface)
    }

    pub fn icon(
        scene: &PopupScene,
        bounds: Rect,
        style: ButtonStyle,
        glyph: char,
        radius: f32,
        theme: &Theme,
    ) -> Result<Self> {
        let colors = style.colors(theme);
        let surface = scene.icon(glyph, 16.0, colors.ink)?;
        Self::build(scene, bounds, radius, colors, surface)
    }

    fn build(
        scene: &PopupScene,
        bounds: Rect,
        radius: f32,
        colors: ButtonColors,
        surface: TextSurface,
    ) -> Result<Self> {
        let visual = holder(scene, bounds)?;
        visual.SetCenterPoint(xy(bounds.width / 2.0, bounds.height / 2.0))?;
        let fill = rounded_rectangle(scene.compositor(), bounds.size(), radius, colors.idle)?;
        visual.Children()?.InsertAtBottom(&fill.visual)?;
        center(&surface, Rect::new(0.0, 0.0, bounds.width, bounds.height))?;
        visual.Children()?.InsertAtTop(&surface.visual)?;
        Ok(Self {
            visual,
            fill,
            label: surface.visual,
            bounds,
            colors,
            enabled: true,
            interaction: Interaction::Idle,
            policy: None,
        })
    }

    pub fn visual(&self) -> &ContainerVisual {
        &self.visual
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn hit_test(&self, point: Vector2) -> bool {
        enabled_hit(self.bounds, self.enabled, point)
    }

    /// Set the enabled state of a button that has not been shown yet. Views rebuild their
    /// buttons on every state change, so a new disabled button must not fade in from the
    /// enabled fill.
    pub fn with_enabled(mut self, enabled: bool) -> Result<Self> {
        self.enabled = enabled;
        if !enabled {
            self.fill.brush.SetColor(self.colors.disabled)?;
        }
        Ok(self)
    }

    pub fn set_enabled(&mut self, animator: &Animator, enabled: bool) -> Result<()> {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.policy = None;
            self.set_interaction(animator, Interaction::Idle)?;
        }
        Ok(())
    }

    pub fn set_interaction(&mut self, animator: &Animator, interaction: Interaction) -> Result<()> {
        let interaction = if self.enabled {
            interaction
        } else {
            Interaction::Idle
        };
        if interaction == self.interaction && self.policy == Some(animator.policy()) {
            return Ok(());
        }
        let color = if !self.enabled {
            self.colors.disabled
        } else if interaction.active() {
            self.colors.hover
        } else {
            self.colors.idle
        };
        animator.animate_color(&self.fill.brush, color, interaction.tween())?;
        if self.interaction == Interaction::Pressed
            || interaction == Interaction::Pressed
            || self.policy == Some(MotionPolicy::Full) && !animator.policy().allows_motion()
        {
            interaction.press(animator, &self.visual, 0.94)?;
        }
        self.interaction = interaction;
        self.policy = Some(animator.policy());
        Ok(())
    }

    /// Replace the icon while retaining its hit rectangle and interaction state.
    pub fn set_icon(&mut self, scene: &PopupScene, glyph: char) -> Result<()> {
        let surface = scene.icon(glyph, 16.0, self.colors.ink)?;
        center(
            &surface,
            Rect::new(0.0, 0.0, self.bounds.width, self.bounds.height),
        )?;
        let children = self.visual.Children()?;
        children.InsertAtTop(&surface.visual)?;
        children.Remove(&self.label)?;
        self.label = surface.visual;
        Ok(())
    }
}

/// A 44 × 26 DIP switch. A settings page can give the label and switch a
/// single, shared hit rectangle via hit_bounds.
pub struct Toggle {
    visual: ContainerVisual,
    track: RoundedRectangle,
    knob: ContainerVisual,
    hit_bounds: Rect,
    colors: (Color, Color),
    checked: bool,
    enabled: bool,
    policy: Option<MotionPolicy>,
}

impl Toggle {
    pub const WIDTH: f32 = 44.0;
    pub const HEIGHT: f32 = 26.0;

    pub fn new(
        scene: &PopupScene,
        origin: Vector2,
        hit_bounds: Rect,
        checked: bool,
        enabled: bool,
        theme: &Theme,
    ) -> Result<Self> {
        hit_bounds.validate()?;
        let visual = holder(
            scene,
            Rect::new(origin.X, origin.Y, Self::WIDTH, Self::HEIGHT),
        )?;
        let colors = (theme.switch_off, theme.accent);
        let track = rounded_rectangle(
            scene.compositor(),
            Vector2::new(Self::WIDTH, Self::HEIGHT),
            13.0,
            track_color(colors, checked, enabled),
        )?;
        let knob = holder(scene, Rect::new(knob_x(checked), 3.0, 20.0, 20.0))?;
        let outline = rounded_rectangle(
            scene.compositor(),
            Vector2::new(20.0, 20.0),
            10.0,
            with_alpha(rgb(0, 0, 0), 45),
        )?;
        outline.visual.SetOffset(xy(0.0, 1.0))?;
        knob.Children()?.InsertAtBottom(&outline.visual)?;
        let face = rounded_rectangle(
            scene.compositor(),
            Vector2::new(20.0, 20.0),
            10.0,
            theme.white,
        )?;
        knob.Children()?.InsertAtTop(&face.visual)?;
        visual.Children()?.InsertAtBottom(&track.visual)?;
        visual.Children()?.InsertAtTop(&knob)?;
        Ok(Self {
            visual,
            track,
            knob,
            hit_bounds,
            colors,
            checked,
            enabled,
            policy: None,
        })
    }

    pub fn visual(&self) -> &ContainerVisual {
        &self.visual
    }

    pub fn bounds(&self) -> Rect {
        self.hit_bounds
    }

    pub fn checked(&self) -> bool {
        self.checked
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn hit_test(&self, point: Vector2) -> bool {
        enabled_hit(self.hit_bounds, self.enabled, point)
    }

    pub fn set_state(&mut self, animator: &Animator, checked: bool, enabled: bool) -> Result<()> {
        if checked == self.checked
            && enabled == self.enabled
            && self.policy == Some(animator.policy())
        {
            return Ok(());
        }
        let timing = Tween::new(180);
        animator.animate_color(
            &self.track.brush,
            track_color(self.colors, checked, enabled),
            timing,
        )?;
        animator.offset(&self.knob, xy(knob_x(checked), 3.0), timing)?;
        self.checked = checked;
        self.enabled = enabled;
        self.policy = Some(animator.policy());
        Ok(())
    }
}

fn knob_x(checked: bool) -> f32 {
    if checked { 21.0 } else { 3.0 }
}

fn track_color(colors: (Color, Color), checked: bool, enabled: bool) -> Color {
    let color = if checked { colors.1 } else { colors.0 };
    if enabled {
        color
    } else {
        with_alpha(color, 110)
    }
}

fn enabled_hit(bounds: Rect, enabled: bool, point: Vector2) -> bool {
    enabled && bounds.contains(point)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_controls_cannot_be_hit_and_switch_has_equal_end_insets() {
        let bounds = Rect::new(12.0, 4.0, 90.0, 36.0);
        assert!(enabled_hit(bounds, true, Vector2::new(12.0, 4.0)));
        assert!(!enabled_hit(bounds, false, Vector2::new(12.0, 4.0)));
        assert!(!enabled_hit(bounds, true, Vector2::new(102.0, 4.0)));
        assert_eq!(knob_x(false), 3.0);
        assert_eq!(Toggle::WIDTH - knob_x(true) - 20.0, 3.0);
    }
}
