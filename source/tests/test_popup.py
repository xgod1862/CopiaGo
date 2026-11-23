import tkinter as tk
import unittest
from datetime import datetime
from unittest import mock

from iphone_clip.history import Entry
from iphone_clip import popup as panel
from iphone_clip.popup import HistoryPopup, fit, font, wrap


class TextLayoutTests(unittest.TestCase):
    def test_fit_keeps_short_text_and_ellipsizes_long_text(self):
        face = font(14)
        self.assertEqual(fit("短文本", face, 200), "短文本")
        cut = fit("这是一段很长很长很长很长很长很长很长的文本", face, 80)
        self.assertTrue(cut.endswith("…"))
        self.assertLessEqual(face.getlength(cut), 80)

    def test_wrap_limits_lines_and_marks_the_cut(self):
        face = font(14)
        lines = wrap("会议改到周四下午三点，请大家提前十分钟到五楼大会议室，带上评审材料和进度说明。", face, 120, 2)
        self.assertEqual(len(lines), 2)
        self.assertTrue(lines[-1].endswith("…"))
        self.assertTrue(all(face.getlength(line) <= 120 + face.size for line in lines))  # may end in punctuation


class PopupRenderTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = tk.Tk()
        cls.root.withdraw()

    @classmethod
    def tearDownClass(cls):
        cls.root.destroy()

    def popup(self):
        noop = lambda *_: None
        return HistoryPopup(self.root, 1.0, on_copy=noop, on_delete=noop, on_clear=noop, on_enable_history=noop,
                            on_toggle=noop, on_forget=noop, version="0.3.0")

    def test_selected_card_offers_copy_and_delete(self):
        popup = self.popup()
        now = datetime.now().isoformat(timespec="minutes")
        popup.entries = [Entry(2, now, "text", "最新一条".encode()), Entry(1, now, "text", "更早一条".encode())]
        popup.history_on = True
        frame = popup.frame()
        self.assertEqual(frame.width, 392)
        actions = [hit[4] for hit in popup.hits]
        for expected in (("copy", 0), ("delete", 0), ("select", 1), ("settings",), ("clear",)):
            self.assertIn(expected, actions)
        self.assertNotIn(("copy", 1), actions)
        self.assertEqual(frame.getpixel((0, 0))[3], 0)  # rounded corner is transparent

    def test_settings_page_offers_switches_forget_and_back(self):
        popup = self.popup()
        popup.page = "settings"
        popup.settings = {"startup": (True, False), "history": (False, True), "notify": (True, True),
                          "pull_types": ("text", "image"), "push_types": ("text",), "ask_limit": 20}
        popup.frame()
        actions = [hit[4] for hit in popup.hits]
        for expected in (("back",), ("forget",), ("toggle", "history"), ("type", "pull_types", "file"),
                         ("type", "ask_limit", 50)):
            self.assertIn(expected, actions)
        self.assertNotIn(("type", "ask_limit", 20), actions)  # the chosen limit is already chosen
        self.assertNotIn(("settings",), actions)
        self.assertNotIn(("toggle", "notify"), actions)  # below the fold until scrolled
        popup.settings_scroll = 10_000
        popup.frame()
        actions = [hit[4] for hit in popup.hits]
        self.assertIn(("toggle", "notify"), actions)
        self.assertIn(("type", "push_types", "image"), actions)
        self.assertNotIn(("type", "push_types", "file"), actions)  # files go one way for now
        self.assertNotIn(("type", "push_types", "text"), actions)  # the only kind left cannot be turned off
        self.assertNotIn(("toggle", "startup"), actions)  # cannot change outside the installed app
        self.assertNotIn(("forget",), actions)  # scrolled out of view

    def test_onboarding_page_follows_pairing_phase(self):
        popup = self.popup()
        popup.page = "onboarding"
        popup.onboarding = {"phase": "idle", "driver": "missing"}
        popup.frame()
        actions = [hit[4] for hit in popup.hits]
        self.assertIn(("pair",), actions)
        self.assertIn(("install",), actions)
        self.assertIn(("close",), actions)
        popup.onboarding = {"phase": "pairing", "driver": "ok"}
        popup.frame()
        self.assertNotIn(("pair",), [hit[4] for hit in popup.hits])  # busy: no second request
        popup.onboarding = {"phase": "done", "driver": "ok"}
        popup.frame()
        self.assertIn(("finish",), [hit[4] for hit in popup.hits])

    def test_empty_state_offers_to_enable_history_only_when_off(self):
        popup = self.popup()
        popup.frame()
        self.assertIn(("enable",), [hit[4] for hit in popup.hits])
        popup.history_on = True
        popup.frame()
        self.assertNotIn(("enable",), [hit[4] for hit in popup.hits])


class ConnectionProgressRenderTests(unittest.TestCase):
    def test_progress_draws_current_fraction_or_animates_and_clears_on_failure(self):
        with mock.patch.object(panel, "LayeredWindow") as window:
            window.return_value.visible = False
            noop = lambda *_: None
            popup = HistoryPopup(mock.Mock(), 1.0, on_copy=noop, on_delete=noop, on_clear=noop,
                                 on_enable_history=noop, on_toggle=noop, on_forget=noop, version="0.3.0")

        for page in ("onboarding", "history"):
            with self.subTest(page=page):
                popup.page = page
                popup.set_onboarding("preparing", "ok")
                popup.set_status("Downloading", "busy", "muted", progressing=True, fraction=0.5)
                with mock.patch.object(popup, "draw_progress", wraps=popup.draw_progress) as bar:
                    frame = popup.frame()
                    _, x, y, width = bar.call_args.args
                    self.assertEqual(frame.getpixel((x + width // 4, y + 2)), panel.ACCENT)
                    self.assertEqual(frame.getpixel((x + width * 3 // 4, y + 2)), panel.TINT)

                    popup.set_status("Signing", "busy", "muted", progressing=True)
                    with mock.patch.object(panel.time, "monotonic", side_effect=[0, 1.5]):
                        start, end = popup.frame(), popup.frame()
                    self.assertEqual(start.getpixel((x + 20, y + 2)), panel.ACCENT)
                    self.assertEqual(end.getpixel((x + 20, y + 2)), panel.TINT)

                    popup.set_status("Signing failed", "warn", "warn")
                    popup.set_onboarding("retry", "ok", retry=True)
                    bar.reset_mock()
                    popup.frame()
                    bar.assert_not_called()
                    self.assertIn(("pair",), [hit[4] for hit in popup.hits])


if __name__ == "__main__":
    unittest.main()
