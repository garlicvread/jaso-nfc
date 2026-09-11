"""Durability and bounded-work tests using an injected filesystem reconciler."""
import os
from contextlib import closing
from pathlib import Path
import sqlite3
import stat
import tempfile
import threading
import unittest
import unicodedata
from types import SimpleNamespace
from unittest.mock import patch

try:
    from jaso_nfc.index import Index
except ImportError:
    Index = None


CREATED, REMOVED, RENAMED, IS_DIR = 0x100, 0x200, 0x800, 0x20000


def event(path, event_id, flags=0):
    return SimpleNamespace(path=str(path), id=event_id, flags=flags)


def entry(path, kind="file", ino=1):
    return dict(path=str(path), kind=kind, dev=1, ino=ino, mtime_ns=1,
                ctime_ns=1, size=1, mode=0o600 | (
                    stat.S_IFDIR if kind in ("dir", "directory") else
                    stat.S_IFLNK if kind == "symlink" else stat.S_IFREG))


class Policy:
    def __init__(self, root, excluded=()):
        self.root = str(root)
        self.excluded = tuple(map(str, excluded))

    def contains(self, path):
        return path == self.root or path.startswith(self.root + os.sep)

    def accepts(self, path):
        return self.contains(path) and not any(
            path == x or path.startswith(x + os.sep) for x in self.excluded)

    def descend(self, path):
        return self.accepts(path)


class Reconciler:
    def __init__(self, root):
        self.policy = Policy(root)
        self.calls = []
        self.results = {}
        self.retries = []
        self.during_scan = None

    def reconcile(self, path, recursive):
        self.calls.append((path, recursive))
        if self.during_scan:
            callback, self.during_scan = self.during_scan, None
            callback()
        result = self.results.get(path, {})
        if isinstance(result, Exception):
            raise result
        scope = result.get("scope", path)
        entries = result.get("entries", [])
        directories = result.get("directories", [scope])
        if not recursive:
            entries = [item for item in entries if os.path.dirname(item["path"]) == scope]
            directories = [scope] if scope in directories else []
        return dict(scope=scope, entries=entries, directories=directories,
                    errors=result.get("errors", []), renamed=result.get("renamed", 0))

    def retry_paths(self, now=None):
        retries, self.retries = self.retries, []
        return retries


class IndexTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(Index, "The persistent Index implementation is missing")
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = str(Path(self.tmp.name) / "root")
        Path(self.root).mkdir()
        self.db_path = str(Path(self.tmp.name) / "index.sqlite3")
        self.volume = SimpleNamespace(key="volume-a", uuid="volume-a", device=1,
                                      mount="/", roots=(self.root,))
        self.normalizer = Reconciler(self.root)
        self.index = Index(self.db_path)
        self.addCleanup(lambda: self.index.close())
        self.index.bind_policy(self.normalizer.policy)
        self.assertTrue(self.index.configure("config-a", [self.volume], [self.root]))

    def complete_baseline(self, entries=(), directories=None):
        entries = list(entries)
        scopes = set(directories or [self.root]) | {item["path"] for item in entries
                                                   if item["kind"] in ("directory", "dir")}
        for scope in scopes:
            self.normalizer.results[scope] = {"entries": [item for item in entries
                if item["path"].startswith(scope + os.sep)], "directories": [scope]}
        self.index.bootstrap_jobs()
        self.assertTrue(self.index.work(self.normalizer))
        while self.index.work(self.normalizer):
            pass
        self.normalizer.calls.clear()

    def stored_paths(self):
        with closing(sqlite3.connect(self.db_path)) as db:
            return {row[0] for row in db.execute("SELECT path FROM entries")}

    def test_baseline_is_explicit_and_restart_uses_existing_index(self):
        self.assertEqual(self.index.status()["pending_jobs"], 0)
        self.complete_baseline([entry(Path(self.root) / "a")])
        self.index.close()
        self.index = Index(self.db_path)
        self.index.bind_policy(self.normalizer.policy)
        self.assertFalse(self.index.configure("config-a", [self.volume], [self.root]))
        self.index.bootstrap_jobs()
        self.assertFalse(self.index.work(self.normalizer))
        self.assertEqual(self.index.status()["baseline_walks"], 1)
        self.assertEqual(self.index.status()["indexed_entries"], 1)

    def test_events_and_cursor_survive_reopen_before_work(self):
        self.complete_baseline()
        self.index.enqueue("volume-a", [event(Path(self.root) / "a", 51, CREATED)])
        self.index.close()
        self.index = Index(self.db_path)
        self.index.bind_policy(self.normalizer.policy)
        self.assertFalse(self.index.configure("config-a", [self.volume], [self.root]))
        self.assertEqual(self.index.cursor("volume-a"), 51)
        self.assertTrue(self.index.work(self.normalizer))
        self.assertEqual(self.normalizer.calls, [(self.root, False)])
        self.assertEqual(self.index.status()["pending_jobs"], 0)

    def test_file_burst_coalesces_to_one_shallow_parent(self):
        self.complete_baseline()
        self.index.enqueue("volume-a", [event(Path(self.root) / str(i), i + 1)
                                        for i in range(1000)])
        self.assertEqual(self.index.status()["pending_jobs"], 1)
        self.index.work(self.normalizer)
        self.assertEqual(self.normalizer.calls, [(self.root, False)])
        self.assertEqual(self.index.status()["shallow_scans"], 1)

    def test_known_nfc_file_content_events_acknowledge_without_directory_work(self):
        path = str(Path(self.root) / "known.txt")
        self.complete_baseline([entry(path)])
        flags = [0x11000, 0x10400, 0x12000, 0x14000, 0x18000, 0x15400]
        self.index.enqueue("volume-a", [event(path, number, value)
                                        for number, value in enumerate(flags, 1)])
        self.assertFalse(self.index.work(self.normalizer))
        self.assertEqual(self.normalizer.calls, [])
        self.assertEqual(self.index.status()["pending_jobs"], 0)
        self.assertEqual(self.index.cursor("volume-a"), len(flags))
        self.assertEqual(self.index.status()["ignored_content_events"], len(flags))
        self.index.close()
        self.index = Index(self.db_path)
        self.index.bind_policy(self.normalizer.policy)
        self.assertEqual(self.index.cursor("volume-a"), len(flags))
        self.assertEqual(self.index.status()["ignored_content_events"], len(flags))

    def test_unknown_file_content_event_discovers_its_name(self):
        self.complete_baseline()
        path = str(Path(self.root) / "new.txt")
        self.normalizer.results[self.root] = {"entries": [entry(path)]}
        self.index.enqueue("volume-a", [event(path, 1, 0x11000)])
        self.assertTrue(self.index.work(self.normalizer))
        self.assertEqual(self.stored_paths(), {path})

    def test_known_nfd_candidate_content_event_still_schedules_normalization(self):
        old = str(Path(self.root) / unicodedata.normalize("NFD", "파일.txt"))
        final = unicodedata.normalize("NFC", old)
        self.complete_baseline([entry(old)])
        self.normalizer.results[self.root] = {"entries": [entry(final)]}
        self.index.enqueue("volume-a", [event(old, 1, 0x11000)])
        self.assertTrue(self.index.work(self.normalizer))
        self.assertEqual(self.stored_paths(), {final})

    def test_mixed_lifecycle_and_unknown_flags_still_reconcile_known_files(self):
        path = str(Path(self.root) / "known.txt")
        self.complete_baseline([entry(path)])
        flags = [0x11000 | CREATED, 0x11000 | REMOVED, 0x11000 | RENAMED,
                 0x11000 | 0x400000, 0x11000 | 0x10000000, 0x10000, 0x1000,
                 0x11000 | 0x1, 0x11000 | 0x2]
        for number, value in enumerate(flags, 1):
            with self.subTest(flags=hex(value)):
                self.index.enqueue("volume-a", [event(path, number, value)])
                self.assertTrue(self.index.work(self.normalizer))
                while self.index.work(self.normalizer):
                    pass
                self.assertEqual(self.index.cursor("volume-a"), number)

    def test_directory_and_non_regular_cached_entries_keep_metadata_work(self):
        folder = str(Path(self.root) / "folder")
        link = str(Path(self.root) / "link")
        self.complete_baseline([entry(folder, "dir"), entry(link, "symlink")])
        for number, (path, flags) in enumerate([(folder, 0x28000), (link, 0x11000)], 1):
            self.index.enqueue("volume-a", [event(path, number, flags)])
            self.assertTrue(self.index.work(self.normalizer))
            while self.index.work(self.normalizer):
                pass

    def test_special_or_unknown_cached_file_modes_keep_metadata_work(self):
        observations = []
        for number, mode in enumerate((stat.S_IFIFO | 0o600, stat.S_IFSOCK | 0o600, 0o600, None)):
            observed = entry(Path(self.root) / str(number))
            observed["mode"] = mode
            observations.append(observed)
        self.complete_baseline(observations)
        for number, observed in enumerate(observations, 1):
            with self.subTest(mode=observed["mode"]):
                self.index.enqueue("volume-a", [event(observed["path"], number, 0x11000)])
                self.assertTrue(self.index.work(self.normalizer))

    def test_ignored_content_event_preserves_existing_scan_retry_deadline(self):
        path = str(Path(self.root) / "known.txt")
        self.complete_baseline([entry(path)])
        self.normalizer.results[self.root] = PermissionError("fixture blocked parent")
        self.index.enqueue("volume-a", [event(path, 1)])
        with patch("jaso_nfc.index.time.time", return_value=100), \
                self.assertLogs("jaso_nfc.index", "WARNING"):
            self.index.work(self.normalizer)
        deadline = self.index.status()["next_retry"]
        self.normalizer.results[self.root] = {"entries": [entry(path)]}
        with patch("jaso_nfc.index.time.time", return_value=101):
            self.index.enqueue("volume-a", [event(path, 2, 0x11000)])
            self.assertFalse(self.index.work(self.normalizer))
        self.assertEqual(self.index.status()["next_retry"], deadline)
        self.assertEqual(self.index.cursor("volume-a"), 2)
        with patch("jaso_nfc.index.time.time", return_value=deadline):
            self.assertTrue(self.index.work(self.normalizer))

    def test_event_during_scan_survives_acknowledgement_and_does_not_block(self):
        self.complete_baseline()
        self.index.enqueue("volume-a", [event(Path(self.root) / "a", 1)])
        def enqueue_in_callback_thread():
            thread = threading.Thread(target=lambda: self.index.enqueue(
                "volume-a", [event(Path(self.root) / "b", 2)]))
            thread.start()
            thread.join(2)
            self.assertFalse(thread.is_alive(), "A scan held the SQLite lock")
        self.normalizer.during_scan = enqueue_in_callback_thread
        self.index.work(self.normalizer)
        self.assertEqual(self.index.status()["pending_jobs"], 1)
        self.index.work(self.normalizer)
        self.assertEqual(self.index.status()["pending_jobs"], 0)
        self.assertEqual(self.index.cursor("volume-a"), 2)

    def test_failed_scan_preserves_index_and_is_backed_off(self):
        old = str(Path(self.root) / "old")
        self.complete_baseline([entry(old)])
        self.normalizer.results[self.root] = PermissionError("not readable")
        self.index.enqueue("volume-a", [event(old, 1)])
        self.assertTrue(self.index.work(self.normalizer))
        self.assertEqual(self.stored_paths(), {old})
        self.assertEqual(self.index.status()["pending_jobs"], 1)
        self.assertFalse(self.index.work(self.normalizer))
        self.assertEqual(self.index.status()["errors"], 1)

    def test_new_directory_is_recursive_and_existing_metadata_is_shallow(self):
        folder = str(Path(self.root) / "folder")
        self.complete_baseline()
        self.index.enqueue("volume-a", [event(folder, 1, CREATED | IS_DIR)])
        self.normalizer.results[folder] = {"entries": [entry(Path(folder) / "a")],
                                           "directories": [folder]}
        self.normalizer.results[self.root] = {"entries": [entry(folder, "directory")]}
        while self.index.work(self.normalizer):
            pass
        self.assertIn((folder, False), self.normalizer.calls)
        self.normalizer.calls.clear()
        self.index.enqueue("volume-a", [event(folder, 2, IS_DIR)])
        self.index.work(self.normalizer)
        self.assertEqual(self.normalizer.calls, [(self.root, False)])

    def test_directory_removal_prunes_old_descendants(self):
        folder = str(Path(self.root) / "folder")
        child = str(Path(folder) / "old")
        self.complete_baseline([entry(folder, "directory"), entry(child)],
                               [self.root, folder])
        self.normalizer.results[self.root] = {}
        self.index.enqueue("volume-a", [event(folder, 1, REMOVED | IS_DIR)])
        self.index.work(self.normalizer)
        self.assertEqual(self.stored_paths(), set())

    def test_moved_directory_does_not_leave_stale_descendants(self):
        before = str(Path(self.root) / "before")
        after = str(Path(self.root) / "after")
        self.complete_baseline([entry(before, "directory", 50),
                                entry(Path(before) / "child", ino=51)], [self.root, before])
        self.normalizer.results[self.root] = {"entries": [entry(after, "directory", 50)]}
        self.normalizer.results[before] = {"directories": []}
        self.normalizer.results[after] = {"entries": [entry(Path(after) / "child", ino=51)],
                                          "directories": [after]}
        self.index.enqueue("volume-a", [event(before, 1, RENAMED | IS_DIR),
                                        event(after, 2, RENAMED | IS_DIR)])
        while self.index.work(self.normalizer):
            pass
        self.assertNotIn(str(Path(before) / "child"), self.stored_paths())
        self.assertIn(str(Path(after) / "child"), self.stored_paths())

    def test_control_events_recover_roots_and_ignore_history_done_path(self):
        self.complete_baseline()
        self.index.enqueue("volume-a", [event("", 4, 0x10)])
        self.assertEqual(self.index.status()["pending_jobs"], 0)
        self.index.enqueue("volume-a", [event("", 5, 0x2)])
        self.index.work(self.normalizer)
        self.assertEqual(self.normalizer.calls, [(self.root, False)])

    def test_root_changed_requires_revalidation(self):
        self.complete_baseline()
        self.index.enqueue("volume-a", [event(self.root, 1, 0x20)])
        self.assertTrue(self.index.status()["needs_revalidation"])
        with self.assertRaisesRegex(RuntimeError, "revalidat"):
            self.index.work(self.normalizer)

    def test_excluded_events_do_not_write_jobs_but_advance_cursor(self):
        excluded = str(Path(self.root) / "state")
        self.normalizer.policy = Policy(self.root, [excluded])
        self.index.bind_policy(self.normalizer.policy)
        self.complete_baseline()
        self.index.enqueue("volume-a", [event(Path(excluded) / "index.sqlite3", 90)])
        self.assertEqual(self.index.cursor("volume-a"), 90)
        self.assertEqual(self.index.status()["pending_jobs"], 0)

    def test_changed_config_invalidates_cursor_and_baseline(self):
        self.complete_baseline()
        self.index.enqueue("volume-a", [event(Path(self.root) / "a", 9)])
        self.assertTrue(self.index.configure("config-b", [self.volume], [self.root]))
        self.assertIsNone(self.index.cursor("volume-a"))
        self.assertFalse(self.index.status()["baseline_complete"])
        self.assertEqual(self.index.status()["pending_jobs"], 0)

    def second_volume(self):
        root = str(Path(self.tmp.name) / "second")
        Path(root).mkdir(exist_ok=True)
        self.normalizer.policy = Policy(self.tmp.name)
        self.index.bind_policy(self.normalizer.policy)
        return SimpleNamespace(key="volume-b", uuid="disk-b", device=2,
                               mount="/", roots=(root,))

    def configure_volumes(self, *volumes):
        return self.index.configure("config-a", volumes,
                                    [root for volume in volumes for root in volume.roots])

    def test_added_root_preserves_existing_cursor_entries_and_retry_job(self):
        old = str(Path(self.root) / "old")
        self.complete_baseline([entry(old)])
        self.index.enqueue("volume-a", [event(old, 51)])
        self.normalizer.results[self.root] = PermissionError("temporarily unavailable")
        with self.assertLogs("jaso_nfc.index", "WARNING"):
            self.index.work(self.normalizer)
        with closing(sqlite3.connect(self.db_path)) as db:
            before = db.execute("SELECT * FROM jobs WHERE volume_key='volume-a'").fetchall()
        second = self.second_volume()
        self.normalizer.calls.clear()
        self.assertTrue(self.configure_volumes(self.volume, second))
        self.assertEqual(self.index.cursor("volume-a"), 51)
        self.assertIsNone(self.index.cursor(second.key))
        self.assertEqual(self.stored_paths(), {old})
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT * FROM jobs WHERE volume_key='volume-a'").fetchall(), before)
            self.assertEqual(db.execute("SELECT COUNT(*) FROM jobs WHERE volume_key=?", (second.key,)).fetchone()[0], 0)
        self.index.bootstrap_jobs()
        self.assertTrue(self.index.work(self.normalizer))
        self.assertEqual(self.normalizer.calls, [(second.roots[0], False)])
        self.assertTrue(self.index.status()["baseline_complete"])
        self.assertEqual(self.index.status()["baseline_walks"], 2)

    def test_removed_root_preserves_other_root_state_and_reconnect_scans_only_returning_root(self):
        old = str(Path(self.root) / "old")
        self.complete_baseline([entry(old)])
        second = self.second_volume()
        second_file = str(Path(second.roots[0]) / "file")
        self.normalizer.results[second.roots[0]] = {"entries": [entry(second_file)]}
        self.configure_volumes(self.volume, second)
        self.index.bootstrap_jobs()
        while self.index.work(self.normalizer):
            pass
        self.index.enqueue(self.volume.key, [event(old, 51)])
        self.index.enqueue(second.key, [event(second_file, 72)])
        with closing(sqlite3.connect(self.db_path)) as db:
            before = db.execute("SELECT * FROM jobs WHERE volume_key='volume-a'").fetchall()
        self.assertFalse(self.configure_volumes(self.volume))
        self.assertEqual(self.index.cursor(self.volume.key), 51)
        self.assertEqual(self.stored_paths(), {old})
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT * FROM jobs").fetchall(), before)
        with self.assertRaises(KeyError):
            self.index.cursor(second.key)
        self.index.work(self.normalizer)
        self.normalizer.calls.clear()
        self.assertTrue(self.configure_volumes(self.volume, second))
        self.assertIsNone(self.index.cursor(second.key))
        self.index.bootstrap_jobs()
        self.assertTrue(self.index.work(self.normalizer))
        self.assertEqual(self.normalizer.calls, [(second.roots[0], False)])
        self.assertFalse(self.index.work(self.normalizer))

    def test_replaced_volume_uuid_resets_only_its_coverage(self):
        old = str(Path(self.root) / "old")
        self.complete_baseline([entry(old)])
        self.index.seed_cursor(self.volume.key, 51)
        second = self.second_volume()
        self.configure_volumes(self.volume, second)
        self.index.bootstrap_jobs()
        self.index.work(self.normalizer)
        self.index.seed_cursor(second.key, 72)
        self.normalizer.calls.clear()
        second.uuid = "replacement-disk"
        self.assertTrue(self.configure_volumes(self.volume, second))
        self.assertEqual(self.index.cursor(self.volume.key), 51)
        self.assertEqual(self.stored_paths(), {old})
        self.assertIsNone(self.index.cursor(second.key))
        self.index.bootstrap_jobs()
        self.index.work(self.normalizer)
        self.assertEqual(self.normalizer.calls, [(second.roots[0], False)])

    def test_added_root_pending_baseline_survives_crash_before_stream_bootstrap(self):
        self.complete_baseline()
        self.index.seed_cursor(self.volume.key, 51)
        second = self.second_volume()
        self.assertTrue(self.configure_volumes(self.volume, second))
        self.index.close()
        self.index = Index(self.db_path)
        self.index.bind_policy(self.normalizer.policy)
        self.assertTrue(self.configure_volumes(self.volume, second))
        self.assertEqual(self.index.cursor(self.volume.key), 51)
        self.assertEqual(self.index.status()["pending_jobs"], 0)
        self.index.bootstrap_jobs()
        self.index.work(self.normalizer)
        self.assertEqual(self.normalizer.calls, [(second.roots[0], False)])
        self.assertTrue(self.index.status()["baseline_complete"])

    def test_existing_work_cannot_complete_a_pending_new_root_baseline(self):
        self.complete_baseline()
        self.index.enqueue(self.volume.key, [event(Path(self.root) / "a", 51)])
        second = self.second_volume()
        self.configure_volumes(self.volume, second)
        self.assertTrue(self.index.work(self.normalizer))
        self.assertFalse(self.index.status()["baseline_complete"])
        self.index.bootstrap_jobs()
        self.index.work(self.normalizer)
        self.assertTrue(self.index.status()["baseline_complete"])

    def test_revalidation_reconciles_only_the_affected_root_and_retains_its_cursor(self):
        old = str(Path(self.root) / "old")
        self.complete_baseline([entry(old)])
        self.index.seed_cursor(self.volume.key, 51)
        second = self.second_volume()
        self.configure_volumes(self.volume, second)
        self.index.bootstrap_jobs()
        self.index.work(self.normalizer)
        self.index.enqueue(second.key, [event(second.roots[0], 72, 0x20)])
        self.assertTrue(self.configure_volumes(self.volume, second))
        self.assertFalse(self.index.status()["needs_revalidation"])
        self.assertEqual(self.index.cursor(self.volume.key), 51)
        self.assertEqual(self.stored_paths(), {old})
        self.assertEqual(self.index.cursor(second.key), 72)
        self.normalizer.calls.clear()
        self.index.bootstrap_jobs()
        self.index.work(self.normalizer)
        self.assertEqual(self.normalizer.calls, [(second.roots[0], False)])

    def test_no_available_roots_is_an_idle_complete_index(self):
        self.complete_baseline([entry(Path(self.root) / "old")])
        self.index.enqueue(self.volume.key, [event(Path(self.root) / "a", 51)])
        self.assertFalse(self.configure_volumes())
        self.index.bootstrap_jobs()
        self.assertFalse(self.index.work(self.normalizer))
        self.assertTrue(self.index.status()["baseline_complete"])
        self.assertEqual(self.index.status()["pending_jobs"], 0)
        self.assertEqual(self.index.status()["cursors"], {})
        self.assertEqual(self.stored_paths(), set())
        self.assertFalse(self.configure_volumes())
        self.assertTrue(self.configure_volumes(self.volume))
        self.assertIsNone(self.index.cursor(self.volume.key))

    def test_parent_scan_preserves_separately_indexed_unvisited_nested_root(self):
        self.complete_baseline()
        nested = str(Path(self.root) / "Library" / "CloudStorage")
        Path(nested).mkdir(parents=True)
        cloud = SimpleNamespace(key="cloud", uuid=self.volume.uuid, device=1,
                                mount="/", roots=(nested,))
        child = str(Path(nested) / "file")
        self.configure_volumes(self.volume, cloud)
        self.normalizer.results[nested] = {"entries": [entry(child)]}
        self.index.bootstrap_jobs()
        while self.index.work(self.normalizer):
            pass
        self.normalizer.results[self.root] = {"directories": [self.root]}
        self.index.request_reconcile([self.root])
        self.index.work(self.normalizer)
        self.assertEqual(self.stored_paths(), {child})
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertIsNotNone(db.execute("SELECT 1 FROM directories WHERE path=?", (nested,)).fetchone())
        self.configure_volumes(self.volume)
        self.assertEqual(self.stored_paths(), set())

    def test_parent_scan_prunes_nested_root_when_known_missing(self):
        self.complete_baseline()
        nested = str(Path(self.root) / "cloud")
        Path(nested).mkdir()
        cloud = SimpleNamespace(key="cloud", uuid=self.volume.uuid, device=1,
                                mount="/", roots=(nested,))
        self.configure_volumes(self.volume, cloud)
        self.normalizer.results[nested] = {"entries": [entry(Path(nested) / "old")]}
        self.index.bootstrap_jobs()
        while self.index.work(self.normalizer):
            pass
        Path(nested).rmdir()
        self.index.request_reconcile([self.root])
        self.index.work(self.normalizer)
        self.assertEqual(self.stored_paths(), set())

    def test_discovery_change_preserves_interrupted_baseline_and_deferred_events(self):
        self.index.bootstrap_jobs()
        def arrive_then_interrupt():
            self.index.enqueue(self.volume.key, [event(Path(self.root) / "changed", 51)])
            raise KeyboardInterrupt("interrupted baseline")
        self.normalizer.during_scan = arrive_then_interrupt
        with self.assertRaises(KeyboardInterrupt):
            self.index.work(self.normalizer)
        with closing(sqlite3.connect(self.db_path)) as db:
            jobs = db.execute("SELECT * FROM jobs").fetchall()
            deferred = db.execute("SELECT * FROM deferred_jobs").fetchall()
        second = self.second_volume()
        self.assertTrue(self.configure_volumes(self.volume, second))
        with closing(sqlite3.connect(self.db_path)) as db:
            self.assertEqual(db.execute("SELECT * FROM jobs").fetchall(), jobs)
            self.assertEqual(db.execute("SELECT * FROM deferred_jobs").fetchall(), deferred)
        self.assertEqual(self.index.cursor(self.volume.key), 51)
        self.normalizer.calls.clear()
        self.index.bootstrap_jobs()
        while self.index.work(self.normalizer):
            pass
        self.assertEqual(self.normalizer.calls, [(self.root, False), (second.roots[0], False)])
        self.assertTrue(self.index.status()["baseline_complete"])

    def test_missing_new_root_does_not_reset_other_coverage_on_rediscovery(self):
        old = str(Path(self.root) / "old")
        self.complete_baseline([entry(old)])
        self.index.seed_cursor(self.volume.key, 51)
        second = self.second_volume()
        self.configure_volumes(self.volume, second)
        self.index.bootstrap_jobs()
        Path(second.roots[0]).rmdir()
        with self.assertRaisesRegex(RuntimeError, "revalidat"):
            self.index.work(self.normalizer)
        self.assertFalse(self.configure_volumes(self.volume))
        self.assertEqual(self.index.cursor(self.volume.key), 51)
        self.assertEqual(self.stored_paths(), {old})
        self.assertTrue(self.index.status()["baseline_complete"])
        self.assertFalse(self.index.work(self.normalizer))

    def test_parent_partial_error_preserves_independent_nested_root(self):
        self.complete_baseline()
        nested = str(Path(self.root) / "cloud")
        Path(nested).mkdir()
        cloud = SimpleNamespace(key="cloud", uuid=self.volume.uuid, device=1,
                                mount="/", roots=(nested,))
        child = str(Path(nested) / "file")
        self.configure_volumes(self.volume, cloud)
        self.normalizer.results[nested] = {"entries": [entry(child)]}
        self.index.bootstrap_jobs()
        self.index.work(self.normalizer)
        self.normalizer.results[self.root] = {
            "directories": [self.root],
            "errors": [{"path": str(Path(self.root) / "blocked"), "error": "denied"}]}
        self.index.request_reconcile([self.root])
        with self.assertLogs("jaso_nfc.index", "WARNING"):
            self.index.work(self.normalizer)
        self.assertEqual(self.stored_paths(), {child})

    def nested_volume(self):
        root = str(Path(self.root) / "nested")
        Path(root).mkdir()
        nested = SimpleNamespace(key="nested-key", uuid=self.volume.uuid,
                                 device=self.volume.device, mount="/", roots=(root,))
        self.configure_volumes(self.volume, nested)
        self.index.bootstrap_jobs()
        while self.index.work(self.normalizer):
            pass
        self.normalizer.calls.clear()
        return nested

    def test_parent_delivered_nested_work_survives_parent_source_removal(self):
        nested = self.nested_volume()
        new = str(Path(nested.roots[0]) / "new")
        self.normalizer.results[nested.roots[0]] = {"entries": [entry(new)]}
        self.index.enqueue(self.volume.key, [event(new, 2)])
        self.configure_volumes(nested)
        self.assertTrue(self.index.work(self.normalizer))
        self.assertIn(new, self.stored_paths())
        self.assertEqual(self.normalizer.calls, [(nested.roots[0], False)])

    def test_overlapping_callbacks_keep_job_after_nested_cursor_advances(self):
        nested = self.nested_volume()
        new = str(Path(nested.roots[0]) / "new")
        self.normalizer.results[nested.roots[0]] = {"entries": [entry(new)]}
        self.index.enqueue(self.volume.key, [event(new, 2)])
        self.index.enqueue(nested.key, [event(new, 3)])
        self.configure_volumes(nested)
        self.assertEqual(self.index.cursor(nested.key), 3)
        self.assertTrue(self.index.work(self.normalizer))
        self.assertIn(new, self.stored_paths())
        self.assertEqual(self.index.status()["baseline_walks"], 2)

    def test_nested_root_directory_event_from_parent_scans_nested_root(self):
        nested = self.nested_volume()
        self.index.enqueue(self.volume.key, [event(nested.roots[0], 2, IS_DIR)])
        self.assertTrue(self.index.work(self.normalizer))
        self.assertEqual(self.normalizer.calls, [(nested.roots[0], False)])

    def test_existing_parent_owned_nested_job_is_rehomed_before_parent_removal(self):
        nested = self.nested_volume()
        new = str(Path(nested.roots[0]) / "new")
        self.normalizer.results[nested.roots[0]] = {"entries": [entry(new)]}
        self.index.enqueue(self.volume.key, [event(new, 2)])
        # Existing databases may contain the ownership written by older builds.
        with closing(sqlite3.connect(self.db_path)) as db:
            db.execute("UPDATE jobs SET volume_key=? WHERE path=?", (self.volume.key, nested.roots[0]))
            db.commit()
        self.configure_volumes(nested)
        self.assertTrue(self.index.work(self.normalizer))
        self.assertIn(new, self.stored_paths())

    def test_unsigned_inode_is_stored_losslessly_without_numeric_affinity_rounding(self):
        path = str(Path(self.root) / "empty")
        inode = 18446744073709551405
        try:
            self.complete_baseline([entry(path, ino=inode)])
        except OverflowError as error:
            self.fail("Unsigned filesystem inode overflowed SQLite binding: " + str(error))
        with closing(sqlite3.connect(self.db_path)) as db:
            value, storage = db.execute("SELECT ino, typeof(ino) FROM entries WHERE path=?", (path,)).fetchone()
        self.assertEqual(value, "u:" + str(inode))
        self.assertEqual(storage, "text")

    def test_unsigned_directory_inode_does_not_look_replaced_on_shallow_refresh(self):
        folder = str(Path(self.root) / "folder")
        child = str(Path(folder) / "child")
        inode = 18446744073709551405
        try:
            self.complete_baseline([entry(folder, "directory", inode), entry(child)], [self.root, folder])
        except OverflowError as error:
            self.fail("Unsigned filesystem inode overflowed SQLite binding: " + str(error))
        self.normalizer.results[self.root] = {"entries": [entry(folder, "directory", inode)]}
        self.index.enqueue(self.volume.key, [event(Path(self.root) / "trigger", 1)])
        self.assertTrue(self.index.work(self.normalizer))
        self.assertFalse(self.index.work(self.normalizer))
        self.assertEqual(self.normalizer.calls, [(self.root, False)])
        self.assertIn(child, self.stored_paths())

    def streaming_tree(self):
        child = str(Path(self.root) / "child")
        deep = str(Path(child) / "deep")
        leaf = str(Path(deep) / "leaf")
        self.normalizer.results[self.root] = {"entries": [entry(child, "directory")]}
        self.normalizer.results[child] = {"entries": [entry(deep, "directory")]}
        self.normalizer.results[deep] = {"entries": [entry(leaf)]}
        return child, deep, leaf

    def test_recursive_baseline_advances_one_directory_per_durable_worker_step(self):
        child, deep, leaf = self.streaming_tree()
        self.index.bootstrap_jobs()
        self.assertTrue(self.index.work(self.normalizer))
        self.assertEqual(self.normalizer.calls, [(self.root, False)])
        self.assertFalse(self.index.status()["baseline_complete"])
        self.assertEqual(self.stored_paths(), {child})
        self.assertEqual(self.index.status()["pending_jobs"], 1)
        self.assertTrue(self.index.work(self.normalizer))
        self.assertEqual(self.stored_paths(), {child, deep})
        self.assertTrue(self.index.work(self.normalizer))
        self.assertEqual(self.stored_paths(), {child, deep, leaf})
        self.assertTrue(self.index.status()["baseline_complete"])
        self.assertEqual(self.index.status()["baseline_walks"], 1)
        self.assertEqual(self.normalizer.calls, [(self.root, False), (child, False), (deep, False)])

    def test_interrupted_directory_traversal_resumes_children_without_restarting_root(self):
        child, deep, leaf = self.streaming_tree()
        self.index.bootstrap_jobs()
        self.index.work(self.normalizer)
        self.index.close()
        self.index = Index(self.db_path)
        self.index.bind_policy(self.normalizer.policy)
        self.assertTrue(self.index.configure("config-a", [self.volume], [self.root]))
        self.normalizer.calls.clear()
        self.index.bootstrap_jobs()
        while self.index.work(self.normalizer):
            pass
        self.assertEqual(self.normalizer.calls, [(child, False), (deep, False)])
        self.assertIn(leaf, self.stored_paths())
        self.assertEqual(self.index.status()["baseline_walks"], 1)
        self.assertTrue(self.index.status()["baseline_complete"])

    def test_explicit_reconciliation_visits_already_indexed_descendants_incrementally(self):
        child, deep, leaf = self.streaming_tree()
        self.index.bootstrap_jobs()
        while self.index.work(self.normalizer):
            pass
        changed = str(Path(deep) / "changed")
        self.normalizer.results[deep] = {"entries": [entry(changed)]}
        self.normalizer.calls.clear()
        self.index.request_reconcile([self.root])
        while self.index.work(self.normalizer):
            pass
        self.assertEqual(self.normalizer.calls, [(self.root, False), (child, False), (deep, False)])
        self.assertIn(changed, self.stored_paths())
        self.assertNotIn(leaf, self.stored_paths())

    def test_event_does_not_collapse_durable_traversal_frontier_back_to_root(self):
        a, b = str(Path(self.root) / "a"), str(Path(self.root) / "b")
        self.normalizer.results[self.root] = {"entries": [entry(a, "directory"), entry(b, "directory")]}
        self.index.bootstrap_jobs()
        self.index.work(self.normalizer)
        with patch("jaso_nfc.index._MAX_JOBS", 1):
            self.index.enqueue(self.volume.key, [event(Path(self.root) / "changed", 10)])
        with closing(sqlite3.connect(self.db_path)) as db:
            jobs = {path: recursive for path, recursive in db.execute("SELECT path, recursive FROM jobs")}
        self.assertEqual(jobs, {self.root: 0, a: 1, b: 1})

    def test_next_wakeup_reports_only_real_work_deadlines(self):
        self.complete_baseline()
        self.assertTrue(hasattr(self.index, "next_wakeup"), "Missing next_wakeup")
        self.assertIsNone(self.index.next_wakeup())
        self.index.enqueue(self.volume.key, [event(Path(self.root) / "a", 1)])
        self.assertEqual(self.index.next_wakeup(), 0)
        self.normalizer.results[self.root] = PermissionError("later")
        with patch("jaso_nfc.index.time.time", return_value=1000):
            with self.assertLogs("jaso_nfc.index", "WARNING"):
                self.index.work(self.normalizer)
        self.assertEqual(self.index.next_wakeup(), 1002)

    def test_real_nested_unicode_baseline_uses_only_shallow_directory_scans(self):
        from jaso_nfc.normalizer import Normalizer, Policy as RealPolicy
        outer = Path(self.root) / unicodedata.normalize("NFD", "폴더")
        inner = outer / unicodedata.normalize("NFD", "하위")
        inner.mkdir(parents=True)
        (inner / unicodedata.normalize("NFD", "파일.txt")).write_text("preserved")
        state = Path(self.tmp.name) / "normalizer-state"
        real = Normalizer(RealPolicy([self.root]), state / "renames.jsonl",
                          state / "retry.json", state / "pending.json", apply=True)
        self.addCleanup(real.close)
        calls = []
        reconcile = real.reconcile
        def shallow_only(path, recursive):
            self.assertFalse(recursive, "Worker requested a whole-subtree materialization")
            calls.append(path)
            return reconcile(path, recursive)
        real.reconcile = shallow_only
        self.index.bind_policy(real.policy)
        self.index.bootstrap_jobs()
        for _ in range(10):
            if not self.index.work(real):
                break
        else:
            self.fail("Directory traversal did not finish")
        actual = {str(Path(parent) / name) for parent, dirs, files in os.walk(self.root)
                  for name in dirs + files}
        self.assertEqual(self.stored_paths(), actual)
        self.assertEqual(len(calls), 3)
        self.assertTrue(all(unicodedata.is_normalized("NFC", path) for path in actual))
        self.assertTrue(self.index.status()["baseline_complete"])
        self.assertEqual(self.index.status()["baseline_walks"], 1)

    def test_due_normalization_retry_reconciles_only_parent(self):
        self.complete_baseline()
        folder = str(Path(self.root) / "sub")
        self.normalizer.retries = [folder]
        self.assertTrue(self.index.work(self.normalizer))
        self.assertEqual(self.normalizer.calls, [(folder, False)])
        self.assertEqual(self.index.status()["baseline_walks"], 1)

    def test_status_read_only_preserves_database(self):
        self.complete_baseline()
        before = Path(self.db_path).stat().st_mtime_ns
        reader = Index(self.db_path, read_only=True)
        try:
            self.assertTrue(reader.status()["baseline_complete"])
            with self.assertRaises(RuntimeError):
                reader.bootstrap_jobs()
        finally:
            reader.close()
        self.assertEqual(Path(self.db_path).stat().st_mtime_ns, before)

    def test_explicit_reconcile_keeps_cursor(self):
        self.complete_baseline()
        self.index.enqueue("volume-a", [event(Path(self.root) / "a", 9)])
        self.index.request_reconcile()
        self.index.work(self.normalizer)
        self.assertEqual(self.index.cursor("volume-a"), 9)
        self.assertIn((self.root, False), self.normalizer.calls)

    def test_seed_cursor_preserves_newer_callback_checkpoint(self):
        self.assertTrue(hasattr(self.index, "seed_cursor"), "Missing seed_cursor")
        self.index.seed_cursor("volume-a", 41)
        self.assertEqual(self.index.cursor("volume-a"), 41)
        self.index.enqueue("volume-a", [event(Path(self.root) / "a", 45)])
        self.index.seed_cursor("volume-a", 42)
        self.assertEqual(self.index.cursor("volume-a"), 45)

    def test_cursor_wrap_starts_new_epoch_and_reconciles(self):
        self.complete_baseline()
        self.index.enqueue("volume-a", [event("", 100, 0x10)])
        self.index.enqueue("volume-a", [event("", 2, 0x8)])
        self.assertEqual(self.index.cursor("volume-a"), 2)
        self.index.work(self.normalizer)
        self.assertEqual(self.normalizer.calls, [(self.root, False)])

    def test_invalid_volume_cursor_resets_and_schedules_recovery(self):
        self.complete_baseline()
        self.index.enqueue("volume-a", [event("", 100, 0x10)])
        self.assertTrue(hasattr(self.index, "invalidate_volume"), "Missing invalidate_volume")
        self.index.invalidate_volume("volume-a")
        self.assertIsNone(self.index.cursor("volume-a"))
        self.assertEqual(self.index.status()["pending_jobs"], 1)
        self.index.work(self.normalizer)
        self.assertEqual(self.normalizer.calls, [(self.root, False)])

    def test_failed_subtree_keeps_existing_entries_when_omitted_from_result(self):
        blocked = str(Path(self.root) / "blocked")
        old = str(Path(blocked) / "old")
        self.complete_baseline([entry(blocked, "directory"), entry(old)], [self.root, blocked])
        self.normalizer.results[self.root] = {
            "entries": [], "directories": [self.root],
            "errors": [{"path": blocked, "error": "PermissionError"}]}
        self.index.request_reconcile()
        self.index.work(self.normalizer)
        self.assertEqual(self.stored_paths(), {blocked, old})

    def test_reconfigure_after_root_change_requires_baseline(self):
        self.complete_baseline()
        self.index.enqueue("volume-a", [event(self.root, 1, 0x20)])
        self.assertTrue(self.index.configure("config-a", [self.volume], [self.root]))
        self.assertFalse(self.index.status()["needs_revalidation"])
        self.assertEqual(self.index.cursor("volume-a"), 1)

    def test_revalidated_identity_preserves_observations_and_control_cursor_on_restart(self):
        old = str(Path(self.root) / "old")
        self.complete_baseline([entry(old)])
        self.index.enqueue("volume-a", [event(self.root, 100, 0x40), event("", 200, 0x10)])
        self.assertTrue(self.index.configure("config-a", [self.volume], [self.root]))
        self.assertEqual(self.index.cursor("volume-a"), 200)
        self.assertEqual(self.stored_paths(), {old})
        self.assertEqual(self.index.status()["pending_baseline_roots"], [self.root])
        self.index.close()
        self.index = Index(self.db_path)
        self.index.bind_policy(self.normalizer.policy)
        self.assertTrue(self.index.configure("config-a", [self.volume], [self.root]))
        self.assertEqual(self.index.cursor("volume-a"), 200)
        self.index.bootstrap_jobs()
        self.assertTrue(self.index.work(self.normalizer))
        self.assertFalse(self.index.work(self.normalizer))

    def test_same_volume_uuid_with_changed_device_number_preserves_index(self):
        self.complete_baseline([entry(Path(self.root) / "a")])
        self.index.enqueue("volume-a", [event("", 41, 0x10)])
        self.volume.device = 8
        self.assertFalse(self.index.configure("config-a", [self.volume], [self.root]))
        self.assertEqual(self.index.cursor("volume-a"), 41)
        self.assertEqual(self.index.status()["indexed_entries"], 1)

    def test_replaced_directory_identity_reindexes_its_subtree(self):
        folder = str(Path(self.root) / "folder")
        old = str(Path(folder) / "old")
        new = str(Path(folder) / "new")
        self.complete_baseline([entry(folder, "directory", 10), entry(old)], [self.root, folder])
        self.normalizer.results[self.root] = {"entries": [entry(folder, "directory", 20)]}
        self.normalizer.results[folder] = {"entries": [entry(new)], "directories": [folder]}
        self.index.enqueue("volume-a", [event(Path(self.root) / "a", 1)])
        while self.index.work(self.normalizer):
            pass
        self.assertIn((folder, False), self.normalizer.calls)
        self.assertNotIn(old, self.stored_paths())
        self.assertIn(new, self.stored_paths())

    def test_many_distinct_parents_collapse_into_bounded_recovery(self):
        self.complete_baseline()
        with patch("jaso_nfc.index._MAX_JOBS", 3):
            self.index.enqueue("volume-a", [event(Path(self.root) / str(i) / "file", i)
                                            for i in range(10)])
        self.assertLessEqual(self.index.status()["pending_jobs"], 3)
        self.index.work(self.normalizer)
        self.assertEqual(self.normalizer.calls, [(self.root, False)])

    def test_failed_baseline_subtree_retry_is_not_counted_as_another_root_walk(self):
        blocked = str(Path(self.root) / "blocked")
        self.normalizer.results[self.root] = {
            "entries": [entry(blocked, "directory")], "directories": [self.root],
            "errors": [{"path": blocked, "error": "PermissionError"}]}
        self.index.bootstrap_jobs()
        with self.assertLogs("jaso_nfc.index", "WARNING"):
            self.index.work(self.normalizer)
        self.normalizer.results[blocked] = {"directories": [blocked]}
        with patch("jaso_nfc.index.time.time", return_value=10**12):
            self.assertTrue(self.index.work(self.normalizer))
        self.assertTrue(self.index.status()["baseline_complete"])
        self.assertEqual(self.index.status()["baseline_walks"], 1)
        self.assertEqual(self.index.status()["subtree_scans"], 1)

    def test_invalid_event_rolls_back_jobs_and_cursor_together(self):
        self.complete_baseline()
        self.index.seed_cursor("volume-a", 5)
        with self.assertRaises(ValueError):
            self.index.enqueue("volume-a", [event(Path(self.root) / "a", 10),
                                            event(Path(self.root) / "b", -1)])
        self.assertEqual(self.index.cursor("volume-a"), 5)
        self.assertEqual(self.index.status()["pending_jobs"], 0)

    def test_real_normalizer_directory_event_indexes_final_child_spellings(self):
        from jaso_nfc.normalizer import Normalizer, Policy as RealPolicy
        state = Path(self.tmp.name) / "normalizer-state"
        real = Normalizer(RealPolicy([self.root]), state / "renames.jsonl",
                          state / "retry.json", state / "pending.json", apply=True)
        self.addCleanup(real.close)
        self.index.bind_policy(real.policy)
        self.index.bootstrap_jobs()
        self.index.work(real)
        folder = Path(self.root) / unicodedata.normalize("NFD", "폴더")
        folder.mkdir()
        (folder / unicodedata.normalize("NFD", "파일.txt")).write_text("preserved")
        self.index.enqueue("volume-a", [event(folder, 1, CREATED | IS_DIR)])
        for _ in range(10):
            if not self.index.work(real):
                break
        else:
            self.fail("Incremental work did not become idle")
        actual = {str(Path(parent) / name) for parent, dirs, files in os.walk(self.root)
                  for name in dirs + files}
        self.assertEqual(self.stored_paths(), actual)
        self.assertEqual(len(actual), 2)
        self.assertTrue(all(unicodedata.is_normalized("NFC", path) for path in actual))
        self.assertEqual(self.index.status()["baseline_walks"], 1)

    def test_actual_scope_error_retries_the_failed_canonical_subtree(self):
        self.complete_baseline()
        actual = str(Path(self.root) / "폴더")
        old = unicodedata.normalize("NFD", actual)
        blocked = str(Path(actual) / "blocked")
        self.normalizer.results[old] = {"scope": actual, "entries": [],
                                        "directories": [actual],
                                        "errors": [{"path": blocked, "error": "PermissionError"}]}
        self.index.request_reconcile([old])
        with self.assertLogs("jaso_nfc.index", "WARNING"):
            self.index.work(self.normalizer)
        with closing(sqlite3.connect(self.db_path)) as db:
            jobs = db.execute("SELECT path FROM jobs").fetchall()
        self.assertEqual(jobs, [(blocked,)])

    def test_missing_nfd_subtree_event_prunes_the_former_nfc_entries(self):
        import shutil
        from jaso_nfc.normalizer import Normalizer, Policy as RealPolicy
        state = Path(self.tmp.name) / "normalizer-state"
        real = Normalizer(RealPolicy([self.root]), state / "renames.jsonl",
                          state / "retry.json", state / "pending.json", apply=True)
        self.addCleanup(real.close)
        parent = Path(self.root) / "부모"
        removed = parent / "폴더"
        removed.mkdir(parents=True)
        (removed / "file").write_text("deleted subtree")
        self.index.bind_policy(real.policy)
        self.index.bootstrap_jobs()
        while self.index.work(real):
            pass
        self.assertIn(str(removed / "file"), self.stored_paths())
        shutil.rmtree(removed)
        old_event_path = unicodedata.normalize("NFD", str(removed))
        self.index.enqueue("volume-a", [event(old_event_path, 1, 0x1 | IS_DIR)])
        calls = []
        reconcile = real.reconcile

        def observe(path, recursive):
            calls.append((path, recursive))
            return reconcile(path, recursive)

        with patch.object(real, "reconcile", side_effect=observe):
            for _ in range(5):
                if not self.index.work(real):
                    break
            else:
                self.fail("Missing subtree follow-up failed to become idle")
        self.assertEqual(self.stored_paths(), {str(parent)})
        self.assertEqual(calls, [(old_event_path, False),
                                 (os.path.dirname(old_event_path), False)])
        self.assertEqual(self.index.status()["baseline_walks"], 1)

    def test_pending_recovery_stops_worker_and_retains_job_and_index(self):
        from jaso_nfc.normalizer import PendingRecoveryError
        old = str(Path(self.root) / "old")
        self.complete_baseline([entry(old)])
        self.normalizer.results[self.root] = PendingRecoveryError("identity missing")
        self.index.enqueue("volume-a", [event(old, 1)])
        with self.assertRaises(PendingRecoveryError):
            self.index.work(self.normalizer)
        self.assertEqual(self.stored_paths(), {old})
        self.assertEqual(self.index.status()["pending_jobs"], 1)
        self.assertEqual(self.index.status()["shallow_scans"], 0)

    def test_unexpected_reconciler_failure_propagates_without_acknowledgement(self):
        self.complete_baseline()
        self.normalizer.results[self.root] = ValueError("invalid reconciler result")
        self.index.enqueue("volume-a", [event(Path(self.root) / "a", 1)])
        with self.assertRaises(ValueError):
            self.index.work(self.normalizer)
        self.assertEqual(self.index.status()["pending_jobs"], 1)

    def test_missing_configured_root_cannot_complete_baseline(self):
        self.index.bootstrap_jobs()
        Path(self.root).rmdir()
        with self.assertRaisesRegex(RuntimeError, "revalidat"):
            self.index.work(self.normalizer)
        self.assertFalse(self.index.status()["baseline_complete"])
        self.assertTrue(self.index.status()["needs_revalidation"])
        self.assertEqual(self.index.status()["pending_jobs"], 1)
        self.assertEqual(self.normalizer.calls, [])

    def test_missing_configured_root_preserves_existing_index(self):
        old = str(Path(self.root) / "old")
        self.complete_baseline([entry(old)])
        self.index.request_reconcile()
        Path(self.root).rmdir()
        with self.assertRaisesRegex(RuntimeError, "revalidat"):
            self.index.work(self.normalizer)
        self.assertEqual(self.stored_paths(), {old})
        self.assertEqual(self.index.status()["pending_jobs"], 1)

    def test_events_during_baseline_become_scoped_followup_instead_of_repeating_root(self):
        self.index.bootstrap_jobs()
        def arrival_during_baseline():
            self.index.enqueue("volume-a", [event(Path(self.root) / "changed", 10)])
        self.normalizer.during_scan = arrival_during_baseline
        self.index.work(self.normalizer)
        self.assertTrue(self.index.status()["baseline_complete"])
        self.assertEqual(self.index.status()["pending_jobs"], 1)
        self.index.work(self.normalizer)
        self.assertEqual(self.normalizer.calls, [(self.root, False), (self.root, False)])
        self.assertFalse(self.index.work(self.normalizer))
        self.assertEqual(self.index.status()["baseline_walks"], 1)

    def test_crash_during_baseline_retains_baseline_and_arriving_events(self):
        self.index.bootstrap_jobs()
        def arrival_then_crash():
            self.index.enqueue("volume-a", [event(Path(self.root) / "changed", 10)])
            raise KeyboardInterrupt("simulated process interruption")
        self.normalizer.during_scan = arrival_then_crash
        with self.assertRaises(KeyboardInterrupt):
            self.index.work(self.normalizer)
        self.index.close()
        self.index = Index(self.db_path)
        self.index.bind_policy(self.normalizer.policy)
        self.assertTrue(self.index.configure("config-a", [self.volume], [self.root]))
        self.assertEqual(self.index.cursor("volume-a"), 10)
        self.index.bootstrap_jobs()
        self.assertTrue(self.index.work(self.normalizer))
        self.assertTrue(self.index.status()["baseline_complete"])
        self.assertFalse(self.index.work(self.normalizer))

    def test_events_during_subtree_scan_do_not_repeat_the_whole_subtree(self):
        self.complete_baseline()
        folder = str(Path(self.root) / "folder")
        self.index.request_reconcile([folder])
        self.normalizer.during_scan = lambda: self.index.enqueue(
            "volume-a", [event(Path(folder) / "child" / "changed", 10)])
        self.index.work(self.normalizer)
        self.index.work(self.normalizer)
        self.assertEqual(self.normalizer.calls, [(folder, False), (str(Path(folder) / "child"), False)])
        self.assertFalse(self.index.work(self.normalizer))

    def test_configured_root_replaced_by_file_requires_revalidation(self):
        self.index.bootstrap_jobs()
        Path(self.root).rmdir()
        Path(self.root).write_text("replacement")
        with self.assertRaisesRegex(RuntimeError, "revalidat"):
            self.index.work(self.normalizer)
        self.assertTrue(self.index.status()["needs_revalidation"])
        self.assertFalse(self.index.status()["baseline_complete"])

    def test_failed_subdirectory_retries_without_rewalking_root(self):
        blocked = str(Path(self.root) / "blocked")
        self.normalizer.results[self.root] = {
            "entries": [entry(Path(self.root) / "good")], "directories": [self.root],
            "errors": [{"path": blocked, "error": "PermissionError"}]}
        self.index.bootstrap_jobs()
        self.index.work(self.normalizer)
        self.assertEqual(self.index.status()["pending_jobs"], 1)
        self.assertFalse(self.index.work(self.normalizer))
        with closing(sqlite3.connect(self.db_path)) as db:
            jobs = db.execute("SELECT path FROM jobs").fetchall()
        self.assertEqual(jobs, [(blocked,)])
        self.assertIn(str(Path(self.root) / "good"), self.stored_paths())


if __name__ == "__main__":
    unittest.main()
