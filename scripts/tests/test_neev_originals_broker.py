"""Broker, eviction and purge contracts over a real Unix socket, real files and
real fetch subprocesses (a local directory stands in for the Storage Box)."""
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
import unittest

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))
spec = importlib.util.spec_from_file_location("neev_originals_broker", OPERATIONS / "neev_originals_broker.py")
broker_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(broker_module)
box = broker_module.box
sys.path.insert(0, str(Path(__file__).parent))
from test_neev_storage_box import FakeStorageBox  # noqa: E402

KEY = "0f8fad5b-d9cb-469f-a165-70867728950e"
OTHER = "7c9e6679-7425-40de-944b-e07fc1f90ae7"
THIRD = "16fd2706-8baf-433b-82eb-8c7fada847da"
DAY = 86400


def digest(data):
    return hashlib.sha256(data).hexdigest()


class RemovingStorageBox(FakeStorageBox):
    """FakeStorageBox plus the restricted shell's `rm` (used only by purge)."""

    def __call__(self, args, **kwargs):
        if args[0] == "ssh" and self.available:
            words = args[args.index("--") + 1:]
            if words[:2] == ["ls", "-1"]:
                self.calls.append(list(args))
                path = self.home / words[2]
                names = sorted(p.name for p in path.iterdir()) if path.is_dir() else []
                return subprocess.CompletedProcess(args, 0, stdout="\n".join(names) + "\n", stderr="")
            if words[:3] == ["stat", "-c", "%Y"]:
                self.calls.append(list(args))
                path = self.home / words[3]
                if not path.exists():
                    return subprocess.CompletedProcess(args, 1, stdout="", stderr="")
                return subprocess.CompletedProcess(args, 0, stdout=f"{int(path.stat().st_mtime)}\n", stderr="")
            if words[0] == "rm":
                self.calls.append(list(args))
                path = self.home / words[1]
                if path.exists():
                    path.unlink()
                    return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
                return subprocess.CompletedProcess(args, 1, stdout="", stderr="")
        return super().__call__(args, **kwargs)


class Harness(unittest.TestCase):
    def setUp(self):
        # AF_UNIX paths are short on macOS; keep the base under /tmp (real path).
        base = Path(os.path.realpath(tempfile.mkdtemp(prefix="nvb-", dir="/tmp")))
        self.addCleanup(lambda: shutil.rmtree(base, ignore_errors=True))
        self.base = base
        self.root = base / "documents"
        self.state = base / "state"
        self.remote = base / "remote"
        for directory in (self.root, self.state, self.remote / "neev-originals"):
            directory.mkdir(mode=0o700, parents=True)
            os.chmod(directory, 0o700)
        identity = base / "id"
        identity.write_text("private")
        os.chmod(identity, 0o600)
        known = base / "known_hosts"
        known.write_text("")
        os.chmod(known, 0o600)
        self.config = {
            "host": "u123456.your-storagebox.de", "user": "u123456", "allow_main_account": True,
            "identity_file": str(identity), "known_hosts_file": str(known),
            "remote_root": "neev-originals", "local_root": str(self.root), "state_dir": str(self.state),
            "broker": {"socket": str(base / "b.sock"), "client_uid": os.getuid(),
                       "socket_gid": os.getgid(), "fetch_timeout_seconds": 30},
        }
        self.fetches = []

    # -- helpers ---------------------------------------------------------------
    def remote_blob(self, org, name, data, ledger=True, evicted=True):
        path = self.remote / "neev-originals" / str(org) / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        if ledger:
            ledger_obj = box.Ledger(str(self.state))
            entry = {"sha256": digest(data), "size": len(data), "verified_at": "t"}
            if evicted:
                entry["evicted_at"] = 1
            ledger_obj.entries[f"{org}/{name}"] = entry
            ledger_obj.save()

    def ledger(self):
        return box.Ledger(str(self.state)).entries

    def local_blob(self, org, name, data, age=0):
        org_dir = self.root / str(org)
        org_dir.mkdir(mode=0o700, exist_ok=True)
        os.chmod(org_dir, 0o700)
        path = org_dir / name
        path.write_bytes(data)
        os.chmod(path, 0o600)
        if age:
            moment = time.time() - age
            os.utime(path, (moment, moment))
        return path

    def fetch_argv(self, relative):
        self.fetches.append(relative)
        return ["cat", str(self.remote / "neev-originals" / relative)]

    def slow_fetch(self, seconds):
        def argv(relative):
            self.fetches.append(relative)
            target = self.remote / "neev-originals" / relative
            return ["sh", "-c", f'sleep {seconds}; exec cat "$0"', str(target)]
        return argv

    def start(self, fetch_argv=None, **settings):
        self.config["broker"].update(settings)
        broker = broker_module.Broker(self.config, fetch_argv=fetch_argv or self.fetch_argv)
        server = broker_module.BrokerServer(broker)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return broker

    def call(self, payload):
        return broker_module.request(self.config["broker"]["socket"], payload)

    def blob(self, op, org, key, kind, data=None, **overrides):
        payload = {"v": 1, "op": op, "org_id": org, "key": key, "kind": kind}
        if op != "forget":
            payload.update({"sha256": digest(data), "byte_length": len(data)})
        payload.update(overrides)
        return payload

    def hydrate(self, org, key, kind, data, **overrides):
        return self.call(self.blob("hydrate", org, key, kind, data, **overrides))

    def forget(self, org, key, kind):
        return self.call(self.blob("forget", org, key, kind))

    def stream(self, org, key, kind, data):
        return broker_module.stream(self.config["broker"]["socket"], self.blob("stream", org, key, kind, data))

    def leftovers(self, org):
        directory = self.root / str(org)
        return [p.name for p in directory.iterdir() if p.name.endswith(".tmp")] if directory.exists() else []


class BrokerTests(Harness):
    def test_hydrates_an_evicted_original_atomically_with_app_ownership(self):
        data = os.urandom(3 * 1024 * 1024 + 11)
        self.remote_blob(7, f"{KEY}.att", data)
        self.start()
        self.assertEqual(self.hydrate(7, KEY, "att", data), {"ok": True, "result": "fetched"})
        final = self.root / "7" / f"{KEY}.att"
        info = os.lstat(final)
        self.assertTrue(stat.S_ISREG(info.st_mode))
        self.assertEqual((info.st_mode & 0o7777, info.st_nlink, info.st_uid), (0o600, 1, os.getuid()))
        self.assertEqual(os.lstat(self.root / "7").st_mode & 0o7777, 0o700)
        self.assertEqual(final.read_bytes(), data)
        self.assertEqual(self.leftovers(7), [])
        self.assertEqual(self.hydrate(7, KEY, "att", data), {"ok": True, "result": "present"})
        self.assertEqual(len(self.fetches), 1)

    def test_only_evicted_ledger_entries_with_matching_metadata_are_hydrated(self):
        data = b"document"
        self.remote_blob(7, f"{KEY}.md", data, ledger=False)
        self.start()
        self.assertEqual(self.hydrate(7, KEY, "md", data), {"ok": False, "error": "not_verified"})
        # Replicated but never evicted: missing locally means lost, not cached.
        self.remote_blob(7, f"{KEY}.md", data, evicted=False)
        self.assertEqual(self.hydrate(7, KEY, "md", data), {"ok": False, "error": "not_verified"})
        self.remote_blob(7, f"{KEY}.md", data)
        self.assertEqual(self.hydrate(7, KEY, "md", b"other bytes!"), {"ok": False, "error": "not_verified"})
        self.assertEqual(self.hydrate(7, KEY, "img", data), {"ok": False, "error": "not_verified"})
        self.assertEqual(self.fetches, [])
        self.assertFalse((self.root / "7" / f"{KEY}.md").exists())

    def test_remote_bytes_that_disagree_are_never_published(self):
        data = b"expected bytes"
        self.remote_blob(7, f"{KEY}.img", data)
        (self.remote / "neev-originals" / "7" / f"{KEY}.img").write_bytes(b"tampered bytes")
        self.start()
        self.assertEqual(self.hydrate(7, KEY, "img", data), {"ok": False, "error": "remote_mismatch"})
        (self.remote / "neev-originals" / "7" / f"{KEY}.img").write_bytes(data + b"extra")
        self.assertEqual(self.hydrate(7, KEY, "img", data), {"ok": False, "error": "remote_mismatch"})
        self.assertFalse((self.root / "7" / f"{KEY}.img").exists())
        self.assertEqual(self.leftovers(7), [])

    def test_outage_and_timeout_are_retryable_unavailable(self):
        data = b"bytes"
        self.remote_blob(7, f"{KEY}.att", data)
        self.start(fetch_argv=lambda relative: ["false"])
        self.assertEqual(self.hydrate(7, KEY, "att", data), {"ok": False, "error": "unavailable"})
        self.assertEqual(self.leftovers(7), [])

    def test_fetch_timeout_kills_the_transfer(self):
        data = b"bytes"
        self.remote_blob(7, f"{KEY}.att", data)
        self.start(fetch_argv=lambda relative: ["sleep", "20"], fetch_timeout_seconds=1)
        began = time.time()
        self.assertEqual(self.hydrate(7, KEY, "att", data), {"ok": False, "error": "unavailable"})
        self.assertLess(time.time() - began, 10)
        self.assertEqual(self.leftovers(7), [])

    def test_malformed_or_path_bearing_requests_are_refused(self):
        self.start()
        base = self.blob("hydrate", 7, KEY, "att", b"bytes")
        for change in ({"key": "../../etc/passwd"}, {"key": KEY.upper()}, {"kind": "sh"},
                       {"org_id": 0}, {"org_id": "7"}, {"org_id": True}, {"sha256": "A" * 64},
                       {"byte_length": -1}, {"path": "/etc/shadow"}, {"v": 2}, {"op": "fetch"},
                       {"op": "delete"}):
            with self.subTest(change=change):
                self.assertEqual(self.call({**base, **change}), {"ok": False, "error": "invalid"})
        forget = self.blob("forget", 7, KEY, "att")
        self.assertEqual(self.call({**forget, "sha256": "a" * 64}), {"ok": False, "error": "invalid"})
        self.assertEqual(self.call({**forget, "key": "../x"}), {"ok": False, "error": "invalid"})
        with socket.socket(socket.AF_UNIX) as client:
            client.connect(self.config["broker"]["socket"])
            client.sendall(b"x" * 5000 + b"\n")
            self.assertEqual(json.loads(client.recv(256)), {"ok": False, "error": "invalid"})
        self.assertEqual(self.fetches, [])

    def test_peer_uid_is_authenticated_for_every_operation(self):
        data = b"bytes"
        self.remote_blob(7, f"{KEY}.att", data)
        self.start(client_uid=os.getuid() + 1)
        self.assertEqual(self.hydrate(7, KEY, "att", data), {"ok": False, "error": "forbidden"})
        self.assertEqual(self.forget(7, KEY, "att"), {"ok": False, "error": "forbidden"})
        self.assertEqual(self.stream(7, KEY, "att", data)[0], {"ok": False, "error": "forbidden"})
        expected = {"ok": True, "result": "pong"} if os.getuid() == 0 else {"ok": False, "error": "forbidden"}
        self.assertEqual(self.call({"v": 1, "op": "ping"}), expected)
        self.assertEqual(self.fetches, [])
        self.assertFalse(Path(box.tombstone_path(self.config)).exists())

    def test_existing_local_blob_is_never_overwritten(self):
        data = b"the original"
        self.remote_blob(7, f"{KEY}.att", data)
        local = self.local_blob(7, f"{KEY}.att", b"a different!")
        self.start()
        self.assertEqual(self.hydrate(7, KEY, "att", data), {"ok": False, "error": "local_mismatch"})
        self.assertEqual(local.read_bytes(), b"a different!")

    def test_symlinked_org_directory_is_refused(self):
        data = b"bytes"
        self.remote_blob(7, f"{KEY}.att", data)
        elsewhere = self.base / "elsewhere"
        elsewhere.mkdir(mode=0o700)
        os.symlink(elsewhere, self.root / "7")
        self.start()
        self.assertEqual(self.hydrate(7, KEY, "att", data), {"ok": False, "error": "unavailable"})
        self.assertEqual(list(elsewhere.iterdir()), [])

    def test_concurrent_requests_for_one_blob_fetch_once_without_starving_slots(self):
        data = os.urandom(512 * 1024)
        self.remote_blob(7, f"{KEY}.att", data)
        # One transfer slot: waiters on the same blob must not hold slots.
        self.start(fetch_argv=self.slow_fetch(0.5), max_parallel=1)
        results = []
        threads = [threading.Thread(target=lambda: results.append(self.hydrate(7, KEY, "att", data)))
                   for _ in range(6)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        self.assertTrue(all(r["ok"] for r in results), results)
        self.assertEqual(len(self.fetches), 1)
        self.assertEqual((self.root / "7" / f"{KEY}.att").read_bytes(), data)

    def test_transfers_queue_for_a_bounded_time_instead_of_failing_immediately(self):
        blobs = {KEY: b"first", OTHER: b"second", THIRD: b"third!"}
        for key, data in blobs.items():
            self.remote_blob(7, f"{key}.att", data)
        self.start(fetch_argv=self.slow_fetch(0.4), max_parallel=1, queue_timeout_seconds=10)
        results = {}
        threads = [threading.Thread(target=lambda k=k, d=d: results.__setitem__(k, self.hydrate(7, k, "att", d)))
                   for k, d in blobs.items()]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        self.assertEqual({r["result"] for r in results.values()}, {"fetched"})

    def test_a_full_queue_times_out_as_busy(self):
        self.remote_blob(7, f"{KEY}.att", b"first")
        self.remote_blob(7, f"{OTHER}.att", b"second")
        self.start(fetch_argv=self.slow_fetch(3), max_parallel=1, queue_timeout_seconds=1)
        slow = threading.Thread(target=lambda: self.hydrate(7, KEY, "att", b"first"))
        slow.start()
        time.sleep(0.3)
        self.assertEqual(self.hydrate(7, OTHER, "att", b"second"), {"ok": False, "error": "busy"})
        slow.join()

    def test_socket_is_group_only_and_never_replaces_a_regular_file(self):
        self.start()
        self.assertEqual(os.stat(self.config["broker"]["socket"]).st_mode & 0o777, 0o660)
        blocked = self.base / "notasocket"
        blocked.write_text("keep")
        self.config["broker"]["socket"] = str(blocked)
        with self.assertRaises(OSError):
            broker_module.BrokerServer(broker_module.Broker(self.config))
        self.assertEqual(blocked.read_text(), "keep")

    def test_config_validation(self):
        self.assertEqual(broker_module.validate_broker(self.config)["client_uid"], os.getuid())
        for bad in ({"client_uid": 0}, {"socket": "relative.sock"}, {"extra": 1}, {"max_parallel": 0},
                    {"queue_timeout_seconds": 0}):
            with self.subTest(bad=bad), self.assertRaises(box.ConfigError):
                broker_module.validate_broker({"broker": {**self.config["broker"], **bad}})


class PublicationTests(Harness):
    def test_publish_is_an_atomic_no_replace_rename(self):
        org = self.root / "7"
        org.mkdir(mode=0o700)
        (org / "a.tmp").write_bytes(b"new")
        (org / "final").write_bytes(b"old")
        fd = os.open(org, os.O_RDONLY | os.O_DIRECTORY)
        try:
            with self.assertRaises(FileExistsError):
                broker_module.publish_noreplace(fd, "a.tmp", "final")
            self.assertEqual((org / "final").read_bytes(), b"old")
            broker_module.publish_noreplace(fd, "a.tmp", "fresh")
        finally:
            os.close(fd)
        self.assertFalse((org / "a.tmp").exists())
        self.assertEqual(os.lstat(org / "fresh").st_nlink, 1)

    def test_stale_broker_temporaries_are_removed_at_start(self):
        stale = self.local_blob(7, f"{KEY}.att.fetch-0123456789abcdef.tmp", b"partial", age=DAY)
        fresh = self.local_blob(7, f"{OTHER}.att.fetch-0123456789abcdef.tmp", b"in progress")
        app_temp = self.local_blob(7, f"{THIRD}.att.tmp", b"app upload", age=DAY)
        broker = broker_module.Broker(self.config, fetch_argv=self.fetch_argv)
        self.assertEqual(broker.cleanup_stale_temporaries(), 1)
        self.assertFalse(stale.exists())
        self.assertTrue(fresh.exists())
        self.assertTrue(app_temp.exists(), "only the broker's own temporaries are touched")

    def test_scans_skip_an_in_flight_publish_but_fail_on_a_real_hard_link(self):
        final = self.local_blob(7, f"{KEY}.att", b"bytes")
        os.link(final, self.root / "7" / f"{KEY}.att.tmp")  # app mid link-then-unlink
        self.assertEqual(box.inventory(str(self.root)), [])
        os.unlink(self.root / "7" / f"{KEY}.att.tmp")
        os.link(final, self.root / "7" / f"{OTHER}.att")
        with self.assertRaises(box.LocalIntegrityError):
            box.inventory(str(self.root))


class StreamTests(Harness):
    def test_stream_sends_verified_bytes_without_touching_the_live_root(self):
        data = os.urandom(2 * 1024 * 1024 + 3)
        # Streaming does not require the blob to have been evicted: a backup may
        # need a replicated original that is missing locally for any reason.
        self.remote_blob(12, f"{KEY}.att", data, evicted=False)
        self.start()
        header, body = self.stream(12, KEY, "att", data)
        self.assertEqual(header, {"ok": True, "result": "stream", "byte_length": len(data)})
        self.assertEqual(body, data)
        self.assertFalse((self.root / "12").exists(), "nothing is republished into the cache")

    def test_stream_refusals_and_outages(self):
        data = b"bytes"
        self.remote_blob(12, f"{KEY}.att", data)
        self.start(fetch_argv=lambda relative: ["false"])
        self.assertEqual(self.stream(12, KEY, "att", data)[0], {"ok": False, "error": "unavailable"})
        self.assertEqual(self.stream(12, OTHER, "att", data)[0], {"ok": False, "error": "not_verified"})

    def test_a_short_remote_is_a_short_stream_the_caller_rejects(self):
        data = b"complete original"
        self.remote_blob(12, f"{KEY}.att", data)
        (self.remote / "neev-originals" / "12" / f"{KEY}.att").write_bytes(data[:5])
        self.start()
        header, body = self.stream(12, KEY, "att", data)
        self.assertTrue(header["ok"])
        self.assertNotEqual(digest(body), digest(data))


class ForgetTests(Harness):
    def gated_fetch(self, gate):
        def argv(relative):
            self.fetches.append(relative)
            target = self.remote / "neev-originals" / relative
            return ["sh", "-c", 'while [ ! -e "$0" ]; do sleep 0.05; done; exec cat "$1"',
                    str(gate), str(target)]
        return argv

    def wait_for_fetch(self):
        for _ in range(200):
            if self.fetches:
                return
            time.sleep(0.02)
        self.fail("fetch never started")

    def test_forget_waits_for_an_in_flight_hydrate_so_the_unlink_after_it_wins(self):
        data = b"attachment being swept"
        self.remote_blob(7, f"{KEY}.att", data)
        gate = self.base / "gate"
        self.start(fetch_argv=self.gated_fetch(gate))
        order, replies = [], {}

        def hydrate():
            replies["hydrate"] = self.hydrate(7, KEY, "att", data)
            order.append("hydrate")

        def forget():
            replies["forget"] = self.forget(7, KEY, "att")
            order.append("forget")

        reader = threading.Thread(target=hydrate)
        reader.start()
        self.wait_for_fetch()  # the read-through is mid-transfer
        sweeper = threading.Thread(target=forget)
        sweeper.start()
        time.sleep(0.5)
        self.assertEqual(order, [], "forget must not return while the fetch is in flight")
        gate.touch()
        reader.join(10)
        sweeper.join(10)
        self.assertEqual(order, ["hydrate", "forget"])
        self.assertEqual(replies, {"hydrate": {"ok": True, "result": "fetched"},
                                   "forget": {"ok": True, "result": "forgotten"}})
        # The app unlinks after forget returns: what was published is removed and
        # can never come back.
        final = self.root / "7" / f"{KEY}.att"
        final.unlink()
        self.assertEqual(self.hydrate(7, KEY, "att", data), {"ok": False, "error": "forgotten"})
        self.assertFalse(final.exists())
        self.assertEqual(self.leftovers(7), [])

    def test_a_tombstone_that_appears_mid_fetch_prevents_publication(self):
        data = b"attachment"
        self.remote_blob(7, f"{KEY}.att", data)
        gate = self.base / "gate"
        self.start(fetch_argv=self.gated_fetch(gate))
        replies = {}
        reader = threading.Thread(target=lambda: replies.setdefault("r", self.hydrate(7, KEY, "att", data)))
        reader.start()
        self.wait_for_fetch()
        # Written directly (as a second broker process might): no lock shared.
        box.save_json(box.tombstone_path(self.config),
                      {"version": 1, "forgotten": {f"7/{KEY}.att": {"at": 1}}})
        gate.touch()
        reader.join(10)
        self.assertEqual(replies["r"], {"ok": False, "error": "forgotten"})
        self.assertFalse((self.root / "7" / f"{KEY}.att").exists())
        self.assertEqual(self.leftovers(7), [])

    def test_forget_tombstones_so_nothing_resurrects(self):
        data = b"deleted attachment"
        self.remote_blob(7, f"{KEY}.att", data)
        self.start()
        self.assertEqual(self.forget(7, KEY, "att"), {"ok": True, "result": "forgotten"})
        self.assertEqual(self.forget(7, KEY, "att"), {"ok": True, "result": "forgotten"})  # idempotent
        self.assertEqual(self.hydrate(7, KEY, "att", data), {"ok": False, "error": "forgotten"})
        self.assertEqual(self.stream(7, KEY, "att", data)[0], {"ok": False, "error": "forgotten"})
        self.assertFalse((self.root / "7").exists())
        self.assertEqual(self.fetches, [])
        self.assertIn(f"7/{KEY}.att", box.load_tombstones(self.config))

    def test_a_tombstoned_local_file_is_never_replicated_again(self):
        self.local_blob(7, f"{KEY}.att", b"late unlink")
        self.start()
        self.forget(7, KEY, "att")
        fake = FakeStorageBox(self.root, self.remote)
        result = box.replicate(self.config, run=fake, log=lambda *_: None)
        self.assertEqual(result["pending"], 0)
        self.assertEqual(fake.calls, [])


class EvictionTests(Harness):
    def setUp(self):
        super().setUp()
        self.config["eviction"] = {"kinds": ["att", "img"], "min_idle_seconds": 14 * DAY,
                                   "max_local_bytes": 0}
        self.box = RemovingStorageBox(self.root, self.remote)

    def replicate(self, snapshot=True):
        box.replicate(self.config, run=self.box, log=lambda *_: None)
        if snapshot:
            self.take_snapshot()

    def take_snapshot(self, name=None, age=0):
        """A Storage Box snapshot: an immutable copy of the account home."""
        name = name or f"snap-{len(list((self.remote / '.zfs' / 'snapshot').glob('*'))) if (self.remote / '.zfs' / 'snapshot').exists() else 0}"
        target = self.remote / ".zfs" / "snapshot" / name
        shutil.copytree(self.remote / "neev-originals", target / "neev-originals")
        if age:
            moment = time.time() - age
            os.utime(target, (moment, moment))
        return name

    def verified(self):
        box.verify(self.config, run=self.box, log=lambda *_: None)

    def evict(self, ping=True, **kwargs):
        return box.evict(self.config, run=self.box, broker_ping=lambda: ping, log=lambda *_: None, **kwargs)

    def test_evicts_only_idle_verified_allowed_kinds_and_hydration_restores(self):
        idle = self.local_blob(7, f"{KEY}.att", b"idle attachment", age=30 * DAY)
        recent = self.local_blob(7, f"{OTHER}.att", b"recently read", age=30 * DAY)
        os.utime(recent, (time.time(), time.time() - 30 * DAY))
        document = self.local_blob(12, f"{KEY}.md", b"doc", age=30 * DAY)  # kind not allowed
        self.replicate()
        unreplicated = self.local_blob(12, f"{OTHER}.img", b"not yet copied", age=30 * DAY)
        self.verified()
        summary = self.evict()
        self.assertEqual(summary["evicted"], 1)
        self.assertFalse(idle.exists())
        for kept in (recent, document, unreplicated):
            self.assertTrue(kept.exists(), kept)
        self.assertTrue(self.ledger()[f"7/{KEY}.att"]["evicted_at"])
        self.assertNotIn("evicted_at", self.ledger()[f"7/{OTHER}.att"])
        self.start()
        data = b"idle attachment"
        self.assertEqual(self.hydrate(7, KEY, "att", data), {"ok": True, "result": "fetched"})
        self.assertEqual(idle.read_bytes(), data)
        # Never evicted, so never hydrated: a missing un-evicted blob is lost.
        recent.unlink()
        self.assertEqual(self.hydrate(7, OTHER, "att", b"recently read"),
                         {"ok": False, "error": "not_verified"})

    def test_eviction_requires_a_recent_successful_verify(self):
        blob = self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        self.replicate()
        with self.assertRaisesRegex(box.RemoteMismatch, "verify"):
            self.evict()
        box.verify(self.config, run=self.box, log=lambda *_: None, now=time.time() - 3 * DAY)
        with self.assertRaisesRegex(box.RemoteMismatch, "verify"):
            self.evict()
        (self.remote / "neev-originals" / "7" / f"{KEY}.att").write_bytes(b"lost")
        with self.assertRaises(box.RemoteMismatch):
            self.verified()
        record = json.loads(Path(box.verify_record_path(self.config)).read_text())
        self.assertEqual((record["ok"], record["failure"]), (False, "mismatch"))
        with self.assertRaisesRegex(box.RemoteMismatch, "verify"):
            self.evict()
        self.assertTrue(blob.exists())

    def test_verify_outage_is_recorded_and_blocks_eviction(self):
        self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        self.replicate()
        self.verified()
        self.box.available = False
        with self.assertRaises(box.RemoteUnavailable):
            self.verified()
        record = json.loads(Path(box.verify_record_path(self.config)).read_text())
        self.assertEqual((record["ok"], record["failure"]), (False, "unavailable"))

    def test_no_pressure_means_no_eviction(self):
        blob = self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        self.replicate()
        self.verified()
        self.config["eviction"]["max_local_bytes"] = 10 ** 12
        self.assertEqual(self.evict()["evicted"], 0)
        self.assertTrue(blob.exists())

    def test_pressure_is_bounded_oldest_first(self):
        oldest = self.local_blob(7, f"{KEY}.att", b"x" * 100, age=40 * DAY)
        newer = self.local_blob(7, f"{OTHER}.att", b"y" * 100, age=20 * DAY)
        self.replicate()
        self.verified()
        self.config["eviction"]["max_local_bytes"] = 150
        summary = self.evict()
        self.assertEqual((summary["need_bytes"], summary["evicted"]), (50, 1))
        self.assertFalse(oldest.exists())
        self.assertTrue(newer.exists())

    def test_fails_safe_when_broker_or_remote_is_unavailable(self):
        blob = self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        self.replicate()
        self.verified()
        with self.assertRaises(box.RemoteUnavailable):
            self.evict(ping=False)
        self.box.available = False
        with self.assertRaises(box.RemoteUnavailable):
            self.evict()
        self.assertTrue(blob.exists())

    def test_fresh_remote_check_keeps_blobs_whose_copy_was_lost(self):
        blob = self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        self.replicate()
        self.verified()
        (self.remote / "neev-originals" / "7" / f"{KEY}.att").unlink()
        with self.assertRaises(box.RemoteMismatch):
            self.evict()
        self.assertTrue(blob.exists())
        self.assertNotIn("evicted_at", self.ledger()[f"7/{KEY}.att"])

    def test_a_read_between_selection_and_unlink_keeps_the_blob(self):
        blob = self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        self.replicate()
        self.verified()
        original = self.box.__call__

        def reading(args, **kwargs):
            if args[0] == "ssh" and "sha256sum" in args:
                os.utime(blob, (time.time(), os.stat(blob).st_mtime))  # the app read it
            return original(args, **kwargs)

        summary = box.evict(self.config, run=reading, broker_ping=lambda: True, log=lambda *_: None)
        self.assertEqual((summary["evicted"], summary["skipped_changed"]), (0, 1))
        self.assertTrue(blob.exists())

    def test_tombstoned_blobs_are_not_eviction_candidates(self):
        blob = self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        self.replicate()
        self.verified()
        self.start()
        self.forget(7, KEY, "att")
        self.assertEqual(self.evict()["evicted"], 0)
        self.assertTrue(blob.exists())

    def test_dry_run_and_configuration_floor(self):
        blob = self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        self.replicate()
        self.verified()
        self.assertEqual(self.evict(dry_run=True)["evicted"], 1)
        self.assertTrue(blob.exists())
        self.assertNotIn("evicted_at", self.ledger()[f"7/{KEY}.att"])
        for bad in ({"min_idle_seconds": 60}, {"kinds": ["exe"]}, {"kinds": []},
                    {"max_verify_age_seconds": 5}):
            with self.subTest(bad=bad), self.assertRaises(box.ConfigError):
                box.eviction_settings({"eviction": {**self.config["eviction"], **bad}})
        with self.assertRaises(box.ConfigError):
            box.eviction_settings({"eviction": {"kinds": ["att"], "min_idle_seconds": DAY}})

    def test_replication_does_not_reupload_and_status_counts_remote_only(self):
        self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        self.replicate()
        self.verified()
        self.evict()
        self.box.calls.clear()
        self.assertEqual(box.replicate(self.config, run=self.box, log=lambda *_: None)["pending"], 0)
        self.assertEqual(self.box.calls, [])
        summary = box.status(self.config, log=lambda *_: None)
        self.assertEqual((summary["local_originals"], summary["remote_only_verified"], summary["evicted"]),
                         (0, 1, 1))
        self.assertTrue(summary["last_verify"]["ok"])

    def test_no_storage_box_snapshot_blocks_eviction_under_pressure(self):
        blob = self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        self.replicate(snapshot=False)
        self.verified()
        with self.assertRaisesRegex(box.EvictionBlocked, "no Storage Box snapshot"):
            self.evict()
        self.assertTrue(blob.exists())
        self.assertNotIn("evicted_at", self.ledger()[f"7/{KEY}.att"])
        record = json.loads(Path(box.evict_record_path(self.config)).read_text())
        self.assertIn("snapshot", record["blocked"])
        # Without pressure there is nothing to block: success, no remote calls.
        self.config["eviction"]["max_local_bytes"] = 10 ** 12
        self.box.calls.clear()
        self.assertEqual(self.evict()["evicted"], 0)
        self.assertEqual(self.box.calls, [])

    def test_pressure_without_snapshots_is_blocked_even_with_no_idle_candidates(self):
        self.local_blob(7, f"{KEY}.att", b"recently used")  # not idle
        self.replicate(snapshot=False)
        self.verified()
        with self.assertRaisesRegex(box.EvictionBlocked, "no Storage Box snapshot"):
            self.evict()
        self.replicate()  # now a snapshot exists: pressure with no idle blob is not an error
        self.assertEqual(self.evict()["evicted"], 0)

    def test_only_blobs_inside_the_newest_snapshot_are_evicted(self):
        old = self.local_blob(7, f"{KEY}.att", b"in snapshot", age=30 * DAY)
        self.replicate()  # snapshot contains KEY only
        newer = self.local_blob(7, f"{OTHER}.att", b"after snapshot", age=30 * DAY)
        self.replicate(snapshot=False)
        self.verified()
        summary = self.evict()
        self.assertEqual((summary["evicted"], summary["not_in_snapshot"]), (1, 1))
        self.assertFalse(old.exists())
        self.assertTrue(newer.exists())

    def test_newest_snapshot_is_chosen_by_time_not_name(self):
        self.replicate(snapshot=False)
        self.take_snapshot("aa-recent", age=60)
        self.take_snapshot("zz-old", age=10 * DAY)  # sorts last by name, but older
        self.assertEqual(box.newest_snapshot(self.config, run=self.box), "aa-recent")
        (self.remote / ".zfs" / "snapshot" / "bad name; rm").mkdir()  # never passed to the shell
        self.assertEqual(box.newest_snapshot(self.config, run=self.box), "aa-recent")

    def test_under_pressure_with_nothing_in_a_snapshot_is_blocked(self):
        blob = self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        (self.remote / ".zfs" / "snapshot" / "empty" / "neev-originals").mkdir(parents=True)
        self.replicate(snapshot=False)
        self.verified()
        with self.assertRaisesRegex(box.EvictionBlocked, "no candidate is in the newest snapshot"):
            self.evict()
        self.assertTrue(blob.exists())

    def test_an_unavailable_storage_box_is_recorded_not_left_stale(self):
        self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        self.replicate()
        self.verified()
        self.box.available = False
        with self.assertRaises(box.RemoteUnavailable):
            self.evict()
        record = json.loads(Path(box.evict_record_path(self.config)).read_text())
        self.assertEqual((record["blocked"], record["evicted"]), ("Storage Box unavailable", 0))

    def test_a_blob_locked_by_the_broker_is_skipped(self):
        blob = self.local_blob(7, f"{KEY}.att", b"idle", age=30 * DAY)
        self.replicate()
        self.verified()
        with box.blob_lock(self.config, f"7/{KEY}.att") as held:
            self.assertTrue(held)
            summary = self.evict()
        self.assertEqual((summary["evicted"], summary["skipped_locked"]), (0, 1))
        self.assertTrue(blob.exists())
        self.assertEqual(self.evict()["evicted"], 1)

    def test_the_broker_waits_for_the_evictor_lock(self):
        data = b"evicted bytes"
        self.remote_blob(7, f"{KEY}.att", data)
        self.start()
        replies = []
        with box.blob_lock(self.config, f"7/{KEY}.att"):
            reader = threading.Thread(target=lambda: replies.append(self.hydrate(7, KEY, "att", data)))
            reader.start()
            time.sleep(0.5)
            self.assertEqual(replies, [], "hydrate must wait while the evictor holds the blob lock")
        reader.join(10)
        self.assertEqual(replies, [{"ok": True, "result": "fetched"}])

    def test_real_broker_ping_gates_the_cli(self):
        self.assertFalse(box._broker_ping(self.config))  # not running yet
        self.start()
        self.assertTrue(box._broker_ping(self.config))


class PurgeTests(Harness):
    def setUp(self):
        super().setUp()
        self.box = RemovingStorageBox(self.root, self.remote)

    def test_purge_removes_only_tombstoned_remote_copies_after_the_grace_period(self):
        for key in (KEY, OTHER, THIRD):
            self.local_blob(7, f"{key}.att", key.encode())
        box.replicate(self.config, run=self.box, log=lambda *_: None)
        self.start()
        self.forget(7, KEY, "att")
        self.forget(7, OTHER, "att")
        tombstones = box.load_tombstones(self.config)
        tombstones[f"7/{KEY}.att"]["at"] = int(time.time()) - 40 * DAY  # past grace
        box.save_json(box.tombstone_path(self.config), {"version": 1, "forgotten": tombstones})
        self.assertEqual(box.purge(self.config, run=self.box, dry_run=True, log=lambda *_: None)["due"], 1)
        self.assertTrue((self.remote / "neev-originals" / "7" / f"{KEY}.att").exists())
        summary = box.purge(self.config, run=self.box, log=lambda *_: None)
        self.assertEqual(summary, {"due": 1, "purged": 1})
        self.assertFalse((self.remote / "neev-originals" / "7" / f"{KEY}.att").exists())
        self.assertTrue((self.remote / "neev-originals" / "7" / f"{OTHER}.att").exists())  # in grace
        self.assertTrue((self.remote / "neev-originals" / "7" / f"{THIRD}.att").exists())  # never forgotten
        self.assertNotIn(f"7/{KEY}.att", self.ledger())
        self.assertIn(f"7/{KEY}.att", box.load_tombstones(self.config), "tombstone is kept")
        removed = [c for c in self.box.calls if c[0] == "ssh" and c[c.index("--") + 1] == "rm"]
        self.assertEqual(len(removed), 1)
        # Idempotent: nothing further is due.
        self.assertEqual(box.purge(self.config, run=self.box, log=lambda *_: None)["due"], 0)


class LockTests(Harness):
    def test_scheduled_commands_wait_for_a_running_replicate(self):
        config_path = self.base / "config.json"
        config_path.write_text(json.dumps(self.config))
        os.chmod(config_path, 0o600)
        fake = FakeStorageBox(self.root, self.remote)
        lock_path = self.state / "lock"
        holder = open(lock_path, "a")
        fcntl.flock(holder, fcntl.LOCK_EX)
        threading.Timer(0.5, lambda: holder.close()).start()
        self.assertEqual(box.main(["--config", str(config_path), "verify"], run=fake, lock_wait_seconds=10), 0)
        holder = open(lock_path, "a")
        fcntl.flock(holder, fcntl.LOCK_EX)
        try:
            self.assertEqual(box.main(["--config", str(config_path), "verify"], run=fake,
                                      lock_wait_seconds=0.3), 1)
            self.assertEqual(box.main(["--config", str(config_path), "replicate"], run=fake), 1)
        finally:
            holder.close()


if __name__ == "__main__":
    unittest.main()
