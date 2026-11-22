import asyncio
import unittest
from unittest import mock

from iphone_clip import device

LISTING_POLICY = {"allPromised": {}}


def snapshot(types, data=None, count=1):
    return {"pasteboard": {"items": [{"types": types, "data": data or {}}],
                           "metadata": {"nonce": "n", "changeCount": count}}}


class FakeService:
    """Answers PULLs by policy; records which policies were asked for."""
    calls: list = []
    answers: dict = {}

    def __init__(self, rsd): pass
    async def __aenter__(self): return self
    async def __aexit__(self, *exc): return False

    async def get(self, name, policy):
        FakeService.calls.append(next(iter(policy)))
        answer = FakeService.answers[next(iter(policy))]
        if isinstance(answer, Exception):
            raise answer
        return answer


def read(answers):
    FakeService.calls, FakeService.answers = [], answers
    with mock.patch("pymobiledevice3.remote.core_device.pasteboard_service.PasteboardService", FakeService):
        return asyncio.run(device.read_clip(object()))


class ReadClipTests(unittest.TestCase):
    def test_video_only_copy_is_not_fetched(self):
        listing = snapshot(["com.apple.mobileslideshow.asset.localidentifier", "com.apple.quicktime-movie"])
        result, clip = read({"allPromised": listing})
        self.assertIsNone(clip)
        self.assertIs(result, listing)
        self.assertEqual(FakeService.calls, ["allPromised"])  # the phone is never asked to prepare the video

    def test_text_is_fetched_after_the_listing(self):
        text = "public.utf8-plain-text"
        _, clip = read({"allPromised": snapshot([text]),
                        "promiseSecondary": snapshot([text], {text: {"data": "你好".encode()}})})
        self.assertEqual(clip.text, "你好")
        self.assertEqual(FakeService.calls, ["allPromised", "promiseSecondary"])

    def test_slow_content_raises_with_the_listing(self):
        listing = snapshot(["public.utf8-plain-text"], count=7)
        with self.assertRaises(device.PullTimeout) as caught:
            read({"allPromised": listing, "promiseSecondary": TimeoutError()})
        self.assertEqual(device.change_id(caught.exception.listing), ("n", 7))


if __name__ == "__main__":
    unittest.main()
