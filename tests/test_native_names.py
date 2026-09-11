"""Stored-name resolution uses descriptors without following symbolic links."""
import errno
import importlib
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unicodedata
import unittest
from unittest.mock import patch

try:
    native_names = importlib.import_module("jaso_nfc.native_names")
except ModuleNotFoundError:
    native_names = None


class NativeNameTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(native_names, "Descriptor-based name resolver is missing")

    def test_non_darwin_preserves_name_without_metadata_access(self):
        with patch.object(native_names.sys, "platform", "linux"), \
                patch.object(native_names.os, "open", side_effect=AssertionError("unexpected open")):
            self.assertEqual(native_names.actual_stored_name(4, "name"), "name")

    @unittest.skipUnless(sys.platform == "darwin", "Darwin descriptor path API")
    def test_actual_file_directory_and_symlink_names(self):
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            names = [unicodedata.normalize("NFD", "파일"),
                     unicodedata.normalize("NFD", "폴더"),
                     unicodedata.normalize("NFD", "링크")]
            (parent / names[0]).write_bytes(b"owned fixture")
            (parent / names[1]).mkdir()
            (parent / names[2]).symlink_to(names[0])
            descriptor = os.open(parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                for name in names:
                    with self.subTest(name=name):
                        self.assertEqual(native_names.actual_stored_name(descriptor, name), name)
            finally:
                os.close(descriptor)

    def test_stale_decomposed_listing_resolves_through_nfc_candidate(self):
        nfc = "파일"
        nfd = unicodedata.normalize("NFD", nfc)
        info = SimpleNamespace(st_dev=1, st_ino=2)
        with patch.object(native_names.sys, "platform", "darwin"), \
                patch.object(native_names.os, "open", side_effect=[FileNotFoundError(errno.ENOENT, "stale listing"), 10]), \
                patch.object(native_names.os, "close"), \
                patch.object(native_names.os, "fstat", return_value=info), \
                patch.object(native_names.os, "stat", return_value=info), \
                patch.object(native_names.fcntl, "fcntl", return_value=os.fsencode("/Volumes/owned/" + nfc) + b"\0"):
            self.assertEqual(native_names.actual_stored_name(4, nfd), nfc)

    def test_changed_parent_entry_is_not_accepted_as_the_opened_object(self):
        with patch.object(native_names.sys, "platform", "darwin"), \
                patch.object(native_names.os, "open", return_value=10), \
                patch.object(native_names.os, "close"), \
                patch.object(native_names.os, "fstat", return_value=SimpleNamespace(st_dev=1, st_ino=2)), \
                patch.object(native_names.os, "stat", return_value=SimpleNamespace(st_dev=1, st_ino=3)), \
                patch.object(native_names.fcntl, "fcntl", return_value=b"/owned/file\0"):
            with self.assertRaises(OSError) as raised:
                native_names.actual_stored_name(4, "file")
        self.assertEqual(raised.exception.errno, errno.ESTALE)

    def test_only_single_entry_names_are_accepted(self):
        for name in ("", ".", "..", "/absolute", "child/file"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                native_names.actual_stored_name(4, name)

    @unittest.skipUnless(sys.platform == "darwin", "Darwin descriptor xattrs")
    def test_marker_roundtrip_on_file_and_symlink_without_following(self):
        for function in ("open_entry", "marker_get", "marker_create", "marker_remove"):
            self.assertTrue(callable(getattr(native_names, function, None)), "Missing " + function)
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            (parent / "file").write_bytes(b"")
            (parent / "link").symlink_to("missing-owned-target")
            parent_fd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                for name in ("file", "link"):
                    with self.subTest(name=name):
                        descriptor = native_names.open_entry(parent_fd, name)
                        try:
                            key = "user.jaso_nfc.test-operation"
                            self.assertIsNone(native_names.marker_get(descriptor, key))
                            created = native_names.marker_create(descriptor, key, b"owned-token")
                            self.assertEqual(created.st_ino, os.fstat(descriptor).st_ino)
                            self.assertEqual(native_names.marker_get(descriptor, key), b"owned-token")
                            with self.assertRaises(OSError) as raised:
                                native_names.marker_create(descriptor, key, b"other-token")
                            self.assertEqual(raised.exception.errno, errno.EEXIST)
                            removed = native_names.marker_remove(descriptor, key, b"owned-token")
                            self.assertEqual(removed.st_ino, os.fstat(descriptor).st_ino)
                            self.assertIsNone(native_names.marker_get(descriptor, key))
                            native_names.marker_remove(descriptor, key, b"owned-token")
                        finally:
                            os.close(descriptor)
            finally:
                os.close(parent_fd)
            self.assertEqual(os.readlink(parent / "link"), "missing-owned-target")
            self.assertEqual((parent / "file").read_bytes(), b"")

    @unittest.skipUnless(sys.platform == "darwin", "Darwin descriptor xattrs")
    def test_marker_cleanup_refuses_another_operation_token(self):
        self.assertTrue(callable(getattr(native_names, "marker_remove", None)), "Missing marker_remove")
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "file"
            path.write_bytes(b"owned fixture")
            descriptor = os.open(path, os.O_RDONLY)
            try:
                key = "user.jaso_nfc.test-operation"
                native_names.marker_create(descriptor, key, b"owner")
                with self.assertRaises(OSError) as raised:
                    native_names.marker_remove(descriptor, key, b"impostor")
                self.assertEqual(raised.exception.errno, errno.ESTALE)
                self.assertEqual(native_names.marker_get(descriptor, key), b"owner")
            finally:
                os.close(descriptor)


if __name__ == "__main__":
    unittest.main()
