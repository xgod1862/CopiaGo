//! Shared page and tab indicator transitions for the popup.

use windows::{
    UI::Composition::{ContainerVisual, Visual},
    core::{Interface, Result, h},
};
use windows_numerics::Vector3;

use crate::animation::{Animator, Tween};

const PAGE_MOTION: Tween = Tween::new(300);

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
    let offset = sign * parent.Size()?.X * 0.22;
    incoming.SetOffset(Vector3::new(offset, 0.0, 0.0))?;
    incoming.SetOpacity(0.0)?;
    children.InsertAtTop(incoming)?;
    animator.offset(incoming, Vector3::default(), PAGE_MOTION)?;
    animator.opacity(incoming, 1.0, PAGE_MOTION)?;
    animator.detach_after(parent, outgoing, || {
        animator.offset(outgoing, Vector3::new(-offset, 0.0, 0.0), PAGE_MOTION)?;
        animator.opacity(outgoing, 0.0, PAGE_MOTION)
    })
}

/// Move the active-tab marker in its parent's DIP coordinates.
pub fn slide_indicator(animator: &Animator, indicator: &impl Interface, x: f32) -> Result<()> {
    let visual: Visual = indicator.cast()?;
    animator.animate_scalar(&visual.cast()?, h!("Offset.X"), x, PAGE_MOTION, |x| {
        visual.SetOffset(Vector3 {
            X: x,
            ..visual.Offset()?
        })
    })
}
