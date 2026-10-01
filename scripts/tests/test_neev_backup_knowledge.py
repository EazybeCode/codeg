"""Schema-22 knowledge blobs in the Neev off-server backup (content backup v3).

The host orchestrator must pass the knowledge root to the runner, verify every
knowledge blob locally against the manifest, prove one of them in the restore
canary, and refuse an output that silently lacks the configured knowledge root.
v2 outputs (no knowledge) remain acceptable when knowledge is not configured.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

OPERATIONS = Path(__file__).parents[1] / "operations"
tests_spec = importlib.util.spec_from_file_location("test_neev_backup", Path(__file__).parent / "test_neev_backup.py")
base_tests = importlib.util.module_from_spec(tests_spec)
tests_spec.loader.exec_module(base_tests)
# One module instance, so BackupError is the class the borrowed fixture raises.
nb = base_tests.nb

SNAP_KEY = "a0000000-0000-4000-8000-00000000000a"
REV_KEY = "b0000000-0000-4000-8000-00000000000b"


def sha(data):
    return hashlib.sha256(data).hexdigest()


class KnowledgeWorld(base_tests.FakeWorld):
    """The runner writes a v3 output; `knowledge` controls its knowledge section."""

    def __init__(self, staging, knowledge="blobs"):
        super().__init__(staging)
        self.knowledge = knowledge
        self.runner_env = None

    def make_output(self):
        out = super().make_output()
        manifest = json.loads(Path(out, "complete.json").read_text())
        manifest["format"] = "neev-content-copy-v3"
        if self.knowledge == "v2":
            manifest["format"] = "neev-content-copy-v2"
        elif self.knowledge == "not configured":
            manifest["knowledge"] = "not configured"
        else:
            blobs = []
            for key, data in ((SNAP_KEY, b"## snapshot\n"), (REV_KEY, b"# Page\n")):
                os.makedirs(os.path.join(out, "blobs-knowledge", "3"), exist_ok=True)
                Path(out, "blobs-knowledge", "3", f"{key}.md").write_bytes(data)
                blobs.append({"org_id": 3, "key": key, "sha256": sha(data), "byte_length": len(data)})
            manifest["knowledge"] = {"tables": ["neev_knowledge_revisions", "neev_knowledge_snapshots"],
                                     "blobs": blobs}
        Path(out, "complete.json").write_text(json.dumps(manifest))
        return out

    def __call__(self, args, check=False, **kwargs):
        if self.step(args) == "runner":
            self.runner_env = dict(kwargs.get("env") or {})
        result = super().__call__(args, check=check, **kwargs)
        if self.step(args) == "restore" and self.corrupt == "knowledge":
            target = args[args.index("--target") + 1]
            includes = [args[i + 1] for i, a in enumerate(args) if a == "--include"]
            knowledge = [p for p in includes if "/blobs-knowledge/" in p]
            for path in knowledge:
                # Same length, different bytes: exercises the hash check, not the length check.
                original = Path(target + path).read_bytes()
                Path(target + path).write_bytes(bytes(b ^ 0xFF for b in original))
        return result


class KnowledgeBackupTests(unittest.TestCase):
    """Borrows the base fixture (config, staging) with a knowledge-aware world,
    without re-running the base module's tests."""

    configured = base_tests.BackupTests.configured
    backup = base_tests.BackupTests.backup

    def setUp(self):
        base_tests.BackupTests.setUp(self)
        knowledge = self.base / "knowledge"
        (knowledge / "3").mkdir(parents=True)
        (knowledge / "3" / f"{SNAP_KEY}.md").write_bytes(b"k" * 5000)
        self.config["knowledge_source"] = str(knowledge)
        self.world = KnowledgeWorld(str(self.base / "staging"))

    def test_runner_receives_the_knowledge_root_only_through_the_environment(self):
        self.backup()
        self.assertEqual(self.world.runner_env["NEEV_BACKUP_KNOWLEDGE_SOURCE"], self.config["knowledge_source"])
        runner_args = [args for step, args in self.world.calls if step == "runner"][0]
        self.assertEqual(runner_args, ["/usr/bin/bash", self.config["runner"]])

    def test_without_knowledge_configured_the_runner_gets_no_knowledge_variable(self):
        del self.config["knowledge_source"]
        self.world.knowledge = "not configured"
        self.backup()
        self.assertNotIn("NEEV_BACKUP_KNOWLEDGE_SOURCE", self.world.runner_env)

    def test_v3_output_is_verified_and_recorded(self):
        record = self.backup()
        self.assertTrue(record["ok"])
        self.assertEqual(record["knowledge_blobs"], 2)
        self.assertEqual(record["knowledge_bytes"], len(b"## snapshot\n") + len(b"# Page\n"))

    def test_the_restore_canary_proves_one_knowledge_blob(self):
        record = self.backup()
        restore = [args for step, args in self.world.calls if step == "restore"][0]
        includes = [restore[i + 1] for i, a in enumerate(restore) if a == "--include"]
        self.assertEqual(len([p for p in includes if "/blobs-knowledge/3/" in p]), 1)
        self.assertIn(record["canary"]["knowledge_bytes"], (len(b"## snapshot\n"), len(b"# Page\n")))

    def test_a_corrupted_restored_knowledge_blob_is_not_promoted(self):
        self.world.corrupt = "knowledge"
        with self.assertRaisesRegex(nb.BackupError, "restored knowledge blob hash differs"):
            self.backup()
        self.assertNotIn("tag", self.world.steps())

    def test_a_locally_corrupted_knowledge_blob_stops_before_upload(self):
        original = self.world.make_output

        def corrupted():
            out = original()
            Path(out, "blobs-knowledge", "3", f"{REV_KEY}.md").write_bytes(b"# Pwnd\n")
            return out

        self.world.make_output = corrupted
        with self.assertRaisesRegex(nb.BackupError, "knowledge blob does not match"):
            self.backup()
        self.assertNotIn("backup", self.world.steps())

    def test_configured_knowledge_with_an_output_that_lacks_it_is_refused(self):
        for index, shape in enumerate(("not configured", "v2")):
            with self.subTest(shape=shape):
                self.world = KnowledgeWorld(str(self.base / "staging"), knowledge=shape)
                self.world.counter = 100 * (index + 1)  # distinct output names
                with self.assertRaisesRegex(nb.BackupError, "lacks the configured knowledge root"):
                    self.backup()
                self.assertNotIn("backup", self.world.steps())

    def test_v2_outputs_remain_valid_when_knowledge_is_not_configured(self):
        del self.config["knowledge_source"]
        self.world.knowledge = "v2"
        record = self.backup()
        self.assertTrue(record["ok"])
        self.assertEqual(record["knowledge_blobs"], 0)

    def test_malformed_knowledge_sections_are_refused(self):
        output = self.world.make_output()
        manifest_path = Path(output, "complete.json")
        good = json.loads(manifest_path.read_text())
        for bad in ({"tables": [], "blobs": [{"org_id": 3, "key": "../x", "sha256": "0" * 64, "byte_length": 1}]},
                    {"tables": [], "blobs": [{"org_id": "3", "key": SNAP_KEY, "sha256": "0" * 64, "byte_length": 1}]},
                    {"blobs": []}, "configured", None):
            with self.subTest(bad=bad):
                manifest_path.write_text(json.dumps({**good, "knowledge": bad}))
                with self.assertRaises(nb.BackupError):
                    nb.verify_local(output, self.world, knowledge_expected=True)

    def manifest_with(self, blobs):
        return {"format": "neev-content-copy-v3", "blobs": [{}],
                "knowledge": {"tables": ["neev_knowledge_snapshots"], "blobs": blobs}}

    def entry(self, key=SNAP_KEY, byte_length=1, org_id=3):
        return {"org_id": org_id, "key": key, "sha256": "0" * 64, "byte_length": byte_length}

    def test_knowledge_entries_are_bounded_by_the_schema_22_cap(self):
        # Task 1 DDL: snapshots and revisions are both <= 65536 bytes.
        self.assertEqual(len(nb.knowledge_blobs(self.manifest_with([self.entry(byte_length=65536)]))), 1)
        self.assertEqual(len(nb.knowledge_blobs(self.manifest_with([self.entry(byte_length=0)]))), 1)
        for size in (65537, 1 << 20):
            with self.subTest(size=size), self.assertRaises(nb.BackupError):
                nb.knowledge_blobs(self.manifest_with([self.entry(byte_length=size)]))

    def test_duplicate_knowledge_entries_are_refused(self):
        with self.assertRaisesRegex(nb.BackupError, "duplicate"):
            nb.knowledge_blobs(self.manifest_with([self.entry(), self.entry()]))
        # Same key under two orgs is two distinct blobs.
        self.assertEqual(len(nb.knowledge_blobs(self.manifest_with([self.entry(), self.entry(org_id=4)]))), 2)

    def test_the_canary_checks_the_restored_knowledge_length_as_well_as_its_hash(self):
        self.backup()
        output = next(p for p in (self.base / "staging").iterdir() if p.name.startswith("neev-content-"))
        manifest = json.loads((output / "complete.json").read_text())
        blob = dict(nb.knowledge_blobs(manifest)[0])
        blob["byte_length"] += 1  # right hash, wrong length
        with self.assertRaisesRegex(nb.BackupError, "restored knowledge blob size differs"):
            nb.restore_canary(self.configured(), "c" * 64, str(output), manifest, self.world,
                              knowledge_blob=blob)

    def test_disk_preflight_counts_the_knowledge_root(self):
        config = self.configured()
        with_knowledge = nb.preflight_disk(config)["estimate_bytes"]
        no_knowledge = nb.preflight_disk({k: v for k, v in config.items() if k != "knowledge_source"})[
            "estimate_bytes"]
        self.assertEqual(with_knowledge - no_knowledge, 5000)


class RunnerScriptTests(unittest.TestCase):
    """neev-content-backup-run.sh runs as root and drops to the app uid, so it is
    checked structurally here; the backup flags it builds are exercised by the
    Node CLI tests."""

    def setUp(self):
        self.text = (OPERATIONS / "neev-content-backup-run.sh").read_text()

    def test_syntax(self):
        subprocess.run(["bash", "-n", str(OPERATIONS / "neev-content-backup-run.sh")], check=True)

    def test_knowledge_root_is_read_before_the_environment_is_cleared_and_passed_as_a_flag(self):
        read = self.text.index("NEEV_BACKUP_KNOWLEDGE_SOURCE")
        wipe = self.text.index("compgen -e")
        self.assertLess(read, wipe, "the variable must be captured before the clean-environment subshell")
        self.assertRegex(self.text, r'--knowledge-source "\$KNOWLEDGE"')
        # Optional: no flag at all when unset, so pre-22 hosts keep working.
        self.assertRegex(self.text, r'\[ -n "\$KNOWLEDGE" \]')

    def test_knowledge_root_must_be_absolute(self):
        self.assertRegex(self.text, r'KNOWLEDGE.*\n.*/\*\)')


if __name__ == "__main__":
    unittest.main()
