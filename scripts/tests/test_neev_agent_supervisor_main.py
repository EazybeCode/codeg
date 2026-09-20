"""Tests for the supervisor daemon config parsing + driver/probe factory wiring.

The fence/socket/serve glue is Linux-only; here the pure config and the factories
(which build CONCRETE drivers/probes without spawning) are exercised."""

import os
import sys
import tempfile
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_supervisor_dto as dto  # noqa: E402
import neev_agent_supervisor_main as main  # noqa: E402
import neev_agent_login_drivers as drivers  # noqa: E402


def _full_env(workspace_root):
    return {
        "NEEV_SUPERVISOR_SOCKET": "/run/neev/supervisor.sock",
        "NEEV_SUPERVISOR_KEY": "/run/neev/control.key",
        "NEEV_SUPERVISOR_STORE_ROOT": "/var/lib/neev-supervisor/credentials",
        "NEEV_SUPERVISOR_MASTER_KEY": "/etc/neev-supervisor/credential-master.key",
        "NEEV_AGENT_WORKSPACE_ROOT": workspace_root,
        "NEEV_SUPERVISOR_REGISTRY": "/var/lib/neev-supervisor/registry.json",
        "NEEV_SUPERVISOR_CONTROL_GID": "65532",
        "NEEV_SUPERVISOR_PEER_UID": "0",
        "NEEV_SUPERVISOR_PEER_GID": "0",
        "NEEV_SUPERVISOR_LANE_UID": "61000",
        "NEEV_SUPERVISOR_LANE_GID": "61000",
        "NEEV_SUPERVISOR_SLOT": "a",
        "NEEV_SUPERVISOR_CODEX_LOGIN_ARGV": "codex\x00app-server",
        "NEEV_SUPERVISOR_CLAUDE_LOGIN_ARGV": "claude\x00auth\x00login",
        "NEEV_SUPERVISOR_CODEX_PROBE_ARGV": "codex\x00app-server",
        "NEEV_SUPERVISOR_CLAUDE_PROBE_ARGV": "claude\x00auth\x00status\x00--json",
        "NEEV_SUPERVISOR_EGRESS_IPV4": "1.1.1.1,8.8.8.8",
    }


class _Scope:
    def __init__(self, org_id, user_id):
        self.org_id = org_id
        self.user_id = user_id


class _Volumes:
    def __init__(self, mount):
        self._mount = mount
        self.opened = []

    def open(self, store_scope, handle):
        self.opened.append(handle)
        return self._mount


class ConfigTests(unittest.TestCase):
    def test_full_env_parses(self):
        with tempfile.TemporaryDirectory() as workspace:
            config = main.config_from_env(_full_env(workspace))
        self.assertEqual(config.control_gid, 65532)
        self.assertEqual(config.peer_gid, 0)
        self.assertEqual(config.lane_uid, 61000)
        self.assertEqual(config.slot, "a")
        self.assertEqual(config.claude_probe_argv, ("claude", "auth", "status", "--json"))
        self.assertEqual(config.codex_login_argv, ("codex", "app-server"))
        self.assertEqual(config.egress_ipv4, ("1.1.1.1", "8.8.8.8"))

    def test_missing_or_bad_values_fail_closed(self):
        with tempfile.TemporaryDirectory() as workspace:
            base = _full_env(workspace)
            for drop in ("NEEV_SUPERVISOR_SOCKET", "NEEV_SUPERVISOR_LANE_UID",
                         "NEEV_SUPERVISOR_CLAUDE_PROBE_ARGV", "NEEV_SUPERVISOR_EGRESS_IPV4"):
                env = dict(base)
                env.pop(drop)
                with self.subTest(drop=drop):
                    with self.assertRaises(main.ConfigError):
                        main.config_from_env(env)
            # bad slot / non-int gid
            bad_slot = dict(base, NEEV_SUPERVISOR_SLOT="c")
            with self.assertRaises(main.ConfigError):
                main.config_from_env(bad_slot)
            bad_gid = dict(base, NEEV_SUPERVISOR_CONTROL_GID="notanint")
            with self.assertRaises(main.ConfigError):
                main.config_from_env(bad_gid)


class FactoryTests(unittest.TestCase):
    def _build(self):
        self.workspace = tempfile.TemporaryDirectory()
        self.addCleanup(self.workspace.cleanup)
        self.mount = tempfile.TemporaryDirectory()
        self.addCleanup(self.mount.cleanup)
        config = main.config_from_env(_full_env(self.workspace.name))
        self.built = []

        def channel_factory(spec, mount, env):
            self.built.append((tuple(spec.provider_argv), mount, env))
            return object()  # not started -> no spawn

        volumes = _Volumes(self.mount.name)
        return main.make_factories(config, volumes, channel_factory=channel_factory)

    def test_driver_for_builds_concrete_provider_drivers(self):
        driver_for, _probe_for = self._build()
        codex = driver_for(dto.Provider.CODEX, _Scope(3, 7), "h" * 32)
        self.assertIsInstance(codex, drivers.CodexLoginDriver)
        claude = driver_for(dto.Provider.CLAUDE, _Scope(3, 7), "h" * 32)
        self.assertIsInstance(claude, drivers.ClaudeLoginDriver)
        # Codex login used the codex login argv; Claude used the claude one.
        self.assertEqual(self.built[0][0], ("codex", "app-server"))
        self.assertEqual(self.built[1][0], ("claude", "auth", "login"))
        # The child env routes through the slot proxy and points HOME under the run dir.
        env = self.built[0][2]
        self.assertIn("HTTPS_PROXY", env)
        self.assertTrue(env["HOME"].endswith("/home"))

    def test_probe_for_builds_concrete_probes_with_status_argv(self):
        _driver_for, probe_for = self._build()
        self.assertIsInstance(probe_for(dto.Provider.CODEX, _Scope(3, 7), "h" * 32),
                              drivers.CodexAccountProbe)
        self.assertIsInstance(probe_for(dto.Provider.CLAUDE, _Scope(3, 7), "h" * 32),
                              drivers.ClaudeAccountProbe)
        # The Claude probe uses the native `claude auth status --json`, not file presence.
        self.assertEqual(self.built[-1][0], ("claude", "auth", "status", "--json"))


if __name__ == "__main__":
    unittest.main()
