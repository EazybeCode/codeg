"""Behavioral tests for Neev's fixed Codex login network profile."""

import dataclasses
import importlib.util
import inspect
from pathlib import Path
import sys
import unittest


MODULE_PATH = (
    Path(__file__).parents[1] / "operations" / "neev_login_network_profile.py"
)


def load_module():
    if not MODULE_PATH.exists():
        return None
    spec = importlib.util.spec_from_file_location(
        "neev_login_network_profile", MODULE_PATH
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("unable to load login network profile module")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


module = load_module()


class LoginNetworkProfileTests(unittest.TestCase):
    def setUp(self):
        if module is None:
            self.fail("login network profile is not implemented")

    def endpoint(self, role, bridge, ipv4, mac, interface):
        return module.Endpoint(role, bridge, ipv4, mac, interface)

    def proxy_endpoints(self):
        return (
            self.endpoint(
                "proxy_a", "nv-login-a", "10.203.0.2", "02:00:00:00:00:01", "veth1a"
            ),
            self.endpoint(
                "proxy_b", "nv-login-b", "10.203.0.10", "02:00:00:00:00:02", "veth2b"
            ),
            self.endpoint(
                "proxy_out",
                "nv-login-out",
                "10.203.0.18",
                "02:00:00:00:00:03",
                "veth3c",
            ),
        )

    def guest_endpoints(self):
        return (
            self.endpoint(
                "guest_a", "nv-login-a", "10.203.0.3", "02:00:00:00:00:04", "veth4d"
            ),
            self.endpoint(
                "guest_b", "nv-login-b", "10.203.0.11", "02:00:00:00:00:05", "veth5e"
            ),
        )

    def assert_invalid(self, operation, secret=None):
        with self.assertRaises(module.ProfileError) as caught:
            operation()
        self.assertEqual(str(caught.exception), "invalid")
        self.assertEqual(repr(caught.exception), "ProfileError('invalid')")
        if secret is not None:
            self.assertNotIn(secret, str(caught.exception))
            self.assertNotIn(secret, repr(caught.exception))

    def test_public_functions_have_only_the_fixed_profile_inputs(self):
        self.assertEqual(
            str(inspect.signature(module.network_profiles)),
            "() -> 'tuple[NetworkProfile, ...]'",
        )
        self.assertEqual(
            str(inspect.signature(module.slot_profile)),
            "(slot: 'str') -> 'SlotProfile'",
        )
        self.assertEqual(
            str(inspect.signature(module.validate_endpoints)),
            "(endpoints: 'tuple[Endpoint, ...]') -> 'tuple[Endpoint, ...]'",
        )

    def test_dataclasses_are_frozen_and_have_exact_string_fields(self):
        expected = {
            module.NetworkProfile: (
                ("name", "str"),
                ("bridge", "str"),
                ("subnet", "str"),
                ("gateway", "str"),
                ("internal", "bool"),
            ),
            module.SlotProfile: (
                ("slot", "str"),
                ("network", "str"),
                ("proxy_ip", "str"),
                ("guest_ip", "str"),
            ),
            module.Endpoint: (
                ("role", "str"),
                ("bridge", "str"),
                ("ipv4", "str"),
                ("mac", "str"),
                ("interface", "str"),
            ),
        }
        for record_type, fields in expected.items():
            with self.subTest(record_type=record_type.__name__):
                self.assertTrue(dataclasses.is_dataclass(record_type))
                self.assertEqual(
                    tuple((field.name, field.type) for field in dataclasses.fields(record_type)),
                    fields,
                )
                self.assertTrue(record_type.__dataclass_params__.frozen)

        profile = module.network_profiles()[0]
        with self.assertRaises(dataclasses.FrozenInstanceError):
            profile.name = "invented"

    def test_networks_and_slots_are_exact_and_stably_ordered(self):
        self.assertEqual(
            module.network_profiles(),
            (
                module.NetworkProfile(
                    "neev-login-a", "nv-login-a", "10.203.0.0/29", "10.203.0.1", True
                ),
                module.NetworkProfile(
                    "neev-login-b", "nv-login-b", "10.203.0.8/29", "10.203.0.9", True
                ),
                module.NetworkProfile(
                    "neev-login-out",
                    "nv-login-out",
                    "10.203.0.16/29",
                    "10.203.0.17",
                    False,
                ),
            ),
        )
        self.assertEqual(
            module.slot_profile("a"),
            module.SlotProfile("a", "neev-login-a", "10.203.0.2", "10.203.0.3"),
        )
        self.assertEqual(
            module.slot_profile("b"),
            module.SlotProfile("b", "neev-login-b", "10.203.0.10", "10.203.0.11"),
        )

    def test_runtime_policy_is_exact_and_immutable(self):
        self.assertEqual(
            module.CODEX_IMAGE,
            "sha256:735d67c77f139634f9903ed731c4d4a0ac03409a3f520b12c588191b3e9406bc",
        )
        self.assertEqual(module.GUEST_RUNTIME, "workos-runsc")
        self.assertEqual(module.GUEST_USER, "65532:65532")
        self.assertIs(module.GUEST_READ_ONLY, True)
        self.assertEqual(module.GUEST_CAP_DROP, ("ALL",))
        self.assertEqual(module.GUEST_SECURITY_OPT, ("no-new-privileges",))
        self.assertEqual(module.GUEST_CPU_COUNT, 1)
        self.assertEqual(module.GUEST_MEMORY_BYTES, 1073741824)
        self.assertEqual(module.GUEST_MEMORY_SWAP_BYTES, 1073741824)
        self.assertEqual(module.GUEST_PIDS_LIMIT, 128)
        self.assertEqual(module.GUEST_CORE_LIMIT, 0)
        self.assertEqual(module.GUEST_LOG_DRIVER, "none")
        self.assertEqual(module.GUEST_RESTART_POLICY, "no")
        self.assertEqual(module.GUEST_SCRATCH_BYTES, 67108864)
        self.assertEqual(module.GUEST_DNS, ("127.0.0.1",))
        self.assertEqual(module.GUEST_DNS_SEARCH, ())
        self.assertEqual(module.GUEST_PUBLISHED_PORTS, ())
        self.assertEqual(module.GUEST_NO_PROXY, "")
        self.assertEqual(
            module.PROXY_ENV_KEYS,
            ("HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"),
        )
        self.assertEqual(
            dict(module.PROXY_URLS),
            {
                "a": "http://10.203.0.2:3128",
                "b": "http://10.203.0.10:3128",
            },
        )
        with self.assertRaises(TypeError):
            module.PROXY_URLS["a"] = "http://invented.invalid"

    def test_slot_selection_rejects_every_non_slot_with_sanitized_error(self):
        secret = "invented-secret-slot"
        error = module.ProfileError(secret)
        self.assertEqual(str(error), "invalid")
        self.assertNotIn(secret, repr(error))
        for value in ("A", "B", "", "c", secret, None, 1, True):
            with self.subTest(value=value):
                self.assert_invalid(
                    lambda value=value: module.slot_profile(value), secret
                )

    def test_validates_proxy_only_or_optional_guests_and_returns_role_order(self):
        proxies = self.proxy_endpoints()
        guests = self.guest_endpoints()
        self.assertEqual(
            module.validate_endpoints((proxies[2], proxies[0], proxies[1])),
            proxies,
        )
        self.assertEqual(
            module.validate_endpoints((guests[1], proxies[2], proxies[1], proxies[0])),
            (guests[1],) + proxies,
        )
        self.assertEqual(
            module.validate_endpoints((proxies[1], guests[0], proxies[2], guests[1], proxies[0])),
            guests + proxies,
        )

    def test_rejects_missing_unknown_duplicate_or_malformed_roles(self):
        proxies = self.proxy_endpoints()
        unknown = self.endpoint(
            "invented", "nv-login-a", "10.203.0.3", "02:00:00:00:00:06", "veth6f"
        )
        malformed = self.endpoint(
            None, "nv-login-a", "10.203.0.3", "02:00:00:00:00:06", "veth6f"
        )
        for endpoints in (
            proxies[:2],
            proxies + (unknown,),
            proxies + (proxies[0],),
            proxies + (malformed,),
            list(proxies),
            None,
        ):
            with self.subTest(endpoints=endpoints):
                self.assert_invalid(lambda endpoints=endpoints: module.validate_endpoints(endpoints))

    def test_rejects_wrong_endpoint_tuple_or_scope_mixing(self):
        proxies = self.proxy_endpoints()
        wrong_values = (
            ("proxy_a", "nv-login-b", "10.203.0.2"),
            ("proxy_a", "nv-login-a", "10.203.0.10"),
            ("proxy_b", "nv-login-out", "10.203.0.10"),
            ("proxy_out", "nv-login-out", "10.203.0.17"),
            ("guest_a", "nv-login-b", "10.203.0.3"),
            ("guest_b", "nv-login-b", "10.203.0.3"),
        )
        for role, bridge, ipv4 in wrong_values:
            endpoint = self.endpoint(
                role, bridge, ipv4, "02:00:00:00:00:06", "veth6f"
            )
            with self.subTest(role=role, bridge=bridge, ipv4=ipv4):
                self.assert_invalid(
                    lambda endpoint=endpoint: module.validate_endpoints(proxies + (endpoint,))
                )

    def test_rejects_duplicate_mac_or_host_interface_across_roles(self):
        proxies = self.proxy_endpoints()
        guest = self.guest_endpoints()[0]
        duplicate_mac = dataclasses.replace(guest, mac=proxies[0].mac)
        duplicate_interface = dataclasses.replace(guest, interface=proxies[0].interface)
        self.assert_invalid(lambda: module.validate_endpoints(proxies + (duplicate_mac,)))
        self.assert_invalid(
            lambda: module.validate_endpoints(proxies + (duplicate_interface,))
        )

    def test_rejects_noncanonical_multicast_zero_or_malformed_mac(self):
        proxies = self.proxy_endpoints()
        guest = self.guest_endpoints()[0]
        bad_macs = (
            "00:00:00:00:00:00",
            "03:00:00:00:00:06",
            "02:00:00:00:00:0A",
            "2:00:00:00:00:06",
            "02-00-00-00-00-06",
            "02:00:00:00:00:gg",
            None,
        )
        for mac in bad_macs:
            with self.subTest(mac=mac):
                endpoint = dataclasses.replace(guest, mac=mac)
                self.assert_invalid(
                    lambda endpoint=endpoint: module.validate_endpoints(proxies + (endpoint,))
                )

    def test_rejects_foreign_or_malformed_host_interfaces(self):
        proxies = self.proxy_endpoints()
        guest = self.guest_endpoints()[0]
        bad_interfaces = (
            "eth0",
            "veth",
            "vethABC",
            "veth123456789abc",
            "veth123456789ab-",
            "veth../secret",
            None,
        )
        for interface in bad_interfaces:
            with self.subTest(interface=interface):
                endpoint = dataclasses.replace(guest, interface=interface)
                self.assert_invalid(
                    lambda endpoint=endpoint: module.validate_endpoints(proxies + (endpoint,))
                )


if __name__ == "__main__":
    unittest.main()
