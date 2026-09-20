"""Pure command-plan + fail-closed + slot-lock tests for the network bootstrap.

Real ip/nft/sysctl execution is a Linux/root canary step; here the argv plan and
its bypass validation are checked on every platform, plus the per-slot proxy lock."""

import os
import sys
import tempfile
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_agent_network_bootstrap as boot  # noqa: E402
import neev_login_network_profile as profile  # noqa: E402


class PlanTests(unittest.TestCase):
    def setUp(self):
        self.plan = boot.plan("eth0")
        self.commands = boot.setup_commands(self.plan)
        self.flat = [" ".join(part) for part in self.commands]

    def test_creates_all_three_bridges_with_gateways(self):
        for reservation in profile.network_profiles():
            self.assertIn("%s link add %s type bridge" % (boot.IP, reservation.bridge), self.flat)
            prefix = reservation.subnet.split("/")[1]
            self.assertIn(
                "%s addr add %s/%s dev %s" % (boot.IP, reservation.gateway, prefix, reservation.bridge),
                self.flat,
            )

    def test_proxy_namespace_and_endpoints_match_the_profile(self):
        self.assertIn("%s netns add %s" % (boot.IP, boot.PROXY_NETNS), self.flat)
        # The a/b proxy addresses must equal the slot profile's proxy_ip.
        endpoints = {bridge: ip for bridge, ip, _h, _g in boot._PROXY_ENDPOINTS}
        self.assertEqual(endpoints["nv-login-a"], profile.slot_profile("a").proxy_ip)
        self.assertEqual(endpoints["nv-login-b"], profile.slot_profile("b").proxy_ip)
        self.assertIn(
            "%s -n %s route add default via 10.203.0.17" % (boot.IP, boot.PROXY_NETNS), self.flat
        )

    def test_nat_and_forward_are_fail_closed_and_uplink_scoped(self):
        boot.assert_fail_closed(self.commands)
        self.assertTrue(any("policy drop" in line for line in self.flat))
        self.assertTrue(any(
            "ip saddr 10.203.0.16/29 oif eth0 masquerade" in line for line in self.flat
        ))
        # Internal guest bridges are never granted forwarding.
        for line in self.flat:
            if line.rstrip().endswith(" accept"):
                self.assertNotIn("nv-login-a", line)
                self.assertNotIn("nv-login-b", line)

    def test_teardown_reverses_table_namespace_and_bridges(self):
        flat = [" ".join(part) for part in boot.teardown_commands(self.plan)]
        self.assertEqual(flat[0], "%s delete table inet %s" % (boot.NFT, boot._TABLE))
        self.assertIn("%s netns del %s" % (boot.IP, boot.PROXY_NETNS), flat)
        for reservation in profile.network_profiles():
            self.assertIn("%s link del %s" % (boot.IP, reservation.bridge), flat)

    def test_uplink_validation_rejects_metacharacters_and_overlong(self):
        for bad in ("", "eth0; rm -rf /", "eth 0", "ETH0", "x" * 16, "eth0\n", None):
            with self.subTest(bad=bad):
                with self.assertRaises(boot.BootstrapError):
                    boot.plan(bad)

    def test_assert_fail_closed_rejects_an_unscoped_masquerade(self):
        tampered = list(self.commands)
        for index, command in enumerate(tampered):
            if command[-1] == "masquerade":
                tampered[index] = [boot.NFT, "add", "rule", "inet", boot._TABLE,
                                   "postrouting", "oif", "eth0", "masquerade"]
        with self.assertRaises(boot.BootstrapError):
            boot.assert_fail_closed(tampered)


class SlotProxyLockTests(unittest.TestCase):
    def test_exclusive_per_slot_lock_reports_busy_and_releases(self):
        with tempfile.TemporaryDirectory() as directory:
            with boot.slot_proxy_lock("a", lock_dir=directory):
                # A second holder of the SAME slot is refused busy.
                with self.assertRaises(boot.BootstrapError) as caught:
                    with boot.slot_proxy_lock("a", lock_dir=directory):
                        pass
                self.assertEqual(caught.exception.category, "busy")
                # A different slot is independent.
                with boot.slot_proxy_lock("b", lock_dir=directory):
                    pass
            # Released after exit: re-acquire succeeds.
            with boot.slot_proxy_lock("a", lock_dir=directory):
                pass

    def test_lock_rejects_non_slot(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(boot.BootstrapError):
                with boot.slot_proxy_lock("c", lock_dir=directory):
                    pass


if __name__ == "__main__":
    unittest.main()
