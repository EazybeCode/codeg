"""Admin-configured integration hosts on the slot egress proxy.

The proxy stays an EXACT allowlist: integration hosts are added as exact
`host:443` authorities, validated as public DNS names, capped, and resolved by
the same isolated resolver (so a host resolving to a private address is still
denied whole). Nothing here may widen the provider's own table."""

import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "operations"))

import neev_provider_egress as egress  # noqa: E402
import neev_provider_endpoints as endpoints  # noqa: E402
import neev_slot_proxy as slot_proxy  # noqa: E402


class IntegrationEndpointsTest(unittest.TestCase):
    def test_valid_hosts_become_exact_443_authorities(self):
        table = endpoints.integration_endpoints(["api.fireflies.ai", "mcp.sanity.io"])
        self.assertEqual(
            sorted(e.authority() for e in table),
            [b"api.fireflies.ai:443", b"mcp.sanity.io:443"],
        )

    def test_rejects_ip_literals_local_names_wildcards_and_overflow(self):
        for bad in (
            "10.0.0.1", "169.254.169.254", "127.0.0.1", "localhost", "metadata.internal",
            "printer.local", "x.localhost", "*.example.com", "Example.com", "a..b",
            "host:443", "", "api.example.com.", "1.2.3.4.nip.io.",
        ):
            with self.subTest(bad=bad):
                with self.assertRaises(endpoints.EndpointError):
                    endpoints.integration_endpoints([bad])
        with self.assertRaises(endpoints.EndpointError):
            endpoints.integration_endpoints(["h%d.example.com" % i for i in range(33)])
        with self.assertRaises(endpoints.EndpointError):
            endpoints.integration_endpoints(["a.example.com", "a.example.com"])
        with self.assertRaises(endpoints.EndpointError):
            endpoints.integration_endpoints("api.example.com")

    def test_policy_merges_without_touching_the_provider_table(self):
        base = egress.policy_for("claude", "run")
        merged = egress.policy_for("claude", "run", extra_hosts=("api.fireflies.ai",))
        self.assertTrue(base.authorities < merged.authorities)
        self.assertIn(b"api.fireflies.ai:443", merged.authorities)
        self.assertEqual(merged.port_for("api.fireflies.ai"), 443)
        self.assertIn("'api.fireflies.ai'", merged.resolver_code["api.fireflies.ai"])
        # The provider-only policy is unchanged by a merged one existing.
        self.assertNotIn(b"api.fireflies.ai:443", egress.policy_for("claude", "run").authorities)

    def test_integration_hosts_get_long_idle_and_lifetime_provider_hosts_do_not(self):
        policy = egress.policy_for("claude", "run", extra_hosts=("getunblocked.com",))
        self.assertEqual(policy.limits_for("getunblocked.com"),
                         (egress.INTEGRATION_IDLE_SECONDS, egress.INTEGRATION_LIFETIME_SECONDS))
        self.assertEqual(policy.limits_for("api.anthropic.com"),
                         (egress.IDLE_SECONDS, egress.LIFETIME_SECONDS))
        self.assertGreaterEqual(egress.INTEGRATION_IDLE_SECONDS, 300)
        self.assertEqual(egress.policy_for("claude", "run").limits, {})

    def test_integration_only_scope_requires_hosts(self):
        policy = egress.policy_for("none", "integrations", extra_hosts=("api.hubspot.com",))
        self.assertEqual(policy.authorities, frozenset([b"api.hubspot.com:443"]))
        with self.assertRaises(egress.EgressError):
            egress.policy_for("none", "integrations")
        with self.assertRaises(egress.EgressError):
            egress.policy_for("none", "run", extra_hosts=("api.hubspot.com",))

    def test_connect_to_an_integration_host_parses_and_others_are_denied(self):
        policy = egress.policy_for("claude", "run", extra_hosts=("api.fireflies.ai",))
        header = b"CONNECT api.fireflies.ai:443 HTTP/1.1\r\nHost: api.fireflies.ai:443\r\n\r\n"
        self.assertEqual(egress.parse_connect(header, policy), "api.fireflies.ai")
        denied = b"CONNECT evil.example.com:443 HTTP/1.1\r\nHost: evil.example.com:443\r\n\r\n"
        with self.assertRaises(egress.EgressError):
            egress.parse_connect(denied, policy)


class SlotProxyIntegrationHostsTest(unittest.TestCase):
    def test_child_argv_carries_each_validated_host(self):
        argv = slot_proxy.child_argv("claude", "run", "a", 5,
                                     extra_hosts=("api.fireflies.ai", "mcp.aimfox.com"))
        pairs = [argv[i + 1] for i, a in enumerate(argv) if a == "--extra-host"]
        self.assertEqual(pairs, ["api.fireflies.ai", "mcp.aimfox.com"])

    def test_child_argv_without_hosts_is_unchanged(self):
        self.assertEqual(
            slot_proxy.child_argv("claude", "run", "a", 5),
            slot_proxy.child_argv("claude", "run", "a", 5, extra_hosts=()),
        )
        self.assertNotIn("--extra-host", slot_proxy.child_argv("claude", "run", "a", 5))

    def test_scope_validation_refuses_bad_hosts_before_spawning(self):
        with self.assertRaises(slot_proxy.SlotProxyError):
            slot_proxy.validate_scope("claude", "run", "a", extra_hosts=("10.0.0.1",))
        slot_proxy.validate_scope("none", "integrations", "b", extra_hosts=("api.notion.com",))
        with self.assertRaises(slot_proxy.SlotProxyError):
            slot_proxy.validate_scope("none", "integrations", "b")

    def test_context_manager_passes_hosts_to_the_launcher(self):
        import contextlib

        seen = {}

        class Launcher:
            def listener_present(self, slot):
                return False

            def start(self, provider, purpose, slot, extra_hosts=()):
                seen["start"] = (provider, purpose, slot, tuple(extra_hosts))
                return object()

            def wait_ready(self, child, timeout):
                pass

            def stop(self, child, timeout):
                return True

        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            with slot_proxy.slot_proxy(
                "codex", "run", "b", extra_hosts=("api.notion.com",),
                launcher=Launcher(), lock=lambda slot: contextlib.nullcontext(),
                quarantine_dir=directory,
            ) as env:
                self.assertIn("HTTPS_PROXY", env)
        self.assertEqual(seen["start"], ("codex", "run", "b", ("api.notion.com",)))

    def test_main_accepts_extra_hosts_and_refuses_bad_ones(self):
        calls = []
        original = slot_proxy._serve
        slot_proxy._serve = lambda *a, **k: calls.append((a, k)) or 0
        try:
            rc = slot_proxy.main(["--serve", "--provider", "claude", "--purpose", "run",
                                  "--host", "10.203.0.2", "--extra-host", "api.fireflies.ai"])
            self.assertEqual(rc, 0)
            self.assertEqual(calls[-1][1].get("extra_hosts"), ("api.fireflies.ai",))
            rc = slot_proxy.main(["--serve", "--provider", "claude", "--purpose", "run",
                                  "--host", "10.203.0.2", "--extra-host", "127.0.0.1"])
            self.assertEqual(rc, 2)
        finally:
            slot_proxy._serve = original


if __name__ == "__main__":
    unittest.main()
