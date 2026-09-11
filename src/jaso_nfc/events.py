"""Persistent, per-volume macOS FSEvents with a standard-library native binding.

Events are reconciliation hints. The consumer must durably enqueue a whole batch
before storing its cursor, and must recover from the explicit control flags.
"""

from __future__ import annotations

import ctypes as C
from dataclasses import dataclass
import fcntl
from functools import lru_cache
import hashlib
import math
import os
import sys
import threading
import time
from typing import Callable, Iterable

MUST_SCAN_SUBDIRS = 0x00000001
USER_DROPPED = 0x00000002
KERNEL_DROPPED = 0x00000004
EVENT_IDS_WRAPPED = 0x00000008
HISTORY_DONE = 0x00000010
ROOT_CHANGED = 0x00000020
MOUNT = 0x00000040
UNMOUNT = 0x00000080
ITEM_CREATED = 0x00000100
ITEM_REMOVED = 0x00000200
ITEM_RENAMED = 0x00000800
ITEM_IS_DIR = 0x00020000
_CONTROL = USER_DROPPED | KERNEL_DROPPED | EVENT_IDS_WRAPPED | HISTORY_DONE
_WATCH_ROOT = 0x04
_IGNORE_SELF = 0x08
_FILE_EVENTS = 0x10
_GETPATH_NOFIRMLINK = 102


@dataclass(frozen=True)
class Event:
    path: str
    flags: int
    id: int


@dataclass(frozen=True)
class Volume:
    key: str
    device: int
    uuid: str
    mount: str
    roots: tuple[str, ...]


class CursorInvalidError(RuntimeError):
    """The saved cursor cannot be safely reused; reconciliation is required."""


class _StatFS(C.Structure):
    # Darwin's 64-bit-inode statfs ABI, for both supported desktop architectures.
    _fields_ = [
        ("f_bsize", C.c_uint32), ("f_iosize", C.c_int32),
        ("f_blocks", C.c_uint64), ("f_bfree", C.c_uint64),
        ("f_bavail", C.c_uint64), ("f_files", C.c_uint64),
        ("f_ffree", C.c_uint64), ("f_fsid", C.c_int32 * 2),
        ("f_owner", C.c_uint32), ("f_type", C.c_uint32),
        ("f_flags", C.c_uint32), ("f_fssubtype", C.c_uint32),
        ("f_fstypename", C.c_char * 16), ("f_mntonname", C.c_char * 1024),
        ("f_mntfromname", C.c_char * 1024), ("f_flags_ext", C.c_uint32),
        ("f_reserved", C.c_uint32 * 7),
    ]


_Callback = C.CFUNCTYPE(None, C.c_void_p, C.c_void_p, C.c_size_t,
                       C.c_void_p, C.POINTER(C.c_uint32), C.POINTER(C.c_uint64))
_DispatchFunction = C.CFUNCTYPE(None, C.c_void_p)


class _Native:
    def __init__(self):
        if sys.platform != "darwin":
            raise OSError("FSEvents requires macOS")
        self.cf = C.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
        self.fs = C.CDLL("/System/Library/Frameworks/CoreServices.framework/CoreServices")
        self.lib = C.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)

        def bind(lib, name, result, *args):
            function = getattr(lib, name)
            function.restype = result
            function.argtypes = list(args)
            setattr(self, name, function)

        ptr = C.c_void_p
        bind(self.cf, "CFStringCreateWithFileSystemRepresentation", ptr, ptr, C.c_char_p)
        bind(self.cf, "CFStringGetCString", C.c_ubyte, ptr, ptr, C.c_long, C.c_uint32)
        bind(self.cf, "CFUUIDCreateString", ptr, ptr, ptr)
        bind(self.cf, "CFArrayCreate", ptr, ptr, C.POINTER(ptr), C.c_long, ptr)
        bind(self.cf, "CFRelease", None, ptr)
        bind(self.fs, "FSEventsCopyUUIDForDevice", ptr, C.c_int32)
        bind(self.fs, "FSEventsGetLastEventIdForDeviceBeforeTime", C.c_uint64, C.c_int32, C.c_double)
        bind(self.fs, "FSEventsGetCurrentEventId", C.c_uint64)
        bind(self.fs, "FSEventStreamCreateRelativeToDevice", ptr, ptr, _Callback,
             ptr, C.c_int32, ptr, C.c_uint64, C.c_double, C.c_uint32)
        bind(self.fs, "FSEventStreamSetDispatchQueue", None, ptr, ptr)
        bind(self.fs, "FSEventStreamStart", C.c_ubyte, ptr)
        bind(self.fs, "FSEventStreamFlushSync", None, ptr)
        bind(self.fs, "FSEventStreamStop", None, ptr)
        bind(self.fs, "FSEventStreamInvalidate", None, ptr)
        bind(self.fs, "FSEventStreamRelease", None, ptr)
        bind(self.lib, "dispatch_queue_create", ptr, C.c_char_p, ptr)
        bind(self.lib, "dispatch_sync_f", None, ptr, ptr, _DispatchFunction)
        bind(self.lib, "dispatch_release", None, ptr)
        # x86_64 retains the legacy statfs symbol as well as the inode64 symbol.
        name = "fstatfs$INODE64" if hasattr(self.lib, "fstatfs$INODE64") else "fstatfs"
        self.fstatfs = getattr(self.lib, name)
        self.fstatfs.argtypes = [C.c_int, C.POINTER(_StatFS)]
        self.fstatfs.restype = C.c_int

    def uuid(self, device: int) -> str:
        uuid = self.FSEventsCopyUUIDForDevice(device)
        if not uuid:
            raise OSError("FSEvents UUID unavailable for volume")
        string = None
        try:
            string = self.CFUUIDCreateString(None, uuid)
            buffer = C.create_string_buffer(64)
            if not string or not self.CFStringGetCString(string, buffer, len(buffer), 0x08000100):
                raise OSError("Cannot decode FSEvents volume UUID")
            return buffer.value.decode("ascii")
        finally:
            if string:
                self.CFRelease(string)
            self.CFRelease(uuid)


@lru_cache(maxsize=1)
def _native() -> _Native:
    return _Native()


def _beneath(path: str, root: str) -> bool:
    return path == root or path.startswith(root.rstrip(os.sep) + os.sep)


def _root_info(root: str) -> tuple[int, str, str]:
    """Return device, physical mount, and volume-relative root without walking."""
    native = _native()
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        stat = os.fstat(fd)
        physical = os.fsdecode(fcntl.fcntl(fd, _GETPATH_NOFIRMLINK, bytes(1024)).split(b"\0", 1)[0])
        filesystem = _StatFS()
        if native.fstatfs(fd, C.byref(filesystem)) != 0:
            error = C.get_errno()
            raise OSError(error, os.strerror(error), root)
        mount = os.fsdecode(filesystem.f_mntonname)
        if not mount:
            raise OSError("Cannot map watched root to its physical volume")
        # statfs may name an external mount through /Volumes, while F_GETPATH
        # reports /System/Volumes/Data/Volumes. Resolve both through descriptors.
        mount_fd = os.open(mount, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        try:
            mount_stat = os.fstat(mount_fd)
            if mount_stat.st_dev != stat.st_dev:
                raise OSError("Volume mount changed while discovering watched root")
            mount = os.fsdecode(fcntl.fcntl(mount_fd, _GETPATH_NOFIRMLINK, bytes(1024)).split(b"\0", 1)[0])
            resolved_mount = os.stat(mount)
            if ((resolved_mount.st_dev, resolved_mount.st_ino)
                    != (mount_stat.st_dev, mount_stat.st_ino)):
                raise OSError("Volume mount changed while discovering watched root")
        finally:
            os.close(mount_fd)
        if not mount or not _beneath(physical, mount):
            raise OSError("Cannot map watched root to its physical volume")
        relative = os.path.relpath(physical, mount)
        if relative == ".":
            relative = ""
        mapped = os.stat(os.path.join(mount, relative))
        if (mapped.st_dev, mapped.st_ino) != (stat.st_dev, stat.st_ino):
            raise OSError("Watched root changed while discovering volume")
        return stat.st_dev, mount, relative
    finally:
        os.close(fd)


def discover_volumes(roots: Iterable[str]) -> list[Volume]:
    """Discover one device stream and independent cursor identity per root.

    FSEventStreamCreateRelativeToDevice accepts exactly one path even though its
    parameter is a CFArray. Roots on the same device must not share a cursor:
    one stream can deliver newer events before another catches up.
    """
    streams: list[Volume] = []
    identities: dict[str, tuple[int, str]] = {}
    seen: set[str] = set()
    for supplied in roots:
        root = os.path.abspath(os.fspath(supplied))
        if root in seen:
            continue
        seen.add(root)
        device, mount, _ = _root_info(root)
        uuid = _native().uuid(device)
        previous = identities.setdefault(uuid, (device, mount))
        if previous != (device, mount):
            raise OSError("Volume identity changed during discovery")
        key = uuid + ":" + hashlib.sha256(os.fsencode(root)).hexdigest()
        streams.append(Volume(key, device, uuid, mount, (root,)))
    return streams


class Stream:
    """One serial native stream. Call lifecycle methods outside its callback.

    Callbacks run on a native dispatch thread and must return promptly. All data
    passed to them is copied. ``error`` latches the first callback/start failure;
    subsequent callbacks are withheld so a failing consumer cannot skip work.
    A stopped instance is closed; recreate it with the durable cursor to resume.
    """

    def __init__(self, volume: Volume, since: int | None,
                 callback: Callable[[list[Event]], None], latency: float = 0.5):
        if len(volume.roots) != 1:
            raise ValueError("A per-device FSEvents stream requires exactly one root")
        if since is not None and (not isinstance(since, int) or not 0 <= since < 2**64 - 1):
            raise ValueError("since must be an unsigned event cursor or None")
        if not math.isfinite(latency) or latency < 0:
            raise ValueError("latency must be finite and nonnegative")
        self.volume = volume
        self.since = since
        self.callback = callback
        self.latency = latency
        self.start_id: int | None = None
        self.error: BaseException | None = None
        self._lock = threading.Lock()
        self._callback_thread: int | None = None
        self._stream = None
        self._queue = None
        self._array = None
        self._strings: list[int] = []
        self._scheduled = False
        self._started = False
        self._closed = False
        self._mapping: list[tuple[str, str]] = []
        self._callback = _Callback(self._receive)

    def _translate(self, path: str, flags: int, event_id: int) -> list[Event]:
        if flags & _CONTROL:
            return [Event("", flags, event_id)]
        # The native stream supplies paths relative to the physical volume.
        relative = path.lstrip("/")
        if ".." in relative.split("/"):
            raise ValueError("FSEvents supplied a path outside its volume")
        physical = os.path.normpath(os.path.join(self.volume.mount, relative))
        result = []
        for logical, watched in self._mapping:
            if _beneath(physical, watched):
                suffix = os.path.relpath(physical, watched)
                mapped = logical if suffix == "." else os.path.join(logical, suffix)
                event = Event(mapped, flags, event_id)
                if event not in result:
                    result.append(event)
            elif _beneath(watched, physical):
                # Coalesced parent notifications cover every watched descendant.
                result.append(Event(logical, flags | MUST_SCAN_SUBDIRS, event_id))
        if not result and flags & (ROOT_CHANGED | MUST_SCAN_SUBDIRS | MOUNT | UNMOUNT):
            result.append(Event("", flags, event_id))
        return result

    def _receive(self, _stream, _info, count, paths, flags, ids):
        if self.error is not None:
            return
        self._callback_thread = threading.get_ident()
        try:
            raw_paths = C.cast(paths, C.POINTER(C.c_char_p))
            batch = []
            for i in range(count):
                path = os.fsdecode(raw_paths[i] or b"")
                batch.extend(self._translate(path, int(flags[i]), int(ids[i])))
            if batch:
                self.callback(batch)
        except BaseException as error:
            # Exceptions must not escape through ctypes (which otherwise logs
            # and discards them, silently losing durable ingestion failures).
            self.error = error
        finally:
            self._callback_thread = None

    def _check_thread(self):
        if self._callback_thread == threading.get_ident():
            raise RuntimeError("Stream lifecycle cannot run inside its callback")

    def start(self):
        self._check_thread()
        with self._lock:
            if self._started:
                return
            if self._closed:
                raise RuntimeError("Create a new Stream with the durable cursor to resume")
            native = None
            try:
                native = _native()
                if native.uuid(self.volume.device) != self.volume.uuid:
                    raise CursorInvalidError("FSEvents volume UUID changed; reconcile the root")
                if self.since is not None and self.since > native.FSEventsGetCurrentEventId():
                    raise CursorInvalidError("Saved event cursor exceeds current history; reconcile the root")
                relatives = []
                for logical in self.volume.roots:
                    device, mount, relative = _root_info(logical)
                    if (device, mount) != (self.volume.device, self.volume.mount):
                        raise CursorInvalidError("Watched root moved to another volume; reconcile the root")
                    self._mapping.append((logical, os.path.join(mount, relative).rstrip("/") or "/"))
                    relatives.append(relative)
                # Apple's current SDK specifies POSIX seconds here. This may
                # replay extra buffered history; it must not skip initial changes.
                self.start_id = self.since if self.since is not None else int(
                    native.FSEventsGetLastEventIdForDeviceBeforeTime(self.volume.device, time.time()))
                for relative in relatives:
                    string = native.CFStringCreateWithFileSystemRepresentation(None, os.fsencode(relative))
                    if not string:
                        raise OSError("Cannot encode FSEvents watch path")
                    self._strings.append(string)
                values = (C.c_void_p * len(self._strings))(*self._strings)
                self._array = native.CFArrayCreate(None, values, len(values), None)
                if not self._array:
                    raise OSError("Cannot allocate FSEvents watch path array")
                self._stream = native.FSEventStreamCreateRelativeToDevice(
                    None, self._callback, None, self.volume.device, self._array,
                    self.start_id, self.latency, _WATCH_ROOT | _IGNORE_SELF | _FILE_EVENTS)
                if not self._stream:
                    raise OSError("Cannot create FSEvents stream")
                self._queue = native.dispatch_queue_create(b"jaso-nfc.events", None)
                if not self._queue:
                    raise OSError("Cannot create FSEvents dispatch queue")
                native.FSEventStreamSetDispatchQueue(self._stream, self._queue)
                self._scheduled = True
                if not native.FSEventStreamStart(self._stream):
                    raise OSError("Cannot start FSEvents stream")
                self._started = True
            except BaseException as error:
                self.error = error
                if native is not None:
                    self._cleanup(native)
                self._closed = True
                raise

    def flush(self):
        """Deliver buffered native events; the consumer owns durable completion."""
        self._check_thread()
        with self._lock:
            if self._started:
                _native().FSEventStreamFlushSync(self._stream)

    def _cleanup(self, native):
        if self._started:
            native.FSEventStreamStop(self._stream)
            self._started = False
        if self._scheduled:
            native.FSEventStreamInvalidate(self._stream)
            self._scheduled = False
        if self._queue:
            # Drain any already-dispatched work before releasing callbacks and
            # native objects. The queue is serial and this runs outside it.
            barrier = _DispatchFunction(lambda _context: None)
            native.dispatch_sync_f(self._queue, None, barrier)
        if self._stream:
            native.FSEventStreamRelease(self._stream)
            self._stream = None
        if self._queue:
            native.dispatch_release(self._queue)
            self._queue = None
        if self._array:
            native.CFRelease(self._array)
            self._array = None
        for string in self._strings:
            native.CFRelease(string)
        self._strings.clear()

    def stop(self):
        """Stop delivery, drain the native queue, and release owned resources."""
        self._check_thread()
        with self._lock:
            if self._closed:
                return
            if self._stream or self._queue or self._array or self._strings:
                self._cleanup(_native())
            self._closed = True
