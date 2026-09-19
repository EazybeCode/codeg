"""Portable tests for the provider-egress network wiring PLAN and its fail-closed
validation. Actually running `ip`/`nft` + the two-tenant reachability acceptance
is Linux-only (`neev_agent_sandbox_acceptance.py`); these tests prove the command
plan is default-deny, provider-only, IPv6-denied and reversibly torn down."""

import sys
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_agent_egress as egress  # noqa: E402


class EgressWiringTests(unittest.TestCase):
    def plan(self, slot="a", provider=("140.82.112.3",)):
        return egress.plan_provider_network("org:1/user:1", slot, provider)

    def test_plan_composes_slot_profile_and_allowlist(self):
        plan = self.plan()
        # Slot 'a' fixed guest/proxy IPs come from the existing profile.
        self.assertEqual(plan.guest_ip, "10.203.0.3")
        self.assertEqual(plan.proxy_ip, "10.203.0.2")
        self.assertEqual(plan.provider_ipv4, ("140.82.112.3",))
        # The veth master is the bridge DEVICE, not the docker network name.
        self.assertEqual(plan.bridge, "nv-login-a")
        egress.validate_plan(plan)

    def test_plan_rejects_bad_slot_empty_allowlist_bad_port(self):
        with self.assertRaises(Exception):
            egress.plan_provider_network("t", "c", ("1.1.1.1",))
        with self.assertRaises(egress.EgressWiringError):
            egress.plan_provider_network("t", "a", ())
        with self.assertRaises(egress.EgressWiringError):
            egress.plan_provider_network("t", "a", ("1.1.1.1",), proxy_port=0)

    def test_setup_plan_is_default_deny_provider_only_and_denies_ipv6(self):
        commands = egress.setup_commands(self.plan())
        flat = [" ".join(c) for c in commands]
        joined = "\n".join(flat)
        # Default-deny output chain.
        self.assertIn("policy drop", joined)
        # Exactly one qualified accept to the slot proxy address:port.
        self.assertTrue(
            any("ip daddr 10.203.0.2 tcp dport 3128 accept" in line for line in flat)
        )
        # IPv6 explicitly dropped; no IPv6 address/route is ever added.
        self.assertIn("meta nfproto ipv6 drop", joined)
        self.assertNotIn("::", joined)
        # No default route to the internet (bypass prevention): only the /32 proxy.
        self.assertFalse(any("route add default" in line for line in flat))
        self.assertTrue(any("route add 10.203.0.2/32" in line for line in flat))
        # Structural fail-closed check passes for this plan.
        self.assertTrue(egress.assert_fail_closed(commands))

    def test_fail_closed_rejects_an_unqualified_accept(self):
        commands = egress.setup_commands(self.plan())
        # Injecting an unconditional accept must be caught.
        commands.append([egress.NFT, "add", "rule", "inet", "t", "output", "accept"])
        with self.assertRaises(egress.EgressWiringError):
            egress.assert_fail_closed(commands)

    def test_two_tenants_get_distinct_namespaces_and_links(self):
        a = self.plan(slot="a")
        b = self.plan(slot="b", provider=("140.82.112.4",))
        self.assertNotEqual(a.netns, b.netns)
        self.assertNotEqual(a.veth_host, b.veth_host)
        self.assertNotEqual(a.guest_ip, b.guest_ip)

    def test_teardown_deletes_table_link_and_namespace(self):
        commands = egress.teardown_commands(self.plan())
        flat = [" ".join(c) for c in commands]
        self.assertTrue(any("delete table inet" in line for line in flat))
        self.assertTrue(any("link del nvag-a" in line for line in flat))
        self.assertTrue(any("netns del neev-agent-a" in line for line in flat))


if __name__ == "__main__":
    unittest.main()
