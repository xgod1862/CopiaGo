import os
import tempfile
import unittest
from unittest import mock

import requests

from iphone_clip import settings, updater

PAGE = "https://github.com/xgod1862/CopiaGo/releases/tag/v1.2.0"


def release(tag="v1.2.0", url=PAGE, **extra):
    return {"tag_name": tag, "html_url": url, "draft": False, "prerelease": False, **extra}


class VersionTests(unittest.TestCase):
    def test_parse_accepts_plain_tags_only(self):
        self.assertEqual(updater.parse_version("v1.2.0"), (1, 2, 0))
        self.assertEqual(updater.parse_version("1.10.3"), (1, 10, 3))
        for text in ("v1.2", "v1.2.0-beta", "latest", "", None, 120):
            with self.subTest(text=text):
                self.assertIsNone(updater.parse_version(text))

    def test_only_a_newer_release_is_offered(self):
        self.assertEqual(updater.newer_release("1.1.0", release()), updater.Release("1.2.0", PAGE))
        # Compared as numbers, not text: 1.10.0 is newer than 1.9.0.
        self.assertIsNotNone(updater.newer_release("1.9.0", release("v1.10.0")))
        self.assertIsNone(updater.newer_release("1.2.0", release()))
        self.assertIsNone(updater.newer_release("1.3.0", release()))

    def test_drafts_prereleases_odd_tags_and_foreign_links_are_ignored(self):
        for payload in (release(draft=True), release(prerelease=True), release("v1.2.0-rc1"),
                        release(url="https://example.com/CopiaGo-Setup.exe"),
                        release(url="https://github.com/someone/else/releases/tag/v9.0.0"),
                        release(url=None), [], None):
            with self.subTest(payload=payload):
                self.assertIsNone(updater.newer_release("1.1.0", payload))

    def test_network_failures_are_quiet(self):
        failing = (requests.ConnectionError("offline"), requests.Timeout("slow"))
        for error in failing:
            with self.subTest(error=error), mock.patch.object(updater.requests, "get", side_effect=error):
                self.assertIsNone(updater.latest_release("1.1.0"))
        limited = mock.Mock(status_code=403)
        with mock.patch.object(updater.requests, "get", return_value=limited):
            self.assertIsNone(updater.latest_release("1.1.0"))
        broken = mock.Mock(status_code=200)
        broken.json.side_effect = ValueError("not JSON")
        with mock.patch.object(updater.requests, "get", return_value=broken):
            self.assertIsNone(updater.latest_release("1.1.0"))

    def test_request_names_the_app_and_has_a_timeout(self):
        response = mock.Mock(status_code=200)
        response.json.return_value = release()
        with mock.patch.object(updater.requests, "get", return_value=response) as get:
            self.assertEqual(updater.latest_release("1.1.0").version, "1.2.0")
        _, kwargs = get.call_args
        self.assertEqual(get.call_args.args[0], updater.LATEST_URL)
        self.assertEqual(kwargs["headers"]["User-Agent"], "CopiaGo/1.1.0")
        self.assertEqual(kwargs["timeout"], updater.TIMEOUT)


class SettingTests(unittest.TestCase):
    def test_check_defaults_on_and_can_be_turned_off(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.dict(os.environ, {"ICB_DATA_DIR": folder}):
            self.assertTrue(settings.update_check_enabled())
            settings.set_update_check_enabled(False)
            self.assertFalse(settings.update_check_enabled())
            settings.set_update_check_enabled(True)
            self.assertTrue(settings.update_check_enabled())


if __name__ == "__main__":
    unittest.main()
