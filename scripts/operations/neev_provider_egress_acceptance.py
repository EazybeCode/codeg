#!/usr/bin/env python3
"""Same-host acceptance for the runtime allowlist CONNECT proxy
(`neev_provider_egress`). Proves, against a REAL network, the properties root asked
to see before we claim runtime egress operational:

  1. ALLOWED   — a CONNECT to an allowlisted host establishes a tunnel AND a real
                 TLS handshake completes through it (bytes flow to a real TLS host).
  2. DENIED    — a CONNECT to a non-allowlisted authority is refused 403 (no dial).
  3. REBIND    — an allowlisted host whose DNS answer is a PRIVATE address is refused
                 403 before any dial (DNS-rebinding safe).

Uses a stable public TLS endpoint (one.one.one.one:443) as the allowed host and a
fixed 127.0.0.1-answering resolver for the rebind host, so nothing provider-specific
or secret is involved. Needs outbound network; opt-in so it never runs in CI:

    NEEV_PROVIDER_EGRESS_ACCEPTANCE=1 python3 neev_provider_egress_acceptance.py

Exit 0 = all properties held; nonzero = a property failed or a prerequisite was
unmet (so a skip can never masquerade as a pass; --allow-skip forces exit 0)."""

from __future__ import annotations

import argparse
import asyncio
import os
import queue
import socket
import ssl
import sys
import threading

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import neev_provider_egress as egress
from types import MappingProxyType

_ALLOWED_HOST = "one.one.one.one"
_REBIND_HOST = "rebind.test"


def _acceptance_policy() -> egress.DestinationPolicy:
    real = egress._RESOLVER_TEMPLATE % (
        _ALLOWED_HOST, 443, egress.MAX_DNS_RESULTS, egress._MAX_RESOLVER_OUTPUT_BYTES
    )
    # A deliberately hostile resolver: an allowlisted authority whose DNS answer is a
    # private address. The proxy must refuse it (validated_ipv4) before dialling.
    rebind = "import sys\nsys.stdout.buffer.write(b'[\"127.0.0.1\"]')\n"
    return egress.DestinationPolicy(
        authorities=frozenset((b"one.one.one.one:443", b"rebind.test:443")),
        ports=MappingProxyType({_ALLOWED_HOST: 443, _REBIND_HOST: 443}),
        resolver_code=MappingProxyType({_ALLOWED_HOST: real, _REBIND_HOST: rebind}),
    )


class _ProxyThread(threading.Thread):
    """Run the asyncio proxy on its own loop so the client can use blocking sockets."""

    def __init__(self, policy):
        super().__init__(daemon=True)
        self._policy = policy
        self._ready = queue.Queue()
        self._loop = None
        self._stop_event = None
        self._proxy = None
        self._server = None

    def run(self):
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)
        self._loop = loop
        self._stop_event = asyncio.Event()
        self._proxy = egress.ProviderEgressProxy(self._policy)
        self._server = loop.run_until_complete(
            asyncio.start_server(self._proxy.handle, "127.0.0.1", 0,
                                 limit=egress.MAX_HEADER_BYTES + 1)
        )
        self._ready.put(self._server.sockets[0].getsockname()[1])
        loop.run_until_complete(self._stop_event.wait())
        self._server.close()
        loop.run_until_complete(self._server.wait_closed())
        loop.run_until_complete(self._proxy.close())
        loop.close()

    def port(self):
        return self._ready.get(timeout=10)

    def shutdown(self):
        if self._loop is not None and self._stop_event is not None:
            self._loop.call_soon_threadsafe(self._stop_event.set)
        self.join(timeout=10)


def _connect_request(authority: bytes) -> bytes:
    return b"CONNECT " + authority + b" HTTP/1.1\r\nHost: " + authority + b"\r\n\r\n"


def _read_until_headers(sock) -> bytes:
    buffer = bytearray()
    while b"\r\n\r\n" not in buffer and len(buffer) < 4096:
        chunk = sock.recv(1024)
        if not chunk:
            break
        buffer.extend(chunk)
    return bytes(buffer)


def _attempt(port, authority):
    sock = socket.create_connection(("127.0.0.1", port), timeout=15)
    try:
        sock.sendall(_connect_request(authority))
        return sock, _read_until_headers(sock)
    except Exception:
        sock.close()
        raise


def run(port):
    failures = []

    def check(name, ok):
        print(("PASS" if ok else "FAIL"), name)
        if not ok:
            failures.append(name)

    # 1. ALLOWED: tunnel establishes AND a real TLS handshake completes through it.
    sock, headers = _attempt(port, b"one.one.one.one:443")
    tls_ok = False
    try:
        if b"200 Connection Established" in headers:
            context = ssl.create_default_context()
            tls = context.wrap_socket(sock, server_hostname=_ALLOWED_HOST)
            try:
                tls.do_handshake()
                tls_ok = bool(tls.getpeercert())
            finally:
                tls.close()
        else:
            sock.close()
    except Exception as error:  # noqa: BLE001 - acceptance surfaces the reason
        print("   (allowed TLS error: %r)" % (error,))
        sock.close()
    check("allowed: allowlisted host tunnels and completes a real TLS handshake", tls_ok)

    # 2. DENIED: a non-allowlisted authority is refused 403.
    sock, headers = _attempt(port, b"example.com:443")
    sock.close()
    check("denied: non-allowlisted authority is 403", b"403 Forbidden" in headers)

    # 3. REBIND: an allowlisted host resolving to a private address is refused 403.
    sock, headers = _attempt(port, b"rebind.test:443")
    sock.close()
    check("rebind: private DNS answer is refused 403 before dial", b"403 Forbidden" in headers)

    return failures


def main(argv=None):
    parser = argparse.ArgumentParser(prog="neev_provider_egress_acceptance")
    parser.add_argument("--allow-skip", action="store_true",
                        help="exit 0 when the acceptance is not opted into / no network")
    args = parser.parse_args(argv)
    if os.environ.get("NEEV_PROVIDER_EGRESS_ACCEPTANCE") != "1":
        print("UNMET: set NEEV_PROVIDER_EGRESS_ACCEPTANCE=1 (needs outbound network)")
        return 0 if args.allow_skip else 2
    thread = _ProxyThread(_acceptance_policy())
    thread.start()
    try:
        port = thread.port()
        failures = run(port)
    finally:
        thread.shutdown()
    print("RESULT:", "ACCEPTED" if not failures else "FAILED %d" % len(failures))
    return 0 if not failures else 1


if __name__ == "__main__":
    raise SystemExit(main())
