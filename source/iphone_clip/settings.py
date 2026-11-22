import json
import os
from pathlib import Path
import sys

from iphone_clip.i18n import LANGUAGE_OPTIONS


def data_dir() -> Path:
    override = os.environ.get("ICB_DATA_DIR")
    if override:
        return Path(override)
    if getattr(sys, "frozen", False):
        app = Path(sys.executable).parent
        if (app / "portable-data").is_file():
            return app / "data"
    return Path(os.environ["LOCALAPPDATA"]) / "iPhoneClipboardPrototype"


def _load() -> dict:
    try:
        values = json.loads((data_dir() / "settings.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return values if isinstance(values, dict) else {}


def _save(key: str, value) -> None:
    path = data_dir() / "settings.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    values = _load()
    values[key] = value
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(values), encoding="utf-8")
    temporary.replace(path)


def language_preference() -> str:
    value = _load().get("language")
    return value if value in LANGUAGE_OPTIONS else "system"


def set_language_preference(value: str) -> None:
    if value not in LANGUAGE_OPTIONS:
        raise ValueError(f"Unsupported language preference: {value}")
    _save("language", value)


def history_enabled() -> bool:
    return _load().get("history") is True


def set_history_enabled(value: bool) -> None:
    _save("history", value)


def notifications_enabled() -> bool:
    # Notifications name only the kind of content, so they default to on.
    return _load().get("notifications") is not False


def set_notifications_enabled(value: bool) -> None:
    _save("notifications", value)


def push_enabled() -> bool:
    # Writing to the phone is opt-in: a compromised PC could otherwise tamper with its clipboard.
    return _load().get("push") is True


def set_push_enabled(value: bool) -> None:
    _save("push", value)


def pc_history_enabled() -> bool:
    # Recording what is copied on this computer is opt-in, like the iPhone history.
    return _load().get("pc_history") is True


def set_pc_history_enabled(value: bool) -> None:
    _save("pc_history", value)


def pc_history_enabled() -> bool:
    # Recording what is copied on this computer is opt-in, like the iPhone history.
    return _load().get("pc_history") is True


def set_pc_history_enabled(value: bool) -> None:
    _save("pc_history", value)


def pull_enabled() -> bool:
    # iPhone copies go straight to the Windows clipboard unless the user turns that off.
    return _load().get("pull") is not False


def set_pull_enabled(value: bool) -> None:
    _save("pull", value)


# Files go from the iPhone to the computer only, for now.
CONTENT_TYPES = {"pull": ("text", "image", "file"), "push": ("text", "image")}


def sync_types(direction: str) -> tuple[str, ...]:
    """Kinds of content synced automatically for "pull" (iPhone to PC) or "push" (PC to iPhone)."""
    known = CONTENT_TYPES[direction]
    value = _load().get(f"{direction}_types")
    if isinstance(value, list):
        chosen = tuple(kind for kind in known if kind in value)
        if chosen:
            return chosen
    return known


def set_sync_types(direction: str, kinds) -> None:
    _save(f"{direction}_types", [kind for kind in CONTENT_TYPES[direction] if kind in kinds])


ASK_CHOICES = (10, 20, 50, 0)  # MB; 0 = never ask


def file_ask_mb() -> int:
    """Files from the iPhone larger than this wait for the user's OK; 0 syncs every size."""
    value = _load().get("file_ask_mb")
    return value if value in ASK_CHOICES else 20


def set_file_ask_mb(value: int) -> None:
    _save("file_ask_mb", value)
