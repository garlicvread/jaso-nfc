"""Setup API tests use only disposable configuration and document fixtures."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import unicodedata

BINARY = Path(os.environ.get("JASO_NATIVE_BINARY", "target/debug/jaso-nfc")).absolute()


class SetupCLI(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="jaso-setup-", suffix=".app")
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name)
        self.root = self.base / "files"
        self.root.mkdir()
        self.config = self.base / "custom settings.json"
        self.state = self.base / "private"
        self.original = dict(scope="configured", roots=[str(self.root)], excludes=[],
                             exclude_names=[".git", "keep"], skip_hidden_tops=[],
                             state_dir=str(self.state), log_dir=str(self.base / "history"), apply=True)
        self.config.write_text(json.dumps(self.original, indent=3) + "\n")
        self.bytes = self.config.read_bytes()

    def call(self, action, *args, success=True):
        result = subprocess.run([str(BINARY), "setup", action, "--config", str(self.config), *args],
                                capture_output=True, text=True, timeout=30,
                                env={**os.environ, "HOME": str(self.base)})
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return json.loads(result.stdout)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stderr

    def draft(self, **changes):
        value = {key: self.original[key] for key in ("scope", "roots", "excludes", "apply")}
        value.update(changes)
        return json.dumps(value)

    def test_read_returns_exact_draft_and_revision_without_writing(self):
        result = self.call("read")
        self.assertEqual(result["config"], json.loads(self.draft()))
        self.assertEqual(len(result["revision"]), 64)
        self.assertFalse(result["running"])
        self.assertFalse(result["paused"])
        self.assertEqual(self.config.read_bytes(), self.bytes)
        self.assertFalse(self.state.exists())

    def test_preview_returns_before_after_and_respects_retained_exclusion_names(self):
        before = unicodedata.normalize("NFD", "보고서.txt")
        (self.root / before).write_bytes(b"contents stay intact")
        ignored = self.root / "keep"
        ignored.mkdir()
        (ignored / before).write_bytes(b"excluded")
        result = self.call("preview", "--draft", self.draft())
        self.assertEqual(result["candidates"], [{"path": str(self.root / before),
                                              "before": before, "after": "보고서.txt"}])
        self.assertEqual(result["errors"], [])
        self.assertTrue(result["complete"])
        self.assertFalse(result["truncated"])
        self.assertIn(before, os.listdir(self.root))
        self.assertEqual((self.root / before).read_bytes(), b"contents stay intact")
        self.assertEqual(self.config.read_bytes(), self.bytes)
        self.assertFalse(self.state.exists())

    def test_preview_rejects_invalid_new_selection_instead_of_clear_result(self):
        plain = self.base / "file.txt"
        plain.write_text("unchanged")
        for selected in [plain, self.base / "missing", self.state]:
            with self.subTest(root=selected):
                self.call("preview", "--draft", self.draft(roots=[str(selected)]), success=False)
        result = self.call("preview", "--draft", self.draft(excludes=[str(self.root)]))
        self.assertTrue(result["errors"])
        self.assertFalse(result["complete"])

    def test_preview_retains_existing_disconnected_root_as_partial_result(self):
        self.root.rmdir()
        result = self.call("preview", "--draft", self.draft())
        self.assertTrue(result["errors"])
        self.assertFalse(result["complete"])

    def test_rejects_unknown_or_missing_draft_fields_without_touching_config(self):
        for draft in [{"scope": "configured"}, {**json.loads(self.draft()), "state_dir": "/other"},
                      {**json.loads(self.draft()), "roots": []}]:
            with self.subTest(draft=draft):
                self.call("preview", "--draft", json.dumps(draft), success=False)
        self.assertEqual(self.config.read_bytes(), self.bytes)

    def test_save_rejects_stale_revision_before_lifecycle_or_config_changes(self):
        error = self.call("save", "--draft", self.draft(apply=False),
                          "--revision", "stale", success=False)
        self.assertIn("changed", error)
        self.assertEqual(self.config.read_bytes(), self.bytes)
        self.assertFalse((self.state / "state").exists())

    def test_pause_and_resume_cannot_override_an_in_progress_setup(self):
        import fcntl
        control = self.state / "state/control.json"
        control.parent.mkdir(parents=True)
        control.write_bytes(b'{"version":1,"paused":true}\n')
        original = control.read_bytes()
        lifecycle = self.base / "Library/Application Support/jaso-nfc/lifecycle.lock"
        lifecycle.parent.mkdir(parents=True)
        with lifecycle.open("wb") as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            for command in ["pause", "resume"]:
                result = subprocess.run([str(BINARY), command, "--config", str(self.config)],
                                        capture_output=True, text=True,
                                        env={**os.environ, "HOME": str(self.base)})
                self.assertNotEqual(result.returncode, 0, "setup lock must block " + command)
                self.assertIn("in progress", result.stderr)
                self.assertEqual(control.read_bytes(), original)
        resumed = subprocess.run([str(BINARY), "resume", "--config", str(self.config)],
                                 capture_output=True, text=True,
                                 env={**os.environ, "HOME": str(self.base)})
        self.assertEqual(resumed.returncode, 0, resumed.stderr)
        self.assertFalse(json.loads(control.read_text())["paused"])


if __name__ == "__main__":
    unittest.main()
