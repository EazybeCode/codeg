"""The org-knowledge Storage Box replica: the unchanged neev_storage_box.py with
a second config, so knowledge is copied and verified in its own remote folder
(`neev-knowledge/`) and is never evicted from the server.

These tests pin the units to the originals line by line (only the documented
lines may differ), load the example config through the real loader, and prove
that eviction and purge cannot act on it.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest

OPERATIONS = Path(__file__).parents[1] / "operations"
spec = importlib.util.spec_from_file_location("neev_storage_box", OPERATIONS / "neev_storage_box.py")
sb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sb)

CONFIG = "/etc/neev-storage-box/knowledge.json"
STATE = "/var/lib/neev-storage-box-knowledge"
LOCAL_ROOT = "/var/lib/neev-native/knowledge"


def lines(name):
    return (OPERATIONS / name).read_text().splitlines()


def differing(original, knowledge):
    a, b = lines(original), lines(knowledge)
    self_check = len(a) == len(b)
    return self_check, [(x, y) for x, y in zip(a, b) if x != y]


class UnitTests(unittest.TestCase):
    def assertOnlyChanged(self, original, knowledge, allowed_prefixes):
        same_length, changes = differing(original, knowledge)
        self.assertTrue(same_length, f"{knowledge} must keep every line of {original}")
        for old, new in changes:
            self.assertTrue(any(new.startswith(p) and old.startswith(p) for p in allowed_prefixes),
                            f"unexpected change in {knowledge}: {old!r} -> {new!r}")
        return {new.split("=", 1)[0]: new.split("=", 1)[1] for _, new in changes if not new.startswith("#")}

    def test_replicate_service_differs_only_in_description_config_and_state(self):
        changed = self.assertOnlyChanged("neev-storage-box.service", "neev-knowledge-storage-box.service",
                                         ("Description=", "ExecStart=", "ReadWritePaths="))
        self.assertEqual(set(changed), {"Description", "ExecStart", "ReadWritePaths"})
        self.assertIn(f"--config {CONFIG} replicate", changed["ExecStart"])
        self.assertEqual(changed["ReadWritePaths"], STATE)

    def test_verify_service_differs_only_in_description_config_state_and_its_comment(self):
        changed = self.assertOnlyChanged("neev-storage-box-verify.service",
                                         "neev-knowledge-storage-box-verify.service",
                                         ("Description=", "ExecStart=", "ReadWritePaths=", "# "))
        self.assertIn(f"--config {CONFIG} verify", changed["ExecStart"])
        self.assertEqual(changed["ReadWritePaths"], STATE)

    def test_comments_do_not_describe_eviction_of_knowledge(self):
        # Knowledge is never evicted; a copied "evicted originals" comment would mislead an operator.
        for name in ("neev-knowledge-storage-box.service", "neev-knowledge-storage-box-verify.service"):
            comments = [line for line in lines(name) if line.startswith("#")]
            self.assertFalse([c for c in comments if "evict" in c.lower() and "never" not in c.lower()], name)

    def test_timers_are_offset_from_the_originals_and_target_the_knowledge_units(self):
        changed = self.assertOnlyChanged("neev-storage-box.timer", "neev-knowledge-storage-box.timer",
                                         ("Description=", "OnCalendar=", "Unit="))
        self.assertEqual(changed["OnCalendar"], "*:30")
        self.assertEqual(changed["Unit"], "neev-knowledge-storage-box.service")
        changed = self.assertOnlyChanged("neev-storage-box-verify.timer", "neev-knowledge-storage-box-verify.timer",
                                         ("Description=", "OnCalendar=", "Unit="))
        self.assertEqual(changed["OnCalendar"], "*-*-* 02:40:00 UTC")
        self.assertEqual(changed["Unit"], "neev-knowledge-storage-box-verify.service")

    def test_there_is_no_knowledge_evict_or_purge_unit(self):
        names = os.listdir(OPERATIONS)
        knowledge_units = sorted(n for n in names if n.startswith("neev-knowledge-storage-box"))
        self.assertEqual(knowledge_units, [
            "neev-knowledge-storage-box-verify.service", "neev-knowledge-storage-box-verify.timer",
            "neev-knowledge-storage-box.config.example.json", "neev-knowledge-storage-box.service",
            "neev-knowledge-storage-box.timer"])
        for name in knowledge_units:
            text = (OPERATIONS / name).read_text()
            self.assertNotRegex(text, r"\b(evict|purge)\b", name)


class ConfigTests(unittest.TestCase):
    def setUp(self):
        self.example = json.loads((OPERATIONS / "neev-knowledge-storage-box.config.example.json").read_text())
        self.original = json.loads((OPERATIONS / "neev-storage-box.config.example.json").read_text())

    def test_same_auth_keys_as_the_existing_config_and_no_broker_or_eviction(self):
        self.assertEqual(set(self.example), (set(self.original) - {"broker", "eviction"}) | {"allow_main_account"})
        for key in ("identity_file", "known_hosts_file"):
            self.assertEqual(self.example[key], self.original[key], "auth is reused, not copied")
        self.assertIs(self.example["allow_main_account"], True)
        self.assertEqual(self.example["remote_root"], "neev-knowledge")
        self.assertEqual(self.example["local_root"], LOCAL_ROOT)
        self.assertEqual(self.example["state_dir"], STATE)
        self.assertNotIn("-sub", self.example["user"], "the approved main account")

    def materialise(self, root):
        identity = root / "id"
        identity.write_text("placeholder")
        os.chmod(identity, 0o600)
        known = root / "known_hosts"
        known.write_text("")
        os.chmod(known, 0o600)
        (root / "local").mkdir()
        (root / "state").mkdir()
        config = dict(self.example, identity_file=str(identity), known_hosts_file=str(known),
                      local_root=str(root / "local"), state_dir=str(root / "state"),
                      host="u123456.your-storagebox.de", user="u123456")
        path = root / "knowledge.json"
        path.write_text(json.dumps(config))
        os.chmod(path, 0o600)
        return str(path)

    def test_the_real_loader_accepts_it_and_eviction_is_refused(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(os.path.realpath(temp))
            config = sb.load_config(self.materialise(root))
            self.assertEqual(config["remote_root"], "neev-knowledge")
            with self.assertRaisesRegex(sb.ConfigError, "eviction is not configured"):
                sb.eviction_settings(config)
            calls = []
            with self.assertRaises(sb.ConfigError):
                sb.evict(config, run=lambda *a, **k: calls.append(a), now=0, broker_ping=lambda: True,
                         log=lambda *_: None)
            self.assertEqual(calls, [], "evict must refuse before running anything")

    def test_remote_root_is_isolated_from_every_existing_namespace(self):
        self.assertNotIn(self.example["remote_root"], {"neev-originals", "neev-backups", "workos-restic"})
        self.assertTrue(sb.SEGMENT.match(self.example["remote_root"]))


box_spec = importlib.util.spec_from_file_location("test_neev_storage_box", Path(__file__).parent / "test_neev_storage_box.py")
box_tests = importlib.util.module_from_spec(box_spec)
box_spec.loader.exec_module(box_tests)

SNAP = "a0000000-0000-4000-8000-00000000000a"
REV = "b0000000-0000-4000-8000-00000000000b"


class ReplicaTests(unittest.TestCase):
    """The unchanged replicate/verify against the knowledge layout, through the
    existing fake rsync + restricted shell."""

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        base = Path(os.path.realpath(temp.name))
        self.root, self.state, self.remote = base / "knowledge", base / "state", base / "remote"
        for directory in (self.root, self.state, self.remote):
            directory.mkdir(mode=0o700)
        self.files = {(3, f"{SNAP}.md"): b"## snapshot\n", (3, f"{REV}.md"): b"# Page\n",
                      (9, f"{SNAP}.md"): b"other tenant\n"}
        for (org, name), data in self.files.items():
            (self.root / str(org)).mkdir(mode=0o700, exist_ok=True)
            (self.root / str(org) / name).write_bytes(data)
            os.chmod(self.root / str(org) / name, 0o600)
        identity, known = base / "id", base / "known_hosts"
        for path in (identity, known):
            path.write_text("")
            os.chmod(path, 0o600)
        example = json.loads((OPERATIONS / "neev-knowledge-storage-box.config.example.json").read_text())
        self.config = dict(example, host="u123456.your-storagebox.de", user="u123456",
                           identity_file=str(identity), known_hosts_file=str(known),
                           local_root=str(self.root), state_dir=str(self.state))
        self.box = box_tests.FakeStorageBox(self.root, self.remote)

    def test_replicate_copies_per_org_into_neev_knowledge_only_and_verify_rehashes(self):
        result = sb.replicate(self.config, run=self.box, log=lambda *_: None)
        self.assertEqual(result["verified"], 3)
        self.assertEqual(sorted(os.listdir(self.remote)), ["neev-knowledge"])
        for (org, name), data in self.files.items():
            copy = self.remote / "neev-knowledge" / str(org) / name
            self.assertEqual(hashlib.sha256(copy.read_bytes()).hexdigest(), hashlib.sha256(data).hexdigest())
        flat = [" ".join(call) for call in self.box.calls]
        self.assertFalse(any("--delete" in call for call in flat))
        self.assertFalse(any(" -- rm " in call for call in flat))
        self.assertTrue(all("--ignore-existing" in call for call in flat if call.startswith("rsync")))
        self.assertEqual(sb.verify(self.config, run=self.box, log=lambda *_: None), 3)
        # The server copy is untouched: replication never moves or removes it.
        for (org, name), data in self.files.items():
            self.assertEqual((self.root / str(org) / name).read_bytes(), data)

    def test_verify_detects_silent_remote_loss(self):
        sb.replicate(self.config, run=self.box, log=lambda *_: None)
        (self.remote / "neev-knowledge" / "3" / f"{REV}.md").write_bytes(b"# changed\n")
        with self.assertRaises(sb.RemoteMismatch):
            sb.verify(self.config, run=self.box, log=lambda *_: None)

    def test_purge_has_nothing_to_act_on_without_a_broker(self):
        sb.replicate(self.config, run=self.box, log=lambda *_: None)
        summary = sb.purge(self.config, run=self.box, now=10**10, log=lambda *_: None)
        self.assertEqual(summary, {"due": 0, "purged": 0})
        self.assertEqual(len(list((self.remote / "neev-knowledge").rglob("*.md"))), 3)


if __name__ == "__main__":
    unittest.main()
