"""Pure spec/argv/distinctness tests for the agent sandbox launcher.

Real namespace/egress/two-user kernel isolation is a Linux acceptance gate
(`neev_agent_sandbox_acceptance.py`), NOT exercised here (darwin). These tests
prove the platform-agnostic policy: which specs are accepted, that the launch
argv carries the isolation primitives, and that two tenants never share identity.
"""

import platform
import sys
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_agent_sandbox as sandbox  # noqa: E402


def spec(**overrides):
    base = dict(
        tenant="org:3/user:7",
        slot="a",
        workdir="/var/lib/neev-native/agents/org3-user7",
        run_uid=61000,
        run_gid=61000,
        provider_argv=("claude", "-p", "hi", "--output-format", "stream-json"),
        limits=sandbox.ResourceLimits(
            cpu_seconds=120,
            address_space_bytes=2 * 1024 * 1024 * 1024,
            open_files=256,
            processes=64,
        ),
    )
    base.update(overrides)
    return sandbox.SandboxSpec(**base)


class SandboxSpecTests(unittest.TestCase):
    def test_valid_spec_accepted(self):
        sandbox.validate_spec(spec())  # no raise

    def test_rejects_privileged_or_login_uid(self):
        for bad in (0, 1000, 59999, 70000):
            with self.assertRaises(sandbox.SandboxError) as caught:
                sandbox.validate_spec(spec(run_uid=bad, run_gid=bad))
            self.assertEqual(caught.exception.category, "invalid")

    def test_rejects_bad_slot_workdir_argv_limits(self):
        cases = [
            dict(slot="c"),
            dict(workdir="relative/x"),
            dict(workdir="/a/../b"),
            dict(workdir="/"),
            dict(provider_argv=()),
            dict(provider_argv=("ok", "bad\x00")),
            dict(limits=sandbox.ResourceLimits(0, 1, 1, 1)),
            dict(limits=sandbox.ResourceLimits(1, 1, 1, -1)),
        ]
        for override in cases:
            with self.assertRaises(sandbox.SandboxError):
                sandbox.validate_spec(spec(**override))

    def test_launch_argv_carries_every_isolation_primitive(self):
        argv = sandbox.build_launch_argv(spec())
        # New namespaces (mount/pid/net/uts/ipc) + fork so the agent is PID 1,
        # plus a private /proc so only the sandbox's own processes are visible.
        for flag in ("--mount", "--uts", "--ipc", "--pid", "--net", "--fork", "--mount-proc"):
            self.assertIn(flag, argv)
        # A single-uid user namespace is deliberately NOT used (it forces
        # setgroups=deny and maps back to host root); real uid drop is via setpriv.
        self.assertNotIn("--user", argv)
        # Dropped identity + cleared groups + hardened privileges so a dropped
        # agent can never regain root by exec'ing a setuid-root binary.
        self.assertIn("--reuid=61000", argv)
        self.assertIn("--regid=61000", argv)
        self.assertIn("--clear-groups", argv)
        self.assertIn("--no-new-privs", argv)
        self.assertIn("--bounding-set=-all", argv)
        self.assertIn("--inh-caps=-all", argv)
        # Resource caps.
        self.assertIn("--cpu=120", argv)
        self.assertIn("--nproc=64", argv)
        self.assertIn("--nofile=256", argv)
        # The provider command runs last, after both `--` separators.
        self.assertEqual(argv[-5:], list(spec().provider_argv))
        self.assertEqual(argv[0], sandbox.UNSHARE)

    def test_deny_all_argv_uses_fresh_net_namespace(self):
        # No egress netns supplied => a fresh empty --net namespace (deny-all).
        argv = sandbox.build_launch_argv(spec())
        self.assertIn("--net", argv)
        self.assertNotIn("exec", argv)  # no `ip netns exec` prefix
        self.assertEqual(argv[0], sandbox.UNSHARE)

    def test_provider_netns_join_omits_net_and_prefixes_ip_netns_exec(self):
        # With a wired egress netns the agent must JOIN it (no --net), otherwise
        # unshare --net would discard the allowlist and behave as deny-all.
        argv = sandbox.build_launch_argv(spec(), join_netns="neev-agent-a")
        self.assertEqual(argv[:4], [sandbox.IP, "netns", "exec", "neev-agent-a"])
        self.assertNotIn("--net", argv)
        self.assertIn("--mount-proc", argv)
        # A bogus netns name is rejected (no shell metacharacters / prefix).
        with self.assertRaises(sandbox.SandboxError):
            sandbox.build_launch_argv(spec(), join_netns="; rm -rf /")

    def test_home_path_is_a_home_leaf_under_the_run_workdir(self):
        self.assertEqual(
            sandbox.home_path("/var/lib/neev-native/agents/org3-user7"),
            "/var/lib/neev-native/agents/org3-user7/home",
        )
        with self.assertRaises(sandbox.SandboxError):
            sandbox.home_path("relative/x")

    def test_bind_source_inserts_the_ns_prelude_before_the_drop_chain(self):
        mount = "/var/lib/neev-supervisor/credentials/" + "a" * 32 + "/mount"
        argv = sandbox.build_launch_argv(spec(), bind_source=mount)
        # The prelude runs the fixed interpreter + prelude script, as root inside the
        # new mount namespace, BEFORE prlimit/setpriv drop privilege.
        self.assertIn(sandbox.PYTHON, argv)
        self.assertIn(sandbox.NS_PRELUDE, argv)
        i = argv.index(sandbox.NS_PRELUDE)
        self.assertEqual(argv[i + 1:i + 5], ["--src", mount, "--dst",
                                             sandbox.home_path(spec().workdir)])
        self.assertEqual(argv[i + 5], "--")
        # Prelude sits after unshare, before prlimit/setpriv; provider still last.
        self.assertLess(argv.index(sandbox.NS_PRELUDE), argv.index(sandbox.PRLIMIT))
        self.assertLess(argv.index(sandbox.PRLIMIT), argv.index(sandbox.SETPRIV))
        self.assertEqual(argv[-5:], list(spec().provider_argv))
        # A malformed bind source (traversal) is rejected.
        with self.assertRaises(sandbox.SandboxError):
            sandbox.build_launch_argv(spec(), bind_source="/a/../b")

    def test_no_bind_source_omits_the_prelude(self):
        argv = sandbox.build_launch_argv(spec())
        self.assertNotIn(sandbox.NS_PRELUDE, argv)

    def test_distinct_tenants_requires_separate_identity(self):
        a = spec()
        b = spec(
            tenant="org:3/user:9",
            slot="b",
            workdir="/var/lib/neev-native/agents/org3-user9",
            run_uid=61001,
            run_gid=61001,
        )
        self.assertTrue(sandbox.distinct_tenants(a, b))
        # Any shared identity dimension denies co-residency.
        self.assertFalse(sandbox.distinct_tenants(a, spec(run_uid=61000, run_gid=61001, slot="b", workdir="/x/y")))
        self.assertFalse(sandbox.distinct_tenants(a, spec(slot="a", run_uid=61001, run_gid=61001, workdir="/x/y")))

    @unittest.skipIf(platform.system() == "Linux", "launch runs on Linux")
    def test_launch_is_unsupported_off_linux(self):
        with self.assertRaises(sandbox.SandboxError) as caught:
            sandbox.launch(spec())
        self.assertEqual(caught.exception.category, "unsupported")


if __name__ == "__main__":
    unittest.main()


class EgressPolicyTests(unittest.TestCase):
    def test_deny_all_denies_everything(self):
        policy = sandbox.EgressPolicy.deny_all()
        self.assertFalse(sandbox.egress_decision(policy, "140.82.112.3"))
        self.assertFalse(sandbox.egress_decision(policy, "10.0.0.1"))

    def test_provider_allowlist_permits_only_listed_and_denies_others(self):
        policy = sandbox.EgressPolicy.provider_allowlist(["140.82.112.3", "140.82.112.4"])
        self.assertTrue(sandbox.egress_decision(policy, "140.82.112.3"))
        self.assertTrue(sandbox.egress_decision(policy, "140.82.112.4"))
        # Default-deny: an unapproved destination (incl. cross-tenant subnets).
        self.assertFalse(sandbox.egress_decision(policy, "140.82.112.9"))
        self.assertFalse(sandbox.egress_decision(policy, "10.203.0.11"))

    def test_spec_egress_validation(self):
        # Empty allowlist and mislabeled deny_all-with-ips are rejected.
        with self.assertRaises(sandbox.SandboxError):
            sandbox.validate_spec(spec(egress=sandbox.EgressPolicy(mode="provider_allowlist", provider_ipv4=())))
        with self.assertRaises(sandbox.SandboxError):
            sandbox.validate_spec(spec(egress=sandbox.EgressPolicy(mode="deny_all", provider_ipv4=("1.1.1.1",))))
        with self.assertRaises(sandbox.SandboxError):
            sandbox.validate_spec(spec(egress=sandbox.EgressPolicy(mode="bogus")))
        # A valid provider allowlist spec is accepted.
        sandbox.validate_spec(spec(egress=sandbox.EgressPolicy.provider_allowlist(["140.82.112.3"])))

    def test_provider_allowlist_rejects_malformed_ipv4(self):
        with self.assertRaises(Exception):
            sandbox.EgressPolicy.provider_allowlist(["not-an-ip"])
