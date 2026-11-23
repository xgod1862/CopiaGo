import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from iphone_clip import settings


class DataLocationTests(unittest.TestCase):
    """Installed builds keep data under the install folder and move it from LOCALAPPDATA once."""

    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.app = self.root / "install"
        self.app.mkdir()
        self.local = self.root / "local"
        env = {key: value for key, value in os.environ.items() if key != "ICB_DATA_DIR"}
        env["LOCALAPPDATA"] = str(self.local)
        self.enterContext(mock.patch.dict(os.environ, env, clear=True))
        self.enterContext(mock.patch.object(sys, "frozen", True, create=True))
        self.enterContext(mock.patch.object(sys, "executable", str(self.app / "iPhoneClipboard.exe")))
        self.enterContext(mock.patch.object(settings, "account_folder_name", return_value="alice"))
        self.restrict = self.enterContext(mock.patch.object(settings, "restrict_to_current_user"))
        self.enterContext(mock.patch.object(settings, "_active", None))
        self.target = self.app / "data" / "alice"
        self.legacy = self.local / settings.LEGACY_DIR_NAME

    def make_legacy(self):
        (self.legacy / "device-data").mkdir(parents=True)
        (self.legacy / "device-data" / "remote_x.plist").write_bytes(b"sealed")
        (self.legacy / "settings.json").write_text('{"push": true}', encoding="utf-8")
        (self.legacy / "history.sqlite3").write_bytes(b"db")
        (self.legacy / "received" / "20260101-x").mkdir(parents=True)
        (self.legacy / "received" / "20260101-x" / "big.mov").write_bytes(b"plain")

    def test_fresh_install_uses_a_restricted_account_folder(self):
        self.assertEqual(settings.prepare_data_dir(), self.target)
        self.assertTrue(self.target.is_dir())
        self.assertEqual(settings.data_dir(), self.target)
        self.restrict.assert_called_once_with(self.target)

    def test_legacy_data_moves_and_received_files_stay_behind(self):
        self.make_legacy()
        self.assertEqual(settings.prepare_data_dir(), self.target)
        self.assertEqual((self.target / "device-data" / "remote_x.plist").read_bytes(), b"sealed")
        self.assertTrue(settings.push_enabled())
        self.assertTrue((self.target / "history.sqlite3").is_file())
        self.assertFalse((self.target / "received").exists())
        self.assertFalse(self.legacy.exists())
        self.assertFalse((self.app / "data" / "alice.migrating").exists())
        # Permissions are set on the staging folder before any file is copied into it.
        self.restrict.assert_called_once_with(self.app / "data" / "alice.migrating")

    def test_existing_new_data_is_never_overwritten(self):
        self.make_legacy()
        self.target.mkdir(parents=True)
        (self.target / "settings.json").write_text('{"push": false}', encoding="utf-8")
        self.assertEqual(settings.prepare_data_dir(), self.target)
        self.assertFalse(settings.push_enabled())
        self.assertTrue(self.legacy.is_dir())

    def test_failed_move_keeps_legacy_data_for_this_run_and_retries(self):
        self.make_legacy()
        with mock.patch.object(settings.shutil, "copy2", side_effect=OSError("disk full")):
            self.assertEqual(settings.prepare_data_dir(), self.legacy)
        self.assertEqual(settings.data_dir(), self.legacy)
        self.assertTrue((self.legacy / "settings.json").is_file())
        self.assertFalse((self.app / "data" / "alice.migrating").exists())
        self.assertFalse(self.target.exists())
        self.assertEqual(settings.prepare_data_dir(), self.target)
        self.assertFalse(self.legacy.exists())

    def test_unwritable_install_folder_falls_back_to_legacy(self):
        self.restrict.side_effect = OSError("access denied")
        self.assertEqual(settings.prepare_data_dir(), self.legacy)
        self.assertFalse(self.target.exists())  # not reused next time without restricted permissions

    def test_portable_build_keeps_its_flat_folder_and_skips_migration(self):
        self.make_legacy()
        (self.app / "portable-data").write_text("", encoding="utf-8")
        self.assertEqual(settings.prepare_data_dir(), self.app / "data")
        self.assertTrue(self.legacy.is_dir())
        self.restrict.assert_not_called()

    def test_source_runs_keep_using_localappdata(self):
        with mock.patch.object(sys, "frozen", False):
            self.assertEqual(settings.prepare_data_dir(), self.legacy)
        self.restrict.assert_not_called()

    def test_override_wins(self):
        with mock.patch.dict(os.environ, {"ICB_DATA_DIR": str(self.root / "custom")}):
            self.assertEqual(settings.prepare_data_dir(), self.root / "custom")
        self.restrict.assert_not_called()

class AccountNameTests(unittest.TestCase):
    def test_account_names_are_made_safe_for_folders(self):
        with mock.patch.dict(sys.modules, {"win32api": None}), \
                mock.patch.dict(os.environ, {"USERNAME": 'a:b*c. '}):
            self.assertEqual(settings.account_folder_name(), "a_b_c")
        with mock.patch.dict(sys.modules, {"win32api": None}), mock.patch.dict(os.environ, {"USERNAME": ""}):
            self.assertEqual(settings.account_folder_name(), "user")

if __name__ == "__main__":
    unittest.main()
