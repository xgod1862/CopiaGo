import io
import plistlib
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

from PIL import Image, UnidentifiedImageError

from iphone_clip import clipboard, pair_store
from iphone_clip.clipboard import Clip, ClipboardWriter, ClipTooLarge, open_image
from test_clipboard import FakeClipboard


def png(width, height):
    buffer = io.BytesIO()
    Image.new("RGB", (width, height), "red").save(buffer, "PNG")
    return buffer.getvalue()


class ImageLimitTests(unittest.TestCase):
    def test_too_many_pixels_is_refused_before_decoding(self):
        with mock.patch.object(clipboard, "MAX_IMAGE_PIXELS", 50):
            with self.assertRaises(ClipTooLarge):
                open_image(png(10, 10))
            with self.assertRaises(ClipTooLarge):
                ClipboardWriter(1, FakeClipboard()).put(Clip(image=png(10, 10)))

    def test_too_many_bytes_is_refused(self):
        with mock.patch.object(clipboard, "MAX_IMAGE_BYTES", 10):
            with self.assertRaises(ClipTooLarge):
                open_image(png(2, 2))

    def test_formats_an_iphone_never_sends_are_not_decoded(self):
        buffer = io.BytesIO()
        Image.new("RGB", (4, 4)).save(buffer, "PCX")
        with self.assertRaises(UnidentifiedImageError):
            open_image(buffer.getvalue())

    def test_normal_photo_still_opens(self):
        with open_image(png(30, 20)) as image:
            self.assertEqual(image.size, (30, 20))


class DiagnosticPrivacyTests(unittest.TestCase):
    def test_unsupported_copy_is_described_without_names_or_content(self):
        from iphone_clip.device import describe_snapshot
        archive = plistlib.dumps({"$archiver": "NSKeyedArchiver", "$objects": ["$null", "季度汇报.pptx"], "$top": {}},
                                 fmt=plistlib.FMT_BINARY)
        snapshot = {"pasteboard": {"items": [{
            "types": ["com.apple.DocumentManager.FPItem.File", "public.html"],
            "data": {"com.apple.DocumentManager.FPItem.File": {"data": archive},
                     "public.html": {"data": b"<p>secret</p>"}}}]}}
        text = describe_snapshot(snapshot)
        self.assertIn("ext=pptx", text)
        self.assertIn("public.html=inline 13B", text)
        self.assertNotIn("季度汇报", text)
        self.assertNotIn("secret", text)


class PairStoreTests(unittest.TestCase):
    def setUp(self):
        self.folder = Path(tempfile.mkdtemp())
        import pymobiledevice3.common as common
        self.common, self.home = common, common._HOMEFOLDER
        common._HOMEFOLDER = self.folder

    def tearDown(self):
        self.common._HOMEFOLDER = self.home

    def test_existing_plaintext_record_is_sealed_and_still_readable_by_the_library(self):
        record = {"public_key": b"p" * 32, "private_key": b"s" * 32, "remote_unlock_host_key": ""}
        path = self.folder / "remote_00008130-ABC.plist"
        path.write_bytes(plistlib.dumps(record))
        pair_store.install(self.folder)
        self.assertTrue(path.read_bytes().startswith(pair_store.MAGIC))
        self.assertNotIn(b"s" * 32, path.read_bytes())

        from pymobiledevice3 import pair_records
        from pymobiledevice3.remote import tunnel_service, userspace_tunnel
        self.assertEqual(pair_records.get_local_pairing_record("remote_00008130-ABC", self.folder), record)
        self.assertEqual(userspace_tunnel.get_local_pairing_record("remote_00008130-ABC", self.folder), record)
        found = list(tunnel_service.iter_remote_pair_records_by_identifier())
        self.assertEqual([(identifier, data) for identifier, _, data in found], [("00008130-ABC", record)])

    def test_new_records_are_sealed_when_the_library_saves_them(self):
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
        from pymobiledevice3.remote.tunnel_service import RemotePairingProtocol
        pair_store.install(self.folder)
        key = Ed25519PrivateKey.generate()
        fake = types.SimpleNamespace(ed25519_private_key=key, remote_unlock_host_key=b"u", peer_alt_irk=None,
                                     pair_record_path=self.folder / "remote_NEW.plist")
        RemotePairingProtocol.save_pair_record(fake)
        self.assertTrue(fake.pair_record_path.read_bytes().startswith(pair_store.MAGIC))
        self.assertEqual(RemotePairingProtocol.pair_record.fget(fake)["private_key"], key.private_bytes_raw())

    def test_non_records_are_left_alone(self):
        path = self.folder / "remote_bad.plist"
        path.write_bytes(b"not a plist")
        pair_store.seal_all(self.folder)
        self.assertEqual(path.read_bytes(), b"not a plist")


if __name__ == "__main__":
    unittest.main()
