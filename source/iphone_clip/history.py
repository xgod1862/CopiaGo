"""Local clipboard history encrypted for the current Windows account.

Two lists share one table: what came from the iPhone ("phone") and what was copied on this
computer ("pc"). Each list has its own limits and is cleared on its own.
"""
from __future__ import annotations

from contextlib import closing
from dataclasses import dataclass
from datetime import datetime
import io
import sqlite3
from pathlib import Path

import win32crypt

from iphone_clip.clipboard import Clip, open_image

MAX_ITEMS = 50  # per list
MAX_BYTES = 128 * 1024 * 1024  # per list
MAX_ITEM_BYTES = 32 * 1024 * 1024
SOURCES = ("phone", "pc")


@dataclass(frozen=True)
class Entry:
    id: int
    created: str
    kind: str
    preview: bytes


def encrypt(data: bytes) -> bytes:
    return win32crypt.CryptProtectData(data, "iPhone Clipboard", None, None, None, 1)


def decrypt(data: bytes) -> bytes:
    return win32crypt.CryptUnprotectData(data, None, None, None, 1)[1]


class History:
    def __init__(self, path: Path):
        self.path = path

    def connect(self) -> sqlite3.Connection:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        db = sqlite3.connect(self.path, timeout=5)
        db.execute("PRAGMA secure_delete = ON")
        db.execute(
            """CREATE TABLE IF NOT EXISTS items (
                id INTEGER PRIMARY KEY, created TEXT NOT NULL, kind TEXT NOT NULL,
                preview BLOB NOT NULL, payload BLOB NOT NULL, source TEXT NOT NULL DEFAULT 'phone'
            )"""
        )
        # Histories from before the computer list was added hold iPhone items only.
        if "source" not in {row[1] for row in db.execute("PRAGMA table_info(items)")}:
            db.execute("ALTER TABLE items ADD COLUMN source TEXT NOT NULL DEFAULT 'phone'")
        return db

    def add(self, clip: Clip, source: str = "phone") -> bool:
        if source not in SOURCES:
            raise ValueError(source)
        if clip.file_data is not None:
            raw, kind = clip.file_data, "file"
            # The preview of a file is its name and size; the payload is the file itself.
            preview = f"{clip.file_name or ''}\n{len(raw)}".encode("utf-8")
        elif clip.image:
            raw, kind = clip.image, "image"
            if len(raw) > MAX_ITEM_BYTES:
                return False
            with open_image(raw) as picture:
                picture.thumbnail((88, 56))
                buffer = io.BytesIO()
                picture.convert("RGBA").save(buffer, "PNG")
            preview = buffer.getvalue()
        elif clip.text:
            raw, kind = clip.text.encode("utf-8"), "text"
            preview = " ".join(clip.text.split())[:96].encode("utf-8")
        else:
            return False
        if len(raw) > MAX_ITEM_BYTES:
            return False
        protected = encrypt(raw)
        with closing(self.connect()) as db, db:
            previous = db.execute("SELECT kind, payload FROM items WHERE source = ? ORDER BY id DESC LIMIT 1",
                                  (source,)).fetchone()
            if previous and previous[0] == kind:
                try:
                    if decrypt(previous[1]) == raw:
                        return True
                except Exception:
                    pass
            db.execute(
                "INSERT INTO items (created, kind, preview, payload, source) VALUES (?, ?, ?, ?, ?)",
                (datetime.now().isoformat(timespec="minutes"), kind, encrypt(preview), protected, source),
            )
            rows = db.execute("SELECT id, length(preview) + length(payload) FROM items WHERE source = ? "
                              "ORDER BY id DESC", (source,))
            total = 0
            cutoff = None
            for index, (item_id, size) in enumerate(rows):
                total += size
                if index >= MAX_ITEMS or total > MAX_BYTES:
                    cutoff = item_id
                    break
            rows.close()
            if cutoff is not None:
                db.execute("DELETE FROM items WHERE source = ? AND id <= ?", (source, cutoff))
        return True

    def entries(self, source: str = "phone") -> list[Entry]:
        if not self.path.is_file():
            return []
        with closing(self.connect()) as db:
            rows = db.execute("SELECT id, created, kind, preview FROM items WHERE source = ? ORDER BY id DESC",
                              (source,))
            result = []
            for item_id, created, kind, preview in rows:
                try:
                    result.append(Entry(item_id, created, kind, decrypt(preview)))
                except Exception:
                    continue
            return result

    def get(self, item_id: int) -> Clip:
        with closing(self.connect()) as db:
            row = db.execute("SELECT kind, payload, preview FROM items WHERE id = ?", (item_id,)).fetchone()
        if row is None:
            raise KeyError(item_id)
        kind, encrypted, preview = row
        raw = decrypt(encrypted)
        if kind == "file":
            return Clip(file_name=decrypt(preview).decode("utf-8").split("\n", 1)[0], file_data=raw)
        return Clip(image=raw) if kind == "image" else Clip(text=raw.decode("utf-8"))

    def remove(self, item_id: int | None = None, source: str | None = None) -> None:
        """Remove one item, one list (``source``), or everything."""
        if not self.path.is_file():
            return
        with closing(self.connect()) as db, db:
            if item_id is not None:
                db.execute("DELETE FROM items WHERE id = ?", (item_id,))
            elif source is not None:
                db.execute("DELETE FROM items WHERE source = ?", (source,))
            else:
                db.execute("DELETE FROM items")
