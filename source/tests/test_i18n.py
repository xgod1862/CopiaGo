import os
import tempfile
import tkinter as tk
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from iphone_clip import i18n, settings
from iphone_clip.device import CONNECTED_WIFI, failure_message
from iphone_clip.popup import HistoryPopup, font, wrap


class LanguageTests(unittest.TestCase):
    def setUp(self):
        self.addCleanup(i18n.use_language, "system")

    def test_system_prefers_windows_display_language(self):
        with mock.patch.object(i18n.ctypes.windll.kernel32, "GetUserDefaultUILanguage") as language_id:
            for value, expected in ((0x0804, "设置"), (0x0404, "设置"), (0x0409, "Settings"), (0x040C, "Settings")):
                with self.subTest(language_id=value):
                    language_id.return_value = value
                    i18n.use_language("system")
                    self.assertEqual(i18n.tr("设置"), expected)

    def test_manual_preference_is_saved_with_existing_settings(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.dict(os.environ, {"ICB_DATA_DIR": folder}):
            saved = Path(folder) / "settings.json"
            saved.write_text('{"history": true}', encoding="utf-8")
            self.assertEqual(settings.language_preference(), "system")
            for value in ("en", "zh-CN", "system"):
                settings.set_language_preference(value)
                self.assertEqual(settings.language_preference(), value)
                self.assertTrue(settings.history_enabled())
            saved.write_text('{"language": "invalid"}', encoding="utf-8")
            self.assertEqual(settings.language_preference(), "system")

    def test_existing_connection_and_error_messages_change_language(self):
        error = type("PasswordRequiredError", (Exception,), {})()
        status = failure_message(error, pairing=True)
        i18n.use_language("en")
        self.assertEqual(str(CONNECTED_WIFI), "Connected (Wi-Fi)")
        self.assertIn("Unlock your iPhone", str(status))
        i18n.use_language("zh-CN")
        self.assertEqual(str(CONNECTED_WIFI), "已连接（无线）")
        self.assertIn("解锁", str(status))

    def test_pairing_settings_allow_language_change_without_pairing(self):
        i18n.use_language("en")
        root = tk.Tk()
        root.withdraw()
        self.addCleanup(root.destroy)
        noop = lambda *_: None
        popup = HistoryPopup(root, 1.0, on_copy=noop, on_delete=noop, on_clear=noop,
                             on_enable_history=noop, on_toggle=noop, on_forget=noop, on_type=mock.Mock())
        popup.back_page = "onboarding"
        popup.page = "onboarding"
        popup.set_settings({"language": "system", "paired": False})
        popup.set_onboarding("developer", "missing", retry=True)

        def click(action):
            popup.frame()
            hit = next(hit for hit in popup.hits if hit[4] == action)
            popup.release(SimpleNamespace(x=hit[0], y=hit[1]))

        click(("settings",))
        self.assertEqual(popup.page, "settings")
        click(("type", "language", "zh-CN"))
        popup.on_type.assert_called_once_with("language", "zh-CN")
        self.assertNotIn(("forget",), [hit[4] for hit in popup.hits])
        click(("back",))
        self.assertEqual(popup.page, "onboarding")

    def test_english_help_wraps_without_splitting_words_that_fit(self):
        face = font(12)
        text = "Unlock your iPhone and enable Developer Mode."
        width = face.getlength("Unlock your iPhone a")
        lines = wrap(text, face, width, 5)
        self.assertEqual(lines[0], "Unlock your iPhone")
        self.assertEqual(" ".join(lines), text)
