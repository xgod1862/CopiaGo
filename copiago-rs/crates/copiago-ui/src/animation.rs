//! Shared Visual Layer transitions. Durations, periods, and delays are in milliseconds.

use std::ffi::c_void;

use windows::{
    Foundation::{IReference, PropertyValue, TimeSpan},
    UI::{
        Color,
        Composition::{
            AnimationStopBehavior, CompositionBatchCompletedEventArgs, CompositionBatchTypes,
            CompositionColorBrush, CompositionColorSpace, CompositionObject, Compositor,
            ContainerVisual, CubicBezierEasingFunction, KeyFrameAnimation, Visual,
        },
    },
    Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW},
    core::{BOOL, HSTRING, IInspectable, Interface, Result, h},
};
use windows_numerics::{Vector2, Vector3};

/// A snapshot of the animation preference. Re-read it when handling composition::POPUP_SETTINGS_CHANGED.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MotionPolicy {
    Full,
    Reduced,
}

impl MotionPolicy {
    pub const fn from_client_area_animations(enabled: bool) -> Self {
        if enabled { Self::Full } else { Self::Reduced }
    }

    /// Read the Windows preference. Fall back to reduced motion if the query fails.
    pub fn system() -> Self {
        let mut enabled = BOOL(0);
        // SAFETY: SPI_GETCLIENTAREAANIMATION writes a BOOL to this valid, writable slot.
        let result = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                Some(std::ptr::from_mut(&mut enabled).cast::<c_void>()),
                Default::default(),
            )
        };
        Self::from_client_area_animations(result.is_ok() && enabled.as_bool())
    }

    pub const fn allows_motion(self) -> bool {
        matches!(self, Self::Full)
    }

    pub const fn duration_ms(self, duration_ms: u32) -> u32 {
        if self.allows_motion() { duration_ms } else { 0 }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Tween {
    pub duration_ms: u32,
    pub delay_ms: u32,
}

impl Tween {
    pub const fn new(duration_ms: u32) -> Self {
        Self::delayed(duration_ms, 0)
    }

    pub const fn delayed(duration_ms: u32, delay_ms: u32) -> Self {
        Self {
            duration_ms,
            delay_ms,
        }
    }
}

/// A component of a Composition vector property, such as Offset.Y.
#[derive(Clone, Copy, Debug)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    fn name(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub period_ms: u32,
    pub damping: f32,
}

impl Spring {
    pub const fn new(period_ms: u32, damping: f32) -> Self {
        Self { period_ms, damping }
    }
}

/// One animator per visible scene. Keep it on the Composition thread.
pub struct Animator {
    compositor: Compositor,
    easing: CubicBezierEasingFunction,
    policy: MotionPolicy,
}

impl Animator {
    pub fn new(compositor: &Compositor, policy: MotionPolicy) -> Result<Self> {
        Ok(Self {
            compositor: compositor.clone(),
            easing: compositor
                .CreateCubicBezierEasingFunction(Vector2::new(0.1, 0.9), Vector2::new(0.2, 1.0))?,
            policy,
        })
    }

    pub fn compositor(&self) -> &Compositor {
        &self.compositor
    }

    pub fn easing(&self) -> &CubicBezierEasingFunction {
        &self.easing
    }

    pub fn policy(&self) -> MotionPolicy {
        self.policy
    }

    /// Update before dispatching new transitions. Owners of repeating effects
    /// should reapply their state to stop or restart those loops.
    pub fn set_policy(&mut self, policy: MotionPolicy) {
        self.policy = policy;
    }

    /// The setter applies the endpoint directly when motion is disabled or duration is zero.
    pub fn animate_scalar(
        &self,
        target: &CompositionObject,
        property: &HSTRING,
        value: f32,
        timing: Tween,
        set: impl FnOnce(f32) -> Result<()>,
    ) -> Result<()> {
        if self.snap(timing) {
            target.StopAnimation(property)?;
            return set(value);
        }
        let animation = self.compositor.CreateScalarKeyFrameAnimation()?;
        animation.InsertKeyFrameWithEasingFunction(1.0, value, &self.easing)?;
        configure(&animation, timing)?;
        target.StartAnimation(property, &animation)
    }

    pub fn animate_color(
        &self,
        brush: &CompositionColorBrush,
        value: Color,
        timing: Tween,
    ) -> Result<()> {
        if self.snap(timing) {
            brush.StopAnimation(h!("Color"))?;
            return brush.SetColor(value);
        }
        let animation = self.compositor.CreateColorKeyFrameAnimation()?;
        animation.SetInterpolationColorSpace(CompositionColorSpace::Rgb)?;
        animation.InsertKeyFrameWithEasingFunction(1.0, value, &self.easing)?;
        configure(&animation, timing)?;
        brush.StartAnimation(h!("Color"), &animation)
    }

    /// The setter applies the endpoint directly when motion is disabled or duration is zero.
    pub fn animate_v2(
        &self,
        target: &CompositionObject,
        property: &HSTRING,
        value: Vector2,
        timing: Tween,
        set: impl FnOnce(Vector2) -> Result<()>,
    ) -> Result<()> {
        if self.snap(timing) {
            target.StopAnimation(property)?;
            return set(value);
        }
        let animation = self.compositor.CreateVector2KeyFrameAnimation()?;
        animation.InsertKeyFrameWithEasingFunction(1.0, value, &self.easing)?;
        configure(&animation, timing)?;
        target.StartAnimation(property, &animation)
    }

    /// The setter applies the endpoint directly when motion is disabled or duration is zero.
    pub fn animate_v3(
        &self,
        target: &CompositionObject,
        property: &HSTRING,
        value: Vector3,
        timing: Tween,
        set: impl FnOnce(Vector3) -> Result<()>,
    ) -> Result<()> {
        if self.snap(timing) {
            target.StopAnimation(property)?;
            return set(value);
        }
        let animation = self.compositor.CreateVector3KeyFrameAnimation()?;
        animation.InsertKeyFrameWithEasingFunction(1.0, value, &self.easing)?;
        configure(&animation, timing)?;
        target.StartAnimation(property, &animation)
    }

    /// Use one scalar spring per component, which can be boxed by windows-rs.
    /// The setter applies all endpoints when motion is disabled.
    pub fn spring(
        &self,
        target: &CompositionObject,
        property: &str,
        components: &[(Axis, f32)],
        spring: Spring,
        set: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let animated = self.policy.allows_motion() && spring.period_ms != 0;
        for &(axis, value) in components {
            let property = HSTRING::from(format!("{property}.{}", axis.name()));
            if animated {
                let animation = self.compositor.CreateSpringScalarAnimation()?;
                let value: IReference<f32> = PropertyValue::CreateSingle(value)?.cast()?;
                animation.SetFinalValue(&value)?;
                animation.SetPeriod(milliseconds(spring.period_ms))?;
                animation.SetDampingRatio(spring.damping)?;
                target.StartAnimation(&property, &animation)?;
            } else {
                target.StopAnimation(&property)?;
            }
        }
        if animated { Ok(()) } else { set() }
    }

    /// Install the implicit Offset transition on a dedicated positioning
    /// visual. This replaces the visual's implicit animation collection.
    pub fn implicit_offset(&self, visual: &impl Interface, duration_ms: u32) -> Result<()> {
        let visual: Visual = visual.cast()?;
        let collection = self.compositor.CreateImplicitAnimationCollection()?;
        if self.policy.duration_ms(duration_ms) != 0 {
            let animation = self.compositor.CreateVector3KeyFrameAnimation()?;
            animation.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
            animation.InsertExpressionKeyFrameWithEasingFunction(
                1.0,
                h!("this.FinalValue"),
                &self.easing,
            )?;
            configure(&animation, Tween::new(duration_ms))?;
            animation.SetTarget(h!("Offset"))?;
            collection.Insert(h!("Offset"), &animation)?;
        } else {
            visual.StopAnimation(h!("Offset"))?;
        }
        visual.SetImplicitAnimations(&collection)
    }

    pub fn opacity(&self, visual: &impl Interface, value: f32, timing: Tween) -> Result<()> {
        let visual: Visual = visual.cast()?;
        self.animate_scalar(&visual.cast()?, h!("Opacity"), value, timing, |value| {
            visual.SetOpacity(value)
        })
    }

    pub fn offset(&self, visual: &impl Interface, value: Vector3, timing: Tween) -> Result<()> {
        let visual: Visual = visual.cast()?;
        self.animate_v3(&visual.cast()?, h!("Offset"), value, timing, |value| {
            visual.SetOffset(value)
        })
    }

    /// Apply uniform XY scale while leaving Z unchanged.
    pub fn spring_scale(&self, visual: &impl Interface, scale: f32, spring: Spring) -> Result<()> {
        let visual: Visual = visual.cast()?;
        self.spring(
            &visual.cast()?,
            "Scale",
            &[(Axis::X, scale), (Axis::Y, scale)],
            spring,
            || visual.SetScale(Vector3::new(scale, scale, visual.Scale()?.Z)),
        )
    }

    /// Run a finite transition, then detach its visual from the same collection.
    pub(crate) fn detach_after(
        &self,
        parent: &ContainerVisual,
        visual: &impl Interface,
        transition: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let visual: Visual = visual.cast()?;
        let children = parent.Children()?;
        if !self.policy.allows_motion() {
            return children.Remove(&visual);
        }
        let batch = self
            .compositor
            .CreateScopedBatch(CompositionBatchTypes::Animation)?;
        let completed = windows::Foundation::TypedEventHandler::<
            IInspectable,
            CompositionBatchCompletedEventArgs,
        >::new(move |_, _| {
            // Closing the scene can invalidate the collection before this callback is dispatched.
            let _ = children.Remove(&visual);
            Ok(())
        });
        let result = batch.Completed(&completed).and_then(|_| transition());
        // End even when installing an animation fails.
        let ended = batch.End();
        result.and(ended)
    }

    fn snap(&self, timing: Tween) -> bool {
        self.policy.duration_ms(timing.duration_ms) == 0
    }
}

pub(crate) const fn milliseconds(value: u32) -> TimeSpan {
    TimeSpan {
        Duration: value as i64 * 10_000,
    }
}

fn configure(animation: &impl Interface, timing: Tween) -> Result<()> {
    let animation: KeyFrameAnimation = animation.cast()?;
    animation.SetDuration(milliseconds(timing.duration_ms))?;
    animation.SetStopBehavior(AnimationStopBehavior::LeaveCurrentValue)?;
    if timing.delay_ms != 0 {
        animation.SetDelayTime(milliseconds(timing.delay_ms))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_motion_clears_animation_duration() {
        let enabled = MotionPolicy::from_client_area_animations(true);
        let disabled = MotionPolicy::from_client_area_animations(false);
        assert!(enabled.allows_motion());
        assert_eq!(enabled.duration_ms(420), 420);
        assert!(!disabled.allows_motion());
        assert_eq!(disabled.duration_ms(420), 0);
    }
}
