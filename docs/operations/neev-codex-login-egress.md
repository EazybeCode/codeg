# Neev Codex login egress proxy

## Accepted component contract

`scripts/operations/neev_codex_login_egress.py` is a fixed-destination HTTP
CONNECT proxy candidate for the managed Codex login path. It listens on
`0.0.0.0:3128` and accepts only the exact, case-sensitive authority
`auth.openai.com:443` over HTTP/1.1 CONNECT. It has no destination, port, timeout,
or capacity configuration and does not read policy from the environment.

The request header is complete and bodyless, no larger than 8192 bytes, and uses
CRLF framing. `Host` is optional because the CONNECT authority is sufficient; if
present it must exactly match `auth.openai.com:443`. The only other accepted
headers are one printable-ASCII `User-Agent` and one `Proxy-Connection` whose
value is exactly `keep-alive` or `Keep-Alive`. Header names are case-insensitive.
Duplicate, folded, authorization, transfer, content-length, extension, malformed,
and post-delimiter input is rejected before resolution.

The fixed runtime bounds are:

- 16 admitted connections, including clients still reading a header or connecting
- 16 DNS results and at most 16 still-running resolver tasks
- 16,384 bytes per tunnel read/write chunk
- 4 MiB transferred in each direction per tunnel
- 5 seconds for the request header
- 5 seconds for DNS
- 10 seconds for connection establishment
- 30 seconds without tunnel activity
- 600 seconds total connection lifetime

Admission does not queue excess clients. Resolver timeouts do not free a resolver
slot while the underlying asyncio resolver task is still running, preventing
repeated timeouts from creating an unbounded collection of resolver work.

## Address validation and DNS pinning

The proxy validates the complete DNS answer before opening a connection. Empty or
oversized answers, malformed or non-canonical numeric addresses, zone identifiers,
IPv4 aliases, IPv4-mapped IPv6, and every non-global, private, loopback,
link-local, multicast, reserved, or unspecified address reject the entire answer.
A mixed public/private answer is never partially accepted.

Canonical global IPv6 answers may coexist in the checked collection but are never
dialed, and an answer without a usable global IPv4 address fails closed. The proxy
passes only a checked numeric IPv4 address and port 443 to
`asyncio.open_connection` with `AF_INET`. It never gives that API the hostname, so
there is no second resolution between policy validation and dialing.

## Opaque tunnel and failure behavior

After the numeric connection succeeds, the proxy writes one 200 response and
relays the remaining client bytes and upstream bytes with backpressure. Bytes that
arrived after the CONNECT delimiter remain in the asyncio reader and become the
first opaque tunnel bytes. TLS remains end-to-end between the unmodified client
and provider; the proxy does not terminate TLS, inspect encrypted hostnames or
content, or handle credentials.

Before establishment, responses are fixed and empty-bodied: 400 for invalid or
overflowing input, 403 for denied policy, 502 for unavailable resolution or
connection, 504 for a deadline, and 503 for capacity or shutdown. Error responses
include `Content-Length: 0` and `Connection: close`. After a 200 has been written,
any timeout, limit, I/O failure, cancellation, or EOF closes and reaps both sides
without injecting another HTTP response into TLS.

The component emits no logs or telemetry, writes no files, and never includes
request bytes, addresses, authentication data, or exception diagnostics in its
wire errors.

## Deployment boundary and remaining gates

This proxy is not an enforced sandbox network by itself. Configuring a client to
use it does not prevent bypass, and binding `0.0.0.0` does not authorize exposing
it publicly. Any eventual deployment must keep it private and separately enforce
a dedicated network that blocks direct egress, host loopback, metadata, private
and link-local ranges, IPv6 bypass, other sandboxes, and application/database
networks. Firewall policy and proxy-bypass testing remain unaccepted gates.

Exact compatibility with the pinned, unmodified official Codex client is also an
unaccepted gate. The single login destination is a conservative candidate, not a
claim that it covers future login or API behavior; a missing destination must fail
closed and receive explicit profile review. No provider client, real provider
request, service, container, firewall, or cloud resource was started or changed by
this component task.

Credential custody, per-member and per-organization connection selection with no
operator or teammate fallback, supervisor control, application integration,
restart behavior, UI flows, prompt execution, and `runtimeEnabled:true` are
separate acceptance gates. This component does not weaken or satisfy them.

Context: [org-memory: wiki/engineering/workos.md]. That page describes WorkOS as
the shared agent workspace and calls out network-exposed agent execution as a
security boundary; current code and this stricter proxy contract remain
authoritative.
