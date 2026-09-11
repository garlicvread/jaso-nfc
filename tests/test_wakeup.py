"""The resident worker sleeps until native delivery, control, or a real deadline."""
import os
from pathlib import Path
import signal
import tempfile
import threading
import time
import types
import unittest
from unittest import mock

from jaso_nfc import service
from jaso_nfc.config import Config
from jaso_nfc.events import Volume
from jaso_nfc.index import Index
from jaso_nfc.normalizer import Policy


class WakeupTests(unittest.TestCase):
    def test_idle_worker_has_no_polling_deadline(self):
        index = types.SimpleNamespace(next_wakeup=lambda: None)
        normalizer = types.SimpleNamespace(retry=types.SimpleNamespace(entries={}))
        sources = types.SimpleNamespace(next_retry_time=None)
        self.assertIsNone(service.idle_timeout(index, normalizer, sources))

    def test_earliest_real_deadline_controls_sleep(self):
        index = types.SimpleNamespace(next_wakeup=lambda: 140)
        normalizer = types.SimpleNamespace(
            policy=types.SimpleNamespace(accepts=lambda path: path == "active"),
            retry=types.SimpleNamespace(entries={"active": {"next_retry": 130},
                                                "excluded": {"next_retry": 50}}))
        sources = types.SimpleNamespace(next_retry_time=150)
        with mock.patch.object(service.time, "time", return_value=100):
            self.assertEqual(service.idle_timeout(index, normalizer, sources), 30)

    def test_parent_scan_backoff_wins_over_an_already_handled_rename_deadline(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = str(Path(tmp) / "root")
            Path(root).mkdir()
            normalizer = types.SimpleNamespace(
                policy=Policy([root]),
                retry=types.SimpleNamespace(entries={root + "/file": {"next_retry": 90}}),
                retry_paths=lambda: [root],
                reconcile=lambda path, recursive: dict(entries=[], directories=[], renamed=0,
                    errors=[{"path": path, "error": "permission denied"}]))
            index = Index(Path(tmp) / "index.sqlite3")
            self.addCleanup(index.close)
            index.bind_policy(normalizer.policy)
            index.configure("fixture", [Volume("v", 1, "uuid", "/", (root,))], [root])
            index.bootstrap_jobs()
            sources = types.SimpleNamespace(next_retry_time=None)
            with mock.patch.object(service.time, "time", return_value=100):
                self.assertTrue(index.work(normalizer))
                self.assertFalse(index.work(normalizer))
                self.assertEqual(service.idle_timeout(
                    index, normalizer, sources, retry_checked_at=100), 2)

    def test_rename_deadline_crossed_after_work_still_wakes_immediately(self):
        index = types.SimpleNamespace(next_wakeup=lambda: None)
        normalizer = types.SimpleNamespace(
            policy=types.SimpleNamespace(accepts=lambda path: True),
            retry=types.SimpleNamespace(entries={"file": {"next_retry": 100.5}}))
        sources = types.SimpleNamespace(next_retry_time=None)
        with mock.patch.object(service.time, "time", return_value=101):
            self.assertEqual(service.idle_timeout(
                index, normalizer, sources, retry_checked_at=100), 0)

    def test_native_and_external_signals_wake_and_drain_without_polling(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "wake.fifo"
            with service.Wakeup(path) as wake:
                done = threading.Event()
                worker = threading.Thread(target=lambda: (wake.wait(None), done.set()))
                worker.start()
                self.assertFalse(done.wait(0.05))
                self.assertTrue(service.signal_wakeup(path))
                self.assertTrue(done.wait(2))
                worker.join(2)
                wake.clear()
                self.assertFalse(wake.wait(0))
                wake.set()
                self.assertTrue(wake.wait(0))
                wake.clear()
                self.assertFalse(wake.wait(0))
            self.assertFalse(path.exists())
            self.assertFalse(service.signal_wakeup(path))

    def test_wakeup_never_overwrites_a_regular_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "wake.fifo"
            path.write_text("preserve")
            with self.assertRaises(OSError):
                with service.Wakeup(path):
                    pass
            self.assertFalse(service.signal_wakeup(path))
            self.assertEqual(path.read_text(), "preserve")

    def test_worker_does_not_sleep_after_draining_a_stop_signal(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Config(roots=[tmp], state_dir=str(Path(tmp) / "state"))
            stop = threading.Event()
            index = mock.Mock()
            index.work.return_value = False
            index.next_wakeup.return_value = None
            index.status.return_value = {}
            normalizer = mock.Mock()
            normalizer.retry.entries = {}
            sources = mock.Mock(active_roots=(), next_retry_time=None)
            sources.refresh_requested = threading.Event()
            original_clear = service.Wakeup.clear

            def stop_before_clear(wake):
                signal.getsignal(signal.SIGTERM)(signal.SIGTERM, None)
                original_clear(wake)

            with mock.patch.object(service.sys, "platform", "darwin"), \
                    mock.patch.object(service, "make_normalizer", return_value=normalizer), \
                    mock.patch("jaso_nfc.index.Index", return_value=index), \
                    mock.patch("jaso_nfc.sources.SourceManager", return_value=sources), \
                    mock.patch.object(service.Wakeup, "clear", stop_before_clear), \
                    mock.patch.object(service.Wakeup, "wait", side_effect=AssertionError(
                        "worker slept after receiving its stop signal")):
                service.watch(config, stop_event=stop)
            self.assertTrue(stop.is_set())
            self.assertFalse(config.state_path("wake.fifo").exists())
            sources.close.assert_called_once()

    def test_external_notification_handles_worker_closing_after_open(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "wake.fifo"
            os.mkfifo(path, 0o600)
            reader = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
            original_open = os.open

            def close_worker_after_open(*args, **kwargs):
                descriptor = original_open(*args, **kwargs)
                os.close(reader)
                return descriptor

            with mock.patch.object(service.os, "open", side_effect=close_worker_after_open):
                self.assertFalse(service.signal_wakeup(path))


if __name__ == "__main__":
    unittest.main()
