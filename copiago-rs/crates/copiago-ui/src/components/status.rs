use crate::{
    animation::{Animator, MotionPolicy, Spring, Tween, milliseconds},
    composition::{Font, PopupScene, RoundedRectangle, rounded_rectangle},
};

use super::{Rect, Status, Theme, at, center, holder, text, xy};
use windows::{
    UI::Composition::{AnimationIterationBehavior, ContainerVisual, SpriteVisual, Visual},
    Win32::Foundation::E_INVALIDARG,
    core::{Error, Interface, Result, h},
};
use windows_numerics::{Vector2, Vector3};

const PILL_INSET: f32 = 37.0;
const PILL_LABEL_LEFT: f32 = 25.0;
const PILL_TRANSITION: Tween = Tween::new(320);

/// Right-aligned connection status. The caller supplies an already localized label.
pub struct StatusPill {
    visual: ContainerVisual,
    background: RoundedRectangle,
    dot: RoundedRectangle,
    label: SpriteVisual,
    label_text: String,
    label_offset: Vector3,
    right_top: Vector2,
    max_width: f32,
    bounds: Rect,
    status: Status,
    policy: MotionPolicy,
    theme: Theme,
}

impl StatusPill {
    pub const HEIGHT: f32 = 28.0;

    pub fn new(
        scene: &PopupScene,
        animator: &Animator,
        right_top: Vector2,
        max_width: f32,
        status: Status,
        label: &str,
        theme: &Theme,
    ) -> Result<Self> {
        if !max_width.is_finite() || max_width <= PILL_INSET {
            return Err(Error::new(E_INVALIDARG, "Invalid status pill width"));
        }
        let colors = status.colors(theme);
        let label_surface = text(
            scene,
            label,
            Font::Semibold,
            12.0,
            max_width - PILL_INSET,
            colors.text,
        )?;
        let bounds = pill_bounds(right_top, label_surface.size.X, max_width);
        let visual = holder(scene, bounds)?;
        let clip = scene.compositor().CreateInsetClip()?;
        visual.SetClip(&clip)?;
        let background = rounded_rectangle(
            scene.compositor(),
            Vector2::new(max_width, Self::HEIGHT),
            Self::HEIGHT / 2.0,
            colors.background,
        )?;
        background.geometry.SetSize(bounds.size())?;
        let dot = rounded_rectangle(scene.compositor(), Vector2::new(7.0, 7.0), 3.5, colors.dot)?;
        dot.visual.SetOffset(xy(12.0, 10.5))?;
        dot.visual.SetCenterPoint(xy(3.5, 3.5))?;
        let label_offset = pill_label_offset(label_surface.size.Y);
        label_surface.visual.SetOffset(label_offset)?;
        let children = visual.Children()?;
        children.InsertAtTop(&background.visual)?;
        children.InsertAtTop(&dot.visual)?;
        children.InsertAtTop(&label_surface.visual)?;
        configure_dot(animator, &dot.visual.cast()?, status, false)?;
        Ok(Self {
            visual,
            background,
            dot,
            label: label_surface.visual,
            label_text: label.into(),
            label_offset,
            right_top,
            max_width,
            bounds,
            status,
            policy: animator.policy(),
            theme: *theme,
        })
    }

    pub fn visual(&self) -> &ContainerVisual {
        &self.visual
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn label(&self) -> &str {
        &self.label_text
    }

    pub fn set_status(
        &mut self,
        scene: &PopupScene,
        animator: &Animator,
        status: Status,
        label: &str,
    ) -> Result<()> {
        let status_changed = status != self.status;
        let redraw_label = status_changed || label != self.label_text;
        if !redraw_label && self.policy == animator.policy() {
            return Ok(());
        }
        let colors = status.colors(&self.theme);
        if redraw_label {
            let surface = text(
                scene,
                label,
                Font::Semibold,
                12.0,
                self.max_width - PILL_INSET,
                colors.text,
            )?;
            let bounds = pill_bounds(self.right_top, surface.size.X, self.max_width);
            let label_offset = pill_label_offset(surface.size.Y);
            self.replace_label(animator, surface.visual, label_offset)?;
            self.label_text = label.into();
            self.bounds = bounds;
        }
        let size = self.bounds.size();
        animator.animate_v2(
            &self.background.geometry.cast()?,
            h!("Size"),
            size,
            PILL_TRANSITION,
            |size| self.background.geometry.SetSize(size),
        )?;
        animator.animate_v2(
            &self.visual.cast()?,
            h!("Size"),
            size,
            PILL_TRANSITION,
            |size| self.visual.SetSize(size),
        )?;
        animator.offset(&self.visual, at(self.bounds.origin()), PILL_TRANSITION)?;
        animator.animate_color(&self.background.brush, colors.background, PILL_TRANSITION)?;
        animator.animate_color(&self.dot.brush, colors.dot, PILL_TRANSITION)?;
        if status_changed || self.policy != animator.policy() {
            configure_dot(animator, &self.dot.visual.cast()?, status, status_changed)?;
        }
        self.status = status;
        self.policy = animator.policy();
        Ok(())
    }

    fn replace_label(
        &mut self,
        animator: &Animator,
        next: SpriteVisual,
        offset: Vector3,
    ) -> Result<()> {
        let moving = animator.policy().allows_motion();
        next.SetOffset(xy(offset.X, offset.Y + if moving { 6.0 } else { 0.0 }))?;
        next.SetOpacity(if moving { 0.0 } else { 1.0 })?;
        self.visual.Children()?.InsertAtTop(&next)?;
        if moving {
            animator.opacity(&next, 1.0, Tween::delayed(260, 60))?;
            animator.offset(&next, offset, PILL_TRANSITION)?;
        }
        let old_offset = xy(self.label_offset.X, self.label_offset.Y - 6.0);
        animator.detach_after(&self.visual, &self.label, || {
            animator.opacity(&self.label, 0.0, Tween::new(160))?;
            animator.offset(&self.label, old_offset, Tween::new(220))
        })?;
        self.label = next;
        self.label_offset = offset;
        Ok(())
    }
}

fn pill_bounds(right_top: Vector2, text_width: f32, max_width: f32) -> Rect {
    let width = (text_width + PILL_INSET).min(max_width);
    Rect::new(right_top.X - width, right_top.Y, width, StatusPill::HEIGHT)
}

fn pill_label_offset(height: f32) -> Vector3 {
    xy(PILL_LABEL_LEFT, (StatusPill::HEIGHT - height) / 2.0)
}

fn configure_dot(animator: &Animator, dot: &Visual, status: Status, pop: bool) -> Result<()> {
    dot.StopAnimation(h!("Scale"))?;
    dot.StopAnimation(h!("Scale.X"))?;
    dot.StopAnimation(h!("Scale.Y"))?;
    dot.StopAnimation(h!("Opacity"))?;
    dot.SetOpacity(1.0)?;
    dot.SetScale(Vector3::new(1.0, 1.0, 1.0))?;
    if !animator.policy().allows_motion() {
        return Ok(());
    }
    if status == Status::Connecting {
        let compositor = animator.compositor();
        let pulse = compositor.CreateScalarKeyFrameAnimation()?;
        let scale = compositor.CreateVector3KeyFrameAnimation()?;
        for (time, opacity, multiplier) in [(0.0, 1.0, 1.0), (0.5, 0.25, 1.35), (1.0, 1.0, 1.0)] {
            pulse.InsertKeyFrame(time, opacity)?;
            scale.InsertKeyFrame(time, Vector3::new(multiplier, multiplier, 1.0))?;
        }
        pulse.SetDuration(milliseconds(1100))?;
        scale.SetDuration(milliseconds(1100))?;
        pulse.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
        scale.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
        dot.StartAnimation(h!("Opacity"), &pulse)?;
        dot.StartAnimation(h!("Scale"), &scale)?;
    } else if pop {
        dot.SetScale(Vector3::new(1.6, 1.6, 1.0))?;
        animator.spring_scale(dot, 1.0, Spring::new(60, 0.45))?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProgressState {
    Hidden,
    Indeterminate,
    Determinate(f32),
    Complete,
}

/// Five-DIP progress bar. Complete fills the bar, holds, then fades by itself.
pub struct ProgressBar {
    visual: ContainerVisual,
    fill: RoundedRectangle,
    width: f32,
    state: ProgressState,
    policy: Option<MotionPolicy>,
}

impl ProgressBar {
    pub const HEIGHT: f32 = 5.0;

    pub fn new(scene: &PopupScene, origin: Vector2, width: f32, theme: &Theme) -> Result<Self> {
        let visual = holder(scene, Rect::new(origin.X, origin.Y, width, Self::HEIGHT))?;
        visual.SetOpacity(0.0)?;
        let clip = scene.compositor().CreateInsetClip()?;
        visual.SetClip(&clip)?;
        let track = rounded_rectangle(
            scene.compositor(),
            Vector2::new(width, Self::HEIGHT),
            2.5,
            theme.tint,
        )?;
        let fill = rounded_rectangle(
            scene.compositor(),
            Vector2::new(width, Self::HEIGHT),
            2.5,
            theme.accent,
        )?;
        fill.geometry.SetSize(Vector2::new(0.0, Self::HEIGHT))?;
        let children = visual.Children()?;
        children.InsertAtBottom(&track.visual)?;
        children.InsertAtTop(&fill.visual)?;
        Ok(Self {
            visual,
            fill,
            width,
            state: ProgressState::Hidden,
            policy: None,
        })
    }

    pub fn visual(&self) -> &ContainerVisual {
        &self.visual
    }

    pub fn set_state(&mut self, animator: &Animator, state: ProgressState) -> Result<()> {
        let state = match state {
            ProgressState::Determinate(value) => ProgressState::Determinate(fraction(value)),
            state => state,
        };
        if state == self.state && self.policy == Some(animator.policy()) {
            return Ok(());
        }
        self.visual.StopAnimation(h!("Opacity"))?;
        let visible = state != ProgressState::Hidden;
        self.visual.SetOpacity(if visible { 1.0 } else { 0.0 })?;
        if self.state == ProgressState::Indeterminate || state == ProgressState::Indeterminate {
            self.fill.visual.StopAnimation(h!("Offset"))?;
            self.fill.visual.SetOffset(Vector3::default())?;
            self.fill.geometry.StopAnimation(h!("Size"))?;
            self.fill
                .geometry
                .SetSize(Vector2::new(0.0, Self::HEIGHT))?;
        }
        match state {
            ProgressState::Hidden => {
                self.fill.geometry.StopAnimation(h!("Size"))?;
                self.fill
                    .geometry
                    .SetSize(Vector2::new(0.0, Self::HEIGHT))?;
            }
            ProgressState::Indeterminate => self.indeterminate(animator)?,
            ProgressState::Determinate(value) => self.animate_fill(animator, value)?,
            ProgressState::Complete => {
                self.animate_fill(animator, 1.0)?;
                let duration = if animator.policy().allows_motion() {
                    360
                } else {
                    1
                };
                // In reduced mode this hides in one frame after the static hold.
                let delay = animator.policy().duration_ms(700) + 500;
                finish_feedback(animator, &self.visual.cast()?, duration, delay)?;
            }
        }
        self.state = state;
        self.policy = Some(animator.policy());
        Ok(())
    }

    fn animate_fill(&self, animator: &Animator, fraction: f32) -> Result<()> {
        animator.animate_v2(
            &self.fill.geometry.cast()?,
            h!("Size"),
            Vector2::new(self.width * fraction, Self::HEIGHT),
            Tween::new(700),
            |size| self.fill.geometry.SetSize(size),
        )
    }

    fn indeterminate(&self, animator: &Animator) -> Result<()> {
        let segment = self.width / 4.0;
        let travel = self.width - segment;
        self.fill
            .geometry
            .SetSize(Vector2::new(segment, Self::HEIGHT))?;
        if !animator.policy().allows_motion() {
            return self.fill.visual.SetOffset(xy(travel / 2.0, 0.0));
        }
        let compositor = animator.compositor();
        let animation = compositor.CreateVector3KeyFrameAnimation()?;
        let linear = compositor.CreateLinearEasingFunction()?;
        animation.InsertKeyFrame(0.0, Vector3::default())?;
        animation.InsertKeyFrameWithEasingFunction(0.5, xy(travel, 0.0), &linear)?;
        animation.InsertKeyFrameWithEasingFunction(1.0, Vector3::default(), &linear)?;
        animation.SetDuration(milliseconds(3000))?;
        animation.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
        self.fill.visual.StartAnimation(h!("Offset"), &animation)
    }
}

fn fraction(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Feedback centered on the supplied horizontal anchor. The top coordinate
/// stays fixed. Showing new feedback replaces the previous message.
pub struct Toast {
    visual: ContainerVisual,
    current: Option<ContainerVisual>,
    anchor: Vector2,
    max_width: f32,
    theme: Theme,
}

impl Toast {
    pub const HEIGHT: f32 = 34.0;
    pub const DISPLAY_MS: u32 = 1800;

    pub fn new(scene: &PopupScene, anchor: Vector2, max_width: f32, theme: &Theme) -> Result<Self> {
        Rect::new(anchor.X, anchor.Y, max_width, Self::HEIGHT).validate()?;
        Ok(Self {
            visual: scene.compositor().CreateContainerVisual()?,
            current: None,
            anchor,
            max_width,
            theme: *theme,
        })
    }

    pub fn visual(&self) -> &ContainerVisual {
        &self.visual
    }

    pub fn show(
        &mut self,
        scene: &PopupScene,
        animator: &Animator,
        message: &str,
        glyph: Option<char>,
    ) -> Result<()> {
        let icon = glyph
            .map(|glyph| scene.icon(glyph, 16.0, self.theme.accent))
            .transpose()?;
        let start = 14.0;
        let text_x = icon
            .as_ref()
            .map_or(start, |icon| start + icon.size.X + 6.0);
        let label = text(
            scene,
            message,
            Font::Semibold,
            13.0,
            self.max_width - text_x - start,
            self.theme.accent,
        )?;
        let width = (text_x + label.size.X + start).min(self.max_width);
        let card = holder(
            scene,
            Rect::new(
                self.anchor.X - width / 2.0,
                self.anchor.Y,
                width,
                Self::HEIGHT,
            ),
        )?;
        card.SetCenterPoint(xy(width / 2.0, Self::HEIGHT / 2.0))?;
        let background = rounded_rectangle(
            scene.compositor(),
            Vector2::new(width, Self::HEIGHT),
            17.0,
            self.theme.tint,
        )?;
        let children = card.Children()?;
        children.InsertAtBottom(&background.visual)?;
        label
            .visual
            .SetOffset(xy(text_x, (Self::HEIGHT - label.size.Y) / 2.0))?;
        children.InsertAtTop(&label.visual)?;
        if let Some(icon) = icon {
            center(&icon, Rect::new(start, 0.0, icon.size.X, Self::HEIGHT))?;
            children.InsertAtTop(&icon.visual)?;
        }
        if animator.policy().allows_motion() {
            card.SetOpacity(0.0)?;
            card.SetScale(Vector3::new(0.8, 0.8, 1.0))?;
            animator.spring_scale(&card, 1.0, Spring::new(55, 0.55))?;
            let fade = animator.compositor().CreateScalarKeyFrameAnimation()?;
            fade.InsertKeyFrame(0.0, 0.0)?;
            fade.InsertKeyFrame(0.1, 1.0)?;
            fade.InsertKeyFrame(0.82, 1.0)?;
            fade.InsertKeyFrameWithEasingFunction(1.0, 0.0, animator.easing())?;
            fade.SetDuration(milliseconds(Self::DISPLAY_MS))?;
            card.StartAnimation(h!("Opacity"), &fade)?;
        } else {
            finish_feedback(animator, &card.cast()?, 1, Self::DISPLAY_MS)?;
        }
        self.dismiss()?;
        self.visual.Children()?.InsertAtTop(&card)?;
        self.current = Some(card);
        Ok(())
    }

    pub fn dismiss(&mut self) -> Result<()> {
        if let Some(card) = &self.current {
            self.visual.Children()?.Remove(card)?;
            self.current = None;
        }
        Ok(())
    }
}

fn finish_feedback(
    animator: &Animator,
    visual: &Visual,
    duration_ms: u32,
    delay_ms: u32,
) -> Result<()> {
    let animation = animator.compositor().CreateScalarKeyFrameAnimation()?;
    animation.InsertKeyFrame(0.0, 1.0)?;
    animation.InsertKeyFrameWithEasingFunction(1.0, 0.0, animator.easing())?;
    animation.SetDuration(milliseconds(duration_ms))?;
    animation.SetDelayTime(milliseconds(delay_ms))?;
    visual.StartAnimation(h!("Opacity"), &animation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pill_layout_uses_measured_width_with_a_fixed_right_edge() {
        let anchor = Vector2::new(350.0, 20.0);
        assert_eq!(
            pill_bounds(anchor, 35.0, 160.0),
            Rect::new(278.0, 20.0, 72.0, 28.0)
        );
        let long = pill_bounds(anchor, 900.0, 160.0);
        assert_eq!(long.width, 160.0);
        assert_eq!(long.right(), anchor.X);
    }

    #[test]
    fn progress_input_is_bounded() {
        assert_eq!(fraction(-0.5), 0.0);
        assert_eq!(fraction(0.4), 0.4);
        assert_eq!(fraction(1.2), 1.0);
        assert_eq!(fraction(f32::NAN), 0.0);
    }
}
