from __future__ import annotations

import argparse
import ctypes
import sys

from iphone_clip import APP_NAME


def main() -> int:
    parser = argparse.ArgumentParser(description=f"{APP_NAME} — iPhone and Windows clipboard sync")
    parser.add_argument("--minimized", action="store_true")
    args = parser.parse_args()

    # A second copy would compete for the same iPhone developer tunnel.
    kernel32 = ctypes.windll.kernel32
    kernel32.CreateMutexW.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_wchar_p]
    kernel32.CreateMutexW.restype = ctypes.c_void_p
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    kernel32.CloseHandle.restype = ctypes.c_int
    kernel32.GetLastError.restype = ctypes.c_ulong
    kernel32.CreateEventW.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_wchar_p]
    kernel32.CreateEventW.restype = ctypes.c_void_p
    kernel32.SetEvent.argtypes = [ctypes.c_void_p]
    kernel32.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
    kernel32.WaitForSingleObject.restype = ctypes.c_ulong
    event = kernel32.CreateEventW(None, False, False, "Local\\iPhoneClipboardShow")
    if not event:
        return 1
    mutex = kernel32.CreateMutexW(None, False, "Local\\iPhoneClipboardPrototype")
    if not mutex:
        kernel32.CloseHandle(event)
        return 1
    if kernel32.GetLastError() == 183:
        if not args.minimized:
            kernel32.SetEvent(event)
        kernel32.CloseHandle(mutex)
        kernel32.CloseHandle(event)
        return 0
    try:
        from iphone_clip.ui import run
        run(minimized=args.minimized, wake=lambda: kernel32.WaitForSingleObject(event, 0) == 0)
    finally:
        kernel32.CloseHandle(mutex)
        kernel32.CloseHandle(event)
    return 0


if __name__ == "__main__":
    sys.exit(main())
