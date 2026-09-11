"""Resolve Darwin's stored filename when directory listings use another form."""
from __future__ import annotations

import errno
import ctypes as C
import fcntl
from functools import lru_cache
import os
import sys
import unicodedata


def _validate_name(name):
    if not name or name in (".", "..") or os.path.basename(name) != name:
        raise ValueError("Expected a single directory entry name")


def open_entry(parent_fd: int, name: str) -> int:
    """Open an entry for metadata without following its final symbolic link."""
    _validate_name(name)
    # Darwin sys/fcntl.h: O_EVTONLY, O_SYMLINK, and F_GETPATH.
    flags = ((0x00008000 | 0x00200000) if sys.platform == "darwin"
             else (os.O_RDONLY | os.O_NOFOLLOW)) | os.O_CLOEXEC
    try:
        return os.open(name, flags, dir_fd=parent_fd)
    except FileNotFoundError:
        candidate = unicodedata.normalize("NFC", name)
        if sys.platform != "darwin" or candidate == name:
            raise
        return os.open(candidate, flags, dir_fd=parent_fd)


def actual_stored_name(parent_fd: int, name: str) -> str:
    """Return an entry's descriptor name without reading data or following links.

    Directory enumeration can present decomposed spellings on exFAT even when
    F_GETPATH reports a composed filename. Confirm the resolved entry remains
    the same object under the supplied parent before returning its spelling.
    """
    _validate_name(name)
    if sys.platform != "darwin":
        return name
    descriptor = open_entry(parent_fd, name)
    try:
        path = os.fsdecode(fcntl.fcntl(descriptor, 50, bytes(1024)).split(b"\0", 1)[0])
        actual = os.path.basename(path)
        if not actual or actual in (".", ".."):
            raise OSError(errno.EIO, "Native descriptor returned an invalid entry name", name)
        opened = os.fstat(descriptor)
        mapped = os.stat(actual, dir_fd=parent_fd, follow_symlinks=False)
        if (opened.st_dev, opened.st_ino) != (mapped.st_dev, mapped.st_ino):
            raise OSError(errno.ESTALE, "Directory entry changed while resolving its stored name", name)
        return actual
    finally:
        os.close(descriptor)


@lru_cache(maxsize=1)
def _xattrs():
    library = C.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
    arguments = [C.c_int, C.c_char_p, C.c_void_p, C.c_size_t, C.c_uint32, C.c_int]
    library.fgetxattr.argtypes = arguments
    library.fgetxattr.restype = C.c_ssize_t
    library.fsetxattr.argtypes = arguments
    library.fsetxattr.restype = C.c_int
    library.fremovexattr.argtypes = [C.c_int, C.c_char_p, C.c_int]
    library.fremovexattr.restype = C.c_int
    return library


def _checked(result):
    if result < 0:
        error = C.get_errno()
        raise OSError(error, os.strerror(error))
    return result


def marker_get(descriptor: int, key: str) -> bytes | None:
    """Read a bounded operation marker from the held entry, including symlinks."""
    try:
        if sys.platform != "darwin":
            return os.getxattr(descriptor, key)
        library = _xattrs()
        encoded = os.fsencode(key)
        size = _checked(library.fgetxattr(descriptor, encoded, None, 0, 0, 0))
        if size > 4096:
            raise OSError(errno.E2BIG, "Operation marker exceeds its size limit")
        buffer = C.create_string_buffer(max(size, 1))
        count = _checked(library.fgetxattr(descriptor, encoded, buffer, size, 0, 0))
        return buffer.raw[:count]
    except OSError as error:
        if error.errno in (getattr(errno, "ENOATTR", 93), getattr(errno, "ENODATA", 61)):
            return None
        raise


def marker_create(descriptor: int, key: str, token: bytes):
    """Create a marker exclusively and return metadata after the mutation."""
    if not token or len(token) > 4096:
        raise ValueError("Operation marker must contain between 1 and 4096 bytes")
    if sys.platform == "darwin":
        # XATTR_CREATE rejects an existing attribute; the held fd never follows links.
        _checked(_xattrs().fsetxattr(descriptor, os.fsencode(key), token, len(token), 0, 2))
    else:
        os.setxattr(descriptor, key, token, flags=os.XATTR_CREATE)
    return os.fstat(descriptor)


def marker_remove(descriptor: int, key: str, token: bytes):
    """Remove only the matching marker; absent markers make cleanup idempotent."""
    current = marker_get(descriptor, key)
    if current is not None:
        if current != token:
            raise OSError(errno.ESTALE, "Operation marker belongs to a different operation")
        if sys.platform == "darwin":
            _checked(_xattrs().fremovexattr(descriptor, os.fsencode(key), 0))
        else:
            os.removexattr(descriptor, key)
    return os.fstat(descriptor)
