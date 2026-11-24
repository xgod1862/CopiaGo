//! Reusable popup visuals. Rectangles and hit-test coordinates use DIPs
//! in the same coordinate space as the component's parent.

mod input;
mod item;
mod scroll;
mod status;
mod theme;

pub use input::{Button, ButtonStyle, Interaction, Toggle};
pub use item::{ItemAction, ItemActions, ItemContent, ItemKind, ItemLayout, ListItem, Removal};
pub use scroll::{ItemHit, ScrollLayout, ScrollView};
pub use status::{ProgressBar, ProgressState, StatusPill, Toast};
pub use theme::{LIGHT_THEME, Status, StatusColors, Theme};

use std::time::Instant;

use crate::composition::{Font, PopupScene, TextStyle, TextSurface};
use windows::{
    UI::{Color, Composition::ContainerVisual},
    Win32::Foundation::E_INVALIDARG,
    core::{Error, Result},
};
use windows_numerics::{Vector2, Vector3};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn size(self) -> Vector2 {
        Vector2::new(self.width, self.height)
    }

    pub fn origin(self) -> Vector2 {
        Vector2::new(self.x, self.y)
    }

    pub fn right(self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(self) -> f32 {
        self.y + self.height
    }

    /// Top and left edges are included. Bottom and right edges are excluded.
    pub fn contains(self, point: Vector2) -> bool {
        self.x <= point.X && point.X < self.right() && self.y <= point.Y && point.Y < self.bottom()
    }

    pub fn contains_rect(self, rect: Self) -> bool {
        self.x <= rect.x
            && rect.right() <= self.right()
            && self.y <= rect.y
            && rect.bottom() <= self.bottom()
    }

    pub fn intersect(self, other: Self) -> Option<Self> {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let width = self.right().min(other.right()) - left;
        let height = self.bottom().min(other.bottom()) - top;
        (width > 0.0 && height > 0.0).then_some(Self::new(left, top, width, height))
    }

    pub fn translated(self, delta: Vector2) -> Self {
        Self::new(self.x + delta.X, self.y + delta.Y, self.width, self.height)
    }

    fn validate(self) -> Result<()> {
        if [self.x, self.y, self.right(), self.bottom()]
            .into_iter()
            .all(f32::is_finite)
            && self.width > 0.0
            && self.height > 0.0
        {
            Ok(())
        } else {
            Err(Error::new(E_INVALIDARG, "Invalid component bounds"))
        }
    }
}

fn xy(x: f32, y: f32) -> Vector3 {
    Vector3::new(x, y, 0.0)
}

fn at(point: Vector2) -> Vector3 {
    xy(point.X, point.Y)
}

fn holder(scene: &PopupScene, bounds: Rect) -> Result<ContainerVisual> {
    bounds.validate()?;
    let visual = scene.compositor().CreateContainerVisual()?;
    visual.SetSize(bounds.size())?;
    visual.SetOffset(at(bounds.origin()))?;
    Ok(visual)
}

fn text(
    scene: &PopupScene,
    value: &str,
    font: Font,
    size: f32,
    max_width: f32,
    color: Color,
) -> Result<TextSurface> {
    scene.text(
        value,
        TextStyle {
            font,
            size,
            max_width: max_width.max(1.0),
            color,
        },
    )
}

fn center(surface: &TextSurface, bounds: Rect) -> Result<()> {
    surface.visual.SetOffset(xy(
        bounds.x + (bounds.width - surface.size.X) / 2.0,
        bounds.y + (bounds.height - surface.size.Y) / 2.0,
    ))
}

/// Sample an Offset animation for hit testing while the compositor renders it.
struct OffsetTransition {
    from: Vector2,
    target: Vector2,
    started: Instant,
    duration_ms: u32,
}

impl OffsetTransition {
    fn new(from: Vector2, target: Vector2, started: Instant, duration_ms: u32) -> Self {
        Self {
            from,
            target,
            started,
            duration_ms,
        }
    }

    fn stationary(offset: Vector2, now: Instant) -> Self {
        Self::new(offset, offset, now, 0)
    }

    fn target(&self) -> Vector2 {
        self.target
    }

    fn at(&self, now: Instant) -> Vector2 {
        if self.duration_ms == 0 {
            return self.target;
        }
        let time = now.saturating_duration_since(self.started).as_secs_f32() * 1000.0
            / self.duration_ms as f32;
        if time >= 1.0 {
            return self.target;
        }
        if time <= 0.0 {
            return self.from;
        }
        let mix = easing_factor(time);
        Vector2::new(
            self.from.X + (self.target.X - self.from.X) * mix,
            self.from.Y + (self.target.Y - self.from.Y) * mix,
        )
    }
}

fn easing_factor(progress: f32) -> f32 {
    fn component(t: f32, first: f32, second: f32) -> f32 {
        let remaining = 1.0 - t;
        3.0 * t * remaining * (remaining * first + t * second) + t * t * t
    }

    // Invert the standard curve's x coordinate, then evaluate its y component.
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..14 {
        let t = (low + high) / 2.0;
        if component(t, 0.1, 0.2) < progress {
            low = t;
        } else {
            high = t;
        }
    }
    component((low + high) / 2.0, 0.9, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_and_hit_at_half_open_edges() {
        let viewport = Rect::new(10.0, 20.0, 200.0, 70.0);
        let row = Rect::new(12.0, 70.0, 190.0, 60.0);
        let visible = viewport.intersect(row).unwrap();
        assert_eq!(visible, Rect::new(12.0, 70.0, 190.0, 20.0));
        assert!(visible.contains(Vector2::new(12.0, 70.0)));
        assert!(!visible.contains(Vector2::new(202.0, 70.0)));
        assert!(!visible.contains(Vector2::new(12.0, 90.0)));
        assert!(!viewport.contains_rect(row));
        assert!(viewport.contains_rect(visible));
        assert!(
            viewport
                .intersect(Rect::new(10.0, 90.0, 5.0, 5.0))
                .is_none()
        );
    }
}
