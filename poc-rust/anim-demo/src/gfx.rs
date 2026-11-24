//! Direct2D/DirectWrite text drawn into composition surfaces.

use windows::{
    Foundation::Size,
    Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat},
    UI::Color,
    UI::Composition::{CompositionGraphicsDevice, Compositor, SpriteVisual},
    Win32::Foundation::POINT,
    Win32::Graphics::Direct2D::Common::*,
    Win32::Graphics::Direct2D::*,
    Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP},
    Win32::Graphics::Direct3D11::*,
    Win32::Graphics::DirectWrite::*,
    Win32::Graphics::Dxgi::IDXGIDevice,
    Win32::System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositorInterop},
    core::*,
};
use windows_numerics::{Matrix3x2, Vector2};

#[derive(Clone, Copy)]
pub struct Font(&'static str, DWRITE_FONT_WEIGHT);
pub const REGULAR: Font = Font("Microsoft YaHei UI", DWRITE_FONT_WEIGHT_NORMAL);
pub const SEMIBOLD: Font = Font("Microsoft YaHei UI", DWRITE_FONT_WEIGHT_SEMI_BOLD);
pub const BOLD: Font = Font("Microsoft YaHei UI", DWRITE_FONT_WEIGHT_BOLD);
pub const ICON: Font = Font("Segoe Fluent Icons", DWRITE_FONT_WEIGHT_NORMAL);

pub struct Gfx {
    s: f32,
    dwrite: IDWriteFactory,
    graphics: CompositionGraphicsDevice,
    _d2d: ID2D1Device,
}

impl Gfx {
    pub fn new(compositor: &Compositor, s: f32) -> Result<Self> {
        unsafe {
            let mut d3d: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                // WARP only rasterizes our few text surfaces on the CPU; the composition and every
                // animation still run on the GPU inside DWM. It avoids loading the GPU vendor's
                // user-mode driver, which costs 15-30 MB of private memory (AMD RX 7900 XT here).
                if std::env::args().any(|a| a == "--hw") { D3D_DRIVER_TYPE_HARDWARE } else { D3D_DRIVER_TYPE_WARP },
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )?;
            let dxgi: IDXGIDevice = d3d.unwrap().cast()?;
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d = factory.CreateDevice(&dxgi)?;
            let interop: ICompositorInterop = compositor.cast()?;
            let graphics = interop.CreateGraphicsDevice(&d2d)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Self { s, dwrite, graphics, _d2d: d2d })
        }
    }

    /// A sprite showing `text` on one line (trimmed with an ellipsis to `max_w` DIPs).
    /// Returns the visual and its pixel size.
    pub fn text(&self, compositor: &Compositor, text: &str, font: Font, size: f32, color: Color, max_w: f32) -> Result<(SpriteVisual, f32, f32)> {
        unsafe {
            let s = self.s;
            let format = self.dwrite.CreateTextFormat(
                &HSTRING::from(font.0),
                None,
                font.1,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size * s,
                w!("zh-CN"),
            )?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let sign = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            format.SetTrimming(&trimming, &sign)?;
            let wide: Vec<u16> = text.encode_utf16().collect();
            let layout = self.dwrite.CreateTextLayout(&wide, &format, max_w * s, size * s * 3.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            let w = m.widthIncludingTrailingWhitespace.min(max_w * s).ceil() + 2.0;
            let h = m.height.ceil() + 2.0;

            let surface = self.graphics.CreateDrawingSurface(
                Size { Width: w, Height: h },
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                DirectXAlphaMode::Premultiplied,
            )?;
            let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
            let mut offset = POINT::default();
            let dc: ID2D1DeviceContext = interop.BeginDraw(None, &mut offset)?;
            dc.SetTransform(&Matrix3x2::translation(offset.x as f32, offset.y as f32));
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            let c = D2D1_COLOR_F {
                r: color.R as f32 / 255.0,
                g: color.G as f32 / 255.0,
                b: color.B as f32 / 255.0,
                a: color.A as f32 / 255.0,
            };
            let brush = dc.CreateSolidColorBrush(&c, None)?;
            dc.DrawTextLayout(Vector2::new(1.0, 1.0), &layout, &brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            interop.EndDraw()?;

            let visual = compositor.CreateSpriteVisual()?;
            visual.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
            visual.SetSize(Vector2::new(w, h))?;
            Ok((visual, w, h))
        }
    }
}
