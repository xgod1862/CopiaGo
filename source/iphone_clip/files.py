"""Files copied on the iPhone: naming, local storage for pasting, and cleanup."""
from __future__ import annotations

import plistlib
import re
import shutil
import tempfile
import time
from datetime import datetime
from pathlib import Path

from iphone_clip.i18n import tr
from iphone_clip.settings import data_dir

FILE_DESCRIPTOR = "com.apple.DocumentManager.FPItem.File"
PHOTO_IDENTIFIER = "com.apple.mobileslideshow.asset.localidentifier"
MAX_FILE_BYTES = 512 * 1024 * 1024  # one pasteboard transfer is held in memory on both ends
KEEP_SECONDS = 24 * 3600
KEEP_BYTES = 2 * 1024 * 1024 * 1024

EXTENSIONS = {
    "com.adobe.pdf": "pdf",
    "org.openxmlformats.wordprocessingml.document": "docx",
    "org.openxmlformats.spreadsheetml.sheet": "xlsx",
    "org.openxmlformats.presentationml.presentation": "pptx",
    "com.microsoft.word.doc": "doc",
    "com.microsoft.excel.xls": "xls",
    "com.microsoft.powerpoint.ppt": "ppt",
    "com.apple.keynote.key": "key",
    "com.apple.iwork.pages.pages": "pages",
    "com.apple.iwork.numbers.numbers": "numbers",
    "public.zip-archive": "zip",
    "public.html": "html",
    "public.plain-text": "txt",
    "public.utf8-plain-text": "txt",
    "public.rtf": "rtf",
    "public.json": "json",
    "public.comma-separated-values-text": "csv",
    "public.mpeg-4": "mp4",
    "com.apple.quicktime-movie": "mov",
    "public.mp3": "mp3",
    "public.mpeg-4-audio": "m4a",
    "public.jpeg": "jpg",
    "public.png": "png",
    "public.heic": "heic",
}
_INVALID = re.compile(r'[<>:"/\\|?*\x00-\x1f]')
_RESERVED = {"CON", "PRN", "AUX", "NUL", *(f"COM{n}" for n in range(1, 10)), *(f"LPT{n}" for n in range(1, 10))}


def content_type(types: list[str]) -> str | None:
    """The type carrying a copied file's bytes: anything but the descriptor and photo id."""
    rest = [name for name in types if name not in (FILE_DESCRIPTOR, PHOTO_IDENTIFIER)]
    return rest[0] if rest else None


def name_from_descriptor(descriptor: bytes | None, uti: str | None) -> str | None:
    """The original file name, read from the Files app's archived item description."""
    if not descriptor:
        return None
    try:
        archive = plistlib.loads(descriptor)
    except Exception:
        return None
    objects = archive.get("$objects", []) if isinstance(archive, dict) else []
    expected = EXTENSIONS.get(uti or "")
    candidates = [item for item in objects if isinstance(item, str) and "/" not in item
                  and re.search(r"\.[A-Za-z0-9]{1,8}$", item)]
    if expected:
        matching = [item for item in candidates if item.lower().endswith("." + expected)]
        candidates = matching or candidates
    return candidates[0] if candidates else None


def safe_name(name: str | None, uti: str | None) -> str:
    """Return a Windows-safe file name, or a localized fallback without an original name."""
    extension = EXTENSIONS.get(uti or "", "bin")
    if name:
        name = _INVALID.sub("_", name).strip(" .")
        stem, dot, suffix = name.rpartition(".")
        if not dot:
            stem, suffix = name, extension
        if stem.upper() in _RESERVED:
            stem = "_" + stem
        stem = stem[:120]
        if stem:
            return f"{stem}.{suffix}"
    return tr("iPhone 文件 {timestamp}.{extension}", timestamp=f"{datetime.now():%Y%m%d-%H%M%S}",
              extension=extension)


def received_dir() -> Path:
    return data_dir() / "received"


def save_received(name: str, data: bytes) -> Path:
    """Write a received file where Windows can paste it from; one folder per transfer keeps names."""
    cleanup()
    root = received_dir()
    root.mkdir(parents=True, exist_ok=True)
    # A unique folder per transfer: the clock alone can repeat for quick successive saves.
    path = Path(tempfile.mkdtemp(prefix=f"{datetime.now():%Y%m%d-%H%M%S}-", dir=root)) / name
    path.write_bytes(data)
    return path


def cleanup(now: float | None = None) -> None:
    """Drop transfers older than a day, then the oldest ones beyond the size budget."""
    root = received_dir()
    if not root.is_dir():
        return
    now = time.time() if now is None else now
    folders = []
    for folder in root.iterdir():
        if not folder.is_dir():
            continue
        try:
            age = now - folder.stat().st_mtime
            size = sum(path.stat().st_size for path in folder.rglob("*") if path.is_file())
        except OSError:
            continue
        if age > KEEP_SECONDS:
            shutil.rmtree(folder, ignore_errors=True)
        else:
            folders.append((folder.stat().st_mtime, size, folder))
    total = sum(size for _, size, _ in folders)
    for _, size, folder in sorted(folders):
        if total <= KEEP_BYTES:
            break
        shutil.rmtree(folder, ignore_errors=True)
        total -= size


def human_size(size: int) -> str:
    if size >= 1024 * 1024 * 1024:
        return f"{size / 1024 ** 3:.1f} GB"
    if size >= 1024 * 1024:
        return f"{size / 1024 ** 2:.1f} MB"
    if size >= 1024:
        return f"{size / 1024:.0f} KB"
    return f"{size} B"


def short_name(name: str, limit: int = 16) -> str:
    """Shorten a file name for a status line, keeping its start and extension."""
    if len(name) <= limit:
        return name
    stem, dot, suffix = name.rpartition(".")
    if not dot or len(suffix) > 6:
        return name[:limit - 1] + "…"
    keep = max(limit - len(suffix) - 2, 4)
    return f"{stem[:keep]}…{dot}{suffix}"
