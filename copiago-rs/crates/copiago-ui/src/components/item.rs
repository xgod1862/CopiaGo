use std::time::Instant;

use crate::{
    animation::{Animator, MotionPolicy, Spring, Tween},
    composition::{Font, PopupScene, RoundedRectangle, rounded_rectangle},
};

use super::{
    Button, ButtonStyle, Interaction, OffsetTransition, Rect, Theme, at, center, holder, text, xy,
};
use windows::{
    UI::{
        Color,
        Composition::{ContainerVisual, Visual},
    },
    Win32::Foundation::E_INVALIDARG,
    core::{Error, Interface, Result, h},
};
use windows_numerics::{Vector2, Vector3};

const REFLOW_MS: u32 = 420;
const EXIT_MS: u32 = 250;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemKind {
    Text,
    Link,
    Image,
    File,
}

impl ItemKind {
    fn tile(self, theme: &Theme) -> (char, Color, Color) {
        match self {
            Self::Text => ('\u{E8C8}', theme.tint, theme.accent),
            Self::Link => ('\u{E71B}', theme.link_tint, theme.link_ink),
            Self::Image => ('\u{EB9F}', theme.tint, theme.accent),
            Self::File => ('\u{E7C3}', theme.file_tint, theme.disconnected.text),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemActions {
    None,
    CopyDelete,
    CopySendDelete,
}

impl ItemActions {
    fn width(self) -> f32 {
        match self {
            Self::None => 0.0,
            Self::CopyDelete => 92.0,
            Self::CopySendDelete => 132.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemAction {
    Copy,
    Send,
    Delete,
}

impl ItemAction {
    const ORDER: [Self; 3] = [Self::Copy, Self::Send, Self::Delete];

    fn icon(self, copied: bool) -> (char, ButtonStyle) {
        match self {
            Self::Copy => (
                if copied { '\u{E73E}' } else { '\u{E8C8}' },
                ButtonStyle::Primary,
            ),
            Self::Send => ('\u{E898}', ButtonStyle::Tinted),
            Self::Delete => ('\u{E74D}', ButtonStyle::Danger),
        }
    }
}

/// Unscrolled row geometry, relative to the row's origin.
#[derive(Clone, Copy, Debug)]
pub struct ItemLayout {
    pub width: f32,
    pub height: f32,
    pub kind: ItemKind,
    pub actions: ItemActions,
}

impl ItemLayout {
    pub const ROW_HEIGHT: f32 = 60.0;
    pub const GAP: f32 = 8.0;

    pub const fn new(width: f32, height: f32, kind: ItemKind, actions: ItemActions) -> Self {
        Self {
            width,
            height,
            kind,
            actions,
        }
    }

    pub fn selected_text_height(lines: usize) -> f32 {
        (46.0 + lines.min(3) as f32 * 20.0).max(Self::ROW_HEIGHT)
    }

    pub fn bounds(self) -> Rect {
        Rect::new(0.0, 0.0, self.width, self.height)
    }

    pub fn tile(self) -> Rect {
        let width = if self.kind == ItemKind::Image {
            56.0
        } else {
            36.0
        };
        Rect::new(12.0, 12.0, width, 36.0)
    }

    pub fn text_left(self) -> f32 {
        if self.kind == ItemKind::Image {
            80.0
        } else {
            60.0
        }
    }

    /// Width for caller-provided wrapped title lines.
    pub fn wrap_width(self) -> f32 {
        (self.width - self.text_left() - self.actions.width()).max(1.0)
    }

    pub fn action_bounds(self, action: ItemAction) -> Option<Rect> {
        let slot = match (self.actions, action) {
            (ItemActions::None, _) | (ItemActions::CopyDelete, ItemAction::Send) => return None,
            (_, ItemAction::Copy) => 0.0,
            (ItemActions::CopyDelete, ItemAction::Delete)
            | (ItemActions::CopySendDelete, ItemAction::Send) => 1.0,
            (ItemActions::CopySendDelete, ItemAction::Delete) => 2.0,
        };
        Some(Rect::new(self.width - 44.0 - 40.0 * slot, 14.0, 32.0, 32.0))
    }

    pub fn hit_action(self, point: Vector2) -> Option<ItemAction> {
        ItemAction::ORDER.into_iter().find(|&action| {
            self.action_bounds(action)
                .is_some_and(|bounds| bounds.contains(point))
        })
    }

    fn text_width(self) -> f32 {
        let controls = if self.actions == ItemActions::None {
            0.0
        } else {
            self.actions.width() - 12.0
        };
        self.width - 14.0 - controls - self.text_left()
    }
}

pub struct ItemContent<'a> {
    /// One line, or up to three lines of already wrapped selected text.
    pub title: &'a [&'a str],
    pub detail: &'a str,
    pub emphasized_detail: bool,
    pub copied: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Removal {
    /// Capacity eviction. Fade in place.
    Fade,
    /// Explicit deletion. Fade and fold the visible height.
    Collapse,
}

/// Data-independent history row. The caller retains IDs and dispatches actions.
pub struct ListItem {
    visual: ContainerVisual,
    entrance: ContainerVisual,
    press: ContainerVisual,
    background: RoundedRectangle,
    image_tile: Option<ContainerVisual>,
    thumbnail: Option<Visual>,
    actions: Vec<(ItemAction, Button)>,
    layout: ItemLayout,
    position: OffsetTransition,
    interaction: Interaction,
    position_policy: Option<MotionPolicy>,
    interaction_policy: Option<MotionPolicy>,
    colors: (Color, Color),
}

impl ListItem {
    pub fn new(
        scene: &PopupScene,
        origin: Vector2,
        layout: ItemLayout,
        content: ItemContent<'_>,
        theme: &Theme,
    ) -> Result<Self> {
        if layout.height < ItemLayout::ROW_HEIGHT || layout.text_width() < 1.0 {
            return Err(Error::new(E_INVALIDARG, "Invalid list item size"));
        }
        let bounds = layout.bounds();
        let visual = holder(scene, bounds.translated(origin))?;
        let selected = layout.actions != ItemActions::None;
        let entrance = holder(scene, bounds)?;
        let press = holder(scene, bounds)?;
        let pivot = xy(layout.width / 2.0, layout.height / 2.0);
        entrance.SetCenterPoint(pivot)?;
        press.SetCenterPoint(pivot)?;
        visual.Children()?.InsertAtTop(&entrance)?;
        entrance.Children()?.InsertAtTop(&press)?;

        let colors = if selected {
            (theme.white, theme.white)
        } else {
            (theme.card, theme.card_hover)
        };
        let compositor = scene.compositor();
        let children = press.Children()?;
        let background = if selected {
            let border = rounded_rectangle(compositor, bounds.size(), 16.0, theme.accent)?;
            children.InsertAtBottom(&border.visual)?;
            let background = rounded_rectangle(
                compositor,
                Vector2::new(layout.width - 3.0, layout.height - 3.0),
                14.5,
                colors.0,
            )?;
            background.visual.SetOffset(xy(1.5, 1.5))?;
            background
        } else {
            rounded_rectangle(compositor, bounds.size(), 16.0, colors.0)?
        };
        children.InsertAtTop(&background.visual)?;

        let (glyph, tile_fill, tile_ink) = layout.kind.tile(theme);
        let tile = holder(scene, layout.tile())?;
        let tile_fill = rounded_rectangle(compositor, layout.tile().size(), 10.0, tile_fill)?;
        let icon = scene.icon(glyph, 18.0, tile_ink)?;
        center(
            &icon,
            Rect::new(0.0, 0.0, layout.tile().width, layout.tile().height),
        )?;
        tile.Children()?.InsertAtBottom(&tile_fill.visual)?;
        tile.Children()?.InsertAtTop(&icon.visual)?;
        children.InsertAtTop(&tile)?;
        let image_tile = if layout.kind == ItemKind::Image {
            let clip = compositor.CreateGeometricClipWithGeometry(&tile_fill.geometry)?;
            tile.SetClip(&clip)?;
            Some(tile)
        } else {
            None
        };

        let expanded =
            selected && layout.kind == ItemKind::Text && layout.height > ItemLayout::ROW_HEIGHT;
        for (index, line) in content
            .title
            .iter()
            .take(if expanded { 3 } else { 1 })
            .enumerate()
        {
            let width = if expanded {
                layout.wrap_width()
            } else {
                layout.text_width()
            };
            let label = text(scene, line, Font::Semibold, 13.5, width, theme.ink)?;
            let y = if expanded {
                12.0 + index as f32 * 20.0
            } else {
                21.0 - label.size.Y / 2.0
            };
            label.visual.SetOffset(xy(layout.text_left(), y))?;
            children.InsertAtTop(&label.visual)?;
        }
        let ink = if content.emphasized_detail {
            theme.accent
        } else {
            theme.muted
        };
        let detail = text(
            scene,
            content.detail,
            Font::Regular,
            12.0,
            layout.text_width(),
            ink,
        )?;
        detail.visual.SetOffset(xy(
            layout.text_left(),
            layout.height - 19.0 - detail.size.Y / 2.0,
        ))?;
        children.InsertAtTop(&detail.visual)?;

        let mut actions = Vec::new();
        for action in ItemAction::ORDER {
            if let Some(bounds) = layout.action_bounds(action) {
                let (icon, style) = action.icon(content.copied);
                let button = Button::icon(scene, bounds, style, icon, 10.0, theme)?;
                children.InsertAtTop(button.visual())?;
                actions.push((action, button));
            }
        }

        Ok(Self {
            visual,
            entrance,
            press,
            background,
            image_tile,
            thumbnail: None,
            actions,
            layout,
            position: OffsetTransition::stationary(origin, Instant::now()),
            interaction: Interaction::Idle,
            position_policy: None,
            interaction_policy: None,
            colors,
        })
    }

    pub fn visual(&self) -> &ContainerVisual {
        &self.visual
    }

    pub fn layout(&self) -> ItemLayout {
        self.layout
    }

    /// Destination in the scrolling content's DIP coordinates.
    pub fn bounds(&self) -> Rect {
        self.layout.bounds().translated(self.position.target())
    }

    /// Hit geometry sampled along the same transition as the positioning visual.
    pub fn bounds_at(&self, now: Instant) -> Rect {
        self.layout.bounds().translated(self.position.at(now))
    }

    pub fn action_bounds_at(&self, action: ItemAction, now: Instant) -> Option<Rect> {
        self.layout
            .action_bounds(action)
            .map(|rect| rect.translated(self.position.at(now)))
    }

    pub fn hit_action(&self, point: Vector2, now: Instant) -> Option<ItemAction> {
        let origin = self.position.at(now);
        self.layout
            .hit_action(Vector2::new(point.X - origin.X, point.Y - origin.Y))
    }

    pub fn move_to(&mut self, animator: &Animator, origin: Vector2, now: Instant) -> Result<()> {
        let duration = animator.policy().duration_ms(REFLOW_MS);
        if let Some(policy) = self.position_policy
            && policy == animator.policy()
            && self.position.target() == origin
        {
            return Ok(());
        }
        if self.position_policy != Some(animator.policy()) {
            animator.implicit_offset(&self.visual, duration)?;
            self.position_policy = Some(animator.policy());
        }
        let from = self.position.at(now);
        self.visual.SetOffset(at(origin))?;
        self.position = OffsetTransition::new(from, origin, now, duration);
        Ok(())
    }

    /// Start the entrance transition on a newly inserted visual.
    pub fn animate_in(&mut self, animator: &Animator, now: Instant) -> Result<()> {
        if !animator.policy().allows_motion() {
            return Ok(());
        }
        let target = self.position.target();
        let from = Vector2::new(target.X, target.Y - 18.0);
        animator.implicit_offset(&self.visual, 0)?;
        self.visual.SetOffset(at(from))?;
        self.entrance.SetOpacity(0.0)?;
        self.entrance.SetScale(Vector3::new(0.97, 0.97, 1.0))?;
        animator.implicit_offset(&self.visual, REFLOW_MS)?;
        self.visual.SetOffset(at(target))?;
        animator.opacity(&self.entrance, 1.0, Tween::delayed(300, 60))?;
        animator.spring_scale(&self.entrance, 1.0, Spring::new(60, 0.7))?;
        self.position = OffsetTransition::new(from, target, now, REFLOW_MS);
        self.position_policy = Some(animator.policy());
        Ok(())
    }

    /// Remove this mounted item. Reflow surviving items with move_to as soon
    /// as the caller removes this item from its layout.
    pub fn remove(
        self,
        animator: &Animator,
        parent: &ContainerVisual,
        behavior: Removal,
    ) -> Result<()> {
        animator.detach_after(parent, &self.visual, || {
            if behavior == Removal::Collapse {
                let clip = animator.compositor().CreateInsetClip()?;
                self.entrance.SetClip(&clip)?;
                animator.animate_scalar(
                    &clip.cast()?,
                    h!("BottomInset"),
                    self.layout.height,
                    Tween::new(EXIT_MS),
                    |height| clip.SetBottomInset(height),
                )?;
            }
            animator.opacity(&self.entrance, 0.0, Tween::new(EXIT_MS))
        })
    }

    pub fn set_interaction(&mut self, animator: &Animator, interaction: Interaction) -> Result<()> {
        if interaction == self.interaction && self.interaction_policy == Some(animator.policy()) {
            return Ok(());
        }
        let fill = if interaction.active() {
            self.colors.1
        } else {
            self.colors.0
        };
        animator.animate_color(&self.background.brush, fill, interaction.tween())?;
        if !animator.policy().allows_motion()
            || self.interaction == Interaction::Pressed
            || interaction == Interaction::Pressed
        {
            interaction.press(animator, &self.press, 0.98)?;
        }
        self.interaction = interaction;
        self.interaction_policy = Some(animator.policy());
        Ok(())
    }

    pub fn set_action_interaction(
        &mut self,
        animator: &Animator,
        active: Option<(ItemAction, Interaction)>,
    ) -> Result<()> {
        for (action, button) in &mut self.actions {
            let state = active
                .filter(|(item, _)| item == action)
                .map_or(Interaction::Idle, |(_, state)| state);
            button.set_interaction(animator, state)?;
        }
        Ok(())
    }

    /// Update the copy button, if present, after copying.
    pub fn set_copied(&mut self, scene: &PopupScene, copied: bool) -> Result<()> {
        if let Some((_, button)) = self
            .actions
            .iter_mut()
            .find(|(action, _)| *action == ItemAction::Copy)
        {
            button.set_icon(scene, ItemAction::Copy.icon(copied).0)?;
        }
        Ok(())
    }

    /// The thumbnail is an unparented visual in 56 × 36 local DIPs. Passing
    /// None restores the placeholder. The image slot clips the thumbnail to its rounded tile.
    pub fn set_thumbnail(&mut self, animator: &Animator, thumbnail: Option<&Visual>) -> Result<()> {
        let tile = self
            .image_tile
            .as_ref()
            .ok_or_else(|| Error::new(E_INVALIDARG, "Thumbnail requires an image item"))?;
        if self.thumbnail.as_ref() == thumbnail {
            return Ok(());
        }
        let children = tile.Children()?;
        if let Some(thumbnail) = thumbnail {
            thumbnail.SetOpacity(if animator.policy().allows_motion() {
                0.0
            } else {
                1.0
            })?;
            children.InsertAtTop(thumbnail)?;
            animator.opacity(thumbnail, 1.0, Tween::new(200))?;
        }
        if let Some(old) = &self.thumbnail {
            children.Remove(old)?;
        }
        self.thumbnail = thumbnail.cloned();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_rows_reserve_the_python_action_slots() {
        let phone = ItemLayout::new(356.0, 60.0, ItemKind::Text, ItemActions::CopyDelete);
        assert_eq!(
            phone.action_bounds(ItemAction::Copy),
            Some(Rect::new(312.0, 14.0, 32.0, 32.0))
        );
        assert_eq!(
            phone.action_bounds(ItemAction::Delete),
            Some(Rect::new(272.0, 14.0, 32.0, 32.0))
        );
        assert_eq!(phone.wrap_width(), 204.0);
        let pc = ItemLayout {
            actions: ItemActions::CopySendDelete,
            ..phone
        };
        assert_eq!(
            pc.action_bounds(ItemAction::Send),
            phone.action_bounds(ItemAction::Delete)
        );
        assert_eq!(
            pc.hit_action(Vector2::new(232.0, 14.0)),
            Some(ItemAction::Delete)
        );
        assert_eq!(pc.hit_action(Vector2::new(264.0, 14.0)), None);
        assert_eq!(pc.hit_action(Vector2::new(271.0, 14.0)), None);
        assert_eq!(pc.hit_action(Vector2::new(312.0, 46.0)), None);
    }

    #[test]
    fn thumbnail_and_expanded_text_have_reserved_room() {
        assert_eq!(ItemLayout::selected_text_height(0), 60.0);
        assert_eq!(ItemLayout::selected_text_height(1), 66.0);
        assert_eq!(ItemLayout::selected_text_height(5), 106.0);
        let image = ItemLayout::new(356.0, 60.0, ItemKind::Image, ItemActions::None);
        assert_eq!(image.tile(), Rect::new(12.0, 12.0, 56.0, 36.0));
        assert_eq!(image.text_left(), 80.0);
        assert!(image.hit_action(Vector2::new(312.0, 20.0)).is_none());
    }
}
