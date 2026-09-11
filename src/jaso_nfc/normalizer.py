"""Policy-aware reconciliation and durable, identity-checked filename changes.

The index consumes actual final paths from this module. A single pending intent
is persisted before each operation and removed only after durable recovery
evidence, independently of the disposable index and bounded retry diagnostics.
"""
import ctypes
import errno
import json
import os
from pathlib import Path
import stat
import sys
import time
import uuid

from .native_names import (actual_stored_name, marker_create, marker_get,
                           marker_remove, open_entry)

from .legacy import (Journal, NO_DESCEND_EXTS, RetryState, TMP_SUFFIX,
                     atomic_json, journal_locks, journal_records, nfc)


def _absolute(path):
    return os.path.abspath(os.fspath(path))


def _within(path, root):
    path, root = nfc(path), nfc(root)
    return path == root or path.startswith(root.rstrip(os.sep) + os.sep)


class Policy:
    """Apply exclusions to every ancestor, including directly delivered events."""

    def __init__(self, roots, excludes=(), exclude_names=(".git",), skip_hidden_tops=(),
                 root_excludes=None):
        self.roots = tuple(dict.fromkeys(_absolute(p) for p in roots))
        self.excludes = tuple(_absolute(p) for p in excludes)
        self.exclude_names = frozenset(nfc(p) for p in exclude_names)
        self.skip_hidden_tops = tuple(_absolute(p) for p in skip_hidden_tops)
        self.root_excludes = {
            nfc(_absolute(root)): tuple(_absolute(path) for path in paths)
            for root, paths in (root_excludes or {}).items()
        }

    def contains(self, path):
        return any(_within(_absolute(path), root) for root in self.roots)

    def _root(self, path):
        roots = [r for r in self.roots if _within(path, r)]
        return max(roots, key=lambda r: len(nfc(r))) if roots else None

    def _components(self, path, root):
        # Number of components, rather than byte offsets, also handles Unicode
        # normalization-equivalent spelling in an event path or configured root.
        parts = Path(path).parts[len(Path(root).parts):]
        current = root
        for part in parts:
            current = os.path.join(current, part)
            yield current, part

    def accepts(self, path):
        path = _absolute(path)
        root = self._root(path)
        if root is None or any(_within(path, excluded) for excluded in self.excludes):
            return False
        if any(_within(path, excluded) for excluded in self.root_excludes.get(nfc(root), ())):
            return False
        for top in self.skip_hidden_tops:
            if _within(path, top) and nfc(path) != nfc(top):
                parts = Path(path).parts[len(Path(top).parts):]
                if parts and parts[0].startswith("."):
                    return False
        # A nested root overrides only its enclosing root's path exclusions.
        # Package, symlink, mount, and excluded-name boundaries still apply
        # between the broadest configured root and the event path.
        boundary_root = min((r for r in self.roots if _within(path, r)),
                            key=lambda r: len(nfc(r)))
        components = list(self._components(path, boundary_root))
        for ancestor, name in components:
            # macOS moves AppleDouble companions with their owning item.
            # Renaming the companion independently can detach its metadata.
            if nfc(name) in self.exclude_names or name.endswith(TMP_SUFFIX) or name.startswith("._"):
                return False
            configured = any(nfc(ancestor) == nfc(r) for r in self.roots)
            if nfc(ancestor) != nfc(path) and self._blocked_directory(ancestor, configured_root=configured):
                return False
        if nfc(path) != nfc(boundary_root) and self._blocked_directory(boundary_root, configured_root=True):
            return False
        return True

    @staticmethod
    def _blocked_directory(path, configured_root=False):
        try:
            info = os.lstat(path)
        except FileNotFoundError:
            return False
        except OSError:
            # Permission/I/O failures are not exclusions. The open/enumeration
            # stage reports them so the index retains its previous evidence.
            return False
        if stat.S_ISLNK(info.st_mode):
            return True
        if not stat.S_ISDIR(info.st_mode):
            return True
        if os.path.splitext(path)[1].lower() in NO_DESCEND_EXTS:
            return True
        return not configured_root and os.path.ismount(path)

    def descend(self, path):
        path = _absolute(path)
        return self.accepts(path) and not self._blocked_directory(
            path, configured_root=any(nfc(path) == nfc(r) for r in self.roots))


def rename_exclusive(source, destination, dir_fd):
    """Rename within an open directory, atomically rejecting any destination."""
    libc = ctypes.CDLL(None, use_errno=True)
    if sys.platform == "darwin":
        function = libc.renameatx_np
        flags = 0x00000004  # RENAME_EXCL from Darwin stdio.h.
    elif hasattr(libc, "renameat2"):
        function = libc.renameat2
        flags = 1  # Linux RENAME_NOREPLACE; also permits portable fixture tests.
    else:
        raise OSError(errno.ENOTSUP, "atomic exclusive rename unavailable")
    function.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int,
                         ctypes.c_char_p, ctypes.c_uint]
    function.restype = ctypes.c_int
    if function(dir_fd, os.fsencode(source), dir_fd, os.fsencode(destination), flags):
        number = ctypes.get_errno()
        raise OSError(number, os.strerror(number), destination)


def _identity(info):
    return [info.st_dev, info.st_ino]


def _stat_at(fd, name):
    try:
        return os.stat(name, dir_fd=fd, follow_symlinks=False)
    except FileNotFoundError:
        return None


def rename_guarded(source, destination, dir_fd, expected_identity, before_fallback):
    """Prefer exclusive rename, with a checked Darwin filesystem fallback.

    The fallback cannot close the interval between its destination check and
    ordinary rename. Persist that weaker mode before mutation, and reject every
    conflict or source replacement detectable before entering that interval.
    """
    try:
        return rename_exclusive(source, destination, dir_fd)
    except OSError as exc:
        if (sys.platform != "darwin"
                or exc.errno not in (errno.ENOTSUP, errno.EOPNOTSUPP, errno.ENOSYS)):
            raise
    refreshed_identity = before_fallback()
    if refreshed_identity is not None:
        expected_identity = refreshed_identity
    current = _stat_at(dir_fd, source)
    if current is None or _identity(current) != expected_identity:
        raise OSError(errno.ESTALE, "source identity changed before guarded rename", source)
    if _stat_at(dir_fd, destination) is not None:
        raise FileExistsError(errno.EEXIST, "destination exists before guarded rename", destination)
    os.rename(source, destination, src_dir_fd=dir_fd, dst_dir_fd=dir_fd)


def _stored_name(fd, identity, candidates=None):
    """Read directory spelling rather than trusting equivalence-sensitive lookup."""
    with os.scandir(fd) as entries:
        for entry in entries:
            if candidates is not None and not any(nfc(entry.name) == nfc(name) for name in candidates):
                continue
            try:
                actual = actual_stored_name(fd, entry.name) if nfc(entry.name) != entry.name else entry.name
                if _identity(os.stat(actual, dir_fd=fd, follow_symlinks=False)) == identity:
                    return actual
            except FileNotFoundError:
                continue
    return None


def _list_directory(fd):
    # Complete enumeration before mutation; failure cannot masquerade as an
    # authoritative empty or partial directory listing in the persistent index.
    values = []
    with os.scandir(fd) as entries:
        for entry in entries:
            try:
                actual = actual_stored_name(fd, entry.name) if nfc(entry.name) != entry.name else entry.name
                values.append((actual, os.stat(actual, dir_fd=fd, follow_symlinks=False)))
            except FileNotFoundError:
                continue
    return values


def _sync_directory(path):
    descriptor = os.open(path or ".", os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


class PendingRecoveryError(OSError):
    """A recorded operation needs resolution before any further mutation."""


class Normalizer:
    RETRY_BASE = 900
    RETRY_MAX = 86400

    def __init__(self, policy, log_path, retry_path, pending_path, apply=True):
        self.policy = policy
        self.log_path = os.fspath(log_path) if log_path else None
        self.retry_path = os.fspath(retry_path) if retry_path else None
        self.pending_path = os.fspath(pending_path) if pending_path else None
        self.apply = apply
        if apply and (not self.log_path or not self.pending_path):
            raise ValueError("applying normalization requires journal and pending paths")
        self.retry = RetryState(self.retry_path, self.RETRY_BASE, self.RETRY_MAX)
        self._journal = None

    def close(self):
        if self._journal:
            self._journal.close()
            self._journal = None

    def _prepare_state(self):
        for path in (self.log_path, self.retry_path, self.pending_path):
            if path:
                os.makedirs(os.path.dirname(_absolute(path)), exist_ok=True)

    def _open_directory(self, path):
        root = self.policy._root(path)
        if root is None:
            raise PermissionError(errno.EACCES, "outside configured roots", path)
        flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
        descriptor = os.open(root, flags)
        try:
            for _, part in self.policy._components(path, root):
                child = os.open(part, flags, dir_fd=descriptor)
                os.close(descriptor)
                descriptor = child
            return descriptor
        except BaseException:
            os.close(descriptor)
            raise

    def _actual_scope(self, path):
        """Resolve stored spellings below the root without following symlinks."""
        root = self.policy._root(path)
        if root is None or path == root:
            return path
        descriptor = self._open_directory(root)
        actual = root
        try:
            for _, part in self.policy._components(path, root):
                try:
                    spelling = actual_stored_name(descriptor, part)
                except FileNotFoundError:
                    return path
                actual = os.path.join(actual, spelling)
                child = os.open(spelling, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                dir_fd=descriptor)
                os.close(descriptor)
                descriptor = child
            return actual
        finally:
            os.close(descriptor)

    def _pending_write(self, operation):
        atomic_json(self.pending_path, operation)
        _sync_directory(os.path.dirname(self.pending_path))

    def _pending_clear(self):
        os.unlink(self.pending_path)
        _sync_directory(os.path.dirname(self.pending_path))

    def _load_pending(self):
        if not self.pending_path or not os.path.exists(self.pending_path):
            return None
        with open(self.pending_path, encoding="utf-8") as stream:
            operation = json.load(stream)
        required = ("dir", "old", "new", "temporary_path", "identity", "operation_id")
        if (not isinstance(operation, dict) or operation.get("version") != 1
                or any(key not in operation for key in required)
                or not isinstance(operation["identity"], list)
                or len(operation["identity"]) != 2
                or any(not isinstance(operation[key], str) for key in required if key != "identity")
                or any(os.path.basename(operation[key]) != operation[key] for key in ("old", "new"))
                or os.path.dirname(operation["temporary_path"]) != operation["dir"]):
            raise PendingRecoveryError(errno.EINVAL, "invalid pending operation; preserve state")
        if "marker" in operation:
            marker = operation["marker"]
            if (not isinstance(marker, dict) or not isinstance(marker.get("name"), str)
                    or not marker["name"].startswith("user.jaso_nfc.")
                    or len(marker["name"]) > 255
                    or marker.get("token") != operation["operation_id"]
                    or not operation["operation_id"].isascii()):
                raise PendingRecoveryError(errno.EINVAL, "invalid pending marker; preserve state")
        return operation

    def _emit_success(self, operation, recovered=False):
        record = dict(operation, status=operation.get("operation_status", "renamed"))
        if recovered:
            record["recovered"] = True
        self._journal.emit(record)
        return record

    def _move(self, operation, source, destination, descriptor):
        def before_fallback():
            operation["rename_mode"] = "guarded"
            if not operation.get("marker"):
                operation["marker"] = dict(name="user.jaso_nfc." + uuid.uuid4().hex,
                                           token=operation["operation_id"])
                operation["phase"] = "marker-intent"
            self._pending_write(operation)
            held = open_entry(descriptor, source)
            try:
                marker = operation["marker"]
                token = marker["token"].encode("ascii")
                current = marker_get(held, marker["name"])
                info = os.fstat(held)
                if current != token:
                    if (current is not None or operation["phase"] != "marker-intent"
                            or _identity(info) != operation["identity"]):
                        raise OSError(errno.ESTALE, "source identity or operation marker changed", source)
                    info = marker_create(held, marker["name"], token)
                    os.fsync(held)
                operation.update(identity=_identity(info), phase="moving")
                self._pending_write(operation)
                return operation["identity"]
            finally:
                os.close(held)

        rename_guarded(source, destination, descriptor, operation["identity"], before_fallback)
        if operation.get("marker"):
            located = self._find_marker(operation, descriptor, (destination,))
            if located:
                operation["identity"] = _identity(located[1])
                self._pending_write(operation)

    @staticmethod
    def _find_marker(operation, descriptor, names):
        marker = operation.get("marker")
        if not marker:
            return None
        for name in names:
            try:
                held = open_entry(descriptor, name)
            except FileNotFoundError:
                continue
            try:
                if marker_get(held, marker["name"]) == marker["token"].encode("ascii"):
                    return actual_stored_name(descriptor, name), os.fstat(held)
            finally:
                os.close(held)
        return None

    def _remove_marker(self, operation, descriptor, name):
        marker = operation.get("marker")
        if not marker:
            return
        held = open_entry(descriptor, name)
        try:
            if _identity(os.fstat(held)) != operation["identity"]:
                raise PendingRecoveryError(errno.ESTALE, "identity changed before marker cleanup", name)
            info = marker_remove(held, marker["name"], marker["token"].encode("ascii"))
            os.fsync(held)
            operation["identity"] = _identity(info)
            operation["identity_finalized"] = True
            self._pending_write(operation)
        finally:
            os.close(held)

    def _cancel(self, operation, descriptor, name):
        if operation.get("marker"):
            operation["phase"] = "not-started"
            self._pending_write(operation)
            self._remove_marker(operation, descriptor, name)
        self._pending_clear()

    def _recorded_success(self, operation):
        result = None
        if self.log_path and os.path.exists(self.log_path):
            for row in journal_records(self.log_path):
                if (row.get("operation_id") == operation["operation_id"]
                        and row.get("status") == operation.get("operation_status", "renamed")):
                    result = row
        return result

    def _complete(self, operation, descriptor, recovered=False, recorded=None):
        if operation.get("marker"):
            operation["phase"] = "committed"
            self._pending_write(operation)
        record = recorded or self._emit_success(operation, recovered)
        if operation.get("marker"):
            self._remove_marker(operation, descriptor, operation["new"])
            if record["identity"] != operation["identity"]:
                record = self._emit_success(operation, recovered)
        self._pending_clear()
        return record

    def _recover(self):
        operation = self._load_pending()
        if operation is None:
            return None
        parent = operation["dir"]
        descriptor = self._open_directory(parent)
        try:
            identity = operation["identity"]
            old, new = operation["old"], operation["new"]
            temporary = os.path.basename(operation["temporary_path"])
            marked = self._find_marker(operation, descriptor, (temporary, new, old))
            if operation.get("phase") == "not-started":
                if marked:
                    operation["identity"] = _identity(marked[1])
                    self._remove_marker(operation, descriptor, marked[0])
                self._pending_clear()
                return dict(operation, status="not-started", recovered=True)
            recorded = self._recorded_success(operation)
            if operation.get("phase") == "committed" and recorded:
                if marked:
                    operation["identity"] = _identity(marked[1])
                    self._pending_write(operation)
                    return self._complete(operation, descriptor, recovered=True, recorded=recorded)
                if operation.get("identity_finalized"):
                    if recorded["identity"] != operation["identity"]:
                        recorded = self._emit_success(operation, recovered=True)
                    self._pending_clear()
                    return dict(recorded, recovered=True)
                # Cleanup can change exFAT's synthetic inode before its final
                # identity reaches disk. The retained success proves the rename
                # completed; never adopt or mutate a possibly replaced target.
                record = dict(recorded, recovered=True, identity_finalization_unavailable=True)
                self._journal.emit(dict(record, status="identity-finalization-unavailable"))
                self._pending_clear()
                return record
            if marked:
                identity = operation["identity"] = _identity(marked[1])
                self._pending_write(operation)
            elif operation.get("phase") == "moving":
                staged = _stat_at(descriptor, temporary)
                if staged is not None:
                    operation.update(expected_identity=identity,
                                     identity=_identity(staged), recovery_action="rollback",
                                     expected_marker=operation.pop("marker"))
                    operation.pop("phase", None)
                    self._pending_write(operation)
                    return self._recover()
                raise PendingRecoveryError(errno.ESTALE, "pending operation marker is missing", parent)
            located = {name: _stat_at(descriptor, name) for name in (old, new, temporary)}
            names = [name for name, _ in _list_directory(descriptor)]
            if operation.get("recovery_action") == "rollback":
                if located[old] and _identity(located[old]) != identity:
                    raise PendingRecoveryError(errno.EEXIST, "rollback destination was replaced", os.path.join(parent, old))
                if old not in names:
                    if not located[temporary] or _identity(located[temporary]) != identity:
                        raise PendingRecoveryError(errno.ESTALE, "rollback identity is missing", parent)
                    self._move(operation, temporary, old, descriptor)
                    os.fsync(descriptor)
                if operation.get("marker"):
                    self._remove_marker(operation, descriptor, old)
                record = dict(operation, status="rolled-back", recovered=True)
                self._journal.emit(record)
                self._pending_clear()
                return record
            matches = [name for name in (temporary, new, old) if name in names
                       and located[name] and _identity(located[name]) == identity]
            if not matches:
                if located[temporary] is not None:
                    # A crash can land between the first rename and its inode
                    # comparison. Preserve and restore the actual staged object
                    # instead of retaining an unusable expected-identity record.
                    operation.update(expected_identity=identity,
                                     identity=_identity(located[temporary]),
                                     recovery_action="rollback")
                    self._pending_write(operation)
                    return self._recover()
                raise PendingRecoveryError(errno.ESTALE, "pending identity is missing", parent)
            if located[new] and _identity(located[new]) != identity:
                raise PendingRecoveryError(errno.EEXIST, "pending destination was replaced", os.path.join(parent, new))
            actual = matches[0]
            if actual == old:
                # Intent persisted but first mutation never happened.
                self._cancel(operation, descriptor, old)
                return dict(operation, status="not-started", recovered=True)
            if actual == temporary:
                self._move(operation, temporary, new, descriptor)
                os.fsync(descriptor)
                identity = operation["identity"]
            if _stored_name(descriptor, identity, (new, old, temporary)) != new:
                raise PendingRecoveryError(errno.ENOTSUP, "recovered name is not stored as NFC", parent)
            # Crash after journal fsync but before intent deletion must not
            # duplicate a reversible operation in the retained history.
            return self._complete(operation, descriptor, recovered=True, recorded=recorded)
        except OSError as exc:
            if isinstance(exc, PendingRecoveryError):
                raise
            raise PendingRecoveryError(exc.errno, str(exc), parent) from exc
        finally:
            os.close(descriptor)

    def recover(self):
        if not self.apply:
            return None
        self._prepare_state()
        with journal_locks(self.log_path, self.pending_path):
            self._journal = Journal(self.log_path, 8 * 1024 * 1024, 1024 * 1024, 3)
            try:
                return self._recover()
            finally:
                self.close()

    def retry_paths(self, now=None):
        now = time.time() if now is None else now
        due = set()
        changed = False
        listings = {}
        for path, record in list(self.retry.entries.items()):
            bounded = min(record["next_retry"], now + self.RETRY_MAX)
            if bounded != record["next_retry"]:
                record["next_retry"] = bounded
                changed = True
            if record["next_retry"] > now:
                continue
            try:
                info = os.lstat(path)
            except FileNotFoundError:
                self.retry.entries.pop(path, None)
                changed = True
                continue
            except OSError:
                info = None
            if not self.policy.accepts(path):
                continue
            parent = os.path.dirname(path)
            if info is not None:
                try:
                    if parent not in listings:
                        descriptor = self._open_directory(parent)
                        try:
                            listings[parent] = _list_directory(descriptor)
                        finally:
                            os.close(descriptor)
                    actual = next((name for name, item in listings[parent]
                                   if _identity(item) == _identity(info)
                                   and nfc(name) == nfc(os.path.basename(path))), None)
                    if actual is not None and nfc(actual) == actual:
                        self.retry.entries.pop(path, None)
                        changed = True
                        continue
                except OSError:
                    pass
            due.add(parent)
        if changed and self.apply:
            self.retry.save()
        return sorted(due)

    @staticmethod
    def _entry(path, info):
        kind = "symlink" if stat.S_ISLNK(info.st_mode) else "dir" if stat.S_ISDIR(info.st_mode) else "file"
        return dict(path=path, kind=kind, dev=info.st_dev, ino=info.st_ino,
                    mtime_ns=info.st_mtime_ns, ctime_ns=info.st_ctime_ns,
                    size=info.st_size, mode=info.st_mode)

    def _rename(self, parent, name, info, fd, target=None, operation_status="renamed"):
        target = nfc(name) if target is None else target
        source = os.path.join(parent, name)
        destination = os.path.join(parent, target)
        if (not self.apply or target == name
                or (operation_status == "renamed"
                    and any(nfc(source) == nfc(root) for root in self.policy.roots))
                or self.retry.deferred(source, destination)):
            return name, None, False
        identity = _identity(info)
        operation = dict(version=1, operation_id=uuid.uuid4().hex,
                         dir=parent, old=name, new=target,
                         operation_status=operation_status, rename_mode="exclusive",
                         type=self._entry(source, info)["kind"], identity=identity,
                         temporary_path=os.path.join(parent, ".jaso-" + uuid.uuid4().hex + TMP_SUFFIX),
                         ts=time.strftime("%Y-%m-%dT%H:%M:%S"))
        temporary = os.path.basename(operation["temporary_path"])
        pending = False
        try:
            current = _stat_at(fd, name)
            if current is None or _identity(current) != identity:
                raise OSError(errno.ESTALE, "source identity changed", source)
            existing = _stat_at(fd, target)
            if existing and _identity(existing) != identity:
                raise FileExistsError(errno.EEXIST, "destination exists", destination)
            self._pending_write(operation)
            pending = True
            self._move(operation, name, temporary, fd)
            os.fsync(fd)
            identity = operation["identity"]
            staged = _stat_at(fd, temporary)
            if staged is not None and _identity(staged) != identity:
                # Source lookup and rename cannot atomically compare inodes.
                # If another writer replaced it, restore the object we moved;
                # durable rollback intent records its real identity first.
                operation.update(expected_identity=identity,
                                 identity=_identity(staged), recovery_action="rollback")
                if operation.get("marker"):
                    operation["expected_marker"] = operation.pop("marker")
                    operation.pop("phase", None)
                identity = operation["identity"]
                self._pending_write(operation)
                self._move(operation, temporary, name, fd)
                os.fsync(fd)
                self._cancel(operation, fd, name)
                pending = False
                raise OSError(errno.ESTALE, "source was replaced before rename; replacement restored", source)
            if staged is None:
                raise PendingRecoveryError(errno.ESTALE, "temporary identity changed", operation["temporary_path"])
            self._move(operation, temporary, target, fd)
            os.fsync(fd)
            identity = operation["identity"]
            stored = _stored_name(fd, identity, (target, name, temporary))
            if stored != target:
                # HFS-like behavior may restore a decomposed spelling. Return
                # the original object to its original name before backing off.
                current = _stat_at(fd, target)
                if current is None or _identity(current) != identity:
                    raise PendingRecoveryError(errno.ESTALE, "normalization identity changed", destination)
                self._move(operation, target, temporary, fd)
                self._move(operation, temporary, name, fd)
                os.fsync(fd)
                self._cancel(operation, fd, name)
                pending = False
                raise OSError(errno.ENOTSUP, "filesystem does not preserve NFC spelling", source)
            self._complete(operation, fd)
            pending = False
            self.retry.entries.pop(source, None)
            return target, None, True
        except OSError as exc:
            # A denied first rename did not mutate anything. Other failures
            # preserve the intent; no later operation may replace that record.
            identity = operation["identity"]
            actual = _stored_name(fd, identity, (target, name, temporary))
            if pending and actual == name and _stat_at(fd, temporary) is None:
                existing = _stat_at(fd, target)
                if existing is None or _identity(existing) == identity:
                    try:
                        self._cancel(operation, fd, name)
                        pending = False
                    except OSError:
                        pass  # Keep durable marker/identity evidence for recovery.
            self.retry.failure(source, destination, str(exc.errno))
            record = dict(operation, status="error", error=str(exc))
            if pending:
                record.update(recovery_required=True)
                if actual:
                    record["recovery_path"] = os.path.join(parent, actual)
            self._journal.emit(record)
            return actual or name, str(exc), False

    @staticmethod
    def _rewrite(result, old, new):
        prefix = old + os.sep
        for entry in result["entries"]:
            if entry["path"].startswith(prefix):
                entry["path"] = new + entry["path"][len(old):]
        result["directories"] = [new + p[len(old):] if p == old or p.startswith(prefix) else p
                                 for p in result["directories"]]
        for error in result["errors"]:
            if error["path"] == old or error["path"].startswith(prefix):
                error["path"] = new + error["path"][len(old):]

    def _walk(self, path, recursive, result):
        if not self.policy.descend(path):
            return
        try:
            fd = self._open_directory(path)
        except FileNotFoundError:
            return
        except OSError as exc:
            result["errors"].append(dict(path=path, error=str(exc)))
            return
        try:
            try:
                children = _list_directory(fd)
            except OSError as exc:
                result["errors"].append(dict(path=path, error=str(exc)))
                return
            result["directories"].append(path)
            for name, info in children:
                source = os.path.join(path, name)
                if not self.policy.accepts(source):
                    continue
                if recursive and stat.S_ISDIR(info.st_mode) and self.policy.descend(source):
                    self._walk(source, True, result)
                if self.apply and self.pending_path and os.path.exists(self.pending_path):
                    result["errors"].append(dict(path=path, error="unresolved pending operation blocks mutations"))
                    return
                actual, error, renamed = self._rename(path, name, info, fd)
                final = os.path.join(path, actual)
                if actual != name and stat.S_ISDIR(info.st_mode):
                    self._rewrite(result, source, final)
                    for candidate in list(self.retry.entries):
                        if candidate.startswith(source + os.sep):
                            self.retry.entries[final + candidate[len(source):]] = self.retry.entries.pop(candidate)
                result["renamed"] += int(renamed)
                if error:
                    result["errors"].append(dict(path=source, error=error))
                try:
                    current = os.stat(actual, dir_fd=fd, follow_symlinks=False)
                    result["entries"].append(self._entry(final, current))
                except FileNotFoundError:
                    continue
                except OSError as exc:
                    result["errors"].append(dict(path=final, error=str(exc)))
        finally:
            os.close(fd)

    def reconcile(self, path, recursive):
        result = dict(entries=[], directories=[], errors=[], renamed=0)
        path = _absolute(path)
        if self.policy.descend(path):
            try:
                actual = self._actual_scope(path)
            except FileNotFoundError:
                return result
            except OSError as exc:
                result["errors"].append(dict(path=path, error=str(exc)))
                return result
            if actual != path:
                result["scope"] = actual
                path = actual
        if not self.apply:
            self._walk(path, recursive, result)
            return result
        self._prepare_state()
        with journal_locks(self.log_path, self.pending_path):
            self._journal = Journal(self.log_path, 8 * 1024 * 1024, 1024 * 1024, 3)
            try:
                self._recover()
                self._walk(path, recursive, result)
                return result
            finally:
                try:
                    self.retry.save()
                finally:
                    self.close()
