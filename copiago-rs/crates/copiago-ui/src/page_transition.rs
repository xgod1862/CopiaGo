//! Shared page and tab indicator transitions for the popup.

use windows::{
    UI::Composition::{ContainerVisual, Visual},
    core::{Interface, Result, h},
};
use windows_numerics::Vector3;

use crate::animation::{Animator, Tween};

// Shared-axis transition: the old page steps aside and fades first, then the new
// page arrives over a short distance. A full crossfade of two moving pages looks busy.
const PAGE_SHIFT_DIPS: f32 = 28.0;
const PAGE_IN: Tween = Tween::new(320).enter();
const PAGE_FADE_IN: Tween = Tween::delayed(200, 70);
const PAGE_OUT: Tween = Tween::new(140).exit();
const INDICATOR_MOTION: Tween = Tween::new(280);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageDirection {
    Forward,
    Back,
}

/// Replace the outgoing child with a freshly built page. Use a viewport-sized,
/// clipped parent so neither page draws outside the card during the transition.
/// Route input to the incoming page immediately. The outgoing child is detached
/// at completion.
pub fn transition_pages(
    animator: &Animator,
    parent: &ContainerVisual,
    outgoing: &ContainerVisual,
    incoming: &ContainerVisual,
    direction: PageDirection,
) -> Result<()> {
    let children = parent.Children()?;
    if !animator.policy().allows_motion() {
        children.InsertAtTop(incoming)?;
        return children.Remove(outgoing);
    }

    let sign = match direction {
        PageDirection::Forward => 1.0,
        PageDirection::Back => -1.0,
    };
    let offset = sign * PAGE_SHIFT_DIPS;
    incoming.SetOffset(Vector3::new(offset, 0.0, 0.0))?;
    incoming.SetOpacity(0.0)?;
    children.InsertAtTop(incoming)?;
    animator.offset(incoming, Vector3::default(), PAGE_IN)?;
    animator.opacity(incoming, 1.0, PAGE_FADE_IN)?;
    animator.detach_after(parent, outgoing, || {
        animator.offset(outgoing, Vector3::new(-offset * 0.5, 0.0, 0.0), PAGE_OUT)?;
        animator.opacity(outgoing, 0.0, PAGE_OUT)
    })
}

/// Move the active-tab marker in its parent's DIP coordinates.
pub fn slide_indicator(animator: &Animator, indicator: &impl Interface, x: f32) -> Result<()> {
    let visual: Visual = indicator.cast()?;
    animator.animate_scalar(&visual.cast()?, h!("Offset.X"), x, INDICATOR_MOTION, |x| {
        visual.SetOffset(Vector3 {
            X: x,
            ..visual.Offset()?
        })
    })
}

/// Crossfade content that changes in place. The caller publishes the new input model immediately.
pub fn crossfade(
    animator: &Animator,
    parent: &ContainerVisual,
    outgoing: &ContainerVisual,
    incoming: &ContainerVisual,
) -> Result<()> {
    let children = parent.Children()?;
    if !animator.policy().allows_motion() {
        children.InsertAbove(incoming, outgoing)?;
        return children.Remove(outgoing);
    }
    incoming.SetOpacity(0.0)?;
    children.InsertAbove(incoming, outgoing)?;
    animator.opacity(incoming, 1.0, Tween::new(180))?;
    animator.detach_after(parent, outgoing, || {
        animator.opacity(outgoing, 0.0, Tween::new(140))
    })
}
