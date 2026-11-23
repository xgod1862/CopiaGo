"""Look up the newest CopiaGo release on GitHub, so the panel can point the user at it.

Only a notice: nothing is downloaded or installed. The user opens the release page and runs the
installer themselves, which keeps signature checks, UAC and closing the running app out of here.
"""
from __future__ import annotations

from dataclasses import dataclass
import re

import requests

from iphone_clip import APP_NAME

REPOSITORY = "xgod1862/CopiaGo"
# /releases/latest never returns drafts or pre-releases, so a test build is not offered to everyone.
LATEST_URL = f"https://api.github.com/repos/{REPOSITORY}/releases/latest"
RELEASE_PAGE_PREFIX = f"https://github.com/{REPOSITORY}/releases/"
TIMEOUT = 10  # seconds; the check runs off the UI thread and gives up quietly
_VERSION = re.compile(r"v?(\d+)\.(\d+)\.(\d+)")


@dataclass(frozen=True)
class Release:
    version: str  # "1.2.0", without the tag's leading "v"
    url: str  # the release page on github.com


def parse_version(text: object) -> tuple[int, int, int] | None:
    """``"v1.2.0"`` or ``"1.2.0"`` as a comparable tuple; None for anything else, e.g. ``"v1.2.0-beta"``."""
    match = _VERSION.fullmatch(text.strip()) if isinstance(text, str) else None
    return tuple(int(part) for part in match.groups()) if match else None


def newer_release(current: str, payload: object) -> Release | None:
    """The release described by a GitHub ``/releases/latest`` response, if it is newer than ``current``.

    Tags that are not plain ``X.Y.Z`` are ignored rather than guessed at. The page link must point
    at this repository's releases, so a malformed response cannot make the panel open another site.
    """
    if not isinstance(payload, dict) or payload.get("draft") or payload.get("prerelease"):
        return None
    latest, installed = parse_version(payload.get("tag_name")), parse_version(current)
    url = payload.get("html_url")
    if latest is None or installed is None or latest <= installed:
        return None
    if not isinstance(url, str) or not url.startswith(RELEASE_PAGE_PREFIX):
        return None
    return Release(".".join(map(str, latest)), url)


def latest_release(current: str) -> Release | None:
    """Ask GitHub for the newest release; None when there is none, it is not newer, or the check fails.

    Blocks for up to ``TIMEOUT`` seconds, so call it from a worker thread. Failures (offline, rate
    limited, proxy) are not reported: the next start simply tries again.
    """
    headers = {"Accept": "application/vnd.github+json", "User-Agent": f"{APP_NAME}/{current}"}
    try:
        response = requests.get(LATEST_URL, headers=headers, timeout=TIMEOUT)
        if response.status_code != 200:
            return None
        return newer_release(current, response.json())
    except (requests.RequestException, ValueError):
        return None
