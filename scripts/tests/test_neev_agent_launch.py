"""Portable tests for the production agent launch entrypoint's spec construction.
Actually spawning the sandbox is Linux-only (`neev_agent_sandbox_acceptance.py`);
these prove the CLI maps to a correct, validated SandboxSpec + egress policy."""

import sys
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_agent_launch as launch  # noqa: E402
import neev_agent_sandbox as sandbox  # noqa: E402


def args(extra):
    base = [
        "--tenant", "org:3/user:7",
        "--slot", "a",
        "--workdir", "/var/lib/neev-native/agents/org3-user7",
        "--uid", "61000",
        "--gid", "61000",
    ]
    return launch._parse(base + extra)


class LaunchSpecTests(unittest.TestCase):
    def test_deny_all_when_no_provider_ip(self):
        spec = launch.build_spec(args(["--", "claude", "-p", "hi"]))
        self.assertEqual(spec.egress.mode, "deny_all")
        self.assertEqual(spec.provider_argv, ("claude", "-p", "hi"))
        self.assertEqual(spec.run_uid, 61000)

    def test_provider_allowlist_when_ip_given(self):
        spec = launch.build_spec(
            args(["--provider-ipv4", "140.82.112.3", "--", "claude", "-p", "hi"])
        )
        self.assertEqual(spec.egress.mode, "provider_allowlist")
        self.assertEqual(spec.egress.provider_ipv4, ("140.82.112.3",))

    def test_missing_provider_argv_rejected(self):
        with self.assertRaises(sandbox.SandboxError):
            launch.build_spec(args([]))

    def test_invalid_uid_rejected(self):
        with self.assertRaises(sandbox.SandboxError):
            launch.build_spec(
                launch._parse([
                    "--tenant", "t", "--slot", "a", "--workdir", "/x/y",
                    "--uid", "0", "--gid", "0", "--", "claude",
                ])
            )

    def test_malformed_provider_ip_rejected(self):
        with self.assertRaises(Exception):
            launch.build_spec(
                args(["--provider-ipv4", "not-an-ip", "--", "claude", "-p", "hi"])
            )


if __name__ == "__main__":
    unittest.main()
