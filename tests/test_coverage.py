"""Coverage discovery uses temporary catalogs and metadata-only probes."""
import builtins
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

try:
    from jaso_nfc.coverage import Coverage, discover_user_coverage
except ImportError:
    Coverage = discover_user_coverage = None


class CoverageTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(discover_user_coverage, "Coverage discovery is missing")
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.users = self.base / "Users"
        self.volumes = self.base / "Volumes"
        self.users.mkdir()
        self.volumes.mkdir()
        self.mounts = set()
        mount_patch = patch("jaso_nfc.coverage.os.path.ismount",
                            side_effect=lambda path: Path(path) in self.mounts)
        mount_patch.start()
        self.addCleanup(mount_patch.stop)

    def discover(self, **kwargs):
        kwargs.setdefault("account_entries", [])
        kwargs.setdefault("startup_devices", ())
        return discover_user_coverage(self.users, self.volumes, **kwargs)

    def mkdir(self, path):
        path.mkdir(parents=True)
        return str(path)

    def test_all_visible_user_directories_and_shared_are_roots(self):
        expected = {self.mkdir(self.users / name) for name in ("one", "two", "Shared")}
        self.mkdir(self.users / ".hidden")
        (self.users / ".localized").write_text("")
        (self.users / "alias").symlink_to(self.users / "one", target_is_directory=True)
        coverage = self.discover()
        self.assertIsInstance(coverage, Coverage)
        self.assertEqual(set(coverage.roots), expected)
        self.assertEqual(set(coverage.catalog_roots), {str(self.users), str(self.volumes),
                                                        str(self.users / "one"), str(self.users / "two")})

    def test_home_internals_are_scoped_exclusions(self):
        home = self.mkdir(self.users / "person")
        coverage = self.discover()
        expected = (home + "/.Trash", home + "/Library")
        self.assertEqual(coverage.root_excludes[home], expected)
        self.assertTrue(set(expected).issubset(coverage.excludes))

    def test_cloud_data_roots_are_explicit_despite_home_library_exclusion(self):
        home = self.users / "person"
        cloud = self.mkdir(home / "Library" / "CloudStorage")
        icloud = self.mkdir(home / "Library" / "Mobile Documents" / "com~apple~CloudDocs")
        coverage = self.discover()
        self.assertEqual(set(coverage.roots), {str(home), cloud, icloud})
        self.assertIn(str(home / "Library"), coverage.catalog_roots)
        self.assertFalse(coverage.root_excludes.get(cloud))
        self.assertFalse(coverage.root_excludes.get(icloud))

    def test_only_real_visible_mounts_are_volume_roots(self):
        external = self.volumes / "Archive"
        self.mkdir(external)
        self.mkdir(self.volumes / "Unmounted folder")
        self.mkdir(self.volumes / ".hidden")
        (self.volumes / "Startup alias").symlink_to(self.users, target_is_directory=True)
        self.mounts.update((external, self.volumes / ".hidden", self.volumes / "Startup alias"))
        coverage = self.discover()
        self.assertEqual(coverage.roots, (str(external),))

    def test_startup_device_alias_is_not_covered_as_external_volume(self):
        alias = self.volumes / "Data alias"
        self.mkdir(alias)
        self.mounts.add(alias)
        coverage = self.discover(startup_devices={os.stat(alias).st_dev})
        self.assertNotIn(str(alias), coverage.roots)

    def test_ordinary_external_library_folder_is_user_data(self):
        external = self.volumes / "Books"
        self.mkdir(external / "Library")
        self.mounts.add(external)
        coverage = self.discover()
        self.assertIn(str(external), coverage.roots)
        self.assertNotIn(str(external / "Library"), coverage.excludes)
        self.assertIn(str(external / ".Spotlight-V100"), coverage.root_excludes[str(external)])
        self.assertIn(str(external / ".fseventsd"), coverage.root_excludes[str(external)])

    def test_bootable_external_volume_excludes_os_directories(self):
        external = self.volumes / "Boot disk"
        marker = external / "System" / "Library" / "CoreServices" / "SystemVersion.plist"
        marker.parent.mkdir(parents=True)
        marker.write_bytes(b"The content must never be read")
        self.mounts.add(external)
        with patch.object(builtins, "open", side_effect=AssertionError("File content read")):
            coverage = self.discover()
        scoped = coverage.root_excludes[str(external)]
        for name in ("System", "Library", "Applications", "private", "usr", "bin", "sbin", "dev"):
            self.assertIn(str(external / name), scoped)
        self.assertNotIn(str(external / "Users"), scoped)

    def test_unreadable_home_remains_known_and_is_reported(self):
        home = self.mkdir(self.users / "restricted")
        original = os.scandir
        def checked(path):
            if os.fspath(path) == home:
                raise PermissionError("directory denied")
            return original(path)
        with patch("jaso_nfc.coverage.os.scandir", side_effect=checked):
            coverage = self.discover()
        self.assertIn(home, coverage.roots)
        self.assertIn(home, coverage.unavailable)

    def test_unreadable_catalog_does_not_discard_other_catalog(self):
        external = self.volumes / "Work"
        self.mkdir(external)
        self.mounts.add(external)
        original = os.scandir
        def checked(path):
            if Path(path) == self.users:
                raise PermissionError("catalog denied")
            return original(path)
        with patch("jaso_nfc.coverage.os.scandir", side_effect=checked):
            coverage = self.discover()
        self.assertIn(str(self.users), coverage.unavailable)
        self.assertIn(str(external), coverage.roots)

    def test_real_account_home_outside_users_is_included_and_services_are_filtered(self):
        home = self.mkdir(self.base / "CustomHomes" / "person")
        service = self.mkdir(self.base / "service")
        missing = str(self.base / "missing")
        accounts = [
            SimpleNamespace(pw_uid=501, pw_dir=home, pw_name="person", pw_shell="/bin/zsh"),
            SimpleNamespace(pw_uid=502, pw_dir=service, pw_name="_service", pw_shell="/bin/zsh"),
            SimpleNamespace(pw_uid=503, pw_dir=service, pw_name="disabled", pw_shell="/usr/bin/false"),
            SimpleNamespace(pw_uid=0, pw_dir=service, pw_name="root", pw_shell="/bin/sh"),
            SimpleNamespace(pw_uid=504, pw_dir=missing, pw_name="gone", pw_shell="/bin/zsh"),
        ]
        coverage = self.discover(account_entries=accounts)
        self.assertEqual(coverage.roots, (home,))

    def test_discovery_does_not_descend_into_user_document_trees(self):
        home = self.users / "person"
        self.mkdir(home / "Documents" / "deep" / "deeper")
        original = os.scandir
        seen = []
        def checked(path):
            seen.append(os.fspath(path))
            self.assertNotIn("Documents", Path(path).parts)
            return original(path)
        with patch("jaso_nfc.coverage.os.scandir", side_effect=checked):
            coverage = self.discover()
        self.assertIn(str(home), coverage.roots)
        self.assertTrue(set(seen).issubset({str(self.users), str(self.volumes), str(home)}))

    def test_shared_library_is_user_data(self):
        shared = self.users / "Shared"
        self.mkdir(shared / "Library")
        coverage = self.discover()
        self.assertIn(str(shared), coverage.roots)
        self.assertNotIn(str(shared / "Library"), coverage.excludes)

    def test_bootable_external_user_homes_have_scoped_internals_and_cloud_roots(self):
        external = self.volumes / "Boot disk"
        marker = external / "System" / "Library" / "CoreServices" / "SystemVersion.plist"
        marker.parent.mkdir(parents=True)
        marker.write_text("")
        home = external / "Users" / "person"
        cloud = self.mkdir(home / "Library" / "CloudStorage")
        self.mounts.add(external)
        coverage = self.discover()
        self.assertTrue({str(external), str(home), cloud}.issubset(coverage.roots))
        self.assertIn(str(home / "Library"), coverage.root_excludes[str(home)])
        self.assertIn(str(external / "Users"), coverage.catalog_roots)

    def test_accessible_cloud_root_survives_denied_parent_directory_listing(self):
        home = self.users / "person"
        cloud = self.mkdir(home / "Library" / "CloudStorage")
        original = os.scandir
        def checked(path):
            if os.fspath(path) in {str(home), str(home / "Library")}:
                raise PermissionError("listing denied but known children accessible")
            return original(path)
        with patch("jaso_nfc.coverage.os.scandir", side_effect=checked):
            coverage = self.discover()
        self.assertIn(cloud, coverage.roots)
        self.assertNotIn(cloud, coverage.unavailable)

    def test_symlinked_library_does_not_create_cloud_roots_through_alias(self):
        home = self.users / "person"
        home.mkdir()
        outside = self.base / "OutsideLibrary"
        self.mkdir(outside / "CloudStorage")
        (home / "Library").symlink_to(outside, target_is_directory=True)
        coverage = self.discover()
        self.assertEqual(coverage.roots, (str(home),))

    def test_policy_integration_keeps_cloud_data_and_global_exclusions(self):
        from jaso_nfc.normalizer import Policy
        home = self.users / "person"
        cloud = self.mkdir(home / "Library" / "CloudStorage")
        coverage = self.discover()
        policy = Policy(coverage.roots, root_excludes=coverage.root_excludes)
        self.assertFalse(policy.accepts(str(home / "Library" / "Caches")))
        self.assertTrue(policy.accepts(cloud + "/Provider/document.txt"))
        restricted = Policy(coverage.roots, excludes=(cloud,), root_excludes=coverage.root_excludes)
        self.assertFalse(restricted.accepts(cloud + "/Provider/document.txt"))

    def test_home_catalog_detects_library_created_after_initial_discovery(self):
        home = self.users / "person"
        self.mkdir(home)
        initial = self.discover()
        self.assertIn(str(home), initial.catalog_roots)
        cloud = self.mkdir(home / "Library" / "CloudStorage")
        refreshed = self.discover()
        self.assertIn(cloud, refreshed.roots)
        self.assertIn(str(home / "Library"), refreshed.catalog_roots)

    def test_mobile_documents_catalog_detects_later_icloud_data_root(self):
        home = self.users / "person"
        mobile = home / "Library" / "Mobile Documents"
        self.mkdir(mobile)
        initial = self.discover()
        self.assertIn(str(mobile), initial.catalog_roots)
        self.assertNotIn(str(mobile / "com~apple~CloudDocs"), initial.roots)
        cloud = self.mkdir(mobile / "com~apple~CloudDocs")
        self.assertIn(cloud, self.discover().roots)

    def test_cloud_catalog_parents_trigger_existing_manager_event_filter(self):
        from jaso_nfc.events import Event, ITEM_CREATED
        from jaso_nfc.sources import SourceManager
        from types import SimpleNamespace
        import threading
        home = self.users / "person"
        mobile = home / "Library" / "Mobile Documents"
        self.mkdir(mobile)
        coverage = self.discover()
        wake = threading.Event()
        manager = SourceManager(None, None, None, wake)
        for parent, child in ((str(home), "Library"), (str(mobile), "com~apple~CloudDocs")):
            self.assertIn(parent, coverage.catalog_roots)
            manager.refresh_requested.clear()
            watch = SimpleNamespace(active=True, volume=SimpleNamespace(roots=(parent,)))
            manager._receive_catalog(watch, [Event(parent + "/" + child, ITEM_CREATED, 1)])
            self.assertTrue(manager.refresh_requested.is_set())

    def test_discovery_is_stable_and_preserves_catalog_spelling(self):
        home = self.mkdir(self.users / "폴더")
        first = self.discover()
        second = self.discover()
        self.assertEqual(first, second)
        self.assertIn(home, first.roots)
        self.assertEqual(os.listdir(self.users), ["폴더"])


if __name__ == "__main__":
    unittest.main()
