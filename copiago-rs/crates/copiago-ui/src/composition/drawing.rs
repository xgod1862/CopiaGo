//! DirectWrite text and icon sprites, rasterized into WARP drawing surfaces.

use std::cell::Cell;

use windows::{
    Foundation::Size,
    Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat},
    UI::{
        Color,
        Composition::{CompositionGraphicsDevice, CompositionStretch, Compositor, SpriteVisual},
    },
    Win32::{
        Foundation::{E_INVALIDARG, E_UNEXPECTED, POINT},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
                    D2D1_PIXEL_FORMAT,
                },
                D2D1_BITMAP_PROPERTIES1, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR,
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Device,
                ID2D1DeviceContext, ID2D1Factory1,
            },
            Direct3D::D3D_DRIVER_TYPE_WARP,
            Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice},
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_LINE_SPACING_METHOD_UNIFORM,
                DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory,
                IDWriteFactory, IDWriteTextFormat,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Dxgi::IDXGIDevice,
        },
        System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositorInterop},
    },
    core::{Error, Interface, PCWSTR, Result, w},
};
use windows_numerics::{Matrix3x2, Vector2};

/// Largest preview edge in pixels: a full-width card at very high display scales.
pub const MAX_THUMBNAIL_SIDE: u32 = 1600;

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

/// Measures a single line in DIPs using the same DirectWrite font as the popup.
/// This can run before creating a graphics device or making a popup visible.
pub struct TextMeasurer(IDWriteFactory);

impl TextMeasurer {
    pub fn new() -> Result<Self> {
        // SAFETY: The shared DirectWrite factory needs no additional parameters.
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.map(Self)
    }

    pub fn width(&self, text: &str, font: Font, size: f32) -> Result<f32> {
        let text = text_units(text);
        let format = single_line_format(&self.0, w!("Microsoft YaHei UI"), font, size)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        // SAFETY: DirectWrite borrows the UTF-16 slice and initialized output storage.
        unsafe {
            self.0
                .CreateTextLayout(&text, &format, 0.0, size * 3.0)?
                .GetMetrics(&mut metrics)?;
        }
        Ok(metrics.widthIncludingTrailingWhitespace)
    }
}

fn text_units(text: &str) -> Vec<u16> {
    text.encode_utf16()
        .map(|unit| match unit {
            0x000A | 0x000D | 0x2028 | 0x2029 => u16::from(b' '),
            _ => unit,
        })
        .collect()
}

fn single_line_format(
    write: &IDWriteFactory,
    family: PCWSTR,
    font: Font,
    size: f32,
) -> Result<IDWriteTextFormat> {
    // SAFETY: All callers supply a static, null-terminated font family and locale.
    unsafe {
        let format = write.CreateTextFormat(
            family,
            None,
            font.weight(),
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("zh-CN"),
        )?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok(format)
    }
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
        let text = text_units(text);
        self.rasterize(
            compositor,
            scale,
            &text,
            style,
            w!("Microsoft YaHei UI"),
            max_lines,
        )
    }

    pub(super) fn thumbnail(
        &self,
        compositor: &Compositor,
        width: u32,
        height: u32,
        straight_bgra: &[u8],
        tile: Vector2,
    ) -> Result<SpriteVisual> {
        let stride = width.checked_mul(4).unwrap_or_default();
        if width == 0
            || height == 0
            || width > MAX_THUMBNAIL_SIDE
            || height > MAX_THUMBNAIL_SIDE
            || stride as usize * height as usize != straight_bgra.len()
        {
            return Err(Error::new(
                E_INVALIDARG,
                "Invalid history preview dimensions",
            ));
        }
        let mut pixels = straight_bgra.to_vec();
        for pixel in pixels.as_chunks_mut::<4>().0 {
            let alpha = u16::from(pixel[3]);
            for component in &mut pixel[..3] {
                *component = ((u16::from(*component) * alpha + 127) / 255) as u8;
            }
        }
        let surface = self.graphics.CreateDrawingSurface(
            Size {
                Width: width as f32,
                Height: height as f32,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        let mut offset = POINT::default();
        // SAFETY: This updates the whole target and writes its offset.
        let context: ID2D1DeviceContext = unsafe { interop.BeginDraw(None, &mut offset)? };
        let paint = || -> Result<()> {
            // SAFETY: The checked pixel buffer remains valid through bitmap creation.
            unsafe {
                let properties = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    ..Default::default()
                };
                let bitmap = context.CreateBitmap(
                    D2D_SIZE_U { width, height },
                    Some(pixels.as_ptr().cast()),
                    stride,
                    &properties,
                )?;
                context.SetDpi(96.0, 96.0);
                context.SetTransform(&Matrix3x2::translation(offset.x as f32, offset.y as f32));
                context.Clear(Some(&D2D1_COLOR_F::default()));
                context.DrawBitmap(
                    &bitmap,
                    Some(&D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: width as f32,
                        bottom: height as f32,
                    }),
                    1.0,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    None,
                    None,
                );
            }
            Ok(())
        };
        let painted = paint();
        // SAFETY: Pair BeginDraw with EndDraw even if creating the bitmap failed.
        let ended = unsafe { interop.EndDraw() };
        painted?;
        ended?;
        let brush = compositor.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::UniformToFill)?;
        brush.SetHorizontalAlignmentRatio(0.5)?;
        brush.SetVerticalAlignmentRatio(0.5)?;
        let visual = compositor.CreateSpriteVisual()?;
        visual.SetSize(tile)?;
        visual.SetBrush(&brush)?;
        Ok(visual)
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
        let format = single_line_format(&self.write, family, style.font, font_px)?;
        // SAFETY: DirectWrite receives a UTF-16 slice and valid output storage.
        let layout = unsafe {
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
