"""Tests for the per-provider CONNECT-proxy destination allowlists."""

import sys
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_provider_endpoints as endpoints  # noqa: E402


class EndpointTableTests(unittest.TestCase):
    def test_every_provider_purpose_has_a_nonempty_allowlist(self):
        for provider in endpoints.PROVIDERS:
            for purpose in endpoints.PURPOSES:
                eps = endpoints.endpoints(provider, purpose)
                self.assertTrue(eps)
                for endpoint in eps:
                    self.assertEqual(endpoint.port, 443)

    def test_observed_login_hosts(self):
        self.assertEqual(endpoints.hosts("codex", "login"), {"auth.openai.com"})
        claude_login = endpoints.hosts("claude", "login")
        # Observed authorize origin + officially-required auth/token hosts, plus
        # api.anthropic.com which the pinned CLI fetches its role/profile from to COMPLETE
        # login (/api/oauth/claude_cli/roles) after the code exchange.
        self.assertEqual(
            claude_login,
            {"claude.com", "claude.ai", "console.anthropic.com",
             "platform.claude.com", "api.anthropic.com"},
        )
        # platform.claude.com is required for token refresh at runtime too.
        self.assertIn("platform.claude.com", endpoints.hosts("claude", "run"))
        self.assertIn("api.anthropic.com", endpoints.hosts("claude", "run"))

    def test_authorities_are_exact_host_port_bytes(self):
        self.assertEqual(
            endpoints.authorities("codex", "login"), {b"auth.openai.com:443"}
        )
        self.assertIn(b"claude.com:443", endpoints.authorities("claude", "login"))

    def test_port_lookup_and_unknown_host(self):
        self.assertEqual(endpoints.port_for("claude", "run", "api.anthropic.com"), 443)
        with self.assertRaises(endpoints.EndpointError):
            endpoints.port_for("claude", "run", "nope.example.com")

    def test_unknown_scope_is_rejected(self):
        for provider, purpose in (("bogus", "login"), ("claude", "bogus")):
            with self.assertRaises(endpoints.EndpointError):
                endpoints.endpoints(provider, purpose)

    def test_confirmed_flags_match_probe_status(self):
        # Claude login/run + codex login are doc/probe CONFIRMED; only codex runtime
        # hosts remain pending an isolated probe.
        self.assertEqual(endpoints.unconfirmed("claude", "login"), ())
        self.assertEqual(endpoints.unconfirmed("claude", "run"), ())
        self.assertEqual(endpoints.unconfirmed("codex", "login"), ())
        self.assertTrue(endpoints.unconfirmed("codex", "run"))

    def test_endpoint_rejects_malformed_host_or_port(self):
        for host, port in (
            ("no-dot", 443),
            ("Bad.UPPER.com", 443),
            ("under_score.com", 443),
            ("*.wild.com", 443),
            ("trailing.dot.", 443),
            ("ok.com", 0),
            ("ok.com", 70000),
            ("ok.com", True),
        ):
            with self.subTest(host=host, port=port):
                with self.assertRaises(endpoints.EndpointError):
                    endpoints.Endpoint(host, port, False)


if __name__ == "__main__":
    unittest.main()
