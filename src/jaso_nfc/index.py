"""Durable, coalesced reconciliation jobs and the observed filesystem index.

Native callbacks only enqueue work. Cursor advancement and its corresponding
jobs share a transaction; filesystem traversal never holds the database lock.
"""
from __future__ import annotations

import json
import logging
import os
from pathlib import Path
import sqlite3
import stat
import threading
import time
import unicodedata
from urllib.parse import quote

from .normalizer import PendingRecoveryError


_LOG = logging.getLogger(__name__)
_MUST_SCAN = 0x1
_LOST_EVENTS = 0x2 | 0x4 | 0x8
_HISTORY_DONE = 0x10
_ROOT_CHANGED = 0x20
_MOUNT_CHANGED = 0x40 | 0x80
_CREATED = 0x100
_REMOVED = 0x200
_RENAMED = 0x800
_IS_DIR = 0x20000
_IS_FILE = 0x10000
# Inode metadata, file data, FinderInfo, ownership, and extended attributes.
_CONTENT_FLAGS = 0x400 | 0x1000 | 0x2000 | 0x4000 | 0x8000
_MAX_JOBS = 4096


def _within(path, root):
    return path == root or path.startswith(root.rstrip(os.sep) + os.sep)


def _path(value):
    return os.path.normpath(os.path.abspath(os.fspath(value)))


def _sqlite_inode(value):
    # INTEGER affinity converts an unprefixed decimal string to an imprecise
    # REAL when it exceeds signed 64-bit. Keep unsigned filesystem IDs exact.
    if value is not None and not -(1 << 63) <= value < (1 << 63):
        return "u:" + str(value)
    return value


class Index:
    """A persistent index with a single worker and thread-safe event intake."""

    def __init__(self, path, read_only=False):
        self.path = _path(path)
        self._read_only = read_only
        self._lock = threading.RLock()
        self._worker = threading.Lock()
        self._policy = None
        self._active_job = None
        self._closed = False
        if read_only:
            uri = "file:" + quote(self.path, safe="/") + "?mode=ro"
            self._db = sqlite3.connect(uri, uri=True, check_same_thread=False, timeout=10)
        else:
            Path(self.path).parent.mkdir(parents=True, exist_ok=True)
            self._db = sqlite3.connect(self.path, check_same_thread=False, timeout=10)
        self._db.row_factory = sqlite3.Row
        if not read_only:
            self._db.execute("PRAGMA journal_mode=WAL")
            self._db.execute("PRAGMA synchronous=FULL")
            self._db.executescript("""
                CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS volumes (
                    key TEXT PRIMARY KEY, uuid TEXT NOT NULL, device INTEGER NOT NULL,
                    mount TEXT NOT NULL, roots TEXT NOT NULL, cursor TEXT);
                CREATE TABLE IF NOT EXISTS jobs (
                    path TEXT PRIMARY KEY, volume_key TEXT NOT NULL,
                    recursive INTEGER NOT NULL, baseline INTEGER NOT NULL DEFAULT 0,
                    generation INTEGER NOT NULL DEFAULT 1, attempts INTEGER NOT NULL DEFAULT 0,
                    next_attempt REAL NOT NULL DEFAULT 0, error TEXT);
                CREATE INDEX IF NOT EXISTS jobs_due ON jobs(next_attempt);
                CREATE INDEX IF NOT EXISTS jobs_events ON jobs(volume_key, recursive, baseline);
                CREATE TABLE IF NOT EXISTS deferred_jobs (
                    path TEXT PRIMARY KEY, volume_key TEXT NOT NULL,
                    recursive INTEGER NOT NULL, baseline INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE IF NOT EXISTS entries (
                    path TEXT PRIMARY KEY, parent TEXT NOT NULL, kind TEXT NOT NULL,
                    dev INTEGER, ino INTEGER, mtime_ns INTEGER, ctime_ns INTEGER,
                    size INTEGER, mode INTEGER);
                CREATE INDEX IF NOT EXISTS entries_parent ON entries(parent);
                CREATE TABLE IF NOT EXISTS directories (path TEXT PRIMARY KEY);
                CREATE TABLE IF NOT EXISTS metrics (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
            """)
            self._db.commit()
            with self._db:
                self._drain_deferred()

    def _writable(self):
        if self._read_only:
            raise RuntimeError("The index is open read-only")

    def _get(self, key, default=None):
        row = self._db.execute("SELECT value FROM meta WHERE key=?", (key,)).fetchone()
        return json.loads(row[0]) if row else default

    def _set(self, key, value):
        self._db.execute("INSERT OR REPLACE INTO meta VALUES (?, ?)", (key, json.dumps(value)))

    def _metric(self, key, amount=1):
        self._db.execute("""INSERT INTO metrics VALUES (?, ?)
            ON CONFLICT(key) DO UPDATE SET value=value+excluded.value""", (key, amount))

    def bind_policy(self, policy):
        """Bind the normalizer's policy before starting native event streams."""
        with self._lock:
            self._policy = policy

    def configure(self, config_signature, volumes, roots):
        """Stage baselines for changed roots while preserving unaffected coverage."""
        self._writable()
        roots = sorted({_path(root) for root in roots})
        identities = sorted((v.key, v.uuid, int(v.device), _path(v.mount),
                             sorted(_path(root) for root in v.roots)) for v in volumes)
        if (roots != sorted({root for *_, volume_roots in identities for root in volume_roots})
                or len({item[0] for item in identities}) != len(identities)):
            raise ValueError("Roots must match uniquely identified volume coverage")
        identity = [config_signature, roots, [(key, uuid, mount, volume_roots)
                    for key, uuid, _device, mount, volume_roots in identities]]
        # JSON normalization makes tuples compare equally after reopening SQLite.
        identity = json.loads(json.dumps(identity))
        with self._worker, self._lock, self._db:
            previous = self._get("identity")
            old = {row["key"]: dict(row) for row in self._db.execute("SELECT * FROM volumes")}
            pending = self._get("pending_baseline_roots")
            if pending is None:
                # Migrate an index created before root-specific baseline staging.
                pending = ({root: key for key, row in old.items() for root in json.loads(row["roots"])}
                           if not self._get("baseline_started", False) else {})
            if previous is None or previous[0] != identity[0]:
                for table in ("jobs", "deferred_jobs", "entries", "directories", "volumes"):
                    self._db.execute("DELETE FROM " + table)
                old, pending = {}, {}
            revalidate = set()
            if self._get("needs_revalidation", False):
                revalidate = set(self._get("revalidation_keys", list(old)))
            retained = {key for key, uuid, _device, mount, volume_roots in identities
                        if key in old
                        and (old[key]["uuid"], old[key]["mount"], json.loads(old[key]["roots"]))
                        == (uuid, mount, volume_roots)}
            retained_roots = [root for key in retained for root in json.loads(old[key]["roots"])]
            # A previously broader source may own a job for an independent root.
            # Transfer that evidence before deleting an unavailable source's jobs.
            retained_owners = [(root, key) for key in retained for root in json.loads(old[key]["roots"])]
            for table in ("jobs", "deferred_jobs"):
                for job in self._db.execute("SELECT path, volume_key FROM " + table).fetchall():
                    owners = [(len(root), owner) for root, owner in retained_owners if _within(job["path"], root)]
                    if job["volume_key"] not in retained and owners:
                        self._db.execute("UPDATE " + table + " SET volume_key=? WHERE path=?",
                                         (max(owners)[1], job["path"]))
            for key, row in old.items():
                if key in retained:
                    continue
                for table in ("jobs", "deferred_jobs", "volumes"):
                    column = "key" if table == "volumes" else "volume_key"
                    self._db.execute("DELETE FROM " + table + " WHERE " + column + "=?", (key,))
                for root in json.loads(row["roots"]):
                    self._prune(root, preserve=[other for other in retained_roots
                                                if other != root and _within(other, root)])
            pending = {root: key for root, key in pending.items() if key in retained}
            # Revalidated metadata acknowledges a control event without erasing
            # its committed cursor. Replaying history from NULL would deliver
            # the same historical mount again and restart reconstruction forever.
            pending.update({root: key for key in revalidate & retained
                            for root in json.loads(old[key]["roots"])})
            for key, uuid, device, mount, volume_roots in identities:
                if key not in retained:
                    self._db.execute("INSERT INTO volumes VALUES (?, ?, ?, ?, ?, NULL)",
                                     (key, uuid, device, mount, json.dumps(volume_roots)))
                    pending.update({root: key for root in volume_roots})
                else:
                    self._db.execute("UPDATE volumes SET device=? WHERE key=?", (device, key))
            for table in ("jobs", "deferred_jobs"):
                for job in self._db.execute("SELECT path, volume_key FROM " + table).fetchall():
                    owner = self._volume_for(job["path"])
                    if owner and owner != job["volume_key"]:
                        self._db.execute("UPDATE " + table + " SET volume_key=? WHERE path=?", (owner, job["path"]))
            self._set("identity", identity)
            self._set("roots", roots)
            self._set("pending_baseline_roots", pending)
            self._set("baseline_started", not pending)
            self._set("needs_revalidation", False)
            self._set("revalidation_keys", [])
            self._refresh_baseline()
            return not self._get("baseline_complete", False)

    def _refresh_baseline(self):
        unfinished = bool(self._get("pending_baseline_roots", {}))
        for table in ("jobs", "deferred_jobs"):
            unfinished = unfinished or self._db.execute(
                "SELECT 1 FROM " + table + " WHERE baseline=1 LIMIT 1").fetchone() is not None
        self._set("baseline_complete", not unfinished)

    def _mark_revalidation(self, volume_key):
        keys = set(self._get("revalidation_keys", []))
        keys.add(volume_key)
        self._set("revalidation_keys", sorted(keys))
        self._set("needs_revalidation", True)

    def cursor(self, volume_key):
        with self._lock:
            row = self._db.execute("SELECT cursor FROM volumes WHERE key=?", (volume_key,)).fetchone()
            if row is None:
                raise KeyError("Unknown volume: " + volume_key)
            return int(row[0]) if row[0] is not None else None

    def seed_cursor(self, volume_key, event_id):
        """Persist a new stream's start checkpoint without overwriting callbacks."""
        self._writable()
        if int(event_id) < 0:
            raise ValueError("Event IDs must be unsigned")
        with self._lock, self._db:
            self._volume_roots(volume_key)
            self._db.execute("UPDATE volumes SET cursor=? WHERE key=? AND cursor IS NULL",
                             (str(int(event_id)), volume_key))

    def invalidate_volume(self, volume_key):
        """Discard an invalid checkpoint and durably request covered-root recovery."""
        self._writable()
        with self._lock, self._db:
            for root in self._volume_roots(volume_key):
                self._queue(root, volume_key, True)
            self._db.execute("UPDATE volumes SET cursor=NULL WHERE key=?", (volume_key,))
            self._metric("recovery_requests")

    def _volume_roots(self, volume_key):
        row = self._db.execute("SELECT roots FROM volumes WHERE key=?", (volume_key,)).fetchone()
        if row is None:
            raise KeyError("Unknown volume: " + volume_key)
        return json.loads(row[0])

    def _volume_for(self, path):
        candidates = [(len(root), row["key"]) for row in self._db.execute("SELECT key, roots FROM volumes")
                      for root in json.loads(row["roots"]) if _within(path, root)]
        return max(candidates)[1] if candidates else None

    def _accepts(self, path):
        return self._policy is not None and self._policy.accepts(path)

    def _defer(self, path, volume_key, recursive, baseline):
        """Keep arriving work separate from a running recursive observation."""
        rows = self._db.execute("SELECT path, recursive FROM deferred_jobs WHERE volume_key=?", (volume_key,)).fetchall()
        for row in rows:
            if row["recursive"] and _within(path, row["path"]):
                path, recursive = row["path"], True
                break
        self._db.execute("""INSERT INTO deferred_jobs VALUES (?, ?, ?, ?)
            ON CONFLICT(path) DO UPDATE SET recursive=MAX(recursive, excluded.recursive),
            baseline=MAX(baseline, excluded.baseline)""", (path, volume_key, int(recursive), int(baseline)))
        if recursive:
            for row in rows:
                if row["path"] != path and _within(row["path"], path):
                    self._db.execute("DELETE FROM deferred_jobs WHERE path=?", (row["path"],))
        count = self._db.execute("SELECT COUNT(*) FROM deferred_jobs WHERE volume_key=?", (volume_key,)).fetchone()[0]
        if count > _MAX_JOBS:
            self._db.execute("DELETE FROM deferred_jobs WHERE volume_key=?", (volume_key,))
            for root in self._volume_roots(volume_key):
                self._db.execute("INSERT INTO deferred_jobs VALUES (?, ?, 1, 0)", (root, volume_key))
            self._metric("queue_overflows")

    def _drain_deferred(self):
        rows = self._db.execute("SELECT * FROM deferred_jobs").fetchall()
        self._db.execute("DELETE FROM deferred_jobs")
        for row in rows:
            self._queue(row["path"], row["volume_key"], bool(row["recursive"]), bool(row["baseline"]))

    def _queue(self, path, volume_key, recursive, baseline=False, due=0, preserve_delay=False):
        """Coalesce pending work without invalidating a running recursive scan."""
        volume_key = self._volume_for(path) or volume_key
        active = self._active_job
        if (active and active["recursive"] and active["volume_key"] == volume_key
                and _within(path, active["path"])):
            self._defer(path, volume_key, recursive, baseline)
            return
        ancestor = path
        while True:
            row = self._db.execute("SELECT path FROM jobs WHERE path=? AND volume_key=? AND recursive=1",
                                   (ancestor, volume_key)).fetchone()
            if row:
                path, recursive = row["path"], True
                break
            parent = os.path.dirname(ancestor)
            if parent == ancestor:
                break
            ancestor = parent
        self._db.execute("""INSERT INTO jobs
            (path, volume_key, recursive, baseline, next_attempt) VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(path) DO UPDATE SET
                recursive=MAX(jobs.recursive, excluded.recursive),
                baseline=MAX(jobs.baseline, excluded.baseline), generation=jobs.generation+1,
                next_attempt=CASE WHEN ? THEN jobs.next_attempt ELSE excluded.next_attempt END""",
                         (path, volume_key, int(recursive), int(baseline), due, int(preserve_delay)))
        if recursive:
            prefix = path.rstrip(os.sep) + os.sep
            bounds = (volume_key, prefix, prefix + chr(0x10FFFF))
            child_baseline = self._db.execute("SELECT MAX(baseline) FROM jobs WHERE volume_key=? AND path>=? AND path<?", bounds).fetchone()[0]
            if child_baseline:
                self._db.execute("UPDATE jobs SET baseline=1 WHERE path=?", (path,))
            self._db.execute("DELETE FROM jobs WHERE volume_key=? AND path>=? AND path<?", bounds)

    def _bound_queue(self, volume_key):
        count = self._db.execute("SELECT COUNT(*) FROM jobs WHERE volume_key=? AND recursive=0 AND baseline=0", (volume_key,)).fetchone()[0]
        if count > _MAX_JOBS:
            for root in self._volume_roots(volume_key):
                self._queue(root, volume_key, True)
            self._metric("queue_overflows")

    def enqueue(self, volume_key, events):
        """Atomically persist event consequences and their shared device cursor."""
        self._writable()
        if self._policy is None:
            raise RuntimeError("Bind the normalizer policy before receiving events")
        with self._lock, self._db:
            roots = self._volume_roots(volume_key)
            newest = self.cursor(volume_key)
            for event in events:
                flags = int(event.flags)
                event_id = int(event.id)
                if event_id < 0:
                    raise ValueError("Event IDs must be unsigned")
                newest = event_id if newest is None or flags & 0x8 else max(newest, event_id)
                self._metric("events_received")
                if flags & (_ROOT_CHANGED | _MOUNT_CHANGED):
                    self._mark_revalidation(volume_key)
                    continue
                if flags & _LOST_EVENTS:
                    for root in roots:
                        self._queue(root, volume_key, True)
                    self._metric("recovery_requests")
                    continue
                if flags & _HISTORY_DONE:
                    continue
                if not event.path:
                    continue
                path = _path(event.path)
                if not self._accepts(path) or not any(_within(path, root) for root in roots):
                    self._metric("excluded_events")
                    continue
                if flags & _MUST_SCAN:
                    self._queue(path, volume_key, True)
                    continue
                if (flags & _IS_FILE and flags & _CONTENT_FLAGS
                        and not flags & ~(_IS_FILE | _CONTENT_FLAGS)
                        and unicodedata.is_normalized("NFC", os.path.basename(path))):
                    known = self._db.execute("SELECT mode FROM entries WHERE path=? AND kind='file'",
                                             (path,)).fetchone()
                    if known and known[0] is not None and stat.S_ISREG(known[0]):
                        # These observations cannot change an already known NFC
                        # name. Unknown paths and deferred NFD candidates still scan.
                        self._metric("ignored_content_events")
                        continue
                owner = self._volume_for(path) or volume_key
                owner_roots = self._volume_roots(owner)
                parent = path if path in owner_roots else os.path.dirname(path)
                if self._accepts(parent):
                    self._queue(parent, volume_key, False)
                if flags & _IS_DIR and not flags & _REMOVED and self._policy.descend(path):
                    known = self._db.execute("SELECT 1 FROM directories WHERE path=?", (path,)).fetchone()
                    if not known or flags & (_CREATED | _RENAMED):
                        self._queue(path, volume_key, True)
                self._bound_queue(volume_key)
            if newest is not None:
                self._db.execute("UPDATE volumes SET cursor=? WHERE key=?", (str(newest), volume_key))
            self._bound_queue(volume_key)

    def bootstrap_jobs(self):
        """Queue newly configured roots only after their streams have started."""
        self._writable()
        with self._lock, self._db:
            for root, volume_key in self._get("pending_baseline_roots", {}).items():
                self._queue(root, volume_key, True, baseline=True)
            self._set("pending_baseline_roots", {})
            self._set("baseline_started", True)
            self._refresh_baseline()

    def request_reconcile(self, paths=None):
        """Request explicit subtree reconciliation while retaining event cursors."""
        self._writable()
        with self._lock, self._db:
            for value in paths if paths is not None else self._get("roots", []):
                path = _path(value)
                volume_key = self._volume_for(path)
                if volume_key is None or not self._accepts(path):
                    raise ValueError("Reconciliation path is outside the active policy: " + path)
                self._queue(path, volume_key, True)

    def _prune(self, path, include_self=True, preserve=()):
        # A lexical prefix range avoids LIKE wildcard interpretation for real names.
        prefix = path.rstrip(os.sep) + os.sep
        condition = "(path>=? AND path<?" + (" OR path=?" if include_self else "") + ")"
        values = [prefix, prefix + chr(0x10FFFF)] + ([path] if include_self else [])
        for root in preserve:
            root_prefix = root.rstrip(os.sep) + os.sep
            condition += " AND NOT (path=? OR (path>=? AND path<?))"
            values.extend((root, root_prefix, root_prefix + chr(0x10FFFF)))
        for table in ("entries", "directories"):
            self._db.execute("DELETE FROM " + table + " WHERE " + condition, values)

    def _replace(self, job, result):
        """Replace only completely enumerated directory scopes on partial errors."""
        path = job["path"]
        errors = result.get("errors", [])
        directories = {_path(p) for p in result.get("directories", [])}
        protected = []
        for row in self._db.execute("SELECT key, roots FROM volumes"):
            if row["key"] == job["volume_key"]:
                continue
            for root in json.loads(row["roots"]):
                if root != path and _within(root, path) and root not in directories:
                    try:
                        os.lstat(root)
                    except FileNotFoundError:
                        self._prune(root)
                        continue
                    except OSError:
                        pass  # An inaccessible independent root retains its last observation.
                    protected.append(root)
        failed_paths = [_path(error.get("path", path)) for error in errors]
        entries = [item for item in result.get("entries", []) if self._accepts(item["path"])]
        if job["recursive"] and not errors:
            self._prune(path, include_self=False, preserve=protected)
            self._db.execute("DELETE FROM directories WHERE path=?", (path,))
        elif not directories and not errors:
            self._prune(path, preserve=protected)
        new_by_parent = {}
        for item in entries:
            new_by_parent.setdefault(os.path.dirname(item["path"]), {})[item["path"]] = item
        for directory in directories:
            if not self._accepts(directory):
                continue
            children = new_by_parent.get(directory, {})
            old = self._db.execute("SELECT path, kind, dev, ino FROM entries WHERE parent=?", (directory,)).fetchall()
            for row in old:
                if any(_within(failed, row["path"]) or _within(row["path"], failed)
                       for failed in failed_paths):
                    continue
                if row["path"] not in children:
                    self._prune(row["path"], preserve=protected)
                elif row["kind"] in ("directory", "dir"):
                    child = children[row["path"]]
                    if (child["kind"] not in ("directory", "dir")
                            or (row["dev"], row["ino"]) != (child.get("dev"), _sqlite_inode(child.get("ino")))):
                        self._prune(row["path"], include_self=False, preserve=protected)
                        self._db.execute("DELETE FROM directories WHERE path=?", (row["path"],))
            self._db.execute("INSERT OR IGNORE INTO directories VALUES (?)", (directory,))
        for item in entries:
            if errors and os.path.dirname(item["path"]) not in directories:
                continue
            self._db.execute("""INSERT OR REPLACE INTO entries
                (path, parent, kind, dev, ino, mtime_ns, ctime_ns, size, mode)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                             (item["path"], os.path.dirname(item["path"]), item["kind"],
                              item.get("dev"), _sqlite_inode(item.get("ino")), item.get("mtime_ns"),
                              item.get("ctime_ns"), item.get("size"), item.get("mode")))
        return errors, directories

    def work(self, normalizer):
        """Run at most one due job; return False when idle or all jobs back off."""
        self._writable()
        with self._worker:
            self.bind_policy(normalizer.policy)
            with self._lock:
                if self._get("needs_revalidation", False):
                    raise RuntimeError("Event roots changed; revalidate volume identity and roots before continuing")
            retries = normalizer.retry_paths()
            with self._lock, self._db:
                self._drain_deferred()
                for value in retries:
                    path = _path(value)
                    volume_key = self._volume_for(path)
                    if volume_key and self._accepts(path):
                        self._queue(path, volume_key, False, preserve_delay=True)
                row = self._db.execute("""SELECT * FROM jobs WHERE next_attempt<=?
                    ORDER BY baseline DESC, recursive ASC, length(path), path LIMIT 1""", (time.time(),)).fetchone()
                if row is None:
                    return False
                job = dict(row)
                configured_root = job["path"] in self._get("roots", [])
                self._active_job = job
            # No transaction or database lock is held while visiting the filesystem.
            try:
                try:
                    if configured_root:
                        try:
                            root_info = os.lstat(job["path"])
                            if not stat.S_ISDIR(root_info.st_mode):
                                raise FileNotFoundError("configured root is no longer a directory")
                        except FileNotFoundError as error:
                            with self._lock, self._db:
                                self._mark_revalidation(job["volume_key"])
                            raise RuntimeError("Configured root is missing; revalidate roots before continuing") from error
                    result = normalizer.reconcile(job["path"], False)
                except PendingRecoveryError:
                    raise
                except OSError as error:
                    result = dict(entries=[], directories=[], errors=[{"path": job["path"], "error": str(error)}], renamed=0)
            except BaseException:
                with self._lock:
                    self._active_job = None
                raise
            with self._lock, self._db:
                self._active_job = None
                is_baseline_root = job["baseline"] and job["path"] in self._get("roots", [])
                metric = "baseline_walks" if is_baseline_root else "subtree_scans" if job["recursive"] else "shallow_scans"
                self._metric(metric)
                self._metric("renamed", int(result.get("renamed", 0)))
                errors, directories = self._replace(dict(job, recursive=False), result)
                self._db.execute("DELETE FROM jobs WHERE path=? AND generation=?", (job["path"], job["generation"]))
                actual_scope = _path(result.get("scope", job["path"]))
                if not directories and not errors:
                    # A deleted scope cannot report its stored spelling. Observe
                    # its parent to remove any canonically equivalent old entry.
                    parent = os.path.dirname(actual_scope)
                    if parent != actual_scope and self._accepts(parent):
                        self._queue(parent, job["volume_key"], False)
                # Parent acknowledgement precedes continuation enqueueing so a
                # completed recursive parent cannot absorb its own child jobs.
                for item in result.get("entries", []):
                    child = item["path"]
                    if (item["kind"] in ("directory", "dir") and self._accepts(child)
                            and self._policy.descend(child)
                            and (job["recursive"] or not self._db.execute(
                                "SELECT 1 FROM directories WHERE path=?", (child,)).fetchone())):
                        self._queue(child, job["volume_key"], True, bool(job["baseline"]))
                for error in errors:
                    failed = _path(error.get("path", actual_scope))
                    if not _within(failed, actual_scope) or not self._accepts(failed):
                        failed = actual_scope
                    # Enumeration failures target a directory; mutation failures target
                    # children of a successfully enumerated directory.
                    recursive = bool(job["recursive"])
                    if os.path.dirname(failed) in directories and failed not in directories:
                        # A known directory failed enumeration; do not walk its siblings.
                        old_dir = self._db.execute("SELECT kind FROM entries WHERE path=?", (failed,)).fetchone()
                        if old_dir and old_dir[0] not in ("directory", "dir"):
                            failed, recursive = os.path.dirname(failed), False
                    attempt = job["attempts"] + 1
                    delay = min(300.0, 2.0 ** min(attempt, 9))
                    self._queue(failed, job["volume_key"], recursive, bool(job["baseline"]), time.time() + delay, preserve_delay=True)
                    self._db.execute("UPDATE jobs SET attempts=MAX(attempts, ?), next_attempt=MAX(next_attempt, ?), error=? WHERE path=?", (attempt, time.time() + delay, str(error.get("error", "scan failed")), failed))
                    self._metric("errors")
                    _LOG.warning("Reconciliation will retry %s: %s", failed, error.get("error", "scan failed"))
                self._drain_deferred()
                self._refresh_baseline()
            return True

    def next_wakeup(self):
        """Return the earliest queued-work epoch, zero for ready work, or None."""
        with self._lock:
            if self._db.execute("SELECT 1 FROM deferred_jobs LIMIT 1").fetchone():
                return 0
            return self._db.execute("SELECT MIN(next_attempt) FROM jobs").fetchone()[0]

    def status(self):
        with self._lock:
            result = {key: 0 for key in ("baseline_walks", "subtree_scans", "shallow_scans", "errors", "renamed", "events_received", "excluded_events", "ignored_content_events", "queue_overflows", "recovery_requests")}
            result.update({row[0]: row[1] for row in self._db.execute("SELECT key, value FROM metrics")})
            result.update(baseline_complete=self._get("baseline_complete", False),
                          needs_revalidation=self._get("needs_revalidation", False))
            result["pending_baseline_roots"] = sorted(self._get("pending_baseline_roots", {}))
            for key, table in (("pending_jobs", "jobs"), ("indexed_entries", "entries"), ("indexed_directories", "directories")):
                result[key] = self._db.execute("SELECT COUNT(*) FROM " + table).fetchone()[0]
            result["deferred_jobs"] = self._db.execute("SELECT COUNT(*) FROM deferred_jobs").fetchone()[0]
            result["pending_jobs"] += result["deferred_jobs"]
            result["next_retry"] = self._db.execute("SELECT MIN(next_attempt) FROM jobs WHERE next_attempt>0").fetchone()[0]
            result["cursors"] = {row[0]: int(row[1]) if row[1] is not None else None for row in self._db.execute("SELECT key, cursor FROM volumes")}
            return result

    def close(self):
        with self._lock:
            if not self._closed:
                self._db.close()
                self._closed = True
