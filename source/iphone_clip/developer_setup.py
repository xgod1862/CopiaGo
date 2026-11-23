"""Prepare the iPhone developer image with bounded downloads and actionable setup errors."""
from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable
from pathlib import Path
import plistlib
import time
from typing import TYPE_CHECKING, Literal, TypeVar
from xml.parsers.expat import ExpatError

import requests
from urllib3.exceptions import ReadTimeoutError

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
    "install": "正在将开发组件安装到 iPhone…",
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


def _download_parts(directory: str, names: dict[str, str], deadline: float) -> dict[str, bytes]:
    from developer_disk_image.repo import DEFAULT_REF, DEVELOPER_DISK_IMAGE_REPO_RAW_URL_FORMAT

    parts = {}
    with requests.Session() as session:
        for local, remote in names.items():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError()
            url = DEVELOPER_DISK_IMAGE_REPO_RAW_URL_FORMAT.format(ref=DEFAULT_REF, path=f"{directory}/{remote}")
            with session.get(url, stream=True, timeout=tuple(min(limit, remaining) for limit in HTTP_TIMEOUT)) as response:
                response.raise_for_status()
                payload = bytearray()
                for chunk in response.iter_content(chunk_size=64 * 1024):
                    if time.monotonic() >= deadline:
                        raise TimeoutError()
                    payload.extend(chunk)
                if not payload:
                    raise ValueError("Empty developer image payload")
                parts[local] = bytes(payload)
    return parts


async def load_image(cryptex: bool) -> Path:
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

    parts = await asyncio.to_thread(_download_parts, source, names, time.monotonic() + DOWNLOAD_TIMEOUT)
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

    await _run_stage("install", check_device(), INSTALL_TIMEOUT)
    cryptex = uses_cryptex_image(rsd)
    progress(message(PROGRESS["download"]))
    folder = await _run_stage("download", load_image(cryptex), DOWNLOAD_TIMEOUT)
    progress(message(PROGRESS["install"]))
    await _run_stage("install", _install_image(rsd, folder, cryptex, progress), INSTALL_TIMEOUT)
