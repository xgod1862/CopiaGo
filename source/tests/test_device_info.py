import json
import os
import tempfile
import types
import unittest
from unittest import mock

from iphone_clip import device_info
from iphone_clip.device_info import DeviceInfo, drawable, model_name


class DeviceInfoTests(unittest.TestCase):
    def test_title_prefers_name_then_model_then_iphone(self):
        self.assertEqual(DeviceInfo("小明的 iPhone", "iPhone 15 Pro", "18.1").title(), "小明的 iPhone")
        self.assertEqual(DeviceInfo("", "iPhone 15 Pro", "18.1").title(), "iPhone 15 Pro")
        self.assertEqual(DeviceInfo().title(), "iPhone")

    def test_summary_skips_a_model_already_used_as_title(self):
        self.assertEqual(DeviceInfo("小明的 iPhone", "iPhone 15 Pro", "18.1").summary(), "iPhone 15 Pro · iOS 18.1")
        self.assertEqual(DeviceInfo("", "iPhone 15 Pro", "18.1").summary(), "iOS 18.1")
        self.assertEqual(DeviceInfo().summary(), "")

    def test_emoji_are_removed_because_the_ui_fonts_cannot_draw_them(self):
        self.assertEqual(drawable("🍎  小明的 iPhone 👨‍👩‍👧 "), "小明的 iPhone")
        self.assertEqual(DeviceInfo("📱", "iPhone 15 Pro").title(), "iPhone 15 Pro")

    def test_merge_keeps_saved_values_the_tunnel_did_not_report(self):
        saved = DeviceInfo("小明的 iPhone", "iPhone 15 Pro", "18.0")
        self.assertEqual(saved.merged(DeviceInfo("", "iPhone 15 Pro", "18.1")),
                         DeviceInfo("小明的 iPhone", "iPhone 15 Pro", "18.1"))

    def test_model_name_falls_back_to_product_type(self):
        table = types.SimpleNamespace(IRECV_DEVICES=[types.SimpleNamespace(product_type="iPhone16,1",
                                                                          display_name="iPhone 15 Pro")])
        with mock.patch.dict("sys.modules", {"pymobiledevice3.irecv_devices": table}):
            self.assertEqual(model_name("iPhone16,1"), "iPhone 15 Pro")
            self.assertEqual(model_name("iPhone99,9"), "iPhone99,9")
        self.assertEqual(model_name(None), "")


class DeviceInfoStorageTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        patcher = mock.patch.dict(os.environ, {"ICB_DATA_DIR": folder.name})
        patcher.start()
        self.addCleanup(patcher.stop)
        # Stand-in for DPAPI so the round trip runs anywhere; the real seal is covered by pair_store.
        store = types.SimpleNamespace(protect=lambda data: b"sealed:" + data,
                                      unprotect=lambda data: data.removeprefix(b"sealed:"))
        modules = mock.patch.dict("sys.modules", {"iphone_clip.pair_store": store})
        modules.start()
        self.addCleanup(modules.stop)

    def test_saved_details_round_trip_sealed_and_forget_removes_them(self):
        info = DeviceInfo("小明的 iPhone", "iPhone 15 Pro", "18.1")
        device_info.save_device_info(info)
        raw = device_info.info_path().read_bytes()
        self.assertTrue(raw.startswith(b"sealed:"))
        self.assertEqual(json.loads(raw.removeprefix(b"sealed:"))["name"], "小明的 iPhone")
        self.assertEqual(device_info.load_device_info(), info)
        device_info.forget_device_info()
        self.assertEqual(device_info.load_device_info(), DeviceInfo())
        device_info.forget_device_info()  # already gone is fine

    def test_damaged_file_only_loses_the_label(self):
        path = device_info.info_path()
        path.parent.mkdir(parents=True)
        path.write_bytes(b"sealed:not json")
        self.assertEqual(device_info.load_device_info(), DeviceInfo())


if __name__ == "__main__":
    unittest.main()
