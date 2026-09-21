"""Runtime provider-egress CONNECT proxy: a bounded, EXACT multi-host allowlist.

The login proxy (`neev_codex_login_egress`) is deliberately pinned to ONE fixed
authority for the interactive login flow. Runtime provider traffic must reach the
provider's *runtime* hosts (from `neev_provider_endpoints`) — still bounded to an
exact allowlist with public-IP validation and NEVER a wildcard / all-Internet
fallback. This module reuses the login proxy's proven, host-agnostic primitives
(public-IPv4 validation, bounded phase deadlines, resolver termination/reaping,
writer cleanup, fixed HTTP responses) and adds only what differs: a multi-authority
CONNECT parse, a per-host DNS resolver whose host is baked in as a validated literal
(no argv/stdin injection surface), and the destination policy itself.

Every allowed destination must be named exactly in the policy; a resolved private/
loopback/link-local/reserved address is denied whole (DNS-rebinding safe). Linux is
the production target for the resolver subprocess; the pure parse/policy logic is
platform-agnostic and unit tested, and a same-host acceptance
(`neev_provider_egress_acceptance.py`) proves a real allowed TLS dial succeeds while
a non-allowlisted authority and a private address are refused.
"""

from __future__ import annotations

import asyncio
import json
import socket
from contextlib import suppress
from dataclasses import dataclass
from types import MappingProxyType
from typing import Any, Optional, Tuple

import neev_codex_login_egress as login
import neev_provider_endpoints as endpoints

# Reuse the login proxy's audited primitives verbatim.
EgressError = login.EgressError
validated_ipv4 = login.validated_ipv4
MAX_HEADER_BYTES = login.MAX_HEADER_BYTES
MAX_CONNECTIONS = login.MAX_CONNECTIONS
MAX_DNS_RESULTS = login.MAX_DNS_RESULTS
_MAX_RESOLVER_OUTPUT_BYTES = login._MAX_RESOLVER_OUTPUT_BYTES
HEADER_SECONDS = login.HEADER_SECONDS
DNS_SECONDS = login.DNS_SECONDS
CONNECT_SECONDS = login.CONNECT_SECONDS
LIFETIME_SECONDS = login.LIFETIME_SECONDS
IDLE_SECONDS = login.IDLE_SECONDS
_RESPONSES = login._RESPONSES
_ESTABLISHED = login._ESTABLISHED

_ALLOWED_HEADERS = frozenset((b"host", b"user-agent", b"proxy-connection"))

# Per-host resolver program: the host is embedded as a validated %r literal (an
# FQDN of [a-z0-9.-] only, so the repr can carry no quote/backslash/injection), and
# the port likewise. No argv, no stdin, no inherited env — identical isolation to the
# login proxy's fixed resolver, generalized to one allowlisted host.
_RESOLVER_TEMPLATE = (
    "import json,socket,sys\n"
    "try:\n"
    " r=socket.getaddrinfo(%r,%d,family=socket.AF_UNSPEC,"
    "type=socket.SOCK_STREAM,proto=socket.IPPROTO_TCP)\n"
    " a=[]\n"
    " for x in r:\n"
    "  a.append(x[4][0])\n"
    "  if len(a)>%d: break\n"
    " p=json.dumps(a,separators=(',',':')).encode('ascii')\n"
    " if len(p)>%d: raise RuntimeError()\n"
    " sys.stdout.buffer.write(p)\n"
    "except BaseException:\n"
    " sys.exit(1)\n"
)


@dataclass(frozen=True)
class DestinationPolicy:
    """An immutable, exact CONNECT allowlist: which `host:port` authorities may be
    tunneled and how to resolve each host."""

    authorities: frozenset          # of bytes b"host:port"
    ports: MappingProxyType         # host(str) -> port(int)
    resolver_code: MappingProxyType  # host(str) -> fixed resolver program(str)

    def hosts(self) -> frozenset:
        return frozenset(self.ports)

    def port_for(self, host: str) -> int:
        try:
            return self.ports[host]
        except KeyError:
            raise EgressError("denied") from None


def policy_for(provider: str, purpose: str) -> DestinationPolicy:
    """Build the runtime CONNECT policy for a provider+purpose from the endpoint
    allowlist. Raises EgressError on an unknown scope (fail closed)."""
    try:
        table = endpoints.endpoints(provider, purpose)
    except endpoints.EndpointError:
        raise EgressError("denied") from None
    ports = {}
    codes = {}
    for endpoint in table:
        ports[endpoint.host] = endpoint.port
        codes[endpoint.host] = _RESOLVER_TEMPLATE % (
            endpoint.host, endpoint.port, MAX_DNS_RESULTS, _MAX_RESOLVER_OUTPUT_BYTES
        )
    return DestinationPolicy(
        authorities=frozenset(e.authority() for e in table),
        ports=MappingProxyType(ports),
        resolver_code=MappingProxyType(codes),
    )


def parse_connect(header: bytes, policy: DestinationPolicy) -> str:
    """Validate one complete, bodyless CONNECT for an authority in `policy`; return
    the target host. Mirrors the login proxy's strict parser but matches the request
    (and Host header) against the allowlist rather than a single fixed authority."""
    if type(header) is not bytes:
        raise EgressError("invalid")
    if len(header) > MAX_HEADER_BYTES:
        raise EgressError("overflow")
    if any(byte not in (0x0D, 0x0A) and (byte < 0x20 or byte > 0x7E) for byte in header):
        raise EgressError("invalid")
    if not header.endswith(b"\r\n\r\n"):
        raise EgressError("invalid")
    framed = header[:-4]
    if b"\r\n\r\n" in framed or b"\r" in framed.replace(b"\r\n", b""):
        raise EgressError("invalid")
    if b"\n" in framed.replace(b"\r\n", b""):
        raise EgressError("invalid")

    lines = framed.split(b"\r\n")
    if not lines or not lines[0]:
        raise EgressError("invalid")
    request_parts = lines[0].split(b" ")
    if len(request_parts) != 3:
        raise EgressError("invalid")
    method, authority, version = request_parts
    if method != b"CONNECT" or version != b"HTTP/1.1":
        raise EgressError("invalid")
    if authority not in policy.authorities:
        raise EgressError("denied")

    seen = set()
    for line in lines[1:]:
        if not line or line[:1] in (b" ", b"\t"):
            raise EgressError("invalid")
        name, separator, value = line.partition(b": ")
        if not separator or b":" in name or not name:
            raise EgressError("invalid")
        try:
            lowered = name.decode("ascii").lower().encode("ascii")
        except UnicodeDecodeError:
            raise EgressError("invalid") from None
        if lowered not in _ALLOWED_HEADERS or lowered in seen:
            raise EgressError("invalid")
        if not value or any(byte < 0x20 or byte > 0x7E for byte in value):
            raise EgressError("invalid")
        if lowered == b"host" and value != authority:
            raise EgressError("invalid")
        # The pinned Claude CLI's Bun HTTP client sends "Proxy-Connection: close" on its
        # CONNECT; accept keep-alive AND close, case-insensitively (a validated token) —
        # any other value is still rejected. (Captured: CONNECT platform.claude.com:443
        # HTTP/1.1, Host platform.claude.com:443, Proxy-Connection: close.)
        if lowered == b"proxy-connection" and value.lower() not in (b"keep-alive", b"close"):
            raise EgressError("invalid")
        seen.add(lowered)
    return authority.rsplit(b":", 1)[0].decode("ascii")


async def _launch_resolver(code: str):  # pragma: no cover - subprocess
    return await asyncio.create_subprocess_exec(
        login.sys.executable, "-I", "-c", code,
        stdin=asyncio.subprocess.DEVNULL,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.DEVNULL,
        env={"LANG": "C", "LC_ALL": "C"},
        close_fds=True,
    )


async def _resolve(host: str, policy: DestinationPolicy) -> list:
    """Resolve one allowlisted host via a fixed, isolated helper; reap it always.
    Reuses the login proxy's bounded-output reader and terminate/reap primitives."""
    if host not in policy.hosts():
        raise EgressError("denied")
    code = policy.resolver_code[host]
    process = None
    try:
        process = await _launch_resolver(code)
        output = await login._read_resolver_output(process)
        returncode = await process.wait()
        if returncode != 0:
            raise EgressError("unavailable")
        try:
            addresses = json.loads(output.decode("ascii"))
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError, TypeError):
            raise EgressError("unavailable") from None
        if type(addresses) is not list:
            raise EgressError("unavailable")
        if len(addresses) > MAX_DNS_RESULTS + 1:
            raise EgressError("overflow")
        if any(type(address) is not str for address in addresses):
            raise EgressError("unavailable")
        return addresses
    except asyncio.CancelledError:
        if process is not None:
            await login._terminate_resolver(process)
        raise
    except login._ResolverCleanupFailure:
        raise
    except EgressError:
        if process is not None:
            await login._terminate_resolver(process)
        raise
    except Exception:
        if process is not None:
            await login._terminate_resolver(process)
        raise EgressError("unavailable") from None


class ProviderEgressProxy:
    """Own admission, resolution, dial and bounded relay for runtime tunnels to a
    fixed multi-host allowlist. Composed from the login proxy's audited primitives."""

    def __init__(self, policy: DestinationPolicy) -> None:
        self._policy = policy
        self._closed = False
        self._active = 0
        self._handlers = set()
        self._relays = set()
        self._writers = set()
        self._writer_waiters = {}

    async def handle(self, reader: asyncio.StreamReader, writer: Any) -> None:
        if self._closed or self._active >= MAX_CONNECTIONS:
            login._refuse_without_waiting(writer)
            return
        self._active += 1
        current = asyncio.current_task()
        if current is not None:
            self._handlers.add(current)
        self._writers.add(writer)
        upstream_writer = None
        established = False
        deadline = asyncio.get_running_loop().time() + LIFETIME_SECONDS
        try:
            header = await self._read_header(
                reader, login._phase_deadline(deadline, HEADER_SECONDS)
            )
            host = parse_connect(header, self._policy)
            addresses = await login._before(
                asyncio.ensure_future(_resolve(host, self._policy)),
                login._phase_deadline(deadline, DNS_SECONDS),
            )
            selected = validated_ipv4(addresses)
            upstream_reader, upstream_writer = await self._dial(
                selected, self._policy.port_for(host),
                login._phase_deadline(deadline, CONNECT_SECONDS),
            )
            self._writers.add(upstream_writer)
            writer.write(_ESTABLISHED)
            established = True
            await login._before(writer.drain(), deadline)
            await self._tunnel(reader, writer, upstream_reader, upstream_writer, deadline)
        except asyncio.CancelledError:
            raise
        except EgressError as error:
            if not established:
                await self._send_error(writer, error)
        except Exception:
            if not established:
                await self._send_error(writer, EgressError("unavailable"))
        finally:
            await self._cleanup_writers(upstream_writer, writer)
            if current is not None:
                self._handlers.discard(current)
            self._active -= 1

    async def close(self) -> None:
        self._closed = True
        current = asyncio.current_task()
        handlers = tuple(task for task in self._handlers if task is not current)
        relays = tuple(task for task in self._relays if task is not current)
        for writer in tuple(self._writers):
            with suppress(Exception):
                writer.close()
        for task in relays + handlers:
            task.cancel()
        if relays + handlers:
            await asyncio.wait(relays + handlers, timeout=2 * IDLE_SECONDS)
        if self._writers:
            await self._cleanup_writers(*tuple(self._writers))

    async def _read_header(self, reader, deadline) -> bytes:
        try:
            header = await login._before(reader.readuntil(b"\r\n\r\n"), deadline)
        except asyncio.IncompleteReadError as error:
            if len(error.partial) > MAX_HEADER_BYTES:
                raise EgressError("overflow") from None
            raise EgressError("invalid") from None
        except asyncio.LimitOverrunError:
            raise EgressError("overflow") from None
        if len(header) > MAX_HEADER_BYTES:
            raise EgressError("overflow")
        return header

    async def _dial(self, addresses, port, deadline) -> Tuple[asyncio.StreamReader, Any]:
        for address in addresses:
            try:
                return await login._before(
                    login._open_connection(address, port, family=socket.AF_INET),
                    deadline,
                )
            except EgressError:
                raise
            except asyncio.CancelledError:
                raise
            except Exception:
                continue
        raise EgressError("unavailable")

    async def _send_error(self, writer, error) -> None:
        response = _RESPONSES[error.code]
        try:
            await asyncio.wait_for(login.writer_drain(writer, response), IDLE_SECONDS)
        except (Exception, asyncio.CancelledError):
            pass

    async def _tunnel(self, inbound_reader, inbound_writer, upstream_reader,
                      upstream_writer, deadline) -> None:
        loop = asyncio.get_running_loop()
        last = [loop.time()]
        activity = asyncio.Event()

        async def relay(source, sink):
            total = 0
            while True:
                remaining = login.MAX_TUNNEL_BYTES_PER_DIRECTION - total
                chunk = await source.read(min(login.IO_CHUNK_BYTES, max(1, remaining + 1)))
                if not chunk:
                    return
                if (
                    len(chunk) > login.IO_CHUNK_BYTES
                    or total + len(chunk) > login.MAX_TUNNEL_BYTES_PER_DIRECTION
                ):
                    raise EgressError("overflow")
                sink.write(chunk)
                await sink.drain()
                total += len(chunk)
                last[0] = loop.time()
                activity.set()

        relays = {
            asyncio.create_task(relay(inbound_reader, upstream_writer)),
            asyncio.create_task(relay(upstream_reader, inbound_writer)),
        }
        self._relays.update(relays)
        waiter: Optional[asyncio.Task] = None
        try:
            while True:
                now = loop.time()
                stop = min(deadline, last[0] + IDLE_SECONDS)
                remaining = stop - now
                if remaining <= 0:
                    raise EgressError("timeout")
                activity.clear()
                waiter = asyncio.create_task(activity.wait())
                done, _pending = await asyncio.wait(
                    relays | {waiter}, timeout=remaining,
                    return_when=asyncio.FIRST_COMPLETED,
                )
                if not done:
                    raise EgressError("timeout")
                finished = done & relays
                if finished:
                    for task in finished:
                        await task
                    return
                await waiter
                waiter = None
        finally:
            if waiter is not None:
                waiter.cancel()
            for task in relays:
                if not task.done():
                    task.cancel()
            await asyncio.gather(
                *relays, *((waiter,) if waiter is not None else ()),
                return_exceptions=True,
            )
            self._relays.difference_update(relays)

    def _writer_waiter(self, writer):
        waiter = self._writer_waiters.get(writer)
        if waiter is None:
            waiter = asyncio.create_task(writer.wait_closed())
            waiter.add_done_callback(login._consume_task_result)
            self._writer_waiters[writer] = waiter
        return waiter

    async def _cleanup_writers(self, *writers) -> None:
        selected = tuple(writer for writer in writers if writer is not None)
        for writer in selected:
            with suppress(Exception):
                writer.close()
        for writer in selected:
            waiter = self._writer_waiter(writer)
            try:
                await asyncio.wait_for(asyncio.shield(waiter), IDLE_SECONDS)
            except asyncio.TimeoutError:
                with suppress(Exception):
                    login._abort_writer(writer)
            except (Exception, asyncio.CancelledError):
                pass
            self._writer_waiters.pop(writer, None)
            self._writers.discard(writer)


async def serve(provider: str, purpose: str, *, host="0.0.0.0", port=3128) -> None:  # pragma: no cover
    """Serve the fixed runtime allowlist for one provider+purpose until cancellation."""
    policy = policy_for(provider, purpose)
    proxy = ProviderEgressProxy(policy)
    server = await asyncio.start_server(
        proxy.handle, host, port, limit=MAX_HEADER_BYTES + 1
    )
    try:
        async with server:
            await server.serve_forever()
    finally:
        server.close()
        await server.wait_closed()
        await proxy.close()
