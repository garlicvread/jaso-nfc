"""Recovery archives remain readable across journal generations."""
import gzip
import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from unittest import mock

from jaso_nfc import legacy
from jaso_nfc import normalizer


class LegacyJournalTests(unittest.TestCase):
    def test_rotation_preserves_success_history_and_bounds_active(self):
        with tempfile.TemporaryDirectory() as folder:
            path = str(Path(folder) / "history.jsonl")
            journal = legacy.Journal(path, 250, 100, 2)
            try:
                for value in range(40):
                    journal.emit(dict(status="renamed", old=str(value), new="new"))
                for value in range(50):
                    journal.emit(dict(status="error", error="diagnostic" * 10))
            finally:
                journal.close()
            self.assertLess(Path(path).stat().st_size, 250)
            records = list(legacy.journal_records(path))
            self.assertEqual([r["old"] for r in records], [str(i) for i in range(40)])
            self.assertLessEqual(len(list(Path(folder).glob("history.errors.jsonl*"))), 3)

    def test_plain_and_compressed_twins_are_read_once(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "history.jsonl"
            directory = Path(str(path) + ".history")
            directory.mkdir()
            raw = directory / "0001-history.jsonl"
            data = json.dumps(dict(status="renamed", old="old", new="new")) + "\n"
            raw.write_text(data)
            with gzip.open(str(raw) + ".gz", "wt") as out:
                out.write(data)
            self.assertEqual(len(list(legacy.journal_records(str(path)))), 1)

    def test_clock_regression_does_not_reorder_archives(self):
        with tempfile.TemporaryDirectory() as folder:
            path = str(Path(folder) / "history.jsonl")
            journal = legacy.Journal(path, 1, 1000, 1)
            try:
                with mock.patch.object(legacy.time, "time_ns", side_effect=[300, 100]):
                    journal.emit(dict(status="renamed", old="first"))
                    journal.emit(dict(status="renamed", old="second"))
            finally:
                journal.close()
            self.assertEqual([r["old"] for r in legacy.journal_records(path)], ["first", "second"])

    def test_revert_deduplicates_recovered_operation_identity(self):
        with tempfile.TemporaryDirectory() as folder:
            path = str(Path(folder) / "history.jsonl")
            source = Path(folder) / "new"
            source.write_text("contents")
            info = source.stat()
            operation = dict(operation_id="one-operation", dir=folder,
                             old="old", new="new", identity=[info.st_dev, info.st_ino])
            journal = legacy.Journal(path, 1000, 1000, 1)
            try:
                journal.emit(dict(operation, status="error", recovery_required=True,
                                  recovery_path=str(source)))
                journal.emit(dict(operation, status="renamed", recovered=True))
            finally:
                journal.close()
            self.assertEqual(legacy.revert(path, None), (1, 0))
            self.assertEqual((Path(folder) / "old").read_text(), "contents")

    def test_revert_destination_race_preserves_both_objects(self):
        with tempfile.TemporaryDirectory() as folder:
            source = Path(folder) / "normalized"
            source.write_text("original contents")
            info = source.stat()
            target = Path(folder) / "original"
            log = Path(folder) / "history.jsonl"
            output = Path(folder) / "revert.jsonl"
            log.write_text(json.dumps(dict(status="renamed", dir=folder,
                old=target.name, new=source.name, identity=[info.st_dev, info.st_ino])) + "\n")
            original = normalizer.rename_exclusive

            def race(src, dst, dir_fd):
                if dst == target.name:
                    target.write_text("concurrent contents")
                return original(src, dst, dir_fd)

            with mock.patch.object(normalizer, "rename_exclusive", side_effect=race):
                self.assertEqual(legacy.revert(str(log), str(output)), (0, 1))
            self.assertEqual(target.read_text(), "concurrent contents")
            records = list(legacy.journal_records(str(output)))
            recovery = next(r for r in records if r.get("recovery_required"))
            retained = Path(recovery["recovery_path"])
            self.assertEqual(retained.stat().st_ino, info.st_ino)
            self.assertEqual(retained.read_text(), "original contents")

    def test_retry_state_retains_old_unresolved_candidates(self):
        with tempfile.TemporaryDirectory() as folder:
            state = str(Path(folder) / "retry.json")
            source = str(Path(folder) / "candidate")
            Path(source).write_text("contents")
            retry = legacy.RetryState(state, 1, 60)
            retry.failure(source, source + "-new", "permission")
            retry.save()
            with mock.patch.object(legacy.time, "time", return_value=time.time() + 31 * 86400):
                reopened = legacy.RetryState(state, 1, 60)
            self.assertIn(source, reopened.entries)

    def test_retry_save_does_not_evict_unresolved_records_at_cap(self):
        with tempfile.TemporaryDirectory() as folder:
            state = str(Path(folder) / "retry.json")
            retry = legacy.RetryState(state, 1, 60)
            retry.MAX_ENTRIES = 1
            for name in ("first", "second"):
                source = str(Path(folder) / name)
                Path(source).write_text("contents")
                retry.failure(source, source + "-new", "permission")
            retry.save()
            self.assertEqual(len(legacy.RetryState(state, 1, 60).entries), 2)


if __name__ == "__main__":
    unittest.main()
