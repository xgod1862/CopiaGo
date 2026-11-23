//! Bounded Windows Imaging Component (WIC) decoding and PNG/DIB conversion.

#[cfg(test)]
pub(crate) mod tests;

use std::fmt;
use std::ptr::{null, null_mut};

use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatBmp, GUID_ContainerFormatGif,
    GUID_ContainerFormatHeif, GUID_ContainerFormatJpeg, GUID_ContainerFormatPng,
    GUID_ContainerFormatTiff, GUID_ContainerFormatWebp, GUID_WICPixelFormat32bppBGRA,
    IWICBitmapSource, IWICFormatConverter, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::Graphics::Imaging::{
    IWICBitmapFrameDecode, IWICBitmapFrameEncode, WICBitmapTransformFlipHorizontal,
    WICBitmapTransformFlipVertical, WICBitmapTransformOptions, WICBitmapTransformRotate90,
    WICBitmapTransformRotate180, WICBitmapTransformRotate270,
};
use windows::Win32::System::Com::StructuredStorage::{
    CreateStreamOnHGlobal, PROPVARIANT, PropVariantClear, PropVariantToUInt16,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, IStream, STREAM_SEEK_END, STREAM_SEEK_SET,
};
use windows::core::{GUID, w};

/// Maximum encoded image size in bytes.
pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
/// Maximum original pixel count, including for scaled decoding.
pub const MAX_IMAGE_PIXELS: u64 = 50_000_000;

#[derive(Debug)]
pub enum ImageError {
    /// Invalid dimensions, layout, or input format.
    InvalidImage,
    /// Encoded bytes or original pixel count exceed the image limits.
    ImageTooLarge,
    /// The HRESULT returned by COM or WIC, including optional codec failures.
    Codec(i32),
}

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidImage => formatter.write_str("Invalid or unsupported image"),
            Self::ImageTooLarge => formatter.write_str("Image exceeds the size limit"),
            Self::Codec(code) => write!(formatter, "WIC operation failed ({code:#010x})"),
        }
    }
}

impl std::error::Error for ImageError {}

impl From<windows::core::Error> for ImageError {
    fn from(error: windows::core::Error) -> Self {
        Self::Codec(error.code().0)
    }
}

type Result<T> = std::result::Result<T, ImageError>;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const BMP_HEADER_BYTES: usize = 14;
const DIB_V5_HEADER_BYTES: usize = 124;
const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;
const LCS_SRGB: u32 = 0x7352_4742;
const LCS_GM_IMAGES: u32 = 4;
const PROFILE_LINKED: u32 = 0x4c49_4e4b;
const PROFILE_EMBEDDED: u32 = 0x4d42_4544;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImageSize {
    width: u32,
    height: u32,
}

impl ImageSize {
    fn checked(width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(ImageError::InvalidImage);
        }
        if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
            return Err(ImageError::ImageTooLarge);
        }
        Ok(Self { width, height })
    }

    fn stride(self) -> u32 {
        self.width * 4
    }

    fn pixel_bytes(self) -> usize {
        (self.stride() * self.height) as usize
    }

    pub(crate) fn dib_bytes(self) -> usize {
        DIB_V5_HEADER_BYTES + self.pixel_bytes()
    }

    fn fit_within(self, max_width: u32, max_height: u32) -> Self {
        if self.width <= max_width && self.height <= max_height {
            return self;
        }
        // Match the aspect-ratio rounding in Pillow's thumbnail operation.
        let aspect = f64::from(self.width) / f64::from(self.height);
        let width = f64::from(max_width);
        let height = f64::from(max_height);
        if width / height >= aspect {
            Self {
                width: round_aspect(height * aspect, |value| (aspect - value / height).abs()),
                height: max_height,
            }
        } else {
            Self {
                width: max_width,
                height: round_aspect(width / aspect, |value| (aspect - width / value).abs()),
            }
        }
    }
}

fn round_aspect(value: f64, difference: impl Fn(f64) -> f64) -> u32 {
    let lower = value.floor().max(1.0);
    let upper = value.ceil().max(1.0);
    if difference(lower) <= difference(upper) {
        lower as u32
    } else {
        upper as u32
    }
}

/// Tightly packed top-down BGRA pixels with straight alpha.
pub struct BgraImage {
    size: ImageSize,
    pixels: Vec<u8>,
}

impl BgraImage {
    /// Builds an image from exactly `width * height * 4` straight-alpha BGRA bytes.
    pub fn from_bgra(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self> {
        let size = ImageSize::checked(width, height)?;
        if pixels.len() != size.pixel_bytes() {
            return Err(ImageError::InvalidImage);
        }
        Ok(Self { size, pixels })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.size.width, self.size.height)
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

/// Decodes the first frame in its stored orientation. HEIC and WebP use optional WIC codecs.
pub fn decode(encoded: &[u8]) -> Result<BgraImage> {
    let input = EncodedImage::new(encoded)?;
    input.with_frame(|factory, source, size| read_bgra(factory, source, size))
}

/// Decodes the first frame to fit within a nonzero pixel box, preserving aspect
/// ratio and stored orientation. Smaller images keep their original size.
///
/// Only the target-sized BGRA buffer is materialized here. The WIC decoder's
/// scratch memory depends on the encoded input (including progressive JPEG scans).
pub fn decode_scaled(encoded: &[u8], max_width: u32, max_height: u32) -> Result<BgraImage> {
    if max_width == 0 || max_height == 0 {
        return Err(ImageError::InvalidImage);
    }
    let input = EncodedImage::new(encoded)?;
    input.with_frame(|factory, source, size| {
        let target = size.fit_within(max_width, max_height);
        if target == size {
            return read_bgra(factory, source, size);
        }

        // SAFETY: The scaler and source are in the active COM apartment and
        // the dimensions are nonzero and bounded by the original pixel count.
        let scaler = unsafe {
            let scaler = factory.CreateBitmapScaler()?;
            // Putting the scaler immediately after the frame lets WIC use
            // the decoder's native downscaling, including the JPEG transform.
            scaler.Initialize(
                source,
                target.width,
                target.height,
                WICBitmapInterpolationModeFant,
            )?;
            scaler
        };
        read_bgra(factory, &scaler, target)
    })
}

/// Encodes straight-alpha BGRA as PNG.
pub fn encode_png(image: &BgraImage) -> Result<Vec<u8>> {
    with_factory(|factory| {
        encode_png_with(factory, image.size, |frame| {
            // SAFETY: The checked BGRA buffer covers every row at this stride.
            unsafe { frame.WritePixels(image.size.height, image.size.stride(), &image.pixels) }
        })
    })
}

/// Converts a PNG to a top-down, alpha-capable packed DIBV5.
pub fn png_to_dib_v5(png: &[u8]) -> Result<Vec<u8>> {
    check_encoded_size(png.len())?;
    with_frame(
        png_payload(png)?,
        &GUID_ContainerFormatPng,
        |factory, source, size| {
            let bgra = to_bgra(factory, source)?;
            let mut dib = vec![0; size.dib_bytes()];
            write_dib_v5(&bgra, size, &mut dib)?;
            Ok(dib)
        },
    )
}

/// Converts a packed DIB or DIBV5 to PNG.
pub fn dib_to_png(dib: &[u8]) -> Result<Vec<u8>> {
    bmp_to_png(&dib_as_bmp(dib)?)
}

fn check_encoded_size(size: usize) -> Result<()> {
    if size > MAX_IMAGE_BYTES {
        return Err(ImageError::ImageTooLarge);
    }
    Ok(())
}

pub(crate) fn png_payload(data: &[u8]) -> Result<&[u8]> {
    let Some(header) = data.get(..33) else {
        return Err(ImageError::InvalidImage);
    };
    if !header.starts_with(PNG_SIGNATURE) || &header[8..16] != b"\0\0\0\x0dIHDR" {
        return Err(ImageError::InvalidImage);
    }
    let width = u32::from_be_bytes(header[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(header[20..24].try_into().unwrap());
    ImageSize::checked(width, height)?;

    // GlobalSize can include allocator padding. Follow chunk boundaries to IEND
    // to recover exactly the PNG bytes, within the encoded size limit.
    let mut offset = header.len();
    loop {
        let chunk = data
            .get(offset..)
            .filter(|chunk| chunk.len() >= 12)
            .ok_or(ImageError::InvalidImage)?;
        let length = u32::from_be_bytes(chunk[..4].try_into().unwrap()) as u64;
        let end = offset as u64 + length + 12;
        if end > MAX_IMAGE_BYTES as u64 {
            return Err(ImageError::ImageTooLarge);
        }
        offset = end as usize;
        if offset > data.len() {
            return Err(ImageError::InvalidImage);
        }
        if &chunk[4..8] == b"IEND" {
            if length != 0 {
                return Err(ImageError::InvalidImage);
            }
            return Ok(&data[..offset]);
        }
    }
}

/// Reuse a PNG from PC history or convert an older encoded image for SET.
pub fn png_for_device(bytes: Vec<u8>) -> Result<Vec<u8>> {
    if bytes.starts_with(PNG_SIGNATURE) {
        let length = png_payload(&bytes)?.len();
        validate_png(&bytes[..length])?;
        let mut bytes = bytes;
        bytes.truncate(length);
        return Ok(bytes);
    }
    encode_png(&decode_scaled(&bytes, u32::MAX, u32::MAX)?)
}

pub(crate) fn validate_png(png: &[u8]) -> Result<()> {
    with_frame(png, &GUID_ContainerFormatPng, |_, _, _| Ok(()))
}

pub(crate) fn bmp_to_png(bmp: &[u8]) -> Result<Vec<u8>> {
    with_frame(bmp, &GUID_ContainerFormatBmp, |factory, source, size| {
        let bgra = to_bgra(factory, source)?;
        encode_source_png(factory, &bgra, size)
    })
}

pub(crate) fn with_clipboard_source<T>(
    encoded: &[u8],
    convert: impl FnOnce(&IWICImagingFactory, &IWICBitmapSource, ImageSize, Option<&[u8]>) -> T,
) -> Result<T> {
    let input = EncodedImage::new(encoded)?;
    let original_png = (input.format == GUID_ContainerFormatPng).then_some(input.bytes);
    input.with_frame(|factory, source, size| {
        let mut rotated_size = size;
        let rotated = if let Some(transform) = exif_transform(source, &input.format) {
            // SAFETY: The rotator and source are in the active apartment.
            // The output size pointers refer to initialized stack values.
            unsafe {
                let rotated = factory.CreateBitmapFlipRotator()?;
                rotated.Initialize(source, transform)?;
                rotated.GetSize(&mut rotated_size.width, &mut rotated_size.height)?;
                Some(rotated)
            }
        } else {
            None
        };
        let source: &IWICBitmapSource = match &rotated {
            Some(rotated) => rotated,
            None => source,
        };
        let rotated_size = ImageSize::checked(rotated_size.width, rotated_size.height)?;
        let bgra = to_bgra(factory, source)?;
        Ok(convert(factory, &bgra, rotated_size, original_png))
    })
}

fn exif_transform(
    source: &IWICBitmapFrameDecode,
    format: &GUID,
) -> Option<WICBitmapTransformOptions> {
    let query = if *format == GUID_ContainerFormatJpeg {
        w!("/app1/ifd/{ushort=274}")
    } else if *format == GUID_ContainerFormatTiff {
        w!("/ifd/{ushort=274}")
    } else {
        return None;
    };
    // SAFETY: The metadata interface is local to the active apartment. The
    // initialized PROPVARIANT is cleared after reading its numeric value.
    let orientation = unsafe {
        let reader = source.GetMetadataQueryReader().ok()?;
        let mut value = PROPVARIANT::default();
        let orientation = reader
            .GetMetadataByName(query, &mut value)
            .and_then(|()| PropVariantToUInt16(&value));
        let _ = PropVariantClear(&mut value);
        orientation.ok()?
    };
    let transform = match orientation {
        2 => WICBitmapTransformFlipHorizontal,
        3 => WICBitmapTransformRotate180,
        4 => WICBitmapTransformFlipVertical,
        // WIC flips the source before rotating for combined transforms.
        5 => WICBitmapTransformOptions(
            WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0,
        ),
        6 => WICBitmapTransformRotate90,
        7 => WICBitmapTransformOptions(
            WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0,
        ),
        8 => WICBitmapTransformRotate270,
        _ => return None,
    };
    Some(transform)
}

fn detect_container(encoded: &[u8]) -> Result<GUID> {
    if encoded.starts_with(PNG_SIGNATURE) {
        Ok(GUID_ContainerFormatPng)
    } else if encoded.starts_with(b"\xff\xd8") {
        Ok(GUID_ContainerFormatJpeg)
    } else if encoded.starts_with(b"II*\0") || encoded.starts_with(b"MM\0*") {
        Ok(GUID_ContainerFormatTiff)
    } else if encoded.starts_with(b"GIF87a") || encoded.starts_with(b"GIF89a") {
        Ok(GUID_ContainerFormatGif)
    } else if encoded.starts_with(b"BM") {
        Ok(GUID_ContainerFormatBmp)
    } else if encoded.starts_with(b"RIFF") && encoded.get(8..12) == Some(b"WEBP") {
        Ok(GUID_ContainerFormatWebp)
    } else if is_heic(encoded) {
        Ok(GUID_ContainerFormatHeif)
    } else {
        Err(ImageError::InvalidImage)
    }
}

fn is_heic(encoded: &[u8]) -> bool {
    let Some(header) = encoded.get(..8).filter(|header| &header[4..] == b"ftyp") else {
        return false;
    };
    let (offset, size) = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
        1 => {
            let Some(larger_size) = encoded.get(8..16) else {
                return false;
            };
            (16, u64::from_be_bytes(larger_size.try_into().unwrap()))
        }
        size => (8, u64::from(size)),
    };
    let Some(brand_bytes) = usize::try_from(size)
        .ok()
        .and_then(|end| encoded.get(offset..end))
    else {
        return false;
    };
    let (brands, remainder) = brand_bytes.as_chunks::<4>();
    let [major, _, compatible @ ..] = brands else {
        return false;
    };
    // A generic ISO BMFF or HEIF brand alone does not identify HEVC images.
    let hevc_brand = |brand: &[u8; 4]| {
        matches!(
            brand,
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"hevm" | b"hevs"
        )
    };
    remainder.is_empty() && (hevc_brand(major) || compatible.iter().any(hevc_brand))
}

struct EncodedImage<'a> {
    bytes: &'a [u8],
    format: GUID,
}

impl<'a> EncodedImage<'a> {
    fn new(mut bytes: &'a [u8]) -> Result<Self> {
        check_encoded_size(bytes.len())?;
        let format = detect_container(bytes)?;
        if format == GUID_ContainerFormatPng {
            bytes = png_payload(bytes)?;
        } else if format == GUID_ContainerFormatBmp {
            let dib = bytes
                .get(BMP_HEADER_BYTES..)
                .ok_or(ImageError::InvalidImage)?;
            check_linked_profile(dib)?;
        }
        Ok(Self { bytes, format })
    }

    fn with_frame<T>(
        &self,
        convert: impl FnOnce(&IWICImagingFactory, &IWICBitmapFrameDecode, ImageSize) -> Result<T>,
    ) -> Result<T> {
        with_frame(self.bytes, &self.format, convert)
    }
}

struct ComApartment(bool);

impl ComApartment {
    fn initialize() -> windows::core::Result<Self> {
        // SAFETY: A null reserved pointer is required. Balance S_OK and
        // S_FALSE on this thread; an existing MTA can also use WIC.
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result == RPC_E_CHANGED_MODE {
            return Ok(Self(false));
        }
        result.ok()?;
        Ok(Self(true))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: This instance owns a matching COM initialization on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn with_factory<T>(run: impl FnOnce(&IWICImagingFactory) -> Result<T>) -> Result<T> {
    let _apartment = ComApartment::initialize()?;
    // SAFETY: COM is initialized on this thread; the factory is used inside this apartment.
    let factory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
    run(&factory)
}

fn with_frame<T>(
    encoded: &[u8],
    format: &GUID,
    convert: impl FnOnce(&IWICImagingFactory, &IWICBitmapFrameDecode, ImageSize) -> Result<T>,
) -> Result<T> {
    // SAFETY: The stream borrows encoded bytes for reading. The decoder, frame
    // and converter are all used and dropped inside the active apartment.
    with_factory(|factory| unsafe {
        let stream = factory.CreateStream()?;
        stream.InitializeFromMemory(encoded)?;
        let decoder = factory.CreateDecoder(format, null())?;
        decoder.Initialize(&stream, WICDecodeMetadataCacheOnDemand)?;
        let source = decoder.GetFrame(0)?;
        let mut width = 0;
        let mut height = 0;
        source.GetSize(&mut width, &mut height)?;
        convert(factory, &source, ImageSize::checked(width, height)?)
    })
}

fn to_bgra(
    factory: &IWICImagingFactory,
    source: &IWICBitmapSource,
) -> windows::core::Result<IWICFormatConverter> {
    // SAFETY: Both objects belong to the active apartment. The converter
    // retains a reference to source for the duration of the conversion.
    unsafe {
        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(
            source,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        Ok(converter)
    }
}

fn read_bgra(
    factory: &IWICImagingFactory,
    source: &IWICBitmapSource,
    size: ImageSize,
) -> Result<BgraImage> {
    let bgra = to_bgra(factory, source)?;
    let mut pixels = vec![0; size.pixel_bytes()];
    // SAFETY: The destination has enough space for every row at the checked stride.
    unsafe { bgra.CopyPixels(null(), size.stride(), &mut pixels)? };
    Ok(BgraImage { size, pixels })
}

pub(crate) fn encode_source_png(
    factory: &IWICImagingFactory,
    source: &IWICBitmapSource,
    size: ImageSize,
) -> Result<Vec<u8>> {
    encode_png_with(factory, size, |frame| {
        // SAFETY: The BGRA source is live in the active apartment. Null writes the whole image.
        unsafe { frame.WriteSource(source, null()) }
    })
}

fn encode_png_with(
    factory: &IWICImagingFactory,
    size: ImageSize,
    write: impl FnOnce(&IWICBitmapFrameEncode) -> windows::core::Result<()>,
) -> Result<Vec<u8>> {
    // SAFETY: The stream is owned by COM, all interfaces are local to this
    // apartment and the output pointers are valid for each WIC call.
    unsafe {
        let stream = CreateStreamOnHGlobal(Default::default(), true)?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, null_mut())?;
        let frame = frame.ok_or(ImageError::InvalidImage)?;
        frame.Initialize(None)?;
        frame.SetSize(size.width, size.height)?;
        let mut pixel_format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut pixel_format)?;
        if pixel_format != GUID_WICPixelFormat32bppBGRA {
            return Err(ImageError::InvalidImage);
        }
        write(&frame)?;
        frame.Commit()?;
        encoder.Commit()?;

        read_encoded_stream(&stream)
    }
}

fn read_encoded_stream(stream: &IStream) -> Result<Vec<u8>> {
    // SAFETY: The stream is live in the active apartment. Read is bounded by the allocated buffer.
    unsafe {
        let mut length = 0;
        stream.Seek(0, STREAM_SEEK_END, Some(&mut length))?;
        if length > MAX_IMAGE_BYTES as u64 {
            return Err(ImageError::ImageTooLarge);
        }
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut png = vec![0; length as usize];
        let mut read = 0;
        stream
            .Read(png.as_mut_ptr().cast(), length as u32, Some(&mut read))
            .ok()?;
        if u64::from(read) != length {
            return Err(ImageError::InvalidImage);
        }
        Ok(png)
    }
}

pub(crate) fn write_dib_v5(
    source: &IWICBitmapSource,
    size: ImageSize,
    data: &mut [u8],
) -> Result<()> {
    if data.len() != size.dib_bytes() {
        return Err(ImageError::InvalidImage);
    }
    let (header, pixels) = data.split_at_mut(DIB_V5_HEADER_BYTES);
    header.fill(0);
    header[..4].copy_from_slice(&(DIB_V5_HEADER_BYTES as u32).to_le_bytes());
    header[4..8].copy_from_slice(&size.width.to_le_bytes());
    header[8..12].copy_from_slice(&(-(size.height as i32)).to_le_bytes());
    header[12..14].copy_from_slice(&1u16.to_le_bytes()); // planes
    header[14..16].copy_from_slice(&32u16.to_le_bytes()); // bits per pixel
    header[16..20].copy_from_slice(&BI_BITFIELDS.to_le_bytes());
    header[20..24].copy_from_slice(&(size.pixel_bytes() as u32).to_le_bytes());
    header[40..44].copy_from_slice(&0x00ff_0000u32.to_le_bytes()); // red
    header[44..48].copy_from_slice(&0x0000_ff00u32.to_le_bytes()); // green
    header[48..52].copy_from_slice(&0x0000_00ffu32.to_le_bytes()); // blue
    header[52..56].copy_from_slice(&0xff00_0000u32.to_le_bytes()); // alpha
    header[56..60].copy_from_slice(&LCS_SRGB.to_le_bytes());
    header[108..112].copy_from_slice(&LCS_GM_IMAGES.to_le_bytes());

    // SAFETY: The destination has enough space for every row at the checked BGRA stride.
    unsafe { source.CopyPixels(null(), size.stride(), pixels)? };
    Ok(())
}

pub(crate) fn dib_as_bmp(dib: &[u8]) -> Result<Vec<u8>> {
    let (offset, end) = dib_layout(dib)?;
    let file_bytes = BMP_HEADER_BYTES + end;
    let mut bmp = Vec::with_capacity(file_bytes);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(file_bytes as u32).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&((BMP_HEADER_BYTES + offset) as u32).to_le_bytes());
    bmp.extend_from_slice(&dib[..end]);
    Ok(bmp)
}

fn dib_layout(data: &[u8]) -> Result<(usize, usize)> {
    let header_bytes = le_u32(data, 0)? as usize;
    let header = data.get(..header_bytes).ok_or(ImageError::InvalidImage)?;
    let (size, depth, colors, entry_bytes, compression, masks) = match header_bytes {
        12 => {
            let width = u32::from(le_u16(header, 4)?);
            let height = u32::from(le_u16(header, 6)?);
            let depth = le_u16(header, 10)?;
            (ImageSize::checked(width, height)?, depth, 0, 3, BI_RGB, 0)
        }
        40 | 52 | 56 | 108 | DIB_V5_HEADER_BYTES => {
            let width = le_u32(header, 4)? as i32;
            let height = le_u32(header, 8)? as i32;
            let width = u32::try_from(width).map_err(|_| ImageError::InvalidImage)?;
            let compression = le_u32(header, 16)?;
            let masks = match (header_bytes, compression) {
                (40, BI_BITFIELDS) => 12,
                (40, BI_ALPHABITFIELDS) => 16,
                _ => 0,
            };
            (
                ImageSize::checked(width, height.unsigned_abs())?,
                le_u16(header, 14)?,
                le_u32(header, 32)?,
                4,
                compression,
                masks,
            )
        }
        _ => return Err(ImageError::InvalidImage),
    };

    if !matches!(depth, 0 | 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(ImageError::InvalidImage);
    }
    let colors = if colors == 0 && matches!(depth, 1 | 4 | 8) {
        1 << depth
    } else {
        colors
    };
    let offset = header_bytes as u64 + masks + u64::from(colors) * entry_bytes;
    let pixel_bytes = if matches!(compression, BI_RGB | BI_BITFIELDS | BI_ALPHABITFIELDS) {
        if depth == 0 {
            return Err(ImageError::InvalidImage);
        }
        let stride = (u64::from(size.width) * u64::from(depth)).div_ceil(32) * 4;
        stride * u64::from(size.height)
    } else {
        let encoded_bytes = le_u32(header, 20)?;
        if encoded_bytes == 0 {
            return Err(ImageError::InvalidImage);
        }
        check_encoded_size(encoded_bytes as usize)?;
        u64::from(encoded_bytes)
    };

    let mut end = offset + pixel_bytes;
    check_linked_profile(header)?;
    if header_bytes == DIB_V5_HEADER_BYTES && le_u32(header, 56)? == PROFILE_EMBEDDED {
        let profile_offset = u64::from(le_u32(header, 112)?);
        let profile_bytes = le_u32(header, 116)?;
        if profile_offset < end || profile_bytes == 0 {
            return Err(ImageError::InvalidImage);
        }
        check_encoded_size(profile_bytes as usize)?;
        end = profile_offset + u64::from(profile_bytes);
    }
    if end > MAX_IMAGE_PIXELS * 4 + MAX_IMAGE_BYTES as u64 {
        return Err(ImageError::ImageTooLarge);
    }
    if end > data.len() as u64 {
        return Err(ImageError::InvalidImage);
    }
    Ok((offset as usize, end as usize))
}

fn check_linked_profile(dib: &[u8]) -> Result<()> {
    // Only inline image data may be passed to the BMP codec. A linked
    // colour profile names a separate file and can refer to a network path.
    if le_u32(dib, 0)? as usize == DIB_V5_HEADER_BYTES && le_u32(dib, 56)? == PROFILE_LINKED {
        return Err(ImageError::InvalidImage);
    }
    Ok(())
}

fn le_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or(ImageError::InvalidImage)?;
    Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
}

fn le_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or(ImageError::InvalidImage)?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}
