"""Native FSEvents contract tests; all writes stay inside temporary fixtures."""

import ctypes
from contextlib import ExitStack, contextmanager
import importlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from types import SimpleNamespace
from unittest.mock import patch

try:
    events = importlib.import_module("jaso_nfc.events")
except ModuleNotFoundError:
    events = None


class RootMappingTests(unittest.TestCase):
    @contextmanager
    def mounted_root(self, child="", mount_device=42, mapped_mount_inode=5):
        logical_mount = "/Volumes/External"
        physical_mount = "/System/Volumes/Data/Volumes/External"
        logical = logical_mount + ("/" + child if child else "")
        physical = physical_mount + ("/" + child if child else "")
        root_info = SimpleNamespace(st_dev=42, st_ino=100 if child else 5)
        mount_info = SimpleNamespace(st_dev=mount_device, st_ino=5)
        def filesystem(_fd, pointer):
            ctypes.cast(pointer, ctypes.POINTER(events._StatFS)).contents.f_mntonname = os.fsencode(logical_mount)
            return 0
        def path_stat(path):
            path = path.rstrip("/")
            if path == physical_mount:
                return SimpleNamespace(st_dev=42, st_ino=mapped_mount_inode)
            if path == physical:
                return root_info
            raise AssertionError("Unexpected path metadata lookup: " + path)
        with ExitStack() as stack:
            stack.enter_context(patch.object(events, "_native", return_value=SimpleNamespace(fstatfs=filesystem)))
            stack.enter_context(patch.object(events.os, "open", side_effect=[10, 20]))
            stack.enter_context(patch.object(events.os, "close"))
            stack.enter_context(patch.object(events.os, "fstat", side_effect=lambda fd: {10: root_info, 20: mount_info}[fd]))
            stack.enter_context(patch.object(events.os, "stat", side_effect=path_stat))
            stack.enter_context(patch.object(events.fcntl, "fcntl", side_effect=lambda fd, *_: os.fsencode({10: physical, 20: physical_mount}[fd]) + b"\0"))
            yield logical, physical_mount

    def test_firmlinked_external_mount_maps_volume_root_and_child(self):
        for child in ("", "photos"):
            with self.subTest(child=child), self.mounted_root(child) as (logical, physical_mount):
                try:
                    mapped = events._root_info(logical)
                except OSError as error:
                    self.fail("Logical mount spelling must resolve through its open descriptor: " + str(error))
                self.assertEqual(mapped, (42, physical_mount, child))

    def test_mount_descriptor_must_still_belong_to_the_watched_device(self):
        with self.mounted_root("photos", mount_device=43) as (logical, _):
            with self.assertRaisesRegex(OSError, "mount changed"):
                events._root_info(logical)

    def test_resolved_mount_path_must_match_its_open_descriptor(self):
        with self.mounted_root("photos", mapped_mount_inode=8) as (logical, _):
            with self.assertRaisesRegex(OSError, "mount changed"):
                events._root_info(logical)


@unittest.skipUnless(sys.platform == "darwin", "native macOS API")
class EventTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(events, "native event adapter has not been implemented")
        self.temp = tempfile.TemporaryDirectory(prefix="jaso-events-")
        self.addCleanup(self.temp.cleanup)
        self.root = self.temp.name
        self.volume = events.discover_volumes([self.root])[0]
        self.streams = []
        self.addCleanup(lambda: [stream.stop() for stream in self.streams])

    def stream(self, callback, since=None):
        stream = events.Stream(self.volume, since, callback, latency=0.05)
        self.streams.append(stream)
        stream.start()
        return stream

    def foreign(self, code, *args):
        subprocess.run([sys.executable, "-c", code, *args], check=True)

    def wait(self, stream, condition, timeout=10):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if condition():
                return
            if stream.error:
                raise stream.error
            stream.flush()
            time.sleep(0.05)
        self.fail("native stream did not deliver expected event before deadline")

    def test_discovery_gives_each_root_a_stable_independent_cursor_key(self):
        child = os.path.join(self.root, "child")
        os.mkdir(child)
        volumes = events.discover_volumes([self.root, child, self.root])
        self.assertEqual([volume.roots for volume in volumes], [(self.root,), (child,)])
        self.assertEqual(len({volume.key for volume in volumes}), 2)
        self.assertEqual([v.key for v in volumes],
                         [v.key for v in events.discover_volumes([self.root, child])])
        for volume in volumes:
            self.assertEqual(volume.device, os.stat(self.root).st_dev)
            self.assertTrue(volume.uuid)
            self.assertTrue(os.path.isabs(volume.mount))

    def test_two_roots_start_deliver_and_replay_independently(self):
        roots = [os.path.join(self.root, name) for name in ("first", "second")]
        for root in roots:
            os.mkdir(root)
        volumes = events.discover_volumes(roots)
        seen = {volume.key: [] for volume in volumes}
        streams = []
        for volume in volumes:
            stream = events.Stream(volume, None, seen[volume.key].extend, latency=0.05)
            self.streams.append(stream)
            streams.append(stream)
            try:
                stream.start()
            except OSError as error:
                self.fail("multiple configured roots must start native watchers: " + str(error))
        live = os.path.join(roots[0], "live")
        self.foreign("from pathlib import Path; import sys; Path(sys.argv[1]).touch()", live)
        self.wait(streams[0], lambda: any(e.path == live for e in seen[volumes[0].key]))
        checkpoints = {}
        for volume, stream in zip(volumes, streams):
            stream.flush()
            checkpoints[volume.key] = max([stream.start_id] + [e.id for e in seen[volume.key]])
            stream.stop()
            seen[volume.key].clear()
        offline = [os.path.join(root, "한.txt") for root in roots]
        self.foreign("from pathlib import Path; import sys; [Path(p).touch() for p in sys.argv[1:]]", *offline)
        for volume, path in zip(volumes, offline):
            replay = events.Stream(volume, checkpoints[volume.key], seen[volume.key].extend, latency=0.05)
            self.streams.append(replay)
            replay.start()
            self.wait(replay, lambda: any(e.path == path for e in seen[volume.key]))
            self.assertTrue(all(not e.path or e.path.startswith(volume.roots[0] + os.sep)
                                or e.path == volume.roots[0] for e in seen[volume.key]))

    def test_multi_root_native_stream_is_rejected_before_registration(self):
        volume = events.Volume(self.volume.key, self.volume.device, self.volume.uuid,
                               self.volume.mount, (self.root, self.root + "/child"))
        with self.assertRaisesRegex(ValueError, "exactly one"):
            events.Stream(volume, None, lambda batch: None)

    def test_live_create_move_delete_and_owned_event_suppression(self):
        seen = []
        stream = self.stream(seen.extend)
        self.assertIsInstance(stream.start_id, int)
        self.assertGreaterEqual(stream.start_id, 0)
        original = os.path.join(self.root, "가.txt")
        renamed = os.path.join(self.root, "other.txt")
        self.foreign("from pathlib import Path; import sys; Path(sys.argv[1]).write_text('x')", original)
        self.wait(stream, lambda: any(e.path == original for e in seen))
        self.foreign("import os,sys; os.rename(sys.argv[1],sys.argv[2])", original, renamed)
        self.wait(stream, lambda: any(e.path == renamed for e in seen))
        seen.clear()
        self.foreign("import os,sys; os.unlink(sys.argv[1])", renamed)
        self.wait(stream, lambda: any(e.path == renamed and e.flags & events.ITEM_REMOVED for e in seen))
        own = os.path.join(self.root, "own-db-write")
        Path(own).write_text("owned")
        stream.flush()
        self.assertFalse(any(e.path == own for e in seen))
        stream.stop()
        stream.stop()

    def test_stopped_stream_replays_external_changes(self):
        seen = []
        stream = self.stream(seen.extend)
        checkpoint = stream.start_id
        stream.stop()
        path = os.path.join(self.root, "한.txt")
        self.foreign("from pathlib import Path; import sys; Path(sys.argv[1]).write_text('offline')", path)
        replay = self.stream(seen.extend, since=checkpoint)
        self.wait(replay, lambda: any(e.path == path for e in seen))
        self.wait(replay, lambda: any(e.flags & events.HISTORY_DONE for e in seen))

    def test_callback_errors_are_observable(self):
        def broken(batch):
            raise RuntimeError("durable inbox failed")
        stream = self.stream(broken)
        deadline = time.monotonic() + 5
        while not stream.error and time.monotonic() < deadline:
            stream.flush()
            time.sleep(0.05)
        self.assertIsInstance(stream.error, RuntimeError)
        self.assertEqual(str(stream.error), "durable inbox failed")

    def test_control_events_preserved_and_bytes_copied(self):
        seen = []
        stream = events.Stream(self.volume, 0, seen.extend)
        paths = (ctypes.c_char_p * 2)(b"ignored-history-path", b"")
        flags = (ctypes.c_uint32 * 2)(events.HISTORY_DONE, events.USER_DROPPED)
        ids = (ctypes.c_uint64 * 2)(12, 13)
        stream._receive(None, None, 2, ctypes.cast(paths, ctypes.c_void_p), flags, ids)
        self.assertEqual(seen, [events.Event("", events.HISTORY_DONE, 12), events.Event("", events.USER_DROPPED, 13)])
        ids[0] = 99
        self.assertEqual(seen[0].id, 12)

    def test_firmlink_root_maps_back_to_configured_path(self):
        # The fixture can live under /var; native paths resolve through /private.
        seen = []
        stream = self.stream(seen.extend)
        filename = os.path.join(self.root, "logical-path")
        self.foreign("from pathlib import Path; import sys; Path(sys.argv[1]).touch()", filename)
        self.wait(stream, lambda: any(e.path == filename for e in seen))
        self.assertFalse(any(e.path and not e.path.startswith(self.root) for e in seen))

    def test_native_loader_failure_is_reported_on_stream(self):
        stream = events.Stream(self.volume, None, lambda batch: None)
        error = OSError("native framework unavailable")
        with patch.object(events, "_native", side_effect=error):
            with self.assertRaisesRegex(OSError, "native framework unavailable"):
                stream.start()
        self.assertIs(stream.error, error)

    def test_failed_native_start_releases_resources(self):
        stream = events.Stream(self.volume, None, lambda batch: None)
        self.streams.append(stream)
        with patch.object(events._native(), "FSEventStreamStart", return_value=0):
            with self.assertRaisesRegex(OSError, "Cannot start FSEvents stream"):
                stream.start()
        self.assertIsInstance(stream.error, OSError)
        self.assertIsNone(stream._stream)
        self.assertIsNone(stream._queue)
        self.assertEqual(stream._strings, [])

    def test_stop_waits_for_callback_before_resource_release(self):
        entered = threading.Event()
        release = threading.Event()
        stopped = threading.Event()

        def slow(batch):
            entered.set()
            release.wait(5)

        stream = self.stream(slow)
        self.assertTrue(entered.wait(5))
        thread = threading.Thread(target=lambda: (stream.stop(), stopped.set()))
        thread.start()
        try:
            self.assertFalse(stopped.wait(0.05))
        finally:
            release.set()
            thread.join(5)
        self.assertTrue(stopped.is_set())
        self.assertIsNone(stream._stream)

    def test_root_rename_preserves_control_notification(self):
        seen = []
        parent = os.path.join(self.root, "watched")
        os.mkdir(parent)
        self.volume = events.discover_volumes([parent])[0]
        stream = self.stream(seen.extend)
        destination = os.path.join(self.root, "moved")
        self.foreign("import os,sys; os.rename(sys.argv[1],sys.argv[2])", parent, destination)
        self.wait(stream, lambda: any(e.flags & events.ROOT_CHANGED for e in seen))
        changed = next(e for e in seen if e.flags & events.ROOT_CHANGED)
        self.assertEqual(changed.id, 0)
        self.assertIn(changed.path, ("", parent))


if __name__ == "__main__":
    unittest.main()
