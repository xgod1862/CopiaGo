use std::ffi::OsString;
use std::mem::MaybeUninit;
use std::os::windows::ffi::OsStringExt;
use std::ptr::null;
use std::time::Instant;

use windows_sys::Win32::UI::Shell::DragQueryFileW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW,
};

use super::*;

const TEXT: &str = "剪贴板\r\nCopiaGo 📋";
const PNG: &[u8] = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\
    \x00\x02\x00\x00\x00\x02\x08\x06\x00\x00\x00\x72\xb6\x0d\x24\x00\x00\x00\
    \x1d\x49\x44\x41\x54\x78\x01\x01\x12\x00\xed\xff\x00\x00\x00\x00\x00\x5b\
    \x59\x51\x80\x00\x00\x11\xff\xff\x15\x8f\x00\x4b\x1f\x10\x04\x84\x87\x36\
    \x5a\xc9\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";
const DIB_V5_HEADER_BYTES: usize = 124;
const PIXELS: &[u8] = &[0, 0, 0, 0, 81, 89, 91, 128, 255, 17, 0, 255, 0, 143, 21, 75];

#[test]
fn unicode_text_and_dropfiles_use_windows_layout() {
    let mut text = encode_text(TEXT).unwrap();
    text.extend_from_slice(&[1, 0]); // bytes after the terminator are ignored
    assert!(matches!(read_text(&text), Ok(ReadData::Text(value)) if value == TEXT));
    assert!(matches!(
        read_text(&[0x00, 0xd8, 0, 0]),
        Err(ClipboardError::InvalidText)
    ));
    assert!(matches!(
        read_text(&[1, 0]),
        Err(ClipboardError::InvalidText)
    ));
    assert!(matches!(
        encode_text("a\0b"),
        Err(ClipboardError::InvalidText)
    ));
    assert!(matches!(
        read_text(&encode_text(" \t\r\n").unwrap()),
        Ok(ReadData::Empty)
    ));

    let paths = test_paths();
    let list = encode_file_list(&paths).unwrap();
    assert_eq!(
        &list[..20],
        &[20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]
    );
    let expected = paths
        .iter()
        .flat_map(|path| path.as_os_str().encode_wide().chain([0]))
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    assert!(list[20..] == expected);
    assert!(matches!(
        encode_file_list(&[]),
        Err(ClipboardError::InvalidFileList)
    ));
    assert!(matches!(
        encode_file_list(&[PathBuf::from("bad\0path")]),
        Err(ClipboardError::InvalidFileList)
    ));
}

#[test]
fn image_limits_and_padding() {
    let mut padded = PNG.to_vec();
    padded.extend_from_slice(&[0; 7]);
    assert!(image::png_payload(&padded).is_ok_and(|data| data == PNG));
    assert!(matches!(
        image::png_payload(&PNG[..PNG.len() - 1]),
        Err(ImageError::InvalidImage)
    ));

    let mut max_pixels = PNG.to_vec();
    max_pixels[16..20].copy_from_slice(&10_000u32.to_be_bytes());
    max_pixels[20..24].copy_from_slice(&5_000u32.to_be_bytes());
    // The envelope reader checks dimensions before handing the stream to WIC.
    assert!(image::png_payload(&max_pixels).is_ok());
    assert!(matches!(
        prepare_image_write(&oversized_png()),
        Err(ClipboardError::ImageTooLarge)
    ));

    let too_many_bytes = vec![0; MAX_IMAGE_BYTES + 1];
    assert!(matches!(
        prepare_image_write(&too_many_bytes),
        Err(ClipboardError::ImageTooLarge)
    ));

    let mut oversized_length = PNG.to_vec();
    oversized_length[33..37].copy_from_slice(&(MAX_IMAGE_BYTES as u32).to_be_bytes());
    assert!(matches!(
        image::png_payload(&oversized_length),
        Err(ImageError::ImageTooLarge)
    ));

    let mut large_bmp = dib24();
    large_bmp[4..8].copy_from_slice(&10_001i32.to_le_bytes());
    large_bmp[8..12].copy_from_slice(&5_000i32.to_le_bytes());
    assert!(matches!(
        image::dib_as_bmp(&large_bmp),
        Err(ImageError::ImageTooLarge)
    ));
}

#[test]
fn wic_preserves_alpha_in_png_and_dib_v5() {
    let (mut dib, mut png) = prepare_image_write(PNG).unwrap();
    png.with_bytes(|data| assert!(*data == *PNG)).unwrap();
    let bmp = dib
        .with_bytes(|data| {
            assert_dib_pixels(data);
            image::dib_as_bmp(data)
        })
        .unwrap()
        .unwrap();
    let decoded = image::bmp_to_png(&bmp).unwrap();
    assert_png_pixels(&decoded, PIXELS);
    let decoded = image::bmp_to_png(&image::dib_as_bmp(&dib24()).unwrap()).unwrap();
    assert_png_pixels(&decoded, &[9, 8, 7, 255, 6, 5, 4, 255]);
}

#[test]
fn wic_applies_tiff_orientation_to_both_formats() {
    let tiff = tiff_with_orientation(5);
    let preview = image::decode_scaled(&tiff, 88, 56).unwrap();
    assert_eq!(preview.dimensions(), (2, 1)); // Python history uses stored orientation.
    let (mut dib, mut png) = prepare_image_write(&tiff).unwrap();
    let expected = &[3, 2, 1, 255, 6, 5, 4, 255];
    dib.with_bytes(|data| {
        assert!(data[4..8] == 1u32.to_le_bytes());
        assert!(data[8..12] == (-2i32).to_le_bytes());
        assert!(data[DIB_V5_HEADER_BYTES..] == *expected);
    })
    .unwrap();
    png.with_bytes(|data| assert_png_pixels(data, expected))
        .unwrap();
}

// Two opaque RGB pixels in one row. Orientation 5 transposes the row to a column.
fn tiff_with_orientation(orientation: u32) -> Vec<u8> {
    const TAG_COUNT: u16 = 11;
    const BITS_OFFSET: u32 = 8 + 2 + 12 * TAG_COUNT as u32 + 4;
    const PIXEL_OFFSET: u32 = BITS_OFFSET + 6;
    let tags: [(u16, u16, u32, u32); TAG_COUNT as usize] = [
        (256, 4, 1, 2),            // ImageWidth
        (257, 4, 1, 1),            // ImageLength
        (258, 3, 3, BITS_OFFSET),  // BitsPerSample
        (259, 3, 1, 1),            // Compression
        (262, 3, 1, 2),            // PhotometricInterpretation: RGB
        (273, 4, 1, PIXEL_OFFSET), // StripOffsets
        (274, 3, 1, orientation),
        (277, 3, 1, 3), // SamplesPerPixel
        (278, 4, 1, 1), // RowsPerStrip
        (279, 4, 1, 6), // StripByteCounts
        (284, 3, 1, 1), // PlanarConfiguration
    ];
    let mut data = b"II*\0\x08\0\0\0".to_vec();
    data.extend_from_slice(&TAG_COUNT.to_le_bytes());
    for (tag, kind, count, value) in tags {
        data.extend_from_slice(&tag.to_le_bytes());
        data.extend_from_slice(&kind.to_le_bytes());
        data.extend_from_slice(&count.to_le_bytes());
        data.extend_from_slice(&value.to_le_bytes());
    }
    data.extend_from_slice(&[0; 4]); // next IFD
    data.extend_from_slice(&[8, 0, 8, 0, 8, 0]);
    data.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
    data
}

#[test]
#[ignore = "replaces the Windows clipboard on the test session"]
fn native_clipboard_round_trip() {
    let window = TestWindow::new();
    let external = TestWindow::new();
    let clipboard = Clipboard::new(window.0).expect("register native Windows clipboard listener");

    let before = clipboard.sequence_number();
    clipboard
        .write_text(TEXT)
        .expect("write native CF_UNICODETEXT");
    assert_ne!(before, clipboard.sequence_number());
    window.wait_for_update();
    assert!(matches!(clipboard.read(), Ok(ClipboardRead::Sensitive)));
    OpenClipboardGuard::run(external.0, |open| {
        assert!(open.available(clipboard.formats.excluded));
        for flag in [clipboard.formats.history, clipboard.formats.cloud] {
            open.with_data(flag, |data| {
                assert!(data.starts_with(&[0; 4]));
                Ok(())
            })?;
        }
        open.with_data(CF_UNICODETEXT.into(), |data| {
            assert!(matches!(read_text(data), Ok(ReadData::Text(text)) if text == TEXT));
            Ok(())
        })
    })
    .expect("read written text and privacy markers from native clipboard");

    replace(
        external.0,
        [(
            CF_UNICODETEXT.into(),
            OwnedMemory::from_bytes(&encode_text(TEXT).unwrap()).unwrap(),
        )],
    );
    assert!(matches!(clipboard.read(), Ok(ClipboardRead::Text(text)) if text == TEXT));

    let conversion = Instant::now();
    clipboard
        .write_image(PNG)
        .expect("write native DIBV5 and PNG");
    let write_png = conversion.elapsed();
    assert!(matches!(clipboard.read(), Ok(ClipboardRead::Sensitive)));
    let dib = OpenClipboardGuard::run(external.0, |open| {
        open.with_data(clipboard.formats.png, |data| {
            assert!(image::png_payload(data)? == PNG);
            Ok(())
        })?;
        assert_eq!(open.bitmap_format()?, Some(CF_DIBV5.into()));
        assert!(open.available(CF_DIB.into())); // synthesized for legacy consumers
        open.with_data(CF_DIBV5.into(), |data| {
            assert_dib_pixels(data);
            Ok(OwnedMemory::from_bytes(data)?)
        })
    })
    .expect("read written PNG and DIBV5 from native clipboard");

    replace(external.0, [(CF_DIBV5.into(), dib)]);
    let ClipboardRead::Png(png) = clipboard.read().expect("read native DIBV5-only bitmap") else {
        panic!("DIBV5 clipboard did not return PNG");
    };
    assert_png_pixels(&png, PIXELS);

    replace(
        external.0,
        [(CF_DIB.into(), OwnedMemory::from_bytes(&dib24()).unwrap())],
    );
    let ClipboardRead::Png(png) = clipboard.read().expect("read native DIB-only bitmap") else {
        panic!("DIB clipboard did not return PNG");
    };
    assert_png_pixels(&png, &[9, 8, 7, 255, 6, 5, 4, 255]);

    // An external PNG must win even if there is a DIB and text.
    replace(
        external.0,
        [
            (
                CF_UNICODETEXT.into(),
                OwnedMemory::from_bytes(&encode_text(TEXT).unwrap()).unwrap(),
            ),
            (CF_DIB.into(), OwnedMemory::from_bytes(&dib24()).unwrap()),
            (clipboard.formats.png, OwnedMemory::from_bytes(PNG).unwrap()),
        ],
    );
    assert!(matches!(clipboard.read(), Ok(ClipboardRead::Png(png)) if png == PNG));

    let paths = test_paths();
    clipboard
        .write_files(&paths)
        .expect("write native CF_HDROP");
    assert!(matches!(clipboard.read(), Ok(ClipboardRead::Sensitive)));
    OpenClipboardGuard::run(external.0, |open| {
        let handle = retry(|| {
            // SAFETY: The clipboard is open and CF_HDROP must belong to it.
            let data = unsafe { GetClipboardData(CF_HDROP.into()) };
            if data.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(data)
            }
        })?;
        // SAFETY: The live CF_HDROP handle stays owned by the open clipboard.
        let count = unsafe { DragQueryFileW(handle, u32::MAX, null_mut(), 0) };
        assert_eq!(count as usize, paths.len());
        for (index, expected) in paths.iter().enumerate() {
            // SAFETY: A null buffer asks for the required length.
            let length = unsafe { DragQueryFileW(handle, index as u32, null_mut(), 0) };
            let mut buffer = vec![0u16; length as usize + 1];
            // SAFETY: The writable buffer has room for length plus the terminator.
            let copied =
                unsafe { DragQueryFileW(handle, index as u32, buffer.as_mut_ptr(), length + 1) };
            assert_eq!(copied, length);
            let path = OsString::from_wide(&buffer[..length as usize]);
            assert!(expected.as_os_str() == path);
        }
        open.with_data(clipboard.formats.drop_effect, |data| {
            assert!(data.starts_with(&DROPEFFECT_COPY.to_le_bytes()));
            Ok(())
        })
    })
    .expect("verify native file list using the Shell drop-list API");

    let before = clipboard.sequence_number();
    assert!(matches!(
        clipboard.write_image(&oversized_png()),
        Err(ClipboardError::ImageTooLarge)
    ));
    assert_eq!(before, clipboard.sequence_number());

    replace(
        external.0,
        [(
            clipboard.formats.png,
            OwnedMemory::from_bytes(&oversized_png()).unwrap(),
        )],
    );
    assert!(matches!(
        clipboard.read(),
        Err(ClipboardError::ImageTooLarge)
    ));
    replace(
        external.0,
        [
            (
                clipboard.formats.viewer_ignore,
                OwnedMemory::from_bytes(b"1").unwrap(),
            ),
            (
                clipboard.formats.png,
                OwnedMemory::from_bytes(&oversized_png()).unwrap(),
            ),
        ],
    );
    assert!(matches!(clipboard.read(), Ok(ClipboardRead::Sensitive)));

    eprintln!(
        "Native clipboard text, PNG, DIBV5, DIB, files, markers, limits and notifications verified. PNG write: {write_png:?}."
    );
}

fn replace(owner: HWND, formats: impl IntoIterator<Item = (u32, OwnedMemory)>) {
    OpenClipboardGuard::run(owner, |open| {
        open.empty()?;
        for (format, data) in formats {
            open.set(format, data)?;
        }
        Ok(())
    })
    .expect("replace clipboard as an external Windows application");
}

fn test_paths() -> [PathBuf; 2] {
    [
        PathBuf::from(r"C:\CopiaGo-clipboard-test\照片 🚀.png"),
        PathBuf::from(r"C:\CopiaGo-clipboard-test\notes.txt"),
    ]
}

fn oversized_png() -> Vec<u8> {
    let mut data = PNG.to_vec();
    data[16..20].copy_from_slice(&10_001u32.to_be_bytes());
    data[20..24].copy_from_slice(&5_000u32.to_be_bytes());
    data
}

fn dib24() -> Vec<u8> {
    let mut bytes = vec![0; 48];
    bytes[..4].copy_from_slice(&40u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&2i32.to_le_bytes());
    bytes[8..12].copy_from_slice(&1i32.to_le_bytes());
    bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
    bytes[14..16].copy_from_slice(&24u16.to_le_bytes());
    bytes[40..48].copy_from_slice(&[9, 8, 7, 6, 5, 4, 0, 0]);
    bytes
}

fn assert_png_pixels(png: &[u8], expected: &[u8]) {
    let (mut dib, _) = prepare_image_write(png).expect("decode PNG for pixel validation");
    dib.with_bytes(|data| {
        assert!(data.get(DIB_V5_HEADER_BYTES..) == Some(expected));
    })
    .unwrap();
}

fn assert_dib_pixels(data: &[u8]) {
    assert!(data.starts_with(&(DIB_V5_HEADER_BYTES as u32).to_le_bytes()));
    assert!(data.get(DIB_V5_HEADER_BYTES..DIB_V5_HEADER_BYTES + PIXELS.len()) == Some(PIXELS));
}

struct TestWindow(HWND);

impl TestWindow {
    fn new() -> Self {
        // SAFETY: STATIC is a built-in window class. HWND_MESSAGE creates a
        // message-only child on this thread; the optional pointers are null.
        let handle = unsafe {
            CreateWindowExW(
                0,
                w!("STATIC"),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                null_mut(),
                null(),
            )
        };
        assert!(
            !handle.is_null(),
            "create native Windows clipboard test window: {}",
            io::Error::last_os_error()
        );
        Self(handle)
    }

    fn wait_for_update(&self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut message = MaybeUninit::<MSG>::uninit();
        while Instant::now() < deadline {
            // SAFETY: The window belongs to this thread and message is a writable MSG.
            let pending = unsafe {
                PeekMessageW(
                    message.as_mut_ptr(),
                    self.0,
                    WM_CLIPBOARDUPDATE,
                    WM_CLIPBOARDUPDATE,
                    PM_REMOVE,
                )
            };
            if pending != 0 {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("native Windows clipboard listener did not receive WM_CLIPBOARDUPDATE");
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        // SAFETY: The test owns the live window on this thread.
        unsafe { DestroyWindow(self.0) };
    }
}
