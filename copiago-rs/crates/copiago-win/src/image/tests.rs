use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

use windows::Win32::Graphics::Imaging::GUID_WICPixelFormat24bppBGR;
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::*;

#[test]
fn png_and_dib_v5_preserve_straight_alpha() {
    let pixels = [11, 22, 33, 0, 90, 70, 50, 128, 241, 107, 17, 255];
    let image = BgraImage::from_bgra(3, 1, pixels.to_vec()).unwrap();
    let png = encode_png(&image).unwrap();
    let decoded = decode(&png).unwrap();
    assert_eq!(decoded.dimensions(), (3, 1));
    assert_eq!(decoded.pixels(), pixels);

    let dib = png_to_dib_v5(&png).unwrap();
    assert_eq!(&dib[DIB_V5_HEADER_BYTES..], pixels);
    let decoded = decode(&dib_to_png(&dib).unwrap()).unwrap();
    assert_eq!(decoded.dimensions(), (3, 1));
    assert_eq!(decoded.pixels(), pixels);
}

#[test]
fn scaled_decode_fits_within_bounds_without_enlarging() {
    let image = BgraImage::from_bgra(5, 4, [163, 28, 20, 255].repeat(20)).unwrap();
    let png = encode_png(&image).unwrap();
    let scaled = decode_scaled(&png, 3, 3).unwrap();
    assert_eq!(scaled.dimensions(), (3, 2));
    assert_eq!(scaled.pixels(), [163, 28, 20, 255].repeat(6));

    let unchanged = decode_scaled(&png, 8, 9).unwrap();
    assert_eq!(unchanged.dimensions(), image.dimensions());
    assert_eq!(unchanged.pixels(), image.pixels());
    assert!(matches!(
        decode_scaled(&png, 0, 9),
        Err(ImageError::InvalidImage)
    ));
}

#[test]
fn scaled_decode_checks_original_pixel_count() {
    let image = BgraImage::from_bgra(1, 1, vec![0; 4]).unwrap();
    let mut png = encode_png(&image).unwrap();
    // Change the IHDR size. The pixel count is checked before WIC opens the image.
    png[16..20].copy_from_slice(&10_001u32.to_be_bytes());
    png[20..24].copy_from_slice(&5_000u32.to_be_bytes());
    assert!(matches!(
        decode_scaled(&png, 96, 96),
        Err(ImageError::ImageTooLarge)
    ));
    assert!(matches!(
        BgraImage::from_bgra(1, 2, vec![0; 4]),
        Err(ImageError::InvalidImage)
    ));
}

#[test]
fn decodes_indexed_gif_and_handles_an_optional_webp_codec() {
    const GIF: &[u8] = b"GIF89a\x02\0\x01\0\x81\0\0\x0b\x16\x21\x2c\x37\x42\0\0\0\0\0\0\
        \x21\xf9\x04\x01\0\0\0\0\x2c\0\0\0\0\x02\0\x01\0\0\x08\x05\0\x01\x04\x08\x08\0\x3b";
    let gif = decode(GIF).unwrap();
    assert_eq!(gif.dimensions(), (2, 1));
    assert_eq!(gif.pixels()[3], 0);
    assert_eq!(gif.pixels()[4..], [66, 55, 44, 255]);

    const WEBP: &[u8] = b"RIFF\x1e\0\0\0WEBPVP8L\x11\0\0\0\x2f\0\0\0\0\
        \x07\xd0\xb5\x46\x34\xbe\xff\x81\x88\xe8\x7f\0\0";
    match decode(WEBP) {
        Ok(webp) => {
            assert_eq!(webp.dimensions(), (1, 1));
            assert_eq!(webp.pixels(), [241, 107, 17, 255]);
        }
        Err(ImageError::Codec(code))
            if code == windows::Win32::Foundation::WINCODEC_ERR_COMPONENTNOTFOUND.0 =>
        {
            println!("WebP codec unavailable ({code:#010x})");
        }
        Err(error) => panic!("unexpected WIC error for a valid WebP: {error}"),
    }
}

#[test]
fn scaled_png_respects_alpha() {
    let pixels = vec![255, 0, 0, 0, 0, 0, 255, 255];
    let png = encode_png(&BgraImage::from_bgra(2, 1, pixels).unwrap()).unwrap();
    let preview = decode_scaled(&png, 1, 1).unwrap();
    assert_eq!(preview.dimensions(), (1, 1));
    assert_eq!(&preview.pixels()[..3], &[0, 0, 255]);
    assert!((127..=128).contains(&preview.pixels()[3]));
}

const THUMBNAIL_CHILD: &str = "COPIAGO_WIC_THUMBNAIL_CHILD";
const JPEG_WIDTH: u32 = 8_000;
const JPEG_HEIGHT: u32 = 6_000;
const PEAK_LIMIT_BYTES: usize = 30_000_000;

#[test]
fn jpeg_thumbnail_uses_bounded_memory() {
    if std::env::var_os(THUMBNAIL_CHILD).is_some() {
        measure_thumbnail();
        return;
    }

    let jpeg = synthetic_jpeg().expect("generate synthetic 48 MP JPEG");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "image::tests::jpeg_thumbnail_uses_bounded_memory",
            "--nocapture",
        ])
        .env(THUMBNAIL_CHILD, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Generation stays in the parent, so its encoder allocations cannot
    // contribute to the child's lifetime high-water marks.
    let send = child.stdin.take().unwrap().write_all(&jpeg);
    let output = child.wait_with_output().unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && report.contains("peak_working_set_bytes="),
        "{report}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    send.unwrap();
    print!("{report}");
}

fn measure_thumbnail() {
    let mut jpeg = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut jpeg)
        .unwrap();
    let started = Instant::now();
    let preview = decode_scaled(&jpeg, 96, 96).expect("decode 48 MP JPEG to a thumbnail");
    let decode_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let png = encode_png(&preview).expect("encode the thumbnail to PNG");
    let total_ms = started.elapsed().as_secs_f64() * 1_000.0;

    assert_eq!(preview.dimensions(), (96, 72));
    assert_eq!(preview.pixels().len(), 96 * 72 * 4);
    let middle = &preview.pixels()[36 * 96 * 4..];
    let red = &middle[24 * 4..25 * 4];
    let green = &middle[72 * 4..73 * 4];
    assert!(red[2] > 160 && red[1] < 100 && red[3] == 255);
    assert!(green[1] > 160 && green[2] < 100 && green[3] == 255);

    let counters = memory_peaks();
    let working_set = counters.PeakWorkingSetSize;
    let private_commit = counters.PeakPagefileUsage;
    println!(
        "input={}x{} jpeg_bytes={} thumbnail=96x72 png_bytes={} decode_ms={decode_ms:.2} \
         total_ms={total_ms:.2} peak_working_set_bytes={working_set} \
         peak_private_commit_bytes={private_commit}",
        JPEG_WIDTH,
        JPEG_HEIGHT,
        jpeg.len(),
        png.len(),
    );
    assert!((1..PEAK_LIMIT_BYTES).contains(&working_set));
    assert!((1..PEAK_LIMIT_BYTES).contains(&private_commit));
}

fn memory_peaks() -> PROCESS_MEMORY_COUNTERS {
    let len = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: len,
        // SAFETY: All fields are integers, for which zero is a valid value.
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: The pseudo-handle names this process and counters is writable for len bytes.
    let success = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, len) };
    assert_ne!(success, 0, "{}", std::io::Error::last_os_error());
    counters
}

fn synthetic_jpeg() -> Result<Vec<u8>> {
    const ROWS_PER_WRITE: u32 = 8;
    let stride = JPEG_WIDTH * 3;
    let mut rows = vec![0; (stride * ROWS_PER_WRITE) as usize];
    let (pixels, _) = rows.as_chunks_mut::<3>();
    for (index, pixel) in pixels.iter_mut().enumerate() {
        let x = index as u32 % JPEG_WIDTH;
        let shade = ((x / 16) % 8 * 6) as u8;
        *pixel = if x < JPEG_WIDTH / 2 {
            [24, 45 + shade, 220]
        } else {
            [30, 200, 24 + shade]
        };
    }
    with_factory(|factory| {
        // SAFETY: All COM objects are used in the active apartment. Each
        // WritePixels call provides exactly ROWS_PER_WRITE valid BGR rows.
        unsafe {
            let stream = CreateStreamOnHGlobal(Default::default(), true)?;
            let encoder = factory.CreateEncoder(&GUID_ContainerFormatJpeg, null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut frame = None;
            encoder.CreateNewFrame(&mut frame, null_mut())?;
            let frame = frame.unwrap();
            frame.Initialize(None)?;
            frame.SetSize(JPEG_WIDTH, JPEG_HEIGHT)?;
            let mut format = GUID_WICPixelFormat24bppBGR;
            frame.SetPixelFormat(&mut format)?;
            assert_eq!(format, GUID_WICPixelFormat24bppBGR);
            for _ in 0..JPEG_HEIGHT / ROWS_PER_WRITE {
                frame.WritePixels(ROWS_PER_WRITE, stride, &rows)?;
            }
            frame.Commit()?;
            encoder.Commit()?;
            read_encoded_stream(&stream)
        }
    })
}
