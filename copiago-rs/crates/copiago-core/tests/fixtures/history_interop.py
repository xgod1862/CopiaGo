"""Exercise the repository's original history code on the Rust test's temporary DB."""

from datetime import datetime
import io
import sys

from PIL import Image
import win32crypt

from iphone_clip.clipboard import Clip
from iphone_clip.history import History
from iphone_clip.settings import data_dir


def seed(history):
    assert history.add(Clip(text=" \u3000来自 Python😀\x1c 保留原文 \n"), "phone")
    buffer = io.BytesIO()
    with Image.new("RGBA", (240, 120), (192, 120, 56, 128)) as image:
        image.save(buffer, "PNG")
    assert history.add(Clip(image=buffer.getvalue()), "phone")
    assert history.add(Clip(file_name="样例😀.bin", file_data=b"\0payload-python\xff"), "pc")


def verify(history):
    image, text, python_image, _ = history.entries("phone")
    assert (image.kind, text.kind) == ("image", "text")
    assert history.get(image.id).image == history.get(python_image.id).image
    with Image.open(io.BytesIO(image.preview)) as thumbnail:
        assert thumbnail.size == (88, 44) and thumbnail.mode == "RGBA"
        assert thumbnail.getpixel((0, 0))[3] == 128
    assert text.preview == "来自 Rust 🦀 第二行".encode("utf-8")
    assert history.get(text.id).text == "\t来自 Rust 🦀 \n 第二行  "
    assert len(text.created) == 16
    assert abs((datetime.now() - datetime.fromisoformat(text.created)).total_seconds()) < 120

    file, python_file = history.entries("pc")
    assert file.kind == "file" and file.preview == "资料🦀.bin\n11".encode("utf-8")
    assert history.get(file.id) == Clip(file_name="资料🦀.bin", file_data=b"\x00Rust file\xff")
    assert history.get(python_file.id) == Clip(file_name="样例😀.bin", file_data=b"\0payload-python\xff")

    from contextlib import closing

    with closing(history.connect()) as db:
        blobs = db.execute("SELECT preview, payload FROM items WHERE id = ?", (text.id,)).fetchone()
        for blob in blobs:
            assert not blob.startswith(b"ICB-DPAPI1\0")
            description, _ = win32crypt.CryptUnprotectData(blob, None, None, None, 1)
            assert description == "CopiaGo"


if __name__ == "__main__":
    action = {"seed": seed, "verify": verify}[sys.argv[1]]
    action(History(data_dir() / "history.sqlite3"))
