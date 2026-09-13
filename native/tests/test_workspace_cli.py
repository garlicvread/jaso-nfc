"""Exercise the actual activity/history CLI against one disposable worker."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unicodedata
import unittest
import uuid

BINARY = Path(os.environ.get("JASO_NATIVE_BINARY", "target/debug/jaso-nfc")).absolute()


class WorkspaceCLI(unittest.TestCase):
    def test_live_activity_selected_restore_and_shutdown(self):
        self.exercise_restore(unicodedata.normalize("NFD", "검사.txt"), "검사.txt")

    def test_encoding_repair_preview_history_restore_and_hold(self):
        self.exercise_restore("µµÀüÀÇ IRÆ÷½ºÅÍ-ÃÖÁ¾º».pdf", "도전의 IR포스터-최종본.pdf")

    def exercise_restore(self, original, expected):
        with tempfile.TemporaryDirectory(prefix="jaso-workspace-", suffix=".app") as directory:
            base = Path(directory)
            files = base / "files"
            files.mkdir()
            (files / original).write_text("fixture content")
            config = base / "config.json"
            config.write_text(json.dumps(dict(scope="configured", roots=[str(files)],
                excludes=[], exclude_names=[], state_dir=str(base / "state"),
                log_dir=str(base / "logs"), apply=True)))

            def call(*args):
                result = subprocess.run([str(BINARY), *args, "--config", str(config)],
                    capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                return json.loads(result.stdout)

            preview = call("scan")
            self.assertEqual(preview["mode"], "scan")
            self.assertIn(str(files / original), preview["candidates"])
            self.assertEqual(os.listdir(files), [original], "preview changed the source name")
            worker = subprocess.Popen([str(BINARY), "watch", "--config", str(config)],
                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
            try:
                deadline = time.monotonic() + 15
                page = None
                while time.monotonic() < deadline:
                    self.assertIsNone(worker.poll(), "fixture worker exited")
                    page = call("history", "list")
                    if page["items"]:
                        break
                    time.sleep(.05)
                self.assertTrue(page["items"], "worker did not record the fixture rename")
                entry = next(item for item in page["items"] if item["old_name"] == original)
                self.assertEqual(entry["new_name"], expected)
                activity = call("activity")
                self.assertTrue(activity["available"])
                self.assertEqual(activity["schema_version"], 1)
                self.assertTrue(activity["activity"]["session_id"])
                self.assertLessEqual(len(activity["activity"]["events"]), 200)
                call("pause")
                preview = call("history", "preview", "--id", entry["id"],
                    "--revision", entry["revision"])
                self.assertTrue(preview["allowed"], preview)
                request_id = str(uuid.uuid4())
                accepted = call("history", "restore", "--request-id", request_id,
                    "--operation-id", entry["id"], "--revision", entry["revision"])
                self.assertEqual(accepted["state"], "queued")
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    result = call("history", "result", "--request-id", request_id)
                    if result["state"] != "queued":
                        break
                    time.sleep(.05)
                self.assertEqual(result["state"], "restored", result)
                self.assertEqual(os.listdir(files), [original])
                self.assertEqual((files / original).read_text(), "fixture content")
                self.assertTrue(call("status")["paused"])
                call("resume")
                time.sleep(.15)
                self.assertEqual(os.listdir(files), [original], "automatic cleanup undid the restore")
                refreshed = call("history", "list")
                self.assertEqual(refreshed["today_count"], page["today_count"])
                self.assertTrue(next(item for item in refreshed["items"] if item["id"] == entry["id"])["restored"])
                storage = call("storage")
                self.assertGreater(storage["allocated_bytes"], 0)
            finally:
                worker.terminate()
                try:
                    _, stderr = worker.communicate(timeout=10)
                except subprocess.TimeoutExpired:
                    worker.kill()
                    _, stderr = worker.communicate(timeout=5)
                    self.fail("fixture worker did not stop")
            self.assertEqual(worker.returncode, 0, stderr)
            self.assertFalse(call("activity")["available"])
            maintained = call("maintain")
            self.assertIn("before", maintained)
            self.assertIn("after", maintained)
            retained = call("history", "list")
            self.assertEqual(retained["total"], refreshed["total"])
            self.assertTrue(next(item for item in retained["items"] if item["id"] == entry["id"])["restored"])


if __name__ == "__main__":
    unittest.main()
