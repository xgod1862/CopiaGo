"""Prepare the iPhone developer image with bounded downloads and actionable setup errors."""
from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from pathlib import Path
import plistlib
import time
from typing import TYPE_CHECKING, Literal, TypeVar
from xml.parsers.expat import ExpatError

import requests
from urllib3.exceptions import ReadTimeoutError

from iphone_clip.files import human_size
from iphone_clip.i18n import Message, message

if TYPE_CHECKING:
    from pymobiledevice3.remote.remote_service_discovery import RemoteServiceDiscoveryService

Stage = Literal["download", "sign", "install"]
T = TypeVar("T")
DOWNLOAD_TIMEOUT = 120
SIGN_TIMEOUT = 45
INSTALL_TIMEOUT = 240
HTTP_TIMEOUT = (10, 25)
MANIFEST = "BuildManifest.plist"
PROGRESS = {
    "download": "正在下载 iPhone 开发组件…",
    "sign": "正在请求 Apple 签名…",
    "install": "正在将开发者镜像安装到 iPhone…",
}
FAILURES = {
    "download": ("下载 iPhone 开发组件失败。请检查电脑网络后重试。",
                 "下载 iPhone 开发组件超时。请检查电脑网络后重试。"),
    "sign": ("Apple 签名失败。请稍后重试。", "请求 Apple 签名超时。请检查电脑网络后重试。"),
    "install": ("无法在 iPhone 上安装开发组件。请保持手机解锁并连着数据线后重试。",
                "安装 iPhone 开发组件超时。请保持手机解锁并连着数据线后重试。"),
}


class DeveloperSetupError(Exception):
    """Carry the stage and a translated status without exposing raw device or server data."""

    def __init__(self, stage: Stage, *, timed_out: bool = False):
        self.stage = stage
        self.status = message(FAILURES[stage][int(timed_out)])
        super().__init__(stage)


@dataclass(frozen=True)
class DownloadProgress(Message):
    """Report the current file's byte fraction when the server supplies its size."""

    fraction: float | None


async def _run_stage(stage: Stage, operation: Awaitable[T], timeout: float) -> T:
    from pymobiledevice3.exceptions import AlreadyMountedError, DeveloperModeIsNotEnabledError

    try:
        return await asyncio.wait_for(operation, timeout=timeout)
    except (AlreadyMountedError, DeveloperModeIsNotEnabledError, DeveloperSetupError):
        raise
    except Exception as exc:
        # Requests wraps a read timeout while streaming the body in ConnectionError.
        timed_out = isinstance(exc, (TimeoutError, requests.Timeout)) or (
            isinstance(exc, requests.ConnectionError) and bool(exc.args) and isinstance(exc.args[0], ReadTimeoutError)
        )
        raise DeveloperSetupError(stage, timed_out=timed_out) from exc


def _download_progress(index: int, count: int, received: int, total: int) -> DownloadProgress:
    values: dict[str, object] = {"index": index, "count": count, "received": human_size(received)}
    if 0 < total and received <= total:
        values["total"] = human_size(total)
        return DownloadProgress("正在下载文件 {index}/{count} · {received} / {total}", values, received / total)
    return DownloadProgress("正在下载文件 {index}/{count} · 已收到 {received}", values, None)


def _download_parts(directory: str, names: dict[str, str], deadline: float,
                    progress: Callable[[DownloadProgress], None] | None = None) -> dict[str, bytes]:
    from developer_disk_image.repo import DEFAULT_REF, DEVELOPER_DISK_IMAGE_REPO_RAW_URL_FORMAT

    parts = {}
    count = len(names)
    with requests.Session() as session:
        for index, (local, remote) in enumerate(names.items(), 1):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError()
            url = DEVELOPER_DISK_IMAGE_REPO_RAW_URL_FORMAT.format(ref=DEFAULT_REF, path=f"{directory}/{remote}")
            if progress is not None:
                progress(_download_progress(index, count, 0, 0))
            with session.get(url, stream=True, headers={"Accept-Encoding": "identity"},
                             timeout=tuple(min(limit, remaining) for limit in HTTP_TIMEOUT)) as response:
                response.raise_for_status()
                total = 0
                if progress is not None:
                    # Requests reports decoded bytes. An encoded or chunked length is not their total.
                    length = response.headers.get("Content-Length", "")
                    if (response.headers.get("Content-Encoding", "identity").lower() == "identity"
                            and not response.headers.get("Transfer-Encoding") and length.isascii() and length.isdecimal()):
                        total = int(length)
                    if total:
                        progress(_download_progress(index, count, 0, total))
                last_report, reported_size = time.monotonic(), 0
                payload = bytearray()
                for chunk in response.iter_content(chunk_size=64 * 1024):
                    now = time.monotonic()
                    if now >= deadline:
                        raise TimeoutError()
                    payload.extend(chunk)
                    if progress is not None and chunk and now - last_report >= 0.25:
                        reported_size = len(payload)
                        progress(_download_progress(index, count, reported_size, total))
                        last_report = now
                if not payload:
                    raise ValueError("Empty developer image payload")
                if progress is not None and reported_size != len(payload):
                    progress(_download_progress(index, count, len(payload), total))
                parts[local] = bytes(payload)
    return parts


async def load_image(cryptex: bool, progress: Callable[[Message], None] | None = None) -> Path:
    """Reuse the library's cache. A canceled download never writes to it."""
    from developer_disk_image.repo import CRYPTEX_IMAGE_PAYLOADS, CRYPTEX_IMAGE_ROOT
    from pymobiledevice3.common import get_home_folder
    from pymobiledevice3.services.mobile_image_mounter import LATEST_DDI_BUILD_ID

    source = CRYPTEX_IMAGE_ROOT if cryptex else "PersonalizedImages/Xcode_iOS_DDI_Personalized"
    folder = get_home_folder() / source.rsplit("/", 1)[-1]
    names = {MANIFEST: MANIFEST}
    if cryptex:
        names.update({name: name for name in CRYPTEX_IMAGE_PAYLOADS.values()})
    else:
        names.update({"Image.dmg": "Image.dmg", "Image.trustcache": "Image.dmg.trustcache"})
    manifest = folder / MANIFEST
    if all((folder / name).is_file() and (folder / name).stat().st_size for name in names):
        try:
            cached = plistlib.loads(manifest.read_bytes())
            if isinstance(cached, dict) and cached.get("ProductBuildVersion") == LATEST_DDI_BUILD_ID:
                return folder
        except (plistlib.InvalidFileException, ValueError, ExpatError):
            pass  # Refresh a partial or corrupt cache from an interrupted setup.

    loop = asyncio.get_running_loop()
    active = True

    def report(event: DownloadProgress) -> None:
        # The download thread may still be unwinding after this attempt times out.
        if active and progress is not None:
            progress(event)

    if progress is not None:
        progress(message(PROGRESS["download"]))
    try:
        parts = await asyncio.to_thread(
            _download_parts, source, names, time.monotonic() + DOWNLOAD_TIMEOUT,
            None if progress is None else lambda event: loop.call_soon_threadsafe(report, event))
    finally:
        active = False
    if not plistlib.loads(parts[MANIFEST]).get("ProductBuildVersion"):
        raise ValueError("Missing developer image build")
    folder.mkdir(parents=True, exist_ok=True)
    # The manifest marks a complete cache, so an interrupted write cannot leave a valid marker.
    manifest.unlink(missing_ok=True)
    for name, payload in parts.items():
        if name != MANIFEST:
            (folder / name).write_bytes(payload)
    manifest.write_bytes(parts[MANIFEST])
    return folder


def _request_apple_signature(payload: bytes, ticket_key: str):
    """Use the pinned library's Apple ticket format with a bounded HTTP request."""
    from pymobiledevice3.exceptions import TSSError
    from pymobiledevice3.restore.tss import TSS_CONTROLLER_ACTION_URL, TSSResponse

    headers = {"Cache-Control": "no-cache", "Content-type": "text/xml; charset=utf-8",
               "User-Agent": "InetURL/1.0", "Expect": "100-continue"}
    response = requests.post(TSS_CONTROLLER_ACTION_URL, headers=headers, data=payload, timeout=HTTP_TIMEOUT)
    response.raise_for_status()
    status = response.content.partition(b"MESSAGE=")[2].partition(b"&")[0]
    if status != b"SUCCESS":
        raise TSSError("Apple did not sign the developer image")
    ticket = TSSResponse(plistlib.loads(response.content.partition(b"REQUEST_STRING=")[2]))
    if not ticket.get(ticket_key) and not ticket.get("ApImg4Ticket"):
        raise TSSError("Apple did not return a developer image ticket")
    return ticket


async def _install_image(rsd: RemoteServiceDiscoveryService, folder: Path, cryptex: bool,
                         progress: Callable[[Message], None]) -> None:
    from pymobiledevice3.services import cryptexd, mobile_image_mounter

    service_module = cryptexd if cryptex else mobile_image_mounter
    original_request = service_module.TSSRequest
    ticket_key = "Cryptex1,Ticket" if cryptex else "ApImg4Ticket"

    class SetupSigningRequest(original_request):
        async def send_receive(self):
            progress(message(PROGRESS["sign"]))
            result = await _run_stage("sign", asyncio.to_thread(
                _request_apple_signature, plistlib.dumps(self._request), ticket_key), SIGN_TIMEOUT)
            if cryptex:
                progress(message(PROGRESS["install"]))
            return result

    # The pinned library has no signer callback. CopiaGo has one bridge and serializes setup
    # attempts. Scope the replacement to this install so the existing iPhone protocol stays intact.
    service_module.TSSRequest = SetupSigningRequest
    try:
        if cryptex:
            await cryptexd.CryptexdService(rsd).auto_install_ddi(folder)
        else:
            async with mobile_image_mounter.PersonalizedImageMounter(lockdown=rsd) as mounter:
                upload_image = mounter.upload_image

                async def upload(*args, **kwargs):
                    # A ticket cached on the phone skips signing but still uploads the image.
                    progress(message(PROGRESS["install"]))
                    return await upload_image(*args, **kwargs)

                mounter.upload_image = upload
                await mounter.mount(folder / "Image.dmg", folder / MANIFEST, folder / "Image.trustcache")
    finally:
        service_module.TSSRequest = original_request


async def prepare_developer_services(rsd: RemoteServiceDiscoveryService,
                                     progress: Callable[[Message], None]) -> None:
    """Download, sign and install the image needed by the connected iPhone's clipboard service."""
    from pymobiledevice3.services.mobile_image_mounter import PersonalizedImageMounter, uses_cryptex_image

    async def check_device():
        async with PersonalizedImageMounter(lockdown=rsd) as mounter:
            await mounter.raise_if_cannot_mount()

    progress(message("正在检查 iPhone 开发者服务…"))
    await _run_stage("install", check_device(), INSTALL_TIMEOUT)
    cryptex = uses_cryptex_image(rsd)
    folder = await _run_stage("download", load_image(cryptex, progress), DOWNLOAD_TIMEOUT)
    progress(message("正在准备安装开发者镜像…"))
    await _run_stage("install", _install_image(rsd, folder, cryptex, progress), INSTALL_TIMEOUT)
