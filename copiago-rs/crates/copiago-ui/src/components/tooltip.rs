//! Hover hints for icon-only buttons.

use windows::{
    UI::Composition::ContainerVisual,
    core::{Result, h},
};
use windows_numerics::Vector3;

use super::{Rect, Theme, holder, text};
use crate::{
    animation::{Animator, Tween},
    composition::{Font, PopupScene, rounded_rectangle},
};

const PAD_X: f32 = 8.0;
const HEIGHT: f32 = 24.0;
const GAP: f32 = 6.0;
const EDGE: f32 = 8.0;
const RISE: f32 = 4.0;
// Appear after the pointer rests, like system tooltips, but fade in softly.
const SHOW: Tween = Tween::delayed(140, 450).enter();
const HIDE: Tween = Tween::new(90).exit();

struct Tip {
    visual: ContainerVisual,
    width: f32,
}

/// Pre-rendered labels, one per key, drawn above every other layer of a view.
pub struct Tooltip {
    visual: ContainerVisual,
    tips: Vec<Tip>,
    shown: Option<(usize, Rect)>,
}

impl Tooltip {
    /// `area` is the view rectangle the hint must stay inside.
    pub fn new(scene: &PopupScene, area: Rect, labels: &[&str], theme: &Theme) -> Result<Self> {
        let visual = holder(scene, area)?;
        let compositor = scene.compositor();
        let mut tips = Vec::with_capacity(labels.len());
        for label in labels {
            let label = text(scene, label, Font::Regular, 12.0, 240.0, theme.white)?;
            let width = (label.size.X + PAD_X * 2.0).ceil();
            let tip = holder(scene, Rect::new(0.0, 0.0, width, HEIGHT))?;
            let background = rounded_rectangle(
                compositor,
                windows_numerics::Vector2::new(width, HEIGHT),
                6.0,
                theme.ink,
            )?;
            let children = tip.Children()?;
            children.InsertAtBottom(&background.visual)?;
            label.visual.SetOffset(Vector3::new(
                PAD_X,
                ((HEIGHT - label.size.Y) / 2.0).round(),
                0.0,
            ))?;
            children.InsertAtTop(&label.visual)?;
            tip.SetOpacity(0.0)?;
            visual.Children()?.InsertAtTop(&tip)?;
            tips.push(Tip { visual: tip, width });
        }
        Ok(Self {
            visual,
            tips,
            shown: None,
        })
    }

    pub fn visual(&self) -> &ContainerVisual {
        &self.visual
    }

    /// Show the label for `key` next to `anchor`, in the same coordinates as `area`.
    /// Pass None to hide. Repeating the current request keeps the running fade.
    pub fn set(&mut self, animator: &Animator, request: Option<(usize, Rect)>) -> Result<()> {
        if request == self.shown {
            return Ok(());
        }
        if let Some((key, _)) = self.shown.take()
            && let Some(tip) = self.tips.get(key)
        {
            animator.opacity(&tip.visual, 0.0, HIDE)?;
        }
        let Some((key, anchor)) = request else {
            return Ok(());
        };
        let Some(tip) = self.tips.get(key) else {
            return Ok(());
        };
        let area = self.visual.Size()?;
        let x = (anchor.x + (anchor.width - tip.width) / 2.0)
            .min(area.X - EDGE - tip.width)
            .max(EDGE)
            .round();
        let above = anchor.y - GAP - HEIGHT;
        let (y, rise) = if above >= EDGE {
            (above, RISE)
        } else {
            (anchor.bottom() + GAP, -RISE)
        };
        let target = Vector3::new(x, y.round(), 0.0);
        tip.visual.StopAnimation(h!("Offset"))?;
        tip.visual
            .SetOffset(Vector3::new(x, target.Y + rise, 0.0))?;
        tip.visual.SetOpacity(0.0)?;
        animator.offset(&tip.visual, target, SHOW)?;
        animator.opacity(&tip.visual, 1.0, SHOW)?;
        self.shown = request;
        Ok(())
    }
}
