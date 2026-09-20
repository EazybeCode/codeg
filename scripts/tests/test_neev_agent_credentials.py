"""Portable tests for per-user/per-provider credential delivery.

No real cryptsetup/root: a fake CredentialVolumes stands in for the LUKS layer and
an injected stat_fn simulates the mount-root ownership, so these prove the
parser-to-open scope mapping, cross-user/handle refusal, missing credentials,
ownership/mode enforcement, secret-free env, and — crucially — that delivery
mutates nothing (no chown/close), so concurrent same-user runs on different lane
uids cannot race."""

import os
import stat
import sys
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_agent_credentials as credentials  # noqa: E402
from neev_credential_store import Scope, StoreError  # noqa: E402


class FakeVolumes:
    """Stands in for CredentialVolumes. `open` mirrors the real scope-bound key:
    it only returns a mountpoint for an exact (scope, handle) it was seeded with,
    so a mismatched handle/scope raises just like a failed LUKS unlock. It never
    chowns/unmounts, and records every call so tests can assert no mutation."""

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

    def close(self, scope, handle):  # must never be called by the launcher
        self.closed.append((scope.org_id, scope.user_id, scope.provider, handle))


def fake_stat(uid, perm=0o700, isdir=True):
    mode = (stat.S_IFDIR if isdir else stat.S_IFREG) | perm

    class _Stat:
        st_mode = mode
        st_uid = uid

    return lambda _path: _Stat()


class ParseTenantTests(unittest.TestCase):
    def test_valid(self):
        self.assertEqual(credentials.parse_tenant("org:3/user:7"), (3, 7))

    def test_rejects_malformed(self):
        for bad in [
            "org:3",
            "user:7/org:3",
            "org:3/user:7/x",
            "org:0/user:7",
            "org:3/user:0",
            "org:03/user:7",  # non-canonical leading zero
            "org:-1/user:7",
            "org:x/user:7",
            "org: 3/user:7",
            "",
        ]:
            with self.assertRaises(credentials.CredentialError, msg=bad):
                credentials.parse_tenant(bad)


class ProviderEnvTests(unittest.TestCase):
    def test_claude_and_codex_paths_are_under_home(self):
        claude = credentials.provider_env("/vol", "claude")
        self.assertEqual(claude["HOME"], "/vol")
        self.assertEqual(claude["CLAUDE_CONFIG_DIR"], "/vol/.claude")
        self.assertNotIn("CODEX_HOME", claude)
        codex = credentials.provider_env("/vol", "codex")
        self.assertEqual(codex["CODEX_HOME"], "/vol/.codex")
        self.assertNotIn("CLAUDE_CONFIG_DIR", codex)

    def test_unknown_provider_rejected(self):
        with self.assertRaises(credentials.CredentialError):
            credentials.provider_env("/vol", "gemini")


class ResolveDeliveryTests(unittest.TestCase):
    def _volumes(self):
        return FakeVolumes({(3, 7, "claude", "h1"): "/mnt/vol-h1"})

    def test_happy_path_maps_scope_and_delivers_home_env(self):
        volumes = self._volumes()
        delivery = credentials.resolve_delivery(
            volumes, "org:3/user:7", "claude", "h1", 65532, stat_fn=fake_stat(65532)
        )
        # Scope is built from tenant + provider and passed to open verbatim.
        self.assertEqual(volumes.opened, [(3, 7, "claude", "h1")])
        self.assertEqual(delivery.home, "/mnt/vol-h1")
        self.assertEqual(delivery.env["HOME"], "/mnt/vol-h1")
        self.assertEqual(delivery.env["CLAUDE_CONFIG_DIR"], "/mnt/vol-h1/.claude")
        # Delivery is mutation-free: nothing was closed/unmounted.
        self.assertEqual(volumes.closed, [])

    def test_cross_user_or_unknown_handle_is_refused(self):
        volumes = self._volumes()
        # Right handle, WRONG user (8 not 7): open cannot unlock this scope.
        with self.assertRaises(credentials.CredentialError) as caught:
            credentials.resolve_delivery(
                volumes, "org:3/user:8", "claude", "h1", 65532, stat_fn=fake_stat(65532)
            )
        self.assertEqual(caught.exception.category, "credentials")
        # Unknown handle likewise.
        with self.assertRaises(credentials.CredentialError):
            credentials.resolve_delivery(
                volumes, "org:3/user:7", "claude", "nope", 65532, stat_fn=fake_stat(65532)
            )
        self.assertEqual(volumes.closed, [])

    def test_missing_credentials_refused(self):
        with self.assertRaises(credentials.CredentialError) as caught:
            credentials.resolve_delivery(
                FakeVolumes({}), "org:3/user:7", "codex", "h1", 65532,
                stat_fn=fake_stat(65532),
            )
        self.assertEqual(caught.exception.category, "credentials")

    def test_volume_not_owned_by_run_uid_is_refused_without_chown(self):
        volumes = self._volumes()
        with self.assertRaises(credentials.CredentialError) as caught:
            credentials.resolve_delivery(
                volumes, "org:3/user:7", "claude", "h1", 65532,
                stat_fn=fake_stat(61000),  # owned by a different uid
            )
        self.assertEqual(caught.exception.category, "owner")
        self.assertEqual(volumes.closed, [])  # never mutated to "fix" ownership

    def test_wrong_mode_or_non_directory_refused(self):
        volumes = self._volumes()
        with self.assertRaises(credentials.CredentialError) as m:
            credentials.resolve_delivery(
                volumes, "org:3/user:7", "claude", "h1", 65532,
                stat_fn=fake_stat(65532, perm=0o755),
            )
        self.assertEqual(m.exception.category, "mode")
        with self.assertRaises(credentials.CredentialError) as d:
            credentials.resolve_delivery(
                volumes, "org:3/user:7", "claude", "h1", 65532,
                stat_fn=fake_stat(65532, isdir=False),
            )
        self.assertEqual(d.exception.category, "shape")

    def test_concurrent_lane_uids_cannot_race_a_chown(self):
        # Two concurrent runs for the SAME (user, provider) on DIFFERENT lane uids.
        # Whichever uid owns the volume is delivered; the other is refused. Neither
        # mutates the volume (no chown/close), so there is no ownership to race.
        volumes = self._volumes()
        ok = credentials.resolve_delivery(
            volumes, "org:3/user:7", "claude", "h1", 65532, stat_fn=fake_stat(65532)
        )
        self.assertEqual(ok.env["HOME"], "/mnt/vol-h1")
        with self.assertRaises(credentials.CredentialError) as other:
            credentials.resolve_delivery(
                volumes, "org:3/user:7", "claude", "h1", 61001, stat_fn=fake_stat(65532)
            )
        self.assertEqual(other.exception.category, "owner")
        self.assertEqual(volumes.closed, [])
        self.assertEqual(len(volumes.opened), 2)


if __name__ == "__main__":
    unittest.main()
