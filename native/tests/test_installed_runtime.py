"""Run the actual installed-worker entry point in isolated native app fixtures."""
import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

PROJECT = Path(__file__).resolve().parents[2]
BINARY = Path(os.environ.get("JASO_NATIVE_BINARY", PROJECT / "target/debug/jaso-nfc")).resolve()


@unittest.skipUnless(sys.platform == "darwin", "Native installed runtime requires macOS")
class InstalledRuntime(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.build = tempfile.TemporaryDirectory(prefix="jaso-runtime-build-")
        cls.addClassCleanup(cls.build.cleanup)
        cls.helper = Path(cls.build.name) / "menu"
        subprocess.run(["clang", "-fobjc-arc", "-framework", "Foundation",
                        str(PROJECT / "native/tests/installed_runtime_fixture.m"),
                        "-o", str(cls.helper)], check=True, capture_output=True)

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="jaso-runtime-")
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name).resolve()
        self.app = self.base / "Jaso NFC.app"
        self.macos = self.app / "Contents/MacOS"
        self.macos.mkdir(parents=True)
        self.binary = self.macos / "jaso-nfc"
        shutil.copy2(BINARY, self.binary)
        shutil.copy2(self.helper, self.macos / "Jaso NFC")
        (self.app / "Contents/Info.plist").write_bytes(plistlib.dumps({
            "CFBundleIdentifier": "io.github.garlicvread.jaso-nfc",
            "CFBundleExecutable": "jaso-nfc", "CFBundlePackageType": "APPL"}))
        self.state = self.base / "state"
        self.root = self.base / "root"
        self.root.mkdir()
        self.config = self.base / "custom config.json"
        self.config.write_text(json.dumps({"state_dir": str(self.state),
            "scope": "configured", "roots": [str(self.root)], "apply": False}))
        self.workers = []
        self.addCleanup(self.cleanup_processes)

    def records(self):
        path = self.state / "fixture-menu.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def cleanup_processes(self):
        for worker in self.workers:
            if worker.poll() is None:
                worker.terminate()
                worker.communicate(timeout=10)
        for record in self.records():
            try:
                os.kill(record["pid"], signal.SIGTERM)
            except ProcessLookupError:
                pass

    def start(self, command="run", **env):
        worker = subprocess.Popen([str(self.binary), command, "--config", str(self.config)],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
            start_new_session=True, env={**os.environ, **env})
        self.workers.append(worker)
        return worker

    def until(self, predicate, worker):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if worker.poll() is not None:
                self.fail("worker exited: " + worker.stderr.read())
            if predicate():
                return
            time.sleep(0.025)
        self.fail("owned worker fixture did not reach expected state")

    def running(self):
        result = subprocess.run([str(self.binary), "status", "--config", str(self.config)],
            text=True, capture_output=True)
        self.assertEqual(result.returncode, 0,
                         f"status failed (exit {result.returncode})\n"
                         f"stdout: {result.stdout}\nstderr: {result.stderr}")
        return json.loads(result.stdout)["running"]

    def test_status_failure_keeps_the_cli_diagnostic(self):
        index = self.state / "state/index.sqlite3"
        index.parent.mkdir(parents=True)
        index.write_bytes(b"not a SQLite database" * 256)
        try:
            self.running()
        except Exception as error:
            self.assertIn("file is not a database", str(error))
        else:
            self.fail("A corrupt index must fail the status probe")

    def test_run_starts_one_menu_after_lock_and_menu_survives_worker_group_stop(self):
        worker = self.start()
        self.until(lambda: len(self.records()) == 1, worker)
        record = self.records()[0]
        self.assertTrue(record["worker_locked"])
        self.assertEqual(record["arguments"][1:], ["--config", str(self.config)])
        self.assertEqual(record["pgid"], record["pid"])
        self.assertEqual(record["sid"], record["pid"])
        self.assertNotEqual(record["pgid"], os.getpgid(worker.pid))
        duplicate_worker = self.start()
        _, error = duplicate_worker.communicate(timeout=10)
        self.assertNotEqual(duplicate_worker.returncode, 0)
        self.assertIn("already running", error)
        os.killpg(worker.pid, signal.SIGTERM)
        _, error = worker.communicate(timeout=10)
        self.assertEqual(worker.returncode, 0, error)
        os.kill(record["pid"], 0)
        self.assertFalse(self.running(), "detached GUI must not inherit the worker runtime lock")
        restarted = self.start()
        self.until(self.running, restarted)
        time.sleep(0.2)
        self.assertEqual(len(self.records()), 1, "existing GUI singleton must survive restart")
        children = subprocess.run(["ps", "-o", "pid=", "-o", "stat=", "-P", str(restarted.pid)],
                                  capture_output=True, text=True)
        self.assertEqual(children.stdout.strip(), "", "duplicate GUI child must be reaped")

    def test_menu_exit_or_invalid_gui_keeps_worker_running_without_relaunch(self):
        worker = self.start(JASO_FIXTURE_MENU_EXIT="1")
        self.until(lambda: len(self.records()) == 1, worker)
        time.sleep(0.2)
        children = subprocess.run(["ps", "-o", "pid=", "-o", "stat=", "-P", str(worker.pid)],
                                  capture_output=True, text=True)
        self.assertEqual(children.stdout.strip(), "", "exited GUI must not become a zombie")
        self.assertTrue(self.running())
        self.assertEqual(len(self.records()), 1)
        worker.terminate()
        worker.communicate(timeout=10)
        (self.macos / "Jaso NFC").unlink()
        restarted = self.start()
        self.until(self.running, restarted)
        self.until(lambda: "menu startup failed" in (self.state / "logs/service.log").read_text(), restarted)
        self.assertEqual(len(self.records()), 1)

    def test_manual_watch_never_opens_gui(self):
        worker = self.start("watch")
        log = self.state / "logs/service.log"
        # This fresh fixture has no earlier startup record. The marker follows
        # runtime-lock acquisition, index setup and source-worker creation;
        # probing status sooner can contend for the runtime lock itself.
        self.until(lambda: log.exists() and "native watch started;" in log.read_text(), worker)
        self.assertTrue(self.running())
        time.sleep(0.1)
        self.assertEqual(self.records(), [])

    def test_worker_acknowledges_its_loaded_config_while_holding_runtime_lock(self):
        worker = self.start()
        acknowledgement = self.state / "state/runtime.json"
        self.until(acknowledgement.exists, worker)
        value = json.loads(acknowledgement.read_text())
        self.assertEqual(value["pid"], worker.pid)
        self.assertEqual(value["config_path"], str(self.config))
        self.assertEqual(len(value["config_signature"]), 64)
        self.assertTrue(self.running())
        worker.terminate()
        _, error = worker.communicate(timeout=10)
        self.assertEqual(worker.returncode, 0, error)
        self.assertFalse(acknowledgement.exists())

    def test_watch_acknowledges_custom_config_without_opening_menu(self):
        worker = self.start("watch")
        acknowledgement = self.state / "state/runtime.json"
        self.until(acknowledgement.exists, worker)
        value = json.loads(acknowledgement.read_text())
        self.assertEqual(value["config_path"], str(self.config))
        self.assertEqual(value["pid"], worker.pid)
        self.assertEqual(self.records(), [])



if __name__ == "__main__":
    unittest.main()
