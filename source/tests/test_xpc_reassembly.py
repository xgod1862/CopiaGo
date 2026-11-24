import asyncio
import time
import unittest
from collections import deque
from types import SimpleNamespace

from pymobiledevice3.remote.remotexpc import RemoteXPCConnection
from pymobiledevice3.remote.xpc_message import create_xpc_wrapper

from iphone_clip import xpc_reassembly

FRAME = 16384  # what the device sends without a larger negotiated frame size
ORIGINAL = RemoteXPCConnection.__dict__["receive_response"]


class FakeConnection:
    """Feeds pre-cut DATA frames to receive_response."""

    def __init__(self, chunks):
        self._previous_frame_data = b""
        self._frames = deque(SimpleNamespace(data=chunk) for chunk in chunks)

    async def _receive_next_data_frame(self):
        return self._frames.popleft()


def frames(*messages, size=FRAME):
    chunks = []
    for message in messages:
        chunks += [message[i:i + size] for i in range(0, len(message), size)]
    return chunks


def receive(method, chunks, count=1):
    connection = FakeConnection(chunks)

    async def run():
        return [await method(connection) for _ in range(count)]

    replies = asyncio.run(run())
    return replies, connection


class ReassemblyTests(unittest.TestCase):
    def test_large_reply_matches_library_result(self):
        message = create_xpc_wrapper({"pasteboard": {"data": bytes(range(256)) * 4000}})
        chunks = frames(message)
        self.assertGreater(len(chunks), 10)
        ours, connection = receive(xpc_reassembly.receive_response, chunks)
        theirs, _ = receive(ORIGINAL, chunks)
        self.assertEqual(ours, theirs)
        self.assertFalse(connection._previous_frame_data)

    def test_header_split_across_frames(self):
        message = create_xpc_wrapper({"command": "x", "blob": b"\x01" * 100})
        ours, _ = receive(xpc_reassembly.receive_response, frames(message, size=7))
        self.assertEqual(ours[0]["blob"], b"\x01" * 100)

    def test_consecutive_replies_and_empty_ones_are_skipped(self):
        first = create_xpc_wrapper({"n": "1", "blob": b"a" * 50000})
        empty = create_xpc_wrapper({})
        second = create_xpc_wrapper({"n": "2"})
        ours, _ = receive(xpc_reassembly.receive_response, frames(first, empty, second), count=2)
        self.assertEqual([reply["n"] for reply in ours], ["1", "2"])

    def test_stream_without_magic_is_rejected(self):
        with self.assertRaises(Exception):
            receive(xpc_reassembly.receive_response, [b"\x00" * 64])

    def test_cost_grows_linearly(self):
        message = create_xpc_wrapper({"data": b"\x00" * (64 * 1024 * 1024)})
        started = time.perf_counter()
        ours, _ = receive(xpc_reassembly.receive_response, frames(message))
        # The library's own loop needs well over ten seconds for 64 MB.
        self.assertLess(time.perf_counter() - started, 5)
        self.assertEqual(len(ours[0]["data"]), 64 * 1024 * 1024)

    def test_install_replaces_method_once(self):
        try:
            xpc_reassembly.install()
            xpc_reassembly.install()
            self.assertIs(RemoteXPCConnection.receive_response, xpc_reassembly.receive_response)
        finally:
            RemoteXPCConnection.receive_response = ORIGINAL
            xpc_reassembly._installed = False


if __name__ == "__main__":
    unittest.main()
