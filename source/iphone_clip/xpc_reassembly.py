"""Linear-time reassembly of RemoteXPC replies.

pymobiledevice3 11.61.1 rebuilds a reply that spans many HTTP/2 DATA frames by concatenating
``bytes`` and re-parsing on every frame. The device sends 16 KB frames, so a copied file of
N bytes costs about N² / 16 KB of copying: 100 MB took over two minutes for that alone.

This replacement keeps the bytes in a ``bytearray`` and parses once the wrapper header says the
whole message has arrived. Everything after the parse is the library's own logic, unchanged.

A connection may carry an ``on_progress(received, total)`` attribute; while a reply spans
several frames it is called with the bytes received so far and the reply's full size.
"""

from __future__ import annotations

from typing import Any

_installed = False


async def receive_response(self) -> dict[str, Any]:
    from pymobiledevice3.exceptions import ProtocolError
    from pymobiledevice3.remote.xpc_message import (
        XpcMessageType,
        XpcWrapper,
        XpcWrapperHeader,
        decode_xpc_object,
    )

    header_size = XpcWrapperHeader.SIZE
    while True:
        frame = await self._receive_next_data_frame()
        buffer = self._previous_frame_data
        if buffer:
            if not isinstance(buffer, bytearray):
                buffer = bytearray(buffer)
            buffer += frame.data
        else:
            buffer = frame.data
        if len(buffer) < header_size:
            self._previous_frame_data = bytearray(buffer)
            continue
        # Parsing only the header still rejects a stream that does not start with the magic.
        header = XpcWrapperHeader.parse(bytes(buffer[:header_size]))
        total = header_size + header.size
        progress = getattr(self, "on_progress", None)
        if progress is not None and (buffer is not frame.data or len(buffer) < total):
            progress(min(len(buffer), total), total)
        if len(buffer) < total:
            self._previous_frame_data = buffer if isinstance(buffer, bytearray) else bytearray(buffer)
            continue
        self._previous_frame_data = b""
        payload = XpcWrapper.parse(bytes(buffer)).payload
        if payload is None:
            continue
        if payload.type != XpcMessageType.DICTIONARY:
            raise ProtocolError(f"The device sent an XPC reply that is not a dictionary: {payload.type!r}")
        if not payload.data:
            # an empty dictionary carries nothing to answer with
            continue
        return decode_xpc_object(payload)


def install() -> None:
    """Replace ``RemoteXPCConnection.receive_response``; safe to call more than once."""
    global _installed
    if _installed:
        return
    from pymobiledevice3.remote.remotexpc import RemoteXPCConnection

    RemoteXPCConnection.receive_response = receive_response
    _installed = True
