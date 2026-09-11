"""Discover user-data roots through fixed catalogs and metadata-only probes.

Discovery never walks document trees or reads file contents. Unreadable known
roots remain in the result so a service can report and retry their coverage.
"""
from dataclasses import dataclass, field
import os
from pathlib import Path
import pwd
import stat


_VOLUME_METADATA = (
    ".DocumentRevisions-V100", ".HFS+ Private Directory Data\r", ".Spotlight-V100",
    ".TemporaryItems", ".Trashes", ".fseventsd", ".vol",
)
_BOOT_INTERNALS = ("Applications", "Library", "System", "bin", "dev", "private", "sbin", "usr")
_NON_LOGIN_SHELLS = frozenset(("/usr/bin/false", "/bin/false", "/sbin/nologin", "/usr/sbin/nologin"))
_NON_USER_HOMES = frozenset(("/", "/var/empty", "/private/var/empty", "/dev/null", "/nonexistent"))


@dataclass(frozen=True)
class Coverage:
    """Discovery snapshot; generated exclusions apply only to their own root."""
    roots: tuple[str, ...]
    excludes: tuple[str, ...]
    catalog_roots: tuple[str, ...]
    unavailable: tuple[str, ...]
    root_excludes: dict[str, tuple[str, ...]] = field(default_factory=dict)


def _absolute(path):
    return os.path.abspath(os.fspath(path))


def _startup_devices():
    devices = set()
    for path in ("/", "/System/Volumes/Data"):
        try:
            devices.add(os.stat(path).st_dev)
        except OSError:
            pass
    return devices


def discover_user_coverage(users_dir=Path("/Users"), volumes_dir=Path("/Volumes"),
                           *, account_entries=None, startup_devices=None):
    """Find account, shared, cloud, and mounted-volume roots without recursion.

    ``account_entries`` and ``startup_devices`` allow isolated catalogs in tests.
    Omit them in the service to query the account database and startup devices.
    ``excludes`` is a reporting union; pass ``root_excludes`` to Policy so an
    explicit cloud-data root does not inherit its home's Library exclusion.
    """
    users_dir, volumes_dir = _absolute(users_dir), _absolute(volumes_dir)
    devices = _startup_devices() if startup_devices is None else set(startup_devices)
    roots, catalogs, unavailable = set(), set(), set()
    scoped = {}
    homes_seen = set()

    def info(path):
        try:
            return os.lstat(path)
        except FileNotFoundError:
            return None
        except OSError:
            unavailable.add(path)
            return None

    def directory(path):
        value = info(path)
        return value is not None and stat.S_ISDIR(value.st_mode)

    def readable(path):
        try:
            # Opening the directory tests access without enumerating its tree.
            with os.scandir(path):
                return True
        except OSError:
            unavailable.add(path)
            return False

    def catalog(path):
        if not directory(path):
            return []
        catalogs.add(path)
        found = []
        try:
            with os.scandir(path) as entries:
                for entry in entries:
                    if entry.name.startswith("."):
                        continue
                    try:
                        value = entry.stat(follow_symlinks=False)
                    except OSError:
                        unavailable.add(entry.path)
                        continue
                    if stat.S_ISDIR(value.st_mode):
                        found.append((entry.path, entry.name, value))
        except OSError:
            unavailable.add(path)
        return sorted(found, key=lambda item: item[0])

    def home(path, shared=False):
        if path in homes_seen or not directory(path):
            return
        homes_seen.add(path)
        roots.add(path)
        if shared:
            readable(path)
            return
        catalogs.add(path)  # Watch creation of a previously absent Library.
        scoped.setdefault(path, set()).update((os.path.join(path, "Library"), os.path.join(path, ".Trash")))
        readable(path)
        library = os.path.join(path, "Library")
        if not directory(library):
            return
        catalogs.add(library)
        readable(library)
        for relative in ("Library/CloudStorage", "Library/Mobile Documents/com~apple~CloudDocs"):
            cloud = os.path.join(path, relative)
            if relative.endswith("com~apple~CloudDocs"):
                parent = os.path.dirname(cloud)
                if not directory(parent):
                    continue
                catalogs.add(parent)  # CloudDocs may appear after Mobile Documents.
                readable(parent)
            if directory(cloud):
                roots.add(cloud)
                readable(cloud)

    for path, name, _value in catalog(users_dir):
        home(path, shared=name == "Shared")

    if account_entries is None:
        try:
            account_entries = pwd.getpwall()
        except OSError:
            unavailable.add(users_dir)
            account_entries = ()
    for account in account_entries:
        name = getattr(account, "pw_name", "")
        candidate = getattr(account, "pw_dir", "")
        shell = getattr(account, "pw_shell", "")
        if (getattr(account, "pw_uid", -1) < 500 or name.startswith("_")
                or shell in _NON_LOGIN_SHELLS or not os.path.isabs(candidate)):
            continue
        candidate = _absolute(candidate)
        if candidate not in _NON_USER_HOMES:
            home(candidate)

    for path, _name, value in catalog(volumes_dir):
        if value.st_dev in devices:
            continue
        try:
            mounted = os.path.ismount(path)
        except OSError:
            unavailable.add(path)
            continue
        if not mounted:
            continue
        roots.add(path)
        exclusions = scoped.setdefault(path, set())
        exclusions.update(os.path.join(path, name) for name in _VOLUME_METADATA)
        readable(path)
        marker = os.path.join(path, "System/Library/CoreServices/SystemVersion.plist")
        marker_info = info(marker)
        if marker_info is not None and stat.S_ISREG(marker_info.st_mode):
            exclusions.update(os.path.join(path, name) for name in _BOOT_INTERNALS)
            external_users = os.path.join(path, "Users")
            for account_home, name, _value in catalog(external_users):
                home(account_home, shared=name == "Shared")

    root_excludes = {path: tuple(sorted(exclusions)) for path, exclusions in sorted(scoped.items())}
    flat = {path for exclusions in root_excludes.values() for path in exclusions}
    return Coverage(tuple(sorted(roots)), tuple(sorted(flat)), tuple(sorted(catalogs)),
                    tuple(sorted(unavailable)), root_excludes)
