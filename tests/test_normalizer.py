"""Scoped normalization against real temporary filesystem objects."""
import errno
from contextlib import contextmanager
import json
import os
from pathlib import Path
import tempfile
import time
from types import SimpleNamespace
import unicodedata
import unittest
from unittest import mock

from jaso_nfc import normalizer as mod
from jaso_nfc import native_names
from jaso_nfc import legacy as legacy_mod
from jaso_nfc.normalizer import Normalizer, PendingRecoveryError, Policy
from jaso_nfc.legacy import journal_records, revert


def nfd(value):
    return unicodedata.normalize("NFD", value)


class NormalizerTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="jaso-scope-")
        self.addCleanup(temp.cleanup)
        self.base = Path(temp.name)
        self.root = self.base / "files"
        self.root.mkdir()
        self.log = self.base / "renames.jsonl"
        self.pending = self.base / "pending.json"
        self.retry = self.base / "retry.json"

    def engine(self, policy=None, **kwargs):
        result = Normalizer(policy or Policy([str(self.root)]), str(self.log),
                            str(self.retry), str(self.pending), **kwargs)
        self.addCleanup(result.close)
        return result

    def file(self, name="한글.txt", parent=None):
        target = (parent or self.root) / nfd(name)
        target.write_text("preserved contents", encoding="utf-8")
        return target

    def assert_marker_absent(self, path, key):
        parent = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            held = native_names.open_entry(parent, path.name)
            try:
                self.assertIsNone(native_names.marker_get(held, key))
            finally:
                os.close(held)
        finally:
            os.close(parent)

    @contextmanager
    def changing_identity(self):
        """Model exFAT's empty-file inode changes while retaining real objects."""
        shifts = {}
        ordinary = os.rename
        create = native_names.marker_create
        remove = native_names.marker_remove
        signature = legacy_mod.path_signature

        def bump(info):
            shifts[info.st_ino] = shifts.get(info.st_ino, 0) + 1
            return info

        def rename(src, dst, **kwargs):
            ordinary(src, dst, **kwargs)
            bump(os.stat(dst, dir_fd=kwargs["dst_dir_fd"], follow_symlinks=False))

        def marker_create(*args):
            return bump(create(*args))

        def marker_remove(*args):
            return bump(remove(*args))

        def path_signature(*args, **kwargs):
            value = signature(*args, **kwargs)
            if isinstance(value[0], int):
                value[1] += shifts.get(value[1], 0) * 1000000000000
            return value

        with mock.patch.object(mod, "_identity", side_effect=lambda info: [
                info.st_dev, info.st_ino + shifts.get(info.st_ino, 0) * 1000000000000]), \
                mock.patch.object(mod.os, "rename", side_effect=rename), \
                mock.patch.object(mod, "marker_create", side_effect=marker_create), \
                mock.patch.object(mod, "marker_remove", side_effect=marker_remove), \
                mock.patch.object(legacy_mod, "path_signature", side_effect=path_signature), \
                mock.patch.object(mod, "rename_exclusive", side_effect=OSError(errno.ENOTSUP, "unsupported")):
            yield

    def test_recursive_names_and_metadata_use_final_parent_spellings(self):
        folder = self.root / nfd("폴더")
        folder.mkdir()
        original = self.file(parent=folder)
        ino = original.stat().st_ino
        result = self.engine().reconcile(str(self.root), True)
        final = self.root / "폴더" / "한글.txt"
        self.assertEqual(result["renamed"], 2)
        self.assertEqual(result["errors"], [])
        self.assertEqual(set(e["path"] for e in result["entries"]),
                         {str(final), str(final.parent)})
        entry = next(e for e in result["entries"] if e["path"] == str(final))
        self.assertEqual(entry["ino"], ino)
        self.assertEqual(set(entry), {"path", "kind", "dev", "ino", "mtime_ns",
                                     "ctime_ns", "size", "mode"})
        self.assertEqual(set(result["directories"]), {str(self.root), str(final.parent)})
        self.assertEqual(final.read_text(), "preserved contents")
        self.assertFalse(self.pending.exists())

    def test_shallow_normalizes_direct_child_but_does_not_visit_descendants(self):
        folder = self.root / "plain"
        folder.mkdir()
        nested = self.file(parent=folder)
        direct = self.file("직접.txt")
        result = self.engine().reconcile(str(self.root), False)
        self.assertEqual(result["renamed"], 1)
        self.assertIn(nested.name, os.listdir(folder))
        self.assertEqual(len(result["entries"]), 2)
        self.assertEqual(result["directories"], [str(self.root)])
        self.assertIn("직접.txt", os.listdir(self.root))

    def test_root_itself_is_never_renamed(self):
        root = self.root / nfd("루트")
        root.mkdir()
        self.file(parent=root)
        result = self.engine(Policy([str(root)])).reconcile(str(root), True)
        self.assertEqual(result["renamed"], 1)
        self.assertIn(root.name, os.listdir(self.root))

    def test_appledouble_metadata_is_not_independently_normalized(self):
        companion = self.file("._메타데이터.txt")
        ordinary = self.file("문서.txt")
        result = self.engine().reconcile(str(self.root), True)
        self.assertEqual(result["renamed"], 1)
        self.assertEqual(result["errors"], [])
        self.assertIn(companion.name, os.listdir(self.root))
        self.assertNotIn(str(companion), [entry["path"] for entry in result["entries"]])
        self.assertIn("문서.txt", os.listdir(self.root))
        self.assertFalse(self.engine().policy.accepts(str(companion)))

    def test_dry_run_preserves_names_and_does_not_write_state(self):
        source = self.file()
        result = self.engine(apply=False).reconcile(str(self.root), True)
        self.assertEqual(result["renamed"], 0)
        self.assertEqual(result["entries"][0]["path"], str(source))
        self.assertFalse(self.log.exists())
        self.assertFalse(self.retry.exists())
        self.assertFalse(self.pending.exists())

    def test_policy_rejects_excluded_ancestors_not_similar_prefixes(self):
        excluded = self.root / "secret"
        allowed = self.root / "secret-copy"
        excluded.mkdir()
        allowed.mkdir()
        original = self.file(parent=excluded)
        self.file(parent=allowed)
        policy = Policy([str(self.root)], excludes=[str(excluded)])
        self.assertFalse(policy.accepts(str(original)))
        self.assertTrue(policy.accepts(str(allowed)))
        engine = self.engine(policy)
        self.assertEqual(engine.reconcile(str(excluded), True)["entries"], [])
        self.assertIn(original.name, os.listdir(excluded))
        self.assertEqual(engine.reconcile(str(allowed), True)["renamed"], 1)

    def test_root_exclusions_skip_home_library_but_allow_independent_cloud_root(self):
        library = self.root / "Library"
        cloud = library / "CloudStorage"
        internal = library / "Application Support"
        cloud.mkdir(parents=True)
        internal.mkdir()
        ordinary = self.file("문서.txt")
        cloud_file = self.file("동기화.txt", parent=cloud)
        internal_file = self.file("내부.txt", parent=internal)
        policy = Policy([str(self.root), str(cloud)],
                        root_excludes={str(self.root): (str(library), str(self.root / ".Trash"))})
        self.assertFalse(policy.accepts(str(library)))
        self.assertFalse(policy.accepts(str(internal_file)))
        self.assertTrue(policy.accepts(str(cloud_file)))
        self.assertEqual(policy._root(str(cloud_file)), str(cloud))
        engine = self.engine(policy, apply=False)
        home_result = engine.reconcile(str(self.root), True)
        cloud_result = engine.reconcile(str(cloud), True)
        self.assertEqual([entry["path"] for entry in home_result["entries"]], [str(ordinary)])
        self.assertEqual([entry["path"] for entry in cloud_result["entries"]], [str(cloud_file)])
        self.assertEqual(home_result["directories"], [str(self.root)])
        self.assertEqual(cloud_result["directories"], [str(cloud)])
        self.assertIn(cloud_file.name, os.listdir(cloud))
        self.assertIn(internal_file.name, os.listdir(internal))
        self.assertFalse(self.log.exists())

    def test_global_exclusion_wins_over_explicit_cloud_root(self):
        library = self.root / "Library"
        cloud = library / "CloudStorage"
        blocked = cloud / "private"
        blocked.mkdir(parents=True)
        source = self.file(parent=blocked)
        policy = Policy([str(self.root), str(cloud), str(blocked)],
                        excludes=(str(blocked),),
                        root_excludes={str(self.root): (str(library),)})
        self.assertTrue(policy.accepts(str(cloud)))
        self.assertFalse(policy.accepts(str(source)))
        result = self.engine(policy).reconcile(str(blocked), True)
        self.assertEqual(result["entries"], [])
        self.assertIn(source.name, os.listdir(blocked))

    def test_root_exclusions_apply_only_to_the_most_specific_root(self):
        library = self.root / "Library"
        cloud = library / "CloudStorage"
        private = cloud / "private"
        private.mkdir(parents=True)
        policy = Policy([str(cloud), str(self.root)], root_excludes={
            str(self.root): (str(library),), str(cloud): (str(private),)})
        self.assertTrue(policy.contains(str(private)))
        self.assertFalse(policy.accepts(str(private)))
        self.assertTrue(policy.accepts(str(cloud / "ordinary.txt")))
        self.assertFalse(policy.accepts(str(library / "ordinary.txt")))

    def test_nested_cloud_root_queued_scope_preserves_actual_spelling(self):
        library = self.root / "Library"
        cloud = library / "CloudStorage"
        cloud.mkdir(parents=True)
        folder = cloud / nfd("폴더")
        folder.mkdir()
        self.file(parent=folder)
        policy = Policy([str(self.root), str(cloud)],
                        root_excludes={str(self.root): (str(library),)})
        engine = self.engine(policy)
        engine.reconcile(str(cloud), False)
        result = engine.reconcile(str(folder), True)
        self.assertEqual(result["scope"], str(cloud / "폴더"))
        self.assertEqual(result["directories"], [str(cloud / "폴더")])
        self.assertEqual(result["entries"][0]["path"], str(cloud / "폴더" / "한글.txt"))

    def test_nested_roots_do_not_bypass_package_or_symlink_ancestors(self):
        package = self.root / "Example.app"
        nested = package / "contents"
        nested.mkdir(parents=True)
        outside = self.base / "outside"
        (outside / "contents").mkdir(parents=True)
        link = self.root / "linked"
        link.symlink_to(outside, target_is_directory=True)
        policy = Policy([str(self.root), str(nested), str(link / "contents")], root_excludes={})
        self.assertFalse(policy.accepts(str(nested / "file")))
        self.assertFalse(policy.accepts(str(link / "contents" / "file")))

    def test_nested_configured_root_name_is_preserved(self):
        nested = self.root / nfd("루트")
        nested.mkdir()
        self.file(parent=nested)
        policy = Policy([str(self.root), str(nested)], root_excludes={})
        result = self.engine(policy).reconcile(str(self.root), True)
        self.assertEqual(result["renamed"], 1)
        self.assertIn(nested.name, os.listdir(self.root))
        self.assertIn("한글.txt", os.listdir(nested))

    def test_direct_event_under_hidden_git_package_or_symlink_is_ignored(self):
        policy = Policy([str(self.root)], skip_hidden_tops=[str(self.root)])
        for name in (".hidden", ".git", "Sample.app", "Sample.framework"):
            folder = self.root / name
            folder.mkdir()
            child = folder / "nested"
            child.mkdir()
            source = self.file(parent=child)
            self.assertFalse(policy.accepts(str(source)), name)
            self.assertEqual(self.engine(policy).reconcile(str(child), True)["entries"], [])
            self.assertIn(source.name, os.listdir(child))
        outside = self.base / "outside"
        outside.mkdir()
        source = self.file(parent=outside)
        link = self.root / "link"
        link.symlink_to(outside, target_is_directory=True)
        self.assertTrue(policy.accepts(str(link)))
        self.assertFalse(policy.descend(str(link)))
        self.assertFalse(policy.accepts(str(link / source.name)))
        self.assertEqual(self.engine(policy).reconcile(str(link), True)["entries"], [])

    def test_package_name_is_normalized_but_contents_are_preserved(self):
        bundle = self.root / nfd("프로그램.app")
        bundle.mkdir()
        child = self.file(parent=bundle)
        result = self.engine().reconcile(str(self.root), True)
        self.assertEqual(result["renamed"], 1)
        self.assertEqual(len(result["entries"]), 1)
        self.assertIn(child.name, os.listdir(self.root / "프로그램.app"))

    def test_symlink_name_is_normalized_without_following_target(self):
        outside = self.base / "outside"
        outside.mkdir()
        source = self.file(parent=outside)
        link = self.root / nfd("연결")
        link.symlink_to(outside)
        result = self.engine().reconcile(str(self.root), True)
        self.assertEqual(result["entries"][0]["kind"], "symlink")
        self.assertEqual(result["renamed"], 1)
        self.assertIn(source.name, os.listdir(outside))

    def test_missing_directory_returns_empty_success(self):
        self.assertEqual(self.engine().reconcile(str(self.root / "gone"), True),
                         dict(entries=[], directories=[], errors=[], renamed=0))

    def test_partial_enumeration_does_not_mutate_the_incomplete_directory(self):
        source = self.file()
        engine = self.engine()
        with mock.patch.object(mod, "_list_directory", side_effect=PermissionError(errno.EACCES, "denied")):
            result = engine.reconcile(str(self.root), True)
        self.assertEqual(len(result["errors"]), 1)
        self.assertEqual(result["renamed"], 0)
        self.assertIn(source.name, os.listdir(self.root))

    def test_pending_intent_is_durable_before_the_first_rename(self):
        source = self.file()
        identity = [source.stat().st_dev, source.stat().st_ino]
        original = mod.rename_exclusive
        calls = []

        def observe(src, dst, dir_fd):
            pending = json.loads(self.pending.read_text())
            self.assertEqual(pending["identity"], identity)
            self.assertIn("temporary_path", pending)
            self.assertEqual(pending["old"], source.name)
            self.assertFalse(Path(pending["temporary_path"]).name.startswith("._"),
                             "Temporary names must not enter Apple's metadata namespace")
            calls.append((src, dst))
            return original(src, dst, dir_fd)

        with mock.patch.object(mod, "rename_exclusive", side_effect=observe):
            result = self.engine().reconcile(str(self.root), True)
        self.assertEqual(result["renamed"], 1)
        self.assertEqual(len(calls), 2)

    def test_crash_after_temporary_hop_recovers_same_inode(self):
        source = self.file()
        inode = source.stat().st_ino
        original = mod.rename_exclusive
        count = 0

        def crash(src, dst, dir_fd):
            nonlocal count
            count += 1
            original(src, dst, dir_fd)
            if count == 1:
                raise KeyboardInterrupt("simulated crash after rename")

        with mock.patch.object(mod, "rename_exclusive", side_effect=crash):
            with self.assertRaises(KeyboardInterrupt):
                self.engine().reconcile(str(self.root), True)
        self.assertTrue(self.pending.exists())
        result = self.engine().recover()
        self.assertEqual(result["status"], "renamed")
        self.assertEqual((self.root / "한글.txt").stat().st_ino, inode)
        self.assertFalse(self.pending.exists())
        self.assertEqual(len(list(journal_records(str(self.log)))), 1)

    def test_conflict_after_temporary_hop_preserves_target_and_source(self):
        source = self.file()
        inode = source.stat().st_ino
        original = mod.rename_exclusive
        count = 0

        def race(src, dst, dir_fd):
            nonlocal count
            count += 1
            if count == 2:
                (self.root / "한글.txt").write_text("concurrent target")
            return original(src, dst, dir_fd)

        with mock.patch.object(mod, "rename_exclusive", side_effect=race):
            result = self.engine().reconcile(str(self.root), True)
        self.assertTrue(result["errors"])
        intent = json.loads(self.pending.read_text())
        temporary = Path(intent["temporary_path"])
        self.assertEqual(temporary.stat().st_ino, inode)
        self.assertEqual(temporary.read_text(), "preserved contents")
        self.assertEqual((self.root / "한글.txt").read_text(), "concurrent target")
        with self.assertRaises(PendingRecoveryError):
            self.engine().recover()
        self.assertTrue(self.pending.exists())

    def test_retry_scope_is_only_failed_candidate_parent_and_survives_restart(self):
        folder = self.root / "nested"
        folder.mkdir()
        source = self.file(parent=folder)
        with mock.patch.object(mod, "rename_exclusive", side_effect=PermissionError(errno.EACCES, "denied")):
            result = self.engine().reconcile(str(self.root), True)
        self.assertTrue(result["errors"])
        engine = self.engine()
        self.assertEqual(engine.retry_paths(), [])
        self.assertEqual(engine.retry_paths(time.time() + 100000), [str(folder)])
        with mock.patch.object(mod, "rename_exclusive") as rename:
            engine.reconcile(str(folder), False)
            rename.assert_not_called()
        self.assertIn(source.name, os.listdir(folder))

    def test_unsupported_stored_normalization_rolls_back_and_backs_off(self):
        source = self.file()
        original = mod._stored_name

        def unsupported(fd, identity, candidates=None):
            value = original(fd, identity, candidates)
            return nfd(value) if value and not value.endswith(mod.TMP_SUFFIX) else value

        with mock.patch.object(mod, "_stored_name", side_effect=unsupported):
            result = self.engine().reconcile(str(self.root), True)
        self.assertEqual(result["renamed"], 0)
        self.assertTrue(result["errors"])
        self.assertFalse(self.pending.exists())
        self.assertIn(source.name, os.listdir(self.root))
        self.assertTrue(json.loads(self.retry.read_text())["entries"])

    def test_revert_reads_new_records_and_restores_deep_original_paths(self):
        folder = self.root / nfd("폴더")
        folder.mkdir()
        source = self.file(parent=folder)
        self.engine().reconcile(str(self.root), True)
        self.assertEqual(revert(str(self.log), str(self.base / "revert.jsonl")), (2, 0))
        self.assertIn(folder.name, os.listdir(self.root))
        self.assertIn(source.name, os.listdir(folder))

    def test_queued_old_scope_returns_actual_parent_spelling(self):
        folder = self.root / nfd("폴더")
        folder.mkdir()
        self.file(parent=folder)
        engine = self.engine()
        engine.reconcile(str(self.root), False)
        result = engine.reconcile(str(folder), True)
        self.assertEqual(result["directories"], [str(self.root / "폴더")])
        self.assertEqual(result["entries"][0]["path"], str(self.root / "폴더" / "한글.txt"))

    def test_crash_after_journal_write_recovers_without_duplicate_history(self):
        self.file()
        engine = self.engine()
        with mock.patch.object(engine, "_pending_clear", side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                engine.reconcile(str(self.root), True)
        self.assertEqual(len(list(journal_records(str(self.log)))), 1)
        self.engine().recover()
        self.assertEqual(len(list(journal_records(str(self.log)))), 1)

    def test_policy_mount_descendants_are_rejected_even_for_direct_events(self):
        mount = self.root / "mount"
        mount.mkdir()
        child = mount / "nested"
        child.mkdir()
        policy = Policy([str(self.root)])
        actual_ismount = os.path.ismount
        with mock.patch.object(mod.os.path, "ismount", side_effect=lambda p: str(p) == str(mount) or actual_ismount(p)):
            self.assertTrue(policy.accepts(str(mount)))
            self.assertFalse(policy.descend(str(mount)))
            self.assertFalse(policy.accepts(str(child)))

    def test_preexisting_hardlink_does_not_confuse_identity_verification(self):
        plain = self.root / "plain.txt"
        plain.write_text("hardlink content")
        source = self.root / nfd("한글.txt")
        os.link(plain, source)
        result = self.engine().reconcile(str(self.root), True)
        self.assertEqual(result["errors"], [])
        self.assertEqual(result["renamed"], 1)
        self.assertEqual((self.root / "한글.txt").stat().st_ino, plain.stat().st_ino)
        self.assertEqual(plain.read_text(), "hardlink content")

    def test_recovery_precedes_processing_a_missing_nested_scope(self):
        folder = self.root / nfd("폴더")
        folder.mkdir()
        original = mod.rename_exclusive

        def crash(src, dst, dir_fd):
            original(src, dst, dir_fd)
            raise KeyboardInterrupt("crashed directory hop")

        with mock.patch.object(mod, "rename_exclusive", side_effect=crash):
            with self.assertRaises(KeyboardInterrupt):
                self.engine().reconcile(str(self.root), True)
        self.engine().reconcile(str(folder / "missing"), True)
        self.assertFalse(self.pending.exists())
        self.assertIn("폴더", os.listdir(self.root))

    def test_missing_retry_candidate_is_removed_durably(self):
        source = self.file()
        with mock.patch.object(mod, "rename_exclusive", side_effect=PermissionError(errno.EACCES, "denied")):
            self.engine().reconcile(str(self.root), True)
        source.unlink()
        self.assertEqual(self.engine().retry_paths(time.time() + 100000), [])
        self.assertEqual(json.loads(self.retry.read_text())["entries"], {})

    def test_cannot_open_directory_is_reported_not_empty_success(self):
        original = os.open

        def denied(path, *args, **kwargs):
            if str(path) == str(self.root):
                raise PermissionError(errno.EACCES, "fixture permission denied")
            return original(path, *args, **kwargs)

        with mock.patch.object(mod.os, "open", side_effect=denied):
            result = self.engine().reconcile(str(self.root), True)
        self.assertTrue(result["errors"])

    def test_source_replaced_before_first_hop_is_restored_to_visible_name(self):
        source = self.file()
        saved = self.base / "external-original"
        inode = source.stat().st_ino
        original = mod.rename_exclusive
        first = True

        def replace(src, dst, dir_fd):
            nonlocal first
            if first:
                first = False
                os.rename(source, saved)
                source.write_text("replacement contents")
            return original(src, dst, dir_fd)

        with mock.patch.object(mod, "rename_exclusive", side_effect=replace):
            result = self.engine().reconcile(str(self.root), True)
        self.assertTrue(result["errors"])
        self.assertIn(source.name, os.listdir(self.root))
        self.assertEqual(source.read_text(), "replacement contents")
        self.assertEqual(saved.stat().st_ino, inode)
        self.assertFalse(self.pending.exists())

    def test_source_replacement_rollback_conflict_records_actual_staged_identity(self):
        source = self.file()
        saved = self.base / "external-original"
        original = mod.rename_exclusive
        first = True
        replacement_inode = None

        def replace(src, dst, dir_fd):
            nonlocal first, replacement_inode
            if first:
                first = False
                os.rename(source, saved)
                source.write_text("replacement contents")
                replacement_inode = source.stat().st_ino
                original(src, dst, dir_fd)
                source.write_text("second replacement")
                return
            return original(src, dst, dir_fd)

        with mock.patch.object(mod, "rename_exclusive", side_effect=replace):
            result = self.engine().reconcile(str(self.root), True)
        self.assertTrue(result["errors"])
        pending = json.loads(self.pending.read_text())
        self.assertEqual(pending["identity"][1], replacement_inode)
        self.assertEqual(pending["recovery_action"], "rollback")
        self.assertEqual(Path(pending["temporary_path"]).read_text(), "replacement contents")
        self.assertEqual(source.read_text(), "second replacement")
        source.unlink()
        self.engine().recover()
        self.assertIn(source.name, os.listdir(self.root))
        self.assertEqual(source.read_text(), "replacement contents")
        self.assertFalse(self.pending.exists())

    def test_retry_candidate_normalized_elsewhere_is_removed(self):
        source = self.file()
        with mock.patch.object(mod, "rename_exclusive", side_effect=PermissionError(errno.EACCES, "denied")):
            self.engine().reconcile(str(self.root), True)
        os.rename(source, self.root / "한글.txt")
        engine = self.engine()
        self.assertEqual(engine.retry_paths(time.time() + 100000), [])
        self.assertEqual(json.loads(self.retry.read_text())["entries"], {})

    def test_retry_candidates_are_not_statted_before_their_due_time(self):
        source = self.file()
        with mock.patch.object(mod, "rename_exclusive", side_effect=PermissionError(errno.EACCES, "denied")):
            self.engine().reconcile(str(self.root), True)
        engine = self.engine()
        with mock.patch.object(mod.os, "lstat", side_effect=AssertionError("premature candidate I/O")):
            self.assertEqual(engine.retry_paths(), [])

    def test_crash_after_source_swap_hop_recovers_the_actual_staged_object(self):
        source = self.file()
        saved = self.base / "external-original"
        original = mod.rename_exclusive

        def crash(src, dst, dir_fd):
            os.rename(source, saved)
            source.write_text("replacement contents")
            original(src, dst, dir_fd)
            raise KeyboardInterrupt("crash before staged identity inspection")

        with mock.patch.object(mod, "rename_exclusive", side_effect=crash):
            with self.assertRaises(KeyboardInterrupt):
                self.engine().reconcile(str(self.root), True)
        result = self.engine().recover()
        self.assertEqual(result["status"], "rolled-back")
        self.assertIn(source.name, os.listdir(self.root))
        self.assertEqual(source.read_text(), "replacement contents")
        self.assertEqual(saved.read_text(), "preserved contents")
        self.assertFalse(self.pending.exists())

    def test_guarded_fallback_normalizes_files_and_directories_and_reverts(self):
        folder = self.root / nfd("폴더")
        folder.mkdir()
        source = self.file(parent=folder)
        inode = source.stat().st_ino
        with mock.patch.object(mod.sys, "platform", "darwin"), mock.patch.object(
                mod, "rename_exclusive", side_effect=OSError(errno.ENOTSUP, "unsupported")):
            result = self.engine().reconcile(str(self.root), True)
            self.assertEqual(result["errors"], [])
            self.assertEqual(result["renamed"], 2)
            self.assertEqual((self.root / "폴더" / "한글.txt").stat().st_ino, inode)
            self.assertEqual(revert(str(self.log), str(self.base / "revert.jsonl")), (2, 0))
        self.assertIn(folder.name, os.listdir(self.root))
        self.assertIn(source.name, os.listdir(folder))
        self.assertEqual(source.read_text(), "preserved contents")
        self.assertTrue(all(row["rename_mode"] == "guarded"
                            for row in journal_records(str(self.log))))

    def test_guarded_mode_is_durable_before_each_ordinary_rename(self):
        self.file()
        ordinary = os.rename
        seen = []

        def observe(src, dst, **kwargs):
            operation = json.loads(self.pending.read_text())
            self.assertEqual(operation["rename_mode"], "guarded")
            self.assertEqual(operation["identity"], mod._identity(
                os.stat(src, dir_fd=kwargs["src_dir_fd"], follow_symlinks=False)))
            seen.append((src, dst))
            return ordinary(src, dst, **kwargs)

        with mock.patch.object(mod.sys, "platform", "darwin"), mock.patch.object(
                mod, "rename_exclusive", side_effect=OSError(errno.ENOSYS, "unavailable")), \
                mock.patch.object(mod.os, "rename", side_effect=observe):
            result = self.engine().reconcile(str(self.root), True)
        self.assertEqual(result["renamed"], 1)
        self.assertEqual(len(seen), 2)

    def test_guarded_fallback_rejects_a_broken_symlink_destination(self):
        self.file()
        calls = 0

        def unavailable(src, dst, dir_fd):
            nonlocal calls
            calls += 1
            if calls == 2:
                (self.root / dst).symlink_to(self.base / "missing")
            raise OSError(errno.EOPNOTSUPP, "unsupported")

        with mock.patch.object(mod.sys, "platform", "darwin"), mock.patch.object(
                mod, "rename_exclusive", side_effect=unavailable):
            result = self.engine().reconcile(str(self.root), True)
        self.assertTrue(result["errors"])
        pending = json.loads(self.pending.read_text())
        self.assertEqual(pending["rename_mode"], "guarded")
        self.assertTrue((self.root / "한글.txt").is_symlink())
        self.assertEqual(Path(pending["temporary_path"]).read_text(), "preserved contents")
        with self.assertRaises(PendingRecoveryError):
            self.engine().recover()

    def test_guarded_fallback_checks_source_again_after_pending_write(self):
        source = self.file()
        saved = self.base / "original"
        engine = self.engine()
        write = engine._pending_write

        def swap(operation):
            write(operation)
            if operation.get("rename_mode") == "guarded":
                os.rename(source, saved)
                source.write_text("concurrent replacement")

        with mock.patch.object(mod.sys, "platform", "darwin"), mock.patch.object(
                mod, "rename_exclusive", side_effect=OSError(errno.ENOTSUP, "unsupported")), \
                mock.patch.object(engine, "_pending_write", side_effect=swap):
            result = engine.reconcile(str(self.root), True)
        self.assertTrue(result["errors"])
        self.assertEqual(source.read_text(), "concurrent replacement")
        self.assertEqual(saved.read_text(), "preserved contents")
        self.assertFalse(any(name.endswith(mod.TMP_SUFFIX) for name in os.listdir(self.root)))

    def test_guarded_fallback_does_not_mutate_when_mode_cannot_be_persisted(self):
        source = self.file()
        engine = self.engine()
        write = engine._pending_write

        def denied(operation):
            if operation.get("rename_mode") == "guarded":
                raise OSError(errno.ENOSPC, "cannot persist mode")
            return write(operation)

        with mock.patch.object(mod.sys, "platform", "darwin"), mock.patch.object(
                mod, "rename_exclusive", side_effect=OSError(errno.ENOTSUP, "unsupported")), \
                mock.patch.object(engine, "_pending_write", side_effect=denied), \
                mock.patch.object(mod.os, "rename") as ordinary:
            result = engine.reconcile(str(self.root), True)
        self.assertTrue(result["errors"])
        ordinary.assert_not_called()
        self.assertIn(source.name, os.listdir(self.root))

    def test_guarded_fallback_recovers_crash_after_ordinary_temporary_hop(self):
        source = self.file()
        inode = source.stat().st_ino
        ordinary = os.rename

        def crash(src, dst, **kwargs):
            ordinary(src, dst, **kwargs)
            raise KeyboardInterrupt("crash after guarded first hop")

        with mock.patch.object(mod.sys, "platform", "darwin"), mock.patch.object(
                mod, "rename_exclusive", side_effect=OSError(errno.ENOTSUP, "unsupported")):
            with mock.patch.object(mod.os, "rename", side_effect=crash):
                with self.assertRaises(KeyboardInterrupt):
                    self.engine().reconcile(str(self.root), True)
            self.assertEqual(json.loads(self.pending.read_text())["rename_mode"], "guarded")
            result = self.engine().recover()
        self.assertEqual(result["status"], "renamed")
        self.assertEqual((self.root / "한글.txt").stat().st_ino, inode)
        self.assertFalse(self.pending.exists())

    def test_guarded_fallback_never_handles_permission_or_conflict_errors(self):
        source = self.file()
        for number in (errno.EACCES, errno.EPERM, errno.EEXIST, errno.EIO):
            with self.subTest(errno=number), mock.patch.object(mod.sys, "platform", "darwin"), \
                    mock.patch.object(mod, "rename_exclusive", side_effect=OSError(number, "denied")), \
                    mock.patch.object(mod.os, "rename") as ordinary:
                engine = self.engine()
                engine.retry.entries.clear()
                self.assertTrue(engine.reconcile(str(self.root), True)["errors"])
                ordinary.assert_not_called()
            self.assertIn(source.name, os.listdir(self.root))

    def test_decomposed_directory_presentation_uses_native_stored_spelling(self):
        folder = self.root / "폴더"
        folder.mkdir()
        file = folder / "한글.txt"
        file.write_text("already composed")
        original = os.scandir

        class PresentedEntries:
            def __init__(self, fd):
                self.inner = original(fd)

            def __enter__(self):
                return (SimpleNamespace(name=nfd(entry.name), stat=entry.stat)
                        for entry in self.inner)

            def __exit__(self, *args):
                self.inner.close()

        with mock.patch.object(mod.os, "scandir", side_effect=PresentedEntries), \
                mock.patch.object(mod, "rename_exclusive") as rename:
            result = self.engine().reconcile(str(self.root), True)
            rename.assert_not_called()
            retry_engine = self.engine()
            retry_engine.retry.failure(str(folder / nfd("한글.txt")), str(file), "45")
            self.assertEqual(retry_engine.retry_paths(time.time() + 100000), [])
        self.assertEqual(result["errors"], [])
        self.assertEqual({entry["path"] for entry in result["entries"]}, {str(folder), str(file)})
        self.assertEqual(result["directories"], [str(self.root), str(folder)])

    def test_guarded_marker_intent_precedes_attachment_and_final_identity_is_journaled(self):
        source = self.file()
        source.write_bytes(b"")
        create = native_names.marker_create
        observed = []

        def observe(descriptor, key, token):
            pending = json.loads(self.pending.read_text())
            self.assertEqual(pending["marker"], {"name": key, "token": token.decode("ascii")})
            self.assertEqual(pending["phase"], "marker-intent")
            observed.append(key)
            return create(descriptor, key, token)

        with mock.patch.object(mod, "marker_create", side_effect=observe), mock.patch.object(
                mod, "rename_exclusive", side_effect=OSError(errno.ENOTSUP, "unsupported")):
            result = self.engine().reconcile(str(self.root), True)
        self.assertEqual(result["errors"], [])
        self.assertEqual(result["renamed"], 1)
        self.assertEqual(len(observed), 1)
        target = self.root / "한글.txt"
        self.assertEqual(target.read_bytes(), b"")
        self.assert_marker_absent(target, observed[0])
        self.assertEqual(list(journal_records(str(self.log)))[-1]["identity"], mod._identity(target.stat()))

    def test_guarded_marker_handles_identity_changes_at_every_mutation(self):
        self.file().write_bytes(b"")
        with self.changing_identity():
            result = self.engine().reconcile(str(self.root), True)
            self.assertEqual(result["errors"], [])
            self.assertEqual(result["renamed"], 1)
            target = self.root / "한글.txt"
            rows = list(journal_records(str(self.log)))
            self.assertEqual(rows[-1]["identity"], mod._identity(target.stat()))
            self.assertEqual(rows[-1]["operation_id"], rows[0]["operation_id"])
            self.assertTrue(rows[-1]["identity_finalized"])
            self.assertEqual(revert(str(self.log), str(self.base / "reverted.jsonl")), (1, 0))
            self.assertIn(nfd("한글.txt"), os.listdir(self.root))
        self.assertEqual(target.read_bytes(), b"")
        self.assert_marker_absent(target, rows[-1]["marker"]["name"])

    def test_guarded_marker_recovers_crash_after_attachment(self):
        source = self.file()
        create = native_names.marker_create

        def crash(*args):
            create(*args)
            raise KeyboardInterrupt("crash after marker attachment")

        with mock.patch.object(mod, "marker_create", side_effect=crash), mock.patch.object(
                mod, "rename_exclusive", side_effect=OSError(errno.ENOTSUP, "unsupported")):
            with self.assertRaises(KeyboardInterrupt):
                self.engine().reconcile(str(self.root), True)
        key = json.loads(self.pending.read_text())["marker"]["name"]
        result = self.engine().recover()
        self.assertEqual(result["status"], "not-started")
        self.assertIn(source.name, os.listdir(self.root))
        self.assert_marker_absent(source, key)
        self.assertFalse(self.pending.exists())

    def test_guarded_marker_recovers_changed_identity_after_each_hop(self):
        for crash_hop in (1, 2):
            with self.subTest(hop=crash_hop):
                source = self.file("항목" + str(crash_hop))
                engine = self.engine()
                with self.changing_identity():
                    ordinary = mod.os.rename
                    calls = 0

                    def crash(src, dst, **kwargs):
                        nonlocal calls
                        ordinary(src, dst, **kwargs)
                        calls += 1
                        if calls == crash_hop:
                            raise KeyboardInterrupt("crash before changed inode can be journaled")

                    with mock.patch.object(mod.os, "rename", side_effect=crash):
                        with self.assertRaises(KeyboardInterrupt):
                            engine.reconcile(str(self.root), True)
                    result = self.engine().recover()
                    self.assertEqual(result["status"], "renamed")
                    target = self.root / mod.nfc(source.name)
                    self.assertEqual(target.read_text(), "preserved contents")
                    self.assert_marker_absent(target, result["marker"]["name"])
                    self.assertFalse(self.pending.exists())

    def test_guarded_marker_cleanup_crash_does_not_adopt_replacement_identity(self):
        self.file().write_bytes(b"")
        with self.changing_identity():
            remove = mod.marker_remove

            def crash(*args):
                remove(*args)
                raise KeyboardInterrupt("crash between cleanup and final identity write")

            with mock.patch.object(mod, "marker_remove", side_effect=crash):
                with self.assertRaises(KeyboardInterrupt):
                    self.engine().reconcile(str(self.root), True)
            target = self.root / "한글.txt"
            target.unlink()
            target.write_text("replacement")
            with mock.patch.object(mod, "rename_guarded", side_effect=AssertionError("unexpected recovery rename")):
                result = self.engine().recover()
            self.assertTrue(result["identity_finalization_unavailable"])
            self.assertEqual(target.read_text(), "replacement")
            self.assertFalse(self.pending.exists())
            self.assertNotEqual(list(journal_records(str(self.log)))[-1]["identity"], mod._identity(target.stat()))
            self.assertEqual(revert(str(self.log), str(self.base / "reverted.jsonl")), (0, 1))
            self.assertEqual(target.read_text(), "replacement")

    def test_guarded_crash_after_source_swap_restores_unmarked_staged_replacement(self):
        source = self.file()
        saved = self.base / "external-original"
        ordinary = os.rename

        def crash(src, dst, **kwargs):
            ordinary(source, saved)
            source.write_text("concurrent replacement")
            ordinary(src, dst, **kwargs)
            raise KeyboardInterrupt("crash after unchecked ordinary rename source swap")

        with mock.patch.object(mod, "rename_exclusive", side_effect=OSError(errno.ENOTSUP, "unsupported")), \
                mock.patch.object(mod.os, "rename", side_effect=crash):
            with self.assertRaises(KeyboardInterrupt):
                self.engine().reconcile(str(self.root), True)
        with mock.patch.object(mod, "rename_exclusive", side_effect=OSError(errno.ENOTSUP, "unsupported")):
            result = self.engine().recover()
        self.assertEqual(result["status"], "rolled-back")
        self.assertEqual(source.read_text(), "concurrent replacement")
        self.assertEqual(saved.read_text(), "preserved contents")
        self.assertIn(source.name, os.listdir(self.root))
        self.assertFalse(self.pending.exists())
        self.assertIn("expected_marker", result)

    def test_marker_attachment_never_overwrites_an_existing_attribute(self):
        source = self.file()
        create = native_names.marker_create

        def conflict(held, key, token):
            create(held, key, b"existing value")
            return create(held, key, token)

        with mock.patch.object(mod, "marker_create", side_effect=conflict), mock.patch.object(
                mod, "rename_exclusive", side_effect=OSError(errno.ENOTSUP, "unsupported")):
            result = self.engine().reconcile(str(self.root), True)
        self.assertTrue(result["errors"])
        self.assertIn(source.name, os.listdir(self.root))
        operation = json.loads(self.pending.read_text())
        parent = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
        try:
            held = native_names.open_entry(parent, source.name)
            try:
                self.assertEqual(native_names.marker_get(held, operation["marker"]["name"]), b"existing value")
            finally:
                os.close(held)
        finally:
            os.close(parent)

    def test_pending_marker_schema_is_validated_before_xattr_access(self):
        source = self.file()
        operation = dict(version=1, operation_id="a" * 32, dir=str(self.root),
                         old=source.name, new="한글.txt", identity=mod._identity(source.stat()),
                         temporary_path=str(self.root / ("reserved" + mod.TMP_SUFFIX)))
        for marker in ([], {"name": "com.other.attribute", "token": "a" * 32},
                       {"name": "user.jaso_nfc." + "b" * 32, "token": "different"}):
            with self.subTest(marker=marker):
                self.pending.write_text(json.dumps(dict(operation, marker=marker)))
                with mock.patch.object(mod, "marker_get", side_effect=AssertionError("unvalidated marker")):
                    with self.assertRaises(PendingRecoveryError):
                        self.engine().recover()
                self.assertTrue(self.pending.exists())


if __name__ == "__main__":
    unittest.main()
