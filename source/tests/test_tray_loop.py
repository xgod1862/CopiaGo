import logging
import os
from pathlib import Path
import queue
import tempfile
import unittest
from unittest import mock


class TrayLoopTests(unittest.TestCase):
    def setUp(self):
        from iphone_clip.ui import Window

        self.folder = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.enterContext(mock.patch.dict(os.environ, {"ICB_DATA_DIR": str(self.folder)}))
        self.window = Window.__new__(Window)
        self.window.closed = False
        self.window.root = mock.Mock()
        self.window.bridge = mock.Mock()
        self.window.tray = mock.Mock()
        self.window.popup = mock.Mock()
        self.window.events = queue.SimpleQueue()
        self.window.wake = mock.Mock(return_value=False)
        self.window.show = mock.Mock()
        self.window.check_connection = mock.Mock()
        self.window.check_pc_clipboard = mock.Mock()
        self.window.watch_ticks = 0

    def test_connection_progress_reaches_the_panel_and_stops_on_failure(self):
        from iphone_clip.developer_setup import DownloadProgress
        from iphone_clip.i18n import message

        self.window.connected = False
        self.window.paired = False
        self.window.last_connected = "10:42"
        self.window.update_onboarding = mock.Mock()
        event = DownloadProgress("Downloading", {}, 0.5)
        self.window.events.put(("connecting", event))

        self.window.drain()

        self.assertEqual(self.window.popup.set_status.call_args.kwargs, {"progressing": True, "fraction": 0.5})
        self.assertIn("Downloading", self.window.popup.set_status.call_args.args[0])

        self.window.events.put(("hold", message("Download timed out")))
        self.window.root.after.call_args.args[1]()

        self.window.popup.set_status.assert_called_with("Download timed out", "warn", "warn", "Download timed out",
                                                      progressing=False, fraction=None)
        self.window.popup.animate_progress.assert_called_once()

    def test_next_tick_can_open_and_quit_after_one_callback_fails(self):
        self.window.events.put(("show", ""))
        self.window.show.side_effect = OSError("private clipboard text")
        self.addCleanup(logging.disable, logging.root.manager.disable)
        logging.disable(logging.CRITICAL)

        self.window.drain()

        log = (self.folder / "tray-errors.log").read_text(encoding="utf-8")
        self.assertIn("OSError", log)
        self.assertIn("ui.py:", log)
        self.assertIn("drain", log)
        self.assertNotIn("private clipboard text", log)
        self.window.root.after.assert_called_once_with(100, self.window.drain)

        self.window.show.side_effect = None
        self.window.show.reset_mock()
        self.window.events.put(("show", ""))
        self.window.events.put(("quit", ""))
        self.window.root.after.call_args.args[1]()

        self.window.show.assert_called_once_with()
        self.window.tray.stop.assert_called_once_with()
        self.window.root.destroy.assert_called_once_with()
        self.assertTrue(self.window.closed)
        self.window.root.after.assert_called_once()  # Closing never schedules another tick.

    def test_failed_log_write_still_schedules_the_next_tick(self):
        blocked_path = self.folder / "not-a-directory"
        blocked_path.touch()
        self.window.check_connection.side_effect = OSError("one failed poll")
        with mock.patch.dict(os.environ, {"ICB_DATA_DIR": str(blocked_path)}):
            self.window.drain()
        self.window.root.after.assert_called_once_with(100, self.window.drain)

    def test_exit_continues_cleanup_after_bridge_stop_fails(self):
        self.window.bridge.stop.side_effect = OSError("one failed cleanup")
        self.window.events.put(("quit", ""))

        self.window.drain()

        self.window.tray.stop.assert_called_once_with()
        self.window.root.destroy.assert_called_once_with()
        self.window.root.after.assert_not_called()
        self.assertIn("bridge.stop", (self.folder / "tray-errors.log").read_text(encoding="utf-8"))
