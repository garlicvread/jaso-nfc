"""Refresh native watches from metadata catalogs without periodic file walks."""
from dataclasses import dataclass
import json
import os
from pathlib import Path
import threading
import time

from .coverage import Coverage, discover_user_coverage
from .events import (CursorInvalidError, Stream as _Stream,
                     discover_volumes as _discover_volumes, MUST_SCAN_SUBDIRS,
                     USER_DROPPED, KERNEL_DROPPED, EVENT_IDS_WRAPPED,
                     ROOT_CHANGED, MOUNT, UNMOUNT)
from .legacy import atomic_json, nfc
from .normalizer import PendingRecoveryError, Policy


_REFRESH_FLAGS = (MUST_SCAN_SUBDIRS | USER_DROPPED | KERNEL_DROPPED |
                  EVENT_IDS_WRAPPED | ROOT_CHANGED | MOUNT | UNMOUNT)


def resolve_coverage(config, discover_coverage=discover_user_coverage):
    """Resolve fixed configuration or metadata-only account/volume discovery."""
    if config.scope == "all-user-files":
        return discover_coverage()
    return Coverage(tuple(config.roots), (), (), ())


def policy_for(config, coverage, *, roots=None):
    """Construct identical exclusion semantics for one-shot and live processing."""
    roots = coverage.roots if roots is None else roots
    exclusions = tuple(dict.fromkeys([*config.excludes, config.state_dir, config.log_dir]))
    hidden = config.skip_hidden_tops if config.scope == "configured" else ()
    return Policy(roots, exclusions, config.exclude_names, hidden,
                  root_excludes=coverage.root_excludes)


def _descriptor(volume):
    return (volume.key, volume.uuid, volume.device, volume.mount, tuple(volume.roots))


@dataclass
class _Watch:
    volume: object
    stream: object = None
    active: bool = True


class SourceManager:
    """Manage data and catalog watches from the service's single worker thread.

    Call ``check`` each loop, then ``refresh`` when ``refresh_requested`` is set.
    ``next_retry_time`` is a Unix timestamp; deadlines trigger metadata discovery
    only. Native callbacks enqueue or signal, never mutate watch lifecycles.
    """

    RETRY_SECONDS = 30

    def __init__(self, config, index, normalizer, wake, *, Stream=_Stream,
                 discover_volumes=_discover_volumes,
                 discover_coverage=discover_user_coverage):
        self.config, self.index, self.normalizer, self.wake = config, index, normalizer, wake
        self._Stream = Stream
        self._discover_volumes = discover_volumes
        self._discover_coverage = discover_coverage
        self.refresh_requested = threading.Event()
        self.unavailable = {}
        self.catalog_unavailable = {}
        self.roots = ()
        self.active_roots = ()
        self.next_retry_time = None
        self._data = {}
        self._catalogs = {}
        self._catalog_desired = {}
        self._start_failures = {}
        self._last_signature = None
        self._last_status = None
        self._root_exclusions = {}
        self._callback_error = None
        self._refresh_lock = threading.Lock()
        self._force_refresh = False
        self._closed = False

    def _signal_refresh(self, reconfigure=False):
        with self._refresh_lock:
            if reconfigure:
                self._force_refresh = True
            self.refresh_requested.set()
        self.wake.set()

    def _receive_data(self, watch, batch):
        if not watch.active:
            return
        try:
            self.index.enqueue(watch.volume.key, batch)
        except BaseException as error:
            self._callback_error = error
            self.wake.set()
            raise
        if any(event.flags & _REFRESH_FLAGS for event in batch):
            self._signal_refresh(any(event.flags & (ROOT_CHANGED | MOUNT | UNMOUNT) for event in batch))
        self.wake.set()

    def _receive_catalog(self, watch, batch):
        if not watch.active:
            return
        roots = {nfc(os.path.abspath(root)) for root in watch.volume.roots}
        for event in batch:
            if event.flags & _REFRESH_FLAGS:
                self._signal_refresh(bool(event.flags & (ROOT_CHANGED | MOUNT | UNMOUNT)))
                return
            if event.path and nfc(os.path.dirname(os.path.abspath(event.path))) in roots:
                self._signal_refresh()
                return

    @staticmethod
    def _stop(watch):
        # Let callbacks already queued on the native serial queue commit before
        # invalidating this token or changing the index's configured keys.
        try:
            watch.stream.stop()
        finally:
            watch.active = False

    def _stop_data(self):
        failure = None
        for watch in reversed(list(self._data.values())):
            try:
                self._stop(watch)
            except BaseException as error:
                failure = failure or error
        self._data.clear()
        if failure:
            raise failure

    def _discover_one(self, root):
        volumes = self._discover_volumes([root])
        if len(volumes) != 1 or tuple(volumes[0].roots) != (root,):
            raise ValueError("native discovery must return exactly one stream for each root")
        return volumes[0]

    def _start_watch(self, volume, *, catalog=False):
        watch = _Watch(volume)
        callback = self._receive_catalog if catalog else self._receive_data
        since = None if catalog else self.index.cursor(volume.key)
        watch.stream = self._Stream(volume, since, lambda batch, observed=watch: callback(observed, batch))
        try:
            try:
                watch.stream.start()
            except CursorInvalidError:
                self._stop(watch)
                if catalog:
                    raise
                self.index.invalidate_volume(volume.key)
                watch = _Watch(volume)
                watch.stream = self._Stream(volume, None, lambda batch, observed=watch: callback(observed, batch))
                watch.stream.start()
            if self._callback_error:
                raise self._callback_error
            if not catalog and self.index.cursor(volume.key) is None:
                self.index.seed_cursor(volume.key, watch.stream.start_id)
            return watch
        except BaseException:
            self._stop(watch)
            raise

    def _sync_catalogs(self, roots, retry_due, force):
        desired, failures = {}, {}
        changed = False
        for root in roots:
            try:
                desired[root] = self._discover_one(root)
            except PendingRecoveryError:
                raise
            except OSError as error:
                failures[root] = str(error)
        for root, watch in list(self._catalogs.items()):
            if root not in desired or _descriptor(watch.volume) != _descriptor(desired[root]) or force:
                self._stop(watch)
                del self._catalogs[root]
                changed = True
        for root, volume in desired.items():
            if root in self._catalogs:
                continue
            if (not force and not retry_due and root in self.catalog_unavailable
                    and self._catalog_desired.get(root) == _descriptor(volume)):
                failures[root] = self.catalog_unavailable[root]
                continue
            try:
                self._catalogs[root] = self._start_watch(volume, catalog=True)
                changed = True
            except PendingRecoveryError:
                raise
            except (OSError, CursorInvalidError) as error:
                failures[root] = str(error)
        self._catalog_desired = {root: _descriptor(volume) for root, volume in desired.items()}
        self.catalog_unavailable = failures
        return changed

    def _status(self):
        return dict(roots=list(self.roots), active_roots=list(self.active_roots),
                    unavailable=dict(sorted(self.unavailable.items())),
                    catalog_unavailable=dict(sorted(self.catalog_unavailable.items())))

    def _save_status(self):
        status = self._status()
        if status == self._last_status:
            return
        path = Path(self.config.state_path("coverage.json"))
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        atomic_json(str(path), status)
        self._last_status = status

    def refresh(self, force=False):
        """Refresh metadata and watches; return whether active sources changed."""
        if self._closed:
            raise RuntimeError("source manager is closed")
        if self._callback_error:
            raise self._callback_error
        with self._refresh_lock:
            force = force or self._force_refresh
            self._force_refresh = False
            self.refresh_requested.clear()
        now = time.time()
        retry_due = self.next_retry_time is not None and now >= self.next_retry_time
        coverage = resolve_coverage(self.config, self._discover_coverage)
        catalog_changed = False
        # A catalog's first stream starts at the present. Close the discovery
        # gap by reading its metadata again after startup, including newly
        # discovered nested catalogs. Bound each pass under continuous churn.
        for attempt in range(4):
            changed_catalogs = self._sync_catalogs(
                coverage.catalog_roots, retry_due, force and attempt == 0)
            catalog_changed = catalog_changed or changed_catalogs
            if not changed_catalogs:
                break
            coverage = resolve_coverage(self.config, self._discover_coverage)
        else:
            self._signal_refresh()
        self.roots = tuple(dict.fromkeys(coverage.roots))
        unavailable = {path: "metadata access unavailable" for path in coverage.unavailable}
        candidates = []
        for root in self.roots:
            try:
                candidates.append(self._discover_one(root))
            except PendingRecoveryError:
                raise
            except OSError as error:
                unavailable[root] = str(error)
        desired_policy = policy_for(self.config, coverage)
        signature = json.dumps({
            "config": self.config.signature(), "roots": self.roots,
            "volumes": [_descriptor(volume) for volume in candidates],
            "excludes": desired_policy.excludes,
            "exclude_names": sorted(desired_policy.exclude_names),
            "hidden": desired_policy.skip_hidden_tops,
            "root_excludes": desired_policy.root_excludes,
        }, sort_keys=True)
        changed = force or signature != self._last_signature or (retry_due and bool(self._start_failures))
        if changed:
            previous_roots = set(self.active_roots)
            previous_watches = list(self._data.values())
            self._stop_data()
            if self._callback_error:
                raise self._callback_error
            for watch in previous_watches:
                if watch.stream.error:
                    raise watch.stream.error
            candidate_roots = tuple(root for volume in candidates for root in volume.roots)
            self.normalizer.policy = policy_for(self.config, coverage, roots=candidate_roots)
            self.index.bind_policy(self.normalizer.policy)
            baseline = self.index.configure(self.config.signature(), candidates, candidate_roots)
            self._start_failures = {}
            for volume in candidates:
                try:
                    self._data[volume.key] = self._start_watch(volume)
                except PendingRecoveryError:
                    raise
                except (OSError, CursorInvalidError) as error:
                    if self._callback_error:
                        raise self._callback_error
                    for root in volume.roots:
                        self._start_failures[root] = str(error)
            available = [watch.volume for watch in self._data.values()]
            self.active_roots = tuple(root for volume in available for root in volume.roots)
            if len(available) != len(candidates):
                self.normalizer.policy = policy_for(self.config, coverage, roots=self.active_roots)
                self.index.bind_policy(self.normalizer.policy)
                baseline = self.index.configure(self.config.signature(), available, self.active_roots)
            # No document enumeration can begin before every available native
            # source has started. The index queues only pending root baselines.
            if baseline:
                self.index.bootstrap_jobs()
            policy_changes = [root for root in self.active_roots if root in previous_roots
                              and self._root_exclusions.get(nfc(root), ())
                              != desired_policy.root_excludes.get(nfc(root), ())]
            if policy_changes:
                self.index.request_reconcile(policy_changes)
            self._root_exclusions = desired_policy.root_excludes
            self._last_signature = signature
        unavailable.update({root: reason for root, reason in self._start_failures.items() if root in self.roots})
        self.unavailable = unavailable
        if self.unavailable or self.catalog_unavailable:
            if self.next_retry_time is None or retry_due or changed:
                self.next_retry_time = now + self.RETRY_SECONDS
        else:
            self.next_retry_time = None
        self._save_status()
        return bool(changed or catalog_changed)

    def check(self):
        """Propagate ingestion failures and signal due metadata-only retries."""
        if self._callback_error:
            raise self._callback_error
        for watch in [*self._catalogs.values(), *self._data.values()]:
            if watch.stream.error:
                raise watch.stream.error
        if self.next_retry_time is not None and time.time() >= self.next_retry_time:
            self._signal_refresh()
        return self.refresh_requested.is_set()

    def close(self):
        if self._closed:
            return
        self._closed = True
        failure = None
        try:
            self._stop_data()
        except BaseException as error:
            failure = error
        for watch in reversed(list(self._catalogs.values())):
            try:
                self._stop(watch)
            except BaseException as error:
                failure = failure or error
        self._catalogs.clear()
        if failure:
            raise failure
