"""Portable tests for per-user/per-provider credential delivery.

No real cryptsetup/root: a fake CredentialVolumes stands in for the LUKS layer, an
injected chown records ownership transfers (chown needs root in production), an
injected stat_fn simulates the mount root, and a real temp dir backs the per-scope
run lock. These prove the parser-to-open scope mapping, cross-user/handle refusal,
missing credentials, shape/mode enforcement, the recover→lend→restore ownership
sequence held across the child's lifetime, honest busy on a held lock, and per-scope
lock isolation (different users never block or expose each other)."""

import fcntl
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_agent_credentials as credentials  # noqa: E402
from neev_credential_store import Scope, StoreError  # noqa: E402


class FakeVolumes:
    def __init__(self, seeded):
        self._seeded = seeded  # {(org, user, provider, handle): path}
        self.opened = []
        self.closed = []

    def open(self, scope, handle):
        assert isinstance(scope, Scope)
        self.opened.append((scope.org_id, scope.user_id, scope.provider, handle))
        try:
            return self._seeded[(scope.org_id, scope.user_id, scope.provider, handle)]
        except KeyError:
            raise StoreError("unavailable") from None

    def close(self, scope, handle):  # the launcher must NEVER unmount/delete
        self.closed.append((scope.org_id, scope.user_id, scope.provider, handle))


def fake_stat(perm=0o700, isdir=True):
    mode = (stat.S_IFDIR if isdir else stat.S_IFREG) | perm

    class _Stat:
        st_mode = mode
        st_uid = credentials.CANONICAL_OWNER_UID

    def _stat(_path, follow_symlinks=True):  # accepts the kwarg deliver() passes
        return _Stat()

    return _stat


class RecordingChown:
    def __init__(self):
        self.calls = []

    def __call__(self, root, uid, gid):
        self.calls.append((root, uid, gid))


def deliver(volumes, tenant, provider, handle, run_uid, run_gid, lock_dir, **kw):
    return credentials.deliver(
        volumes, tenant, provider, handle, run_uid, run_gid,
        lock_dir=lock_dir, **kw,
    )


class ParseTenantTests(unittest.TestCase):
    def test_valid(self):
        self.assertEqual(credentials.parse_tenant("org:3/user:7"), (3, 7))

    def test_rejects_malformed(self):
        for bad in [
            "org:3", "user:7/org:3", "org:3/user:7/x", "org:0/user:7",
            "org:3/user:0", "org:03/user:7", "org:-1/user:7", "org:x/user:7",
            "org: 3/user:7", "",
        ]:
            with self.assertRaises(credentials.CredentialError, msg=bad):
                credentials.parse_tenant(bad)


class ProviderEnvTests(unittest.TestCase):
    def test_paths_are_under_home(self):
        claude = credentials.provider_env("/vol", "claude")
        self.assertEqual(claude, {"HOME": "/vol", "CLAUDE_CONFIG_DIR": "/vol/.claude"})
        codex = credentials.provider_env("/vol", "codex")
        self.assertEqual(codex, {"HOME": "/vol", "CODEX_HOME": "/vol/.codex"})


class DeliverTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.lock_dir = os.path.join(self.tmp.name, "run-locks")

    def _volumes(self):
        return FakeVolumes({(3, 7, "claude", "h1"): "/mnt/vol-h1"})

    def test_recover_lend_and_restore_ownership_around_the_child(self):
        volumes, chown = self._volumes(), RecordingChown()
        C = credentials.CANONICAL_OWNER_UID
        with deliver(
            volumes, "org:3/user:7", "claude", "h1", 61000, 61000,
            self.lock_dir, chown_tree=chown, stat_fn=fake_stat(),
        ) as delivery:
            self.assertEqual(volumes.opened, [(3, 7, "claude", "h1")])
            self.assertEqual(delivery.home, "/mnt/vol-h1")
            self.assertEqual(delivery.env["HOME"], "/mnt/vol-h1")
            # Inside the run: recovered to canonical, then lent to the lane uid.
            self.assertEqual(
                chown.calls,
                [("/mnt/vol-h1", C, C), ("/mnt/vol-h1", 61000, 61000)],
            )
        # On exit the canonical owner is restored, and nothing was unmounted/deleted.
        self.assertEqual(chown.calls[-1], ("/mnt/vol-h1", C, C))
        self.assertEqual(volumes.closed, [])

    def test_home_as_sets_seen_home_while_chown_targets_the_physical_mount(self):
        # With a bind delivery, the provider's HOME is the lane-traversable path,
        # but ownership transfer (and the bind source) must be the physical mount.
        volumes, chown = self._volumes(), RecordingChown()
        C = credentials.CANONICAL_OWNER_UID
        with deliver(
            volumes, "org:3/user:7", "claude", "h1", 61000, 61000,
            self.lock_dir, home_as="/data/agents/run-x/home",
            chown_tree=chown, stat_fn=fake_stat(),
        ) as delivery:
            self.assertEqual(delivery.home, "/data/agents/run-x/home")
            self.assertEqual(delivery.mount, "/mnt/vol-h1")
            self.assertEqual(delivery.env["HOME"], "/data/agents/run-x/home")
            self.assertEqual(
                delivery.env["CLAUDE_CONFIG_DIR"], "/data/agents/run-x/home/.claude"
            )
            # chown still operates on the physical mount, not the seen HOME.
            self.assertEqual(chown.calls, [("/mnt/vol-h1", C, C), ("/mnt/vol-h1", 61000, 61000)])
        self.assertEqual(chown.calls[-1], ("/mnt/vol-h1", C, C))

    def test_malformed_home_as_is_refused(self):
        for bad in ("relative/home", "/home\x00x", 5):
            with self.assertRaises(credentials.CredentialError) as caught:
                with deliver(
                    self._volumes(), "org:3/user:7", "claude", "h1", 61000, 61000,
                    self.lock_dir, home_as=bad,
                    chown_tree=RecordingChown(), stat_fn=fake_stat(),
                ):
                    pass
            self.assertEqual(caught.exception.category, "home")

    def test_ownership_restored_even_when_child_raises(self):
        volumes, chown = self._volumes(), RecordingChown()
        C = credentials.CANONICAL_OWNER_UID
        with self.assertRaises(RuntimeError):
            with deliver(
                volumes, "org:3/user:7", "claude", "h1", 61000, 61000,
                self.lock_dir, chown_tree=chown, stat_fn=fake_stat(),
            ):
                raise RuntimeError("child failed / cancelled")
        self.assertEqual(chown.calls[-1], ("/mnt/vol-h1", C, C))  # restored

    def test_cross_user_or_unknown_handle_refused_without_chown(self):
        volumes, chown = self._volumes(), RecordingChown()
        for tenant, handle in [("org:3/user:8", "h1"), ("org:3/user:7", "nope")]:
            with self.assertRaises(credentials.CredentialError) as caught:
                with deliver(
                    volumes, tenant, "claude", handle, 61000, 61000,
                    self.lock_dir, chown_tree=chown, stat_fn=fake_stat(),
                ):
                    pass
            self.assertEqual(caught.exception.category, "credentials")
        self.assertEqual(chown.calls, [])  # never lent an unauthorized volume

    def test_missing_credentials_refused(self):
        with self.assertRaises(credentials.CredentialError) as caught:
            with deliver(
                FakeVolumes({}), "org:3/user:7", "codex", "h1", 61000, 61000,
                self.lock_dir, chown_tree=RecordingChown(), stat_fn=fake_stat(),
            ):
                pass
        self.assertEqual(caught.exception.category, "credentials")

    def test_wrong_mode_or_non_directory_refused(self):
        for kw, category in [
            (dict(perm=0o755), "mode"),
            (dict(isdir=False), "shape"),
        ]:
            with self.assertRaises(credentials.CredentialError) as caught:
                with deliver(
                    self._volumes(), "org:3/user:7", "claude", "h1", 61000, 61000,
                    self.lock_dir, chown_tree=RecordingChown(), stat_fn=fake_stat(**kw),
                ):
                    pass
            self.assertEqual(caught.exception.category, category)

    def test_held_scope_lock_reports_busy_not_a_second_lend(self):
        # A concurrent run for the SAME (user, provider) holds the lock; a second
        # delivery must report busy (honest) rather than lend the volume twice.
        os.makedirs(self.lock_dir, mode=0o700, exist_ok=True)
        held = os.open(
            os.path.join(self.lock_dir, "3-7-claude.lock"),
            os.O_RDWR | os.O_CREAT, 0o600,
        )
        self.addCleanup(os.close, held)
        fcntl.flock(held, fcntl.LOCK_EX)
        saved = credentials.RUN_LOCK_TIMEOUT_SECONDS
        credentials.RUN_LOCK_TIMEOUT_SECONDS = 0.2
        chown = RecordingChown()
        try:
            with self.assertRaises(credentials.CredentialError) as caught:
                with deliver(
                    self._volumes(), "org:3/user:7", "claude", "h1", 61000, 61000,
                    self.lock_dir, chown_tree=chown, stat_fn=fake_stat(),
                ):
                    pass
        finally:
            credentials.RUN_LOCK_TIMEOUT_SECONDS = saved
        self.assertEqual(caught.exception.category, "busy")
        self.assertEqual(chown.calls, [])  # never touched the volume

    def test_different_scopes_do_not_block_each_other(self):
        # Holding user 7's lock must not stop user 9's delivery (different volume,
        # different lockfile) — no cross-user blocking or exposure.
        os.makedirs(self.lock_dir, mode=0o700, exist_ok=True)
        held = os.open(
            os.path.join(self.lock_dir, "3-7-claude.lock"),
            os.O_RDWR | os.O_CREAT, 0o600,
        )
        self.addCleanup(os.close, held)
        fcntl.flock(held, fcntl.LOCK_EX)
        volumes = FakeVolumes({(3, 9, "claude", "h9"): "/mnt/vol-h9"})
        with deliver(
            volumes, "org:3/user:9", "claude", "h9", 61000, 61000,
            self.lock_dir, chown_tree=RecordingChown(), stat_fn=fake_stat(),
        ) as delivery:
            self.assertEqual(delivery.home, "/mnt/vol-h9")


if __name__ == "__main__":
    unittest.main()
