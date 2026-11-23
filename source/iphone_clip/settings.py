from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import sys

from iphone_clip.i18n import LANGUAGE_OPTIONS

LEGACY_DIR_NAME = "iPhoneClipboardPrototype"
# Disposable copies of received files: up to 2 GB, recreated on demand, so never migrated.
NOT_MIGRATED = frozenset({"received"})

_active: Path | None = None  # chosen once at startup by prepare_data_dir()


def legacy_data_dir() -> Path:
    """Where versions up to 1.1.0 kept installed-mode data, and where source runs still do."""
    return Path(os.environ["LOCALAPPDATA"]) / LEGACY_DIR_NAME


def account_folder_name() -> str:
    """The current Windows account name as a folder name.

    Matches Inno Setup's {username} (both come from GetUserName) so the uninstaller finds the
    same folder. Renaming the account therefore starts a fresh, empty data folder.
    """
    try:
        import win32api

        name = win32api.GetUserName()
    except Exception:
        name = os.environ.get("USERNAME", "")
    name = re.sub(r'[<>:"/\\|?*\x00-\x1f]', "_", name).rstrip(" .")
    return name or "user"


def default_data_dir() -> Path:
    """The data folder this build should use, before any fallback.

    Installed builds keep one folder per Windows account under the install folder, so data
    follows the drive chosen at install time. A portable build keeps its flat ``data`` folder.
    Running from source keeps using LOCALAPPDATA.
    """
    if getattr(sys, "frozen", False):
        app = Path(sys.executable).parent
        if (app / "portable-data").is_file():
            return app / "data"
        return app / "data" / account_folder_name()
    return legacy_data_dir()


def data_dir() -> Path:
    override = os.environ.get("ICB_DATA_DIR")
    if override:
        return Path(override)
    return _active if _active is not None else default_data_dir()


def restrict_to_current_user(folder: Path) -> None:
    """Give only this Windows account (and SYSTEM) access to ``folder`` and everything below it.

    A folder on a drive other than C: usually inherits the drive root's permissions, which let
    every signed-in account read it. received/ holds plaintext copies of phone files, so the
    inherited permissions are replaced by a protected DACL that existing children also inherit.
    """
    import ntsecuritycon as con
    import win32api
    import win32security

    token = win32security.OpenProcessToken(win32api.GetCurrentProcess(), win32security.TOKEN_QUERY)
    user = win32security.GetTokenInformation(token, win32security.TokenUser)[0]
    system = win32security.CreateWellKnownSid(win32security.WinLocalSystemSid)
    inherit = con.OBJECT_INHERIT_ACE | con.CONTAINER_INHERIT_ACE
    dacl = win32security.ACL()
    for sid in (user, system):
        dacl.AddAccessAllowedAceEx(win32security.ACL_REVISION, inherit, con.FILE_ALL_ACCESS, sid)
    win32security.SetNamedSecurityInfo(
        str(folder), win32security.SE_FILE_OBJECT,
        win32security.DACL_SECURITY_INFORMATION | win32security.PROTECTED_DACL_SECURITY_INFORMATION,
        None, None, dacl, None)


def _same_folder(first: Path, second: Path) -> bool:
    return os.path.normcase(os.path.abspath(first)) == os.path.normcase(os.path.abspath(second))


def _has_entries(folder: Path) -> bool:
    try:
        return any(folder.iterdir())
    except OSError:
        return False


def _migrate(legacy: Path, target: Path) -> None:
    """Copy legacy data into an empty ``target``, then remove the legacy folder.

    The copy goes to a sibling staging folder that is renamed into place only when complete, so
    an interrupted copy (full disk, crash) leaves the legacy folder intact for the next start.
    Files copied into the staging folder inherit its restricted permissions.
    """
    staging = target.with_name(target.name + ".migrating")
    shutil.rmtree(staging, ignore_errors=True)  # a previous attempt that did not finish
    staging.mkdir(parents=True)
    try:
        restrict_to_current_user(staging)
        for entry in legacy.iterdir():
            if entry.name in NOT_MIGRATED or entry.suffix == ".tmp":
                continue
            if entry.is_dir():
                shutil.copytree(entry, staging / entry.name)
            else:
                shutil.copy2(entry, staging / entry.name)
        if target.exists():
            target.rmdir()  # empty, checked by the caller
        staging.rename(target)
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    # The data is safe in its new place. A leftover legacy file only costs space.
    shutil.rmtree(legacy, ignore_errors=True)


def prepare_data_dir() -> Path:
    """Choose the data folder for this run, moving data from the legacy location if needed.

    Call once at startup, after the single-instance mutex is held and before anything reads
    settings. Data already in the new folder is never overwritten. When the new folder cannot
    be created or the move fails, this run uses the legacy folder so nothing appears lost, and
    the move is retried at the next start.
    """
    global _active
    if os.environ.get("ICB_DATA_DIR"):
        return data_dir()
    target = default_data_dir()
    legacy = legacy_data_dir()
    # Source runs already use the legacy folder; portable builds never used it.
    if _same_folder(target, legacy) or _is_portable(target):
        _active = target
        return target
    try:
        if legacy.is_dir() and not _has_entries(target):
            _migrate(legacy, target)
        elif not target.is_dir():
            target.mkdir(parents=True)
            restrict_to_current_user(target)
    except Exception:
        # An empty folder left without restricted permissions would be reused as-is next time.
        try:
            if target.is_dir() and not _has_entries(target):
                target.rmdir()
        except OSError:
            pass
        _active = legacy
        return legacy
    _active = target
    return target


def _is_portable(target: Path) -> bool:
    return target.name == "data" and (target.parent / "portable-data").is_file()


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


def update_check_enabled() -> bool:
    """Whether to ask GitHub for a newer release at startup. On unless the user turned it off."""
    return _load().get("update_check") is not False


def set_update_check_enabled(value: bool) -> None:
    _save("update_check", value)


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
