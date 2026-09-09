"""Safety contracts: stale/partial sources and failed uploads never trigger pruning."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

MODULE = Path(__file__).parents[1] / "operations" / "workos_backup.py"


def record_restic(calls):
    # External archive tools and Restic are exercised for real in the host rehearsal.
    def run(args, **kwargs):
        if args[0] == "/usr/bin/restic":
            calls.append(args)
        return subprocess.CompletedProcess(args, 0, stdout='{"message_type":"summary","snapshot_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}\n')
    return run


class BackupTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue(MODULE.exists(), "backup workflow has not been implemented")
        spec = importlib.util.spec_from_file_location("workos_backup", MODULE)
        self.backup = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.backup)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.now = 200000.0
        self.config = {
            "repository": "sftp:backup:workos",
            "password_file": str(self.root / "password"),
            "ssh_config": str(self.root / "ssh_config"),
            "cache_dir": str(self.root / "cache"),
            "state_dir": str(self.root / "state"),
            "host": "workos-test",
            "sources": [],
        }
        for label in ("postgres", "data", "claude", "codex"):
            folder = self.root / label
            folder.mkdir()
            path = folder / "latest.archive"
            path.write_bytes(b"completed archive")
            os.utime(path, (self.now - 120, self.now - 120))
            self.config["sources"].append({"label": label, "directory": str(folder), "pattern": "*.archive"})

    def test_selects_latest_file_in_each_exact_source_directory(self):
        older = self.root / "postgres" / "older.archive"
        older.write_bytes(b"older")
        os.utime(older, (1, 1))
        paths = self.backup.select_sources(self.config, self.now)
        self.assertEqual([p.name for p in paths], ["latest.archive"] * 4)

    def test_missing_source_is_rejected(self):
        (self.root / "data" / "latest.archive").unlink()
        with self.assertRaises(ValueError):
            self.backup.select_sources(self.config, self.now)

    def test_empty_newest_file_is_not_replaced_with_older_success(self):
        path = self.root / "data" / "new.archive"
        path.touch()
        os.utime(path, (self.now - 70, self.now - 70))
        with self.assertRaises(ValueError):
            self.backup.select_sources(self.config, self.now)

    def test_stale_source_is_rejected(self):
        os.utime(self.root / "postgres" / "latest.archive", (1, 1))
        with self.assertRaises(ValueError):
            self.backup.select_sources(self.config, self.now)

    def test_recently_written_source_is_rejected(self):
        os.utime(self.root / "data" / "latest.archive", (self.now, self.now))
        with self.assertRaises(ValueError):
            self.backup.select_sources(self.config, self.now)

    def test_future_timestamp_is_rejected(self):
        os.utime(self.root / "data" / "latest.archive", (self.now + 500, self.now + 500))
        with self.assertRaises(ValueError):
            self.backup.select_sources(self.config, self.now)

    def test_symlink_cannot_import_unrelated_files(self):
        path = self.root / "data" / "latest.archive"
        path.unlink()
        path.symlink_to(self.root / "postgres" / "latest.archive")
        with self.assertRaises(ValueError):
            self.backup.select_sources(self.config, self.now)

    def test_duplicate_or_missing_coverage_is_rejected(self):
        self.config["sources"][0]["label"] = "data"
        with self.assertRaises(ValueError):
            self.backup.select_sources(self.config, self.now)

    def test_dry_run_never_uploads_or_prunes(self):
        calls = []
        self.backup.run_backup(self.config, self.now, dry_run=True, run=record_restic(calls))
        self.assertEqual(len(calls), 1)
        self.assertIn("--dry-run", calls[0])
        self.assertNotIn("forget", calls[0])

    def test_failed_upload_does_not_check_or_prune_or_record_success(self):
        calls = []
        def fail(args, **kwargs):
            if args[0] == "/usr/bin/restic":
                calls.append(args)
                raise subprocess.CalledProcessError(3, args)
        with self.assertRaises(subprocess.CalledProcessError):
            self.backup.run_backup(self.config, self.now, run=fail)
        self.assertEqual(len(calls), 1)
        self.assertFalse((self.root / "state" / "last-success.json").exists())

    def test_no_retention_before_restore_verification(self):
        calls = []
        self.backup.run_backup(self.config, self.now, run=record_restic(calls))
        self.assertEqual(len(calls), 3)
        self.assertIn("backup", calls[0])
        self.assertIn("check", calls[1])
        self.assertIn("tag", calls[2])
        self.assertTrue((self.root / "state" / "last-success.json").exists())

    def test_failed_integrity_check_cannot_prune(self):
        calls = []
        def check_fails(args, **kwargs):
            if args[0] == "/usr/bin/restic":
                calls.append(args)
            if "check" in args:
                raise subprocess.CalledProcessError(1, args)
            return subprocess.CompletedProcess(args, 0, stdout='{"message_type":"summary","snapshot_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}\n')
        with self.assertRaises(subprocess.CalledProcessError):
            self.backup.run_backup(self.config, self.now, run=check_fails)
        self.assertEqual(len(calls), 2)
        self.assertFalse((self.root / "state" / "last-success.json").exists())

    def test_verified_retention_is_scoped_to_this_host_and_tag(self):
        state = self.root / "state"
        state.mkdir()
        (state / "restore-verified").write_text(self.config["repository"] + "\n")
        calls = []
        self.backup.run_backup(self.config, self.now, run=record_restic(calls))
        policy = calls[3]
        for value in ("forget", "--prune", "--keep-daily", "7", "--keep-weekly", "4", "--keep-monthly", "3", "--host", "workos-test", "--tag", "workos-daily", "host,tags"):
            self.assertIn(value, policy)

    def test_changed_repository_requires_new_restore_verification(self):
        state = self.root / "state"
        state.mkdir()
        (state / "restore-verified").write_text("sftp:other:repo\n")
        calls = []
        self.backup.run_backup(self.config, self.now, run=record_restic(calls))
        self.assertEqual(len(calls), 3)

    def test_different_labels_cannot_point_at_the_same_source(self):
        self.config["sources"][1]["directory"] = self.config["sources"][0]["directory"]
        with self.assertRaises(ValueError):
            self.backup.select_sources(self.config, self.now)

    def test_hardlinked_archives_cannot_count_as_distinct_coverage(self):
        path = self.root / "data" / "latest.archive"
        path.unlink()
        os.link(self.root / "postgres" / "latest.archive", path)
        with self.assertRaises(ValueError):
            self.backup.select_sources(self.config, self.now)

    def test_invalid_archive_never_reaches_restic(self):
        calls = []
        def reject_archive(args, **kwargs):
            calls.append(args)
            if args[0] == "/usr/bin/pg_restore":
                raise subprocess.CalledProcessError(1, args)
        with self.assertRaises(subprocess.CalledProcessError):
            self.backup.run_backup(self.config, self.now, run=reject_archive)
        self.assertFalse(any("backup" in args for args in calls))

    def test_source_changed_during_validation_cannot_upload(self):
        calls = []
        def mutate_during_validation(args, **kwargs):
            calls.append(args)
            if args[0] == "/usr/bin/pg_restore":
                (self.root / "postgres" / "latest.archive").write_bytes(b"writer resumed")
        with self.assertRaises(ValueError):
            self.backup.run_backup(self.config, self.now, run=mutate_during_validation)
        self.assertFalse(any("backup" in args for args in calls))

    def test_source_changed_during_upload_cannot_check_or_prune(self):
        calls = []
        def mutate_during_upload(args, **kwargs):
            calls.append(args)
            if "backup" in args:
                (self.root / "postgres" / "latest.archive").write_bytes(b"writer resumed")
        with self.assertRaises(ValueError):
            self.backup.run_backup(self.config, self.now, run=mutate_during_upload)
        self.assertFalse(any("check" in args or "forget" in args for args in calls))

    def test_failed_snapshot_is_not_counted_by_next_runs_retention(self):
        state = self.root / "state"
        state.mkdir()
        (state / "restore-verified").write_text(self.config["repository"] + "\n")
        snapshots = {}
        count = 0
        retained_candidates = []
        def backend(args, **kwargs):
            nonlocal count
            if args[0] != "/usr/bin/restic":
                return subprocess.CompletedProcess(args, 0)
            if "backup" in args:
                count += 1
                snapshot = ("a" if count == 1 else "b") * 64
                snapshots[snapshot] = args[args.index("--tag") + 1]
                if count == 1:
                    (self.root / "postgres" / "latest.archive").write_bytes(b"writer resumed")
                return subprocess.CompletedProcess(args, 0, stdout='{"message_type":"summary","snapshot_id":"' + snapshot + '"}\n')
            if "tag" in args:
                snapshots[args[-1]] = args[args.index("--add") + 1]
            if "forget" in args:
                wanted = args[args.index("--tag") + 1]
                retained_candidates.extend(key for key, tag in snapshots.items() if tag == wanted)
            return subprocess.CompletedProcess(args, 0)
        with self.assertRaises(ValueError):
            self.backup.run_backup(self.config, self.now, run=backend)
        os.utime(self.root / "postgres" / "latest.archive", (self.now - 120, self.now - 120))
        self.backup.run_backup(self.config, self.now, run=backend)
        self.assertEqual(retained_candidates, ["b" * 64])
        self.assertEqual(snapshots["a" * 64], "workos-pending")

    def test_missing_snapshot_id_cannot_promote_or_prune(self):
        calls = []
        def no_summary(args, **kwargs):
            calls.append(args)
            return subprocess.CompletedProcess(args, 0, stdout="{}\n")
        with self.assertRaises(ValueError):
            self.backup.run_backup(self.config, self.now, run=no_summary)
        self.assertFalse(any("tag" in args or "forget" in args for args in calls))


if __name__ == "__main__":
    unittest.main()
