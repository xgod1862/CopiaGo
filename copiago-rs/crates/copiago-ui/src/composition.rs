//! Win32 popup host and drawing primitives for the Visual Layer.

mod drawing;
mod thread;
mod window;

pub use drawing::{Font, TextStyle, TextSurface};
pub use thread::CompositionThread;
pub use window::POPUP_SETTINGS_CHANGED;

use std::{cell::OnceCell, rc::Rc};

use crate::{
    components::Rect,
    focus::{Card, FocusElement, FocusEvent, FocusId, FocusRing},
};
use drawing::Drawing;
use window::{CLOSE_POPUP, DPI_CHANGED, Placement, PopupWindows};
use windows::{
    UI::{
        Color,
        Composition::{
            CompositionColorBrush, CompositionRoundedRectangleGeometry, Compositor,
            ContainerVisual, Desktop::DesktopWindowTarget, ShapeVisual, Visual,
        },
    },
    Win32::{
        Foundation::{E_FAIL, E_INVALIDARG, HWND, POINT},
        System::WinRT::Composition::ICompositorDesktopInterop,
        UI::WindowsAndMessaging::MSG,
    },
    core::{Error, Interface, Result},
};
use windows_numerics::{Vector2, Vector3};

pub const DEFAULT_DPI: u32 = 96;
const SHADOW_MARGIN: f32 = 40.0;

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color {
        A: 255,
        R: r,
        G: g,
        B: b,
    }
}

/// The card dimensions in DIPs, excluding the shadow.
#[derive(Clone, Copy, Debug)]
pub struct PopupBounds {
    pub width: f32,
    pub height: f32,
    pub radius: f32,
}

impl PopupBounds {
    pub const fn new(width: f32, height: f32, radius: f32) -> Self {
        Self {
            width,
            height,
            radius,
        }
    }

    pub fn pixel_size(self, dpi: u32) -> Result<(i32, i32)> {
        let placement = Placement::new(POINT::default(), self, dpi)?;
        Ok((placement.width, placement.height))
    }

    fn size(self) -> Vector2 {
        Vector2::new(self.width, self.height)
    }
}

/// Popup windows and the optional scene. A hidden host owns no Composition graphics.
pub struct PopupHost<'ui> {
    visible: Option<VisiblePopup>,
    windows: PopupWindows,
    _thread: &'ui CompositionThread,
}

impl<'ui> PopupHost<'ui> {
    /// Create hidden windows on a Per-Monitor V2 aware UI thread.
    pub fn new(thread: &'ui CompositionThread) -> Result<Self> {
        Ok(Self {
            visible: None,
            windows: PopupWindows::new()?,
            _thread: thread,
        })
    }

    /// The card's input window. Mouse coordinates for this HWND start at the
    /// top-left of the card and are in physical pixels.
    pub fn hwnd(&self) -> HWND {
        self.windows.input()
    }

    pub fn scene(&self) -> Option<&PopupScene> {
        self.visible.as_ref().map(|visible| &visible.scene)
    }

    /// Present a fresh scene with its card at the given screen pixel.
    /// Set activate to false when opening without taking keyboard focus.
    pub fn show(
        &mut self,
        position: POINT,
        bounds: PopupBounds,
        activate: bool,
        build: impl FnOnce(&PopupScene) -> Result<()>,
    ) -> Result<()> {
        self.hide();
        let placement = Placement::new(position, bounds, self.windows.dpi_at(position)?)?;
        let visible = VisiblePopup::new(self.windows.visual(), placement, build)?;
        self.windows.place(placement)?;
        self.place_accessibility(placement);
        if let Err(error) = self.windows.show(activate) {
            self.hide();
            return Err(error);
        }
        self.visible = Some(visible);
        Ok(())
    }

    /// Move a visible card to a screen pixel. A monitor DPI change queues a
    /// rebuild through handle_message.
    pub fn move_to(&mut self, position: POINT) -> Result<()> {
        if let Some(visible) = &mut self.visible {
            let placement =
                Placement::new(position, visible.placement.bounds, visible.placement.dpi)?;
            if let Err(error) = self.windows.place(placement) {
                self.hide();
                return Err(error);
            }
            visible.placement = placement;
            self.place_accessibility(placement);
        }
        Ok(())
    }

    /// Call before dispatching a message returned by GetMessageW. Returns true
    /// for messages handled here. The builder restores the card contents when
    /// Windows changes monitor DPI, including re-rasterizing text and icons.
    pub fn handle_message(
        &mut self,
        message: &MSG,
        rebuild: impl FnOnce(&PopupScene) -> Result<()>,
    ) -> Result<bool> {
        if message.hwnd != self.hwnd() {
            return Ok(false);
        }
        match message.message {
            CLOSE_POPUP => self.hide(),
            DPI_CHANGED => {
                if let Some((dpi, position)) = self.windows.take_dpi_change()
                    && let Some(visible) = self.visible.as_mut()
                {
                    let placement = Placement::new(position, visible.placement.bounds, dpi)?;
                    if let Err(error) = visible
                        .rebuild(placement, rebuild)
                        .and_then(|()| self.windows.place(placement))
                    {
                        self.hide();
                        return Err(error);
                    }
                    self.place_accessibility(placement);
                    self.refresh_focus()?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Hide immediately and tear down the visual tree, surfaces, and graphics devices.
    pub fn hide(&mut self) {
        self.windows.interaction().hide();
        self.windows.hide();
        self.visible = None;
    }

    /// Publish the visible page's controls in tab order. Call again after
    /// a page, scroll offset, layout or label change. Existing focus survives
    /// while the same control stays enabled and present.
    pub fn set_focus_elements(&self, elements: Vec<FocusElement>) -> Result<()> {
        if self.visible.is_none() {
            return Err(Error::new(E_FAIL, "The popup is hidden"));
        }
        self.windows.interaction().publish(self.hwnd(), elements)?;
        self.refresh_focus()
    }

    /// The last logical target, including while this popup has lost Win32 focus.
    pub fn focused_element(&self) -> Option<FocusId> {
        self.windows.interaction().focused_element()
    }

    /// Update logical focus without setting Win32 focus. Call for pointer selection
    /// or to set a default action when opening a page. None clears logical focus.
    pub fn set_focused_element(&self, id: Option<FocusId>) -> Result<bool> {
        let changed = self.windows.interaction().request_focus(id);
        if changed {
            self.refresh_focus()?;
        }
        Ok(changed)
    }

    /// Process the card's queued keyboard or UI Automation messages before
    /// TranslateMessage/DispatchMessage. Some means the message was consumed.
    pub fn handle_focus_message(&self, message: &MSG) -> Result<Option<FocusEvent>> {
        if message.hwnd != self.hwnd() || self.visible.is_none() {
            return Ok(None);
        }
        let event = self.windows.interaction().handle_message(message);
        if matches!(event, Some(FocusEvent::Changed(_))) {
            self.refresh_focus()?;
        }
        Ok(event)
    }

    /// Notify assistive technology that a button or list item action ran.
    /// Call before an action that removes its own element or starts longer work.
    pub fn notify_invoked(&self, id: FocusId) {
        self.windows.interaction().notify_invoked(self.hwnd(), id);
    }

    fn place_accessibility(&self, placement: Placement) {
        self.windows.interaction().place(Card {
            origin: placement.position,
            bounds: Rect::new(0.0, 0.0, placement.bounds.width, placement.bounds.height),
            scale: f64::from(placement.scale()),
        });
    }

    fn refresh_focus(&self) -> Result<()> {
        let interaction = self.windows.interaction();
        if let Some(scene) = self.scene() {
            scene.show_focus(interaction.focus_bounds())?;
        }
        interaction.announce_focus(self.hwnd());
        Ok(())
    }
}

impl Drop for PopupHost<'_> {
    fn drop(&mut self) {
        self.hide();
    }
}

/// The resources for one visible popup. Handles obtained here belong to the
/// visible scene and must be discarded when the popup is hidden or rebuilt.
pub struct PopupScene {
    compositor: Compositor,
    drawing: Rc<OnceCell<Drawing>>,
    root: ContainerVisual,
    motion_root: ContainerVisual,
    content: ContainerVisual,
    focus_ring: OnceCell<FocusRing>,
    dpi: u32,
}

impl PopupScene {
    fn new(
        compositor: &Compositor,
        drawing: Rc<OnceCell<Drawing>>,
        placement: Placement,
    ) -> Result<Self> {
        let bounds = placement.bounds;
        let root = compositor.CreateContainerVisual()?;
        let scale = placement.scale();
        let margin = placement.margin as f32;
        root.SetOffset(Vector3::new(margin, margin, 0.0))?;
        root.SetScale(Vector3::new(scale, scale, 1.0))?;

        let size = bounds.size();
        let motion_root = compositor.CreateContainerVisual()?;
        motion_root.SetSize(size)?;
        motion_root.SetCenterPoint(Vector3::new(size.X / 2.0, size.Y / 2.0, 0.0))?;
        root.Children()?.InsertAtTop(&motion_root)?;

        // LayerVisual's shadow can cover the layer's own contents. Put only
        // the silhouette in that layer and keep the card contents above it.
        let shadow_layer = compositor.CreateLayerVisual()?;
        // LayerVisual's offscreen raster is sized in pixels. Keeping its DIP
        // size here clips the shadow's right and bottom edges at higher DPI.
        shadow_layer.SetSize(Vector2::new(
            placement.width as f32,
            placement.height as f32,
        ))?;
        let silhouette = rounded_rectangle(compositor, size, bounds.radius, rgb(255, 255, 255))?;
        shadow_layer.Children()?.InsertAtTop(&silhouette.visual)?;
        let shadow = compositor.CreateDropShadow()?;
        shadow.SetBlurRadius(28.0)?;
        shadow.SetOffset(Vector3::new(0.0, 10.0, 0.0))?;
        shadow.SetColor(rgb(23, 32, 43))?;
        shadow.SetOpacity(0.20)?;
        shadow_layer.SetShadow(&shadow)?;
        motion_root.Children()?.InsertAtBottom(&shadow_layer)?;

        let content = compositor.CreateContainerVisual()?;
        content.SetSize(size)?;
        content.Children()?.InsertAtBottom(
            &rounded_rectangle(compositor, size, bounds.radius, rgb(255, 255, 255))?.visual,
        )?;
        motion_root.Children()?.InsertAtTop(&content)?;

        Ok(Self {
            compositor: compositor.clone(),
            drawing,
            root,
            motion_root,
            content,
            focus_ring: OnceCell::new(),
            dpi: placement.dpi,
        })
    }

    pub fn compositor(&self) -> &Compositor {
        &self.compositor
    }

    /// Animate this visual to move, fade, or scale the card and shadow together.
    pub fn motion_root(&self) -> &ContainerVisual {
        &self.motion_root
    }

    /// Place child visuals here using coordinates in DIPs, relative to the card.
    pub fn content(&self) -> &ContainerVisual {
        &self.content
    }

    pub fn dpi(&self) -> u32 {
        self.dpi
    }

    pub fn scale(&self) -> f32 {
        self.dpi as f32 / DEFAULT_DPI as f32
    }

    pub fn client_to_dips(&self, point: POINT) -> Vector2 {
        Vector2::new(point.x as f32 / self.scale(), point.y as f32 / self.scale())
    }

    /// Rasterize one ellipsized line using the current DPI.
    pub fn text(&self, text: &str, style: TextStyle) -> Result<TextSurface> {
        self.draw(|drawing| drawing.text(&self.compositor, self.scale(), text, style))
    }

    /// Rasterize wrapped text with an ellipsis at the last visible line if needed.
    pub fn wrapped_text(&self, text: &str, style: TextStyle, max_lines: u8) -> Result<TextSurface> {
        self.draw(|drawing| {
            drawing.wrapped_text(&self.compositor, self.scale(), text, style, max_lines)
        })
    }

    /// Rasterize a glyph from Segoe Fluent Icons or Segoe MDL2 Assets.
    pub fn icon(&self, glyph: char, size: f32, color: Color) -> Result<TextSurface> {
        self.draw(|drawing| drawing.icon(&self.compositor, self.scale(), glyph, size, color))
    }

    fn draw(&self, draw: impl FnOnce(&Drawing) -> Result<TextSurface>) -> Result<TextSurface> {
        if let Some(drawing) = self.drawing.get() {
            return draw(drawing);
        }
        let drawing = Drawing::new(&self.compositor)?;
        draw(self.drawing.get_or_init(|| drawing))
    }

    fn show_focus(&self, bounds: Option<Rect>) -> Result<()> {
        if let Some(ring) = self.focus_ring.get() {
            return ring.show(bounds);
        }
        if bounds.is_some() {
            let ring = FocusRing::new(self)?;
            self.focus_ring.get_or_init(|| ring).show(bounds)?;
        }
        Ok(())
    }
}

struct VisiblePopup {
    target: DesktopWindowTarget,
    scene: PopupScene,
    placement: Placement,
}

impl VisiblePopup {
    fn new(
        visual_hwnd: HWND,
        placement: Placement,
        build: impl FnOnce(&PopupScene) -> Result<()>,
    ) -> Result<Self> {
        let compositor = Compositor::new()?;
        let interop: ICompositorDesktopInterop = compositor.cast()?;
        // SAFETY: The host owns a live window on the current CompositionThread.
        let target = unsafe { interop.CreateDesktopWindowTarget(visual_hwnd, true)? };
        let visible = Self {
            target,
            scene: PopupScene::new(&compositor, Rc::default(), placement)?,
            placement,
        };
        build(&visible.scene)?;
        visible.target.SetRoot(&visible.scene.root)?;
        Ok(visible)
    }

    fn rebuild(
        &mut self,
        placement: Placement,
        build: impl FnOnce(&PopupScene) -> Result<()>,
    ) -> Result<()> {
        let replacement = PopupScene::new(
            self.scene.compositor(),
            self.scene.drawing.clone(),
            placement,
        )?;
        build(&replacement)?;
        self.target.SetRoot(&replacement.root)?;
        self.scene = replacement;
        self.placement = placement;
        Ok(())
    }
}

impl Drop for VisiblePopup {
    fn drop(&mut self) {
        let _ = self.target.SetRoot(None::<&Visual>);
        let _ = self.target.Close();
        let _ = self.scene.compositor.Close();
    }
}

/// The handles callers need to animate a rounded shape or its fill.
pub struct RoundedRectangle {
    pub visual: ShapeVisual,
    pub geometry: CompositionRoundedRectangleGeometry,
    pub brush: CompositionColorBrush,
}

/// Construct a filled shape with DIP dimensions and a corner radius in DIPs.
pub fn rounded_rectangle(
    compositor: &Compositor,
    size: Vector2,
    radius: f32,
    color: Color,
) -> Result<RoundedRectangle> {
    let radius = bounded_radius(size, radius)?;
    let geometry = compositor.CreateRoundedRectangleGeometry()?;
    geometry.SetSize(size)?;
    geometry.SetCornerRadius(Vector2::new(radius, radius))?;
    let brush = compositor.CreateColorBrushWithColor(color)?;
    let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
    shape.SetFillBrush(&brush)?;
    let visual = compositor.CreateShapeVisual()?;
    visual.SetSize(size)?;
    visual.Shapes()?.Append(&shape)?;
    Ok(RoundedRectangle {
        visual,
        geometry,
        brush,
    })
}

fn bounded_radius(size: Vector2, radius: f32) -> Result<f32> {
    if !size.X.is_finite()
        || !size.Y.is_finite()
        || !radius.is_finite()
        || size.X <= 0.0
        || size.Y <= 0.0
        || radius < 0.0
    {
        return Err(Error::new(
            E_INVALIDARG,
            "Invalid rounded rectangle dimensions",
        ));
    }
    Ok(radius.min(size.X.min(size.Y) / 2.0))
}

#[cfg(test)]
mod focus_tests {
    use windows::Win32::{
        Foundation::WPARAM,
        UI::{
            Input::KeyboardAndMouse::{VK_RETURN, VK_TAB},
            WindowsAndMessaging::{SendMessageW, WM_KEYDOWN, WM_KILLFOCUS, WM_SETFOCUS},
        },
    };

    use super::*;
    use crate::focus::FocusRole;

    #[test]
    #[ignore = "requires a desktop with Windows Composition"]
    fn isolated_window_paints_focus_and_routes_keys_without_native_input() -> Result<()> {
        let thread = CompositionThread::new()?;
        let mut popup = PopupHost::new(&thread)?;
        popup.show(
            POINT { x: 72, y: 72 },
            PopupBounds::new(120.0, 100.0, 12.0),
            false,
            |_| Ok(()),
        )?;
        let id = FocusId(1);
        popup.set_focus_elements(vec![FocusElement {
            id,
            parent: None,
            name: String::from("Test control"),
            role: FocusRole::Button,
            bounds: Some(Rect::new(10.0, 12.0, 80.0, 35.0)),
            enabled: true,
        }])?;
        let mut message = MSG {
            hwnd: popup.hwnd(),
            message: WM_KEYDOWN,
            wParam: WPARAM(usize::from(VK_TAB.0)),
            ..Default::default()
        };
        assert_eq!(popup.handle_focus_message(&message)?, None);
        // SAFETY: Send notifications to our own HWND on this thread without
        // changing the Windows foreground window or injecting keyboard input.
        unsafe { SendMessageW(popup.hwnd(), WM_SETFOCUS, None, None) };
        assert_eq!(
            popup.handle_focus_message(&message)?,
            Some(FocusEvent::Changed(Some(id)))
        );
        let ring = popup.scene().unwrap().focus_ring.get().unwrap();
        assert!(ring.is_visible()?);
        message.wParam = WPARAM(usize::from(VK_RETURN.0));
        assert_eq!(
            popup.handle_focus_message(&message)?,
            Some(FocusEvent::Activated(id))
        );
        // SAFETY: The test still owns this HWND and sends a local notification.
        unsafe { SendMessageW(popup.hwnd(), WM_KILLFOCUS, None, None) };
        popup.refresh_focus()?;
        assert!(!ring.is_visible()?);
        assert_eq!(popup.handle_focus_message(&message)?, None);
        popup.hide();
        assert!(popup.scene().is_none());
        assert_eq!(popup.focused_element(), None);
        Ok(())
    }
}
