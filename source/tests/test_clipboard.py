import io
import struct
import unittest

from PIL import Image

from iphone_clip.clipboard import Clip, ClipboardWriter
from iphone_clip.device import change_id


class FakeClipboard:
    CF_DIB = 8
    CF_UNICODETEXT = 13

    def __init__(self, reject_format=None):
        self.formats = {}
        self.ops = []
        self.reject_format = reject_format

    def RegisterClipboardFormat(self, name):
        self.formats[name] = 100 + len(self.formats)
        return self.formats[name]

    def OpenClipboard(self, hwnd):
        self.ops.append(("open", hwnd))

    def EmptyClipboard(self):
        self.ops.append(("empty",))

    def SetClipboardData(self, format_id, value):
        if format_id == self.reject_format:
            raise OSError("privacy marker rejected")
        self.ops.append(("set", format_id, value))

    def CloseClipboard(self):
        self.ops.append(("close",))


class ClipboardPrivacyTests(unittest.TestCase):
    def test_history_markers_are_written_before_unicode_text(self):
        api = FakeClipboard()
        writer = ClipboardWriter(99, api)
        self.assertEqual(writer.put(Clip(text="测试\nhello😀")), "文本")
        sets = [item for item in api.ops if item[0] == "set"]
        self.assertEqual([item[1] for item in sets], [writer.exclude_format, writer.history_format, writer.cloud_format, api.CF_UNICODETEXT])
        self.assertEqual(sets[1][2], struct.pack("<I", 0))
        self.assertEqual(sets[2][2], struct.pack("<I", 0))
        self.assertEqual(sets[-1][2], "测试\nhello😀")
        self.assertEqual(api.ops[-1], ("close",))

    def test_failure_to_set_privacy_marker_never_writes_content(self):
        api = FakeClipboard()
        writer = ClipboardWriter(99, api)
        api.reject_format = writer.history_format
        with self.assertRaises(OSError):
            writer.put(Clip(text="private sample"))
        self.assertFalse(any(item[0] == "set" and item[1] == api.CF_UNICODETEXT for item in api.ops))
        self.assertEqual(api.ops[-1], ("close",))

    def test_photo_is_offered_as_dib_and_png_with_privacy_markers(self):
        image = Image.new("RGB", (4, 3), "red")
        encoded = io.BytesIO()
        image.save(encoded, format="PNG")
        api = FakeClipboard()
        writer = ClipboardWriter(99, api)
        self.assertEqual(writer.put(Clip(image=encoded.getvalue())), "图片")
        data = {item[1]: item[2] for item in api.ops if item[0] == "set"}
        self.assertIn(api.CF_DIB, data)
        self.assertEqual(data[api.CF_DIB][:4], struct.pack("<I", 40))
        self.assertEqual(data[writer.png_format][:8], b"\x89PNG\r\n\x1a\n")

    def test_empty_and_invalid_text_do_not_open_clipboard(self):
        for clip in (Clip(), Clip(text="bad\0text")):
            api = FakeClipboard()
            writer = ClipboardWriter(99, api)
            with self.assertRaises(ValueError):
                writer.put(clip)
            self.assertFalse(api.ops)

    def test_detects_clipboard_changes_made_by_other_programs(self):
        api = FakeClipboard()
        api.sequence = 1
        api.GetClipboardSequenceNumber = lambda: api.sequence
        writer = ClipboardWriter(99, api)
        self.assertFalse(writer.changed_elsewhere())  # Nothing written yet.
        api.sequence = 5
        writer.put(Clip(text="from iPhone"))
        self.assertFalse(writer.changed_elsewhere())
        api.sequence = 6
        self.assertTrue(writer.changed_elsewhere())

    def test_metadata_is_compared_without_reading_clip_content(self):
        self.assertEqual(change_id({"pasteboard": {"metadata": {"nonce": "a", "changeCount": 7}}}), ("a", 7))
        self.assertIsNone(change_id({"pasteboard": {"metadata": {}}}))
        self.assertIsNone(change_id({}))


if __name__ == "__main__":
    unittest.main()
