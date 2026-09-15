"""Validate the actual read-only distribution without installing any login jobs."""
import hashlib
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest

PROJECT = Path(__file__).resolve().parents[2]
PAYLOAD = Path(os.environ.get("JASO_INSTALLER_PAYLOAD", PROJECT / "dist/Jaso NFC.app"))
RELEASE = os.environ.get("JASO_RELEASE_MODE", "0") == "1"
VERSION = plistlib.loads((PROJECT / "native/macos/Info.plist").read_bytes())["CFBundleShortVersionString"]
SUFFIX = "" if RELEASE else "-local"
PACKAGE = Path(os.environ.get("JASO_INSTALLER_DMG",
               PROJECT / f"dist/Jaso-NFC-{VERSION}-{platform.machine()}{SUFFIX}.dmg"))


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
        self.assertEqual(info["JasoContributor"], "garlicvread")
        self.assertEqual(info["JasoContributorEmail"], "ceo@aidall.tech")
        self.assertEqual(info["JasoSupportEmail"], "aidall_manager@aidall.tech")
        self.assertTrue(os.access(self.installer / "Contents/MacOS/Jaso NFC Installer", os.X_OK))
        self.assertTrue((self.installer / "Contents/Resources/JasoNFC.icns").is_file())
        source = plistlib.loads((PROJECT / "native/macos/Info.plist").read_bytes())
        for key in ("CFBundleShortVersionString", "CFBundleVersion", "LSMinimumSystemVersion"):
            self.assertEqual(info[key], source[key])

    def test_package_architecture_matches_every_executable(self):
        binaries = (self.app / "Contents/MacOS/jaso-nfc",
                    self.app / "Contents/MacOS/Jaso NFC",
                    self.installer / "Contents/MacOS/Jaso NFC Installer")
        architectures = [subprocess.run(["lipo", "-archs", str(binary)], check=True,
                                        capture_output=True, text=True).stdout.split()
                         for binary in binaries]
        self.assertTrue(architectures[0])
        for architecture in architectures[1:]:
            self.assertEqual(set(architecture), set(architectures[0]),
                             "Installer and payload must support the same architectures")
        version = plistlib.loads((self.app / "Contents/Info.plist").read_bytes())["CFBundleShortVersionString"]
        suffix = "" if RELEASE else "-local"
        self.assertEqual(PACKAGE.name, f"Jaso-NFC-{version}-{'-'.join(architectures[0])}{suffix}.dmg")

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
                         "aidall_manager@aidall.tech", "garlicvread <ceo@aidall.tech>",
                         "Open Jaso NFC…", "Folders", "Jaso NFC 열기…", "‘폴더’",
                         "Preview filenames", "Start automatic cleanup", "https://github.com/garlicvread/jaso-nfc", "설치"):
            self.assertIn(required, instructions)
        self.assertEqual((self.mount / "LICENSE.txt").read_bytes(), (PROJECT / "LICENSE").read_bytes())
        self.assertEqual((self.app / "Contents/Resources/LICENSE.txt").read_bytes(),
                         (PROJECT / "LICENSE").read_bytes())
        self.assertFalse((self.mount / "Applications").exists(), "Do not imply unsupported drag-drop setup")

    @unittest.skipUnless(RELEASE, "Local packages use ad hoc signing without notarization")
    def test_release_signatures_and_notarization(self):
        sys.path.insert(0, str(PROJECT / "scripts"))
        import release_signing
        release_signing.verify("installer", self.installer)
        release_signing.verify("dmg", PACKAGE)
        for path in (self.app, PACKAGE):
            subprocess.run(["xcrun", "stapler", "validate", str(path)],
                           check=True, capture_output=True)
        # These are static assessments of the completed distribution. They do
        # not install the app, test downloaded quarantine, or establish FDA access.
        release_signing.assess("payload", self.app)
        release_signing.assess("installer", self.installer)
        release_signing.assess("dmg", PACKAGE)

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
