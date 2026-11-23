"""Prepare the iPhone developer image with resumable downloads and actionable setup errors."""
from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from datetime import datetime
import hashlib
from pathlib import Path
import plistlib
import shutil
import threading
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
# A slow link may take minutes for the ~18 MB image. Only a silent connection fails early.
DOWNLOAD_TIMEOUT = 15 * 60
STALL_TIMEOUT = 30
SIGN_TIMEOUT = 45
INSTALL_TIMEOUT = 240
HTTP_TIMEOUT = (10, 25)
DOWNLOAD_HTTP_TIMEOUT = (10, STALL_TIMEOUT)
DDI_REPO = "doronz88/DeveloperDiskImage"
# The commit that published build 27B52, which pymobiledevice3 11.61.1 expects
# (LATEST_DDI_BUILD_ID). A commit URL never changes, so every source returns the same bytes and
# PINNED_FILES can check them. Update DDI_REF, DDI_BUILD and PINNED_FILES together when upgrading
# pymobiledevice3; a test fails while they disagree with the library.
DDI_REF = "dcec53989fa57695c2b3d3e11c2585f4735b2fef"
DDI_BUILD = "27B52"
# Tried in this order, starting with the source that last delivered data. The proxy and the CDN
# are third parties without service terms, so they come after GitHub. SHA-256 checks make a
# tampered or truncated response fail like a network error and move on to the next source.
# jsDelivr refuses some files of this repository (it exceeds the CDN's package size limit).
SOURCES = (
    ("raw.githubusercontent.com", "https://raw.githubusercontent.com/{repo}/{ref}/{path}"),
    ("ghfast.top", "https://ghfast.top/https://raw.githubusercontent.com/{repo}/{ref}/{path}"),
    ("fastly.jsdelivr.net", "https://fastly.jsdelivr.net/gh/{repo}@{ref}/{path}"),
)
# (size in bytes, SHA-256) of each published file at DDI_REF, keyed by the repository folder.
PINNED_FILES: dict[str, dict[str, tuple[int, str]]] = {
    "PersonalizedImages/Xcode_iOS_DDI_Personalized": {
        "BuildManifest.plist": (800656, "3c4eee2be4a0a853044dc32dbbd62699302b1dfd317aaf4f1cf36368b7639bb0"),
        "Image.dmg": (17084416, "0de55b0ccde9d5790c9c9f4e1a413225443fce7ea75acb503aca11e7d8156646"),
        "Image.dmg.trustcache": (1851, "f0fb56da2829f69b8aae113584349635738e68d77c82a726ed72040890f16bf4"),
    },
    "PersonalizedImages/Xcode_iOS_DDI_Cryptex": {
        "BuildManifest.plist": (804097, "537391eb2f37e604fc693f422c1568af97220067853e663391ba3e8258569d6e"),
        "Image.dmg": (17239040, "500d4139f7963650e6c796d950267ed38909e7d183ad26bba073257529bfe173"),
        "Image.dmg.cryptex_info": (428, "0180ee20ce5daa7978fe38f24c742361726789ea6af82c3c1d2afc480734a531"),
        "Image.dmg.root_hash": (229, "5f3281961396be5fe008e11641ffc25c7db24c11d3789ff0c4ff0fa6d47b94ca"),
        "Image.dmg.trustcache": (1851, "2719aa3f1c99245432512116fb8a56d55ec224c706e545772d4c17c1233ac475"),
    },
}
LOG_NAME = "developer-setup.log"
MAX_LOG_BYTES = 256 * 1024
MANIFEST = "BuildManifest.plist"
PROGRESS = {
    "download": "正在下载 iPhone 开发组件…",
    "sign": "正在请求 Apple 签名…",
    "install": "正在将开发者镜像安装到 iPhone…",
}
FAILURES = {
    "download": ("下载 iPhone 开发组件失败。已下载的部分会保留，请检查电脑网络后点「继续连接」。",
                 "下载 iPhone 开发组件超时。已下载的部分会保留，请检查电脑网络后点「继续连接」。"),
    "sign": ("Apple 签名失败。请稍后重试。", "请求 Apple 签名超时。请检查电脑网络后重试。"),
    "install": ("无法在 iPhone 上安装开发组件。请保持手机解锁并连着数据线后重试。",
                "安装 iPhone 开发组件超时。请保持手机解锁并连着数据线后重试。"),
}
# One download thread at a time may write the staging folder. A canceled attempt's thread can
# still be inside a socket read for up to STALL_TIMEOUT, so the next attempt waits for it here.
_download_lock = threading.Lock()


class DeveloperSetupError(Exception):
    """Carry the stage and a translated status without exposing raw device or server data."""

    def __init__(self, stage: Stage, *, timed_out: bool = False):
        self.stage = stage
        self.status = message(FAILURES[stage][int(timed_out)])
        super().__init__(stage)


class DownloadCanceled(Exception):
    """The attempt that started this download thread has already ended."""


class IntegrityError(Exception):
    """A source returned bytes that do not match the pinned size or SHA-256."""


@dataclass(frozen=True)
class DownloadProgress(Message):
    """Report the current file's byte fraction when the server supplies its size."""

    fraction: float | None


@dataclass
class DownloadStats:
    """Byte counters for the local setup log. Written by the download thread, read afterwards."""

    file: str = ""
    index: int = 0
    count: int = 0
    received: int = 0
    total: int = 0
    resumed: int = 0
    fetched: int = 0
    restarts: int = 0
    http_status: int | None = None
    source: str = ""
    failures: list[str] = field(default_factory=list)


def record_setup_event(stage: str, outcome: str, elapsed: float, error: BaseException | None = None,
                       stats: DownloadStats | None = None) -> None:
    """Append one line to the local setup log.

    Only stage, timing, byte counts, HTTP status and exception type names are written. Exception
    text is left out because it can contain local paths, device identifiers or server data.
    """
    try:
        from iphone_clip.settings import data_dir

        path = data_dir() / LOG_NAME
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.exists() and path.stat().st_size >= MAX_LOG_BYTES:
            path.replace(path.with_suffix(".log.1"))
        fields = [datetime.now().isoformat(timespec="seconds"), stage, outcome, f"elapsed={elapsed:.1f}s"]
        if error is not None:
            names, seen, current = [], set(), error
            while current is not None and id(current) not in seen and len(names) < 4:
                seen.add(id(current))
                names.append(type(current).__name__)
                response = getattr(current, "response", None)
                if isinstance(getattr(response, "status_code", None), int):
                    names[-1] += f"[{response.status_code}]"
                current = current.__cause__ or current.__context__
            fields.append("error=" + "<-".join(names))
        if stats is not None and (stats.count or stats.fetched):
            speed = stats.fetched / elapsed / 1024 if elapsed > 0 else 0
            fields += [f"file={stats.file}", f"part={stats.index}/{stats.count}",
                       f"received={stats.received}", f"total={stats.total}", f"resumed={stats.resumed}",
                       f"fetched={stats.fetched}", f"restarts={stats.restarts}",
                       f"http={stats.http_status}", f"speed={speed:.0f}KB/s", f"source={stats.source or '-'}"]
            if stats.failures:
                fields.append("failures=" + ",".join(stats.failures[-10:]))
        with path.open("a", encoding="utf-8") as log:
            log.write(" ".join(fields) + "\n")
    except (OSError, KeyError):
        pass  # A missing or read-only data folder must not hide the setup error itself.


async def _run_stage(stage: Stage, operation: Awaitable[T], timeout: float,
                     stats: DownloadStats | None = None) -> T:
    from pymobiledevice3.exceptions import AlreadyMountedError, DeveloperModeIsNotEnabledError

    started = time.monotonic()
    try:
        result = await asyncio.wait_for(operation, timeout=timeout)
    except (AlreadyMountedError, DeveloperModeIsNotEnabledError, DeveloperSetupError):
        raise
    except Exception as exc:
        # Requests wraps a read timeout while streaming the body in ConnectionError.
        timed_out = isinstance(exc, (TimeoutError, requests.Timeout)) or (
            isinstance(exc, requests.ConnectionError) and bool(exc.args) and isinstance(exc.args[0], ReadTimeoutError)
        )
        record_setup_event(stage, "timeout" if timed_out else "failed", time.monotonic() - started, exc, stats)
        raise DeveloperSetupError(stage, timed_out=timed_out) from exc
    if stats is not None and stats.fetched:
        record_setup_event(stage, "ok", time.monotonic() - started, stats=stats)
    return result


def _download_progress(index: int, count: int, received: int, total: int) -> DownloadProgress:
    values: dict[str, object] = {"index": index, "count": count, "received": human_size(received)}
    if 0 < total and received <= total:
        values["total"] = human_size(total)
        return DownloadProgress("正在下载文件 {index}/{count} · {received} / {total}", values, received / total)
    return DownloadProgress("正在下载文件 {index}/{count} · 已收到 {received}", values, None)


def _content_range(value: str) -> tuple[int, int] | None:
    """Parse ``bytes start-end/total``. Returns (start, total) with total 0 when unknown."""
    unit, _, spec = value.partition(" ")
    span, _, total = spec.partition("/")
    start = span.partition("-")[0]
    if unit != "bytes" or not start.isdecimal():
        return None
    return int(start), int(total) if total.isdecimal() else 0


def _prepare_staging(staging: Path) -> None:
    """Keep parts only when they were downloaded for the same pinned commit."""
    marker = staging / ".ref"
    try:
        current = marker.read_text("ascii").strip()
    except (OSError, UnicodeDecodeError):
        current = ""
    if current != DDI_REF:
        shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True, exist_ok=True)
    marker.write_text(DDI_REF, "ascii")


def _fetch_file(session: requests.Session, url: str, target: Path, expected: tuple[int, str], deadline: float,
                cancel: threading.Event, stats: DownloadStats, report: Callable[[int, int], None]) -> None:
    """Download one pinned file into ``target`` through a ``.part`` file.

    Every source serves the same commit, so a part started on one source is resumed on another
    with a plain ``Range`` request. The pinned size and SHA-256 decide whether the joined bytes are
    the expected file; a mismatch deletes the part.
    """
    size, digest = expected
    part = target.with_name(target.name + ".part")
    offset = part.stat().st_size if part.is_file() else 0
    if offset > size:
        part.unlink()
        offset = 0
    hasher = hashlib.sha256()
    if offset:
        with part.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                hasher.update(block)
    received = offset
    stats.received, stats.total = received, size
    if offset < size:
        headers = {"Accept-Encoding": "identity"}
        if offset:
            headers["Range"] = f"bytes={offset}-"
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError()
        oversized = False
        with session.get(url, stream=True, headers=headers,
                         timeout=tuple(min(limit, remaining) for limit in DOWNLOAD_HTTP_TIMEOUT)) as response:
            stats.http_status = response.status_code
            if response.status_code == 416:
                part.unlink(missing_ok=True)  # This source disagrees with the part. The next one starts over.
                raise requests.HTTPError("Range not satisfiable", response=response)
            response.raise_for_status()
            if response.status_code == 206:
                resumed = _content_range(response.headers.get("Content-Range", ""))
                if not offset or resumed is None or resumed[0] != offset:
                    raise requests.ConnectionError("Unexpected partial response")
                stats.resumed += offset
            elif offset:
                # A source without range support sends the whole file again.
                stats.restarts += 1
                offset = received = 0
                hasher = hashlib.sha256()
            report(received, size)
            last_report, reported = time.monotonic(), received
            with part.open("ab" if offset else "wb") as stream:
                for chunk in response.iter_content(chunk_size=64 * 1024):
                    if cancel.is_set():
                        raise DownloadCanceled()
                    now = time.monotonic()
                    if now >= deadline:
                        raise TimeoutError()
                    if received + len(chunk) > size:
                        oversized = True
                        break
                    stream.write(chunk)
                    hasher.update(chunk)
                    received += len(chunk)
                    stats.received = received
                    stats.fetched += len(chunk)
                    if chunk and now - last_report >= 0.25:
                        report(received, size)
                        last_report, reported = now, received
        if oversized:
            part.unlink(missing_ok=True)
            raise IntegrityError("Developer image file is larger than pinned")
        if received != size:
            # The server closed the body early. Keep the part so another source or attempt resumes it.
            raise requests.ConnectionError("Incomplete developer image payload")
        if reported != received:
            report(received, size)
    if hasher.hexdigest() != digest:
        part.unlink(missing_ok=True)
        raise IntegrityError("Developer image file does not match its pinned hash")
    part.replace(target)


def _ordered_sources(preferred: str | None) -> list[tuple[str, str]]:
    return sorted(SOURCES, key=lambda source: source[0] != preferred)


def _download_parts(directory: str, names: dict[str, str], staging: Path, deadline: float,
                    cancel: threading.Event, stats: DownloadStats,
                    progress: Callable[[DownloadProgress], None] | None = None,
                    preferred: str | None = None) -> None:
    """Fill ``staging`` with every file in ``names``, trying each source in turn per file.

    Completed files from earlier attempts are kept. The source that last delivered data is tried
    first for the next file, so a blocked source costs one timeout per attempt, not one per file.
    """
    pinned = PINNED_FILES[directory]
    if not _download_lock.acquire(timeout=max(0.0, deadline - time.monotonic())):
        raise TimeoutError()
    try:
        if cancel.is_set():
            raise DownloadCanceled()
        _prepare_staging(staging)
        count = len(names)
        sources = _ordered_sources(preferred)
        with requests.Session() as session:
            for index, (local, remote) in enumerate(names.items(), 1):
                target = staging / local
                stats.file, stats.index, stats.count = local, index, count
                if target.is_file():
                    continue  # Moved here only after its hash matched.

                def report(received: int, total: int, index=index) -> None:
                    if progress is not None:
                        progress(_download_progress(index, count, received, total))

                last_error: Exception | None = None
                for attempt, (name, template) in enumerate(sources):
                    if attempt and progress is not None:
                        fraction = stats.received / stats.total if stats.total else None
                        progress(DownloadProgress("正在换用备用下载源 · 文件 {index}/{count}",
                                                  {"index": index, "count": count}, fraction))
                    url = template.format(repo=DDI_REPO, ref=DDI_REF, path=f"{directory}/{remote}")
                    before = stats.fetched
                    try:
                        _fetch_file(session, url, target, pinned[remote], deadline, cancel, stats, report)
                    except (DownloadCanceled, TimeoutError):
                        raise
                    except (requests.RequestException, IntegrityError) as exc:
                        # A blocked, throttled or tampering source must not end the download.
                        stats.failures.append(f"{name}:{type(exc).__name__}")
                        last_error = exc
                        continue
                    finally:
                        if stats.fetched > before:
                            stats.source = name
                    sources = _ordered_sources(name)
                    break
                else:
                    assert last_error is not None
                    raise last_error
    finally:
        _download_lock.release()


def _staging_folder(folder: Path) -> Path:
    return folder.with_name(folder.name + ".download")


def _source_file(folder: Path) -> Path:
    return folder.parent / "developer-image-source"


async def load_image(cryptex: bool, progress: Callable[[Message], None] | None = None,
                     stats: DownloadStats | None = None) -> Path:
    """Reuse the cache, or download the pinned files into a sibling staging folder and move them in.

    A canceled or failed download leaves its parts in the staging folder for the next attempt and
    never touches the cache the library reads.
    """
    from developer_disk_image.repo import CRYPTEX_IMAGE_PAYLOADS, CRYPTEX_IMAGE_ROOT

    from pymobiledevice3.common import get_home_folder

    source = CRYPTEX_IMAGE_ROOT if cryptex else "PersonalizedImages/Xcode_iOS_DDI_Personalized"
    folder = get_home_folder() / source.rsplit("/", 1)[-1]
    names = {MANIFEST: MANIFEST}
    if cryptex:
        names.update({name: name for name in CRYPTEX_IMAGE_PAYLOADS.values()})
    else:
        names.update({"Image.dmg": "Image.dmg", "Image.trustcache": "Image.dmg.trustcache"})
    pinned = PINNED_FILES[source]
    manifest = folder / MANIFEST
    # Sizes catch a truncated cache cheaply. Hashes were checked when the files were downloaded.
    if all((folder / local).is_file() and (folder / local).stat().st_size == pinned[remote][0]
           for local, remote in names.items()):
        try:
            cached = plistlib.loads(manifest.read_bytes())
            if isinstance(cached, dict) and cached.get("ProductBuildVersion") == DDI_BUILD:
                return folder
        except (plistlib.InvalidFileException, ValueError, ExpatError):
            pass  # Refresh a corrupt cache from an interrupted setup.

    loop = asyncio.get_running_loop()
    active = True
    cancel = threading.Event()
    staging = _staging_folder(folder)
    stats = stats if stats is not None else DownloadStats()
    try:
        preferred = _source_file(folder).read_text("ascii").strip()
    except (OSError, UnicodeDecodeError):
        preferred = None

    def report(event: DownloadProgress) -> None:
        # The download thread may still be unwinding after this attempt times out.
        if active and progress is not None:
            progress(event)

    if progress is not None:
        progress(message(PROGRESS["download"]))
    try:
        await asyncio.to_thread(
            _download_parts, source, names, staging, time.monotonic() + DOWNLOAD_TIMEOUT, cancel, stats,
            None if progress is None else lambda event: loop.call_soon_threadsafe(report, event), preferred)
    finally:
        active = False
        cancel.set()
        if stats.source and stats.source != preferred:
            try:
                _source_file(folder).write_text(stats.source, "ascii")
            except OSError:
                pass  # Only the source order of the next attempt depends on this file.
    folder.mkdir(parents=True, exist_ok=True)
    # The manifest marks a complete cache, so an interrupted move cannot leave a valid marker.
    manifest.unlink(missing_ok=True)
    for name in names:
        if name != MANIFEST:
            (staging / name).replace(folder / name)
    (staging / MANIFEST).replace(manifest)
    shutil.rmtree(staging, ignore_errors=True)
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
    stats = DownloadStats()
    # The thread's own deadline normally fires first. The extra margin covers waiting for an
    # earlier attempt's thread to leave its socket read.
    folder = await _run_stage("download", load_image(cryptex, progress, stats),
                              DOWNLOAD_TIMEOUT + STALL_TIMEOUT, stats)
    progress(message("正在准备安装开发者镜像…"))
    await _run_stage("install", _install_image(rsd, folder, cryptex, progress), INSTALL_TIMEOUT)
