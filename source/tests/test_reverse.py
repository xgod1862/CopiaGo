import asyncio
import io
import struct
import unittest
from unittest import mock

from PIL import Image

from iphone_clip import device
from iphone_clip.clipboard import Clip, ClipboardWriter


class StoreClipboard:
    """A fake Windows clipboard that keeps what was put on it."""
    CF_DIB = 8
    CF_UNICODETEXT = 13

    def __init__(self):
        self.formats, self.data, self.sequence = {}, {}, 1

    def RegisterClipboardFormat(self, name):
        return self.formats.setdefault(name, 100 + len(self.formats))

    def OpenClipboard(self, hwnd): pass
    def CloseClipboard(self): pass

    def EmptyClipboard(self):
        self.data.clear()

    def SetClipboardData(self, format_id, value):
        self.data[format_id] = value
        self.sequence += 1

    def IsClipboardFormatAvailable(self, format_id):
        return format_id in self.data

    def GetClipboardData(self, format_id):
        return self.data[format_id]

    def GetClipboardSequenceNumber(self):
        return self.sequence


def png(width=6, height=4):
    buffer = io.BytesIO()
    Image.new("RGB", (width, height), "blue").save(buffer, "PNG")
    return buffer.getvalue()


class ReadWindowsClipboardTests(unittest.TestCase):
    def setUp(self):
        self.api = StoreClipboard()
        self.writer = ClipboardWriter(1, self.api)

    def test_plain_text_is_read(self):
        self.api.data[self.api.CF_UNICODETEXT] = "会议纪要"
        self.assertEqual(self.writer.read(), (Clip(text="会议纪要"), False))

    def test_password_manager_content_is_never_read(self):
        self.api.data[self.api.CF_UNICODETEXT] = "hunter2"
        self.api.data[self.api.RegisterClipboardFormat("Clipboard Viewer Ignore")] = b""
        self.assertEqual(self.writer.read(), (None, True))

    def test_history_opt_out_counts_as_sensitive(self):
        self.api.data[self.api.CF_UNICODETEXT] = "123456"
        self.api.data[self.api.RegisterClipboardFormat("CanIncludeInClipboardHistory")] = struct.pack("<I", 0)
        self.assertEqual(self.writer.read(), (None, True))

    def test_what_this_app_wrote_from_the_phone_is_not_sent_back(self):
        self.writer.put(Clip(text="来自 iPhone"))
        self.assertEqual(self.writer.read(), (None, True))

    def test_png_and_dib_images_become_png(self):
        self.api.data[self.writer.png_format] = png()
        clip, sensitive = self.writer.read()
        self.assertFalse(sensitive)
        self.assertEqual(clip.image, png())
        self.api.data.clear()
        bmp = io.BytesIO()
        Image.new("RGB", (5, 3), "green").save(bmp, "BMP")
        self.api.data[self.api.CF_DIB] = bmp.getvalue()[14:]
        clip, _ = self.writer.read()
        self.assertTrue(clip.image.startswith(b"\x89PNG"))
        with Image.open(io.BytesIO(clip.image)) as image:
            self.assertEqual(image.size, (5, 3))


class PushTests(unittest.TestCase):
    def test_push_marks_its_own_change_and_echo(self):
        sent = []

        class FakeService:
            def __init__(self, rsd): pass
            async def __aenter__(self): return self
            async def __aexit__(self, *exc): return False

            async def set(self, items):
                sent.append(items)
                return {"pasteboard": {"metadata": {"nonce": "n", "changeCount": 42}}}

        bridge = device.Bridge(lambda *a: None, lambda *a: None)

        async def main():
            bridge.rsd, bridge.rsd_lock = object(), asyncio.Lock()
            with mock.patch("pymobiledevice3.remote.core_device.pasteboard_service.PasteboardService", FakeService):
                await bridge._push(Clip(text="从电脑来"))

        asyncio.run(main())
        self.assertEqual(sent[0][0]["data"]["public.utf8-plain-text"]["data"], "从电脑来".encode())
        self.assertEqual(bridge.last_change, ("n", 42))  # the monitor will not pull it back
        self.assertTrue(bridge.is_echo(Clip(text="从电脑来")))
        self.assertFalse(bridge.is_echo(Clip(text="手机上新复制的")))

    def test_push_without_connection_is_refused(self):
        bridge = device.Bridge(lambda *a: None, lambda *a: None)
        with self.assertRaises(ConnectionError):
            bridge.push(Clip(text="x"))


if __name__ == "__main__":
    unittest.main()
