"""Gating, retention scope and cleanup safety of the Neev off-server backup."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

MODULE = Path(__file__).parents[1] / "operations" / "neev_backup.py"
spec = importlib.util.spec_from_file_location("neev_backup", MODULE)
nb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(nb)

SNAP = "a" * 64
DAY = 86400


def sha(data):
    return hashlib.sha256(data).hexdigest()


class FakeWorld:
    """The runner writes a real output; restic 'uploads' by remembering it and
    'restores' by copying real bytes (optionally corrupted)."""

    def __init__(self, staging):
        self.staging = staging
        self.calls = []
        self.fail = set()      # step names that fail
        self.corrupt = None    # "dump" | "blob" | "manifest"
        self.snapshots = {}
        self.counter = 0

    def make_output(self):
        self.counter += 1
        stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime(1_790_000_000 + self.counter * 60)) + "Z"
        out = os.path.join(self.staging, f"neev-content-{stamp}")
        os.makedirs(os.path.join(out, "blobs", "7"))
        dump = b"PGDMP fake dump"
        Path(out, "metadata.dump").write_bytes(dump)
        blobs = []
        for key, data in (("0f8fad5b-d9cb-469f-a165-70867728950e", b"attachment"),
                          ("7c9e6679-7425-40de-944b-e07fc1f90ae7", b"# doc")):
            kind = "att" if data == b"attachment" else "md"
            Path(out, "blobs", "7", f"{key}.{kind}").write_bytes(data)
            blobs.append({"org_id": 7, "key": key, "kind": kind, "sha256": sha(data), "byte_length": len(data)})
        manifest = {"format": "neev-content-copy-v2", "source_schema": "public",
                    "dump": {"sha256": sha(dump), "byte_length": len(dump)}, "tables": [], "blobs": blobs}
        Path(out, "complete.json").write_text(json.dumps(manifest))
        return out

    def step(self, args):
        if args[0] == "/usr/bin/bash":
            return "runner"
        if args[0] == "/usr/bin/pg_restore":
            return "pg_restore"
        for word in ("backup", "check", "restore", "tag", "forget", "cat", "init", "snapshots"):
            if word in args:
                return word
        return "other"

    def __call__(self, args, check=False, **kwargs):
        step = self.step(args)
        self.calls.append((step, list(args)))
        if step in self.fail:
            if check:
                raise subprocess.CalledProcessError(1, args)
            return subprocess.CompletedProcess(args, 1, stdout="", stderr="")
        stdout = ""
        if step == "runner":
            stdout = f"{{\"copied_blobs\": 2}}\ncomplete: {self.make_output()}\n"
        elif step == "backup":
            output = args[-1]
            self.snapshots[SNAP] = output
            stdout = json.dumps({"message_type": "summary", "snapshot_id": SNAP}) + "\n"
            if "no-id" in self.fail:
                stdout = json.dumps({"message_type": "summary"}) + "\n"
        elif step == "snapshots":
            promoted = [] if "unpromoted" in self.fail else [{"id": "b" * 64, "original": SNAP, "tags": ["neev-daily"]}]
            stdout = json.dumps(promoted)
        elif step == "restore":
            target = args[args.index("--target") + 1]
            includes = [args[i + 1] for i, a in enumerate(args) if a == "--include"]
            for path in includes:
                destination = target + path
                os.makedirs(os.path.dirname(destination), exist_ok=True)
                shutil.copyfile(path, destination)
            victim = {"dump": includes[1], "blob": includes[2], "manifest": includes[0]}.get(self.corrupt)
            if victim:
                Path(target + victim).write_bytes(b"corrupted")
        return subprocess.CompletedProcess(args, 0, stdout=stdout, stderr="")

    def steps(self):
        return [step for step, _ in self.calls]


class BackupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(os.path.realpath(self.temp.name))
        for name in ("state", "cache", "staging", "source"):
            (base / name).mkdir()
        (base / "source" / "blob").write_bytes(b"x" * 1000)
        self.base = base
        self.config = {
            "repository": "sftp:workos-storage:neev-backups", "password_file": "/etc/workos-backup/repository-password",
            "ssh_config": "/etc/workos-backup/ssh_config", "ssh_host": "workos-storage",
            "cache_dir": str(base / "cache"), "state_dir": str(base / "state"), "staging_dir": str(base / "staging"),
            "runner": "/opt/neev-content-backup/neev-content-backup-run.sh",
            "node": "/opt/neev-content-backup/node/bin/node",
            "backup_script": "/opt/neev-content-backup/neev-content-backup.mjs",
            "app_container": "neev-app", "source": str(base / "source"), "host": "neev-native",
            "min_free_bytes": 1, "broker_socket": "/run/neev-originals/broker.sock",
            "forbidden_repositories": ["sftp:workos-storage:workos-restic"],
        }
        self.world = FakeWorld(str(base / "staging"))

    def configured(self):
        path = self.base / "config.json"
        path.write_text(json.dumps(self.config))
        os.chmod(path, 0o600)
        return nb.load_config(str(path))

    def backup(self, now=None):
        return nb.backup(self.configured(), run=self.world, now=now, log=lambda *_: None)

    def staged(self):
        return sorted(os.listdir(self.config["staging_dir"]))

    # -- the happy path -------------------------------------------------------
    def test_pipeline_order_and_promotion_of_exactly_the_uploaded_snapshot(self):
        record = self.backup()
        self.assertEqual(self.world.steps(),
                         ["runner", "pg_restore", "backup", "check", "restore", "pg_restore", "tag", "snapshots",
                          "forget"])
        tag = dict(self.world.calls)["tag"]
        self.assertEqual(tag[-6:], ["tag", "--remove", "neev-pending", "--add", "neev-daily", SNAP])
        backup_call = dict(self.world.calls)["backup"]
        self.assertIn("neev-pending", backup_call)
        self.assertTrue(record["ok"] and record["promoted"])
        self.assertEqual(record["promoted_snapshot"], "b" * 64)
        state = json.loads(Path(self.config["state_dir"], "state.json").read_text())
        self.assertEqual([v["snapshot"] for v in state["verified_outputs"].values()], ["b" * 64])
        self.assertEqual(record["canary"]["dump_bytes"], len(b"PGDMP fake dump"))
        self.assertTrue(json.loads(Path(self.config["state_dir"], "last-success.json").read_text())["ok"])

    def test_restic_uses_the_separate_repository_and_existing_ssh_alias(self):
        self.backup()
        for step, args in self.world.calls:
            if args[0].endswith("restic"):
                self.assertEqual(args[args.index("--repo") + 1], "sftp:workos-storage:neev-backups")
                self.assertIn("sftp.command=ssh -F /etc/workos-backup/ssh_config workos-storage -s sftp", args)
                self.assertEqual(args[args.index("--password-file") + 1], "/etc/workos-backup/repository-password")

    def test_retention_is_scoped_to_host_and_promoted_tag(self):
        args = nb.retention_args(self.configured())
        self.assertEqual(args[:7], ["forget", "--host", "neev-native", "--tag", "neev-daily", "--group-by", "host,tags"])
        self.assertEqual(args[7:], ["--keep-daily", "7", "--keep-weekly", "4", "--keep-monthly", "3", "--prune"])
        self.assertNotIn("neev-pending", args)

    # -- failures before cleanup -----------------------------------------------
    def assert_failed_without_promotion_or_cleanup(self, failing_step, expect_steps_absent=("tag", "forget")):
        # Older verified output that a successful run would clean up.
        self.backup()
        self.backup()
        before = self.staged()
        self.world.calls.clear()
        self.world.fail.add(failing_step)
        with self.assertRaises(nb.BackupError):
            self.backup()
        for step in expect_steps_absent:
            self.assertNotIn(step, self.world.steps(), f"{step} ran after {failing_step} failed")
        record = json.loads(Path(self.config["state_dir"], "last-run.json").read_text())
        self.assertFalse(record["ok"])
        self.assertTrue(set(before) <= set(self.staged()), "nothing verified was cleaned up")

    def test_runner_failure_stops_before_any_upload(self):
        self.assert_failed_without_promotion_or_cleanup("runner", ("backup", "tag", "forget"))

    def test_upload_failure_is_not_promoted_or_cleaned(self):
        self.assert_failed_without_promotion_or_cleanup("backup")

    def test_missing_snapshot_id_is_not_promoted(self):
        self.assert_failed_without_promotion_or_cleanup("no-id")

    def test_check_failure_is_not_promoted(self):
        self.assert_failed_without_promotion_or_cleanup("check")

    def test_canary_restore_failure_is_not_promoted(self):
        self.assert_failed_without_promotion_or_cleanup("restore")

    def test_canary_hash_mismatches_are_not_promoted(self):
        for index, (corrupt, reason) in enumerate((("dump", "restored dump hash differs"),
                                                   ("blob", "restored blob hash differs"),
                                                   ("manifest", "restored manifest differs"))):
            with self.subTest(corrupt=corrupt):
                self.world = FakeWorld(self.config["staging_dir"])
                self.world.counter = 100 * (index + 1)  # distinct output names per run
                self.world.corrupt = corrupt
                with self.assertRaisesRegex(nb.BackupError, reason):
                    self.backup()
                self.assertNotIn("tag", self.world.steps())
                self.assertFalse([n for n in self.staged() if n.startswith(".canary-")], "canary scratch removed")

    def test_unconfirmed_promotion_leaves_local_copies(self):
        self.assert_failed_without_promotion_or_cleanup("unpromoted", ("forget",))

    def test_promotion_failure_leaves_local_copies(self):
        self.assert_failed_without_promotion_or_cleanup("tag", ("forget",))

    def test_local_corruption_is_caught_before_upload(self):
        original = self.world.make_output

        def corrupted():
            out = original()
            Path(out, "blobs", "7", "0f8fad5b-d9cb-469f-a165-70867728950e.att").write_bytes(b"changed!!!")
            return out

        self.world.make_output = corrupted
        with self.assertRaises(nb.BackupError):
            self.backup()
        self.assertNotIn("backup", self.world.steps())

    def test_disk_preflight_refuses_before_running_anything(self):
        self.config["min_free_bytes"] = 1 << 62
        with self.assertRaisesRegex(nb.BackupError, "insufficient free disk"):
            self.backup()
        self.assertEqual(self.world.calls, [])

    # -- cleanup ------------------------------------------------------------------
    def test_cleanup_keeps_newest_verified_and_every_unverified_complete_output(self):
        self.backup()
        self.backup()
        self.backup()
        verified = self.staged()
        self.assertEqual(len(verified), 1, "only the newest verified output is kept")
        # An unverified complete output (upload failed) is never deleted later.
        self.world.fail.add("backup")
        with self.assertRaises(nb.BackupError):
            self.backup()
        unverified = [n for n in self.staged() if n not in verified]
        self.world.fail.clear()
        self.backup()
        self.assertIn(unverified[0], self.staged())

    def test_incomplete_outputs_are_removed_only_when_old_and_after_success(self):
        staging = Path(self.config["staging_dir"])
        old = staging / "neev-content-20200101T000000Z"
        old.mkdir()
        (old / "INCOMPLETE").write_text("x")
        os.utime(old, (time.time() - 3 * DAY, time.time() - 3 * DAY))
        fresh = staging / "neev-content-20200102T000000Z"
        fresh.mkdir()
        self.world.fail.add("check")
        with self.assertRaises(nb.BackupError):
            self.backup()
        self.assertTrue(old.exists(), "no cleanup on a failed run")
        self.world.fail.clear()
        self.backup()
        self.assertFalse(old.exists())
        self.assertTrue(fresh.exists())

    def test_cleanup_ignores_foreign_names_and_symlinks(self):
        staging = Path(self.config["staging_dir"])
        (staging / "manual-copy").mkdir()
        outside = self.base / "outside"
        outside.mkdir()
        os.symlink(outside, staging / "neev-content-20200101T000000Z")
        self.backup()
        self.backup()
        self.assertTrue((staging / "manual-copy").exists())
        self.assertTrue(outside.exists())

    # -- configuration and locking ------------------------------------------------
    def test_refuses_the_workos_repository_and_public_config(self):
        self.config["repository"] = "sftp:workos-storage:workos-restic"
        with self.assertRaisesRegex(nb.BackupError, "another job"):
            self.configured()
        self.config["repository"] = "sftp:workos-storage:neev-backups"
        path = self.base / "config.json"
        path.write_text(json.dumps(self.config))
        os.chmod(path, 0o644)
        with self.assertRaises(nb.BackupError):
            nb.load_config(str(path))

    def test_init_never_reinitialises_an_existing_repository(self):
        path = self.base / "config.json"
        path.write_text(json.dumps(self.config))
        os.chmod(path, 0o600)
        self.assertEqual(nb.main(["--config", str(path), "init"], run=self.world), 1)  # cat config ok
        self.assertNotIn("init", self.world.steps())
        self.world.fail.add("cat")
        self.assertEqual(nb.main(["--config", str(path), "init"], run=self.world), 0)
        self.assertIn("init", self.world.steps())

    def test_concurrent_runs_are_refused(self):
        import fcntl
        path = self.base / "config.json"
        path.write_text(json.dumps(self.config))
        os.chmod(path, 0o600)
        with open(Path(self.config["state_dir"], "lock"), "a") as holder:
            fcntl.flock(holder, fcntl.LOCK_EX)
            self.assertEqual(nb.main(["--config", str(path), "backup"], run=self.world), 1)
        self.assertEqual(self.world.calls, [])


if __name__ == "__main__":
    unittest.main()
