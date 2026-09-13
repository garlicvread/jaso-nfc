"""Check source release identities without building or opening the application."""
import ast
import os
from pathlib import Path
import plistlib
import tomllib
import unittest

PROJECT = Path(__file__).resolve().parents[2]


class ReleaseMetadata(unittest.TestCase):
    def test_app_python_and_lockfile_match_cargo_release(self):
        cargo = tomllib.loads((PROJECT / "Cargo.toml").read_text())["package"]
        version = cargo["version"]
        python = tomllib.loads((PROJECT / "pyproject.toml").read_text())["project"]
        app = plistlib.loads((PROJECT / "native/macos/Info.plist").read_bytes())
        lock = tomllib.loads((PROJECT / "Cargo.lock").read_text())["package"]
        locked = [package for package in lock if package["name"] == cargo["name"]]
        self.assertEqual(len(locked), 1)
        module = ast.parse((PROJECT / "src/jaso_nfc/__init__.py").read_text())
        python_version = next(ast.literal_eval(node.value) for node in module.body
                              if isinstance(node, ast.Assign)
                              and any(isinstance(target, ast.Name) and target.id == "__version__"
                                      for target in node.targets))
        for surface, actual in (("app", app["CFBundleShortVersionString"]),
                                ("python package", python["version"]),
                                ("python CLI", python_version),
                                ("Cargo lockfile", locked[0]["version"])):
            with self.subTest(surface=surface):
                self.assertEqual(actual, version)

    def test_publisher_and_contributor_have_distinct_metadata(self):
        app = plistlib.loads((PROJECT / "native/macos/Info.plist").read_bytes())
        python = tomllib.loads((PROJECT / "pyproject.toml").read_text())["project"]
        self.assertEqual(app["JasoPublisher"], "AidALL Inc.")
        self.assertEqual(app.get("JasoContributor"), "garlicvread")
        self.assertEqual(app.get("JasoContributorEmail"), "ceo@aidall.tech")
        self.assertIn({"name": "garlicvread", "email": "ceo@aidall.tech"}, python["authors"])

    @unittest.skipUnless(os.environ.get("JASO_RELEASE_APP"), "No built application selected")
    def test_selected_payload_matches_release_metadata(self):
        source = plistlib.loads((PROJECT / "native/macos/Info.plist").read_bytes())
        app = Path(os.environ["JASO_RELEASE_APP"])
        payload = plistlib.loads((app / "Contents/Info.plist").read_bytes())
        for key in ("CFBundleIdentifier", "CFBundleShortVersionString", "CFBundleVersion",
                    "LSMinimumSystemVersion", "JasoPublisher", "JasoContributor",
                    "JasoContributorEmail", "JasoSupportEmail", "JasoSourceURL"):
            with self.subTest(key=key):
                self.assertEqual(payload.get(key), source[key],
                                 "Rebuild the app from this release before packaging it")


if __name__ == "__main__":
    unittest.main(verbosity=2)
