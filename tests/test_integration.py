"""End-to-end native events, normalization, index persistence, and restart."""

import json
from contextlib import closing
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time
import unicodedata
import unittest

from jaso_nfc.config import Config
from jaso_nfc.index import Index


@unittest.skipUnless(sys.platform == 'darwin', 'native watcher requires macOS')
class WorkerIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.root = self.base / 'watched'
        self.root.mkdir()
        self.config = Config(roots=[str(self.root)], excludes=[], skip_hidden_tops=[],
                             state_dir=str(self.base / 'support'), apply=True)
        self.config_path = self.base / 'config.json'
        self.config.save(self.config_path)
        self.process = None
        self.addCleanup(self.stop_worker)

    def start_worker(self):
        environment = dict(os.environ)
        environment['PYTHONPATH'] = str(Path(__file__).resolve().parents[1] / 'src')
        self.process = subprocess.Popen(
            [sys.executable, '-m', 'jaso_nfc', 'watch', '--config', str(self.config_path)],
            env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def stop_worker(self):
        if self.process is not None:
            process, self.process = self.process, None
            process.terminate()
            try:
                stdout, stderr = process.communicate(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                stdout, stderr = process.communicate()
                self.fail('worker did not stop promptly: ' + stderr)
            self.assertEqual(process.returncode, 0, stdout + stderr)

    def status(self):
        try:
            index = Index(self.config.state_path('index.sqlite3'), read_only=True)
            try:
                return index.status()
            finally:
                index.close()
        except sqlite3.OperationalError:
            return {}

    def wait_for(self, condition, message):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if self.process and self.process.poll() is not None:
                stdout, stderr = self.process.communicate()
                self.fail('worker exited early: ' + stdout + stderr)
            if condition():
                return
            time.sleep(0.05)
        log = Path(self.config.log_dir) / 'service.log'
        self.fail(message + '\n' + (log.read_text() if log.exists() else str(self.status())))

    def idle(self):
        status = self.status()
        return status.get('baseline_complete') and status.get('pending_jobs') == 0

    def indexed_paths(self):
        with closing(sqlite3.connect(self.config.state_path('index.sqlite3'))) as connection:
            return {row[0] for row in connection.execute('SELECT path FROM entries')}

    def test_live_changes_offline_replay_and_restart_without_new_baseline(self):
        initial = unicodedata.normalize('NFD', '처음.txt')
        (self.root / initial).write_text('initial bytes')
        self.start_worker()
        self.wait_for(lambda: self.idle() and '처음.txt' in os.listdir(self.root), 'initial normalization failed')
        self.assertEqual((self.root / '처음.txt').read_text(), 'initial bytes')

        live = unicodedata.normalize('NFD', '새파일.txt')
        (self.root / live).write_text('live bytes')
        self.wait_for(lambda: '새파일.txt' in os.listdir(self.root) and self.idle(), 'live event was lost')
        staging = self.base / 'incoming'
        staging.mkdir()
        child = unicodedata.normalize('NFD', '내용.txt')
        (staging / child).write_text('moved bytes')
        os.rename(staging, self.root / unicodedata.normalize('NFD', '묶음'))
        expected = str(self.root / '묶음/내용.txt')
        self.wait_for(lambda: expected in self.indexed_paths() and self.idle(), 'moved subtree not indexed')
        self.assertEqual(Path(expected).read_text(), 'moved bytes')

        before = self.status()
        self.assertTrue(all(cursor is not None for cursor in before['cursors'].values()))
        self.stop_worker()
        (self.root / unicodedata.normalize('NFD', '중단중.txt')).write_text('offline bytes')
        (self.root / '새파일.txt').unlink()
        self.start_worker()
        self.wait_for(lambda: '중단중.txt' in os.listdir(self.root) and self.idle(), 'offline replay was lost')
        self.wait_for(lambda: str(self.root / '새파일.txt') not in self.indexed_paths(), 'deleted file remains indexed')
        after = self.status()
        self.assertEqual(after['baseline_walks'], before['baseline_walks'])
        self.assertEqual((self.root / '중단중.txt').read_text(), 'offline bytes')

    def test_cli_preview_does_not_create_runtime_or_mutate(self):
        name = unicodedata.normalize('NFD', '미리보기.txt')
        (self.root / name).write_text('preview')
        environment = dict(os.environ)
        environment['PYTHONPATH'] = str(Path(__file__).resolve().parents[1] / 'src')
        result = subprocess.run([sys.executable, '-m', 'jaso_nfc', 'scan', '--config', str(self.config_path)],
                                env=environment, capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)['mode'], 'scan')
        self.assertIn(name, os.listdir(self.root))
        self.assertFalse(Path(self.config.state_dir).exists())

    def test_control_request_wakes_an_idle_worker_without_file_events(self):
        self.start_worker()
        self.wait_for(self.idle, 'worker failed to become idle')
        time.sleep(0.1)
        before = self.status()['shallow_scans'] + self.status()['subtree_scans']
        environment = dict(os.environ)
        environment['PYTHONPATH'] = str(Path(__file__).resolve().parents[1] / 'src')
        result = subprocess.run(
            [sys.executable, '-m', 'jaso_nfc', 'reconcile', '--config', str(self.config_path)],
            env=environment, capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(json.loads(result.stdout)['worker_notified'])
        self.wait_for(lambda: self.idle() and self.status()['shallow_scans']
                      + self.status()['subtree_scans'] > before,
                      'control work remained asleep without a filesystem event')

    def test_multiple_roots_deliver_live_and_offline_changes(self):
        second = self.base / 'second-root'
        second.mkdir()
        self.config.roots.append(str(second))
        self.config.save(self.config_path)
        self.start_worker()
        self.wait_for(self.idle, 'multiple-root watcher failed to start')
        for root in (self.root, second):
            (root / unicodedata.normalize('NFD', '여러루트.txt')).write_text('live')
        self.wait_for(lambda: all('여러루트.txt' in os.listdir(root) for root in (self.root, second))
                      and self.idle(), 'a configured root lost live coverage')
        before = self.status()['baseline_walks']
        self.stop_worker()
        for root in (self.root, second):
            (root / unicodedata.normalize('NFD', '재시작.txt')).write_text('offline')
        self.start_worker()
        self.wait_for(lambda: all('재시작.txt' in os.listdir(root) for root in (self.root, second))
                      and self.idle(), 'a configured root lost historical coverage')
        self.assertEqual(self.status()['baseline_walks'], before)


if __name__ == '__main__':
    unittest.main()
