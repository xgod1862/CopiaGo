//! Minimal conversion between encoded images and Windows clipboard bitmaps.

use std::ptr::{null, null_mut};

use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatBmp, GUID_ContainerFormatGif,
    GUID_ContainerFormatJpeg, GUID_ContainerFormatPng, GUID_ContainerFormatTiff,
    GUID_ContainerFormatWebp, GUID_WICPixelFormat32bppBGRA, IWICBitmapSource, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::Graphics::Imaging::{
    IWICBitmapFrameDecode, WICBitmapTransformFlipHorizontal, WICBitmapTransformFlipVertical,
    WICBitmapTransformOptions, WICBitmapTransformRotate90, WICBitmapTransformRotate180,
    WICBitmapTransformRotate270,
};
use windows::Win32::System::Com::StructuredStorage::{
    CreateStreamOnHGlobal, PROPVARIANT, PropVariantClear, PropVariantToUInt16,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, STREAM_SEEK_END, STREAM_SEEK_SET,
};
use windows::core::{GUID, w};

use super::memory::OwnedMemory;
use super::{ClipboardError, MAX_IMAGE_BYTES, MAX_IMAGE_PIXELS, Result};

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

#[derive(Clone, Copy)]
struct ImageSize {
    width: u32,
    height: u32,
}

impl ImageSize {
    fn checked(width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(ClipboardError::InvalidImage);
        }
        if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
            return Err(ClipboardError::ImageTooLarge);
        }
        Ok(Self { width, height })
    }
}

fn check_encoded_size(size: usize) -> Result<()> {
    if size > MAX_IMAGE_BYTES {
        return Err(ClipboardError::ImageTooLarge);
    }
    Ok(())
}

pub(super) fn png_payload(data: &[u8]) -> Result<&[u8]> {
    let Some(header) = data.get(..33) else {
        return Err(ClipboardError::InvalidImage);
    };
    if !header.starts_with(PNG_SIGNATURE) || &header[8..16] != b"\0\0\0\x0dIHDR" {
        return Err(ClipboardError::InvalidImage);
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
            .ok_or(ClipboardError::InvalidImage)?;
        let length = u32::from_be_bytes(chunk[..4].try_into().unwrap()) as u64;
        let end = offset as u64 + length + 12;
        if end > MAX_IMAGE_BYTES as u64 {
            return Err(ClipboardError::ImageTooLarge);
        }
        offset = end as usize;
        if offset > data.len() {
            return Err(ClipboardError::InvalidImage);
        }
        if &chunk[4..8] == b"IEND" {
            if length != 0 {
                return Err(ClipboardError::InvalidImage);
            }
            return Ok(&data[..offset]);
        }
    }
}

pub(super) fn validate_png(png: &[u8]) -> Result<()> {
    decode(png, &GUID_ContainerFormatPng, |_, _, _| Ok(()))
}

pub(super) fn bmp_to_png(bmp: &[u8]) -> Result<Vec<u8>> {
    decode(bmp, &GUID_ContainerFormatBmp, |factory, source, size| {
        let bgra = to_bgra(factory, source)?;
        encode_png(factory, &bgra, size)
    })
}

pub(super) fn prepare_write(encoded: &[u8]) -> Result<(OwnedMemory, OwnedMemory)> {
    check_encoded_size(encoded.len())?;
    let format = detect_container(encoded)?;
    let original_png = if format == GUID_ContainerFormatPng {
        Some(png_payload(encoded)?)
    } else {
        None
    };
    if format == GUID_ContainerFormatBmp {
        let dib = encoded
            .get(BMP_HEADER_BYTES..)
            .ok_or(ClipboardError::InvalidImage)?;
        check_linked_profile(dib)?;
    }

    decode(
        original_png.unwrap_or(encoded),
        &format,
        |factory, source, size| {
            let transform = exif_transform(source, &format);
            let mut rotated_size = size;
            let rotated = if let Some(transform) = transform {
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
            let dib = encode_dib_v5(&bgra, rotated_size)?;
            let png = match original_png {
                Some(png) => OwnedMemory::from_bytes(png)?,
                None => OwnedMemory::from_bytes(&encode_png(factory, &bgra, rotated_size)?)?,
            };
            Ok((dib, png))
        },
    )
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
    } else {
        Err(ClipboardError::InvalidImage)
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

fn decode<T>(
    encoded: &[u8],
    format: &GUID,
    convert: impl FnOnce(&IWICImagingFactory, &IWICBitmapFrameDecode, ImageSize) -> Result<T>,
) -> Result<T> {
    let _apartment = ComApartment::initialize()?;
    // SAFETY: COM is initialized. The stream borrows encoded bytes for
    // reading, and all interfaces using those bytes are dropped here.
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = factory.CreateStream()?;
        stream.InitializeFromMemory(encoded)?;
        let decoder = factory.CreateDecoder(format, null())?;
        decoder.Initialize(&stream, WICDecodeMetadataCacheOnDemand)?;
        let source = decoder.GetFrame(0)?;
        let mut width = 0;
        let mut height = 0;
        source.GetSize(&mut width, &mut height)?;
        convert(&factory, &source, ImageSize::checked(width, height)?)
    }
}

fn to_bgra(
    factory: &IWICImagingFactory,
    source: &IWICBitmapSource,
) -> windows::core::Result<windows::Win32::Graphics::Imaging::IWICFormatConverter> {
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

fn encode_png(
    factory: &IWICImagingFactory,
    source: &IWICBitmapSource,
    size: ImageSize,
) -> Result<Vec<u8>> {
    // SAFETY: The stream is owned by COM, all interfaces are local to this
    // apartment and the output pointers are valid for each WIC call.
    unsafe {
        let stream = CreateStreamOnHGlobal(Default::default(), true)?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, null_mut())?;
        let frame = frame.ok_or(ClipboardError::InvalidImage)?;
        frame.Initialize(None)?;
        frame.SetSize(size.width, size.height)?;
        let mut pixel_format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut pixel_format)?;
        if pixel_format != GUID_WICPixelFormat32bppBGRA {
            return Err(ClipboardError::InvalidImage);
        }
        frame.WriteSource(source, null())?;
        frame.Commit()?;
        encoder.Commit()?;

        let mut length = 0;
        stream.Seek(0, STREAM_SEEK_END, Some(&mut length))?;
        if length > MAX_IMAGE_BYTES as u64 {
            return Err(ClipboardError::ImageTooLarge);
        }
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut png = vec![0; length as usize];
        let mut read = 0;
        stream
            .Read(png.as_mut_ptr().cast(), length as u32, Some(&mut read))
            .ok()?;
        if u64::from(read) != length {
            return Err(ClipboardError::InvalidImage);
        }
        Ok(png)
    }
}

fn encode_dib_v5(source: &IWICBitmapSource, size: ImageSize) -> Result<OwnedMemory> {
    // ImageSize guarantees that width * height * 4 fits in u32.
    let stride = size.width * 4;
    let pixel_bytes = stride * size.height;
    let len = DIB_V5_HEADER_BYTES + pixel_bytes as usize;
    let mut dib = OwnedMemory::new(len)?;
    dib.with_bytes(|data| {
        let (header, pixels) = data.split_at_mut(DIB_V5_HEADER_BYTES);
        header[..4].copy_from_slice(&(DIB_V5_HEADER_BYTES as u32).to_le_bytes());
        header[4..8].copy_from_slice(&size.width.to_le_bytes());
        header[8..12].copy_from_slice(&(-(size.height as i32)).to_le_bytes());
        header[12..14].copy_from_slice(&1u16.to_le_bytes()); // planes
        header[14..16].copy_from_slice(&32u16.to_le_bytes()); // bits per pixel
        header[16..20].copy_from_slice(&BI_BITFIELDS.to_le_bytes());
        header[20..24].copy_from_slice(&pixel_bytes.to_le_bytes());
        header[40..44].copy_from_slice(&0x00ff_0000u32.to_le_bytes()); // red
        header[44..48].copy_from_slice(&0x0000_ff00u32.to_le_bytes()); // green
        header[48..52].copy_from_slice(&0x0000_00ffu32.to_le_bytes()); // blue
        header[52..56].copy_from_slice(&0xff00_0000u32.to_le_bytes()); // alpha
        header[56..60].copy_from_slice(&LCS_SRGB.to_le_bytes());
        header[108..112].copy_from_slice(&LCS_GM_IMAGES.to_le_bytes());

        // SAFETY: The BGRA source writes at most stride * height bytes into
        // this locked, uniquely borrowed pixel buffer. Null requests all rows.
        unsafe { source.CopyPixels(null(), stride, pixels) }
    })??;
    Ok(dib)
}

pub(super) fn dib_as_bmp(dib: &[u8]) -> Result<Vec<u8>> {
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
    let header = data
        .get(..header_bytes)
        .ok_or(ClipboardError::InvalidImage)?;
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
            let width = u32::try_from(width).map_err(|_| ClipboardError::InvalidImage)?;
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
        _ => return Err(ClipboardError::InvalidImage),
    };

    if !matches!(depth, 0 | 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(ClipboardError::InvalidImage);
    }
    let colors = if colors == 0 && matches!(depth, 1 | 4 | 8) {
        1 << depth
    } else {
        colors
    };
    let offset = header_bytes as u64 + masks + u64::from(colors) * entry_bytes;
    let pixel_bytes = if matches!(compression, BI_RGB | BI_BITFIELDS | BI_ALPHABITFIELDS) {
        if depth == 0 {
            return Err(ClipboardError::InvalidImage);
        }
        let stride = (u64::from(size.width) * u64::from(depth)).div_ceil(32) * 4;
        stride * u64::from(size.height)
    } else {
        let encoded_bytes = le_u32(header, 20)?;
        if encoded_bytes == 0 {
            return Err(ClipboardError::InvalidImage);
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
            return Err(ClipboardError::InvalidImage);
        }
        check_encoded_size(profile_bytes as usize)?;
        end = profile_offset + u64::from(profile_bytes);
    }
    if end > MAX_IMAGE_PIXELS * 4 + MAX_IMAGE_BYTES as u64 {
        return Err(ClipboardError::ImageTooLarge);
    }
    if end > data.len() as u64 {
        return Err(ClipboardError::InvalidImage);
    }
    Ok((offset as usize, end as usize))
}

fn check_linked_profile(dib: &[u8]) -> Result<()> {
    // Only inline image data may be passed to the BMP codec. A linked
    // colour profile names a separate file and can refer to a network path.
    if le_u32(dib, 0)? as usize == DIB_V5_HEADER_BYTES && le_u32(dib, 56)? == PROFILE_LINKED {
        return Err(ClipboardError::InvalidImage);
    }
    Ok(())
}

fn le_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or(ClipboardError::InvalidImage)?;
    Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
}

fn le_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or(ClipboardError::InvalidImage)?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}
