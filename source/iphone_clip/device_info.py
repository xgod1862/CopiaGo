"""The paired phone's name, model and system version, for telling devices apart in the UI.

Only display strings are kept here. They are sealed with DPAPI next to the pairing record,
because a phone's name often contains its owner's name.
"""
from __future__ import annotations

from dataclasses import asdict, dataclass
import json
from pathlib import Path
import re

INFO_FILE = "device-info.bin"
FALLBACK_TITLE = "iPhone"
# Emoji and their joiners are common in phone names, but neither Microsoft YaHei nor the
# bundled Noto Sans SC has glyphs for them, so Pillow would draw empty boxes.
_UNDRAWABLE = re.compile("[\U0001F000-\U0010FFFF‍⃣︎️]")


@dataclass(frozen=True)
class DeviceInfo:
    """What the user calls the phone and what it is. Any field may be empty when unknown."""

    name: str = ""  # the name set on the phone, e.g. "小明的 iPhone"
    model: str = ""  # marketing name such as "iPhone 15 Pro", or the raw ProductType
    os_version: str = ""  # e.g. "18.1"

    def title(self) -> str:
        """Best single label for the phone: its name, else its model, else "iPhone"."""
        return drawable(self.name) or self.model or FALLBACK_TITLE

    def summary(self) -> str:
        """Model and system, e.g. "iPhone 15 Pro · iOS 18.1"; the model is left out when it is the title."""
        parts = []
        if self.model and self.model != self.title():
            parts.append(self.model)
        if self.os_version:
            parts.append(f"iOS {self.os_version}")
        return " · ".join(parts)

    def merged(self, newer: "DeviceInfo") -> "DeviceInfo":
        """Fill in what ``newer`` knows, keeping older values where it reports nothing."""
        return DeviceInfo(newer.name or self.name, newer.model or self.model, newer.os_version or self.os_version)


def drawable(text: str) -> str:
    """Remove characters the UI fonts cannot draw, collapse spaces, and trim."""
    return " ".join(_UNDRAWABLE.sub("", text or "").split())


def model_name(product_type: str | None) -> str:
    """Turn a ProductType such as "iPhone16,1" into "iPhone 15 Pro" using pymobiledevice3's table."""
    if not product_type:
        return ""
    try:
        from pymobiledevice3.irecv_devices import IRECV_DEVICES
    except ImportError:
        return product_type
    for device in IRECV_DEVICES:
        if device.product_type == product_type:
            return device.display_name
    return product_type  # a phone newer than the library's table


def info_path() -> Path:
    from iphone_clip.settings import data_dir

    return data_dir() / "device-data" / INFO_FILE


def load_device_info() -> DeviceInfo:
    """The last known details of the paired phone, or an empty record if none were saved or readable."""
    try:
        from iphone_clip.pair_store import unprotect

        values = json.loads(unprotect(info_path().read_bytes()).decode("utf-8"))
    except Exception:
        # A missing, damaged or foreign file only costs the label, never the connection.
        return DeviceInfo()
    if not isinstance(values, dict):
        return DeviceInfo()
    return DeviceInfo(*(str(values.get(key) or "") for key in ("name", "model", "os_version")))


def save_device_info(info: DeviceInfo) -> None:
    from iphone_clip.pair_store import protect

    path = info_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_bytes(protect(json.dumps(asdict(info), ensure_ascii=False).encode("utf-8")))
    temporary.replace(path)


def forget_device_info() -> None:
    info_path().unlink(missing_ok=True)
