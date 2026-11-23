import asyncio
import hashlib
import os
import plistlib
import shutil
from pathlib import Path
import tempfile
import threading
import time
import unittest
from types import SimpleNamespace
from unittest import mock

import requests
from urllib3.exceptions import ReadTimeoutError

from iphone_clip import developer_setup, device
from iphone_clip.device import Bridge
from pymobiledevice3 import common, lockdown
from pymobiledevice3.exceptions import AlreadyMountedError, DeveloperModeIsNotEnabledError
from pymobiledevice3.remote.core_device.pasteboard_service import PasteboardService
from pymobiledevice3.remote import tunnel_service, userspace_tunnel
from pymobiledevice3.services import cryptexd, mobile_image_mounter


class DeveloperSetupRetryTests(unittest.IsolatedAsyncioTestCase):
    async def test_usb_pairing_reports_when_the_phone_confirms_trust(self):
        events = []
        waiting = asyncio.Event()
        confirmed = asyncio.Event()
        client = mock.AsyncMock(udid="paired-phone")
        client.get_enable_wifi_connections.return_value = True
        service = mock.AsyncMock()

        async def trust(**_):
            waiting.set()
            await confirmed.wait()
            return client

        with (
            mock.patch.object(device, "usb_identifiers", return_value=["paired-phone"]),
            mock.patch.object(device, "remember_device"),  # not into the real data folder
            mock.patch.object(lockdown, "create_using_usbmux", side_effect=trust),
            mock.patch.object(tunnel_service.RemotePairingLockdownService, "create", return_value=service),
        ):
            task = asyncio.create_task(device.pair_over_usb(events.append))
            try:
                await asyncio.wait_for(waiting.wait(), timeout=1)
                self.assertEqual(len(events), 1)
                self.assertIn("正在等待 iPhone 信任", events[0].text)
                service.connect.assert_not_awaited()
            finally:
                confirmed.set()
                result = await asyncio.wait_for(task, timeout=1)
            self.assertEqual(result, "paired-phone")
            self.assertIn("已确认 iPhone 信任", events[1].text)
            service.connect.assert_awaited_once_with(autopair=True)

    async def test_download_timeout_keeps_pairing_and_waits_for_retry(self):
        messages = []
        waiting = asyncio.Event()
        connected = asyncio.Event()
        bridge = None

        def status(state, event):
            if state == "device":
                return  # a DeviceInfo for the panel, not a status message
            messages.append((state, event.text))
            if state == "hold":
                waiting.set()
            elif state == "connected":
                bridge.stop()
                connected.set()

        bridge = Bridge(status, lambda *_: None)
        tunnel = mock.MagicMock()
        tunnel.__aenter__.side_effect = [
            SimpleNamespace(peer_info={"Services": {}}),
            SimpleNamespace(peer_info={"Services": {PasteboardService.SERVICE_NAME: {}}}),
        ]

        async def monitor(*_, wireless):
            status("connected", device.CONNECTED_WIFI)

        mounter = mock.MagicMock()
        mounter.__aenter__.return_value.raise_if_cannot_mount = mock.AsyncMock()

        with (
            mock.patch.object(device, "paired_identifiers", return_value=["paired-phone"]),
            mock.patch.object(device, "usb_identifiers", mock.AsyncMock(return_value=["paired-phone"])) as usb,
            mock.patch.object(device, "configure_pairing_storage"),
            mock.patch.object(device.logging, "disable"),
            # Keeps the test from saving device details into the real data folder.
            mock.patch.object(device, "read_device_info", mock.AsyncMock(return_value=device.DeviceInfo())),
            mock.patch.object(bridge, "_monitor", side_effect=monitor),
            mock.patch.object(userspace_tunnel, "UserspaceRsdTunnel", return_value=tunnel) as open_tunnel,
            mock.patch.object(mobile_image_mounter, "PersonalizedImageMounter", return_value=mounter),
            mock.patch.object(mobile_image_mounter, "uses_cryptex_image", return_value=True),
            mock.patch.object(developer_setup, "load_image", side_effect=requests.Timeout),
        ):
            task = asyncio.create_task(bridge.run())
            try:
                await asyncio.wait_for(waiting.wait(), timeout=1)
                self.assertEqual(messages[-1], ("hold", developer_setup.FAILURES["download"][1]))
                self.assertEqual(open_tunnel.call_count, 1)
                usb.assert_not_awaited()  # Switching cables cannot fix a PC download timeout.
                bridge.request_pair()
                await asyncio.wait_for(connected.wait(), timeout=1)
                self.assertEqual(open_tunnel.call_count, 2)
            finally:
                bridge.stop()
                await asyncio.wait_for(task, timeout=1)


class FakeResponse:
    def __init__(self, status=200, body=b"", headers=None, fail_after=None):
        self.status_code = status
        self.headers = headers or {}
        self.body = body
        self.fail_after = fail_after

    def __enter__(self):
        return self

    def __exit__(self, *_):
        return False

    def raise_for_status(self):
        if self.status_code >= 400:
            raise requests.HTTPError(response=self)

    def iter_content(self, chunk_size):
        for start in range(0, len(self.body), 3):
            if self.fail_after is not None and start >= self.fail_after:
                raise requests.ConnectionError(ReadTimeoutError(None, None, "read timed out"))
            yield self.body[start:start + 3]


class FakeSession:
    def __init__(self, *responses):
        self.responses = list(responses)
        self.calls = []

    def __enter__(self):
        return self

    def __exit__(self, *_):
        return False

    def get(self, url, **kwargs):
        self.calls.append((url, kwargs))
        response = self.responses.pop(0)
        if isinstance(response, Exception):
            raise response
        return response


BODY = b"abcdef"


def pin(**files):
    """Pin test files by content: pin(**{"Image.dmg": b"..."})."""
    return mock.patch.dict(developer_setup.PINNED_FILES, {"test": {
        name: (len(data), hashlib.sha256(data).hexdigest()) for name, data in files.items()}})


def fetch(session, staging, deadline=None, cancel=None, stats=None, events=None, names=None, preferred=None):
    stats = stats if stats is not None else developer_setup.DownloadStats()
    with mock.patch.object(developer_setup.requests, "Session", return_value=session):
        developer_setup._download_parts(
            "test", names or {"Image.dmg": "Image.dmg"}, staging,
            developer_setup.time.monotonic() + 60 if deadline is None else deadline,
            cancel or threading.Event(), stats, None if events is None else events.append, preferred)
    return stats


def host(call):
    return [name for name, _ in developer_setup.SOURCES if call[0].startswith(
        dict(developer_setup.SOURCES)[name].split("{")[0])][0]


class PinnedImageTests(unittest.TestCase):
    def test_pinned_build_matches_the_library(self):
        # Upgrading pymobiledevice3 without updating DDI_REF and PINNED_FILES would download an
        # image the library does not expect, and refresh the cache on every connection.
        self.assertEqual(developer_setup.DDI_BUILD, mobile_image_mounter.LATEST_DDI_BUILD_ID)
        self.assertRegex(developer_setup.DDI_REF, r"^[0-9a-f]{40}$")

    def test_every_file_the_loader_requests_is_pinned(self):
        from developer_disk_image.repo import CRYPTEX_IMAGE_PAYLOADS, CRYPTEX_IMAGE_ROOT

        self.assertEqual(set(developer_setup.PINNED_FILES[CRYPTEX_IMAGE_ROOT]),
                         {developer_setup.MANIFEST, *CRYPTEX_IMAGE_PAYLOADS.values()})
        self.assertEqual(set(developer_setup.PINNED_FILES["PersonalizedImages/Xcode_iOS_DDI_Personalized"]),
                         {developer_setup.MANIFEST, "Image.dmg", "Image.dmg.trustcache"})
        for files in developer_setup.PINNED_FILES.values():
            for size, digest in files.values():
                self.assertGreater(size, 0)
                self.assertRegex(digest, r"^[0-9a-f]{64}$")

    def test_every_source_uses_the_pinned_commit(self):
        for _, template in developer_setup.SOURCES:
            url = template.format(repo=developer_setup.DDI_REPO, ref=developer_setup.DDI_REF, path="x/y")
            self.assertIn(developer_setup.DDI_REF, url)
            self.assertTrue(url.startswith("https://"))
            self.assertNotIn("/main/", url)


class ImageDownloadTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory()
        self.addCleanup(self.root.cleanup)
        self.staging = Path(self.root.name) / "Image.download"
        pinned = pin(**{"Image.dmg": BODY})
        pinned.start()
        self.addCleanup(pinned.stop)

    async def test_retry_refreshes_an_incomplete_cache_then_reuses_the_complete_image(self):
        manifest = plistlib.dumps({"ProductBuildVersion": developer_setup.DDI_BUILD})
        files = {developer_setup.MANIFEST: manifest, "Image.dmg": b"image", "Image.trustcache": b"trust"}
        remote = {developer_setup.MANIFEST: manifest, "Image.dmg": b"image", "Image.dmg.trustcache": b"trust"}
        key = "PersonalizedImages/Xcode_iOS_DDI_Personalized"
        attempts = []

        def download(_source, names, staging, _deadline, _cancel, stats, *_):
            attempts.append(staging)
            if len(attempts) == 1:
                raise requests.Timeout()
            staging.mkdir(parents=True, exist_ok=True)
            for name in names:
                (staging / name).write_bytes(files[name])
            stats.source = "ghfast.top"

        with tempfile.TemporaryDirectory() as root:
            folder = Path(root) / "Xcode_iOS_DDI_Personalized"
            folder.mkdir()
            (folder / developer_setup.MANIFEST).write_bytes(manifest)

            with (
                mock.patch.dict(developer_setup.PINNED_FILES, {key: {
                    name: (len(data), hashlib.sha256(data).hexdigest()) for name, data in remote.items()}}),
                mock.patch.object(common, "get_home_folder", return_value=Path(root)),
                mock.patch.object(developer_setup, "_download_parts", side_effect=download) as parts,
            ):
                with self.assertRaises(requests.Timeout):
                    await developer_setup.load_image(False)
                self.assertFalse((folder / "Image.dmg").exists())
                self.assertEqual(await developer_setup.load_image(False), folder)
                self.assertEqual((folder / "Image.dmg").read_bytes(), b"image")
                self.assertFalse((Path(root) / "Xcode_iOS_DDI_Personalized.download").exists())
                # The source that worked is tried first next time.
                self.assertEqual((Path(root) / "developer-image-source").read_text(), "ghfast.top")
                progress = mock.Mock()
                self.assertEqual(await developer_setup.load_image(False, progress), folder)
                progress.assert_not_called()
                self.assertEqual(parts.call_count, 2)
                # A cached file with the wrong size is downloaded again.
                (folder / "Image.dmg").write_bytes(b"imag")
                await developer_setup.load_image(False)
                self.assertEqual(parts.call_count, 3)
                self.assertEqual(parts.call_args.args[-1], "ghfast.top")

    def test_download_uses_a_stall_timeout_and_the_pinned_commit(self):
        session = FakeSession(FakeResponse(body=BODY))
        fetch(session, self.staging)
        self.assertEqual((self.staging / "Image.dmg").read_bytes(), BODY)
        url, kwargs = session.calls[0]
        self.assertEqual(url, f"https://raw.githubusercontent.com/doronz88/DeveloperDiskImage/"
                              f"{developer_setup.DDI_REF}/test/Image.dmg")
        self.assertEqual(kwargs["timeout"], developer_setup.DOWNLOAD_HTTP_TIMEOUT)
        self.assertGreaterEqual(developer_setup.DOWNLOAD_TIMEOUT, 10 * 60)

    def test_progress_uses_the_pinned_size(self):
        events = []
        with mock.patch.object(developer_setup, "time", SimpleNamespace(
                monotonic=mock.Mock(side_effect=[0, 1, 2, 3, 4, 5]))):
            fetch(FakeSession(FakeResponse(body=BODY, headers={"Content-Encoding": "gzip"})), self.staging,
                  deadline=100, events=events)
        self.assertEqual([event.fraction for event in events], [0, 0.5, 1])
        self.assertEqual([event.values["received"] for event in events], ["0 B", "3 B", "6 B"])
        self.assertEqual(events[-1].values["total"], "6 B")

    def test_interrupted_file_resumes_on_the_next_source(self):
        session = FakeSession(FakeResponse(body=BODY, fail_after=3),
                              FakeResponse(206, b"def", {"Content-Range": "bytes 3-5/6"}))
        events = []
        stats = fetch(session, self.staging, events=events)
        self.assertEqual([host(call) for call in session.calls], ["raw.githubusercontent.com", "ghfast.top"])
        self.assertNotIn("Range", session.calls[0][1]["headers"])
        self.assertEqual(session.calls[1][1]["headers"]["Range"], "bytes=3-")
        self.assertEqual((self.staging / "Image.dmg").read_bytes(), BODY)
        self.assertEqual((stats.resumed, stats.fetched, stats.restarts), (3, 6, 0))
        self.assertEqual(stats.failures, ["raw.githubusercontent.com:ConnectionError"])
        self.assertEqual(stats.source, "ghfast.top")
        switch = [event for event in events if "备用下载源" in event.text]
        self.assertEqual(len(switch), 1)
        self.assertEqual(switch[0].fraction, 0.5)  # The bar keeps its place while switching.

    def test_part_left_by_an_earlier_attempt_resumes_with_range(self):
        self.staging.mkdir()
        (self.staging / ".ref").write_text(developer_setup.DDI_REF)
        (self.staging / "Image.dmg.part").write_bytes(b"abcd")
        session = FakeSession(FakeResponse(206, b"ef", {"Content-Range": "bytes 4-5/6"}))
        stats = fetch(session, self.staging)
        self.assertEqual(session.calls[0][1]["headers"]["Range"], "bytes=4-")
        self.assertEqual((self.staging / "Image.dmg").read_bytes(), BODY)
        self.assertEqual(stats.resumed, 4)

    def test_tampered_or_oversized_body_moves_to_the_next_source(self):
        for bad in (b"abcxyz", b"abcdefgh"):
            with self.subTest(bad=bad):
                shutil.rmtree(self.staging, ignore_errors=True)
                session = FakeSession(FakeResponse(body=bad), FakeResponse(body=BODY))
                stats = fetch(session, self.staging)
                self.assertEqual((self.staging / "Image.dmg").read_bytes(), BODY)
                self.assertEqual(stats.failures, ["raw.githubusercontent.com:IntegrityError"])
                # The bad part is gone, so the next source started from the first byte.
                self.assertNotIn("Range", session.calls[1][1]["headers"])

    def test_source_without_range_support_sends_the_whole_file(self):
        self.staging.mkdir()
        (self.staging / ".ref").write_text(developer_setup.DDI_REF)
        (self.staging / "Image.dmg.part").write_bytes(b"abc")
        stats = fetch(FakeSession(FakeResponse(200, BODY)), self.staging)
        self.assertEqual((self.staging / "Image.dmg").read_bytes(), BODY)
        self.assertEqual(stats.restarts, 1)

    def test_unusable_range_response_moves_on_and_starts_over(self):
        for first in (FakeResponse(416), FakeResponse(206, b"f", {"Content-Range": "bytes 5-5/6"})):
            with self.subTest(status=first.status_code):
                shutil.rmtree(self.staging, ignore_errors=True)
                self.staging.mkdir()
                (self.staging / ".ref").write_text(developer_setup.DDI_REF)
                (self.staging / "Image.dmg.part").write_bytes(b"abc")
                session = FakeSession(first, FakeResponse(206, b"def", {"Content-Range": "bytes 3-5/6"}),
                                      FakeResponse(200, BODY))
                fetch(session, self.staging)
                self.assertEqual((self.staging / "Image.dmg").read_bytes(), BODY)

    def test_all_sources_failing_raises_the_last_error(self):
        session = FakeSession(requests.ConnectionError(), FakeResponse(403),
                              requests.ConnectionError(ReadTimeoutError(None, None, "slow")))
        stats = developer_setup.DownloadStats()
        with self.assertRaises(requests.ConnectionError) as failure:
            fetch(session, self.staging, stats=stats)
        self.assertIsInstance(failure.exception.args[0], ReadTimeoutError)
        self.assertEqual(len(stats.failures), len(developer_setup.SOURCES))
        self.assertIn("ghfast.top:HTTPError", stats.failures)

    def test_preferred_source_is_tried_first_and_the_working_source_sticks(self):
        with pin(**{"a": b"aaa", "b": b"bbb"}):
            session = FakeSession(requests.ConnectionError(), FakeResponse(body=b"aaa"), FakeResponse(body=b"bbb"))
            fetch(session, self.staging, names={"a": "a", "b": "b"}, preferred="fastly.jsdelivr.net")
        self.assertEqual([host(call) for call in session.calls],
                         ["fastly.jsdelivr.net", "raw.githubusercontent.com", "raw.githubusercontent.com"])

    def test_parts_for_another_commit_are_discarded(self):
        self.staging.mkdir()
        (self.staging / ".ref").write_text("0" * 40)
        (self.staging / "Image.dmg").write_bytes(b"old")
        fetch(FakeSession(FakeResponse(body=BODY)), self.staging)
        self.assertEqual((self.staging / "Image.dmg").read_bytes(), BODY)

    def test_completed_files_from_an_earlier_attempt_are_not_downloaded_again(self):
        self.staging.mkdir()
        (self.staging / ".ref").write_text(developer_setup.DDI_REF)
        (self.staging / "Image.dmg").write_bytes(BODY)
        session = FakeSession()
        fetch(session, self.staging)
        self.assertEqual(session.calls, [])

    def test_canceled_attempt_stops_reading(self):
        cancel = threading.Event()
        cancel.set()
        with self.assertRaises(developer_setup.DownloadCanceled):
            fetch(FakeSession(FakeResponse(body=BODY)), self.staging, cancel=cancel)

    def test_trickled_body_cannot_hold_the_thread_past_the_deadline(self):
        class TrickleRaw:
            calls = 0

            def read1(self, amt, decode_content=None):
                TrickleRaw.calls += 1
                developer_setup.time.sleep(0.01)
                return b"a"  # Always just in time for the socket timeout, never a full chunk.

        response = FakeResponse(body=BODY)
        response.raw = TrickleRaw()
        started = developer_setup.time.monotonic()
        with pin(**{"Image.dmg": b"a" * 10_000_000}), self.assertRaises(TimeoutError):
            fetch(FakeSession(response), self.staging, deadline=started + 0.1)
        self.assertLess(developer_setup.time.monotonic() - started, 1)
        self.assertGreater(TrickleRaw.calls, 1)

    def test_socket_timeout_from_read1_is_reported_as_a_timeout(self):
        class SilentRaw:
            def read1(self, amt, decode_content=None):
                raise ReadTimeoutError(None, None, "read timed out")

        responses = []
        for _ in developer_setup.SOURCES:
            response = FakeResponse(body=BODY)
            response.raw = SilentRaw()
            responses.append(response)
        with self.assertRaises(requests.ConnectionError) as failure:
            fetch(FakeSession(*responses), self.staging)
        self.assertIsInstance(failure.exception.args[0], ReadTimeoutError)

    def test_bad_prefix_from_one_source_does_not_waste_the_next_source(self):
        session = FakeSession(
            FakeResponse(200, b"abX"),  # short and wrong: kept as a part, then resumed
            FakeResponse(206, b"def", {"Content-Range": "bytes 3-5/6"}),  # correct suffix
            FakeResponse(200, BODY),  # same source again, from byte zero
        )
        stats = fetch(session, self.staging)
        self.assertEqual((self.staging / "Image.dmg").read_bytes(), BODY)
        self.assertEqual([host(call) for call in session.calls],
                         ["raw.githubusercontent.com", "ghfast.top", "ghfast.top"])
        self.assertNotIn("Range", session.calls[2][1]["headers"])
        self.assertEqual(stats.failures, ["raw.githubusercontent.com:ConnectionError",
                                          "ghfast.top:IntegrityError(resumed)"])

    def test_overall_deadline_does_not_try_other_sources(self):
        session = FakeSession(FakeResponse(body=BODY), FakeResponse(body=BODY))
        with self.assertRaises(TimeoutError):
            fetch(session, self.staging, deadline=developer_setup.time.monotonic() - 1)
        self.assertEqual(session.calls, [])


class DaemonThreadTests(unittest.IsolatedAsyncioTestCase):
    async def test_download_and_signing_run_in_daemon_threads(self):
        # Executor workers are joined at interpreter exit, so quitting mid-download would hang.
        seen = []

        def download(*_):
            seen.append(threading.current_thread().daemon)
            raise requests.ConnectionError()

        with (
            tempfile.TemporaryDirectory() as root,
            mock.patch.object(common, "get_home_folder", return_value=Path(root)),
            mock.patch.object(developer_setup, "_download_parts", side_effect=download),
            self.assertRaises(requests.ConnectionError),
        ):
            await developer_setup.load_image(False)
        self.assertEqual(seen, [True])

    async def test_result_error_and_cancellation_are_delivered(self):
        self.assertEqual(await developer_setup._run_in_daemon_thread(lambda a, b: a + b, 2, 3), 5)
        with self.assertRaises(ValueError):
            await developer_setup._run_in_daemon_thread(int, "x")
        release = threading.Event()
        task = asyncio.create_task(developer_setup._run_in_daemon_thread(release.wait, 2))
        await asyncio.sleep(0.01)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        release.set()  # The late result is dropped without raising InvalidStateError.
        await asyncio.sleep(0.05)

    def test_quitting_does_not_wait_for_a_running_download(self):
        import subprocess
        import sys

        script = (
            "import asyncio, threading, time\n"
            "from iphone_clip import developer_setup as d\n"
            "started = threading.Event()\n"
            "def slow():\n"
            "    started.set(); time.sleep(60)\n"
            "async def main():\n"
            "    task = asyncio.create_task(d._run_in_daemon_thread(slow))\n"
            "    await asyncio.to_thread(started.wait, 5)\n"
            "    task.cancel()\n"
            "asyncio.run(main())\n"
        )
        started = time.monotonic()
        subprocess.run([sys.executable, "-c", script], cwd=Path(__file__).resolve().parents[1],
                       check=True, timeout=30)
        self.assertLess(time.monotonic() - started, 20)


class SetupLogTests(unittest.IsolatedAsyncioTestCase):
    async def test_failure_log_has_stage_bytes_and_types_but_no_exception_text(self):
        stats = developer_setup.DownloadStats(file="Image.dmg", index=2, count=3, received=4096,
                                              total=8192, resumed=1024, fetched=3072, http_status=200)

        async def download():
            raise requests.ConnectionError(ReadTimeoutError(None, None, r"C:\Users\secret-name"))

        with tempfile.TemporaryDirectory() as root, mock.patch.dict(os.environ, {"ICB_DATA_DIR": root}):
            with self.assertRaises(developer_setup.DeveloperSetupError):
                await developer_setup._run_stage("download", download(), 1, stats)
            line = (Path(root) / developer_setup.LOG_NAME).read_text("utf-8")
        self.assertIn(" download timeout ", line)
        for field in ("error=ConnectionError", "part=2/3", "received=4096", "total=8192",
                      "resumed=1024", "fetched=3072", "http=200"):
            self.assertIn(field, line)
        self.assertNotIn("secret-name", line)

    async def test_log_failure_does_not_hide_the_setup_error(self):
        async def fail():
            raise OSError()

        with tempfile.TemporaryDirectory() as root:
            blocker = Path(root) / "not-a-folder"
            blocker.write_bytes(b"")
            with mock.patch.dict(os.environ, {"ICB_DATA_DIR": str(blocker / "data")}):
                with self.assertRaises(developer_setup.DeveloperSetupError) as failure:
                    await developer_setup._run_stage("install", fail(), 1)
        self.assertEqual(failure.exception.stage, "install")


class DeveloperSetupStageTests(unittest.IsolatedAsyncioTestCase):
    async def test_first_setup_reports_signing_and_installation_errors_separately(self):
        for cryptex in (True, False):
            for refused in (True, False):
                with self.subTest(cryptex=cryptex, refused=refused):
                    service_module = cryptexd if cryptex else mobile_image_mounter
                    real_request = service_module.TSSRequest
                    statuses = []
                    mounter = mock.MagicMock()
                    device_service = mounter.__aenter__.return_value
                    device_service.raise_if_cannot_mount = mock.AsyncMock()

                    async def install(*_):
                        request = service_module.TSSRequest()
                        await request.send_receive()
                        if not cryptex:
                            await device_service.upload_image()
                        raise OSError("device disconnected")

                    installer = mock.Mock(auto_install_ddi=mock.AsyncMock(side_effect=install))
                    device_service.mount = mock.AsyncMock(side_effect=install)
                    key = "Cryptex1,Ticket" if cryptex else "ApImg4Ticket"
                    signature = b"STATUS=0&MESSAGE=SUCCESS&REQUEST_STRING=" + plistlib.dumps({key: b"ticket"})
                    response = mock.Mock(content=b"STATUS=94&MESSAGE=REFUSED" if refused else signature)

                    with (
                        mock.patch.object(mobile_image_mounter, "PersonalizedImageMounter", return_value=mounter),
                        mock.patch.object(mobile_image_mounter, "uses_cryptex_image", return_value=cryptex),
                        mock.patch.object(developer_setup, "load_image", return_value=Path("cached-ddi")),
                        mock.patch.object(cryptexd, "CryptexdService", return_value=installer),
                        mock.patch("iphone_clip.developer_setup.requests.post", return_value=response) as post,
                        self.assertRaises(developer_setup.DeveloperSetupError) as failure,
                    ):
                        await developer_setup.prepare_developer_services(mock.Mock(), lambda event: statuses.append(event.text))
                    self.assertEqual(failure.exception.stage, "sign" if refused else "install")
                    signing = statuses.index(developer_setup.PROGRESS["sign"])
                    if refused:
                        self.assertNotIn(developer_setup.PROGRESS["install"], statuses)
                    else:
                        self.assertEqual(statuses[signing + 1], developer_setup.PROGRESS["install"])
                    self.assertEqual(post.call_args.kwargs["timeout"], developer_setup.HTTP_TIMEOUT)
                    self.assertIs(service_module.TSSRequest, real_request)

    async def test_preserves_developer_mode_prompt_and_does_not_download_if_image_is_mounted(self):
        for error in (DeveloperModeIsNotEnabledError, AlreadyMountedError):
            with self.subTest(error=error):
                mounter = mock.MagicMock()
                mounter.__aenter__.return_value.raise_if_cannot_mount = mock.AsyncMock(side_effect=error)
                with (
                    mock.patch.object(mobile_image_mounter, "PersonalizedImageMounter", return_value=mounter),
                    mock.patch.object(developer_setup, "load_image") as download,
                    self.assertRaises(error),
                ):
                    await developer_setup.prepare_developer_services(mock.Mock(), mock.Mock())
                download.assert_not_awaited()


    async def test_stream_read_timeout_is_reported_as_download_timeout(self):
        async def download():
            raise requests.ConnectionError(ReadTimeoutError(None, None, "read timed out"))

        with self.assertRaises(developer_setup.DeveloperSetupError) as failure:
            await developer_setup._run_stage("download", download(), 1)
        self.assertEqual(failure.exception.status.text, developer_setup.FAILURES["download"][1])

    async def test_late_download_does_not_report_progress_or_write_cache_after_deadline(self):
        started = threading.Event()
        release = threading.Event()
        finished = threading.Event()

        def slow_download(_source, _names, staging, _deadline, cancel, _stats, report, _preferred):
            started.set()
            release.wait(timeout=2)
            report(developer_setup.DownloadProgress("late", {}, 1.0))
            self.assertTrue(cancel.is_set())  # The thread is told to stop reading.
            finished.set()

        with (
            tempfile.TemporaryDirectory() as root,
            mock.patch.object(common, "get_home_folder", return_value=Path(root)),
            mock.patch.object(developer_setup, "_download_parts", side_effect=slow_download),
        ):
            events = []
            task = asyncio.create_task(developer_setup.load_image(True, events.append))
            try:
                self.assertTrue(await asyncio.to_thread(started.wait, 1))
                with self.assertRaises(developer_setup.DeveloperSetupError) as failure:
                    await developer_setup._run_stage("download", task, 0.02)
                self.assertEqual(failure.exception.status.text, developer_setup.FAILURES["download"][1])
            finally:
                release.set()
                self.assertTrue(await asyncio.to_thread(finished.wait, 1))
            self.assertEqual([event.text for event in events], [developer_setup.PROGRESS["download"]])
            self.assertEqual(list(Path(root).iterdir()), [])
