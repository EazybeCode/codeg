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
    def __init__(
        self,
        *,
        fail_write=False,
        fail_drain=False,
        stall_drain=False,
        stall_wait_closed=False,
    ):
        self.data = bytearray()
        self.closed = False
        self.waited_closed = False
        self.fail_write = fail_write
        self.fail_drain = fail_drain
        self.stall_drain = stall_drain
        self.stall_wait_closed = stall_wait_closed
        self.drain_started = asyncio.Event()
        self.wait_closed_started = asyncio.Event()

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
        self.wait_closed_started.set()
        if self.stall_wait_closed:
            await asyncio.Future()
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


class FakeStdout:
    def __init__(self, data=b"", stalled=False):
        self.data = bytearray(data)
        self.stalled = stalled
        self.read_started = asyncio.Event()

    async def read(self, maximum):
        self.read_started.set()
        if self.stalled:
            await asyncio.Future()
        if not self.data:
            return b""
        chunk = bytes(self.data[:maximum])
        del self.data[:maximum]
        await asyncio.sleep(0)
        return chunk


class FakeProcess:
    def __init__(
        self,
        output=b"",
        *,
        exit_code=0,
        stalled=False,
        terminate_completes=True,
        kill_completes=True,
    ):
        self.stdout = FakeStdout(output, stalled=stalled)
        self.returncode = None if stalled else exit_code
        self.exit_code = exit_code
        self.terminate_completes = terminate_completes
        self.kill_completes = kill_completes
        self.terminate_calls = 0
        self.kill_calls = 0
        self.wait_calls = 0
        self.reaped = False
        self.terminal = asyncio.Event()
        if self.returncode is not None:
            self.terminal.set()

    def terminate(self):
        self.terminate_calls += 1
        if self.terminate_completes:
            self.returncode = -15
            self.terminal.set()

    def kill(self):
        self.kill_calls += 1
        if self.kill_completes:
            self.returncode = -9
            self.terminal.set()

    async def wait(self):
        self.wait_calls += 1
        await self.terminal.wait()
        self.reaped = True
        return self.returncode


class AbortTransport:
    def __init__(self, writer):
        self.writer = writer
        self.abort_calls = 0

    def abort(self):
        self.abort_calls += 1
        self.writer.aborted = True
        self.writer.closed = True
        self.writer.terminal.set()


class BufferedWriter(MemoryWriter):
    def __init__(self):
        super().__init__()
        self.close_requested = False
        self.aborted = False
        self.terminal = asyncio.Event()
        self.transport = AbortTransport(self)

    def close(self):
        self.close_requested = True

    async def wait_closed(self):
        self.wait_closed_started.set()
        await self.terminal.wait()
        self.closed = True
        self.waited_closed = True


class ResolverProcessTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        if module is None:
            self.fail("login egress module is not implemented")
        if not hasattr(module, "_launch_resolver"):
            self.fail("terminable resolver helper is not implemented")

    async def test_launch_is_fixed_isolated_and_has_no_inherited_environment(self):
        process = object()
        launch = mock.AsyncMock(return_value=process)
        with mock.patch.object(asyncio, "create_subprocess_exec", launch):
            self.assertIs(await module._launch_resolver(), process)

        args = launch.await_args.args
        options = launch.await_args.kwargs
        self.assertEqual(args[:3], (sys.executable, "-I", "-c"))
        self.assertEqual(len(args), 4)
        self.assertIn("auth.openai.com", args[3])
        self.assertNotIn("sys.argv", args[3])
        self.assertEqual(options["stdin"], asyncio.subprocess.DEVNULL)
        self.assertEqual(options["stdout"], asyncio.subprocess.PIPE)
        self.assertEqual(options["stderr"], asyncio.subprocess.DEVNULL)
        self.assertEqual(options["env"], {"LANG": "C", "LC_ALL": "C"})
        self.assertIs(options["close_fds"], True)

    async def test_resolver_accepts_bounded_json_and_sanitizes_child_failures(self):
        cases = (
            (b'["1.1.1.1","2606:4700:4700::1111"]', 0, None),
            (b"not-json", 0, "unavailable"),
            (b"[1]", 0, "unavailable"),
            (b'["1.1.1.1"]', 7, "unavailable"),
            (
                b"[" + b",".join([b'\"1.1.1.1\"'] * 18) + b"]",
                0,
                "overflow",
            ),
            (b"x" * 2049, 0, "overflow"),
        )
        for output, exit_code, expected_error in cases:
            with self.subTest(expected_error=expected_error, exit_code=exit_code):
                process = FakeProcess(output, exit_code=exit_code)
                with mock.patch.object(
                    module, "_launch_resolver", mock.AsyncMock(return_value=process)
                ):
                    if expected_error is None:
                        self.assertEqual(
                            await module._resolve("auth.openai.com"),
                            ["1.1.1.1", "2606:4700:4700::1111"],
                        )
                    else:
                        with self.assertRaises(module.EgressError) as caught:
                            await module._resolve("auth.openai.com")
                        self.assertEqual(caught.exception.code, expected_error)
                self.assertTrue(process.reaped)

    async def test_resolver_refuses_nonfixed_host_before_launch(self):
        launch = mock.AsyncMock()
        with mock.patch.object(module, "_launch_resolver", launch):
            with self.assertRaises(module.EgressError) as caught:
                await module._resolve("evil.invalid")
        self.assertEqual(caught.exception.code, "denied")
        launch.assert_not_awaited()

    async def test_real_owned_local_child_is_killed_and_reaped_on_cancellation(self):
        launched = asyncio.Event()
        processes = []

        async def launch_sleeping_child():
            process = await asyncio.create_subprocess_exec(
                sys.executable,
                "-I",
                "-c",
                "import time; time.sleep(60)",
                stdin=asyncio.subprocess.DEVNULL,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.DEVNULL,
                env={"LANG": "C", "LC_ALL": "C"},
                close_fds=True,
            )
            processes.append(process)
            launched.set()
            return process

        try:
            with mock.patch.object(
                module, "_launch_resolver", launch_sleeping_child
            ), mock.patch.object(module, "RESOLVER_CLEANUP_SECONDS", 0.2):
                task = asyncio.create_task(module._resolve("auth.openai.com"))
                await asyncio.wait_for(launched.wait(), 1.0)
                task.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await asyncio.wait_for(task, 1.0)
        finally:
            for process in processes:
                if process.returncode is None:
                    process.kill()
                    await process.wait()

        self.assertEqual(len(processes), 1)
        self.assertIsNotNone(processes[0].returncode)
        self.assertEqual(await processes[0].wait(), processes[0].returncode)


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

    async def test_dns_timeout_terminates_and_reaps_helper_before_slot_release(self):
        if not hasattr(module, "_launch_resolver"):
            self.fail("terminable resolver helper is not implemented")
        stalled = FakeProcess(
            stalled=True, terminate_completes=False, kill_completes=True
        )
        successor = FakeProcess(b'["1.1.1.1"]')
        processes = iter((stalled, successor))

        async def launch():
            return next(processes)

        async def failed_dial(unused_host, unused_port, *, family):
            del family
            raise OSError("invented dial failure")

        first_writer = MemoryWriter()
        second_writer = MemoryWriter()
        with mock.patch.object(module, "_launch_resolver", launch), mock.patch.object(
            module, "_open_connection", failed_dial
        ), mock.patch.multiple(
            module,
            MAX_CONNECTIONS=1,
            DNS_SECONDS=0.01,
            RESOLVER_CLEANUP_SECONDS=0.01,
        ):
            await self.proxy.handle(reader_with(VALID_REQUEST), first_writer)
            self.assertTrue(
                bytes(first_writer.data).startswith(b"HTTP/1.1 504 Gateway Timeout")
            )
            self.assertEqual(stalled.terminate_calls, 1)
            self.assertEqual(stalled.kill_calls, 1)
            self.assertTrue(stalled.reaped)

            await self.proxy.handle(reader_with(VALID_REQUEST), second_writer)
            self.assertTrue(
                bytes(second_writer.data).startswith(b"HTTP/1.1 502 Bad Gateway")
            )
            self.assertNotIn(b"503", second_writer.data)

    async def test_shutdown_during_dns_force_kills_and_reaps_owned_helper(self):
        if not hasattr(module, "_launch_resolver"):
            self.fail("terminable resolver helper is not implemented")
        process = FakeProcess(
            stalled=True, terminate_completes=False, kill_completes=True
        )
        writer = MemoryWriter()
        with mock.patch.object(
            module, "_launch_resolver", mock.AsyncMock(return_value=process)
        ), mock.patch.multiple(
            module, DNS_SECONDS=5.0, RESOLVER_CLEANUP_SECONDS=0.01
        ):
            handler = asyncio.create_task(
                self.proxy.handle(reader_with(VALID_REQUEST, eof=False), writer)
            )
            await asyncio.wait_for(process.stdout.read_started.wait(), 0.5)
            await asyncio.wait_for(self.proxy.close(), 0.5)
            await asyncio.gather(handler, return_exceptions=True)
        self.assertEqual(process.terminate_calls, 1)
        self.assertEqual(process.kill_calls, 1)
        self.assertTrue(process.reaped)
        self.assertTrue(writer.closed and writer.waited_closed)

    async def test_unconfirmed_resolver_cleanup_fails_close_and_retains_ownership(self):
        if not hasattr(module, "_launch_resolver"):
            self.fail("terminable resolver helper is not implemented")
        process = FakeProcess(
            stalled=True, terminate_completes=False, kill_completes=False
        )
        writer = MemoryWriter()
        with mock.patch.object(
            module, "_launch_resolver", mock.AsyncMock(return_value=process)
        ), mock.patch.multiple(
            module, DNS_SECONDS=5.0, RESOLVER_CLEANUP_SECONDS=0.01
        ):
            handler = asyncio.create_task(
                self.proxy.handle(reader_with(VALID_REQUEST, eof=False), writer)
            )
            await asyncio.wait_for(process.stdout.read_started.wait(), 0.5)
            with self.assertRaises(module.EgressError) as caught:
                await asyncio.wait_for(self.proxy.close(), 0.5)
            self.assertEqual(caught.exception.code, "unavailable")
            self.assertFalse(process.reaped)
            self.assertTrue(self.proxy._resolvers)
            await asyncio.gather(handler, return_exceptions=True)

            process.kill_completes = True
            await asyncio.wait_for(self.proxy.close(), 0.5)
        self.assertTrue(process.reaped)
        self.assertFalse(self.proxy._resolvers)

    async def test_repeated_failed_close_reuses_retained_resolver_waiter(self):
        process = FakeProcess(
            stalled=True, terminate_completes=False, kill_completes=False
        )
        writer = MemoryWriter()
        with mock.patch.object(
            module, "_launch_resolver", mock.AsyncMock(return_value=process)
        ), mock.patch.multiple(
            module, DNS_SECONDS=5.0, RESOLVER_CLEANUP_SECONDS=0.01
        ):
            handler = asyncio.create_task(
                self.proxy.handle(reader_with(VALID_REQUEST, eof=False), writer)
            )
            await asyncio.wait_for(process.stdout.read_started.wait(), 0.5)

            retained_waiter = None
            for unused_attempt in range(3):
                with self.assertRaises(module.EgressError) as caught:
                    await asyncio.wait_for(self.proxy.close(), 0.5)
                self.assertEqual(caught.exception.code, "unavailable")
                self.assertEqual(len(self.proxy._resolvers), 1)
                retained = next(iter(self.proxy._resolvers)).exception()
                self.assertIsInstance(retained, module._ResolverCleanupFailure)
                self.assertIs(retained.process, process)
                if retained_waiter is None:
                    retained_waiter = retained.waiter
                self.assertIs(retained.waiter, retained_waiter)
                self.assertFalse(retained.waiter.done())
                self.assertEqual(process.wait_calls, 1)

            await asyncio.gather(handler, return_exceptions=True)
            process.kill_completes = True
            await asyncio.wait_for(self.proxy.close(), 0.5)

        self.assertTrue(process.reaped)
        self.assertEqual(process.wait_calls, 1)
        self.assertFalse(self.proxy._resolvers)

    async def test_cancellation_during_resolver_launch_has_no_detached_task(self):
        if not hasattr(module, "_launch_resolver"):
            self.fail("terminable resolver helper is not implemented")
        launch_started = asyncio.Event()
        launch_cancelled = asyncio.Event()

        async def stalled_launch():
            launch_started.set()
            try:
                await asyncio.Future()
            except asyncio.CancelledError:
                launch_cancelled.set()
                raise

        writer = MemoryWriter()
        with mock.patch.object(module, "_launch_resolver", stalled_launch):
            handler = asyncio.create_task(
                self.proxy.handle(reader_with(VALID_REQUEST, eof=False), writer)
            )
            await asyncio.wait_for(launch_started.wait(), 0.5)
            handler.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await asyncio.wait_for(handler, 0.5)
        self.assertTrue(launch_cancelled.is_set())
        self.assertTrue(writer.closed and writer.waited_closed)
        await asyncio.wait_for(self.proxy.close(), 0.5)

    async def test_concurrent_resolver_helpers_never_exceed_connection_ceiling(self):
        if not hasattr(module, "_launch_resolver"):
            self.fail("terminable resolver helper is not implemented")
        processes = [FakeProcess(stalled=True), FakeProcess(stalled=True)]
        launches = []

        async def launch():
            process = processes[len(launches)]
            launches.append(process)
            return process

        with mock.patch.object(module, "_launch_resolver", launch), mock.patch.multiple(
            module, MAX_CONNECTIONS=2, RESOLVER_CLEANUP_SECONDS=0.01
        ):
            handlers = [
                asyncio.create_task(
                    self.proxy.handle(
                        reader_with(VALID_REQUEST, eof=False), MemoryWriter()
                    )
                )
                for _unused in range(2)
            ]
            await asyncio.gather(
                *(process.stdout.read_started.wait() for process in processes)
            )
            rejected = MemoryWriter()
            await self.proxy.handle(reader_with(VALID_REQUEST), rejected)
            self.assertEqual(len(launches), 2)
            self.assertTrue(
                bytes(rejected.data).startswith(b"HTTP/1.1 503 Service Unavailable")
            )
            await asyncio.wait_for(self.proxy.close(), 0.5)
            await asyncio.gather(*handlers, return_exceptions=True)
        self.assertTrue(all(process.reaped for process in processes))

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

    async def test_stalled_writer_cleanup_aborts_both_streams_before_slot_release(self):
        async def resolve(unused_host):
            return ["1.1.1.1"]

        inbound_writer = BufferedWriter()
        upstream_writer = BufferedWriter()

        async def open_connection(unused_host, unused_port, *, family):
            del family
            return reader_with(b"server-tls"), upstream_writer

        with mock.patch.object(module, "_resolve", resolve), mock.patch.object(
            module, "_open_connection", open_connection
        ), mock.patch.multiple(module, IDLE_SECONDS=0.01, MAX_CONNECTIONS=1):
            await asyncio.wait_for(
                self.proxy.handle(
                    reader_with(VALID_REQUEST + b"client-tls"), inbound_writer
                ),
                0.5,
            )
            self.assertTrue(inbound_writer.close_requested)
            self.assertTrue(upstream_writer.close_requested)
            self.assertTrue(inbound_writer.aborted and inbound_writer.closed)
            self.assertTrue(upstream_writer.aborted and upstream_writer.closed)

            released_writer = MemoryWriter()
            await self.proxy.handle(reader_with(b"GET / HTTP/1.1\r\n\r\n"), released_writer)
            self.assertTrue(
                bytes(released_writer.data).startswith(b"HTTP/1.1 400 Bad Request")
            )
            self.assertNotIn(b"503", released_writer.data)

    async def test_interrupted_writer_cleanup_aborts_before_handler_finishes(self):
        writer = BufferedWriter()
        with mock.patch.object(module, "IDLE_SECONDS", 0.01):
            handler = asyncio.create_task(
                self.proxy.handle(reader_with(b"GET / HTTP/1.1\r\n\r\n"), writer)
            )
            await asyncio.wait_for(writer.wait_closed_started.wait(), 0.5)
            handler.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await asyncio.wait_for(handler, 0.5)
        self.assertTrue(writer.close_requested)
        self.assertTrue(writer.aborted and writer.closed)
        await asyncio.wait_for(self.proxy.close(), 0.5)

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

    async def test_saturated_and_closed_refusals_never_await_untracked_writers(self):
        expected = (
            b"HTTP/1.1 503 Service Unavailable\r\n"
            b"Content-Length: 0\r\n"
            b"Connection: close\r\n\r\n"
        )

        async def assert_prompt_refusal(proxy):
            writer = BufferedWriter()
            task = asyncio.create_task(proxy.handle(reader_with(VALID_REQUEST), writer))
            try:
                await asyncio.sleep(0)
                await asyncio.sleep(0)
                self.assertTrue(task.done(), "rejected handler remained unowned")
                await task
            finally:
                if not task.done():
                    task.cancel()
                    await asyncio.gather(task, return_exceptions=True)
            self.assertEqual(bytes(writer.data), expected)
            self.assertTrue(writer.close_requested)
            self.assertTrue(writer.aborted and writer.closed)
            self.assertFalse(writer.drain_started.is_set())
            self.assertFalse(writer.wait_closed_started.is_set())

        with mock.patch.object(module, "MAX_CONNECTIONS", 1), mock.patch.object(
            module, "IDLE_SECONDS", 0.01
        ):
            active = asyncio.create_task(
                self.proxy.handle(reader_with(b"", eof=False), MemoryWriter())
            )
            await asyncio.sleep(0)
            try:
                await assert_prompt_refusal(self.proxy)
            finally:
                await asyncio.wait_for(self.proxy.close(), 0.5)
                await asyncio.gather(active, return_exceptions=True)

            await assert_prompt_refusal(self.proxy)
            await asyncio.wait_for(self.proxy.close(), 0.5)

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
