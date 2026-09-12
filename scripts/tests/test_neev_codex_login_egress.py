"""Behavioral tests for Neev's bounded Codex login egress proxy."""

import asyncio
from contextlib import nullcontext
import gc
import importlib.util
import socket
import sys
from pathlib import Path
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).parents[1] / "operations" / "neev_codex_login_egress.py"
AUTHORITY = b"auth.openai.com:443"
VALID_REQUEST = (
    b"CONNECT " + AUTHORITY + b" HTTP/1.1\r\n"
    b"Host: " + AUTHORITY + b"\r\n\r\n"
)


def load_module():
    if not MODULE_PATH.exists():
        return None
    spec = importlib.util.spec_from_file_location("neev_codex_login_egress", MODULE_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError("unable to load login egress module")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


module = load_module()


class PolicyTests(unittest.TestCase):
    def setUp(self):
        if module is None:
            self.fail("login egress module is not implemented")

    def assert_egress_error(self, expected, operation, secret=None):
        with self.assertRaises(module.EgressError) as caught:
            operation()
        self.assertEqual(caught.exception.code, expected)
        self.assertEqual(str(caught.exception), expected)
        self.assertEqual(repr(caught.exception), "EgressError(%r)" % expected)
        self.assertIsNone(caught.exception.__cause__)
        if secret is not None:
            self.assertNotIn(secret, str(caught.exception))
            self.assertNotIn(secret, repr(caught.exception))

    def test_error_exposes_only_allowlisted_codes(self):
        for value, expected in (
            ("invalid", "invalid"),
            ("denied", "denied"),
            ("unavailable", "unavailable"),
            ("timeout", "timeout"),
            ("overflow", "overflow"),
            ("busy", "busy"),
            ("provider-secret", "invalid"),
            (None, "invalid"),
        ):
            with self.subTest(value=value):
                error = module.EgressError(value)
                self.assertEqual(error.code, expected)
                self.assertEqual(str(error), expected)
                self.assertNotIn("provider-secret", repr(error))

    def test_exact_connect_and_optional_headers(self):
        requests = (
            VALID_REQUEST,
            b"CONNECT auth.openai.com:443 HTTP/1.1\r\n\r\n",
            b"CONNECT auth.openai.com:443 HTTP/1.1\r\n"
            b"host: auth.openai.com:443\r\n"
            b"user-agent: invented-client/1\r\n"
            b"proxy-connection: keep-alive\r\n\r\n",
            b"CONNECT auth.openai.com:443 HTTP/1.1\r\n"
            b"HOST: auth.openai.com:443\r\n"
            b"USER-AGENT: INVENTED\r\n"
            b"PROXY-CONNECTION: Keep-Alive\r\n\r\n",
        )
        for request in requests:
            with self.subTest(request=request):
                self.assertEqual(module.parse_connect(request), "auth.openai.com")

    def test_exact_8192_byte_boundary_is_valid_and_next_byte_overflows(self):
        prefix = b"CONNECT auth.openai.com:443 HTTP/1.1\r\nUser-Agent: "
        suffix = b"\r\n\r\n"
        request = (
            prefix
            + (b"a" * (module.MAX_HEADER_BYTES - len(prefix) - len(suffix)))
            + suffix
        )
        self.assertEqual(len(request), 8192)
        self.assertEqual(module.parse_connect(request), "auth.openai.com")
        self.assert_egress_error("overflow", lambda: module.parse_connect(request + b"a"))

    def test_request_line_rejects_non_connect_and_non_http11(self):
        cases = (
            b"GET auth.openai.com:443 HTTP/1.1\r\n\r\n",
            b"POST auth.openai.com:443 HTTP/1.1\r\n\r\n",
            b"CONNECT auth.openai.com:443 HTTP/1.0\r\n\r\n",
            b"CONNECT auth.openai.com:443 HTTP/2\r\n\r\n",
        )
        for request in cases:
            with self.subTest(request=request):
                self.assert_egress_error(
                    "invalid", lambda request=request: module.parse_connect(request)
                )

    def test_destination_must_be_the_exact_authority(self):
        authorities = (
            b"auth.openai.com:80",
            b"1.1.1.1:443",
            b"auth.openai.com.:443",
            b"user:password@auth.openai.com:443",
            b"evil-auth.openai.com:443",
            b"auth.openai.com.evil.invalid:443",
            b"https://auth.openai.com:443/",
            b"AUTH.OPENAI.COM:443",
        )
        for authority in authorities:
            request = b"CONNECT " + authority + b" HTTP/1.1\r\n\r\n"
            with self.subTest(authority=authority):
                self.assert_egress_error(
                    "denied", lambda request=request: module.parse_connect(request), "password"
                )

    def test_control_non_ascii_and_line_framing_are_invalid(self):
        cases = (
            VALID_REQUEST.replace(b"HTTP/1.1", b"HTTP/1.1\x00"),
            VALID_REQUEST.replace(b"auth.openai.com", b"auth.openai.co\x00"),
            VALID_REQUEST.replace(b"auth.openai.com", b"auth.openai.co\xff"),
            VALID_REQUEST.replace(b"Host:", b"H\xffst:"),
            VALID_REQUEST.replace(b"\r\n", b"\n"),
            VALID_REQUEST.replace(b"Host:", b"\tHost:"),
            VALID_REQUEST[:-2],
            VALID_REQUEST + b"extra",
        )
        for request in cases:
            with self.subTest(request=request):
                self.assert_egress_error(
                    "invalid", lambda request=request: module.parse_connect(request)
                )

    def test_headers_are_allowlisted_unique_and_bodyless(self):
        cases = (
            VALID_REQUEST[:-2] + b"Host: auth.openai.com:443\r\n\r\n",
            VALID_REQUEST[:-2] + b"Content-Length: 0\r\n\r\n",
            VALID_REQUEST[:-2] + b"Transfer-Encoding: chunked\r\n\r\n",
            VALID_REQUEST[:-2] + b"Proxy-Authorization: Basic c2VjcmV0\r\n\r\n",
            VALID_REQUEST[:-2] + b"X-Forwarded-For: 127.0.0.1\r\n\r\n",
            VALID_REQUEST[:-2] + b" folded\r\n\r\n",
            VALID_REQUEST[:-2] + b"Proxy-Connection: close\r\n\r\n",
            VALID_REQUEST.replace(b"auth.openai.com:443\r\n\r\n", b"evil.invalid:443\r\n\r\n"),
        )
        for request in cases:
            with self.subTest(request=request):
                self.assert_egress_error(
                    "invalid", lambda request=request: module.parse_connect(request), "c2VjcmV0"
                )

    def test_header_values_must_be_printable_ascii(self):
        for value in (b"client\x00name", b"client\x7fname", b"client\xffname"):
            request = (
                b"CONNECT auth.openai.com:443 HTTP/1.1\r\nUser-Agent: "
                + value
                + b"\r\n\r\n"
            )
            with self.subTest(value=value):
                self.assert_egress_error(
                    "invalid", lambda request=request: module.parse_connect(request)
                )

    def test_public_ipv4_is_numeric_and_deduplicated(self):
        self.assertEqual(module.validated_ipv4(["1.1.1.1", "1.1.1.1"]), ("1.1.1.1",))
        self.assertEqual(
            module.validated_ipv4(["2606:4700:4700::1111", "8.8.8.8"]),
            ("8.8.8.8",),
        )

    def test_mixed_dns_answer_never_dials_private_address(self):
        self.assert_egress_error(
            "denied", lambda: module.validated_ipv4(["1.1.1.1", "127.0.0.1"])
        )

    def test_non_global_dns_answers_are_denied_as_a_whole(self):
        addresses = (
            "10.0.0.1",
            "172.16.0.1",
            "192.168.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "192.0.2.1",
            "224.0.0.1",
            "ff02::1",
            "0.0.0.0",
            "240.0.0.1",
            "::1",
            "fe80::1",
            "::ffff:1.1.1.1",
        )
        for address in addresses:
            with self.subTest(address=address):
                self.assert_egress_error(
                    "denied",
                    lambda address=address: module.validated_ipv4([address]),
                )

    def test_malformed_alias_noncanonical_and_wrong_types_are_invalid(self):
        collections = (
            ["2130706433"],
            ["1.1.1.01"],
            ["2001:0DB8::1"],
            ["fe80::1%lo0"],
            ["not-an-address"],
            [1],
            "1.1.1.1",
            None,
        )
        for addresses in collections:
            with self.subTest(addresses=addresses):
                self.assert_egress_error(
                    "invalid",
                    lambda addresses=addresses: module.validated_ipv4(addresses),
                )

    def test_dns_collection_bounds_and_ipv4_requirement(self):
        self.assert_egress_error("unavailable", lambda: module.validated_ipv4([]))
        self.assert_egress_error(
            "overflow", lambda: module.validated_ipv4(["1.1.1.1"] * 17)
        )
        self.assert_egress_error(
            "unavailable", lambda: module.validated_ipv4(["2606:4700:4700::1111"])
        )


class MemoryWriter:
    def __init__(self, *, fail_write=False, fail_drain=False, stall_drain=False):
        self.data = bytearray()
        self.closed = False
        self.waited_closed = False
        self.fail_write = fail_write
        self.fail_drain = fail_drain
        self.stall_drain = stall_drain
        self.drain_started = asyncio.Event()

    def write(self, data):
        if self.fail_write:
            raise ConnectionError("invented write detail")
        self.data.extend(data)

    async def drain(self):
        self.drain_started.set()
        if self.fail_drain:
            raise ConnectionError("invented drain detail")
        if self.stall_drain:
            await asyncio.Future()
        await asyncio.sleep(0)

    def close(self):
        self.closed = True

    async def wait_closed(self):
        self.waited_closed = True

    def is_closing(self):
        return self.closed


def reader_with(data=b"", eof=True):
    reader = asyncio.StreamReader()
    if data:
        reader.feed_data(data)
    if eof:
        reader.feed_eof()
    return reader


class LoginEgressProxyTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        if module is None:
            self.fail("login egress module is not implemented")
        self.proxy = module.LoginEgressProxy()

    async def asyncTearDown(self):
        await self.proxy.close()

    async def test_positive_transcript_forwards_real_bytes_both_ways_and_closes(self):
        inbound = reader_with(VALID_REQUEST + b"client-tls")
        inbound_writer = MemoryWriter()
        outbound = reader_with(b"server-tls")
        outbound_writer = MemoryWriter()
        dialed = []

        async def resolve(host):
            self.assertEqual(host, "auth.openai.com")
            return ["1.1.1.1"]

        async def open_connection(host, port, *, family):
            dialed.append((host, port, family))
            return outbound, outbound_writer

        with mock.patch.object(module, "_resolve", resolve), mock.patch.object(
            module, "_open_connection", open_connection
        ):
            await self.proxy.handle(inbound, inbound_writer)

        self.assertEqual(dialed, [("1.1.1.1", 443, socket.AF_INET)])
        self.assertEqual(outbound_writer.data, b"client-tls")
        self.assertEqual(
            inbound_writer.data,
            b"HTTP/1.1 200 Connection Established\r\n\r\nserver-tls",
        )
        self.assertTrue(inbound_writer.closed and inbound_writer.waited_closed)
        self.assertTrue(outbound_writer.closed and outbound_writer.waited_closed)

    async def test_invalid_and_denied_requests_never_resolve_or_dial(self):
        requests = (
            (b"GET / HTTP/1.1\r\n\r\n", b"HTTP/1.1 400 Bad Request"),
            (b"CONNECT evil.invalid:443 HTTP/1.1\r\n\r\n", b"HTTP/1.1 403 Forbidden"),
        )
        for request, status in requests:
            with self.subTest(request=request):
                writer = MemoryWriter()
                resolver = mock.AsyncMock()
                dial = mock.AsyncMock()
                with mock.patch.object(module, "_resolve", resolver), mock.patch.object(
                    module, "_open_connection", dial
                ):
                    await self.proxy.handle(reader_with(request), writer)
                self.assertTrue(bytes(writer.data).startswith(status))
                self.assertEqual(bytes(writer.data).count(b"HTTP/1.1"), 1)
                resolver.assert_not_awaited()
                dial.assert_not_awaited()

    async def test_dns_rebinding_answer_is_denied_before_dial(self):
        writer = MemoryWriter()
        dial = mock.AsyncMock()

        async def resolve(unused_host):
            return ["1.1.1.1", "127.0.0.1"]

        with mock.patch.object(module, "_resolve", resolve), mock.patch.object(
            module, "_open_connection", dial
        ):
            await self.proxy.handle(reader_with(VALID_REQUEST), writer)
        self.assertTrue(bytes(writer.data).startswith(b"HTTP/1.1 403 Forbidden"))
        dial.assert_not_awaited()

    async def test_resolver_and_connect_failures_are_opaque_502(self):
        async def broken_resolve(unused_host):
            raise OSError("invented resolver secret")

        async def good_resolve(unused_host):
            return ["1.1.1.1"]

        async def broken_connect(unused_host, unused_port, *, family):
            del family
            raise OSError("invented connect secret")

        cases = ((broken_resolve, mock.AsyncMock()), (good_resolve, broken_connect))
        for resolver, dial in cases:
            with self.subTest(resolver=resolver.__name__):
                writer = MemoryWriter()
                with mock.patch.object(module, "_resolve", resolver), mock.patch.object(
                    module, "_open_connection", dial
                ):
                    await self.proxy.handle(reader_with(VALID_REQUEST), writer)
                payload = bytes(writer.data)
                self.assertTrue(payload.startswith(b"HTTP/1.1 502 Bad Gateway"))
                self.assertNotIn(b"secret", payload)

    async def test_header_eof_timeout_and_overflow_are_bounded_400_or_504(self):
        cases = (
            (reader_with(VALID_REQUEST[:-4]), {}, b"HTTP/1.1 400 Bad Request"),
            (
                reader_with(b"", eof=False),
                {"HEADER_SECONDS": 0.01},
                b"HTTP/1.1 504 Gateway Timeout",
            ),
            (
                reader_with(b"x" * (module.MAX_HEADER_BYTES + 1)),
                {},
                b"HTTP/1.1 400 Bad Request",
            ),
        )
        for reader, constants, status in cases:
            with self.subTest(status=status):
                writer = MemoryWriter()
                patcher = (
                    mock.patch.multiple(module, **constants)
                    if constants
                    else nullcontext()
                )
                with patcher:
                    await asyncio.wait_for(self.proxy.handle(reader, writer), 0.5)
                self.assertTrue(bytes(writer.data).startswith(status))

    async def test_dns_timeout_keeps_resolution_slot_until_underlying_finishes(self):
        started = asyncio.Event()
        finish = asyncio.Event()

        async def stalled_resolve(unused_host):
            started.set()
            await finish.wait()
            return ["1.1.1.1"]

        first_writer = MemoryWriter()
        second_writer = MemoryWriter()
        with mock.patch.object(module, "_resolve", stalled_resolve), mock.patch.object(
            module, "MAX_CONNECTIONS", 1
        ), mock.patch.object(module, "DNS_SECONDS", 0.01):
            await self.proxy.handle(reader_with(VALID_REQUEST), first_writer)
            self.assertTrue(bytes(first_writer.data).startswith(b"HTTP/1.1 504 Gateway Timeout"))
            await self.proxy.handle(reader_with(VALID_REQUEST), second_writer)
            self.assertTrue(
                bytes(second_writer.data).startswith(
                    b"HTTP/1.1 503 Service Unavailable"
                )
            )
            finish.set()
            await asyncio.sleep(0)
        await started.wait()

    async def test_late_resolver_failure_after_timeout_is_reaped_without_diagnostic(self):
        finish = asyncio.Event()

        async def stalled_resolve(unused_host):
            await finish.wait()
            raise OSError("invented late resolver detail")

        loop = asyncio.get_running_loop()
        diagnostics = []
        previous_handler = loop.get_exception_handler()
        loop.set_exception_handler(lambda unused_loop, context: diagnostics.append(context))
        try:
            writer = MemoryWriter()
            with mock.patch.object(module, "_resolve", stalled_resolve), mock.patch.object(
                module, "DNS_SECONDS", 0.01
            ):
                await self.proxy.handle(reader_with(VALID_REQUEST), writer)
                self.assertTrue(
                    bytes(writer.data).startswith(b"HTTP/1.1 504 Gateway Timeout")
                )
                finish.set()
                await asyncio.sleep(0)
                await asyncio.sleep(0)
                gc.collect()
                await asyncio.sleep(0)
            self.assertEqual(diagnostics, [])
        finally:
            loop.set_exception_handler(previous_handler)

    async def test_connect_timeout_is_504_and_never_sends_200(self):
        async def resolve(unused_host):
            return ["1.1.1.1"]

        async def stalled_connect(unused_host, unused_port, *, family):
            del family
            await asyncio.Future()

        writer = MemoryWriter()
        with mock.patch.object(module, "_resolve", resolve), mock.patch.object(
            module, "_open_connection", stalled_connect
        ), mock.patch.object(module, "CONNECT_SECONDS", 0.01):
            await asyncio.wait_for(self.proxy.handle(reader_with(VALID_REQUEST), writer), 0.5)
        self.assertTrue(bytes(writer.data).startswith(b"HTTP/1.1 504 Gateway Timeout"))
        self.assertNotIn(b"200", writer.data)

    async def test_cancellation_during_dns_connect_and_forwarding_reaps_streams(self):
        for phase in ("dns", "connect", "forward"):
            with self.subTest(phase=phase):
                entered = asyncio.Event()
                finish_dns = asyncio.Event()
                outbound_writer = MemoryWriter()

                async def resolve(unused_host):
                    if phase == "dns":
                        entered.set()
                        await finish_dns.wait()
                    return ["1.1.1.1"]

                async def open_connection(unused_host, unused_port, *, family):
                    del family
                    if phase == "connect":
                        entered.set()
                        await asyncio.Future()
                    if phase == "forward":
                        entered.set()
                    return reader_with(b"", eof=False), outbound_writer

                inbound_writer = MemoryWriter()
                with mock.patch.object(module, "_resolve", resolve), mock.patch.object(
                    module, "_open_connection", open_connection
                ):
                    task = asyncio.create_task(
                        self.proxy.handle(reader_with(VALID_REQUEST, eof=False), inbound_writer)
                    )
                    await asyncio.wait_for(entered.wait(), 0.5)
                    task.cancel()
                    with self.assertRaises(asyncio.CancelledError):
                        await task
                    finish_dns.set()
                    await asyncio.sleep(0)
                self.assertTrue(inbound_writer.closed and inbound_writer.waited_closed)
                if phase == "forward":
                    self.assertTrue(outbound_writer.closed and outbound_writer.waited_closed)

    async def test_tunnel_write_or_drain_failure_closes_without_second_response(self):
        async def resolve(unused_host):
            return ["1.1.1.1"]

        for failing_writer in (
            MemoryWriter(fail_write=True),
            MemoryWriter(fail_drain=True),
        ):
            with self.subTest(failing_writer=failing_writer):
                inbound_writer = MemoryWriter()

                async def open_connection(unused_host, unused_port, *, family):
                    del family
                    return reader_with(b"server-data"), failing_writer

                with mock.patch.object(module, "_resolve", resolve), mock.patch.object(
                    module, "_open_connection", open_connection
                ):
                    await self.proxy.handle(
                        reader_with(VALID_REQUEST + b"client-data"), inbound_writer
                    )
                self.assertEqual(bytes(inbound_writer.data).count(b"HTTP/1.1"), 1)
                self.assertTrue(bytes(inbound_writer.data).startswith(b"HTTP/1.1 200"))
                self.assertTrue(failing_writer.closed and failing_writer.waited_closed)

    async def test_200_drain_failure_never_appends_an_http_error_to_tls(self):
        async def resolve(unused_host):
            return ["1.1.1.1"]

        outbound_writer = MemoryWriter()

        async def open_connection(unused_host, unused_port, *, family):
            del family
            return reader_with(b"", eof=False), outbound_writer

        inbound_writer = MemoryWriter(fail_drain=True)
        with mock.patch.object(module, "_resolve", resolve), mock.patch.object(
            module, "_open_connection", open_connection
        ):
            await self.proxy.handle(reader_with(VALID_REQUEST, eof=False), inbound_writer)
        self.assertEqual(
            bytes(inbound_writer.data),
            b"HTTP/1.1 200 Connection Established\r\n\r\n",
        )
        self.assertTrue(inbound_writer.closed and outbound_writer.closed)

    async def test_idle_lifetime_and_byte_ceilings_close_established_tunnel(self):
        async def resolve(unused_host):
            return ["1.1.1.1"]

        cases = (
            ({"IDLE_SECONDS": 0.01}, reader_with(VALID_REQUEST, eof=False)),
            (
                {"IDLE_SECONDS": 5.0, "LIFETIME_SECONDS": 0.01},
                reader_with(VALID_REQUEST, eof=False),
            ),
            ({"MAX_TUNNEL_BYTES_PER_DIRECTION": 4}, reader_with(b"12345")),
        )
        for constants, inbound in cases:
            with self.subTest(constants=constants):
                inbound_writer = MemoryWriter()
                outbound_writer = MemoryWriter()

                async def open_connection(unused_host, unused_port, *, family):
                    del family
                    return reader_with(b"", eof=False), outbound_writer

                with mock.patch.object(module, "_resolve", resolve), mock.patch.object(
                    module, "_open_connection", open_connection
                ), mock.patch.multiple(module, **constants):
                    await asyncio.wait_for(
                        self.proxy.handle(
                            inbound
                            if constants.get("MAX_TUNNEL_BYTES_PER_DIRECTION") is None
                            else reader_with(VALID_REQUEST + b"12345"),
                            inbound_writer,
                        ),
                        0.5,
                    )
                self.assertTrue(bytes(inbound_writer.data).startswith(b"HTTP/1.1 200"))
                self.assertEqual(bytes(inbound_writer.data).count(b"HTTP/1.1"), 1)
                self.assertTrue(outbound_writer.closed)

    async def test_busy_admission_is_immediate_and_close_reaps_active_handler(self):
        first_reader = reader_with(b"", eof=False)
        first_writer = MemoryWriter()
        second_writer = MemoryWriter()
        with mock.patch.object(module, "MAX_CONNECTIONS", 1):
            first = asyncio.create_task(self.proxy.handle(first_reader, first_writer))
            await asyncio.sleep(0)
            await asyncio.wait_for(
                self.proxy.handle(reader_with(VALID_REQUEST), second_writer), 0.2
            )
            self.assertTrue(
                bytes(second_writer.data).startswith(
                    b"HTTP/1.1 503 Service Unavailable"
                )
            )
            await asyncio.wait_for(self.proxy.close(), 0.5)
            with self.assertRaises(asyncio.CancelledError):
                await first
        self.assertTrue(first_writer.closed and first_writer.waited_closed)

    async def test_real_loopback_handler_reply_shutdown_and_slot_release(self):
        with mock.patch.object(module, "MAX_CONNECTIONS", 1):
            server = await asyncio.start_server(
                self.proxy.handle, "127.0.0.1", 0, limit=module.MAX_HEADER_BYTES
            )
            try:
                port = server.sockets[0].getsockname()[1]
                replies = []
                for request in (
                    b"GET / HTTP/1.1\r\n\r\n",
                    b"CONNECT evil.invalid:443 HTTP/1.1\r\n\r\n",
                ):
                    reader, writer = await asyncio.open_connection("127.0.0.1", port)
                    writer.write(request)
                    await writer.drain()
                    replies.append(await asyncio.wait_for(reader.read(), 0.5))
                    writer.close()
                    await writer.wait_closed()
                self.assertTrue(replies[0].startswith(b"HTTP/1.1 400 Bad Request"))
                self.assertTrue(replies[1].startswith(b"HTTP/1.1 403 Forbidden"))
                self.assertNotIn(b"503", replies[1])
            finally:
                server.close()
                await server.wait_closed()
                await asyncio.wait_for(self.proxy.close(), 0.5)


if __name__ == "__main__":
    unittest.main()
