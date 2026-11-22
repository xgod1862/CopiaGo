"""Keep wireless pairing records encrypted on disk with Windows DPAPI.

A remote pairing record holds the private key that lets this computer open a developer
tunnel to the iPhone over Wi-Fi. pymobiledevice3 stores it as a plain plist; here every
record is sealed right after it is written and opened in memory when the library reads it.
DPAPI ties the file to this Windows account, so a copied data folder (backup, cloud sync,
another user) is useless. It cannot stop malware running as the same user.
"""
from __future__ import annotations

import os
import plistlib
from pathlib import Path
from typing import Any, Optional

import win32crypt

MAGIC = b"ICB-DPAPI1\0"
_DESCRIPTION = "iPhone Clipboard pairing record"
_installed = False


def protect(data: bytes) -> bytes:
    return MAGIC + win32crypt.CryptProtectData(data, _DESCRIPTION, None, None, None, 1)


def unprotect(data: bytes) -> bytes:
    if not data.startswith(MAGIC):
        return data  # written before sealing, or by another tool
    return win32crypt.CryptUnprotectData(data[len(MAGIC):], None, None, None, 1)[1]


def read_record(path: Path) -> dict[str, Any]:
    return plistlib.loads(unprotect(path.read_bytes()))


def seal(path: Path) -> None:
    """Encrypt a plaintext record in place; already sealed files are left alone."""
    data = path.read_bytes()
    if data.startswith(MAGIC):
        return
    plistlib.loads(data)  # only seal something that really is a pairing record
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_bytes(protect(data))
    os.replace(temporary, path)


def seal_all(folder: Path) -> None:
    for path in folder.glob("remote_*.plist"):
        try:
            seal(path)
        except (OSError, ValueError, plistlib.InvalidFileException):
            continue


def install(folder: Path) -> None:
    """Route the library's pairing-record reads and writes through DPAPI."""
    global _installed
    seal_all(folder)
    if _installed:
        return
    from pymobiledevice3 import lockdown, pair_records
    from pymobiledevice3.remote import remote_service_discovery, tunnel_service, userspace_tunnel
    from pymobiledevice3.safe_paths import device_file_path

    def get_local_pairing_record(identifier: str, cache_folder: Path) -> Optional[dict[str, Any]]:
        path = device_file_path(cache_folder, identifier, f".{pair_records.PAIRING_RECORD_EXT}")
        return read_record(path) if path.exists() else None

    def iter_remote_pair_records_by_identifier():
        for path in pair_records.iter_remote_pair_records():
            identifier = path.parts[-1].split("remote_", 1)[1].split(".", 1)[0]
            try:
                yield identifier, path, read_record(path)
            except (OSError, ValueError, plistlib.InvalidFileException):
                continue

    # Modules that imported these functions by name hold their own reference.
    for module in (pair_records, lockdown, remote_service_discovery, userspace_tunnel):
        if hasattr(module, "get_local_pairing_record"):
            module.get_local_pairing_record = get_local_pairing_record
    for module in (pair_records, tunnel_service):
        if hasattr(module, "iter_remote_pair_records_by_identifier"):
            module.iter_remote_pair_records_by_identifier = iter_remote_pair_records_by_identifier

    protocol = tunnel_service.RemotePairingProtocol
    original_save = protocol.save_pair_record

    def save_pair_record(self) -> None:
        original_save(self)
        seal(self.pair_record_path)

    def pair_record(self) -> Optional[dict[str, Any]]:
        path = self.pair_record_path
        return read_record(path) if path.exists() else None

    protocol.save_pair_record = save_pair_record
    protocol.pair_record = property(pair_record)
    _installed = True
