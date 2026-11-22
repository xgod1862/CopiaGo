"""Write the current item to Windows without opting into history or cloud sync, and read
what the user copied on Windows for sending to the iPhone."""

from __future__ import annotations

import hashlib
import io
import struct
import time
from dataclasses import dataclass
from typing import Any


# Images come from whatever was copied on the phone, so they are untrusted input to native decoders.
MAX_IMAGE_BYTES = 64 * 1024 * 1024
MAX_IMAGE_PIXELS = 50_000_000  # a 48 MP iPhone photo still fits
IMAGE_FORMATS = ("PNG", "JPEG", "HEIF", "TIFF", "GIF", "WEBP", "BMP")


# Password managers mark what they copy with these; such content never leaves this computer.
SENSITIVE_MARKERS = ("ExcludeClipboardContentFromMonitorProcessing", "Clipboard Viewer Ignore")
OPT_OUT_FLAGS = ("CanIncludeInClipboardHistory", "CanUploadToCloudClipboard")


class ClipTooLarge(ValueError):
    pass


def fingerprint(clip: "Clip") -> str:
    data = clip.file_data or clip.image or (clip.text or "").encode("utf-8")
    return hashlib.sha256(data).hexdigest()


def dib_to_png(dib: bytes) -> bytes:
    """Turn a Windows CF_DIB bitmap into PNG for the iPhone, with the same size limits."""
    from PIL import Image
    image = Image.open(io.BytesIO(dib), formats=("DIB",))
    with image:
        if image.width * image.height > MAX_IMAGE_PIXELS:
            raise ClipTooLarge("图片像素过多")
        output = io.BytesIO()
        image.save(output, format="PNG")
    if output.tell() > MAX_IMAGE_BYTES:
        raise ClipTooLarge("图片数据过大")
    return output.getvalue()


def open_image(encoded: bytes):
    """Open image bytes, refusing oversized data before any pixel is decoded."""
    from PIL import Image
    if len(encoded) > MAX_IMAGE_BYTES:
        raise ClipTooLarge("图片数据过大")
    try:
        import pillow_heif
        pillow_heif.register_heif_opener()
    except ImportError:
        pass
    # Only the formats an iPhone puts on the pasteboard: Pillow's other decoders stay unreachable.
    image = Image.open(io.BytesIO(encoded), formats=IMAGE_FORMATS)
    if image.width * image.height > MAX_IMAGE_PIXELS:
        image.close()
        raise ClipTooLarge("图片像素过多")
    return image


class _SafeWin32Clipboard:
    """Use explicit global memory handles for both text and registered formats."""

    def __init__(self) -> None:
        import ctypes
        import win32clipboard

        self._ctypes = ctypes
        self._api = win32clipboard
        self._kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self._user = ctypes.WinDLL("user32", use_last_error=True)
        self._kernel.GlobalAlloc.argtypes = (ctypes.c_uint, ctypes.c_size_t)
        self._kernel.GlobalAlloc.restype = ctypes.c_void_p
        self._kernel.GlobalLock.argtypes = (ctypes.c_void_p,)
        self._kernel.GlobalLock.restype = ctypes.c_void_p
        self._kernel.GlobalUnlock.argtypes = (ctypes.c_void_p,)
        self._kernel.GlobalUnlock.restype = ctypes.c_int
        self._kernel.GlobalFree.argtypes = (ctypes.c_void_p,)
        self._kernel.GlobalFree.restype = ctypes.c_void_p
        self._user.SetClipboardData.argtypes = (ctypes.c_uint, ctypes.c_void_p)
        self._user.SetClipboardData.restype = ctypes.c_void_p

    def __getattr__(self, name: str) -> Any:
        return getattr(self._api, name)

    def SetClipboardData(self, format_id: int, data: bytes | str) -> None:
        ctypes = self._ctypes
        if isinstance(data, str):
            if format_id != self._api.CF_UNICODETEXT:
                raise ValueError("Unexpected text format")
            payload = data.encode("utf-16-le") + b"\0\0"
        else:
            payload = bytes(data)
        if not payload:
            raise ValueError("Clipboard format cannot be empty")
        handle = self._kernel.GlobalAlloc(0x0042, len(payload))  # GHND = movable, zeroed
        if not handle:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            pointer = self._kernel.GlobalLock(handle)
            if not pointer:
                raise ctypes.WinError(ctypes.get_last_error())
            try:
                ctypes.memmove(pointer, payload, len(payload))
            finally:
                self._kernel.GlobalUnlock(handle)
            if not self._user.SetClipboardData(format_id, handle):
                raise ctypes.WinError(ctypes.get_last_error())
            handle = None  # Windows now owns the allocation.
        finally:
            if handle:
                self._kernel.GlobalFree(handle)


@dataclass(frozen=True)
class Clip:
    text: str | None = None
    image: bytes | None = None
    file_name: str | None = None  # a file copied in the Files app, with its original name
    file_data: bytes | None = None

    @property
    def kind(self) -> str:
        return "file" if self.file_data is not None else "image" if self.image else "text"


class ClipboardWriter:
    """Writes and reads only the current item; no item or history is stored by this class."""

    def __init__(self, owner_hwnd: int, api: Any | None = None) -> None:
        if api is None:
            api = _SafeWin32Clipboard()
        self.api = api
        self.owner_hwnd = owner_hwnd
        self.exclude_format = api.RegisterClipboardFormat("ExcludeClipboardContentFromMonitorProcessing")
        self.history_format = api.RegisterClipboardFormat("CanIncludeInClipboardHistory")
        self.cloud_format = api.RegisterClipboardFormat("CanUploadToCloudClipboard")
        self.png_format = api.RegisterClipboardFormat("PNG")
        self.sequence: int | None = None
        if not all((self.exclude_format, self.history_format, self.cloud_format, self.png_format)):
            raise RuntimeError("注册系统剪贴板格式失败")

    def put(self, clip: Clip) -> str:
        # Decode before opening or clearing the Windows clipboard.
        dib, png = self._convert_image(clip.image) if clip.image else (None, None)
        if dib is None and clip.text is None:
            raise ValueError("没有可同步的文本或图片")
        if clip.text is not None and "\x00" in clip.text:
            raise ValueError("文本包含 Windows 剪贴板无法表示的空字符")

        self._open()
        try:
            self.api.EmptyClipboard()
            # Apply privacy formats before the content. On failure, do not add content.
            self.api.SetClipboardData(self.exclude_format, b"1")
            self.api.SetClipboardData(self.history_format, struct.pack("<I", 0))
            self.api.SetClipboardData(self.cloud_format, struct.pack("<I", 0))
            if dib is not None:
                self.api.SetClipboardData(self.api.CF_DIB, dib)
                self.api.SetClipboardData(self.png_format, png)
            elif clip.text is not None:
                self.api.SetClipboardData(self.api.CF_UNICODETEXT, clip.text)
            return "图片" if dib is not None else "文本"
        finally:
            self.api.CloseClipboard()
            self.sequence = self._current_sequence()

    def put_files(self, paths: list) -> str:
        """Put files on the Windows clipboard the way Explorer's Copy does (CF_HDROP)."""
        names = "".join(str(path) + "\0" for path in paths) + "\0"
        # DROPFILES: offset of the list, a point, "not client", "wide" — then UTF-16 paths.
        dropfiles = struct.pack("<IiiII", 20, 0, 0, 0, 1) + names.encode("utf-16-le")
        drop_effect = self.api.RegisterClipboardFormat("Preferred DropEffect")
        self._open()
        try:
            self.api.EmptyClipboard()
            self.api.SetClipboardData(self.exclude_format, b"1")
            self.api.SetClipboardData(self.history_format, struct.pack("<I", 0))
            self.api.SetClipboardData(self.cloud_format, struct.pack("<I", 0))
            self.api.SetClipboardData(self.api.CF_HDROP, dropfiles)
            self.api.SetClipboardData(drop_effect, struct.pack("<I", 1))  # DROPEFFECT_COPY
            return "文件"
        finally:
            self.api.CloseClipboard()
            self.sequence = self._current_sequence()

    def _open(self) -> None:
        for attempt in range(8):
            try:
                self.api.OpenClipboard(self.owner_hwnd)
                return
            except Exception:
                if attempt == 7:
                    raise RuntimeError("Windows 剪贴板正被其他程序占用") from None
                time.sleep(0.05 * (attempt + 1))

    def read(self) -> tuple[Clip | None, bool]:
        """The Windows clipboard as (clip, sensitive). Sensitive content is never returned.

        This app's own writes carry the same markers, so they are never sent back to the phone.
        """
        api = self.api
        self._open()
        try:
            for name in SENSITIVE_MARKERS:
                if api.IsClipboardFormatAvailable(api.RegisterClipboardFormat(name)):
                    return None, True
            for name in OPT_OUT_FLAGS:
                format_id = api.RegisterClipboardFormat(name)
                if api.IsClipboardFormatAvailable(format_id):
                    value = bytes(api.GetClipboardData(format_id))
                    if len(value) >= 4 and struct.unpack("<I", value[:4])[0] == 0:
                        return None, True
            if api.IsClipboardFormatAvailable(self.png_format):
                png, dib = bytes(api.GetClipboardData(self.png_format)), None
            elif api.IsClipboardFormatAvailable(api.CF_DIB):
                png, dib = None, bytes(api.GetClipboardData(api.CF_DIB))
            elif api.IsClipboardFormatAvailable(api.CF_UNICODETEXT):
                text = api.GetClipboardData(api.CF_UNICODETEXT)
                return (Clip(text=text) if text and text.strip() else None), False
            else:
                return None, False
        finally:
            api.CloseClipboard()
        # Convert outside the clipboard lock so other programs are not kept waiting.
        if png is not None:
            if len(png) > MAX_IMAGE_BYTES:
                raise ClipTooLarge("图片数据过大")
            with open_image(png):
                pass  # checks format and size
            return Clip(image=png), False
        return Clip(image=dib_to_png(dib)), False

    def _current_sequence(self) -> int | None:
        read = getattr(self.api, "GetClipboardSequenceNumber", None)
        return read() if read else None

    def changed_elsewhere(self) -> bool:
        """Whether another program wrote the Windows clipboard after this writer last did."""
        if self.sequence is None:
            return False
        return self._current_sequence() != self.sequence

    @staticmethod
    def _convert_image(encoded: bytes) -> tuple[bytes, bytes]:
        from PIL import ImageOps
        with open_image(encoded) as image:
            image.load()
            image = ImageOps.exif_transpose(image)
            # CF_DIB uses a BMP payload without the 14-byte file header.
            dib_stream = io.BytesIO()
            image.convert("RGB").save(dib_stream, format="BMP")
            png_stream = io.BytesIO()
            image.save(png_stream, format="PNG")
            return dib_stream.getvalue()[14:], png_stream.getvalue()
