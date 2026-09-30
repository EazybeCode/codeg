"""Safety contracts for Storage Box replication: copy + verify, never delete."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

MODULE = Path(__file__).parents[1] / "operations" / "neev_storage_box.py"
spec = importlib.util.spec_from_file_location("neev_storage_box", MODULE)
box = importlib.util.module_from_spec(spec)
spec.loader.exec_module(box)

KEY_A = "0f8fad5b-d9cb-469f-a165-70867728950e"
KEY_B = "7c9e6679-7425-40de-944b-e07fc1f90ae7"


class FakeStorageBox:
    """Simulates rsync + the port-23 restricted shell against a local directory."""

    def __init__(self, local_root, remote_home, available=True):
        self.local_root = local_root
        self.home = remote_home
        self.available = available
        self.calls = []
        self.corrupt = set()

    def __call__(self, args, **kwargs):
        self.calls.append(list(args))
        if args[0] == "ssh-keygen":
            return subprocess.run(args, **kwargs)
        if not self.available:
            return subprocess.CompletedProcess(args, 255, stdout="", stderr="unreachable")
        if args[0] == "rsync":
            listing = next(a for a in args if a.startswith("--files-from="))
            target = self.home / args[-1].split(":", 1)[1]
            code = 0
            for rel in Path(listing.split("=", 1)[1]).read_text().split():
                destination = target / rel
                destination.parent.mkdir(parents=True, exist_ok=True)
                if destination.exists() and "--ignore-existing" in args:
                    continue
                source = Path(self.local_root) / rel
                if not source.exists():
                    code = 24  # rsync: some source files vanished
                    continue
                destination.write_bytes(b"corrupt" if rel in self.corrupt else source.read_bytes())
            return subprocess.CompletedProcess(args, code, stdout="", stderr="")
        words = args[args.index("--") + 1:]
        if words[0] == "sha256sum":
            out, code = [], 0
            for rel in words[1:]:
                path = self.home / rel
                if path.exists():
                    out.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {rel}")
                else:
                    code = 1
            return subprocess.CompletedProcess(args, code, stdout="\n".join(out) + "\n", stderr="")
        return subprocess.CompletedProcess(args, 0, stdout="", stderr="")


class StorageBoxTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(os.path.realpath(self.temp.name))
        self.root = base / "documents"
        self.state = base / "state"
        self.remote = base / "remote"
        for directory in (self.root, self.state, self.remote):
            directory.mkdir(mode=0o700)
            os.chmod(directory, 0o700)
        self.blob(7, KEY_A + ".att", b"attachment bytes")
        self.blob(7, KEY_B + ".md", b"# doc")
        self.blob(12, KEY_A + ".img", b"\x89PNG")
        identity = base / "id_ed25519"
        identity.write_text("private")
        os.chmod(identity, 0o600)
        known = base / "known_hosts"
        known.write_text("")
        os.chmod(known, 0o600)
        self.config = {
            "host": "u123456-sub1.your-storagebox.de",
            "user": "u123456-sub1",
            "identity_file": str(identity),
            "known_hosts_file": str(known),
            "remote_root": "neev-originals",
            "local_root": str(self.root),
            "state_dir": str(self.state),
        }
        self.box = FakeStorageBox(self.root, self.remote)
        self.logs = []

    def blob(self, org, name, data):
        org_dir = self.root / str(org)
        org_dir.mkdir(mode=0o700, exist_ok=True)
        os.chmod(org_dir, 0o700)
        path = org_dir / name
        path.write_bytes(data)
        os.chmod(path, 0o600)
        return path

    def replicate(self, **kwargs):
        return box.replicate(self.config, run=self.box, log=self.logs.append, **kwargs)

    def ledger(self):
        return json.loads((self.state / "verified.json").read_text())["verified"]

    def local_snapshot(self):
        return {str(p.relative_to(self.root)): p.read_bytes() for p in self.root.rglob("*") if p.is_file()}

    def write_config(self, **overrides):
        path = Path(self.state) / "config.json"
        path.write_text(json.dumps({**self.config, **overrides}))
        os.chmod(path, 0o600)
        return str(path)

    # --- configuration -----------------------------------------------------
    def test_config_accepts_sub_account_and_rejects_unsafe_settings(self):
        self.assertEqual(box.load_config(self.write_config())["user"], "u123456-sub1")
        for bad in ({"port": 22}, {"remote_root": "/abs"}, {"remote_root": "a/../b"},
                    {"user": "u123456", "host": "u123456.your-storagebox.de"},
                    {"host": "evil.example.com"}, {"local_root": "relative"},
                    {"extra": 1}):
            with self.subTest(bad=bad), self.assertRaises(box.ConfigError):
                box.load_config(self.write_config(**bad))
        main = self.write_config(user="u123456", host="u123456.your-storagebox.de", allow_main_account=True)
        self.assertEqual(box.load_config(main)["user"], "u123456")

    def test_config_file_and_key_must_be_private(self):
        path = self.write_config()
        os.chmod(path, 0o640)
        with self.assertRaises(box.ConfigError):
            box.load_config(path)
        os.chmod(path, 0o600)
        os.chmod(self.config["identity_file"], 0o644)
        with self.assertRaises(box.ConfigError):
            box.load_config(path)

    def test_ssh_is_strict_batch_and_ignores_user_config(self):
        command = " ".join(box.remote_command(self.config, ["ls"]))
        for required in ("-F /dev/null", "-p 23", "StrictHostKeyChecking=yes", "BatchMode=yes",
                         "IdentitiesOnly=yes", "ForwardAgent=no", "UpdateHostKeys=no",
                         "UserKnownHostsFile=" + self.config["known_hosts_file"]):
            self.assertIn(required, command)
        self.assertNotIn("accept-new", command)

    # --- host key pinning --------------------------------------------------
    def pin_generated_key(self):
        key = Path(self.state) / "hostkey"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
        public = (key.with_suffix(".pub")).read_text().split()
        Path(self.config["known_hosts_file"]).write_text(
            f"[{self.config['host']}]:23 {public[0]} {public[1]}\n")
        listed = subprocess.run(["ssh-keygen", "-l", "-E", "sha256", "-f", str(key.with_suffix(".pub"))],
                                stdout=subprocess.PIPE, text=True, check=True)
        return listed.stdout.split()[1]

    @unittest.skipUnless(shutil.which("ssh-keygen"), "ssh-keygen required")
    def test_known_hosts_requires_pinned_published_key_and_never_scans(self):
        with self.assertRaisesRegex(box.ConfigError, "no pinned entry"):
            box.check_known_hosts(self.config, run=self.box)
        fingerprint = self.pin_generated_key()
        with self.assertRaisesRegex(box.ConfigError, "not in Hetzner"):
            box.check_known_hosts(self.config, run=self.box)
        original = dict(box.HETZNER_STORAGE_BOX_FINGERPRINTS)
        box.HETZNER_STORAGE_BOX_FINGERPRINTS[fingerprint] = "ED25519"
        try:
            self.assertEqual(box.check_known_hosts(self.config, run=self.box), [fingerprint])
        finally:
            box.HETZNER_STORAGE_BOX_FINGERPRINTS.clear()
            box.HETZNER_STORAGE_BOX_FINGERPRINTS.update(original)
        self.assertFalse([c for c in self.box.calls if c[0] in ("ssh-keyscan", "ssh")])

    # --- local inventory ---------------------------------------------------
    def test_inventory_hashes_finals_and_skips_unpublished_temporaries(self):
        self.blob(7, KEY_A + ".att.tmp", b"half")
        found = {b["path"]: b for b in box.inventory(str(self.root))}
        self.assertEqual(sorted(found), sorted([f"7/{KEY_A}.att", f"7/{KEY_B}.md", f"12/{KEY_A}.img"]))
        self.assertEqual(found[f"7/{KEY_A}.att"]["sha256"], hashlib.sha256(b"attachment bytes").hexdigest())

    def test_inventory_fails_closed_on_contract_violations(self):
        cases = []
        cases.append(lambda: self.blob(7, "notes.txt", b"x"))
        cases.append(lambda: os.chmod(self.blob(7, KEY_B + ".att", b"x"), 0o644))
        cases.append(lambda: os.link(self.root / "7" / (KEY_A + ".att"), self.root / "7" / (KEY_B + ".img")))
        cases.append(lambda: os.symlink("/etc/hosts", self.root / "7" / (KEY_B + ".img")))
        cases.append(lambda: (self.root / "orgx").mkdir(mode=0o700))
        cases.append(lambda: os.chmod(self.root / "12", 0o755))
        for index, damage in enumerate(cases):
            with self.subTest(case=index):
                self.setUp()
                damage()
                with self.assertRaises(box.LocalIntegrityError):
                    box.inventory(str(self.root))

    # --- replication -------------------------------------------------------
    def test_replicate_copies_per_org_verifies_and_records(self):
        before = self.local_snapshot()
        result = self.replicate()
        self.assertEqual(result["verified"], 3)
        self.assertEqual((self.remote / "neev-originals" / "7" / (KEY_A + ".att")).read_bytes(), b"attachment bytes")
        self.assertEqual((self.remote / "neev-originals" / "12" / (KEY_A + ".img")).read_bytes(), b"\x89PNG")
        self.assertEqual(set(self.ledger()), {f"7/{KEY_A}.att", f"7/{KEY_B}.md", f"12/{KEY_A}.img"})
        self.assertEqual(self.local_snapshot(), before, "replication never changes local originals")
        # Second run has nothing pending and makes no remote calls.
        self.box.calls.clear()
        self.assertEqual(self.replicate()["pending"], 0)
        self.assertEqual(self.box.calls, [])

    def test_remote_unavailable_fails_safe(self):
        self.box.available = False
        before = self.local_snapshot()
        with self.assertRaises(box.RemoteUnavailable):
            self.replicate()
        self.assertFalse((self.state / "verified.json").exists())
        self.assertEqual(self.local_snapshot(), before)

    def test_mismatch_is_not_recorded_and_existing_remote_is_never_overwritten(self):
        self.box.corrupt.add(f"7/{KEY_A}.att")
        with self.assertRaises(box.RemoteMismatch):
            self.replicate()
        self.assertNotIn(f"7/{KEY_A}.att", self.ledger())
        self.assertIn(f"7/{KEY_B}.md", self.ledger())
        # A retry does not replace the conflicting remote file; it keeps failing.
        self.box.corrupt.clear()
        with self.assertRaises(box.RemoteMismatch):
            self.replicate()
        self.assertEqual((self.remote / "neev-originals" / "7" / (KEY_A + ".att")).read_bytes(), b"corrupt")

    def test_blob_swept_during_run_is_skipped_not_failed(self):
        vanished = self.root / "7" / (KEY_B + ".md")
        original = self.box.__call__

        def sweeping(args, **kwargs):
            if args[0] == "rsync" and vanished.exists():
                vanished.unlink()
            return original(args, **kwargs)

        result = box.replicate(self.config, run=sweeping, log=self.logs.append)
        self.assertEqual(result["pending"], 3)
        # Only files whose remote hash matched count as verified.
        self.assertEqual((result["verified"], result["vanished"]), (2, 1))
        self.assertEqual(len(self.ledger()), 2)
        self.assertNotIn(f"7/{KEY_B}.md", self.ledger())

    def test_dry_run_and_limit(self):
        self.assertEqual(self.replicate(dry_run=True)["pending"], 3)
        self.assertEqual(self.box.calls, [])
        self.replicate(limit=1)
        self.assertEqual(len(self.ledger()), 1)

    def test_no_command_ever_removes_or_moves_originals(self):
        self.replicate()
        box.verify(self.config, run=self.box, log=self.logs.append)
        box.status(self.config, log=self.logs.append)
        for call in self.box.calls:
            if call[0] == "ssh":
                self.assertNotIn(call[call.index("--") + 1], ("rm", "mv", "unlink", "rmdir"))
            self.assertNotIn("--delete", call)
            self.assertNotIn("--remove-source-files", call)

    def test_verify_detects_remote_loss(self):
        self.replicate()
        (self.remote / "neev-originals" / "12" / (KEY_A + ".img")).unlink()
        with self.assertRaisesRegex(box.RemoteMismatch, "1 of 3"):
            box.verify(self.config, run=self.box, log=self.logs.append)

    def test_status_reports_coverage_and_eviction_configuration(self):
        self.replicate(limit=2)
        summary = box.status(self.config, log=self.logs.append)
        self.assertEqual((summary["verified_remote"], summary["pending"]), (2, 1))
        self.assertEqual(summary["eviction"], "not configured")

    def test_ledger_write_is_atomic(self):
        self.replicate()
        before = (self.state / "verified.json").read_text()
        ledger = box.Ledger(str(self.state))
        ledger.entries["bad"] = {"sha256": object()}  # not JSON serialisable
        with self.assertRaises(TypeError):
            ledger.save()
        self.assertEqual((self.state / "verified.json").read_text(), before)
        self.assertEqual([p.name for p in self.state.iterdir() if p.name.startswith(".verified")], [])

    def test_main_maps_failures_to_exit_codes(self):
        path = self.write_config()
        self.box.available = False
        self.assertEqual(box.main(["--config", path, "replicate"], run=self.box), 3)
        self.box.available = True
        self.assertEqual(box.main(["--config", path, "replicate"], run=self.box), 0)
        self.assertEqual(box.main(["--config", path, "verify", "--sample", "2"], run=self.box), 0)
        self.assertEqual(box.main(["--config", path + ".missing", "status"], run=self.box), 1)


if __name__ == "__main__":
    unittest.main()
