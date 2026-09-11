"""Black-box tests for the native executable; all mutations use owned fixtures."""
import fcntl
import json
import os
from pathlib import Path
import subprocess
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

if __name__ == "__main__":
    unittest.main()
