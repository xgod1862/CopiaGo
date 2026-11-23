use windows::{
    UI::Composition::{CompositionRoundedRectangleGeometry, ShapeVisual},
    core::Result,
};
use windows_numerics::{Vector2, Vector3};

use crate::{
    components::{LIGHT_THEME, Rect},
    composition::PopupScene,
};

/// A dark focus stroke with a white outline for dark and light controls.
pub(crate) struct FocusRing {
    visual: ShapeVisual,
    outline: CompositionRoundedRectangleGeometry,
}

impl FocusRing {
    pub(crate) fn new(scene: &PopupScene) -> Result<Self> {
        let compositor = scene.compositor();
        let visual = compositor.CreateShapeVisual()?;
        let outline = compositor.CreateRoundedRectangleGeometry()?;
        for (width, color) in [(3.0, LIGHT_THEME.white), (1.5, LIGHT_THEME.ink)] {
            let stroke = compositor.CreateSpriteShapeWithGeometry(&outline)?;
            stroke.SetStrokeBrush(&compositor.CreateColorBrushWithColor(color)?)?;
            stroke.SetStrokeThickness(width)?;
            visual.Shapes()?.Append(&stroke)?;
        }
        visual.SetIsVisible(false)?;
        // The overlay follows the card transform and sits above all page content.
        scene.motion_root().Children()?.InsertAtTop(&visual)?;
        Ok(Self { visual, outline })
    }

    pub(crate) fn show(&self, rect: Option<Rect>) -> Result<()> {
        const INSET: f32 = 2.0;
        if let Some(rect) =
            rect.filter(|rect| rect.width > INSET * 2.0 && rect.height > INSET * 2.0)
        {
            let width = rect.width - INSET * 2.0;
            let height = rect.height - INSET * 2.0;
            let radius = 8.0_f32.min(width.min(height) / 2.0);
            self.visual.SetSize(rect.size())?;
            self.visual.SetOffset(Vector3::new(rect.x, rect.y, 0.0))?;
            self.outline.SetSize(Vector2::new(width, height))?;
            self.outline.SetOffset(Vector2::new(INSET, INSET))?;
            self.outline.SetCornerRadius(Vector2::new(radius, radius))?;
            self.visual.SetIsVisible(true)?;
        } else {
            self.visual.SetIsVisible(false)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn is_visible(&self) -> Result<bool> {
        self.visual.IsVisible()
    }
}
