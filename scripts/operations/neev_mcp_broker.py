#!/usr/bin/env python3
"""Per-run broker for an organization's shared, authenticated MCP integrations.

The agent never holds an integration's credential. Its MCP config names only
`neev_mcp_bridge.py <key>`, which relays stdio to a socket this broker owns. The
broker runs as a DIFFERENT uid in its own pid+mount namespace, inside the run's
network namespace, so:

  * the credential is never in the agent's files, argv, environment or /proc
    view (a different uid in a different pid namespace cannot be read);
  * the broker's only way out is the run's slot CONNECT proxy, whose exact
    allowlist holds the provider's hosts plus the admin-configured hosts of the
    enabled integrations (resolved and dialled by public IP, DNS-rebind safe);
  * the listening sockets are abstract (`\\0neev-mcp/<key>`), which the kernel
    scopes to the network namespace; the peer must be the run's lane uid.

Per integration:

  * `remote`  — this process is the MCP client of the remote server: Streamable
    HTTP (JSON or SSE replies, `Mcp-Session-Id`), falling back to the legacy
    HTTP+SSE transport when the URL refuses a POST at initialize (404/405). The
    stored secret headers are added to every upstream request and never sent
    anywhere else (an SSE `endpoint` on another origin is refused).
  * `package` — a pinned npm package from the image (`/opt/neev/mcp`) is started
    per connection with exactly its declared secret env plus the proxy env
    (`NODE_USE_ENV_PROXY=1` makes Node's fetch/https use the CONNECT proxy).

Failures reach the agent as JSON-RPC errors with a fixed state in
`error.data.neevState` — `auth_failed`, `not_found`, `egress_denied`,
`unreachable`, `upstream_error`, `protocol_error`, `bad_request`,
`package_unavailable`, `egress_not_configured` — never a credential and never
the upstream's body.

The bundle (servers + secrets) arrives as ONE line on stdin from the launcher.
"""

import http.client
import json
import os
import re
import signal
import socket
import ssl
import struct
import subprocess
import sys
import threading
import time
from collections import namedtuple
from urllib.parse import urljoin, urlsplit

# `python3 -I` leaves the script's own directory off sys.path; the sibling
# modules live beside this file in the same root-owned directory.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

BUNDLE_VERSION = 1
MAX_BUNDLE_BYTES = 256 * 1024
MAX_SERVERS = 32
MAX_LINE_BYTES = 8 * 1024 * 1024
# Per integration, concurrent agent connections; across the broker, concurrent
# upstream tunnels. The slot proxy admits 16 tunnels for the provider AND the
# integrations together, so the broker keeps well under it.
MAX_CONNECTIONS_PER_KEY = 2
MAX_UPSTREAM_TUNNELS = 6
UPSTREAM_WAIT_SECONDS = 60
HTTP_TIMEOUT_SECONDS = 120
SSE_ENDPOINT_TIMEOUT_SECONDS = 20
PROTOCOL_VERSION_HEADER = "MCP-Protocol-Version"

KEY_RE = re.compile(r"[a-z0-9][a-z0-9_-]{0,23}\Z")
HEADER_NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9-]{0,63}\Z")
ENV_NAME_RE = re.compile(r"[A-Z_][A-Z0-9_]{0,63}\Z")
_FORBIDDEN_HEADER_PREFIXES = ("proxy-", "sec-")
_FORBIDDEN_HEADERS = frozenset((
    "host", "content-length", "content-type", "accept", "connection", "transfer-encoding",
    "te", "upgrade", "proxy-authorization", "proxy-connection", "keep-alive", "cookie",
    "mcp-session-id", "mcp-protocol-version", "last-event-id",
))
_MAX_SECRET_VALUE = 8192

# Where the image installs the pinned packages (deploy/mcp-packages). Never taken
# from the bundle: the bundle names a package id, this table names the file.
NODE = "/opt/neev/node/bin/node"
MCP_ROOT = "/opt/neev/mcp/node_modules"

PackageSpec = namedtuple("PackageSpec", ("argv", "env_names"))
# Exactly ONE secret env name per package, the one the package reads its token
# from. Never an admin-chosen name: `NODE_OPTIONS`, `LD_PRELOAD`,
# `NODE_TLS_REJECT_UNAUTHORIZED`, proxy variables and the like can never reach a
# child this way. (HubSpot reads `PRIVATE_APP_ACCESS_TOKEN` first; the server's
# import maps the legacy `HUBSPOT_ACCESS_TOKEN` onto it.)
PACKAGES = {
    "hubspot": PackageSpec(
        argv=(NODE, MCP_ROOT + "/@hubspot/mcp-server/dist/index.js"),
        env_names=("PRIVATE_APP_ACCESS_TOKEN",),
    ),
    "notion": PackageSpec(
        argv=(NODE, MCP_ROOT + "/@suekou/mcp-notion-server/build/index.js"),
        env_names=("NOTION_API_TOKEN",),
    ),
}


# A disposable ACCEPTANCE fixture (the release kit's stdio stub), never present in
# the production image. Usable only when the launcher passes `--accept-fixture`
# AND the file is root-owned and not group/world-writable in a root-owned,
# non-writable directory — so neither a flag nor a planted file alone enables it.
ACCEPT_FIXTURE_DIR = "/opt/neev-accept"
ACCEPT_FIXTURE_PACKAGE = "accept-stub"
ACCEPT_FIXTURE_ENV = "NEEV_ACCEPT_TOKEN"


def accept_fixture_spec(directory=ACCEPT_FIXTURE_DIR, lstat=os.lstat):
    import stat

    script = os.path.join(directory, "stub_mcp_server.py")
    for path, want_dir in ((directory, True), (script, False)):
        try:
            info = lstat(path)
        except OSError:
            return None
        if (info.st_uid != 0 or info.st_mode & 0o022 or stat.S_ISLNK(info.st_mode)
                or stat.S_ISDIR(info.st_mode) != want_dir):
            return None
    return PackageSpec(argv=("/usr/bin/python3", "-I", script), env_names=(ACCEPT_FIXTURE_ENV,))


class IntegrationError(Exception):
    STATES = frozenset((
        "invalid", "auth_failed", "forbidden", "session_expired", "busy", "not_found",
        "connection_lost", "egress_denied", "unreachable",
        "upstream_error", "protocol_error", "package_unavailable", "egress_not_configured",
        "broker_unavailable", "bad_request",
    ))

    def __init__(self, state, status=None):
        if state not in self.STATES:
            state = "protocol_error"
        super().__init__(state)
        self.state = state
        self.status = status


_MESSAGES = {
    "auth_failed": "the service rejected the stored credentials; an admin must update them",
    "forbidden": "the credentials were accepted but this access is not allowed (403)",
    "session_expired": "the service ended the session and it could not be re-established",
    "connection_lost": "the connection was lost after the request was sent; the call may "
                       "have run and was NOT retried",
    "busy": "too many integration connections are open in this run",
    "not_found": "the service endpoint was not found; an admin must check its URL",
    "egress_denied": "the network policy refused this host",
    "unreachable": "the service could not be reached",
    "upstream_error": "the service failed",
    "protocol_error": "the service answered in a way this broker does not understand",
    "bad_request": "the service rejected the request as malformed (400); an admin should "
                   "check the stored credentials and URL",
    "package_unavailable": "the integration package is not installed in this deployment",
    "egress_not_configured": "this run has no network path for integrations",
    "broker_unavailable": "the integration broker is not running for this run",
    "invalid": "the integration is misconfigured",
}


def error_reply(message_id, key, error):
    status = " (HTTP %d)" % error.status if error.status else ""
    return {
        "jsonrpc": "2.0",
        "id": message_id,
        "error": {
            "code": -32001,
            "message": "Integration %s unavailable: %s%s — %s" % (
                key, error.state, status, _MESSAGES.get(error.state, "")),
            "data": {"neevState": error.state, "httpStatus": error.status},
        },
    }


# --------------------------------------------------------------------------- #
# Bundle
# --------------------------------------------------------------------------- #


def _host_ok(host):
    import neev_provider_endpoints as endpoints

    try:
        endpoints.integration_endpoints([host])
    except endpoints.EndpointError:
        return False
    return True


def _secret_map(value, name_re, kind):
    if not isinstance(value, dict) or len(value) > 16:
        raise IntegrationError("invalid")
    out = {}
    for name, secret in value.items():
        if not isinstance(name, str) or not name_re.match(name):
            raise IntegrationError("invalid")
        if kind == "header" and (name.lower() in _FORBIDDEN_HEADERS
                                 or name.lower().startswith(_FORBIDDEN_HEADER_PREFIXES)):
            raise IntegrationError("invalid")
        if (not isinstance(secret, str) or not secret or len(secret) > _MAX_SECRET_VALUE
                or any(ord(c) < 0x20 or ord(c) > 0x7E for c in secret)):
            raise IntegrationError("invalid")
        out[name] = secret
    return out


def parse_bundle(raw, packages=None):
    """Validated list of server dicts from the launcher's bundle line."""
    packages = PACKAGES if packages is None else packages
    if not isinstance(raw, (bytes, bytearray)) or len(raw) > MAX_BUNDLE_BYTES:
        raise IntegrationError("invalid")
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        raise IntegrationError("invalid") from None
    if (not isinstance(value, dict) or value.get("neevIntegrations") != BUNDLE_VERSION
            or not isinstance(value.get("servers"), list)
            or len(value["servers"]) > MAX_SERVERS):
        raise IntegrationError("invalid")
    servers, keys = [], set()
    for server in value["servers"]:
        if not isinstance(server, dict):
            raise IntegrationError("invalid")
        key = server.get("key")
        if not isinstance(key, str) or not KEY_RE.match(key) or key in keys:
            raise IntegrationError("invalid")
        keys.add(key)
        hosts = server.get("hosts")
        if (not isinstance(hosts, list) or not 1 <= len(hosts) <= 4
                or not all(isinstance(h, str) and _host_ok(h) for h in hosts)):
            raise IntegrationError("invalid")
        env = _secret_map(server.get("env", {}), ENV_NAME_RE, "env")
        headers = _secret_map(server.get("headers", {}), HEADER_NAME_RE, "header")
        transport = server.get("transport")
        clean = {"key": key, "transport": transport, "hosts": list(hosts),
                 "env": env, "headers": headers}
        if transport == "remote":
            url = server.get("url")
            try:
                parts = urlsplit(url) if isinstance(url, str) and len(url) <= 2048 else None
                port = parts.port if parts is not None else None
            except ValueError:  # e.g. a non-numeric or out-of-range port
                raise IntegrationError("invalid") from None
            if (parts is None or parts.scheme != "https" or parts.hostname not in hosts
                    or parts.username or parts.password or parts.query or parts.fragment
                    or port not in (None, 443) or "package" in server or env):
                raise IntegrationError("invalid")
            clean["url"] = url
        elif transport == "package":
            package = server.get("package")
            spec = packages.get(package) if isinstance(package, str) else None
            if spec is None or headers or "url" in server:
                raise IntegrationError("invalid")
            if not set(env) <= set(spec.env_names):
                raise IntegrationError("invalid")
            clean["package"] = package
        else:
            raise IntegrationError("invalid")
        servers.append(clean)
    return servers


def bundle_hosts(servers):
    """Every admin-configured host, once, in order: the slot proxy's extra hosts."""
    seen = []
    for server in servers:
        for host in server["hosts"]:
            if host not in seen:
                seen.append(host)
    return tuple(seen)


def secret_values(servers):
    values = []
    for server in servers:
        for secret in list(server["env"].values()) + list(server["headers"].values()):
            values.append(secret)
            # A header like "Bearer <token>" also hides the bare token.
            values.extend(part for part in secret.split() if len(part) >= 8)
    return sorted(set(values), key=len, reverse=True)


def redact(text, servers):
    for value in secret_values(servers):
        if len(value) >= 4:
            text = text.replace(value, "[redacted]")
    return text


# --------------------------------------------------------------------------- #
# Sockets and peers
# --------------------------------------------------------------------------- #


def socket_address(key, socket_dir=None):
    """Abstract (netns-scoped) in production; a path under `socket_dir` in tests."""
    if socket_dir:
        return os.path.join(socket_dir, key + ".sock")
    return "\0neev-mcp/" + key


def peer_allowed(expected_uid, actual_uid):
    if expected_uid is None:
        return True
    return actual_uid is not None and actual_uid == expected_uid


def peer_uid(conn):
    option = getattr(socket, "SO_PEERCRED", None)
    if option is None:
        return None
    try:
        raw = conn.getsockopt(socket.SOL_SOCKET, option, struct.calcsize("3i"))
    except OSError:
        return None
    _pid, uid, _gid = struct.unpack("3i", raw)
    return uid


# --------------------------------------------------------------------------- #
# Upstream connections (CONNECT proxy only)
# --------------------------------------------------------------------------- #


def proxy_connector(proxy_url, context=None):
    """A connector that tunnels to `host:443` through the CONNECT proxy, or
    IntegrationError when the run has no proxy: there is no direct path."""
    if not proxy_url:
        raise IntegrationError("egress_not_configured")
    parts = urlsplit(proxy_url)
    if parts.scheme != "http" or not parts.hostname or not parts.port:
        raise IntegrationError("egress_not_configured")
    tls = context or ssl.create_default_context()

    def connect(host, port=443):
        return _TunnelledHTTPS(parts.hostname, parts.port, host, port, tls)

    return connect


class _TunnelledHTTPS(http.client.HTTPSConnection):
    """HTTPS to `host:port` through an HTTP/1.1 CONNECT proxy.

    `http.client.set_tunnel` sends `CONNECT ... HTTP/1.0` on the Python versions
    we run, which the slot proxy's strict parser refuses (it admits exactly
    `HTTP/1.1` with an optional matching `Host`). So the tunnel is opened here,
    explicitly, and TLS is then started with the TARGET's name for SNI and
    certificate verification."""

    def __init__(self, proxy_host, proxy_port, host, port, context):
        super().__init__(host, port, timeout=HTTP_TIMEOUT_SECONDS, context=context)
        self._proxy = (proxy_host, proxy_port)
        self._target = (host, port)
        self._tls = context

    def connect(self):
        raw = socket.create_connection(self._proxy, timeout=HTTP_TIMEOUT_SECONDS)
        try:
            authority = "%s:%d" % self._target
            raw.sendall(("CONNECT %s HTTP/1.1\r\nHost: %s\r\n\r\n"
                         % (authority, authority)).encode("ascii"))
            head = b""
            while b"\r\n\r\n" not in head:
                chunk = raw.recv(4096)
                if not chunk:
                    raise OSError("Tunnel connection failed: 502 closed")
                head += chunk
                if len(head) > 16384:
                    raise OSError("Tunnel connection failed: 502 oversized")
            status_line = head.split(b"\r\n", 1)[0].decode("latin-1")
            match = re.match(r"HTTP/1\.[01] (\d{3})", status_line)
            if not match:
                raise OSError("Tunnel connection failed: 502 malformed")
            if match.group(1) != "200":
                raise OSError("Tunnel connection failed: %s" % match.group(1))
            self.sock = self._tls.wrap_socket(raw, server_hostname=self._target[0])
        except BaseException:
            raw.close()
            raise


def _classify_os_error(error):
    text = str(error)
    match = re.search(r"Tunnel connection failed: (\d{3})", text)
    if match:
        code = int(match.group(1))
        if code == 403:
            return IntegrationError("egress_denied", code)
        return IntegrationError("unreachable", code)
    return IntegrationError("unreachable")


def _classify_status(status):
    # Redirects are never followed (a redirect would carry the secret headers to
    # wherever it points): a 3xx is a protocol error, not a hop.
    if 300 <= status < 400:
        return IntegrationError("protocol_error", status)
    if status == 401:
        return IntegrationError("auth_failed", status)
    if status == 403:
        return IntegrationError("forbidden", status)
    if status in (404, 405, 410):
        return IntegrationError("not_found", status)
    if status >= 500:
        return IntegrationError("upstream_error", status)
    if 400 <= status < 500:
        return IntegrationError("bad_request", status)
    return IntegrationError("protocol_error", status)


def _invalid_token_challenge(response):
    """A `WWW-Authenticate: Bearer error="invalid_token"` reply: the credential
    was refused, whatever the status (GitHub answers 400 to a token it cannot
    parse)."""
    challenge = response.getheader("WWW-Authenticate") or ""
    return bool(re.search(r'(?i)\bbearer\b.*\berror\s*=\s*"?invalid_token\b', challenge))


def _iter_sse(response):
    """Yield (event, data) from an SSE response, bounded per event."""
    event, data, size = "message", [], 0
    while True:
        line = response.readline(MAX_LINE_BYTES + 1)
        if not line:
            if data:
                yield event, "\n".join(data)
            return
        if len(line) > MAX_LINE_BYTES:
            raise IntegrationError("protocol_error")
        line = line.decode("utf-8", "replace").rstrip("\r\n")
        if not line:
            if data:
                yield event, "\n".join(data)
            event, data, size = "message", [], 0
            continue
        if line.startswith(":"):
            continue
        field, _, value = line.partition(":")
        value = value[1:] if value.startswith(" ") else value
        if field == "event":
            event = value
        elif field == "data":
            size += len(value)
            if size > MAX_LINE_BYTES:
                raise IntegrationError("protocol_error")
            data.append(value)


def same_origin_endpoint(base_url, endpoint):
    """The absolute POST URL a legacy SSE server announced, if it stays on the
    stream's own https origin (so secret headers never go anywhere else)."""
    if not isinstance(endpoint, str) or not endpoint or len(endpoint) > 2048:
        return None
    if endpoint.startswith("//"):
        return None
    absolute = urljoin(base_url, endpoint)
    base, target = urlsplit(base_url), urlsplit(absolute)
    if (target.scheme != "https" or target.hostname != base.hostname
            or (target.port or 443) != (base.port or 443) or target.username):
        return None
    return absolute


def _path(url):
    parts = urlsplit(url)
    return (parts.path or "/") + ("?" + parts.query if parts.query else "")


_TUNNELS = threading.BoundedSemaphore(MAX_UPSTREAM_TUNNELS)


def _id_key(message_id):
    """A hashable, type-preserving key for a JSON-RPC id (1 and "1" differ)."""
    return (type(message_id).__name__, json.dumps(message_id))
_REINIT_ID = "neev-broker-reinit"


class _Leased:
    """An upstream connection holding one of the broker's tunnel slots until it
    is closed (once)."""

    def __init__(self, conn, slots):
        self._conn = conn
        self._slots = slots
        self._released = False

    def __getattr__(self, name):
        return getattr(self._conn, name)

    def close(self):
        try:
            self._conn.close()
        finally:
            if not self._released:
                self._released = True
                self._slots.release()


class RemoteUpstream:
    """One MCP client session to a remote server, on behalf of one agent
    connection. `exchange(message)` returns the messages to hand back (the
    reply last); legacy SSE replies are delivered via `on_message`.

    A session the server drops (a 404 on a request that carries a session id, or
    a legacy SSE stream that ends) is re-established ONCE — the agent's own
    `initialize` is replayed under an internal id and its reply swallowed — and
    the request retried. Only if that fails is it `session_expired`. A 404 at
    initialize, after the SSE fallback, is `not_found`."""

    def __init__(self, url, headers, connector, on_message=None, tunnels=None, key="integration"):
        self.key = key
        # Requests sent on the legacy SSE session whose reply has not arrived.
        # If the stream ends first, each is answered ONCE with `connection_lost`
        # (never resent: the call may already have run).
        # id -> the legacy stream generation it was sent on: a stream that ends
        # answers only ITS ids, never one already re-sent on a newer stream.
        self._pending = {}
        self._pending_lock = threading.Lock()
        self._generation = 0
        self.url = url
        self.host = urlsplit(url).hostname
        self.headers = dict(headers)
        self.connector = connector
        self.on_message = on_message
        self.tunnels = tunnels or _TUNNELS
        self.session_id = None
        self.protocol_version = None
        self.mode = "auto"  # -> "streamable" | "sse"
        self._sse_post_url = None
        self._sse_conn = None
        self._sse_alive = False
        self._init = None
        self._initialized = False
        self._reinit_reply = threading.Event()

    def _connect(self):
        if not self.tunnels.acquire(timeout=UPSTREAM_WAIT_SECONDS):
            raise IntegrationError("busy")
        try:
            conn = self.connector(self.host, 443)
        except IntegrationError:
            self.tunnels.release()
            raise
        except OSError as error:
            self.tunnels.release()
            raise _classify_os_error(error) from None
        return _Leased(conn, self.tunnels)

    def _request(self, method, url, body=None, accept="application/json, text/event-stream"):
        headers = dict(self.headers)
        headers["Accept"] = accept
        if body is not None:
            headers["Content-Type"] = "application/json"
        if self.session_id:
            headers["Mcp-Session-Id"] = self.session_id
        if self.protocol_version:
            headers[PROTOCOL_VERSION_HEADER] = self.protocol_version
        conn = self._connect()
        try:
            conn.request(method, _path(url), body=body, headers=headers)
        except IntegrationError:
            conn.close()
            raise
        except (OSError, http.client.HTTPException, UnicodeError) as error:
            conn.close()
            raise _classify_os_error(error) from None
        try:
            return conn, conn.getresponse()
        except (OSError, http.client.HTTPException) as error:
            conn.close()
            # The request was SENT: whether it ran is unknown, so it is never
            # retried and never reported as plain unreachability.
            if body is not None and b'"id"' in body:
                raise IntegrationError("connection_lost") from None
            raise _classify_os_error(error) from None

    def exchange(self, message):
        method = message.get("method")
        if method == "initialize":
            self._init = message
        elif method == "notifications/initialized":
            self._initialized = True
        if self.mode == "sse":
            self._sse_send(message)
            return []
        try:
            return self._post(message)
        except IntegrationError as error:
            if (error.status == 404 and self.mode == "streamable" and self.session_id
                    and method != "initialize"):
                self._reinitialize()
                try:
                    return self._post(message)
                except IntegrationError as retry:
                    if retry.status == 404:
                        raise IntegrationError("session_expired", 404) from None
                    raise
            raise

    def _post(self, message, internal=False):
        is_init = message.get("method") == "initialize"
        body = json.dumps(message).encode("utf-8")
        conn, response = self._request("POST", self.url, body)
        try:
            # Backwards compatibility (MCP spec): a 4xx to the initialize POST
            # means "try the legacy HTTP+SSE transport" — 400, 404 or 405 seen.
            # A 400 that names the token as invalid is a credential failure,
            # not a legacy server: no fallback.
            if is_init and response.status == 400 and _invalid_token_challenge(response):
                response.read(65536)
                raise IntegrationError("auth_failed", 400)
            if is_init and self.mode == "auto" and response.status in (400, 404, 405):
                response.read(65536)
                conn.close()
                self._open_sse(after_status=response.status)
                self._sse_post(message)
                return []
            if response.status >= 300:
                response.read(65536)
                raise _classify_status(response.status)
            if is_init:
                self.mode = "streamable"
                session = response.getheader("Mcp-Session-Id")
                if session and re.fullmatch(r"[\x21-\x7e]{1,256}", session):
                    self.session_id = session
            if response.status == 202 or "id" not in message:
                response.read(65536)
                return []
            ctype = (response.getheader("Content-Type") or "").split(";")[0].strip().lower()
            out = []
            if ctype == "text/event-stream":
                answered, total = False, 0
                try:
                    for _event, data in _iter_sse(response):
                        total += len(data)
                        if total > MAX_LINE_BYTES:
                            raise IntegrationError("protocol_error")
                        try:
                            item = json.loads(data)
                        except ValueError:
                            continue
                        out.append(item)
                        if isinstance(item, dict) and item.get("id") == message["id"] and (
                                "result" in item or "error" in item):
                            answered = True
                            break
                except (OSError, http.client.HTTPException):
                    answered = False
                if not answered and not internal:
                    # The stream ended before the reply: deliver what arrived,
                    # then answer the request once — the call may have run.
                    for item in out:
                        if self.on_message:
                            self.on_message(item)
                    raise IntegrationError("connection_lost")
            elif ctype == "application/json":
                raw = response.read(MAX_LINE_BYTES + 1)
                if len(raw) > MAX_LINE_BYTES:
                    raise IntegrationError("protocol_error")
                try:
                    item = json.loads(raw)
                except ValueError:
                    raise IntegrationError("protocol_error") from None
                out.extend(item if isinstance(item, list) else [item])
            else:
                raise IntegrationError("protocol_error", response.status)
            if is_init:
                for item in out:
                    if isinstance(item, dict) and isinstance(item.get("result"), dict):
                        version = item["result"].get("protocolVersion")
                        if isinstance(version, str) and re.fullmatch(r"[0-9-]{4,32}", version):
                            self.protocol_version = version
            return [] if internal else out
        finally:
            conn.close()

    def _reinitialize(self):
        """Replay the agent's initialize (internal id, reply swallowed) and its
        `initialized` notification on a fresh session."""
        if self._init is None:
            raise IntegrationError("session_expired")
        replay = dict(self._init, id=_REINIT_ID)
        if self.mode == "sse":
            self._close_sse()
            self._sse_post_url = None
            self._open_sse()
            self._reinit_reply.clear()
            self._sse_post(replay)
            if not self._reinit_reply.wait(SSE_ENDPOINT_TIMEOUT_SECONDS):
                raise IntegrationError("session_expired")
        else:
            self.session_id = None
            self._post(replay, internal=True)
        if self._initialized:
            notice = {"jsonrpc": "2.0", "method": "notifications/initialized"}
            if self.mode == "sse":
                self._sse_post(notice)
            else:
                self._post(notice, internal=True)

    # Legacy HTTP+SSE (2024-11-05): GET opens the stream, `endpoint` names the POST URL.
    def _open_sse(self, after_status=None):
        conn, response = self._request("GET", self.url, accept="text/event-stream")
        if response.status >= 300:
            status = response.status
            invalid_token = _invalid_token_challenge(response)
            response.read(65536)
            conn.close()
            # The initialize POST got 400 and the legacy GET is refused too
            # (400/401): no transport accepts these requests. With stored
            # credentials that is the credential (GitHub answers 400 to a token
            # it cannot parse), never "not an MCP server".
            if invalid_token or (after_status == 400 and status in (400, 401)
                                 and self.headers):
                raise IntegrationError("auth_failed", status)
            raise _classify_status(status)
        events = _iter_sse(response)
        deadline = time.monotonic() + SSE_ENDPOINT_TIMEOUT_SECONDS
        try:
            for event, data in events:
                if event == "endpoint":
                    target = same_origin_endpoint(self.url, data.strip())
                    if target is None:
                        raise IntegrationError("protocol_error")
                    self._sse_post_url = target
                    break
                if time.monotonic() > deadline:
                    break
        except IntegrationError:
            conn.close()
            raise
        if self._sse_post_url is None:
            conn.close()
            raise IntegrationError("protocol_error")
        self.mode = "sse"
        self._sse_conn = conn
        self._sse_alive = True
        self._generation += 1
        threading.Thread(target=self._pump_sse, args=(events, conn, self._generation),
                         daemon=True).start()

    def _pump_sse(self, events, conn, generation=0):
        try:
            for event, data in events:
                if event != "message":
                    continue
                try:
                    item = json.loads(data)
                except ValueError:
                    continue
                if isinstance(item, dict) and item.get("id") == _REINIT_ID:
                    self._reinit_reply.set()
                    continue
                if isinstance(item, dict) and "id" in item and (
                        "result" in item or "error" in item):
                    with self._pending_lock:
                        self._pending.pop(_id_key(item["id"]), None)
                if self.on_message:
                    self.on_message(item)
        except Exception:
            pass
        finally:
            if self._sse_conn is conn:
                self._sse_alive = False
            self._answer_lost(generation)

    def _answer_lost(self, generation):
        with self._pending_lock:
            lost = sorted(key for key, sent_on in self._pending.items() if sent_on == generation)
            for key in lost:
                del self._pending[key]
        for _kind, raw in lost:
            if self.on_message:
                self.on_message(error_reply(json.loads(raw), self.key,
                                            IntegrationError("connection_lost")))

    def _sse_send(self, message):
        """POST on the legacy session, re-establishing it once if the stream has
        ended or the server no longer knows the session (404)."""
        if self._sse_alive:
            try:
                self._sse_post(message)
                return
            except IntegrationError as error:
                if error.status != 404:
                    raise
        if message.get("method") == "initialize":
            raise IntegrationError("session_expired")
        self._reinitialize()
        try:
            self._sse_post(message)
        except IntegrationError as error:
            if error.status == 404:
                raise IntegrationError("session_expired", 404) from None
            raise

    def _sse_post(self, message):
        tracked = "id" in message and "method" in message and message["id"] != _REINIT_ID
        if tracked:
            with self._pending_lock:
                self._pending[_id_key(message["id"])] = self._generation
        try:
            self._sse_post_once(message)
        except IntegrationError:
            if tracked:  # the broker answers this one itself
                with self._pending_lock:
                    self._pending.pop(_id_key(message["id"]), None)
            raise

    def _sse_post_once(self, message):
        conn, response = self._request("POST", self._sse_post_url,
                                       json.dumps(message).encode("utf-8"),
                                       accept="application/json, text/event-stream")
        try:
            response.read(65536)
            if response.status >= 300:
                raise _classify_status(response.status)
        finally:
            conn.close()

    def _close_sse(self):
        conn, self._sse_conn, self._sse_alive = self._sse_conn, None, False
        if conn is not None:
            try:
                conn.close()
            except Exception:
                pass

    def close(self):
        self._close_sse()
        if self.session_id and self.mode == "streamable":
            # Best effort: tell the server the session is over.
            try:
                conn, response = self._request("DELETE", self.url)
                response.read(65536)
                conn.close()
            except Exception:
                pass


# --------------------------------------------------------------------------- #
# Broker
# --------------------------------------------------------------------------- #


class _Writer:
    """Agent-bound writer. Every line is redacted of every secret value first:
    a tool result or a package's output must not hand a credential back."""

    def __init__(self, conn, secrets=()):
        self.conn = conn
        self.lock = threading.Lock()
        self.secrets = [value.encode("utf-8") for value in secrets if len(value) >= 4]

    def redact(self, data):
        for value in self.secrets:
            if value in data:
                data = data.replace(value, b"[redacted]")
        return data

    def write(self, item):
        data = (json.dumps(item, separators=(",", ":")) + "\n").encode("utf-8")
        self.write_raw(data)

    def write_raw(self, data):
        data = self.redact(data)
        with self.lock:
            self.conn.sendall(data)


def _read_lines(conn):
    buffer = b""
    while True:
        chunk = conn.recv(65536)
        if not chunk:
            if buffer.strip():
                yield buffer
            return
        buffer += chunk
        while b"\n" in buffer:
            line, buffer = buffer.split(b"\n", 1)
            if line.strip():
                yield line
        if len(buffer) > MAX_LINE_BYTES:
            raise IntegrationError("protocol_error")


class Broker:
    def __init__(self, servers, *, socket_dir=None, peer_uid=None, connector=None,
                 proxy=None, packages=None, home=None, log=None):
        self.servers = {server["key"]: server for server in servers}
        self.all_servers = list(servers)
        self.socket_dir = socket_dir
        self.peer_uid = peer_uid
        self.proxy = proxy
        self.connector = connector
        self.packages = PACKAGES if packages is None else packages
        self.home = home or "/nonexistent"
        self.log = log or (lambda text: None)
        self._listeners = []
        self._stopping = threading.Event()
        self._active = {}
        self._children = set()
        self._lock = threading.Lock()

    # -- lifecycle --
    def start(self):
        for key in self.servers:
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.bind(socket_address(key, self.socket_dir))
            listener.listen(MAX_CONNECTIONS_PER_KEY)
            self._listeners.append(listener)
            threading.Thread(target=self._accept, args=(key, listener), daemon=True).start()

    def stop(self):
        self._stopping.set()
        for listener in self._listeners:
            try:
                listener.close()
            except OSError:
                pass
        with self._lock:
            children = list(self._children)
        for child in children:
            _kill(child)

    def wait(self):
        self._stopping.wait()

    # -- connections --
    def _accept(self, key, listener):
        while not self._stopping.is_set():
            try:
                conn, _ = listener.accept()
            except OSError:
                return
            if not peer_allowed(self.peer_uid, peer_uid(conn) if self.peer_uid is not None else None):
                self.log("NEEV-MCP-BROKER key=%s refused=peer" % key)
                conn.close()
                continue
            with self._lock:
                if self._active.get(key, 0) >= MAX_CONNECTIONS_PER_KEY:
                    conn.close()
                    continue
                self._active[key] = self._active.get(key, 0) + 1
            threading.Thread(target=self._serve, args=(key, conn), daemon=True).start()

    def _serve(self, key, conn):
        server = self.servers[key]
        try:
            if server["transport"] == "remote":
                self._serve_remote(key, server, conn)
            else:
                self._serve_package(key, server, conn)
        except Exception as error:  # never let a secret-bearing traceback out
            self.log("NEEV-MCP-BROKER key=%s ended=%s" % (key, type(error).__name__))
        finally:
            with self._lock:
                self._active[key] -= 1
            try:
                conn.close()
            except OSError:
                pass

    def _connector(self):
        if self.connector is not None:
            return self.connector
        return proxy_connector(self.proxy)

    def _serve_remote(self, key, server, conn):
        writer = _Writer(conn, secret_values(self.all_servers))
        closed = threading.Event()

        def on_message(item):
            if item is None:
                closed.set()
                return
            try:
                writer.write(item)
            except OSError:
                closed.set()

        upstream = None
        failure = None
        try:
            upstream = RemoteUpstream(server["url"], server["headers"], self._connector(),
                                      on_message=on_message, key=key)
        except IntegrationError as error:
            failure = error
        try:
            for line in _read_lines(conn):
                try:
                    message = json.loads(line)
                except ValueError:
                    continue
                if not isinstance(message, dict):
                    continue
                is_request = "id" in message and "method" in message
                if failure is not None:
                    if is_request:
                        writer.write(error_reply(message["id"], key, failure))
                    continue
                try:
                    for item in upstream.exchange(message):
                        writer.write(item)
                except IntegrationError as error:
                    self.log("NEEV-MCP-BROKER key=%s state=%s status=%s"
                             % (key, error.state, error.status))
                    if message.get("method") == "initialize":
                        failure = error
                    if is_request:
                        writer.write(error_reply(message["id"], key, error))
        finally:
            if upstream is not None:
                upstream.close()

    def package_env(self, server, spec):
        """The package's declared secret env FIRST, then the fixed environment on
        top, so no secret can replace PATH, HOME, TMPDIR or the proxy settings.
        TMPDIR/HOME are the run's 0700 broker directory (no shared /tmp use)."""
        env = {name: value for name, value in server["env"].items()
               if name in spec.env_names}
        env.update({"PATH": "/opt/neev/node/bin:/usr/local/bin:/usr/bin:/bin",
                    "HOME": self.home, "TMPDIR": self.home, "LANG": "C.UTF-8",
                    "NODE_USE_ENV_PROXY": "1"})
        if self.proxy:
            for name in ("HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"):
                env[name] = self.proxy
            env["NO_PROXY"] = env["no_proxy"] = ""
        return env

    def _serve_package(self, key, server, conn):
        writer = _Writer(conn, secret_values(self.all_servers))
        spec = self.packages.get(server["package"])
        if not self.proxy:
            self._refuse_all(key, conn, writer, IntegrationError("egress_not_configured"))
            return
        if spec is None or not all(os.path.exists(part) for part in spec.argv[:2] if os.path.isabs(part)):
            self._refuse_all(key, conn, writer, IntegrationError("package_unavailable"))
            return
        try:
            child = subprocess.Popen(
                list(spec.argv), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL, env=self.package_env(server, spec),
                cwd=self.home if os.path.isdir(self.home) else "/", close_fds=True,
                start_new_session=True)
        except OSError:
            self._refuse_all(key, conn, writer, IntegrationError("package_unavailable"))
            return
        with self._lock:
            self._children.add(child)

        def pump_out():
            try:
                while True:
                    line = child.stdout.readline(MAX_LINE_BYTES + 1)
                    if not line:
                        break
                    if len(line) > MAX_LINE_BYTES:
                        # Oversized: drop the whole line rather than split it
                        # into two invalid messages.
                        while line and not line.endswith(b"\n"):
                            line = child.stdout.readline(MAX_LINE_BYTES + 1)
                        continue
                    writer.write_raw(line if line.endswith(b"\n") else line + b"\n")
            except OSError:
                pass
            finally:
                try:
                    conn.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

        threading.Thread(target=pump_out, daemon=True).start()
        try:
            for line in _read_lines(conn):
                child.stdin.write(line + b"\n")
                child.stdin.flush()
        except (OSError, ValueError, IntegrationError):
            pass
        finally:
            _kill(child)
            with self._lock:
                self._children.discard(child)

    def _refuse_all(self, key, conn, writer, error):
        self.log("NEEV-MCP-BROKER key=%s state=%s" % (key, error.state))
        for line in _read_lines(conn):
            try:
                message = json.loads(line)
            except ValueError:
                continue
            if isinstance(message, dict) and "id" in message and "method" in message:
                writer.write(error_reply(message["id"], key, error))


def _kill(child):
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except (OSError, ProcessLookupError):
        try:
            child.kill()
        except OSError:
            pass
    try:
        child.wait(timeout=5)
    except Exception:
        pass


PR_SET_DUMPABLE = 4


def harden_process(prctl=None):
    """Not dumpable: package children (same uid, same pid namespace) cannot read
    the broker's memory — which holds every integration's secrets — through
    /proc/<pid>/mem or ptrace, whatever Yama's setting. Linux only; elsewhere a
    no-op returning False."""
    if prctl is None:
        if not sys.platform.startswith("linux"):
            return False
        import ctypes

        prctl = ctypes.CDLL(None, use_errno=True).prctl
    return prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) == 0


def lifeline_eof(stream):
    """Block until the launcher's end of the bundle pipe closes."""
    while True:
        try:
            chunk = stream.read(4096)
        except OSError:
            return
        if not chunk:
            return


def main(argv=None):  # pragma: no cover - process entry (sandboxed by the launcher)
    import argparse

    parser = argparse.ArgumentParser(prog="neev_mcp_broker")
    parser.add_argument("--peer-uid", type=int, required=True)
    parser.add_argument("--home", required=True)
    parser.add_argument("--accept-fixture", action="store_true", default=False)
    args = parser.parse_args(argv)
    # Everything the broker or a package child creates is private to this uid
    # (design review A2'(a): shared /tmp entries must not be lane-readable).
    os.umask(0o077)
    harden_process()
    packages = dict(PACKAGES)
    if args.accept_fixture:
        fixture = accept_fixture_spec()
        if fixture is None:
            sys.stderr.write("NEEV-MCP-BROKER fixture=refused\n")
            return 2
        packages[ACCEPT_FIXTURE_PACKAGE] = fixture
    raw = sys.stdin.buffer.readline(MAX_BUNDLE_BYTES + 1)
    try:
        servers = parse_bundle(raw.rstrip(b"\n"), packages=packages)
    except IntegrationError:
        sys.stderr.write("NEEV-MCP-BROKER bundle=invalid\n")
        return 2
    proxy = os.environ.get("HTTPS_PROXY") or None

    def log(text):
        try:
            sys.stderr.write(redact(text, servers) + "\n")
            sys.stderr.flush()
        except Exception:
            pass

    broker = Broker(servers, peer_uid=args.peer_uid, proxy=proxy, home=args.home, log=log,
                    packages=packages)
    try:
        broker.start()
    except OSError:
        # A name already bound in this netns is a squatter, never ours to share.
        log("NEEV-MCP-BROKER bind=refused")
        return 3
    signal.signal(signal.SIGTERM, lambda *_: broker.stop())
    # Tell the launcher the sockets are bound (it waits for this byte).
    sys.stdout.write("R\n")
    sys.stdout.flush()
    sys.stdout.close()
    # Lifeline: the launcher keeps the bundle pipe open for the whole run. EOF
    # means the launcher is gone (even SIGKILLed), and the broker — PID 1 of its
    # own pid namespace, so its package children die with it — exits at once.
    lifeline_eof(sys.stdin.buffer)
    broker.stop()
    os._exit(0)


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
