"""Tests for the runtime multi-host allowlist CONNECT proxy.

Pure parse/policy is verified directly; the handler is driven with in-memory
streams (no real sockets) to prove an allowlisted authority is tunneled, a
non-allowlisted authority is refused 403, and a resolved private address is denied
before any dial. The real TLS reachability is a same-host acceptance."""

import asyncio
import socket
import sys
import unittest
from pathlib import Path
from unittest import mock

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_provider_egress as egress  # noqa: E402
import neev_codex_login_egress as login  # noqa: E402


def connect(authority):
    return (
        b"CONNECT " + authority + b" HTTP/1.1\r\n"
        b"Host: " + authority + b"\r\n\r\n"
    )


class _AbortTransport:
    def __init__(self, writer):
        self.writer = writer

    def abort(self):
        self.writer.closed = True
        self.writer.terminal.set()


class MemoryWriter:
    def __init__(self):
        self.data = bytearray()
        self.closed = False
        self.terminal = asyncio.Event()
        self.transport = _AbortTransport(self)

    def write(self, data):
        self.data.extend(data)

    async def drain(self):
        await asyncio.sleep(0)

    def close(self):
        self.closed = True
        self.terminal.set()

    async def wait_closed(self):
        await self.terminal.wait()


def reader_with(data=b"", eof=True):
    reader = asyncio.StreamReader()
    if data:
        reader.feed_data(data)
    if eof:
        reader.feed_eof()
    return reader


class PolicyAndParseTests(unittest.TestCase):
    def setUp(self):
        self.policy = egress.policy_for("codex", "run")

    def test_policy_covers_exactly_the_endpoint_allowlist(self):
        self.assertEqual(self.policy.hosts(), {"chatgpt.com", "api.openai.com", "auth.openai.com"})
        self.assertIn(b"chatgpt.com:443", self.policy.authorities)
        self.assertEqual(self.policy.port_for("chatgpt.com"), 443)
        with self.assertRaises(egress.EgressError):
            self.policy.port_for("nope.example.com")

    def test_resolver_code_bakes_the_host_literal_without_argv(self):
        code = self.policy.resolver_code["chatgpt.com"]
        self.assertIn("'chatgpt.com'", code)
        self.assertNotIn("sys.argv", code)

    def test_parse_connect_accepts_any_allowlisted_authority(self):
        self.assertEqual(egress.parse_connect(connect(b"chatgpt.com:443"), self.policy), "chatgpt.com")
        self.assertEqual(egress.parse_connect(connect(b"api.openai.com:443"), self.policy), "api.openai.com")

    def test_parse_connect_denies_non_allowlisted_or_wrong_port(self):
        for authority in (b"api.anthropic.com:443", b"evil.invalid:443", b"chatgpt.com:80"):
            with self.subTest(authority=authority):
                with self.assertRaises(egress.EgressError) as caught:
                    egress.parse_connect(connect(authority), self.policy)
                self.assertEqual(caught.exception.code, "denied")

    def test_parse_connect_rejects_mismatched_host_header(self):
        request = (
            b"CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: api.openai.com:443\r\n\r\n"
        )
        with self.assertRaises(egress.EgressError) as caught:
            egress.parse_connect(request, self.policy)
        self.assertEqual(caught.exception.code, "invalid")

    def test_parse_connect_accepts_proxy_connection_close(self):
        # The EXACT shape the pinned Claude CLI (Bun) sends during login — captured:
        # CONNECT platform.claude.com:443 HTTP/1.1 / Host: platform.claude.com:443 /
        # Proxy-Connection: close. Must be accepted (rejecting `close` broke login).
        policy = egress.policy_for("claude", "login")
        request = (
            b"CONNECT platform.claude.com:443 HTTP/1.1\r\n"
            b"Host: platform.claude.com:443\r\n"
            b"Proxy-Connection: close\r\n\r\n"
        )
        self.assertEqual(egress.parse_connect(request, policy), "platform.claude.com")
        # case-insensitive keep-alive/close both accepted
        for value in (b"keep-alive", b"Keep-Alive", b"close", b"Close", b"CLOSE"):
            ok = (b"CONNECT platform.claude.com:443 HTTP/1.1\r\n"
                  b"Host: platform.claude.com:443\r\nProxy-Connection: " + value + b"\r\n\r\n")
            self.assertEqual(egress.parse_connect(ok, policy), "platform.claude.com")
        # any OTHER value is still rejected
        bad = (b"CONNECT platform.claude.com:443 HTTP/1.1\r\n"
               b"Host: platform.claude.com:443\r\nProxy-Connection: te\r\n\r\n")
        with self.assertRaises(egress.EgressError) as caught:
            egress.parse_connect(bad, policy)
        self.assertEqual(caught.exception.code, "invalid")

    def test_connect_classifier_is_fixed_vocab_and_leaks_no_values(self):
        header = (
            b"CONNECT platform.claude.com:443 HTTP/1.1\r\n"
            b"Host: platform.claude.com:443\r\n"
            b"Proxy-Connection: close\r\n"
            b"Proxy-Authorization: Basic c2VjcmV0LXRva2Vu\r\n\r\n"
        )
        summary = egress.classify_connect_request(header)
        self.assertIn("method=CONNECT", summary)
        self.assertIn("version=http11", summary)
        self.assertIn("authority=platform.claude.com:443(known)", summary)
        self.assertIn("host_matches=True", summary)
        self.assertIn("proxy_connection=close", summary)
        self.assertIn("connection_present=False", summary)
        self.assertIn("Proxy-Authorization", summary)   # the NAME is fine to show
        self.assertNotIn("c2VjcmV0", summary)           # the VALUE is never shown

    def test_connect_classifier_flags_connection_header_and_unknown_host(self):
        header = (
            b"CONNECT sneaky.example.net:443 HTTP/2\r\n"
            b"Connection: keep-alive\r\n\r\n"
        )
        summary = egress.classify_connect_request(header)
        self.assertIn("version=http2", summary)
        self.assertIn("connection_present=True", summary)
        self.assertIn("(other)", summary)               # unknown host classed, still shown host:port
        self.assertEqual(egress.classify_connect_request(b""), "shape=no_header")
        self.assertIn("malformed", egress.classify_connect_request(
            b"CONNECT not an authority HTTP/1.1\r\n\r\n"))

    def test_unknown_scope_policy_is_denied(self):
        with self.assertRaises(egress.EgressError):
            egress.policy_for("bogus", "run")


class HandlerTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.policy = egress.policy_for("claude", "run")  # api.anthropic.com
        self.proxy = egress.ProviderEgressProxy(self.policy)

    async def asyncTearDown(self):
        await self.proxy.close()

    async def test_allowlisted_authority_tunnels_bytes_both_ways(self):
        inbound = reader_with(connect(b"api.anthropic.com:443") + b"client-tls")
        inbound_writer = MemoryWriter()
        outbound = reader_with(b"server-tls")
        outbound_writer = MemoryWriter()
        dialed = []

        async def resolve(host, policy):
            self.assertEqual(host, "api.anthropic.com")
            return ["1.1.1.1"]

        async def open_connection(address, port, *, family):
            dialed.append((address, port, family))
            return outbound, outbound_writer

        with mock.patch.object(egress, "_resolve", resolve), mock.patch.object(
            login, "_open_connection", open_connection
        ):
            await self.proxy.handle(inbound, inbound_writer)

        self.assertEqual(dialed, [("1.1.1.1", 443, socket.AF_INET)])
        self.assertEqual(outbound_writer.data, b"client-tls")
        self.assertEqual(
            inbound_writer.data,
            b"HTTP/1.1 200 Connection Established\r\n\r\nserver-tls",
        )

    async def test_non_allowlisted_authority_is_403_without_resolve_or_dial(self):
        writer = MemoryWriter()
        resolver = mock.AsyncMock()
        dial = mock.AsyncMock()
        with mock.patch.object(egress, "_resolve", resolver), mock.patch.object(
            login, "_open_connection", dial
        ):
            await self.proxy.handle(reader_with(connect(b"evil.invalid:443")), writer)
        self.assertTrue(bytes(writer.data).startswith(b"HTTP/1.1 403 Forbidden"))
        resolver.assert_not_awaited()
        dial.assert_not_awaited()

    async def test_private_resolved_address_is_denied_before_dial(self):
        writer = MemoryWriter()
        dial = mock.AsyncMock()

        async def resolve(host, policy):
            return ["1.1.1.1", "127.0.0.1"]

        with mock.patch.object(egress, "_resolve", resolve), mock.patch.object(
            login, "_open_connection", dial
        ):
            await self.proxy.handle(reader_with(connect(b"api.anthropic.com:443")), writer)
        self.assertTrue(bytes(writer.data).startswith(b"HTTP/1.1 403 Forbidden"))
        dial.assert_not_awaited()

    async def test_resolver_or_connect_failure_is_opaque_502(self):
        async def resolve(host, policy):
            return ["1.1.1.1"]

        async def broken_connect(address, port, *, family):
            raise OSError("invented connect secret")

        writer = MemoryWriter()
        with mock.patch.object(egress, "_resolve", resolve), mock.patch.object(
            login, "_open_connection", broken_connect
        ):
            await self.proxy.handle(reader_with(connect(b"api.anthropic.com:443")), writer)
        payload = bytes(writer.data)
        self.assertTrue(payload.startswith(b"HTTP/1.1 502 Bad Gateway"))
        self.assertNotIn(b"secret", payload)

    async def test_admission_ceiling_refuses_when_saturated(self):
        with mock.patch.object(egress, "MAX_CONNECTIONS", 1):
            first = asyncio.create_task(
                self.proxy.handle(reader_with(b"", eof=False), MemoryWriter())
            )
            await asyncio.sleep(0)
            rejected = MemoryWriter()
            await self.proxy.handle(reader_with(connect(b"api.anthropic.com:443")), rejected)
            self.assertTrue(bytes(rejected.data).startswith(b"HTTP/1.1 503 Service Unavailable"))
            await self.proxy.close()
            await asyncio.gather(first, return_exceptions=True)


if __name__ == "__main__":
    unittest.main()
