import asyncio
import os
import plistlib
import struct
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

from iphone_clip import device, files
from iphone_clip.clipboard import Clip, ClipboardWriter
from iphone_clip.history import History
from test_reverse import StoreClipboard

PPTX = "org.openxmlformats.presentationml.presentation"


def descriptor(name):
    return plistlib.dumps({"$archiver": "NSKeyedArchiver", "$version": 100000, "$top": {},
                           "$objects": ["$null", "Documents", name, 19907087]}, fmt=plistlib.FMT_BINARY)


class NamingTests(unittest.TestCase):
    def test_original_name_is_read_from_the_description(self):
        self.assertEqual(files.name_from_descriptor(descriptor("季度汇报.pptx"), PPTX), "季度汇报.pptx")
        self.assertIsNone(files.name_from_descriptor(b"not a plist", PPTX))

    def test_names_are_made_safe_for_windows(self):
        self.assertEqual(files.safe_name('a<b>:c?.pdf', "com.adobe.pdf"), "a_b__c_.pdf")
        self.assertEqual(files.safe_name("CON.txt", "public.plain-text"), "_CON.txt")
        self.assertEqual(files.safe_name("无扩展名", PPTX), "无扩展名.pptx")
        with mock.patch("iphone_clip.i18n._language", "zh-CN"):
            self.assertTrue(files.safe_name(None, PPTX).startswith("iPhone 文件 "))
        with mock.patch("iphone_clip.i18n._language", "en"):
            self.assertRegex(files.safe_name(None, PPTX), r"^iPhone file \d{8}-\d{6}\.pptx$")


class StorageTests(unittest.TestCase):
    def setUp(self):
        self.env = mock.patch.dict(os.environ, {"ICB_DATA_DIR": tempfile.mkdtemp()})
        self.env.start()

    def tearDown(self):
        self.env.stop()

    def test_each_transfer_keeps_its_own_name(self):
        first = files.save_received("报告.pdf", b"one")
        second = files.save_received("报告.pdf", b"two")
        self.assertNotEqual(first, second)
        self.assertEqual((first.name, second.name), ("报告.pdf", "报告.pdf"))
        self.assertEqual(second.read_bytes(), b"two")

    def test_old_and_excess_transfers_are_removed(self):
        old = files.save_received("old.pdf", b"x")
        past = time.time() - files.KEEP_SECONDS - 60
        os.utime(old.parent, (past, past))
        kept = files.save_received("new.pdf", b"y")  # saving cleans up first
        self.assertFalse(old.exists())
        with mock.patch.object(files, "KEEP_BYTES", 0):
            files.cleanup()
        self.assertFalse(kept.exists())


class ClipboardFileTests(unittest.TestCase):
    def test_files_go_on_the_clipboard_like_explorer_copy(self):
        api = StoreClipboard()
        api.CF_HDROP = 15
        writer = ClipboardWriter(1, api)
        writer.put_files([Path(r"C:\temp\报告.pdf")])
        data = api.data[15]
        offset, _, _, _, wide = struct.unpack("<IiiII", data[:20])
        self.assertEqual((offset, wide), (20, 1))
        self.assertEqual(data[20:].decode("utf-16-le"), "C:\\temp\\报告.pdf\0\0")
        self.assertEqual(writer.read(), (None, True))  # never sent back to the phone


class FileHistoryTests(unittest.TestCase):
    def test_file_round_trip(self):
        history = History(Path(tempfile.mkdtemp()) / "h.sqlite3")
        history.add(Clip(file_name="季度汇报.pptx", file_data=b"PK\x03\x04data"))
        entry = history.entries()[0]
        self.assertEqual((entry.kind, entry.preview.decode()), ("file", "季度汇报.pptx\n8"))
        self.assertEqual(history.get(entry.id), Clip(file_name="季度汇报.pptx", file_data=b"PK\x03\x04data"))


def snapshot(data, count=1):
    return {"pasteboard": {"items": [{"types": [device.FILE_DESCRIPTOR, PPTX], "data": data}],
                           "metadata": {"nonce": "n", "changeCount": count}}}


class FakeService:
    answers: dict = {}
    calls: list = []

    def __init__(self, rsd): pass
    async def __aenter__(self): return self
    async def __aexit__(self, *exc): return False

    async def get(self, name, policy):
        key = next(iter(policy))
        FakeService.calls.append(key)
        return FakeService.answers[key]


class BridgeFileTests(unittest.TestCase):
    def run_with(self, answers, coroutine_factory):
        FakeService.answers, FakeService.calls = answers, []
        with mock.patch("pymobiledevice3.remote.core_device.pasteboard_service.PasteboardService", FakeService):
            return asyncio.run(coroutine_factory())

    def test_large_file_is_described_without_its_bytes(self):
        answers = {"allPromised": snapshot({}),
                   "thresholdData": snapshot({device.FILE_DESCRIPTOR: {"data": descriptor("季度汇报.pptx")},
                                              PPTX: {"isPromised": True, "size": 19907087}})}
        _, offer = self.run_with(answers, lambda: device.read_clip(object()))
        self.assertIsInstance(offer, device.FileOffer)
        self.assertEqual((offer.name, offer.size, offer.data, offer.change),
                         ("季度汇报.pptx", 19907087, None, ("n", 1)))

    def test_fetch_transfers_the_offered_file_and_refuses_a_stale_one(self):
        bridge = device.Bridge(lambda *a: None, lambda *a: None)
        offer = device.FileOffer("季度汇报.pptx", PPTX, 10, ("n", 1))

        async def fetch():
            bridge.rsd, bridge.rsd_lock = object(), asyncio.Lock()
            return await bridge._fetch(offer)

        clip = self.run_with({"allPromised": snapshot({}), "thresholdData": snapshot({PPTX: {"data": b"0123456789"}})},
                             fetch)
        self.assertEqual(clip, Clip(file_name="季度汇报.pptx", file_data=b"0123456789"))
        with self.assertRaises(device.OfferExpired):
            self.run_with({"allPromised": snapshot({}, count=2), "thresholdData": snapshot({})}, fetch)
        self.assertEqual(FakeService.calls, ["allPromised"])  # nothing transferred for a stale offer

    def test_fetch_reports_progress_of_the_transfer_only(self):
        bridge = device.Bridge(lambda *a: None, lambda *a: None)
        offer = device.FileOffer("季度汇报.pptx", PPTX, 10, ("n", 1))
        reports, listing_progress = [], []

        class Connection:
            on_progress = None

        class ProgressService(FakeService):
            def __init__(self, rsd):
                self.service = Connection()

            async def get(self, name, policy):
                if next(iter(policy)) == "thresholdData":
                    self.service.on_progress(4, 10)
                    self.service.on_progress(10, 10)
                else:
                    listing_progress.append(self.service.on_progress)
                return await super().get(name, policy)

        async def fetch():
            bridge.rsd, bridge.rsd_lock = object(), asyncio.Lock()
            return await bridge._fetch(offer, lambda received, total: reports.append((received, total)))

        FakeService.answers = {"allPromised": snapshot({}), "thresholdData": snapshot({PPTX: {"data": b"0123456789"}})}
        FakeService.calls = []
        with mock.patch("pymobiledevice3.remote.core_device.pasteboard_service.PasteboardService", ProgressService):
            asyncio.run(fetch())
        self.assertEqual(reports, [(4, 10), (10, 10)])
        self.assertEqual(listing_progress, [None])  # the quick listing request is not part of the transfer

    def test_throttled_progress_drops_bursts_but_keeps_the_end(self):
        reports = []
        forward = device.throttled(lambda received, total: reports.append(received), interval=60)
        for received in range(1, 11):
            forward(received, 10)
        self.assertEqual(reports, [1, 10])


if __name__ == "__main__":
    unittest.main()
