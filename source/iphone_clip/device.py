"""Trusted USB pairing and one-way clipboard monitoring over an iOS developer tunnel."""

from __future__ import annotations

import asyncio
import contextlib
from dataclasses import dataclass
import logging
import os
import plistlib
import re
import threading
import time
from pathlib import Path
from typing import Callable

from iphone_clip.clipboard import Clip, fingerprint
from iphone_clip.developer_setup import DeveloperSetupError, prepare_developer_services
from iphone_clip.device_info import DeviceInfo, forget_device_info, load_device_info, model_name, save_device_info
from iphone_clip.files import FILE_DESCRIPTOR, MAX_FILE_BYTES, content_type, name_from_descriptor, safe_name
from iphone_clip.settings import data_dir

from iphone_clip.i18n import Message, message

# Every state carries a Message, except "device", which carries the connected phone's DeviceInfo.
StatusCallback = Callable[[str, Message], None]
IDLE_PROMPT = message("连接并信任你的 iPhone")
CONNECTED_WIFI = message("已连接（无线）")
CONNECTED_USB = message("已连接（USB） · 可以拔掉数据线，之后将通过无线连接")
ClipCallback = Callable[[Clip, bool], None]  # (clip, copied while disconnected)
DESCRIPTION_BYTES = 256 * 1024  # a file this small comes along with its description


@dataclass(frozen=True)
class FileOffer:
    """A file copied in the Files app, described but (unless small) not yet transferred."""
    name: str  # safe for Windows, original where the phone told us
    uti: str
    size: int  # bytes as the phone reported them; 0 when unknown
    change: tuple | None  # pasteboard state it belongs to
    data: bytes | None = None


class OfferExpired(Exception):
    """Something else was copied on the iPhone since the file was offered."""


class FileTooLarge(Exception):
    pass


def throttled(report: Callable[[int, int], None], interval: float = 0.2) -> Callable[[int, int], None]:
    """Pass progress on at most every ``interval`` seconds; the final, complete count always passes."""
    last = -interval

    def forward(received: int, total: int) -> None:
        nonlocal last
        now = time.monotonic()
        if received >= total or now - last >= interval:
            last = now
            report(received, total)

    return forward


def _entry(snapshot: dict, uti: str) -> dict:
    for item in (snapshot.get("pasteboard") or {}).get("items", ()):
        entry = (item.get("data") or {}).get(uti)
        if isinstance(entry, dict):
            return entry
    return {}


def _inline(snapshot: dict, uti: str) -> bytes | None:
    data = _entry(snapshot, uti).get("data")
    return bytes(data) if isinstance(data, (bytes, bytearray)) else None


def diagnostic_event(stage: str, error: BaseException | None = None) -> None:
    """Optional local diagnostic containing error types only, never content or IDs."""
    destination = os.environ.get("ICB_DIAGNOSTIC_FILE")
    if not destination:
        return
    details = stage
    if error is not None:
        details += " " + type(error).__name__
        if isinstance(error, ImportError) and error.name:
            details += " " + error.name
        if type(error).__name__ == "PyInstallerImportError":
            details += " " + str(error)
    try:
        with open(destination, "a", encoding="utf-8") as stream:
            stream.write(details + "\n")
    except OSError:
        pass


def configure_pairing_storage() -> Path:
    """Keep pairing credentials and developer assets under the app's local-data folder."""
    import pymobiledevice3.common as common

    folder = data_dir() / "device-data"
    common._HOMEFOLDER = folder
    from iphone_clip.pair_store import install
    from iphone_clip.windows_discovery import enable_windows_discovery
    install(folder)
    enable_windows_discovery()
    return folder


async def usb_identifiers() -> list[str]:
    from pymobiledevice3 import usbmux

    devices = await usbmux.list_devices()
    return [str(d.serial) for d in devices if str(d.connection_type).upper() == "USB"]


class USBPairingError(RuntimeError):
    """Pairing could not start because the USB device selection was invalid."""


async def pair_over_usb(progress: Callable[[Message], None] | None = None) -> str:
    """Set up one explicitly trusted device for wireless RemotePairing."""
    from pymobiledevice3.exceptions import RemotePairingCompletedError
    from pymobiledevice3.lockdown import create_using_usbmux
    from pymobiledevice3.remote.tunnel_service import RemotePairingLockdownService

    identifiers = await usb_identifiers()
    if not identifiers:
        raise USBPairingError(message("未找到 USB 连接的 iPhone。请用数据线连接并解锁后重试"))
    if len(identifiers) != 1:
        raise USBPairingError(message("请暂时只连接一台 iPhone"))
    if progress is not None:
        progress(message("正在等待 iPhone 信任此电脑。请解锁手机，按提示点「信任」并输入锁屏密码。"))
    lockdown = await create_using_usbmux(serial=identifiers[0], autopair=True)
    try:
        if progress is not None:
            progress(message("已确认 iPhone 信任，正在准备无线配对…"))
        # Values read before trust can be the reduced unpaired set, so ask again now.
        name = lockdown.all_values.get("DeviceName")
        with contextlib.suppress(Exception):
            name = await lockdown.get_value(key="DeviceName")
        remember_device(DeviceInfo(str(name or ""), model_name(lockdown.product_type),
                                   str(lockdown.product_version or "")))
        if not await lockdown.get_enable_wifi_connections():
            await lockdown.set_enable_wifi_connections(True)
        service = await RemotePairingLockdownService.create(lockdown)
        try:
            try:
                await service.connect(autopair=True)
            except RemotePairingCompletedError:
                # The device closes this channel as soon as pairing is complete.
                pass
        finally:
            await service.close()
        return str(lockdown.udid)
    finally:
        await lockdown.close()


def paired_identifiers() -> list[str]:
    from pymobiledevice3.pair_records import iter_remote_paired_identifiers

    return sorted(set(iter_remote_paired_identifiers()))


def forget_paired_devices() -> None:
    """Remove wireless pairing records; the next connection needs USB pairing again."""
    from pymobiledevice3.pair_records import iter_remote_pair_records

    for record in list(iter_remote_pair_records()):
        record.unlink(missing_ok=True)
    forget_device_info()


def remember_device(info: DeviceInfo) -> DeviceInfo:
    """Merge fresh details into the saved ones and return the result.

    Saving is best effort: a full disk or a DPAPI failure must not stop pairing or a connection.
    """
    known = load_device_info().merged(info)
    try:
        save_device_info(known)
    except Exception as error:
        diagnostic_event("device_info", error)
    return known


async def read_device_info(rsd) -> DeviceInfo:
    """Model and system from the tunnel handshake; the phone's name from lockdown when available.

    The name query goes through the tunnel's lockdown service, which not every iOS version
    offers. Without it the name saved during USB pairing is used instead.
    """
    name = ""
    with contextlib.suppress(Exception):
        name = str(await asyncio.wait_for(rsd.get_value(key="DeviceName"), 3) or "")
    return remember_device(DeviceInfo(name, model_name(getattr(rsd, "product_type", "")),
                                      str(getattr(rsd, "product_version", "") or "")))


def change_id(snapshot: dict) -> tuple[object, object] | None:
    board = snapshot.get("pasteboard")
    if not isinstance(board, dict):
        return None
    meta = board.get("metadata")
    if not isinstance(meta, dict) or meta.get("changeCount") is None:
        return None
    return meta.get("nonce"), meta.get("changeCount")


TEXT_UTIS = frozenset({"public.utf8-plain-text", "public.plain-text", "public.text", "public.utf16-plain-text",
                       "public.utf16-external-plain-text", "public.url"})


class PullTimeout(Exception):
    """The phone did not hand over the content in time; ``listing`` still says what changed."""

    def __init__(self, listing: dict) -> None:
        super().__init__("pasteboard content timed out")
        self.listing = listing


async def read_clip(rsd) -> tuple[dict, Clip | None]:
    from pymobiledevice3.remote.core_device.pasteboard_service import (
        GENERAL_PASTEBOARD,
        IMAGE_UTIS,
        POLICY_ALL_PROMISED,
        POLICY_ALL_RESOLVED,
        POLICY_PROMISE_SECONDARY,
        PasteboardContent,
        PasteboardService,
    )

    async with PasteboardService(rsd) as service:
        # Formats first: asking for data makes the phone prepare it, which can take minutes for a
        # video from Photos. Only text, pictures and the description of a file are fetched here.
        listing = await asyncio.wait_for(service.get(GENERAL_PASTEBOARD, POLICY_ALL_PROMISED), 8)
        types = snapshot_types(listing)
        if FILE_DESCRIPTOR in types and content_type(types):
            return listing, await describe_file(service, listing)
        if not set(types) & (TEXT_UTIS | set(IMAGE_UTIS)):
            return listing, None
        try:
            snapshot = await asyncio.wait_for(service.get(GENERAL_PASTEBOARD, POLICY_PROMISE_SECONDARY), 15)
        except TimeoutError:
            raise PullTimeout(listing) from None
        content = PasteboardContent.from_snapshot(snapshot)
        if content.image:
            return snapshot, Clip(image=content.image)
        types = {
            name
            for item in (snapshot.get("pasteboard") or {}).get("items", ())
            for name in item.get("types", ())
        }
        if types.intersection(IMAGE_UTIS):
            # An app can advertise a real raster image as a secondary
            # representation. Keep the current Windows clipboard on failure.
            try:
                snapshot = await asyncio.wait_for(service.get(GENERAL_PASTEBOARD, POLICY_ALL_RESOLVED), 30)
            except TimeoutError:
                return snapshot, None
            content = PasteboardContent.from_snapshot(snapshot)
            return snapshot, Clip(image=content.image) if content.image else None
        text = content.text
        return snapshot, Clip(text=text) if text and text.strip("\ufffc \t\r\n") else None


async def describe_file(service, listing: dict) -> FileOffer:
    """Name and size of a copied file in one quick request; the bytes only if the file is small."""
    from pymobiledevice3.remote.core_device.pasteboard_service import GENERAL_PASTEBOARD, policy_threshold

    uti = content_type(snapshot_types(listing))
    try:
        described = await asyncio.wait_for(service.get(GENERAL_PASTEBOARD, policy_threshold(DESCRIPTION_BYTES)), 30)
    except TimeoutError:
        raise PullTimeout(listing) from None
    data = _inline(described, uti)
    size = len(data) if data is not None else int(_entry(described, uti).get("size") or 0)
    name = safe_name(name_from_descriptor(_inline(described, FILE_DESCRIPTOR), uti), uti)
    return FileOffer(name, uti, size, change_id(listing), data)


DESCRIPTOR_FIELDS = ("filename", "displayName", "documentSize", "fileSize", "size", "contentType",
                     "typeIdentifier", "itemIdentifier", "creationDate", "contentModificationDate")


def _structure_hint(raw: bytes) -> str:
    """Shape of a small descriptor (field names, a file extension), never its values."""
    try:
        value = plistlib.loads(raw)
    except Exception:
        printable = sum(32 <= byte < 127 for byte in raw[:64])
        return f"opaque printable={printable}/{min(len(raw), 64)}"
    if isinstance(value, dict):
        keys = sorted(str(key) for key in value)[:20]
        strings = value.get("$objects", []) if "$objects" in value else list(value.values())
        extensions = sorted({match.group(1).lower() for item in strings if isinstance(item, str)
                             for match in [re.search(r"\.([A-Za-z0-9]{1,8})$", item)] if match})
        # Which well-known descriptor fields exist, and any byte-count-like numbers (sizes, dates).
        names = {item.lower() for item in strings if isinstance(item, str) and len(item) <= 40}
        fields = [field for field in DESCRIPTOR_FIELDS if field.lower() in names]
        numbers = sorted({item for item in strings if isinstance(item, int) and not isinstance(item, bool)
                          and item >= 1024})[:8]
        return (f"plist keys={'/'.join(keys)} ext={'/'.join(extensions) or '-'} "
                f"fields={'/'.join(fields) or '-'} numbers={'/'.join(map(str, numbers)) or '-'}")
    return f"plist {type(value).__name__}"


def describe_snapshot(snapshot: dict) -> str:
    """Per item and type: inline size or what the device promised. For diagnostics only."""
    items = (snapshot.get("pasteboard") or {}).get("items", ()) if isinstance(snapshot, dict) else ()
    parts = []
    for index, item in enumerate(items):
        data = item.get("data") or {}
        for uti in item.get("types", ()):
            entry = data.get(uti)
            if isinstance(entry, dict) and isinstance(entry.get("data"), (bytes, bytearray)):
                info = f"inline {len(entry['data'])}B"
                if len(entry["data"]) <= 64 * 1024 and not uti.startswith("public."):
                    info += " " + _structure_hint(bytes(entry["data"]))
            elif isinstance(entry, dict):
                info = "promised " + " ".join(f"{key}={entry[key]}" for key in sorted(entry)
                                              if not isinstance(entry[key], (bytes, bytearray)))
            else:
                info = "absent"
            parts.append(f"{index}:{uti}={info}")
    return "; ".join(parts)


def snapshot_types(snapshot: dict) -> list[str]:
    """The UTIs on the pasteboard: names of formats only, never content."""
    items = (snapshot.get("pasteboard") or {}).get("items", ()) if isinstance(snapshot, dict) else ()
    return sorted({name for item in items for name in item.get("types", ()) if isinstance(name, str)})


def unsupported_kind(types: list[str]) -> Message | None:
    """What a copy that yielded no text or picture was, in words for the status line."""
    if not types:
        return None
    if any("movie" in name or "video" in name or name in ("public.mpeg-4", "public.avi", "public.mpeg")
           for name in types):
        return message("视频")
    if any(name in ("public.file-url", "public.url") or name.startswith(("com.adobe.pdf", "public.zip"))
           for name in types):
        return message("文件")
    return message("文件或其他内容")


def failure_message(exc: BaseException, pairing: bool) -> Message:
    if isinstance(exc, USBPairingError):
        return exc.args[0]
    name = type(exc).__name__
    if name in ("ConnectionRefusedError", "ConnectionFailedError", "ConnectionFailedToUsbmuxdError"):
        return message("等待 Apple 设备连接服务或 iPhone…")
    if name == "DeveloperModeIsNotEnabledError":
        return message("需要开启开发者模式：在 iPhone「设置 → 隐私与安全性 → 开发者模式」中开启并重启手机，解锁后点「继续连接」")
    if name == "UserDeniedPairingError":
        return message("iPhone 上选择了「不信任」。请重新插拔数据线，在提示时点「信任」，再点「连接 iPhone」")
    if name == "PasswordRequiredError":
        return message("请先解锁 iPhone，再点「连接 iPhone」")
    if name == "PairingDialogResponsePendingError" or (pairing and isinstance(exc, TimeoutError)):
        return message("未完成配对：请在 iPhone 上点「信任」并输入锁屏密码，然后再点「连接 iPhone」")
    if pairing:
        return message("配对未完成：请保持 iPhone 解锁并连着数据线，然后再点「连接 iPhone」")
    return message("等待 iPhone 连接；请检查解锁、信任及同一局域网")


class Bridge:
    def __init__(self, status: StatusCallback, on_clip: ClipCallback,
                 on_offer: Callable[[FileOffer], None] = lambda offer: None) -> None:
        self.status = status
        self.on_clip = on_clip
        self.on_offer = on_offer
        self.stopping = threading.Event()
        self.last_change: tuple[object, object] | None = None
        self.pair_requested = threading.Event()
        self.reset_requested = threading.Event()
        # Set while the first connection is being set up: failures then wait for the user's
        # "continue" instead of retrying every few seconds, which made the guide jump around.
        self.hold_failures = threading.Event()
        self.loop: asyncio.AbstractEventLoop | None = None
        self.rsd = None  # the live tunnel while connected
        self.rsd_lock: asyncio.Lock | None = None
        self.pushed: tuple[str | None, float] = (None, 0.0)  # what this computer last put on the phone

    def push(self, clip: Clip):
        """Put a clip on the iPhone pasteboard from any thread; returns a concurrent future."""
        if self.loop is None or self.rsd is None:
            raise ConnectionError("iPhone 未连接")
        return asyncio.run_coroutine_threadsafe(self._push(clip), self.loop)

    async def _push(self, clip: Clip) -> None:
        from pymobiledevice3.remote.core_device.pasteboard_service import PasteboardService, data_item, text_item

        rsd, lock = self.rsd, self.rsd_lock
        if rsd is None or lock is None:
            raise ConnectionError("iPhone 未连接")
        items = [data_item("public.png", clip.image)] if clip.image else [text_item(clip.text or "")]
        self.pushed = (fingerprint(clip), time.monotonic())
        async with lock:
            async with PasteboardService(rsd) as service:
                reply = await asyncio.wait_for(service.set(items), 20)
        current = change_id(reply)
        if current is not None:
            self.last_change = current  # our own change: not something to copy back

    def fetch(self, offer: FileOffer, progress: Callable[[int, int], None] | None = None):
        """Transfer an offered file from any thread; returns a concurrent future of a Clip.

        ``progress(received, total)`` is called on the bridge thread, at most about five times a second.
        """
        if self.loop is None:
            raise ConnectionError("iPhone 未连接")
        return asyncio.run_coroutine_threadsafe(self._fetch(offer, progress), self.loop)

    async def _fetch(self, offer: FileOffer, progress: Callable[[int, int], None] | None = None) -> Clip:
        from pymobiledevice3.remote.core_device.pasteboard_service import (
            GENERAL_PASTEBOARD,
            POLICY_ALL_PROMISED,
            PasteboardService,
            policy_threshold,
        )

        if offer.data is not None:
            return Clip(file_name=offer.name, file_data=offer.data)
        if offer.size > MAX_FILE_BYTES:
            raise FileTooLarge(offer.size)
        rsd, lock = self.rsd, self.rsd_lock
        if rsd is None or lock is None:
            raise ConnectionError("iPhone 未连接")
        limit = (offer.size or MAX_FILE_BYTES) + 1
        timeout = min(900.0, 60.0 + limit / (1024 * 1024))  # allows about 1 MB/s
        async with lock:
            async with PasteboardService(rsd) as service:
                listing = await asyncio.wait_for(service.get(GENERAL_PASTEBOARD, POLICY_ALL_PROMISED), 8)
                if offer.change is not None and change_id(listing) != offer.change:
                    raise OfferExpired()
                if progress is not None:
                    service.service.on_progress = throttled(progress)
                snapshot = await asyncio.wait_for(service.get(GENERAL_PASTEBOARD, policy_threshold(limit)), timeout)
        data = _inline(snapshot, offer.uti)
        if data is None:
            raise FileTooLarge(offer.size)
        return Clip(file_name=offer.name, file_data=data)

    def is_echo(self, clip: Clip) -> bool:
        sent, at = self.pushed
        return sent is not None and time.monotonic() - at < 120 and fingerprint(clip) == sent

    def request_pair(self) -> None:
        self.pair_requested.set()

    def request_reset(self) -> None:
        """Drop the current connection after the pairing records were removed."""
        self.reset_requested.set()

    def interrupted(self) -> bool:
        return self.stopping.is_set() or self.reset_requested.is_set()

    def stop(self) -> None:
        self.stopping.set()

    async def run(self) -> None:
        configure_pairing_storage()
        self.loop = asyncio.get_running_loop()
        # Library diagnostics may contain device details; never log clipboard contents.
        logging.disable(logging.CRITICAL)
        prompted = False
        while not self.stopping.is_set():
            if self.reset_requested.is_set():
                self.reset_requested.clear()
                self.pair_requested.clear()
                self.last_change = None
                prompted = False
            records = []
            pairing = False
            try:
                records = paired_identifiers()
                diagnostic_event(f"paired_records={len(records)}")
                if not records:
                    if not self.pair_requested.is_set():
                        # After a failure, keep its message visible until the user tries again.
                        if not prompted:
                            self.status("setup", IDLE_PROMPT)
                            prompted = True
                        while not self.interrupted() and not self.pair_requested.is_set():
                            await asyncio.sleep(0.25)
                        if self.interrupted():
                            continue
                    self.pair_requested.clear()
                    pairing = True
                    self.status("pairing", message("正在查找通过 USB 连接的 iPhone…"))
                    await asyncio.wait_for(pair_over_usb(lambda event: self.status("pairing", event)), timeout=120)
                    pairing = False
                    configure_pairing_storage()  # seals the record just written, whichever path wrote it
                    records = paired_identifiers()
                    if not records:
                        raise RuntimeError("无线配对记录尚未生成")
                if len(records) != 1:
                    self.status("error", message("已配对多台 iPhone。请在设置中点「忘记此 iPhone」后重新配对"))
                    await asyncio.sleep(5)
                    continue
                serial = records[0]
                try:
                    await self._connect(serial, wireless=True)
                except asyncio.CancelledError:
                    raise
                except Exception as exc:
                    diagnostic_event("wireless", exc)
                    if self.interrupted():
                        raise
                    # GitHub downloads and Apple signing use the PC's network on either transport.
                    if isinstance(exc, DeveloperSetupError) and exc.stage != "install":
                        raise
                    # USB is a first-setup fallback. Once removed, the next pass retries Wi-Fi.
                    if serial not in await usb_identifiers():
                        raise
                    try:
                        await self._connect(serial, wireless=False)
                    except DeveloperSetupError:
                        raise
                    except Exception as usb_error:
                        # A generic USB failure should not obscure the known installation error.
                        if (isinstance(exc, DeveloperSetupError) and
                                type(usb_error).__name__ != "DeveloperModeIsNotEnabledError"):
                            raise exc
                        raise
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                diagnostic_event("run", exc)
                if self.stopping.is_set():
                    break
                if self.reset_requested.is_set():
                    continue
                developer = type(exc).__name__ == "DeveloperModeIsNotEnabledError"
                setup_error = isinstance(exc, DeveloperSetupError)
                if records and (developer or setup_error or self.hold_failures.is_set()):
                    # Keep the failure visible until the user retries, even with an existing pairing.
                    if developer:
                        self.status("developer", failure_message(exc, pairing))
                    elif setup_error:
                        diagnostic_event(f"developer setup {exc.stage}", exc.__cause__)
                        self.status("hold", exc.status)
                    else:
                        self.status("hold", message("连接未完成：请保持 iPhone 解锁并连着数据线，然后点「继续连接」"))
                    while not self.interrupted() and not self.pair_requested.is_set():
                        await asyncio.sleep(0.25)
                    self.pair_requested.clear()
                    continue
                self.status("setup" if not records else "waiting", failure_message(exc, pairing))
                await asyncio.sleep(4)

    async def _connect(self, serial: str, *, wireless: bool) -> None:
        from pymobiledevice3.remote.core_device.pasteboard_service import PasteboardService
        from pymobiledevice3.remote.userspace_tunnel import UserspaceRsdTunnel
        from pymobiledevice3.exceptions import AlreadyMountedError

        from iphone_clip import xpc_reassembly

        xpc_reassembly.install()
        self.status("connecting", message("正在建立无线连接…" if wireless else "正在建立USB连接…"))
        for attempt in range(2):
            async with UserspaceRsdTunnel(serial=serial, autopair=False, remotepairing=wireless) as rsd:
                diagnostic_event("tunnel_open")
                services = (rsd.peer_info or {}).get("Services", {})
                if PasteboardService.SERVICE_NAME not in services:
                    if attempt:
                        raise DeveloperSetupError("install")
                    try:
                        await prepare_developer_services(rsd, lambda event: self.status("connecting", event))
                    except AlreadyMountedError:
                        pass
                    self.status("connecting", message("正在重新连接并验证剪贴板服务…"))
                    continue  # Refresh the advertised service table after mounting.
                self.status("device", await read_device_info(rsd))
                await self._monitor(rsd, wireless=wireless)
                return

    async def _monitor(self, rsd: object, *, wireless: bool) -> None:
        # The tunnel and its lock are shared with pushes from this computer while connected.
        lock = asyncio.Lock()
        changed = asyncio.Event()
        self.rsd, self.rsd_lock = rsd, lock
        try:
            await self._watch(rsd, lock, changed, wireless=wireless)
        finally:
            self.rsd = None

    async def _watch(self, rsd: object, lock: asyncio.Lock, changed: asyncio.Event, *, wireless: bool) -> None:
        from pymobiledevice3.remote.core_device.pasteboard_service import (
            GENERAL_PASTEBOARD,
            PASTEBOARD_CHANGED_NOTIFICATION,
            POLICY_ALL_PROMISED,
            PasteboardService,
        )
        from pymobiledevice3.services.notification_proxy import NotificationProxyService

        async def metadata() -> tuple[object, object] | None:
            async with lock:
                async with PasteboardService(rsd) as service:
                    snapshot = await asyncio.wait_for(service.get(GENERAL_PASTEBOARD, POLICY_ALL_PROMISED), 8)
            return change_id(snapshot)

        async with NotificationProxyService(rsd) as notifications:
            await notifications.notify_register_dispatch(PASTEBOARD_CHANGED_NOTIFICATION)
            for notification in ("com.apple.springboard.lockstate", "com.apple.mobile.keybagd.lock_status"):
                with contextlib.suppress(Exception):
                    await notifications.notify_register_dispatch(notification)
            initial = await metadata()
            diagnostic_event("monitor_ready")
            stale = False
            if self.last_change is not None and initial != self.last_change:
                stale = True
                changed.set()
            elif self.last_change is None:
                self.last_change = initial

            self.status("connected", CONNECTED_WIFI if wireless else CONNECTED_USB)

            async def receive() -> None:
                async for event in notifications.receive_notification():
                    if event.get("Command") == "ProxyDeath":
                        raise ConnectionError("notification proxy closed")
                    if event.get("Name") in (
                        PASTEBOARD_CHANGED_NOTIFICATION,
                        "com.apple.springboard.lockstate",
                        "com.apple.mobile.keybagd.lock_status",
                    ):
                        changed.set()
                raise ConnectionError("notification stream closed")

            async def poll() -> None:
                failures = 0
                while not self.stopping.is_set():
                    await asyncio.sleep(2)
                    try:
                        current = await metadata()
                        failures = 0
                        if current is not None and current != self.last_change:
                            changed.set()
                    except asyncio.CancelledError:
                        raise
                    except Exception:
                        failures += 1
                        if failures >= 3:
                            raise ConnectionError("device tunnel stopped responding")

            async def pull() -> None:
                nonlocal stale
                while not self.stopping.is_set():
                    await changed.wait()
                    changed.clear()
                    try:
                        async with lock:
                            snapshot, clip = await read_clip(rsd)
                    except PullTimeout as timeout:
                        # Skip this copy but keep the connection; the next copy is read as usual.
                        diagnostic_event("pull timeout types=" + ",".join(snapshot_types(timeout.listing)))
                        snapshot, clip = timeout.listing, None
                        if change_id(snapshot) is not None:
                            self.last_change = change_id(snapshot)
                        continue
                    except TimeoutError as error:
                        # Even the list of formats is late: try again shortly. A dead tunnel is noticed
                        # by the poll, which gives up after repeated failures.
                        diagnostic_event("pull listing", error)
                        await asyncio.sleep(5)
                        changed.set()
                        continue
                    was_stale, stale = stale, False
                    current = change_id(snapshot)
                    if current is not None and current == self.last_change:
                        continue
                    if current is not None:
                        self.last_change = current
                    if isinstance(clip, FileOffer):
                        self.on_offer(clip)
                    elif clip and not self.is_echo(clip):
                        self.on_clip(clip, was_stale)
                    elif clip is None:
                        types = snapshot_types(snapshot)
                        diagnostic_event("unsupported types=" + ",".join(types))
                        diagnostic_event("unsupported detail " + describe_snapshot(snapshot))
                        if os.environ.get("ICB_DIAGNOSTIC_FILE"):
                            await probe_sizes()
                        kind = unsupported_kind(types)
                        if kind:
                            self.status("unsupported", message("iPhone 上复制了{kind}，目前只同步文字、图片和「文件」App 里的文件", kind=kind))

            async def probe_sizes() -> None:
                """Diagnostics only: ask the phone for sizes without the data, and time it."""
                from pymobiledevice3.remote.core_device.pasteboard_service import policy_threshold
                started = time.monotonic()
                try:
                    async with lock:
                        async with PasteboardService(rsd) as service:
                            probe = await asyncio.wait_for(service.get(GENERAL_PASTEBOARD, policy_threshold(1)), 90)
                    diagnostic_event(f"size probe {time.monotonic() - started:.1f}s " + describe_snapshot(probe))
                except Exception as error:
                    diagnostic_event(f"size probe failed {time.monotonic() - started:.1f}s", error)

            async def watch_stop() -> None:
                while not self.interrupted():
                    await asyncio.sleep(0.5)
                raise ConnectionAbortedError("stopping")

            tasks = [asyncio.create_task(coro()) for coro in (receive, poll, pull, watch_stop)]
            try:
                done, _ = await asyncio.wait(tasks, return_when=asyncio.FIRST_EXCEPTION)
                for task in done:
                    task.result()
            finally:
                for task in tasks:
                    task.cancel()
                await asyncio.gather(*tasks, return_exceptions=True)
