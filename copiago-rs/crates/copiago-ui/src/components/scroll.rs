use std::time::Instant;

use crate::{
    animation::{Animator, Tween},
    composition::{PopupScene, RoundedRectangle, rounded_rectangle},
};

use super::{ItemAction, ItemLayout, ListItem, OffsetTransition, Rect, Theme, holder, xy};
use windows::{
    UI::Composition::ContainerVisual,
    Win32::Foundation::E_INVALIDARG,
    core::{Error, Result},
};
use windows_numerics::Vector2;

const WHEEL_DIPS: f32 = 56.0;
const WHEEL_DELTA: f32 = 120.0;
const SETTLE_MS: u32 = 240;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemHit {
    Body,
    Action(ItemAction),
}

/// Scroll geometry in the parent's DIP coordinates. Child rectangles use
/// the viewport's unscrolled top-left as their origin.
#[derive(Clone, Copy, Debug)]
pub struct ScrollLayout {
    viewport: Rect,
    content_height: f32,
}

impl ScrollLayout {
    pub fn new(viewport: Rect, content_height: f32) -> Result<Self> {
        viewport.validate()?;
        if !content_height.is_finite() || content_height < 0.0 {
            return Err(Error::new(E_INVALIDARG, "Invalid scroll content height"));
        }
        Ok(Self {
            viewport,
            content_height,
        })
    }

    pub fn viewport(self) -> Rect {
        self.viewport
    }

    pub fn limit(self) -> f32 {
        (self.content_height - self.viewport.height).max(0.0)
    }

    pub fn clamp(self, offset: f32) -> f32 {
        if offset.is_finite() {
            offset.clamp(0.0, self.limit())
        } else {
            0.0
        }
    }

    pub fn wheel_dips(delta: i16) -> f32 {
        -f32::from(delta) * WHEEL_DIPS / WHEEL_DELTA
    }

    pub fn thumb(self, offset: f32) -> Option<Rect> {
        let limit = self.limit();
        if limit == 0.0 {
            return None;
        }
        let height = (self.viewport.height * (self.viewport.height / self.content_height))
            .floor()
            .max(28.0)
            .min(self.viewport.height);
        let top = self.viewport.y + self.clamp(offset) / limit * (self.viewport.height - height);
        Some(Rect::new(self.viewport.right() - 8.0, top, 4.0, height))
    }

    pub fn content_point(self, point: Vector2, offset: f32) -> Option<Vector2> {
        self.viewport.contains(point).then(|| {
            Vector2::new(
                point.X - self.viewport.x,
                point.Y - self.viewport.y + self.clamp(offset),
            )
        })
    }

    pub fn visible_bounds(self, child: Rect, offset: f32) -> Option<Rect> {
        self.viewport.intersect(self.place_child(child, offset))
    }

    pub fn fully_visible(self, child: Rect, offset: f32) -> bool {
        self.viewport.contains_rect(self.place_child(child, offset))
    }

    /// The visible part of a row can be hit. A row action needs its entire button visible.
    pub fn hit_item(
        self,
        point: Vector2,
        offset: f32,
        item: ItemLayout,
        origin: Vector2,
    ) -> Option<ItemHit> {
        let content = self.content_point(point, offset)?;
        let point = Vector2::new(content.X - origin.X, content.Y - origin.Y);
        if !item.bounds().contains(point) {
            return None;
        }
        if let Some(action) = item.hit_action(point)
            && let Some(bounds) = item.action_bounds(action)
            && self.fully_visible(bounds.translated(origin), offset)
        {
            return Some(ItemHit::Action(action));
        }
        Some(ItemHit::Body)
    }

    fn place_child(self, child: Rect, offset: f32) -> Rect {
        child.translated(Vector2::new(
            self.viewport.x,
            self.viewport.y - self.clamp(offset),
        ))
    }

    /// Offset that brings an item into view for keyboard or accessibility navigation.
    pub fn reveal(self, child: Rect, offset: f32) -> f32 {
        let offset = self.clamp(offset);
        self.clamp(if child.y < offset || child.height > self.viewport.height {
            child.y
        } else if child.bottom() > offset + self.viewport.height {
            child.bottom() - self.viewport.height
        } else {
            offset
        })
    }
}

/// Clipped content with an optional thumb. Wheel/trackpad messages accumulate
/// into a bounded destination while the compositor eases to the target.
pub struct ScrollView {
    visual: ContainerVisual,
    content: ContainerVisual,
    thumb: RoundedRectangle,
    layout: ScrollLayout,
    position: OffsetTransition,
}

impl ScrollView {
    pub fn new(scene: &PopupScene, layout: ScrollLayout, theme: &Theme) -> Result<Self> {
        let visual = holder(scene, layout.viewport)?;
        let clip = scene.compositor().CreateInsetClip()?;
        visual.SetClip(&clip)?;
        let content = scene.compositor().CreateContainerVisual()?;
        let thumb = rounded_rectangle(
            scene.compositor(),
            Vector2::new(4.0, layout.viewport.height),
            2.0,
            theme.scrollbar,
        )?;
        let children = visual.Children()?;
        children.InsertAtBottom(&content)?;
        children.InsertAtTop(&thumb.visual)?;
        let view = Self {
            visual,
            content,
            thumb,
            layout,
            position: OffsetTransition::stationary(Vector2::new(0.0, 0.0), Instant::now()),
        };
        view.place_thumb(0.0)?;
        Ok(view)
    }

    pub fn visual(&self) -> &ContainerVisual {
        &self.visual
    }

    pub fn content(&self) -> &ContainerVisual {
        &self.content
    }

    pub fn layout(&self) -> ScrollLayout {
        self.layout
    }

    /// Current offset for input, sampled using the compositor's easing curve.
    pub fn offset_at(&self, now: Instant) -> f32 {
        self.position.at(now).Y
    }

    pub fn target_offset(&self) -> f32 {
        self.position.target().Y
    }

    pub fn content_point(&self, point: Vector2, now: Instant) -> Option<Vector2> {
        self.layout.content_point(point, self.offset_at(now))
    }

    pub fn hit_item(&self, point: Vector2, item: &ListItem, now: Instant) -> Option<ItemHit> {
        self.layout.hit_item(
            point,
            self.offset_at(now),
            item.layout(),
            item.bounds_at(now).origin(),
        )
    }

    pub fn set_content_height(&mut self, content_height: f32, now: Instant) -> Result<()> {
        if content_height == self.layout.content_height {
            return Ok(());
        }
        self.layout = ScrollLayout::new(self.layout.viewport, content_height)?;
        self.jump_to(self.target_offset(), now)
    }

    pub fn scroll_wheel(&mut self, animator: &Animator, delta: i16, now: Instant) -> Result<()> {
        self.scroll_by(animator, ScrollLayout::wheel_dips(delta), now)
    }

    pub fn scroll_by(&mut self, animator: &Animator, dips: f32, now: Instant) -> Result<()> {
        self.scroll_to(animator, self.target_offset() + dips, now)
    }

    pub fn scroll_to(&mut self, animator: &Animator, offset: f32, now: Instant) -> Result<()> {
        let target = self.layout.clamp(offset);
        let duration = animator.policy().duration_ms(SETTLE_MS);
        if target == self.target_offset() && duration != 0 {
            return Ok(());
        }
        let current = self.offset_at(now);
        self.content.SetOffset(xy(0.0, -current))?;
        self.place_thumb(current)?;
        let timing = Tween::new(duration);
        animator.offset(&self.content, xy(0.0, -target), timing)?;
        if let Some(bounds) = self.layout.thumb(target) {
            animator.offset(&self.thumb.visual, self.thumb_origin(bounds), timing)?;
        }
        self.position = OffsetTransition::new(
            Vector2::new(0.0, current),
            Vector2::new(0.0, target),
            now,
            duration,
        );
        Ok(())
    }

    pub fn reveal(&mut self, animator: &Animator, child: Rect, now: Instant) -> Result<()> {
        self.scroll_to(
            animator,
            self.layout.reveal(child, self.target_offset()),
            now,
        )
    }

    pub(crate) fn jump_to(&mut self, offset: f32, now: Instant) -> Result<()> {
        use windows::core::h;

        let offset = self.layout.clamp(offset);
        self.content.StopAnimation(h!("Offset"))?;
        self.thumb.visual.StopAnimation(h!("Offset"))?;
        self.position = OffsetTransition::stationary(Vector2::new(0.0, offset), now);
        self.content.SetOffset(xy(0.0, -offset))?;
        self.place_thumb(offset)
    }

    fn thumb_origin(&self, bounds: Rect) -> windows_numerics::Vector3 {
        xy(
            bounds.x - self.layout.viewport.x,
            bounds.y - self.layout.viewport.y,
        )
    }

    fn place_thumb(&self, offset: f32) -> Result<()> {
        if let Some(bounds) = self.layout.thumb(offset) {
            self.thumb.visual.SetIsVisible(true)?;
            self.thumb.geometry.SetSize(bounds.size())?;
            self.thumb.visual.SetOffset(self.thumb_origin(bounds))
        } else {
            self.thumb.visual.SetIsVisible(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_item_actions_use_the_sampled_offset_and_full_clip() {
        use super::super::{ItemActions, ItemKind};
        use std::time::Duration;
        let layout = ScrollLayout::new(Rect::new(0.0, 100.0, 392.0, 100.0), 220.0).unwrap();
        let row = ItemLayout::new(356.0, 60.0, ItemKind::Text, ItemActions::CopyDelete);
        let start = Instant::now();
        let movement = OffsetTransition::new(
            Vector2::new(18.0, 0.0),
            Vector2::new(18.0, 68.0),
            start,
            420,
        );
        let button = Vector2::new(345.0, 122.0);
        assert_eq!(
            layout.hit_item(button, 0.0, row, movement.at(start)),
            Some(ItemHit::Action(ItemAction::Copy))
        );
        let mid = movement.at(start + Duration::from_millis(210));
        assert!(mid.Y > 51.0 && mid.Y < 68.0);
        assert_eq!(layout.hit_item(button, 0.0, row, mid), None);
        assert_eq!(
            layout.hit_item(Vector2::new(345.0, 187.0), 0.0, row, movement.target()),
            Some(ItemHit::Body),
        );
        assert_eq!(
            layout.hit_item(Vector2::new(345.0, 157.0), 30.0, row, movement.target()),
            Some(ItemHit::Action(ItemAction::Copy)),
        );
        assert_eq!(
            layout.hit_item(Vector2::new(345.0, 200.0), 0.0, row, movement.target()),
            None
        );
    }

    #[test]
    fn wheel_keeps_sub_notch_deltas_and_is_clamped_to_content() {
        let layout = ScrollLayout::new(Rect::new(18.0, 90.0, 356.0, 310.0), 578.0).unwrap();
        assert_eq!(ScrollLayout::wheel_dips(120), -56.0);
        assert_eq!(ScrollLayout::wheel_dips(-30), 14.0);
        assert_eq!(layout.clamp(-16.0), 0.0);
        assert_eq!(layout.clamp(437.0), 268.0);
        assert_eq!(layout.thumb(0.0).unwrap().y, 90.0);
        assert_eq!(layout.thumb(268.0).unwrap().bottom(), 400.0);
    }

    #[test]
    fn hit_testing_and_reveal_follow_viewport_clip() {
        let layout = ScrollLayout::new(Rect::new(18.0, 90.0, 356.0, 310.0), 600.0).unwrap();
        let row = Rect::new(0.0, 300.0, 348.0, 60.0);
        assert_eq!(
            layout.content_point(Vector2::new(18.0, 396.0), 10.0),
            Some(Vector2::new(0.0, 316.0))
        );
        assert!(
            layout
                .content_point(Vector2::new(18.0, 400.0), 10.0)
                .is_none()
        );
        assert_eq!(
            layout.visible_bounds(row, 10.0),
            Some(Rect::new(18.0, 380.0, 348.0, 20.0))
        );
        assert!(!layout.fully_visible(row, 10.0));
        let offset = layout.reveal(row, 10.0);
        assert_eq!(offset, 50.0);
        assert!(layout.fully_visible(row, offset));
        let short = ScrollLayout::new(layout.viewport(), 33.0).unwrap();
        assert!(short.thumb(0.0).is_none());
        assert_eq!(short.reveal(row, 0.0), 0.0);
    }
}
