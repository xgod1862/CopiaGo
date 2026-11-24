//! DirectWrite text and icon sprites, rasterized into WARP drawing surfaces.

use std::cell::Cell;

use windows::{
    Foundation::Size,
    Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat},
    UI::{
        Color,
        Composition::{CompositionGraphicsDevice, Compositor, SpriteVisual},
    },
    Win32::{
        Foundation::{E_INVALIDARG, E_UNEXPECTED, POINT},
        Graphics::{
            Direct2D::{
                Common::D2D1_COLOR_F, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                D2D1CreateFactory, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1,
            },
            Direct3D::D3D_DRIVER_TYPE_WARP,
            Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice},
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_LINE_SPACING_METHOD_UNIFORM,
                DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory,
                IDWriteFactory,
            },
            Dxgi::IDXGIDevice,
        },
        System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositorInterop},
    },
    core::{Error, Interface, PCWSTR, Result, w},
};
use windows_numerics::{Matrix3x2, Vector2};

/// Weight in Microsoft YaHei UI. DirectWrite supplies fallback for other scripts.
#[derive(Clone, Copy, Debug, Default)]
pub enum Font {
    #[default]
    Regular,
    Semibold,
    Bold,
}

impl Font {
    fn weight(self) -> DWRITE_FONT_WEIGHT {
        match self {
            Self::Regular => DWRITE_FONT_WEIGHT_NORMAL,
            Self::Semibold => DWRITE_FONT_WEIGHT_SEMI_BOLD,
            Self::Bold => DWRITE_FONT_WEIGHT_BOLD,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TextStyle {
    pub font: Font,
    /// Font size and maximum width in DIPs.
    pub size: f32,
    pub max_width: f32,
    pub color: Color,
}

/// A premultiplied BGRA drawing surface displayed by this sprite.
pub struct TextSurface {
    pub visual: SpriteVisual,
    /// The sprite size in DIPs, including a small antialiasing margin.
    pub size: Vector2,
}

pub(super) struct Drawing {
    graphics: CompositionGraphicsDevice,
    _device: ID2D1Device,
    write: IDWriteFactory,
    icon_family: Cell<Option<PCWSTR>>,
}

impl Drawing {
    pub(super) fn new(compositor: &Compositor) -> Result<Self> {
        let mut d3d = None;
        // SAFETY: The device output is an initialized, writable COM pointer slot.
        // A null adapter with WARP selects software rasterization.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )?
        };
        let dxgi: IDXGIDevice = d3d
            .ok_or_else(|| Error::from_hresult(E_UNEXPECTED))?
            .cast()?;
        // SAFETY: The factories and device are created and used on the same UI thread.
        let (device, graphics, write) = unsafe {
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let device = factory.CreateDevice(&dxgi)?;
            let interop: ICompositorInterop = compositor.cast()?;
            let graphics = interop.CreateGraphicsDevice(&device)?;
            let write = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            (device, graphics, write)
        };
        Ok(Self {
            graphics,
            _device: device,
            write,
            icon_family: Cell::new(None),
        })
    }

    pub(super) fn text(
        &self,
        compositor: &Compositor,
        scale: f32,
        text: &str,
        style: TextStyle,
    ) -> Result<TextSurface> {
        self.wrapped_text(compositor, scale, text, style, 1)
    }

    pub(super) fn wrapped_text(
        &self,
        compositor: &Compositor,
        scale: f32,
        text: &str,
        style: TextStyle,
        max_lines: u8,
    ) -> Result<TextSurface> {
        let text: Vec<u16> = text
            .encode_utf16()
            .map(|unit| match unit {
                0x000A | 0x000D | 0x2028 | 0x2029 => u16::from(b' '),
                _ => unit,
            })
            .collect();
        self.rasterize(
            compositor,
            scale,
            &text,
            style,
            w!("Microsoft YaHei UI"),
            max_lines,
        )
    }

    pub(super) fn icon(
        &self,
        compositor: &Compositor,
        scale: f32,
        glyph: char,
        size: f32,
        color: Color,
    ) -> Result<TextSurface> {
        let family = self.icon_font()?;
        let mut utf16 = [0; 2];
        self.rasterize(
            compositor,
            scale,
            glyph.encode_utf16(&mut utf16),
            TextStyle {
                font: Font::Regular,
                size,
                max_width: 2.0 * size,
                color,
            },
            family,
            1,
        )
    }

    fn icon_font(&self) -> Result<PCWSTR> {
        if let Some(family) = self.icon_family.get() {
            return Ok(family);
        }
        let mut index = 0;
        let mut exists = Default::default();
        // SAFETY: The collection output and both lookup outputs remain writable; the name is static.
        unsafe {
            let mut fonts = None;
            self.write.GetSystemFontCollection(&mut fonts, false)?;
            let fonts = fonts.ok_or_else(|| Error::from_hresult(E_UNEXPECTED))?;
            fonts.FindFamilyName(w!("Segoe Fluent Icons"), &mut index, &mut exists)?;
        }
        let family = if exists.as_bool() {
            w!("Segoe Fluent Icons")
        } else {
            w!("Segoe MDL2 Assets")
        };
        self.icon_family.set(Some(family));
        Ok(family)
    }

    fn rasterize(
        &self,
        compositor: &Compositor,
        scale: f32,
        text: &[u16],
        style: TextStyle,
        family: PCWSTR,
        max_lines: u8,
    ) -> Result<TextSurface> {
        let font_px = style.size * scale;
        let max_px = (style.max_width * scale).floor();
        if !font_px.is_finite() || !max_px.is_finite() || font_px <= 0.0 || max_px < 3.0 {
            return Err(Error::new(E_INVALIDARG, "Invalid text surface dimensions"));
        }

        let mut metrics = DWRITE_TEXT_METRICS::default();
        // SAFETY: Both string pointers refer to static, null-terminated font and locale names.
        // DirectWrite receives a UTF-16 slice and valid output storage for the layout metrics.
        let layout = unsafe {
            let format = self.write.CreateTextFormat(
                family,
                None,
                style.font.weight(),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                font_px,
                w!("zh-CN"),
            )?;
            let layout_height = if max_lines > 1 {
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
                let line_height = font_px * 1.5;
                format.SetLineSpacing(
                    DWRITE_LINE_SPACING_METHOD_UNIFORM,
                    line_height,
                    font_px * 1.12,
                )?;
                line_height * f32::from(max_lines) + 0.01
            } else {
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                font_px * 3.0
            };
            let sign = self.write.CreateEllipsisTrimmingSign(&format)?;
            format.SetTrimming(
                &DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    ..Default::default()
                },
                &sign,
            )?;
            let layout = self
                .write
                .CreateTextLayout(text, &format, max_px - 2.0, layout_height)?;
            layout.GetMetrics(&mut metrics)?;
            layout
        };
        let width = metrics
            .widthIncludingTrailingWhitespace
            .ceil()
            .min(max_px - 2.0)
            + 2.0;
        let height = metrics
            .height
            .min(if max_lines > 1 {
                font_px * 1.5 * f32::from(max_lines)
            } else {
                f32::INFINITY
            })
            .ceil()
            + 2.0;
        let surface = self.graphics.CreateDrawingSurface(
            Size {
                Width: width,
                Height: height,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        let mut offset = POINT::default();
        // SAFETY: The null rectangle updates the whole surface and writes its pixel offset.
        let context: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
        let paint = || -> Result<()> {
            let color = style.color;
            // SAFETY: The context and referenced layout, colors and transform are valid for this draw.
            unsafe {
                // The text metrics are already in pixels, including on high-DPI monitors.
                context.SetDpi(96.0, 96.0);
                context.SetTransform(&Matrix3x2::translation(offset.x as f32, offset.y as f32));
                context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                context.Clear(Some(&D2D1_COLOR_F::default()));
                let brush = context.CreateSolidColorBrush(
                    &D2D1_COLOR_F {
                        r: f32::from(color.R) / 255.0,
                        g: f32::from(color.G) / 255.0,
                        b: f32::from(color.B) / 255.0,
                        a: f32::from(color.A) / 255.0,
                    },
                    None,
                )?;
                context.DrawTextLayout(
                    Vector2::new(1.0, 1.0),
                    &layout,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                );
            }
            Ok(())
        };
        let painted = paint();
        // SAFETY: Every successful BeginDraw is paired with EndDraw, including on paint failure.
        let ended = unsafe { interop.EndDraw() };
        painted?;
        ended?;

        let size = Vector2::new(width / scale, height / scale);
        let visual = compositor.CreateSpriteVisual()?;
        visual.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
        visual.SetSize(size)?;
        Ok(TextSurface { visual, size })
    }
}
