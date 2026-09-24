"""Portable tests for per-user/per-provider credential delivery.

No real cryptsetup/root: a fake CredentialVolumes stands in for the LUKS layer, an
injected chown records ownership transfers (chown needs root in production), an
injected stat_fn simulates the mount root, and a real temp dir backs the per-scope
run lock. These prove the parser-to-open scope mapping, cross-user/handle refusal,
missing credentials, shape/mode enforcement, the recover→lend→restore ownership
sequence held across the child's lifetime, honest busy on a held lock, and per-scope
lock isolation (different users never block or expose each other)."""

import contextlib
import fcntl
import io
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
    def __init__(self, seeded, close_error=None, on_close=None):
        self._seeded = seeded  # {(org, user, provider, handle): path}
        self._close_error = close_error
        self._on_close = on_close
        self.opened = []
        self.closed = []
        self.revoked = []

    def open(self, scope, handle):
        assert isinstance(scope, Scope)
        self.opened.append((scope.org_id, scope.user_id, scope.provider, handle))
        try:
            return self._seeded[(scope.org_id, scope.user_id, scope.provider, handle)]
        except KeyError:
            raise StoreError("unavailable") from None

    def close(self, scope, handle):
        # `close` unmaps; it is NOT the destructive half. A lend gives the mapping
        # (and its autoclear loop device) back so a volume is unmapped at rest.
        assert isinstance(scope, Scope)
        self.closed.append((scope.org_id, scope.user_id, scope.provider, handle))
        if self._on_close is not None:
            self._on_close()
        if self._close_error is not None:
            raise self._close_error

    def revoke(self, scope, handle):  # the launcher must NEVER delete credentials
        self.revoked.append((scope.org_id, scope.user_id, scope.provider, handle))


class OpenOnlyVolumes:
    """The documented minimum a caller may supply: `open(scope, handle) -> path`."""

    def __init__(self, seeded):
        self._seeded = seeded

    def open(self, scope, handle):
        return self._seeded[(scope.org_id, scope.user_id, scope.provider, handle)]


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
    # These tests use fake mount paths, so default the provider-config-dir creation to a
    # no-op; the real behaviour is covered by test_ensure_provider_home_* below.
    kw.setdefault("ensure_provider_home", lambda home, prov: None)
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

    def test_ensure_provider_home_creates_config_dir_and_is_idempotent(self):
        # Codex refuses to start when $CODEX_HOME is absent, so deliver pre-creates it.
        with tempfile.TemporaryDirectory() as home:
            credentials._ensure_provider_home(home, "codex")
            self.assertTrue(os.path.isdir(os.path.join(home, ".codex")))
            credentials._ensure_provider_home(home, "codex")  # idempotent
            credentials._ensure_provider_home(home, "claude")
            self.assertTrue(os.path.isdir(os.path.join(home, ".claude")))

    def test_deliver_creates_provider_home_on_the_physical_mount(self):
        with tempfile.TemporaryDirectory() as tmp:
            mount = os.path.join(tmp, "mount")
            os.mkdir(mount, 0o700)
            volumes = FakeVolumes({(3, 7, "codex", "h1"): mount})
            with deliver(
                volumes, "org:3/user:7", "codex", "h1", 61000, 61000,
                os.path.join(tmp, "locks"),
                chown_tree=RecordingChown(), stat_fn=fake_stat(),
                ensure_provider_home=credentials._ensure_provider_home,  # the real one
            ):
                self.assertTrue(os.path.isdir(os.path.join(mount, ".codex")))


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
        # On exit the canonical owner is restored, the mapping is given back, and
        # nothing was DELETED — close unmaps, revoke destroys, and a lend never revokes.
        self.assertEqual(chown.calls[-1], ("/mnt/vol-h1", C, C))
        self.assertEqual(volumes.closed, [(3, 7, "claude", "h1")])
        self.assertEqual(volumes.revoked, [])

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

    # -- releasing the mapping (the loop-device leak that broke every sign-in) ---- #
    #
    # Live 2026-09-24: `deliver` opened volumes and never closed them, so every volume
    # ever lent kept its dm mapping — and with it one autoclear loop device — for the
    # life of the container. The deployment had two loop nodes; once two connections
    # held them, `CredentialVolumes.create` failed at `cryptsetup open` for EVERY new
    # sign-in, the supervisor reported a bare UNAVAILABLE, and the server cancelled the
    # attempt it had just recorded. Users saw "This service is temporarily unavailable"
    # one second after pressing Connect, with nothing in any log.

    def test_release_runs_under_the_scope_run_lock(self):
        # The safety property behind unmapping at all: the close happens while this
        # scope's run lock is still held, so no other lend for the SAME volume can be
        # reading the mapping as it goes away. Proven, not asserted: a fresh fd on the
        # same lockfile must fail to take it while close is running (flock conflicts
        # across open file descriptions, including within one process).
        observed = {}

        def probe():
            fd = os.open(
                os.path.join(self.lock_dir, "3-7-claude.lock"),
                os.O_RDWR | os.O_CREAT, 0o600,
            )
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                observed["held"] = False  # we got it -> deliver had already let go
                fcntl.flock(fd, fcntl.LOCK_UN)
            except OSError:
                observed["held"] = True
            finally:
                os.close(fd)

        volumes = FakeVolumes({(3, 7, "claude", "h1"): "/mnt/vol-h1"}, on_close=probe)
        with deliver(
            volumes, "org:3/user:7", "claude", "h1", 61000, 61000,
            self.lock_dir, chown_tree=RecordingChown(), stat_fn=fake_stat(),
        ):
            pass
        self.assertEqual(volumes.closed, [(3, 7, "claude", "h1")])
        self.assertTrue(observed["held"])

    def test_release_runs_when_the_run_fails(self):
        volumes = self._volumes()
        with self.assertRaises(RuntimeError):
            with deliver(
                volumes, "org:3/user:7", "claude", "h1", 61000, 61000,
                self.lock_dir, chown_tree=RecordingChown(), stat_fn=fake_stat(),
            ):
                raise RuntimeError("child failed / cancelled")
        self.assertEqual(volumes.closed, [(3, 7, "claude", "h1")])

    def test_release_runs_when_the_lend_is_refused_after_open(self):
        # Everything between a successful `open` and the lend can refuse: a mount root
        # of the wrong shape or mode, a failing stat, a provider-home creation that
        # raises. Each of those exits owes the mapping back too — leaking one per
        # refusal is how the pool is exhausted by attempts that never ran at all.
        cases = [
            (dict(stat_fn=fake_stat(perm=0o755)), "mode"),
            (dict(stat_fn=fake_stat(isdir=False)), "shape"),
        ]
        for kwargs, category in cases:
            volumes = self._volumes()
            with self.assertRaises(credentials.CredentialError) as caught:
                with deliver(
                    volumes, "org:3/user:7", "claude", "h1", 61000, 61000,
                    self.lock_dir, chown_tree=RecordingChown(), **kwargs,
                ):
                    pass
            self.assertEqual(caught.exception.category, category)
            self.assertEqual(volumes.closed, [(3, 7, "claude", "h1")], category)

    def test_release_runs_when_preparing_the_provider_home_raises(self):
        volumes, chown = self._volumes(), RecordingChown()

        def boom(_home, _provider):
            raise OSError("cannot create config dir")

        # An OSError raised inside the lend surfaces through the run lock's own OSError
        # handling as CredentialError("lock") — a pre-existing mislabel, asserted here
        # as observed rather than quietly changed; what this test is about is that the
        # mapping is still given back on the way out.
        with self.assertRaises(credentials.CredentialError):
            with deliver(
                volumes, "org:3/user:7", "claude", "h1", 61000, 61000,
                self.lock_dir, chown_tree=chown, stat_fn=fake_stat(),
                ensure_provider_home=boom,
            ):
                pass
        self.assertEqual(volumes.closed, [(3, 7, "claude", "h1")])
        # It had already chowned to canonical by then, so that is restored too.
        C = credentials.CANONICAL_OWNER_UID
        self.assertEqual(chown.calls[-1], ("/mnt/vol-h1", C, C))

    def test_a_refusal_before_open_never_closes_anything(self):
        # `open` itself failing means there is no mapping of ours to give back, and
        # closing on a handle we were refused would be a cross-scope action.
        volumes = self._volumes()
        with self.assertRaises(credentials.CredentialError):
            with deliver(
                volumes, "org:3/user:8", "claude", "h1", 61000, 61000,
                self.lock_dir, chown_tree=RecordingChown(), stat_fn=fake_stat(),
            ):
                pass
        self.assertEqual(volumes.closed, [])

    def test_a_close_that_fails_is_reported_and_does_not_fail_the_run(self):
        # A busy unmount is documented to leave the volume for an administrator, the
        # store's metadata lock is non-blocking so a concurrent create can make this
        # transiently unavailable, and `open` revalidates and reuses a surviving
        # mapping. Failing a finished sign-in over that would be worse than the leak —
        # but it must never be silent.
        volumes = FakeVolumes(
            {(3, 7, "claude", "h1"): "/mnt/vol-h1"}, close_error=StoreError("busy")
        )
        with contextlib.redirect_stderr(io.StringIO()) as log:
            with deliver(
                volumes, "org:3/user:7", "claude", "h1", 61000, 61000,
                self.lock_dir, chown_tree=RecordingChown(), stat_fn=fake_stat(),
            ) as delivery:
                self.assertEqual(delivery.home, "/mnt/vol-h1")
        self.assertEqual(log.getvalue(), "NEEV-CRED-DIAG stage=release code=busy\n")

    def test_a_close_failure_reports_only_an_allowlisted_code(self):
        # An exception from outside the store's fixed vocabulary must not put its
        # message — which could carry a path or a handle — into the daemon log.
        volumes = FakeVolumes(
            {(3, 7, "claude", "h1"): "/mnt/vol-h1"},
            close_error=RuntimeError("/var/lib/neev-supervisor/credentials/deadbeef"),
        )
        with contextlib.redirect_stderr(io.StringIO()) as log:
            with deliver(
                volumes, "org:3/user:7", "claude", "h1", 61000, 61000,
                self.lock_dir, chown_tree=RecordingChown(), stat_fn=fake_stat(),
            ):
                pass
        self.assertEqual(log.getvalue(), "NEEV-CRED-DIAG stage=release code=unknown\n")
        self.assertNotIn("deadbeef", log.getvalue())

    def test_volumes_without_close_are_still_supported(self):
        # `open(scope, handle) -> path` is the documented minimum for a caller-supplied
        # double; a missing close is not an error.
        volumes = OpenOnlyVolumes({(3, 7, "claude", "h1"): "/mnt/vol-h1"})
        with deliver(
            volumes, "org:3/user:7", "claude", "h1", 61000, 61000,
            self.lock_dir, chown_tree=RecordingChown(), stat_fn=fake_stat(),
        ) as delivery:
            self.assertEqual(delivery.home, "/mnt/vol-h1")

    def test_repeated_connections_never_accumulate_open_volumes(self):
        # The invariant that keeps the pool bounded: after N sign-ins/runs across
        # several users and both providers, nothing is left mapped. Capacity depends on
        # how many lends are IN FLIGHT, never on how many users have ever signed in.
        seeded = {
            (3, user, provider, f"h{user}{provider}"): f"/mnt/vol-{user}-{provider}"
            for user in (7, 8, 9)
            for provider in ("claude", "codex")
        }
        volumes = FakeVolumes(seeded)
        for _round in range(3):
            for (org, user, provider, handle) in sorted(seeded):
                with deliver(
                    volumes, f"org:{org}/user:{user}", provider, handle, 61000, 61000,
                    self.lock_dir, chown_tree=RecordingChown(), stat_fn=fake_stat(),
                ):
                    pass
        self.assertEqual(len(volumes.opened), 18)
        # Every open was paired with a close of the SAME scope and handle: a lend can
        # never unmap another user's or another provider's volume.
        self.assertEqual(volumes.closed, volumes.opened)
        self.assertEqual(volumes.revoked, [])

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
