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
        # chatgpt.com joined the LOGIN list after a live capture showed the pinned CLI
        # calling it mid-device-flow and the proxy denying it (login never completed).
        self.assertEqual(
            endpoints.hosts("codex", "login"), {"auth.openai.com", "chatgpt.com"}
        )
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
            endpoints.authorities("codex", "login"),
            {b"auth.openai.com:443", b"chatgpt.com:443"},
        )
        self.assertIn(b"claude.com:443", endpoints.authorities("claude", "login"))

    def test_codex_login_widening_admits_chatgpt_only_and_stays_exact(self):
        """Adding chatgpt.com must not become a soft/suffix match or a general opening."""
        hosts = endpoints.hosts("codex", "login")
        for host in (
            "api.openai.com",          # a RUN host — not needed to log in
            "api.github.com",          # the CLI's update check, correctly denied live
            "chatgpt.com.evil.invalid",  # suffix-prefix confusion
            "evil.invalid.chatgpt.com",  # subdomain of the allowed host
            "sub.chatgpt.com",
            "www.chatgpt.com",
            "openai.com",
            "example.invalid",
        ):
            with self.subTest(host=host):
                self.assertNotIn(host, hosts)
                with self.assertRaises(endpoints.EndpointError):
                    endpoints.port_for("codex", "login", host)
        # No wildcard/empty authority ever leaks into the byte set, and the login
        # scope stays strictly narrower than the runtime scope.
        for authority in endpoints.authorities("codex", "login"):
            self.assertRegex(authority, rb"^[a-z0-9.-]+:443\Z")
        self.assertLess(hosts, endpoints.hosts("codex", "run"))

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

    def test_acp_providers_are_exact_and_entirely_unconfirmed(self):
        # Nothing for the ACP providers has been probed on a pinned image, so
        # EVERY entry must say so — a reader of `unconfirmed()` sees the whole list.
        for provider in ("cursor", "gemini", "opencode"):
            for purpose in ("login", "run"):
                with self.subTest(p=provider, u=purpose):
                    eps = endpoints.endpoints(provider, purpose)
                    self.assertEqual(endpoints.unconfirmed(provider, purpose), eps)
        self.assertEqual(
            endpoints.hosts("gemini", "login"),
            {"oauth2.googleapis.com", "www.googleapis.com", "cloudcode-pa.googleapis.com"},
        )
        self.assertEqual(endpoints.hosts("opencode", "login"), {"auth.openai.com"})
        self.assertEqual(
            endpoints.hosts("opencode", "run"), {"chatgpt.com", "auth.openai.com"}
        )
        self.assertIn("api2.cursor.sh", endpoints.hosts("cursor", "login"))
        # The user's browser opens the sign-in pages; the sandboxed CLI never does.
        for provider, host in (
            ("gemini", "accounts.google.com"),
            ("cursor", "authenticator.cursor.sh"),
            ("cursor", "accounts.x.ai"),
            # OpenCode's other upstreams stay unreachable: only ChatGPT is offered.
            ("opencode", "opencode.ai"),
            ("opencode", "models.dev"),
            ("opencode", "api.openai.com"),
            # No DNS record (resolved from the pinned image).
            ("cursor", "api5.cursor.sh"),
        ):
            for purpose in ("login", "run"):
                with self.subTest(p=provider, h=host, u=purpose):
                    self.assertNotIn(host, endpoints.hosts(provider, purpose))

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
