"""Bounded fixed-destination CONNECT proxy for Neev Codex login traffic.

This component pins a validated numeric address for one opaque TLS tunnel. It is
not, by itself, a network-isolation boundary.
"""

from __future__ import annotations

import asyncio
import ipaddress
import socket
from contextlib import suppress
from typing import Any, Awaitable, Optional, Tuple


DESTINATION_HOST = "auth.openai.com"
DESTINATION_PORT = 443
LISTEN_HOST = "0.0.0.0"
LISTEN_PORT = 3128

MAX_HEADER_BYTES = 8192
MAX_CONNECTIONS = 16
MAX_DNS_RESULTS = 16
IO_CHUNK_BYTES = 16384
MAX_TUNNEL_BYTES_PER_DIRECTION = 4 * 1024 * 1024
HEADER_SECONDS = 5.0
DNS_SECONDS = 5.0
CONNECT_SECONDS = 10.0
IDLE_SECONDS = 30.0
LIFETIME_SECONDS = 600.0

_AUTHORITY = b"auth.openai.com:443"
_ERROR_CODES = frozenset(
    ("invalid", "denied", "unavailable", "timeout", "overflow", "busy")
)
_ALLOWED_HEADERS = frozenset((b"host", b"user-agent", b"proxy-connection"))
_RESPONSES = {
    "invalid": b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    "overflow": b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    "denied": b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    "unavailable": b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    "timeout": b"HTTP/1.1 504 Gateway Timeout\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    "busy": b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
}
_ESTABLISHED = b"HTTP/1.1 200 Connection Established\r\n\r\n"


class EgressError(Exception):
    """An egress failure containing only an allowlisted public code."""

    def __init__(self, code: str):
        safe_code = code if type(code) is str and code in _ERROR_CODES else "invalid"
        self.code = safe_code
        super().__init__(safe_code)


def parse_connect(header: bytes) -> str:
    """Validate one complete, bodyless CONNECT request for the fixed authority."""
    if type(header) is not bytes:
        raise EgressError("invalid")
    if len(header) > MAX_HEADER_BYTES:
        raise EgressError("overflow")
    if any(
        byte not in (0x0D, 0x0A) and (byte < 0x20 or byte > 0x7E)
        for byte in header
    ):
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
    if authority != _AUTHORITY:
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
        if lowered == b"host" and value != _AUTHORITY:
            raise EgressError("invalid")
        if lowered == b"proxy-connection" and value not in (b"keep-alive", b"Keep-Alive"):
            raise EgressError("invalid")
        seen.add(lowered)
    return DESTINATION_HOST


def validated_ipv4(addresses: list[str]) -> tuple[str, ...]:
    """Validate a complete DNS answer and select unique public numeric IPv4."""
    if type(addresses) is not list:
        raise EgressError("invalid")
    if not addresses:
        raise EgressError("unavailable")
    if len(addresses) > MAX_DNS_RESULTS:
        raise EgressError("overflow")

    selected = []
    seen = set()
    for address in addresses:
        if type(address) is not str or not address or "%" in address:
            raise EgressError("invalid")
        try:
            parsed = ipaddress.ip_address(address)
        except ValueError:
            raise EgressError("invalid") from None
        if isinstance(parsed, ipaddress.IPv6Address) and parsed.ipv4_mapped is not None:
            raise EgressError("denied")
        if str(parsed) != address:
            raise EgressError("invalid")
        if (
            parsed.is_private
            or parsed.is_loopback
            or parsed.is_link_local
            or parsed.is_multicast
            or parsed.is_reserved
            or parsed.is_unspecified
            or not parsed.is_global
        ):
            raise EgressError("denied")
        if isinstance(parsed, ipaddress.IPv4Address) and address not in seen:
            selected.append(address)
            seen.add(address)
    if not selected:
        raise EgressError("unavailable")
    return tuple(selected)


async def _resolve(host: str) -> list[str]:
    loop = asyncio.get_running_loop()
    results = await loop.getaddrinfo(
        host,
        DESTINATION_PORT,
        family=socket.AF_UNSPEC,
        type=socket.SOCK_STREAM,
        proto=socket.IPPROTO_TCP,
    )
    addresses = []
    for result in results:
        addresses.append(result[4][0])
        if len(addresses) > MAX_DNS_RESULTS:
            break
    return addresses


async def _open_connection(host: str, port: int, *, family: int):
    return await asyncio.open_connection(host, port, family=family)


def _phase_deadline(lifetime_deadline: float, seconds: float) -> float:
    loop = asyncio.get_running_loop()
    return min(lifetime_deadline, loop.time() + seconds)


async def _before(awaitable: Awaitable[Any], deadline: float) -> Any:
    remaining = deadline - asyncio.get_running_loop().time()
    if remaining <= 0:
        if hasattr(awaitable, "close"):
            awaitable.close()  # type: ignore[attr-defined]
        raise EgressError("timeout")
    try:
        return await asyncio.wait_for(awaitable, remaining)
    except asyncio.TimeoutError:
        raise EgressError("timeout") from None


async def _close_writer(writer: Any) -> None:
    if writer is None:
        return
    with suppress(Exception):
        writer.close()
    try:
        await asyncio.wait_for(writer.wait_closed(), IDLE_SECONDS)
    except (Exception, asyncio.CancelledError):
        pass


def _refuse_without_waiting(writer: Any) -> None:
    """Best-effort fixed refusal for connections outside the owned lifecycle."""
    with suppress(Exception):
        writer.write(_RESPONSES["busy"])
    with suppress(Exception):
        writer.close()


class LoginEgressProxy:
    """Own admission, resolution, dial, relay, and shutdown for login tunnels."""

    def __init__(self) -> None:
        self._closed = False
        self._active = 0
        self._handlers = set()
        self._resolvers = set()
        self._relays = set()
        self._writers = set()

    async def handle(self, reader: asyncio.StreamReader, writer: Any) -> None:
        """Own one inbound CONNECT connection from admission through cleanup."""
        if self._closed or self._active >= MAX_CONNECTIONS:
            _refuse_without_waiting(writer)
            return

        self._active += 1
        current = asyncio.current_task()
        if current is not None:
            self._handlers.add(current)
        self._writers.add(writer)
        upstream_writer = None
        established = False
        lifetime_deadline = asyncio.get_running_loop().time() + LIFETIME_SECONDS
        try:
            header = await self._read_header(
                reader, _phase_deadline(lifetime_deadline, HEADER_SECONDS)
            )
            host = parse_connect(header)
            addresses = await self._resolve_checked(
                host, _phase_deadline(lifetime_deadline, DNS_SECONDS)
            )
            upstream_reader, upstream_writer = await self._dial(
                addresses, _phase_deadline(lifetime_deadline, CONNECT_SECONDS)
            )
            self._writers.add(upstream_writer)
            writer.write(_ESTABLISHED)
            established = True
            await _before(writer.drain(), lifetime_deadline)
            await self._tunnel(
                reader,
                writer,
                upstream_reader,
                upstream_writer,
                lifetime_deadline,
            )
        except asyncio.CancelledError:
            raise
        except EgressError as error:
            if not established:
                await self._send_error(writer, error)
        except Exception:
            if not established:
                await self._send_error(writer, EgressError("unavailable"))
        finally:
            await _close_writer(upstream_writer)
            await _close_writer(writer)
            if upstream_writer is not None:
                self._writers.discard(upstream_writer)
            self._writers.discard(writer)
            if current is not None:
                self._handlers.discard(current)
            self._active -= 1

    async def close(self) -> None:
        """Stop admission, close all streams, and reap owned asynchronous work."""
        self._closed = True
        current = asyncio.current_task()
        handlers = tuple(task for task in self._handlers if task is not current)
        relays = tuple(task for task in self._relays if task is not current)
        for writer in tuple(self._writers):
            with suppress(Exception):
                writer.close()
        for task in relays + handlers:
            task.cancel()
        if relays or handlers:
            await asyncio.gather(*relays, *handlers, return_exceptions=True)
        resolvers = tuple(self._resolvers)
        if resolvers:
            await asyncio.gather(*resolvers, return_exceptions=True)

    async def _read_header(self, reader: asyncio.StreamReader, deadline: float) -> bytes:
        try:
            header = await _before(reader.readuntil(b"\r\n\r\n"), deadline)
        except asyncio.IncompleteReadError as error:
            if len(error.partial) > MAX_HEADER_BYTES:
                raise EgressError("overflow") from None
            raise EgressError("invalid") from None
        except asyncio.LimitOverrunError:
            raise EgressError("overflow") from None
        if len(header) > MAX_HEADER_BYTES:
            raise EgressError("overflow")
        return header

    async def _resolve_checked(self, host: str, deadline: float) -> tuple[str, ...]:
        if len(self._resolvers) >= MAX_CONNECTIONS:
            raise EgressError("busy")
        task = asyncio.create_task(_resolve(host))
        self._resolvers.add(task)

        def resolution_finished(finished: asyncio.Task) -> None:
            self._resolvers.discard(finished)
            with suppress(Exception, asyncio.CancelledError):
                finished.result()

        task.add_done_callback(resolution_finished)
        try:
            addresses = await _before(asyncio.shield(task), deadline)
        except EgressError:
            raise
        except asyncio.CancelledError:
            raise
        except Exception:
            raise EgressError("unavailable") from None
        return validated_ipv4(addresses)

    async def _dial(
        self, addresses: tuple[str, ...], deadline: float
    ) -> Tuple[asyncio.StreamReader, Any]:
        for address in addresses:
            try:
                return await _before(
                    _open_connection(address, DESTINATION_PORT, family=socket.AF_INET),
                    deadline,
                )
            except EgressError:
                raise
            except asyncio.CancelledError:
                raise
            except Exception:
                continue
        raise EgressError("unavailable")

    async def _send_error(self, writer: Any, error: EgressError) -> None:
        response = _RESPONSES[error.code]
        try:
            await asyncio.wait_for(writer_drain(writer, response), IDLE_SECONDS)
        except (Exception, asyncio.CancelledError):
            pass

    async def _tunnel(
        self,
        inbound_reader: asyncio.StreamReader,
        inbound_writer: Any,
        upstream_reader: asyncio.StreamReader,
        upstream_writer: Any,
        lifetime_deadline: float,
    ) -> None:
        loop = asyncio.get_running_loop()
        last_activity = [loop.time()]
        activity = asyncio.Event()

        async def relay(source: asyncio.StreamReader, sink: Any) -> None:
            total = 0
            while True:
                remaining = MAX_TUNNEL_BYTES_PER_DIRECTION - total
                chunk = await source.read(min(IO_CHUNK_BYTES, max(1, remaining + 1)))
                if not chunk:
                    return
                if (
                    len(chunk) > IO_CHUNK_BYTES
                    or total + len(chunk) > MAX_TUNNEL_BYTES_PER_DIRECTION
                ):
                    raise EgressError("overflow")
                sink.write(chunk)
                await sink.drain()
                total += len(chunk)
                last_activity[0] = loop.time()
                activity.set()

        relays = {
            asyncio.create_task(relay(inbound_reader, upstream_writer)),
            asyncio.create_task(relay(upstream_reader, inbound_writer)),
        }
        self._relays.update(relays)
        event_waiter: Optional[asyncio.Task] = None
        try:
            while True:
                now = loop.time()
                deadline = min(lifetime_deadline, last_activity[0] + IDLE_SECONDS)
                remaining = deadline - now
                if remaining <= 0:
                    raise EgressError("timeout")
                activity.clear()
                event_waiter = asyncio.create_task(activity.wait())
                done, unused_pending = await asyncio.wait(
                    relays | {event_waiter},
                    timeout=remaining,
                    return_when=asyncio.FIRST_COMPLETED,
                )
                del unused_pending
                if not done:
                    raise EgressError("timeout")
                finished_relays = done & relays
                if finished_relays:
                    for task in finished_relays:
                        await task
                    return
                await event_waiter
                event_waiter = None
        finally:
            if event_waiter is not None:
                event_waiter.cancel()
            for task in relays:
                if not task.done():
                    task.cancel()
            await asyncio.gather(
                *relays,
                *((event_waiter,) if event_waiter is not None else ()),
                return_exceptions=True,
            )
            self._relays.difference_update(relays)


async def writer_drain(writer: Any, data: bytes) -> None:
    writer.write(data)
    await writer.drain()


async def serve() -> None:
    """Serve the fixed local policy until cancellation or listener failure."""
    proxy = LoginEgressProxy()
    server = await asyncio.start_server(
        proxy.handle,
        LISTEN_HOST,
        LISTEN_PORT,
        limit=MAX_HEADER_BYTES + 1,
    )
    try:
        async with server:
            await server.serve_forever()
    finally:
        server.close()
        await server.wait_closed()
        await proxy.close()


if __name__ == "__main__":
    asyncio.run(serve())
