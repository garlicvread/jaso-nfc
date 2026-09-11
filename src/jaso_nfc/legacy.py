#!/usr/bin/env python3
"""Retained journal format, retry state, and compatibility recovery.

Names are verified after normalization. Filesystems that do not preserve NFC
spelling are not supported for normalization; equivalence is not preservation.
Successful operation history is retained independently of bounded diagnostics.
"""
import contextlib
import errno
import fcntl
import gzip
import json
import logging
import logging.handlers
import math
import os
from pathlib import Path
import shutil
import tempfile
import time
import unicodedata
import uuid

NO_DESCEND_EXTS = {".app", ".photoslibrary", ".musiclibrary", ".tvlibrary",
                   ".aplibrary", ".framework"}
TMP_SUFFIX = ".__jaso_nfc_tmp__"


def nfc(name):
    try:
        return unicodedata.normalize("NFC", name)
    except Exception:
        return name


@contextlib.contextmanager
def journal_locks(*paths):
    """All journal readers/writers use the same locks, in one order."""
    with contextlib.ExitStack() as stack:
        for path in sorted({os.path.abspath(p) for p in paths if p}):
            fh = stack.enter_context(open(path + ".lock", "a"))
            fcntl.flock(fh, fcntl.LOCK_EX)
        yield


def atomic_json(path, value):
    fd, tmp = tempfile.mkstemp(prefix=".retry-", dir=os.path.dirname(path) or ".")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(value, fh, ensure_ascii=False, separators=(",", ":"))
            fh.write("\n")
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp, path)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def path_signature(path, parent=False):
    try:
        st = os.lstat(path)
    except OSError as exc:
        return ["unavailable", exc.errno]
    result = [st.st_dev, st.st_ino, st.st_mode, st.st_uid, st.st_gid,
              getattr(st, "st_flags", 0)]
    # Directory contents change during normal renames. Those changes must not
    # reset the cooldown of every sibling with a persistent permission error.
    if not parent:
        result.append(st.st_ctime_ns)
    return result


def candidate_signature(src, dst):
    return [path_signature(src), path_signature(dst),
            path_signature(os.path.dirname(src), parent=True)]


class RetryState:
    def __init__(self, path, base, maximum):
        if base <= 0 or maximum < base:
            raise ValueError("retry intervals must satisfy 0 < base <= maximum")
        self.path, self.base, self.maximum = path, base, maximum
        self.entries = {}
        if path and os.path.exists(path):
            with open(path, encoding="utf-8") as fh:
                data = json.load(fh)
            if not isinstance(data, dict) or data.get("version") != 1:
                raise ValueError("unsupported retry state; preserve it before resetting")
            entries = data.get("entries")
            if not isinstance(entries, dict):
                raise ValueError("invalid retry state entries")
            for key, rec in entries.items():
                if (not isinstance(key, str) or not isinstance(rec, dict)
                        or not isinstance(rec.get("signature"), list)
                        or not isinstance(rec.get("reason"), str)
                        or type(rec.get("count")) is not int or rec["count"] < 1
                        or any(type(rec.get(k)) not in (int, float)
                               or not math.isfinite(rec[k])
                               for k in ("last_failure", "next_retry"))):
                    raise ValueError("invalid retry state record")
            # Incremental processing has no later full scan to rediscover lost
            # work. Keep every unresolved record until explicit resolution.
            self.entries = entries

    def deferred(self, src, dst):
        rec = self.entries.get(src)
        if not rec:
            return False
        if rec["signature"] != candidate_signature(src, dst):
            self.entries.pop(src, None)
            return False
        now = time.time()
        # Bound delay even after a backwards wall-clock adjustment.
        rec["next_retry"] = min(rec["next_retry"], now + self.maximum)
        return now < rec["next_retry"]

    def failure(self, src, dst, reason):
        signature = candidate_signature(src, dst)
        old = self.entries.get(src, {})
        count = (old.get("count", 0) + 1 if old.get("signature") == signature
                 and old.get("reason") == reason else 1)
        now = time.time()
        delay = min(self.maximum, self.base * 2 ** min(count - 1, 20))
        self.entries[src] = dict(signature=signature, reason=reason, count=count,
                                 last_failure=now, next_retry=now + delay)

    def save(self):
        if not self.path:
            return
        atomic_json(self.path, {"version": 1, "entries": self.entries})


class Journal:
    """Retain recoverable operations; bound disposable diagnostics separately."""
    def __init__(self, path, max_bytes, error_max_bytes, error_backups):
        if min(max_bytes, error_max_bytes, error_backups) <= 0:
            raise ValueError("log limits and backup count must be positive")
        self.path, self.max_bytes = path, max_bytes
        self.fh = self.errors = None
        if path:
            self.fh = open(path, "ab")
            try:
                size = self.fh.tell()
                if size:
                    with open(path, "rb") as existing:
                        existing.seek(-1, os.SEEK_END)
                        if existing.read(1) != b"\n":
                            length = min(size, 65536)
                            while True:
                                existing.seek(size - length)
                                tail = existing.read(length)
                                if b"\n" in tail or length == size:
                                    break
                                length = min(size, length * 2)
                            # A complete final JSON record may lack a newline.
                            # An interrupted/incomplete one must be repaired
                            # explicitly before this run mutates any filenames.
                            json.loads(tail.rsplit(b"\n", 1)[-1])
                            self.fh.write(b"\n")
                            self.fh.flush()
                            os.fsync(self.fh.fileno())
                error_path = os.path.splitext(path)[0] + ".errors.jsonl"
                self.errors = logging.handlers.RotatingFileHandler(
                    error_path, maxBytes=error_max_bytes, backupCount=error_backups,
                    encoding="utf-8")
                self.errors.setFormatter(logging.Formatter("%(message)s"))
            except BaseException:
                self.fh.close()
                raise

    def rotate(self):
        self.fh.flush()
        os.fsync(self.fh.fileno())
        self.fh.close()
        directory = Path(self.path + ".history")
        directory.mkdir(exist_ok=True)
        previous = [int(item.name.split("-", 1)[0]) for item in directory.iterdir()
                    if item.name.split("-", 1)[0].isdigit()]
        sequence = max(time.time_ns(), max(previous, default=0) + 1)
        raw = directory / f"{sequence:020d}-{uuid.uuid4().hex}.jsonl"
        os.replace(self.path, raw)
        # A plain segment is a valid durable history segment if compression is
        # interrupted. Readers select it once, even if a gzip twin also exists.
        tmp = str(raw) + ".gz.tmp"
        try:
            with open(raw, "rb") as source, open(tmp, "xb") as output:
                with gzip.GzipFile(fileobj=output, mode="wb", mtime=0) as compressed:
                    shutil.copyfileobj(source, compressed)
                output.flush()
                os.fsync(output.fileno())
            os.replace(tmp, str(raw) + ".gz")
            raw.unlink()
        except OSError:
            if os.path.exists(tmp):
                os.unlink(tmp)
        self.fh = open(self.path, "ab")

    def emit(self, rec):
        if not self.path:
            return
        text = json.dumps(rec, ensure_ascii=False)
        if rec.get("status") in ("renamed", "reverted") or rec.get("recovery_required"):
            data = (text + "\n").encode("utf-8")
            self.fh.write(data)
            self.fh.flush()
            os.fsync(self.fh.fileno())
            # The operation already happened. Persist its evidence before any
            # archive mkdir/rename/compression can fail. A segment can exceed
            # the target by the final record; no record is split or dropped.
            if self.fh.tell() >= self.max_bytes:
                self.rotate()
        else:
            record = logging.LogRecord("jaso-nfc", logging.WARNING, "", 0, text, (), None)
            # Call the underlying stream directly after rotation: logging's
            # normal emit swallows write errors, which would hide lost records.
            if self.errors.shouldRollover(record):
                self.errors.doRollover()
            self.errors.stream.write(text + "\n")
            self.errors.flush()

    def close(self):
        if self.fh:
            self.fh.close()
        if self.errors:
            self.errors.close()


def journal_records(log_file):
    path = Path(log_file)
    directory = Path(str(path) + ".history")
    segments = {}
    if directory.is_dir():
        for item in directory.iterdir():
            if item.name.endswith(".jsonl"):
                segments[item.name] = item
            elif item.name.endswith(".jsonl.gz"):
                segments.setdefault(item.name[:-3], item)
    sources = [segments[key] for key in sorted(segments)]
    if path.exists():
        sources.append(path)
    elif not sources:
        raise FileNotFoundError(log_file)
    for source in sources:
        opener = gzip.open if source.name.endswith(".gz") else open
        with opener(source, "rt", encoding="utf-8") as fh:
            for line in fh:
                if line.strip():
                    yield json.loads(line)


def revert(log_file, out_log):
    """Reverse retained operations using the same durable exclusive move path.

    If no output path is requested, retain inverse recovery evidence beside the
    source journal. A crash during recovery must remain recoverable too.
    """
    from . import normalizer

    output = os.fspath(out_log) if out_log else os.fspath(log_file) + ".revert.jsonl"
    pending = output + ".pending.json"
    os.makedirs(os.path.dirname(os.path.abspath(output)), exist_ok=True)
    with journal_locks(log_file, output, pending):
        entries = [rec for rec in journal_records(log_file)
                   if rec.get("status") == "renamed" or rec.get("recovery_required")]
        # Partial and later recovered records describe one mutation.
        seen_operations = set()
        unique = []
        for rec in reversed(entries):
            operation = rec.get("operation_id")
            if operation and operation in seen_operations:
                continue
            if operation:
                seen_operations.add(operation)
            unique.append(rec)
        roots = [rec["dir"] for rec in unique]
        engine = normalizer.Normalizer(normalizer.Policy(roots), output, None, pending)
        engine._journal = Journal(output, 8 * 1024 * 1024, 1024 * 1024, 3)
        done = failed = 0
        try:
            engine._recover()
            for operation in unique:
                parent = operation["dir"]
                destination = os.path.join(parent, operation["old"])
                source = operation.get("recovery_path") or os.path.join(parent, operation["new"])
                record = dict(revert=True, dir=parent, old=operation["new"],
                              new=operation["old"], ts=time.strftime("%Y-%m-%dT%H:%M:%S"))
                descriptor = None
                try:
                    if operation.get("identity"):
                        candidates = [operation.get("recovery_path"), operation.get("temporary_path"),
                                      source, destination]
                        source = next((path for path in candidates if path
                                       and path_signature(path)[:2] == operation["identity"]), None)
                        if source is None:
                            raise OSError(errno.ESTALE, "recovery identity remains unresolved", destination)
                    if os.path.dirname(source) != parent:
                        raise OSError(errno.EXDEV, "recovery source moved outside its recorded directory", source)
                    descriptor = engine._open_directory(parent)
                    info = os.stat(os.path.basename(source), dir_fd=descriptor, follow_symlinks=False)
                    if operation.get("identity") and normalizer._identity(info) != operation["identity"]:
                        raise OSError(errno.ESTALE, "source identity changed", source)
                    name = normalizer._stored_name(descriptor, normalizer._identity(info),
                                                   (os.path.basename(source),))
                    if name is None:
                        raise OSError(errno.ESTALE, "source spelling changed", source)
                    _, error, _ = engine._rename(parent, name, info, descriptor,
                                                target=operation["old"], operation_status="reverted")
                    if error:
                        failed += 1
                    else:
                        done += 1
                    if os.path.exists(pending):
                        # Never overwrite another unresolved operation's intent.
                        failed += len(unique) - done - failed
                        break
                except OSError as exc:
                    failed += 1
                    engine._journal.emit(dict(record, status="error", error=str(exc)))
                finally:
                    if descriptor is not None:
                        os.close(descriptor)
        finally:
            engine.close()
        return done, failed
