"""Source lifecycle tests isolate native delivery and retain real state files."""
import errno
import json
from pathlib import Path
import sqlite3
import tempfile
import threading
import types
import unittest
from unittest import mock

from jaso_nfc.coverage import Coverage
from jaso_nfc.events import (CursorInvalidError, Event, Volume, ITEM_CREATED,
                             ROOT_CHANGED, USER_DROPPED, MOUNT, HISTORY_DONE)
from jaso_nfc.normalizer import Policy
from jaso_nfc.index import Index
from jaso_nfc.sources import SourceManager, policy_for, resolve_coverage


class FakeIndex:
    def __init__(self, timeline):
        self.timeline = timeline
        self.volumes = {}
        self.cursors = {}
        self.entries = {}
        self.batches = []
        self.configurations = []
        self.fail_enqueue = None
        self.reconciliations = []

    def bind_policy(self, policy):
        self.policy = policy

    def configure(self, signature, volumes, roots):
        self.timeline.append(("configure", tuple(roots)))
        self.configurations.append((signature, tuple(v.key for v in volumes), tuple(roots)))
        changed = bool(set(roots) - {r for v in self.volumes.values() for r in v.roots})
        self.volumes = {v.key: v for v in volumes}
        for volume in volumes:
            self.cursors.setdefault(volume.key, None)
        return changed

    def cursor(self, key):
        if key not in self.volumes:
            raise KeyError(key)
        return self.cursors[key]

    def seed_cursor(self, key, event_id):
        if self.cursors[key] is None:
            self.cursors[key] = event_id

    def invalidate_volume(self, key):
        self.cursors[key] = None

    def enqueue(self, key, events):
        if self.fail_enqueue:
            raise self.fail_enqueue
        if key not in self.volumes:
            raise AssertionError("callback before index configuration")
        self.batches.append((key, events))
        self.cursors[key] = max(event.id for event in events)

    def bootstrap_jobs(self):
        self.timeline.append(("bootstrap", tuple(self.volumes)))

    def request_reconcile(self, paths):
        self.timeline.append(("reconcile", tuple(paths)))
        self.reconciliations.append(tuple(paths))


class SourceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="jaso-sources-")
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.a, self.b = str(self.base / "home-a"), str(self.base / "home-b")
        self.catalog = str(self.base / "Users")
        self.timeline = []
        self.streams = []
        self.start_failures = set()
        self.discovery_failures = set()
        self.coverage = Coverage((self.a,), (), (self.catalog,), ())
        self.config = types.SimpleNamespace(
            scope="all-user-files", roots=[], excludes=[], exclude_names=[".git"],
            skip_hidden_tops=[self.a], state_dir=str(self.base / "runtime"),
            log_dir=str(self.base / "runtime" / "logs"), signature=lambda: "static-config",
            state_path=lambda name: self.base / "runtime" / "state" / name)
        self.index = FakeIndex(self.timeline)
        self.normalizer = types.SimpleNamespace(policy=Policy([]))
        self.wake = threading.Event()
        test = self

        class FakeStream:
            def __init__(self, volume, since, callback):
                self.volume, self.since, self.callback = volume, since, callback
                self.start_id = since if since is not None else 100
                self.error = None
                self.stopped = False
                test.streams.append(self)

            def start(self):
                root = self.volume.roots[0]
                test.timeline.append(("start", root))
                if root in test.start_failures:
                    raise OSError(errno.EACCES, "fixture native start denied", root)

            def stop(self):
                test.timeline.append(("stop", self.volume.roots[0]))
                self.stopped = True

        self.Stream = FakeStream

    def discover(self, roots):
        self.assertEqual(len(roots), 1)
        root = roots[0]
        if root in self.discovery_failures:
            raise OSError(errno.EACCES, "fixture volume access denied", root)
        return [Volume("volume:" + root, 1, "uuid", "/", (root,))]

    def manager(self):
        manager = SourceManager(self.config, self.index, self.normalizer, self.wake,
                                Stream=self.Stream, discover_volumes=self.discover,
                                discover_coverage=lambda: self.coverage)
        self.addCleanup(manager.close)
        return manager

    def active_stream(self, root):
        return next(stream for stream in reversed(self.streams)
                    if stream.volume.roots == (root,) and not stream.stopped)

    def test_catalog_and_data_start_before_bootstrap_and_callbacks_have_known_keys(self):
        manager = self.manager()
        self.assertTrue(manager.refresh())
        catalog_position = self.timeline.index(("start", self.catalog))
        data_position = self.timeline.index(("start", self.a))
        bootstrap_position = next(i for i, event in enumerate(self.timeline) if event[0] == "bootstrap")
        self.assertLess(catalog_position, data_position)
        self.assertLess(data_position, bootstrap_position)
        self.active_stream(self.a).callback([Event(self.a + "/file", ITEM_CREATED, 101)])
        self.assertEqual(len(self.index.batches), 1)
        self.assertTrue(self.wake.is_set())
        self.assertEqual(manager.roots, (self.a,))
        self.assertEqual(manager.active_roots, (self.a,))

    def test_per_root_discovery_failure_does_not_block_another_root(self):
        self.coverage = Coverage((self.a, self.b), (), (), ())
        self.discovery_failures.add(self.b)
        manager = self.manager()
        manager.refresh()
        self.assertEqual(manager.active_roots, (self.a,))
        self.assertIn(self.b, manager.unavailable)
        state = json.loads(self.config.state_path("coverage.json").read_text())
        self.assertEqual(state["roots"], [self.a, self.b])
        self.assertEqual(state["active_roots"], [self.a])
        self.assertIn(self.b, state["unavailable"])
        self.assertIsNotNone(manager.next_retry_time)

    def test_root_created_before_first_catalog_start_is_discovered(self):
        original_start = self.Stream.start

        def add_home_before_watch(stream):
            if stream.volume.roots == (self.catalog,):
                self.coverage = Coverage((self.a, self.b), (), (self.catalog,), ())
            original_start(stream)

        with mock.patch.object(self.Stream, "start", add_home_before_watch):
            manager = self.manager()
            manager.refresh()
        self.assertEqual(manager.active_roots, (self.a, self.b))

    def test_new_catalog_is_rechecked_after_it_starts(self):
        cloud = self.b + "/Library/CloudStorage"
        library = self.b + "/Library"
        original_start = self.Stream.start

        def grow_catalogs(stream):
            if stream.volume.roots == (self.catalog,):
                self.coverage = Coverage((self.a, self.b), (), (self.catalog, library), ())
            elif stream.volume.roots == (library,):
                self.coverage = Coverage((self.a, self.b, cloud), (), (self.catalog, library), ())
            original_start(stream)

        with mock.patch.object(self.Stream, "start", grow_catalogs):
            manager = self.manager()
            manager.refresh()
        self.assertEqual(manager.active_roots, (self.a, self.b, cloud))

    def test_start_failure_keeps_other_root_and_retries_after_deadline(self):
        self.coverage = Coverage((self.a, self.b), (), (), ())
        self.start_failures.add(self.b)
        manager = self.manager()
        with mock.patch("jaso_nfc.sources.time.time", return_value=1000):
            manager.refresh()
            number = len(self.streams)
            self.assertFalse(manager.refresh())
            self.assertEqual(len(self.streams), number)
        self.assertEqual(manager.active_roots, (self.a,))
        self.assertEqual(self.index.configurations[-1][2], (self.a,))
        self.start_failures.clear()
        with mock.patch("jaso_nfc.sources.time.time", return_value=1031):
            self.assertTrue(manager.check())
            self.assertTrue(manager.refresh_requested.is_set())
            self.assertTrue(manager.refresh())
        self.assertEqual(manager.active_roots, (self.a, self.b))
        self.assertEqual(manager.unavailable, {})
        self.assertIsNone(manager.next_retry_time)

    def test_same_metadata_refresh_never_restarts_streams_or_bootstraps_again(self):
        manager = self.manager()
        manager.refresh()
        events = list(self.timeline)
        state_time = self.config.state_path("coverage.json").stat().st_mtime_ns
        self.assertFalse(manager.refresh())
        self.assertEqual(self.timeline, events)
        self.assertEqual(self.config.state_path("coverage.json").stat().st_mtime_ns, state_time)

    def test_hotplug_keeps_static_signature_and_existing_index_evidence(self):
        manager = self.manager()
        manager.refresh()
        self.index.entries[self.a + "/existing"] = "preserved"
        previous = self.active_stream(self.a)
        self.coverage = Coverage((self.a, self.b), (), (self.catalog,), ())
        manager.refresh()
        self.assertTrue(previous.stopped)
        self.assertEqual(self.index.entries[self.a + "/existing"], "preserved")
        self.assertEqual({c[0] for c in self.index.configurations}, {"static-config"})
        count = len(self.index.batches)
        previous.callback([Event(self.a + "/stale", ITEM_CREATED, 102)])
        self.assertEqual(len(self.index.batches), count)

    def test_catalog_ignores_nested_traffic_but_accepts_direct_children_and_loss(self):
        manager = self.manager()
        manager.refresh()
        stream = self.active_stream(self.catalog)
        stream.callback([Event(self.catalog + "/person/docs/file", ITEM_CREATED, 1)])
        self.assertFalse(manager.refresh_requested.is_set())
        stream.callback([Event(self.catalog + "/person", ITEM_CREATED, 2)])
        self.assertTrue(manager.refresh_requested.is_set())
        manager.refresh_requested.clear()
        stream.callback([Event("", USER_DROPPED, 3)])
        self.assertTrue(manager.refresh_requested.is_set())
        self.assertEqual(self.index.batches, [])

    def test_data_root_control_requests_refresh_and_still_enqueues_batch(self):
        manager = self.manager()
        manager.refresh()
        self.active_stream(self.a).callback([Event(self.a, ROOT_CHANGED, 200)])
        self.assertTrue(manager.refresh_requested.is_set())
        self.assertEqual(len(self.index.batches), 1)

    def test_catalog_failure_has_its_own_status_and_metadata_retry(self):
        self.start_failures.add(self.catalog)
        manager = self.manager()
        manager.refresh()
        self.assertEqual(manager.unavailable, {})
        self.assertIn(self.catalog, manager.catalog_unavailable)
        self.assertEqual(manager.active_roots, (self.a,))
        self.assertIsNotNone(manager.next_retry_time)

    def test_all_roots_unavailable_configures_empty_index(self):
        self.discovery_failures.add(self.a)
        manager = self.manager()
        manager.refresh()
        self.assertEqual(manager.active_roots, ())
        self.assertEqual(self.index.configurations[-1][1:], ((), ()))

    def test_stream_callback_database_failure_is_fatal(self):
        manager = self.manager()
        manager.refresh()
        failure = sqlite3.OperationalError("fixture disk full")
        self.active_stream(self.a).error = failure
        with self.assertRaises(sqlite3.OperationalError):
            manager.check()
        self.index.fail_enqueue = failure
        with self.assertRaises(sqlite3.OperationalError):
            self.active_stream(self.a).callback([Event(self.a + "/file", ITEM_CREATED, 1)])

    def test_configured_scope_has_no_catalog_and_retains_hidden_top_policy(self):
        self.config.scope = "configured"
        self.config.roots = [self.a]
        coverage = resolve_coverage(self.config, discover_coverage=lambda: self.fail("unexpected global discovery"))
        self.assertEqual(coverage.catalog_roots, ())
        self.assertFalse(policy_for(self.config, coverage).accepts(self.a + "/.hidden/file"))
        manager = self.manager()
        manager.refresh()
        self.assertEqual(len(self.streams), 1)

    def test_all_user_policy_uses_root_exclusions_and_includes_hidden_user_data(self):
        cloud = self.a + "/Library/CloudStorage"
        coverage = Coverage((self.a, cloud), (), (), (), {self.a: (self.a + "/Library",)})
        policy = policy_for(self.config, coverage)
        self.assertTrue(policy.accepts(self.a + "/.notes/file"))
        self.assertFalse(policy.accepts(self.a + "/Library/preferences"))
        self.assertTrue(policy.accepts(cloud + "/file"))

    def test_cursor_reset_does_not_reactivate_failed_stream_callback(self):
        self.config.scope, self.config.roots = "configured", [self.a]
        original_start = self.Stream.start
        count = 0

        def invalid_first(stream):
            nonlocal count
            count += 1
            if count == 1:
                raise CursorInvalidError("fixture stale cursor")
            original_start(stream)

        with mock.patch.object(self.Stream, "start", invalid_first):
            manager = self.manager()
            manager.refresh()
        self.assertEqual(len(self.streams), 2)
        self.streams[0].callback([Event(self.a + "/stale", ITEM_CREATED, 500)])
        self.assertEqual(self.index.batches, [])
        self.streams[1].callback([Event(self.a + "/new", ITEM_CREATED, 501)])
        self.assertEqual(len(self.index.batches), 1)

    def test_callback_during_start_keeps_its_durable_cursor(self):
        self.config.scope, self.config.roots = "configured", [self.a]
        original_start = self.Stream.start

        def synchronous_batch(stream):
            original_start(stream)
            stream.callback([Event(self.a + "/new", ITEM_CREATED, 500)])

        with mock.patch.object(self.Stream, "start", synchronous_batch):
            self.manager().refresh()
        self.assertEqual(self.index.cursor("volume:" + self.a), 500)

    def test_drained_callback_failure_prevents_reconfiguration(self):
        manager = self.manager()
        manager.refresh()
        count = len(self.index.configurations)
        old = self.active_stream(self.a)
        failure = sqlite3.OperationalError("fixture disk full during drain")
        self.index.fail_enqueue = failure
        original_stop = self.Stream.stop

        def swallowed_callback_failure(stream):
            if stream is old:
                try:
                    stream.callback([Event(self.a + "/last", ITEM_CREATED, 500)])
                except sqlite3.OperationalError as error:
                    stream.error = error
            original_stop(stream)

        self.coverage = Coverage((self.a, self.b), (), (self.catalog,), ())
        with mock.patch.object(self.Stream, "stop", swallowed_callback_failure):
            with self.assertRaises(sqlite3.OperationalError):
                manager.refresh()
        self.assertEqual(len(self.index.configurations), count)

    def test_changed_root_exclusions_reconcile_only_that_existing_root(self):
        self.coverage = Coverage((self.a, self.b), (), (), ())
        manager = self.manager()
        manager.refresh()
        self.coverage = Coverage((self.a, self.b), (), (), (), {self.b: (self.b + "/System",)})
        manager.refresh()
        self.assertEqual(self.index.reconciliations, [(self.b,)])
        reconcile_position = next(i for i, item in enumerate(self.timeline) if item[0] == "reconcile")
        last_start = max(i for i, item in enumerate(self.timeline) if item[0] == "start")
        self.assertGreater(reconcile_position, last_start)

    def test_real_index_hotplug_does_not_repeat_existing_root_baseline(self):
        Path(self.a).mkdir()
        Path(self.b).mkdir()
        self.index = Index(self.base / "index.sqlite3")
        self.addCleanup(self.index.close)
        calls = []

        def reconcile(path, recursive):
            calls.append(path)
            return dict(entries=[dict(path=path + "/file", kind="file", dev=1, ino=1,
                                      mtime_ns=1, ctime_ns=1, size=1, mode=0o600)],
                        directories=[path], errors=[], renamed=0)

        self.normalizer.reconcile = reconcile
        self.normalizer.retry_paths = lambda: []
        manager = self.manager()
        manager.refresh()
        while self.index.work(self.normalizer):
            pass
        self.assertEqual(calls, [self.a])
        self.coverage = Coverage((self.a, self.b), (), (), ())
        manager.refresh()
        while self.index.work(self.normalizer):
            pass
        self.assertEqual(calls, [self.a, self.b])
        self.assertEqual(self.index.status()["baseline_walks"], 2)
        stored = {row[0] for row in self.index._db.execute("SELECT path FROM entries")}
        self.assertEqual(stored, {self.a + "/file", self.b + "/file"})

    def test_real_index_failed_source_retry_does_not_walk_healthy_root_again(self):
        Path(self.a).mkdir()
        Path(self.b).mkdir()
        self.index = Index(self.base / "index.sqlite3")
        self.addCleanup(self.index.close)
        calls = []
        self.normalizer.reconcile = lambda path, recursive: (
            calls.append(path) or dict(entries=[], directories=[path], errors=[], renamed=0))
        self.normalizer.retry_paths = lambda: []
        self.coverage = Coverage((self.a, self.b), (), (), ())
        self.start_failures.add(self.b)
        manager = self.manager()
        with mock.patch("jaso_nfc.sources.time.time", return_value=1000):
            manager.refresh()
            while self.index.work(self.normalizer):
                pass
        self.assertEqual(calls, [self.a])
        with mock.patch("jaso_nfc.sources.time.time", return_value=1031):
            manager.refresh()
            self.assertFalse(self.index.work(self.normalizer))
        self.assertEqual(calls, [self.a])
        self.assertEqual(self.index.status()["pending_baseline_roots"], [])

    def test_root_control_revalidates_even_when_volume_descriptor_is_unchanged(self):
        Path(self.a).mkdir()
        self.index = Index(self.base / "index.sqlite3")
        self.addCleanup(self.index.close)
        self.normalizer.reconcile = lambda path, recursive: dict(
            entries=[], directories=[path], errors=[], renamed=0)
        self.normalizer.retry_paths = lambda: []
        manager = self.manager()
        manager.refresh()
        while self.index.work(self.normalizer):
            pass
        previous = self.active_stream(self.a)
        previous.callback([Event(self.a, ROOT_CHANGED, 500)])
        self.assertTrue(manager.refresh())
        self.assertTrue(previous.stopped)
        while self.index.work(self.normalizer):
            pass
        self.assertEqual(self.index.status()["baseline_walks"], 2)

    def test_historical_mount_revalidation_resumes_after_its_committed_cursor(self):
        Path(self.a).mkdir()
        child = str(Path(self.a) / "child")
        Path(child).mkdir()
        self.config.scope, self.config.roots = "configured", [self.a]
        self.index = Index(self.base / "index.sqlite3")
        self.addCleanup(self.index.close)
        calls = []

        def reconcile(path, recursive):
            calls.append((path, recursive))
            entries = ([dict(path=child, kind="dir", dev=1, ino=1)]
                       if path == self.a else [])
            return dict(entries=entries, directories=[path], errors=[], renamed=0)

        self.normalizer.reconcile = reconcile
        self.normalizer.retry_paths = lambda: []
        original_start = self.Stream.start

        def replay_mount_since_checkpoint(stream):
            original_start(stream)
            if stream.since is None or stream.since < 100:
                stream.callback([Event(self.a, MOUNT, 100), Event("", HISTORY_DONE, 200)])

        with mock.patch.object(self.Stream, "start", replay_mount_since_checkpoint):
            manager = self.manager()
            manager.refresh()
            self.assertTrue(manager.refresh_requested.is_set())
            manager.refresh()
            self.assertFalse(manager.refresh_requested.is_set())
            while self.index.work(self.normalizer):
                pass
        self.assertEqual([stream.since for stream in self.streams], [None, 200])
        self.assertEqual(calls, [(self.a, False), (child, False)])
        self.assertEqual(self.index.status()["baseline_walks"], 1)
        self.assertTrue(self.index.status()["baseline_complete"])

    def test_refresh_does_not_erase_a_concurrent_catalog_signal(self):
        manager = self.manager()
        manager.refresh()
        catalog = self.active_stream(self.catalog)
        clearing, release_clear, signaled = threading.Event(), threading.Event(), threading.Event()
        original_clear = manager.refresh_requested.clear
        errors = []

        def paused_clear():
            clearing.set()
            if not release_clear.wait(5):
                raise AssertionError("refresh did not resume")
            original_clear()

        def refresh():
            try:
                manager.refresh()
            except BaseException as error:
                errors.append(error)

        def signal():
            catalog.callback([Event(self.catalog, ROOT_CHANGED, 500)])
            signaled.set()

        with mock.patch.object(manager.refresh_requested, "clear", paused_clear):
            worker = threading.Thread(target=refresh)
            worker.start()
            self.assertTrue(clearing.wait(5))
            callback = threading.Thread(target=signal)
            callback.start()
            signaled.wait(0.05)
            release_clear.set()
            worker.join(5)
            callback.join(5)
        self.assertFalse(errors)
        self.assertTrue(signaled.is_set())
        self.assertTrue(manager.refresh_requested.is_set())

if __name__ == "__main__":
    unittest.main()
