"""Per-pixel-alpha windows for rounded corners and soft shadows, which plain Tk cannot draw.

The window's pixels come from a Pillow image via UpdateLayeredWindow; Tk still owns the
window, so mouse and keyboard bindings on ``LayeredWindow.top`` work as usual.
"""
from __future__ import annotations

import ctypes
from ctypes import wintypes
import tkinter as tk

from PIL import Image

_user32 = ctypes.WinDLL("user32", use_last_error=True)
_gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)

GWL_EXSTYLE = -20
WS_EX_TRANSPARENT = 0x00000020
WS_EX_TOOLWINDOW = 0x00000080
WS_EX_LAYERED = 0x00080000
WS_EX_NOACTIVATE = 0x08000000
ULW_ALPHA = 0x02
SWP_NOSIZE, SWP_NOMOVE, SWP_NOACTIVATE = 0x0001, 0x0002, 0x0010


class _BitmapInfoHeader(ctypes.Structure):
    _fields_ = [("biSize", wintypes.DWORD), ("biWidth", wintypes.LONG), ("biHeight", wintypes.LONG),
                ("biPlanes", wintypes.WORD), ("biBitCount", wintypes.WORD), ("biCompression", wintypes.DWORD),
                ("biSizeImage", wintypes.DWORD), ("biXPelsPerMeter", wintypes.LONG),
                ("biYPelsPerMeter", wintypes.LONG), ("biClrUsed", wintypes.DWORD),
                ("biClrImportant", wintypes.DWORD)]


class _BlendFunction(ctypes.Structure):
    _fields_ = [("BlendOp", ctypes.c_ubyte), ("BlendFlags", ctypes.c_ubyte),
                ("SourceConstantAlpha", ctypes.c_ubyte), ("AlphaFormat", ctypes.c_ubyte)]


_user32.GetParent.argtypes = (wintypes.HWND,)
_user32.GetParent.restype = wintypes.HWND
_user32.GetWindowLongW.argtypes = (wintypes.HWND, ctypes.c_int)
_user32.GetWindowLongW.restype = wintypes.LONG
_user32.SetWindowLongW.argtypes = (wintypes.HWND, ctypes.c_int, wintypes.LONG)
_user32.SetWindowLongW.restype = wintypes.LONG
_user32.GetDC.argtypes = (wintypes.HWND,)
_user32.GetDC.restype = wintypes.HDC
_user32.ReleaseDC.argtypes = (wintypes.HWND, wintypes.HDC)
_user32.SetWindowPos.argtypes = (wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int,
                                 ctypes.c_int, wintypes.UINT)
_user32.UpdateLayeredWindow.argtypes = (wintypes.HWND, wintypes.HDC, ctypes.POINTER(wintypes.POINT),
                                        ctypes.POINTER(wintypes.SIZE), wintypes.HDC,
                                        ctypes.POINTER(wintypes.POINT), wintypes.COLORREF,
                                        ctypes.POINTER(_BlendFunction), wintypes.DWORD)
_user32.UpdateLayeredWindow.restype = wintypes.BOOL
_gdi32.CreateCompatibleDC.argtypes = (wintypes.HDC,)
_gdi32.CreateCompatibleDC.restype = wintypes.HDC
_gdi32.CreateDIBSection.argtypes = (wintypes.HDC, ctypes.c_void_p, wintypes.UINT,
                                    ctypes.POINTER(ctypes.c_void_p), wintypes.HANDLE, wintypes.DWORD)
_gdi32.CreateDIBSection.restype = wintypes.HBITMAP
_gdi32.SelectObject.argtypes = (wintypes.HDC, wintypes.HGDIOBJ)
_gdi32.SelectObject.restype = wintypes.HGDIOBJ
_gdi32.DeleteObject.argtypes = (wintypes.HGDIOBJ,)
_gdi32.DeleteDC.argtypes = (wintypes.HDC,)


class LayeredWindow:
    """A borderless, topmost window whose every pixel, including alpha, comes from an image."""

    def __init__(self, master: tk.Misc, *, click_through: bool = False) -> None:
        self.top = tk.Toplevel(master)
        self.top.withdraw()
        self.top.overrideredirect(True)
        self.top.attributes("-topmost", True)
        self.extra_style = WS_EX_LAYERED | WS_EX_TOOLWINDOW
        if click_through:
            self.extra_style |= WS_EX_TRANSPARENT | WS_EX_NOACTIVATE
        self.hwnd = None
        self.position = (0, 0)
        self.size = (0, 0)
        self.visible = False

    def present(self, image: Image.Image, x: int, y: int) -> None:
        """Show the image with its top-left corner at screen pixel (x, y)."""
        width, height = image.size
        if not self.visible:
            # Tk may recreate the native window when mapping it, so map off-screen, then make it
            # layered; a layered window stays invisible until it gets content.
            self.top.geometry(f"{width}x{height}+-32000+-32000")
            self.top.deiconify()
            self.top.update_idletasks()
            self.visible = True
            self.position = None
        self.ensure_layered()
        if (x, y) != self.position or image.size != self.size:
            # Keep Tk's idea of the window in step so event coordinates stay correct.
            self.top.geometry(f"{width}x{height}+{x}+{y}")
        self.position, self.size = (x, y), image.size
        self._update(image)

    def ensure_layered(self) -> None:
        self.hwnd = _user32.GetParent(self.top.winfo_id()) or self.top.winfo_id()
        style = _user32.GetWindowLongW(self.hwnd, GWL_EXSTYLE)
        if style & self.extra_style != self.extra_style:
            _user32.SetWindowLongW(self.hwnd, GWL_EXSTYLE, style | self.extra_style)

    def redraw(self, image: Image.Image) -> None:
        self.present(image, *self.position)

    def hide(self) -> None:
        if self.visible:
            self.top.withdraw()
            self.visible = False

    def place_behind(self, other: "LayeredWindow") -> None:
        _user32.SetWindowPos(self.hwnd, other.hwnd, 0, 0, 0, 0, SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE)

    def _update(self, image: Image.Image) -> None:
        width, height = image.size
        red, green, blue, alpha = image.convert("RGBA").convert("RGBa").split()  # premultiplied
        data = Image.merge("RGBA", (blue, green, red, alpha)).tobytes()
        header = _BitmapInfoHeader(ctypes.sizeof(_BitmapInfoHeader), width, -height, 1, 32, 0, 0, 0, 0, 0, 0)
        screen = _user32.GetDC(None)
        memory = _gdi32.CreateCompatibleDC(screen)
        bits = ctypes.c_void_p()
        bitmap = _gdi32.CreateDIBSection(screen, ctypes.byref(header), 0, ctypes.byref(bits), None, 0)
        try:
            if not bitmap:
                raise ctypes.WinError(ctypes.get_last_error())
            ctypes.memmove(bits, data, len(data))
            previous = _gdi32.SelectObject(memory, bitmap)
            blend = _BlendFunction(0, 0, 255, 1)  # AC_SRC_OVER with per-pixel AC_SRC_ALPHA
            self.updated = _user32.UpdateLayeredWindow(self.hwnd, screen, ctypes.byref(wintypes.POINT(*self.position)),
                                                       ctypes.byref(wintypes.SIZE(width, height)), memory,
                                                       ctypes.byref(wintypes.POINT(0, 0)), 0, ctypes.byref(blend),
                                                       ULW_ALPHA)
            _gdi32.SelectObject(memory, previous)
        finally:
            if bitmap:
                _gdi32.DeleteObject(bitmap)
            _gdi32.DeleteDC(memory)
            _user32.ReleaseDC(None, screen)
