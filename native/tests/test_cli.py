"""Black-box tests for the native executable; all mutations use owned fixtures."""
import fcntl
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
import unicodedata

BINARY = Path(os.environ.get("JASO_NATIVE_BINARY", "target/debug/jaso-nfc")).absolute()

def stored_name(path):
    try:
        fd = os.open(path, 0x8000 | 0x200000)  # Darwin O_EVTONLY | O_SYMLINK
    except FileNotFoundError:
        fd = os.open(unicodedata.normalize("NFC", str(path)), 0x8000 | 0x200000)
    try:
        return os.path.basename(os.fsdecode(fcntl.fcntl(fd, 50, bytes(1024)).split(b"\0", 1)[0]))
    finally:
        os.close(fd)

def remove_fixture(path):
    """exFAT may enumerate NFD while only its stored NFC spelling can unlink."""
    if not path.exists():
        return
    for entry in list(os.scandir(path)):
        try:
            candidate = path / stored_name(entry.path)
        except FileNotFoundError:
            continue  # Removing a file can also remove its AppleDouble companion.
        if candidate.is_dir() and not candidate.is_symlink():
            remove_fixture(candidate)
        else:
            candidate.unlink()
    path.rmdir()

class NativeCLI(unittest.TestCase):
    def call(self, *args, config):
        result = subprocess.run([str(BINARY), *args, "--config", str(config)], capture_output=True, text=True,
                                env={**os.environ, "HOME": str(config.parent)})
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        return json.loads(result.stdout)

    def wait_for_worker_ready(self, worker, state, timeout=20):
        # The worker publishes this record after index initialization. Merely
        # seeing its PID or database file does not make status reads ready.
        deadline = time.monotonic() + timeout
        ready = {}
        while time.monotonic() < deadline:
            if worker.poll() is not None:
                self.fail("worker exited before readiness: " + worker.stderr.read())
            try:
                ready = json.loads((state / "state/runtime.json").read_text())
            except FileNotFoundError:
                ready = {}
            if ready.get("pid") == worker.pid:
                return
            time.sleep(.01)
        self.fail(f"worker {worker.pid} did not publish runtime readiness: {ready}")

    def fixture(self, base):
        # The .app boundary prevents an unrelated installed broad watcher from
        # entering this fixture. The tested worker is rooted explicitly inside.
        package = tempfile.TemporaryDirectory(prefix="jaso-native-", suffix=".app", dir=base)
        self.addCleanup(package.cleanup)
        self.addCleanup(remove_fixture, Path(package.name))
        state = tempfile.TemporaryDirectory(prefix="jaso-native-state-")
        self.addCleanup(state.cleanup)
        root = Path(package.name) / "files"
        root.mkdir()
        config = Path(state.name) / "config.json"
        config.write_text(json.dumps(dict(roots=[str(root)], excludes=[], skip_hidden_tops=[], state_dir=state.name, apply=True)))
        return root, config, Path(state.name)

    def test_scan_descends_and_preview_never_applies_loaded_config(self):
        root, config, state = self.fixture(None)
        folder = root / "child"
        folder.mkdir()
        name = unicodedata.normalize("NFD", "한글.txt")
        source = folder / name
        source.write_bytes(b"unchanged")
        preview = self.call("scan", config=config)
        self.assertIn(str(source), preview["candidates"])
        self.assertEqual(stored_name(source), name)
        self.assertFalse((state / "state").exists())
        applied = self.call("scan", "--apply", config=config)
        self.assertEqual(applied["renamed"], 1)
        self.assertEqual(stored_name(source), "한글.txt")
        self.assertEqual(source.read_bytes(), b"unchanged")

    def test_filesystem_roundtrip(self):
        for base in [None, *filter(None, os.environ.get("JASO_TEST_VOLUMES", "").split(":"))]:
            with self.subTest(volume=base):
                root, config, state = self.fixture(base)
                originals = {}
                for name, payload in [("비어있음", b""), ("내용.txt", b"native payload"), ("속성.txt", b"metadata")]:
                    decomposed = unicodedata.normalize("NFD", name)
                    path = root / decomposed
                    path.write_bytes(payload)
                    originals[name] = decomposed
                subprocess.run(["/usr/bin/xattr", "-w", "user.jaso_test", "retained", str(root / originals["속성.txt"])], check=True)
                directory = unicodedata.normalize("NFD", "폴더")
                (root / directory).mkdir()
                child = unicodedata.normalize("NFD", "아래.txt")
                (root / directory / child).write_bytes(b"nested")
                link = unicodedata.normalize("NFD", "링크")
                os.symlink("missing-target", root / link)
                applied = self.call("scan", "--apply", config=config)
                self.assertEqual(applied["renamed"], 6, applied)
                self.assertEqual(applied["errors"], [])
                for name in originals:
                    self.assertEqual(stored_name(root / name), name)
                self.assertEqual(stored_name(root / "폴더" / "아래.txt"), "아래.txt")
                self.assertEqual(os.readlink(root / "링크"), "missing-target")
                self.assertEqual(subprocess.check_output(["/usr/bin/xattr", "-p", "user.jaso_test", str(root / "속성.txt")]).strip(), b"retained")
                self.assertEqual(self.call("scan", "--apply", config=config)["renamed"], 0)
                reverted = self.call("revert", "--journal", str(state / "logs/renames.jsonl"), config=config)
                self.assertEqual(reverted, {"reverted": 6, "failed": 0})
                for original in originals.values():
                    self.assertEqual(stored_name(root / original), original)
                self.assertEqual((root / directory / child).read_bytes(), b"nested")

    def test_watch_package_boundaries_drain_without_parent_rescan_loop(self):
        root, config, state = self.fixture(None)
        packages = [root / "Editor.app", root / "Runtime.framework"]
        for package in packages:
            package.mkdir()
            (package / "internal.txt").write_bytes(b"package contents")
        (root / "documents").mkdir()
        document = root / "documents" / unicodedata.normalize("NFD", "일반.txt")
        document.write_bytes(b"ordinary document")
        worker = subprocess.Popen([str(BINARY), "watch", "--config", str(config)],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        try:
            self.wait_for_worker_ready(worker, state)
            deadline = time.monotonic() + 20
            status = {}
            while time.monotonic() < deadline:
                self.assertIsNone(worker.poll(), "worker exited before the queue drained")
                status = self.call("status", config=config)
                if status.get("baseline_complete") and status.get("pending_jobs") == 0:
                    break
                time.sleep(0.1)
            else:
                self.fail("package-containing root never reached idle: " + json.dumps(status))
            self.assertEqual(stored_name(document), "일반.txt")
            self.assertEqual(document.read_bytes(), b"ordinary document")
            with sqlite3.connect(f"file:{state / 'state/index.sqlite3'}?mode=ro", uri=True) as database:
                paths = {row[0] for row in database.execute("SELECT path FROM entries")}
            for package in packages:
                self.assertIn(str(package), paths, "a skipped package must retain its saved entry")
                self.assertNotIn(str(package / "internal.txt"), paths)
                self.assertEqual((package / "internal.txt").read_bytes(), b"package contents")
        finally:
            if worker.poll() is None:
                worker.terminate()
            _, error = worker.communicate(timeout=10)
            self.assertEqual(worker.returncode, 0, error)

    def test_watch_pause_restart_and_incremental_replay(self):
        roots = []
        for base in [None, *filter(None, os.environ.get("JASO_TEST_VOLUMES", "").split(":"))]:
            root, config, state = self.fixture(base)
            roots.append(root)
        value = json.loads(config.read_text())
        value["roots"] = list(map(str, roots))
        config.write_text(json.dumps(value))
        worker = None
        def start():
            return subprocess.Popen([str(BINARY), "watch", "--config", str(config)], stdout=subprocess.DEVNULL,
                                    stderr=subprocess.PIPE, text=True, env={**os.environ, "HOME": str(config.parent)})
        def status():
            if worker.poll() is not None:
                self.fail("worker exited: " + worker.stderr.read())
            return self.call("status", config=config)
        def until(predicate, description):
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                if predicate():
                    return
                time.sleep(0.1)
            self.fail(description + ": " + json.dumps(status(), ensure_ascii=False))
        def stop():
            worker.terminate()
            _, error = worker.communicate(timeout=10)
            self.assertEqual(worker.returncode, 0, error)
        try:
            worker = start()
            self.wait_for_worker_ready(worker, state)
            until(lambda: status().get("baseline_complete"), "initial index")
            initial = status()
            self.assertEqual(len(initial["active_roots"]), len(roots))
            def create(label):
                paths = []
                for root in roots:
                    path = root / unicodedata.normalize("NFD", label + ".txt")
                    path.write_bytes(b"event payload")
                    paths.append(path)
                return paths
            active = create("실행중")
            until(lambda: all(stored_name(p) == unicodedata.normalize("NFC", p.name) for p in active), "live normalization")
            until(lambda: status()["pending_jobs"] == 0, "drained queue")
            self.assertEqual(status()["baseline_walks"], initial["baseline_walks"])
            self.call("pause", config=config)
            paused_paths = create("일시정지")
            until(lambda: status()["pending_jobs"] > 0, "paused events remain queued")
            self.assertTrue(all(stored_name(p) == p.name for p in paused_paths))
            stop()
            offline_paths = create("꺼진동안")
            worker = start()
            self.wait_for_worker_ready(worker, state)
            until(lambda: status().get("running") and status().get("active_roots"), "restart")
            self.assertTrue(status()["paused"])
            self.assertTrue(all(stored_name(p) == p.name for p in paused_paths + offline_paths))
            self.call("resume", config=config)
            until(lambda: all(stored_name(p) == unicodedata.normalize("NFC", p.name) for p in paused_paths + offline_paths), "restart event replay")
            until(lambda: status()["pending_jobs"] == 0, "final queue drain")
            final = status()
            self.assertEqual(final["baseline_walks"], initial["baseline_walks"])
            self.assertEqual(final["errors"], 0)
            self.assertEqual(final["renamed"], 3 * len(roots))
            self.assertTrue(all(p.read_bytes() == b"event payload" for p in active + paused_paths + offline_paths))
            stop()
        finally:
            if worker is not None and worker.poll() is None:
                worker.terminate()
                worker.communicate(timeout=10)


class WorkerReadiness(unittest.TestCase):
    """Keep the startup barrier strict without depending on scheduler timing."""
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="jaso-readiness-", suffix=".app")
        self.addCleanup(directory.cleanup)
        self.state = Path(directory.name)
        self.marker = self.state / "state/runtime.json"
        self.marker.parent.mkdir()

    def start_fixture(self, code):
        worker = subprocess.Popen([sys.executable, "-c", code, str(self.marker)],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        def stop():
            if worker.poll() is None:
                worker.terminate()
            worker.communicate(timeout=5)
        self.addCleanup(stop)
        return worker

    def test_waits_for_delayed_acknowledgement_from_current_pid(self):
        self.marker.write_text(json.dumps({"pid": -1}))
        worker = self.start_fixture(
            "import json,os,sys,time; from pathlib import Path; "
            "time.sleep(.05); path=Path(sys.argv[1]); temp=path.with_suffix('.tmp'); "
            "temp.write_text(json.dumps({'pid':os.getpid()})); os.replace(temp,path); time.sleep(10)")
        NativeCLI.wait_for_worker_ready(self, worker, self.state)
        self.assertEqual(json.loads(self.marker.read_text())["pid"], worker.pid)
        self.assertIsNone(worker.poll())

    def test_worker_exit_is_reported_before_readiness(self):
        worker = self.start_fixture("import sys; sys.stderr.write('fixture worker crash'); sys.exit(7)")
        with self.assertRaisesRegex(AssertionError, "worker exited before readiness: fixture worker crash"):
            NativeCLI.wait_for_worker_ready(self, worker, self.state)

    def test_stale_pid_does_not_satisfy_bounded_startup_wait(self):
        self.marker.write_text(json.dumps({"pid": -1}))
        worker = self.start_fixture("import time; time.sleep(10)")
        with self.assertRaisesRegex(AssertionError, "did not publish runtime readiness"):
            NativeCLI.wait_for_worker_ready(self, worker, self.state, timeout=.05)
        self.assertIsNone(worker.poll())

    def test_malformed_acknowledgement_is_not_retried(self):
        self.marker.write_text("not JSON")
        worker = self.start_fixture("import time; time.sleep(10)")
        with self.assertRaises(json.JSONDecodeError):
            NativeCLI.wait_for_worker_ready(self, worker, self.state)

    def test_status_still_rejects_database_corruption(self):
        fixture = NativeCLI()
        self.addCleanup(fixture.doCleanups)
        _, config, state = fixture.fixture(None)
        database = state / "state/index.sqlite3"
        database.parent.mkdir()
        database.write_bytes(b"not a SQLite database")
        with self.assertRaisesRegex(AssertionError, "not a database"):
            fixture.call("status", config=config)

if __name__ == "__main__":
    unittest.main()
