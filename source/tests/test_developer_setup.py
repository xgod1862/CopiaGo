import asyncio
import plistlib
from pathlib import Path
import tempfile
import threading
import unittest
from types import SimpleNamespace
from unittest import mock

import requests
from urllib3.exceptions import ReadTimeoutError

from iphone_clip import developer_setup, device
from iphone_clip.device import Bridge
from pymobiledevice3 import common
from pymobiledevice3.exceptions import AlreadyMountedError, DeveloperModeIsNotEnabledError
from pymobiledevice3.remote.core_device.pasteboard_service import PasteboardService
from pymobiledevice3.remote import userspace_tunnel
from pymobiledevice3.services import cryptexd, mobile_image_mounter


class DeveloperSetupRetryTests(unittest.IsolatedAsyncioTestCase):
    async def test_download_timeout_keeps_pairing_and_waits_for_retry(self):
        messages = []
        waiting = asyncio.Event()
        connected = asyncio.Event()
        bridge = None

        def status(state, event):
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
            mock.patch.object(bridge, "_monitor", side_effect=monitor),
            mock.patch.object(userspace_tunnel, "UserspaceRsdTunnel", return_value=tunnel) as open_tunnel,
            mock.patch.object(mobile_image_mounter, "PersonalizedImageMounter", return_value=mounter),
            mock.patch.object(mobile_image_mounter, "uses_cryptex_image", return_value=True),
            mock.patch.object(developer_setup, "load_image", side_effect=requests.Timeout),
        ):
            task = asyncio.create_task(bridge.run())
            try:
                await asyncio.wait_for(waiting.wait(), timeout=1)
                self.assertEqual(messages[-1], ("hold", "下载 iPhone 开发组件超时。请检查电脑网络后重试。"))
                self.assertEqual(open_tunnel.call_count, 1)
                usb.assert_not_awaited()  # Switching cables cannot fix a PC download timeout.
                bridge.request_pair()
                await asyncio.wait_for(connected.wait(), timeout=1)
                self.assertEqual(open_tunnel.call_count, 2)
            finally:
                bridge.stop()
                await asyncio.wait_for(task, timeout=1)


class ImageDownloadTests(unittest.IsolatedAsyncioTestCase):
    async def test_retry_refreshes_an_incomplete_cache_then_reuses_the_complete_image(self):
        manifest = plistlib.dumps({"ProductBuildVersion": mobile_image_mounter.LATEST_DDI_BUILD_ID})
        with tempfile.TemporaryDirectory() as root:
            folder = Path(root) / "Xcode_iOS_DDI_Personalized"
            folder.mkdir()
            (folder / developer_setup.MANIFEST).write_bytes(manifest)

            with (
                mock.patch.object(common, "get_home_folder", return_value=Path(root)),
                mock.patch.object(developer_setup, "_download_parts", side_effect=[
                    requests.Timeout(),
                    {developer_setup.MANIFEST: manifest, "Image.dmg": b"image", "Image.trustcache": b"trust"},
                ]) as download,
            ):
                with self.assertRaises(requests.Timeout):
                    await developer_setup.load_image(False)
                self.assertFalse((folder / "Image.dmg").exists())
                self.assertEqual(await developer_setup.load_image(False), folder)
                self.assertEqual(await developer_setup.load_image(False), folder)
                self.assertEqual(download.call_count, 2)

    def test_download_sets_connection_and_read_timeouts(self):
        response = mock.MagicMock()
        response.__enter__.return_value.iter_content.return_value = [b"image"]
        with mock.patch("iphone_clip.developer_setup.requests.Session") as session:
            get = session.return_value.__enter__.return_value.get
            get.return_value = response
            result = developer_setup._download_parts(
                "PersonalizedImages/test", {"Image.dmg": "Image.dmg"}, developer_setup.time.monotonic() + 120)
        self.assertEqual(result, {"Image.dmg": b"image"})
        self.assertEqual(get.call_args.kwargs["timeout"], developer_setup.HTTP_TIMEOUT)


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
                    self.assertIn(developer_setup.PROGRESS["sign"], statuses)
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

    async def test_late_download_does_not_write_cache_after_deadline(self):
        started = threading.Event()
        release = threading.Event()
        finished = threading.Event()

        def slow_download(*_):
            started.set()
            release.wait(timeout=2)
            finished.set()
            return {developer_setup.MANIFEST: plistlib.dumps({"ProductBuildVersion": "build"})}

        with (
            tempfile.TemporaryDirectory() as root,
            mock.patch.object(common, "get_home_folder", return_value=Path(root)),
            mock.patch.object(developer_setup, "_download_parts", side_effect=slow_download),
        ):
            task = asyncio.create_task(developer_setup.load_image(True))
            try:
                self.assertTrue(await asyncio.to_thread(started.wait, 1))
                with self.assertRaises(developer_setup.DeveloperSetupError) as failure:
                    await developer_setup._run_stage("download", task, 0.02)
                self.assertEqual(failure.exception.status.text, developer_setup.FAILURES["download"][1])
            finally:
                release.set()
                self.assertTrue(await asyncio.to_thread(finished.wait, 1))
            self.assertEqual(list(Path(root).iterdir()), [])
