"""Continuous worker and user LaunchAgent installation."""

import contextlib
import fcntl
import hashlib
import json
import logging
from logging.handlers import RotatingFileHandler
import os
from pathlib import Path
import plistlib
import shutil
import signal
import select
import stat
import subprocess
import sys
import tempfile
import threading
import time

from . import __version__

LABEL = 'io.github.garlicvread.jaso-nfc'
LOGGER = logging.getLogger('jaso_nfc')
INSTALL_STOP_TIMEOUT = 15.0


class Wakeup:
    """A local FIFO wakes the single worker without a polling/helper thread."""

    def __init__(self, path):
        self.path = Path(path)
        self.fd = None

    def __enter__(self):
        try:
            os.mkfifo(self.path, 0o600)
        except FileExistsError:
            pass
        descriptor = os.open(self.path, os.O_RDWR | os.O_NONBLOCK | os.O_NOFOLLOW)
        info = os.fstat(descriptor)
        if not stat.S_ISFIFO(info.st_mode):
            os.close(descriptor)
            raise OSError('worker wakeup path is not a FIFO')
        self.fd = descriptor
        self.identity = (info.st_dev, info.st_ino)
        return self

    def set(self):
        try:
            os.write(self.fd, b'\0')
        except BlockingIOError:
            pass  # Buffered notifications already make the descriptor readable.

    def clear(self):
        while True:
            try:
                if not os.read(self.fd, 4096):
                    return
            except BlockingIOError:
                return

    def wait(self, timeout=None):
        return bool(select.select([self.fd], [], [], timeout)[0])

    def __exit__(self, *_):
        os.close(self.fd)
        self.fd = None
        try:
            info = self.path.lstat()
            if stat.S_ISFIFO(info.st_mode) and (info.st_dev, info.st_ino) == self.identity:
                self.path.unlink()
        except FileNotFoundError:
            pass


def signal_wakeup(path):
    """Notify an installed worker after another process commits control work."""
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    except OSError:
        return False
    try:
        if not stat.S_ISFIFO(os.fstat(descriptor).st_mode):
            return False
        try:
            os.write(descriptor, b'\0')
        except BlockingIOError:
            pass
        except BrokenPipeError:
            return False  # The worker exited after the writer opened the FIFO.
        return True
    finally:
        os.close(descriptor)


def idle_timeout(index, normalizer, sources, *, retry_checked_at=None):
    """Wait indefinitely when idle; wake only for a concrete retry deadline."""
    deadlines = [index.next_wakeup(), sources.next_retry_time]
    deadlines.extend(record['next_retry'] for path, record in normalizer.retry.entries.items()
                     if normalizer.policy.accepts(path) and (
                         retry_checked_at is None or record['next_retry'] > retry_checked_at))
    earliest = min((value for value in deadlines if value is not None), default=None)
    return None if earliest is None else max(0, earliest - time.time())


def prepare_directories(config):
    for path in (config.state_dir, config.log_dir, config.state_path('index.sqlite3').parent):
        Path(path).mkdir(parents=True, exist_ok=True, mode=0o700)


@contextlib.contextmanager
def runtime_lock(config, *, timeout=0):
    prepare_directories(config)
    with open(Path(config.state_dir) / '.lock', 'a') as handle:
        deadline = time.monotonic() + timeout
        while True:
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError as exc:
                if timeout <= 0:
                    raise RuntimeError('jaso-nfc is already running; stop it before this operation') from exc
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError('Timed out waiting for the stopped worker to release its runtime lock') from exc
                time.sleep(min(0.05, remaining))
        yield


def make_normalizer(config):
    from .normalizer import Normalizer
    return Normalizer(config.policy(), str(Path(config.log_dir) / 'renames.jsonl'),
                      str(config.state_path('skip.json')),
                      str(config.state_path('pending.json')), apply=config.apply)


def runtime_status(config):
    """Inspect the worker lock and deferred rename queue without creating files."""
    running = False
    lock = Path(config.state_dir) / '.lock'
    if lock.exists():
        with lock.open('r') as handle:
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                running = True
    retries = config.state_path('skip.json')
    entries = json.loads(retries.read_text(encoding='utf-8'))['entries'] if retries.exists() else {}
    path = config.state_path('coverage.json')
    coverage = json.loads(path.read_text(encoding='utf-8')) if path.exists() else {}
    return {'running': running, 'apply': config.apply, 'scope': config.scope,
            'roots': coverage.get('roots', config.roots),
            'active_roots': coverage.get('active_roots', []),
            'unavailable_roots': coverage.get('unavailable', {}),
            'catalog_unavailable': coverage.get('catalog_unavailable', {}),
            'deferred_renames': len(entries),
            'next_rename_retry': min((r['next_retry'] for r in entries.values()), default=None),
            'pending_recovery': config.state_path('pending.json').exists()}


def watch(config, stop_event=None):
    from .index import Index
    from .sources import SourceManager
    if sys.platform != 'darwin':
        raise RuntimeError('native watching requires macOS')
    stop = stop_event or threading.Event()
    wake = threading.Event()
    previous_handlers = {}
    if threading.current_thread() is threading.main_thread():
        def request_stop(signum, frame):
            stop.set()
            wake.set()
        for sig in (signal.SIGTERM, signal.SIGINT):
            previous_handlers[sig] = signal.signal(sig, request_stop)
    try:
        with runtime_lock(config), Wakeup(config.state_path('wake.fifo')) as wake:
            handler = RotatingFileHandler(Path(config.log_dir) / 'service.log',
                                          maxBytes=1024 * 1024, backupCount=3)
            handler.setFormatter(logging.Formatter('%(asctime)s %(levelname)s %(message)s'))
            LOGGER.addHandler(handler)
            LOGGER.setLevel(logging.INFO)
            normalizer = make_normalizer(config)
            index = Index(config.state_path('index.sqlite3'))
            sources = SourceManager(config, index, normalizer, wake)
            try:
                sources.refresh()
                normalizer.recover()
                LOGGER.info('watch started: scope=%s roots=%d apply=%s',
                            config.scope, len(sources.active_roots), config.apply)
                while not stop.is_set():
                    wake.clear()
                    if stop.is_set():
                        break
                    sources.check()
                    if sources.refresh_requested.is_set():
                        sources.refresh()
                    try:
                        # Due rename retries become indexed parent jobs, whose
                        # enumeration backoff must govern subsequent sleeping.
                        retry_checked_at = time.time()
                        if index.work(normalizer):
                            continue
                    except RuntimeError:
                        if not index.status()['needs_revalidation']:
                            raise
                        sources.refresh(force=True)
                        continue
                    wake.wait(idle_timeout(index, normalizer, sources,
                                           retry_checked_at=retry_checked_at))
                LOGGER.info('watch stopped: %s', index.status())
            finally:
                # No callback may retain/use a closed database connection.
                sources.close()
                index.close()
                normalizer.close()
                LOGGER.removeHandler(handler)
                handler.close()
    finally:
        for sig, old in previous_handlers.items():
            signal.signal(sig, old)


def launch_agent(config, executable, runner):
    value = {
        'Label': LABEL,
        'ProgramArguments': [executable, runner, 'watch', '--config',
                             str(Path(config.state_dir) / 'config.json')],
        'RunAtLoad': True,
        'KeepAlive': True,
        'ThrottleInterval': 30,
        'ProcessType': 'Background',
        'LowPriorityIO': True,
        'Nice': 10,
        'Umask': 0o077,
        # The application owns bounded diagnostic logs. No append-only launchd files.
        'StandardOutPath': '/dev/null',
        'StandardErrorPath': '/dev/null',
        'EnvironmentVariables': {'PYTHONUNBUFFERED': '1', 'PYTHONDONTWRITEBYTECODE': '1'},
    }
    return plistlib.dumps(value, sort_keys=False)


def install_files(config, agents_dir=None):
    """Stage an immutable package copy; do not start or stop jobs here."""
    prepare_directories(config)
    source = Path(__file__).parent
    digest = hashlib.sha256()
    for path in sorted(source.glob('*.py')):
        digest.update(path.name.encode())
        digest.update(path.read_bytes())
    release = Path(config.state_dir) / 'releases' / f'{__version__}-{digest.hexdigest()[:12]}'
    release.parent.mkdir(exist_ok=True)
    if not release.exists():
        staging = Path(tempfile.mkdtemp(prefix='.install-', dir=release.parent))
        try:
            shutil.copytree(source, staging / 'jaso_nfc', ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
            (staging / 'run.py').write_text(
                'from jaso_nfc.cli import main\nraise SystemExit(main())\n', encoding='utf-8')
            os.replace(staging, release)
        finally:
            if staging.exists():
                shutil.rmtree(staging)
    config.save(Path(config.state_dir) / 'config.json')
    agents = Path(agents_dir) if agents_dir else Path.home() / 'Library/LaunchAgents'
    agents.mkdir(parents=True, exist_ok=True)
    plist = agents / f'{LABEL}.plist'
    data = launch_agent(config, sys.executable, str(release / 'run.py'))
    fd, tmp = tempfile.mkstemp(prefix='.jaso-', dir=agents)
    try:
        with os.fdopen(fd, 'wb') as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, plist)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)
    return plist


def stop_agent():
    return subprocess.run(['launchctl', 'bootout', f'gui/{os.getuid()}/{LABEL}'],
                          capture_output=True, text=True)


def stop_for_change():
    result = stop_agent()
    # launchctl reports ESRCH (3) for an already-unloaded user job.
    if result.returncode not in (0, 3, 113):
        raise RuntimeError(result.stderr.strip() or 'could not stop the LaunchAgent')


def install(config):
    if sys.platform != 'darwin':
        raise RuntimeError('LaunchAgent installation requires macOS')
    # Capture old service state for rollback on a failed bootstrap.
    plist = Path.home() / 'Library/LaunchAgents' / f'{LABEL}.plist'
    old_plist = plist.read_bytes() if plist.exists() else None
    config_path = Path(config.state_dir) / 'config.json'
    old_config = config_path.read_bytes() if config_path.exists() else None
    stop_for_change()
    staging_started = False
    try:
        with runtime_lock(config, timeout=INSTALL_STOP_TIMEOUT):
            staging_started = True
            installed = install_files(config)
        result = subprocess.run(['launchctl', 'bootstrap', f'gui/{os.getuid()}', str(installed)],
                                capture_output=True, text=True)
        if result.returncode:
            raise RuntimeError(result.stderr.strip() or 'launchctl bootstrap failed')
        return installed
    except BaseException:
        if not staging_started:
            raise
        with runtime_lock(config):
            if old_config is not None:
                config_path.write_bytes(old_config)
            if old_plist is not None:
                plist.write_bytes(old_plist)
            elif plist.exists():
                plist.unlink()
        if old_plist is not None:
            subprocess.run(['launchctl', 'bootstrap', f'gui/{os.getuid()}', str(plist)],
                           capture_output=True)
        raise


def uninstall():
    if sys.platform != 'darwin':
        raise RuntimeError('LaunchAgent removal requires macOS')
    stop_for_change()
    path = Path.home() / 'Library/LaunchAgents' / f'{LABEL}.plist'
    if path.exists():
        path.unlink()
    return path
