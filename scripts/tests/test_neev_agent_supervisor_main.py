"""Tests for the supervisor daemon: config parsing, factory wiring (concrete drivers/
probes with FIXED provider argv), and the abandoned-run purge. The fence/socket/serve
glue + LoginChannel lifecycle are Linux-only."""

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
        "NEEV_SUPERVISOR_SOCKET": "/run/neev/ipc/control.sock",
        "NEEV_SUPERVISOR_KEY": "/run/neev/ipc/control.key",
        "NEEV_SUPERVISOR_FENCE_DIR": "/run/neev/fence",
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
        "NEEV_SUPERVISOR_UPLINK": "eth0",
        "NEEV_SUPERVISOR_EGRESS_IPV4": "1.1.1.1,8.8.8.8",
    }


class _Scope:
    def __init__(self, org_id, user_id):
        self.org_id = org_id
        self.user_id = user_id


class ConfigTests(unittest.TestCase):
    def test_full_env_parses(self):
        config = main.config_from_env(_full_env("/data/agents"))
        self.assertEqual(config.control_gid, 65532)
        self.assertEqual(config.peer_gid, 0)
        self.assertEqual(config.lane_uid, 61000)
        self.assertEqual(config.slot, "a")
        self.assertEqual(config.uplink, "eth0")
        self.assertEqual(config.fence_dir, "/run/neev/fence")
        self.assertEqual(config.egress_ipv4, ("1.1.1.1", "8.8.8.8"))

    def test_missing_or_bad_values_fail_closed(self):
        base = _full_env("/data/agents")
        for drop in ("NEEV_SUPERVISOR_SOCKET", "NEEV_SUPERVISOR_LANE_UID",
                     "NEEV_SUPERVISOR_FENCE_DIR", "NEEV_SUPERVISOR_UPLINK",
                     "NEEV_SUPERVISOR_EGRESS_IPV4"):
            env = dict(base)
            env.pop(drop)
            with self.subTest(drop=drop):
                with self.assertRaises(main.ConfigError):
                    main.config_from_env(env)
        with self.assertRaises(main.ConfigError):
            main.config_from_env(dict(base, NEEV_SUPERVISOR_SLOT="c"))
        with self.assertRaises(main.ConfigError):
            main.config_from_env(dict(base, NEEV_SUPERVISOR_CONTROL_GID="notanint"))
        # The control key MUST be named control.key (the transport loads it by name).
        with self.assertRaises(main.ConfigError):
            main.config_from_env(dict(base, NEEV_SUPERVISOR_KEY="/run/neev/other.key"))


class FactoryTests(unittest.TestCase):
    def _factories(self):
        config = main.config_from_env(_full_env("/data/agents"))
        self.built = []

        def channel_factory(provider, store_scope, handle, argv):
            self.built.append((provider, tuple(argv)))
            return object()  # not started -> no sandbox/deliver

        return main.make_factories(config, volumes=object(), channel_factory=channel_factory)

    def test_driver_for_builds_concrete_drivers_with_fixed_argv(self):
        driver_for, _probe = self._factories()
        self.assertIsInstance(driver_for(dto.Provider.CODEX, _Scope(3, 7), "h" * 32),
                              drivers.CodexLoginDriver)
        self.assertIsInstance(driver_for(dto.Provider.CLAUDE, _Scope(3, 7), "h" * 32),
                              drivers.ClaudeLoginDriver)
        self.assertEqual(self.built[0], (dto.Provider.CODEX, ("codex", "app-server")))
        self.assertEqual(self.built[1], (dto.Provider.CLAUDE, ("claude", "auth", "login")))

    def test_probe_for_builds_concrete_probes_with_status_argv(self):
        _driver, probe_for = self._factories()
        self.assertIsInstance(probe_for(dto.Provider.CODEX, _Scope(3, 7), "h" * 32),
                              drivers.CodexAccountProbe)
        self.assertIsInstance(probe_for(dto.Provider.CLAUDE, _Scope(3, 7), "h" * 32),
                              drivers.ClaudeAccountProbe)
        # Claude probe uses the native `claude auth status --json`.
        self.assertEqual(self.built[-1], (dto.Provider.CLAUDE, ("claude", "auth", "status", "--json")))


class SlotQuarantineTests(unittest.TestCase):
    """A login channel whose sandbox child / run workdir did not tear down cleanly must
    write the DURABLE slot_proxy quarantine (fixed lane) so the next login AND probe are
    refused, and the failure must be raised, never swallowed."""

    def _channel(self, cleanup, quarantine_dir):
        config = main.config_from_env(_full_env("/data/agents"))
        return main.LoginChannel(
            config, object(), dto.Provider.CODEX, _Scope(3, 7), "h" * 32,
            ("codex", "app-server"), cleanup=cleanup, quarantine_dir=quarantine_dir,
        )

    def test_cleanup_failure_quarantines_slot_and_blocks_next_tenant(self):
        import neev_slot_proxy as slot_proxy

        with tempfile.TemporaryDirectory() as qdir:
            channel = self._channel(lambda path: False, qdir)  # workdir removal fails
            channel._workdir = "/data/agents/run-xyz"
            with self.assertRaises(main.ChannelError):
                channel.terminate()  # surfaced, not swallowed
            self.assertTrue(slot_proxy.is_quarantined("a", directory=qdir))
            # The NEXT login AND probe on the same fixed slot are refused.
            driver_for, probe_for = main.make_factories(
                main.config_from_env(_full_env("/data/agents")), object(),
                quarantine_dir=qdir,
            )
            with self.assertRaises(main.ChannelError):
                driver_for(dto.Provider.CODEX, _Scope(9, 9), "g" * 32)
            with self.assertRaises(main.ChannelError):
                probe_for(dto.Provider.CLAUDE, _Scope(9, 9), "g" * 32)

    def test_clean_unwind_does_not_quarantine(self):
        import neev_slot_proxy as slot_proxy

        with tempfile.TemporaryDirectory() as qdir:
            channel = self._channel(lambda path: True, qdir)  # workdir removed cleanly
            channel._workdir = "/data/agents/run-xyz"
            channel.terminate()  # no raise
            self.assertFalse(slot_proxy.is_quarantined("a", directory=qdir))


class PurgeTests(unittest.TestCase):
    def test_purges_only_validated_run_dirs(self):
        with tempfile.TemporaryDirectory() as root:
            os.mkdir(os.path.join(root, "run-abc123"))
            os.mkdir(os.path.join(root, "run-def456"))
            os.mkdir(os.path.join(root, "notrun"))          # not a run dir
            os.symlink("/etc", os.path.join(root, "run-link"))  # symlink, skipped
            removed = []

            def fake_cleanup(path):
                removed.append(os.path.basename(path))
                return True

            count = main.purge_abandoned_runs(root, cleanup=fake_cleanup)
            self.assertEqual(count, 2)
            self.assertEqual(sorted(removed), ["run-abc123", "run-def456"])
            # non-run dir and the symlink were never handed to cleanup.
            self.assertNotIn("notrun", removed)
            self.assertNotIn("run-link", removed)

    def test_missing_root_is_noop(self):
        self.assertEqual(main.purge_abandoned_runs("/nonexistent/neev/root", cleanup=lambda p: True), 0)

    def test_abandoned_runs_remain_detects_unremovable_leftover(self):
        with tempfile.TemporaryDirectory() as root:
            self.assertFalse(main._abandoned_runs_remain(root))  # clean
            os.mkdir(os.path.join(root, "run-leftover"))
            self.assertTrue(main._abandoned_runs_remain(root))   # a run dir survived
            os.symlink("/etc", os.path.join(root, "run-link"))   # symlink never counts
            os.rmdir(os.path.join(root, "run-leftover"))
            self.assertFalse(main._abandoned_runs_remain(root))


if __name__ == "__main__":
    unittest.main()
