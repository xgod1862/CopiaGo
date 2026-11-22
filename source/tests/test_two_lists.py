import sqlite3
import tempfile
import tkinter as tk
import unittest
from datetime import datetime
from pathlib import Path

from iphone_clip import history as history_module
from iphone_clip.clipboard import Clip
from iphone_clip.history import Entry, History, encrypt
from iphone_clip.popup import HistoryPopup


class TwoListHistoryTests(unittest.TestCase):
    def setUp(self):
        self.history = History(Path(tempfile.mkdtemp()) / "history.sqlite3")

    def test_lists_are_kept_and_cleared_apart(self):
        self.history.add(Clip(text="来自手机"), "phone")
        self.history.add(Clip(text="电脑上复制"), "pc")
        self.assertEqual([e.preview for e in self.history.entries("phone")], ["来自手机".encode()])
        self.assertEqual([e.preview for e in self.history.entries("pc")], ["电脑上复制".encode()])
        self.history.remove(source="pc")
        self.assertEqual(self.history.entries("pc"), [])
        self.assertEqual(len(self.history.entries("phone")), 1)

    def test_repeat_is_ignored_per_list_only(self):
        self.history.add(Clip(text="同一句"), "phone")
        self.history.add(Clip(text="同一句"), "pc")
        self.history.add(Clip(text="同一句"), "pc")
        self.assertEqual(len(self.history.entries("phone")), 1)
        self.assertEqual(len(self.history.entries("pc")), 1)

    def test_limit_applies_to_each_list(self):
        original = history_module.MAX_ITEMS
        history_module.MAX_ITEMS = 3
        try:
            for number in range(5):
                self.history.add(Clip(text=f"pc {number}"), "pc")
            self.history.add(Clip(text="phone"), "phone")
        finally:
            history_module.MAX_ITEMS = original
        self.assertEqual([e.preview.decode() for e in self.history.entries("pc")], ["pc 4", "pc 3", "pc 2"])
        self.assertEqual(len(self.history.entries("phone")), 1)

    def test_old_history_becomes_the_iphone_list(self):
        db = sqlite3.connect(self.history.path)
        db.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, created TEXT NOT NULL, kind TEXT NOT NULL, "
                   "preview BLOB NOT NULL, payload BLOB NOT NULL)")
        db.execute("INSERT INTO items (created, kind, preview, payload) VALUES (?, 'text', ?, ?)",
                   ("2026-11-20T10:00", encrypt("旧记录".encode()), encrypt("旧记录".encode())))
        db.commit()
        db.close()
        self.assertEqual([e.preview for e in self.history.entries("phone")], ["旧记录".encode()])
        self.assertEqual(self.history.entries("pc"), [])


class TwoListPopupTests(unittest.TestCase):
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
                            on_toggle=noop, on_forget=noop)

    def test_computer_items_can_be_sent_and_tabs_switch(self):
        popup = self.popup()
        now = datetime.now().isoformat(timespec="minutes")
        popup.set_entries({"phone": ([Entry(1, now, "text", "手机".encode())], True),
                           "pc": ([Entry(2, now, "text", "电脑".encode())], True)})
        popup.frame()
        actions = [hit[4] for hit in popup.hits]
        self.assertIn(("tab", "pc"), actions)
        self.assertNotIn(("send_item", 0), actions)  # iPhone items are already on the phone
        popup.page = "history"
        popup.switch_tab("pc")
        popup.frame()
        actions = [hit[4] for hit in popup.hits]
        for expected in (("send_item", 0), ("copy", 0), ("delete", 0), ("tab", "phone")):
            self.assertIn(expected, actions)

    def test_each_list_offers_its_own_switch_when_off(self):
        popup = self.popup()
        popup.set_entries({"phone": ([], True), "pc": ([], False)})
        popup.frame()
        self.assertNotIn(("enable",), [hit[4] for hit in popup.hits])
        popup.switch_tab("pc")
        popup.frame()
        self.assertIn(("enable",), [hit[4] for hit in popup.hits])


if __name__ == "__main__":
    unittest.main()
