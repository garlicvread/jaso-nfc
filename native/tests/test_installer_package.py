"""Validate the actual read-only distribution without installing any login jobs."""
import hashlib
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest

PROJECT = Path(__file__).resolve().parents[2]
PACKAGE = Path(os.environ.get("JASO_INSTALLER_DMG", PROJECT / "dist/installer.dmg"))
PAYLOAD = Path(os.environ.get("JASO_INSTALLER_PAYLOAD", PROJECT / "dist/Jaso NFC.app"))


@unittest.skipUnless(sys.platform == "darwin", "DMG requires macOS")
class InstallerPackage(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not PACKAGE.is_file():
            raise AssertionError(f"Installation package is missing: {PACKAGE}")
        cls.directory = tempfile.TemporaryDirectory(prefix="jaso-package-test-")
        cls.addClassCleanup(cls.directory.cleanup)
        cls.mount = Path(cls.directory.name) / "volume"
        subprocess.run(["hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint",
                        str(cls.mount), str(PACKAGE)], check=True, capture_output=True)
        cls.addClassCleanup(subprocess.run, ["hdiutil", "detach", str(cls.mount)],
                            check=True, capture_output=True)
        cls.installer = cls.mount / "Install Jaso NFC.app"
        cls.app = cls.installer / "Contents/Resources/Jaso NFC.app"

    def test_complete_independent_installer(self):
        info = plistlib.loads((self.installer / "Contents/Info.plist").read_bytes())
        self.assertEqual(info["CFBundleIdentifier"], "io.github.garlicvread.jaso-nfc.installer")
        self.assertEqual(info["CFBundleExecutable"], "Jaso NFC Installer")
        self.assertEqual(info["JasoPublisher"], "AidALL Inc.")
        self.assertEqual(info["JasoSupportEmail"], "aidall_manager@aidall.tech")
        self.assertTrue(os.access(self.installer / "Contents/MacOS/Jaso NFC Installer", os.X_OK))
        self.assertTrue((self.installer / "Contents/Resources/JasoNFC.icns").is_file())

    def test_payload_matches_the_tested_application(self):
        original = {str(p.relative_to(PAYLOAD)): p for p in PAYLOAD.rglob("*") if p.is_file()}
        packaged = {str(p.relative_to(self.app)): p for p in self.app.rglob("*") if p.is_file()}
        self.assertEqual(original.keys(), packaged.keys())
        for name in original:
            with self.subTest(file=name):
                self.assertEqual(original[name].read_bytes(), packaged[name].read_bytes())
        for bundle in (self.installer, self.app):
            subprocess.run(["codesign", "--verify", "--deep", "--strict", str(bundle)],
                           check=True, capture_output=True)
        version = plistlib.loads((self.app / "Contents/Info.plist").read_bytes())["CFBundleShortVersionString"]
        result = subprocess.run([str(self.app / "Contents/MacOS/jaso-nfc"), "--version"],
                                check=True, capture_output=True, text=True)
        self.assertEqual(result.stdout.strip(), f"jaso-nfc {version}")

    def test_getting_started_instructions_license_and_publisher(self):
        instructions = (self.mount / "READ ME FIRST.txt").read_text()
        for required in ("Double-click", "Install Jaso NFC.app", "AidALL Inc.",
                         "aidall_manager@aidall.tech", "garlicvread", "Manage folders",
                         "Preview filenames", "Start automatic cleanup", "https://github.com/garlicvread/jaso-nfc", "설치"):
            self.assertIn(required, instructions)
        self.assertEqual((self.mount / "LICENSE.txt").read_bytes(), (PROJECT / "LICENSE").read_bytes())
        self.assertFalse((self.mount / "Applications").exists(), "Do not imply unsupported drag-drop setup")

    def test_read_only_volume_and_checksum(self):
        self.assertTrue(os.statvfs(self.mount).f_flag & os.ST_RDONLY)
        checksum = PACKAGE.with_suffix(PACKAGE.suffix + ".sha256").read_text().split()[0]
        self.assertEqual(hashlib.sha256(PACKAGE.read_bytes()).hexdigest(), checksum)
        for path in self.installer.rglob("*"):
            self.assertFalse(path.is_symlink(), str(path))

    def test_checksum_is_portable_to_a_download_directory(self):
        checksum = PACKAGE.with_suffix(PACKAGE.suffix + ".sha256")
        record = checksum.read_text().strip().split(maxsplit=1)
        self.assertEqual(len(record), 2)
        self.assertEqual(record[1], PACKAGE.name,
                         "The public checksum must contain only the DMG basename")
        with tempfile.TemporaryDirectory(prefix="jaso-download-test-") as directory:
            download = Path(directory)
            shutil.copy2(PACKAGE, download / PACKAGE.name)
            shutil.copy2(checksum, download / checksum.name)
            result = subprocess.run(["shasum", "-a", "256", "-c", checksum.name],
                                    cwd=download, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn(f"{PACKAGE.name}: OK", result.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
