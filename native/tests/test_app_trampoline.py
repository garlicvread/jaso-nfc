"""Exercise the real Rust entry point in owned app bundles, without the real GUI."""
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest

PROJECT = Path(__file__).resolve().parents[2]
BINARY = Path(os.environ.get("JASO_NATIVE_BINARY", PROJECT / "target/debug/jaso-nfc")).resolve()
IDENTIFIER = "io.github.garlicvread.jaso-nfc"


@unittest.skipUnless(sys.platform == "darwin", "Native app dispatch requires macOS")
class AppTrampoline(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.build = tempfile.TemporaryDirectory(prefix="jaso-trampoline-build-")
        cls.addClassCleanup(cls.build.cleanup)
        cls.helper = Path(cls.build.name) / "fixture"
        subprocess.run(["clang", "-fobjc-arc", "-framework", "AppKit",
                        str(PROJECT / "native/tests/app_trampoline_fixture.m"),
                        "-o", str(cls.helper)], check=True, capture_output=True)

    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="jaso-trampoline-", suffix=".app")
        self.addCleanup(directory.cleanup)
        self.app = Path(directory.name).resolve()
        self.macos = self.app / "Contents/MacOS"
        self.macos.mkdir(parents=True)
        self.executable = self.macos / "jaso-nfc"
        shutil.copy2(BINARY, self.executable)
        shutil.copy2(self.helper, self.macos / "Jaso NFC")
        self.info = {"CFBundleIdentifier": IDENTIFIER, "CFBundleExecutable": "jaso-nfc",
                     "CFBundleName": "Jaso Trampoline Fixture", "CFBundlePackageType": "APPL",
                     "LSUIElement": True}
        self.write_info()

    def write_info(self, fmt=plistlib.FMT_XML):
        (self.app / "Contents/Info.plist").write_bytes(plistlib.dumps(self.info, fmt=fmt))

    def run_binary(self, *arguments, binary=None):
        with subprocess.Popen([str(binary or self.executable), *arguments],
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as process:
            try:
                stdout, stderr = process.communicate(timeout=12)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate()
                self.fail("Owned app fixture did not exit")
            return process.pid, process.returncode, stdout, stderr

    def test_no_arguments_execs_native_gui_in_the_same_process_and_bundle(self):
        for fmt in (plistlib.FMT_XML, plistlib.FMT_BINARY):
            with self.subTest(format=fmt):
                self.write_info(fmt)
                pid, code, stdout, stderr = self.run_binary()
                self.assertEqual(code, 0, stderr)
                record = json.loads(stdout)
                self.assertEqual(record["pid"], pid, "exec must not retain a wrapper process")
                self.assertEqual(record["identifier"], IDENTIFIER)
                self.assertEqual(record["application_identifier"], IDENTIFIER)
                self.assertEqual(Path(record["bundle"]).resolve(), self.app)
                for key in ("executable", "actual_executable", "application_executable"):
                    self.assertEqual(Path(record[key]).resolve(), self.macos / "Jaso NFC")
                self.assertEqual(len(record["arguments"]), 1)

    def test_explicit_cli_commands_do_not_launch_gui(self):
        _, code, stdout, stderr = self.run_binary("--version")
        self.assertEqual(code, 0, stderr)
        self.assertTrue(stdout.startswith("jaso-nfc "), stdout)
        _, code, stdout, stderr = self.run_binary("watch", "--help")
        self.assertEqual(code, 0, stderr)
        self.assertIn("--config", stdout)

    def test_explicit_cli_does_not_require_valid_app_metadata_or_a_gui(self):
        (self.app / "Contents/Info.plist").unlink()
        (self.macos / "Jaso NFC").unlink()
        for arguments in (("--version",), ("watch", "--help"), ("status", "--help"),
                          ("restart", "--help")):
            with self.subTest(arguments=arguments):
                _, code, stdout, stderr = self.run_binary(*arguments)
                self.assertEqual(code, 0, stderr)
                self.assertNotIn('"pid"', stdout)

    def test_unbundled_cli_keeps_no_argument_help(self):
        plain = Path(self.build.name) / "plain-jaso-nfc"
        shutil.copy2(BINARY, plain)
        _, code, stdout, stderr = self.run_binary(binary=plain)
        self.assertNotEqual(code, 0)
        self.assertIn("Usage:", stderr)
        self.assertEqual(stdout, "")

    def test_wrong_or_malformed_bundle_metadata_never_launches_gui(self):
        for key, wrong in (("CFBundleIdentifier", "unrelated.app"),
                           ("CFBundleExecutable", "Jaso NFC"),
                           ("CFBundlePackageType", "BNDL")):
            with self.subTest(key=key):
                previous = self.info[key]
                self.info[key] = wrong
                self.write_info()
                _, code, stdout, stderr = self.run_binary()
                self.assertNotEqual(code, 0)
                self.assertEqual(stdout, "")
                self.assertIn(key, stderr)
                self.info[key] = previous
        (self.app / "Contents/Info.plist").write_text("not a property list")
        _, code, stdout, stderr = self.run_binary()
        self.assertNotEqual(code, 0)
        self.assertEqual(stdout, "")
        self.assertIn("application metadata", stderr)

    def test_missing_symlink_or_script_gui_is_not_executed(self):
        menu = self.macos / "Jaso NFC"
        menu.unlink()
        _, code, stdout, stderr = self.run_binary()
        self.assertNotEqual(code, 0)
        self.assertEqual(stdout, "")
        self.assertIn("native GUI", stderr)
        menu.symlink_to(self.helper)
        _, code, stdout, _ = self.run_binary()
        self.assertNotEqual(code, 0)
        self.assertEqual(stdout, "")
        menu.unlink()
        menu.write_text("#!/bin/sh\nprintf 'unexpected script execution'\n")
        menu.chmod(0o755)
        _, code, stdout, _ = self.run_binary()
        self.assertNotEqual(code, 0)
        self.assertEqual(stdout, "")

    def test_non_executable_native_gui_is_rejected(self):
        (self.macos / "Jaso NFC").chmod(0o644)
        _, code, stdout, stderr = self.run_binary()
        self.assertNotEqual(code, 0)
        self.assertEqual(stdout, "")
        self.assertIn("native GUI", stderr)

    def test_forged_native_header_cannot_fall_back_to_a_shell(self):
        menu = self.macos / "Jaso NFC"
        menu.write_bytes(b"\xca\xfe\xba\xbe\nprintf 'unexpected shell execution'\n")
        menu.chmod(0o755)
        _, code, stdout, stderr = self.run_binary()
        self.assertNotEqual(code, 0, stdout)
        self.assertEqual(stdout, "")
        self.assertIn("native GUI", stderr)

    def test_packaging_names_worker_as_native_app_main(self):
        info = plistlib.loads((PROJECT / "native/macos/Info.plist").read_bytes())
        self.assertEqual(info["CFBundleIdentifier"], IDENTIFIER)
        self.assertEqual(info["CFBundleExecutable"], "jaso-nfc")
        self.assertEqual(info["CFBundlePackageType"], "APPL")


if __name__ == "__main__":
    unittest.main()
