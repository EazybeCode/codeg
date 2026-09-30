#!/usr/bin/env python3
"""Direct sign-in: bind a relayed browser callback to ONE login attempt, then
deliver it to that attempt's CLI loopback listener (direct-signin-contract.md).

The vendor CLI (Codex app-server `chatgpt`, `claude auth login`) runs its own
PKCE browser login in the sandbox and listens on 127.0.0.1:<port><path>. The
user's browser is redirected there by the provider; Neev relays that redirect's
query. Everything that can go wrong is refused HERE, before the CLI sees it:

  * Only the callback the CLI itself announced: port/path come from the CLI's
    own redirect_uri at login start, never from the request. Host is always
    127.0.0.1. Paths: /auth/callback (Codex), /callback (Claude).
  * Only this attempt's callback: `state` must equal the attempt's state
    (constant-time). Claude 2.1.267 EXITS on a wrong state or a missing code
    (measured), so a stray or forged callback must never reach it.
  * Only the documented keys: code, state, scope, error, error_description —
    each once, bounded, printable. Re-encoded by us; the raw text is never
    forwarded, logged or reflected.
  * Once: an attempt accepts one delivered callback.

`deliver()` runs the GET INSIDE the attempt's sandbox network namespace
(`ip netns exec`), dropped to the lane uid, via `--serve` in this same file.
"""

from __future__ import annotations

import hmac
import json
import os
import subprocess
import sys
from dataclasses import dataclass
from typing import Optional, Tuple
from urllib.parse import parse_qsl, urlencode, urlsplit

CALLBACK_PATHS = {"codex": "/auth/callback", "claude": "/callback"}
ALLOWED_KEYS = ("code", "state", "scope", "error", "error_description")
LOOPBACK_HOSTS = ("localhost", "127.0.0.1")
MAX_QUERY_BYTES = 1024
MAX_VALUE_CHARS = 1024
# Short on purpose (review I5): the supervisor handles operations serially, so a
# hung listener must not stall every other user's login ops for long. A local
# listener that has not answered in 4 s is not going to.
DELIVER_TIMEOUT_SECONDS = 4.0
MAX_RESPONSE_BYTES = 64 * 1024


class CallbackRejected(Exception):
    """A fixed, non-reflective reason. Never carries any part of the query."""

    REASONS = frozenset(("shape", "key", "state", "code", "target", "used", "owner"))

    def __init__(self, reason: str):
        if reason not in self.REASONS:
            reason = "shape"
        super().__init__(reason)
        self.reason = reason


@dataclass(frozen=True)
class CallbackTarget:
    """What the CLI announced at login start: its loopback listener and state."""

    provider: str
    port: int
    path: str
    state: str

    def __post_init__(self):
        if self.provider not in CALLBACK_PATHS or self.path != CALLBACK_PATHS[self.provider]:
            raise CallbackRejected("target")
        if type(self.port) is not int or isinstance(self.port, bool) or not 1024 <= self.port <= 65535:
            raise CallbackRejected("target")
        if type(self.state) is not str or not 16 <= len(self.state) <= 256:
            raise CallbackRejected("target")


def target_from_authorize_url(provider: str, url: str) -> CallbackTarget:
    """Pure: the CallbackTarget from the CLI's own browser authorize URL."""
    try:
        parts = urlsplit(url)
        query = dict(parse_qsl(parts.query, keep_blank_values=True))
        redirect = urlsplit(query.get("redirect_uri", ""))
        port = redirect.port
    except ValueError:
        raise CallbackRejected("target") from None
    if (parts.scheme != "https" or redirect.scheme != "http"
            or redirect.hostname not in LOOPBACK_HOSTS or redirect.query
            or redirect.fragment or redirect.username or redirect.password
            or port is None):
        raise CallbackRejected("target")
    return CallbackTarget(provider, port, redirect.path, query.get("state", ""))


def canonical_query(raw: str, target: CallbackTarget) -> str:
    """Pure: the re-encoded query to deliver, or CallbackRejected.

    `raw` is the redirect's query string, optionally with a leading '?', or the
    whole pasted loopback URL (`http://localhost:<port><path>?...`), which must
    then name exactly the target's port and path."""
    if type(raw) is not str:
        raise CallbackRejected("shape")
    raw = raw.strip()
    if not raw or len(raw.encode("utf-8", "surrogatepass")) > MAX_QUERY_BYTES:
        raise CallbackRejected("shape")
    if any(ord(ch) < 0x21 or ord(ch) >= 0x7F for ch in raw):
        raise CallbackRejected("shape")
    if raw.startswith(("http://", "https://")):
        try:
            parts = urlsplit(raw)
            port = parts.port
        except ValueError:
            raise CallbackRejected("target") from None
        if (parts.scheme != "http" or parts.hostname not in LOOPBACK_HOSTS
                or port != target.port or parts.path != target.path
                or parts.username or parts.password):
            raise CallbackRejected("target")
        raw = parts.query
    elif raw.startswith("?"):
        raw = raw[1:]
    try:
        pairs = parse_qsl(raw, keep_blank_values=True, strict_parsing=True)
    except ValueError:
        raise CallbackRejected("shape") from None
    values = {}
    for key, value in pairs:
        if key not in ALLOWED_KEYS:
            raise CallbackRejected("key")
        if key in values:
            raise CallbackRejected("key")
        if len(value) > MAX_VALUE_CHARS or not value.isprintable():
            raise CallbackRejected("shape")
        values[key] = value
    state = values.get("state", "")
    if not hmac.compare_digest(state.encode("utf-8"), target.state.encode("utf-8")):
        raise CallbackRejected("state")
    has_code = bool(values.get("code"))
    has_error = bool(values.get("error"))
    if has_code == has_error:
        # Exactly one: a code (approved) or an error (denied). Claude exits on a
        # callback with neither, so it is never sent.
        raise CallbackRejected("code")
    return urlencode([(key, values[key]) for key in ALLOWED_KEYS if key in values])


class CallbackGate:
    """Single-use binding of one attempt's target. Not thread-safe by design:
    the supervisor serialises operations per attempt.

    `check()` validates WITHOUT consuming; `commit()` consumes. The gate is
    consumed only once the CLI's listener actually ANSWERED (review B2): a
    delivery that never reached it (netns exec error, timeout, spawn failure)
    leaves the gate open, so the same callback — or the paste fallback — can be
    retried instead of being refused as "used" until the attempt expires."""

    def __init__(self, target: CallbackTarget):
        self.target = target
        self._used = False

    @property
    def used(self) -> bool:
        return self._used

    def check(self, raw: str) -> str:
        if self._used:
            raise CallbackRejected("used")
        return canonical_query(raw, self.target)

    def commit(self) -> None:
        self._used = True

    def accept(self, raw: str) -> str:
        """check() + commit() in one step (for callers that deliver nothing)."""
        query = self.check(raw)
        self.commit()
        return query


def assert_delivery_owner(netns: str, slot: str, holds_slot: bool, child_alive: bool) -> None:
    """Deliver only into THIS login's own sandbox (review I5).

    The egress netns is per SLOT (`neev-agent-<slot>`), not per attempt; it is
    safe to reach 127.0.0.1:<port> there only while this channel holds that
    slot's exclusive lock (so no other tenant's process can be listening in it)
    and its own CLI child is still running."""
    if (not holds_slot or not child_alive or type(slot) is not str
            or netns != f"neev-agent-{slot}"):
        raise CallbackRejected("owner")


# --------------------------------------------------------------------------- #
# Delivery (inside the sandbox netns)
# --------------------------------------------------------------------------- #

IP = "/usr/sbin/ip"
SETPRIV = "/usr/bin/setpriv"
PYTHON = "/usr/bin/python3"


def serve_argv(netns: str, uid: int, gid: int, script: Optional[str] = None) -> list:
    """Pure: the fixed command that performs ONE loopback GET inside `netns` as
    the lane uid. The request itself travels on stdin, never in argv."""
    if not isinstance(netns, str) or not netns or "/" in netns or netns.startswith("-"):
        raise CallbackRejected("target")
    for value in (uid, gid):
        if type(value) is not int or value <= 0:
            raise CallbackRejected("target")
    return [IP, "netns", "exec", netns, SETPRIV, f"--reuid={uid}", f"--regid={gid}",
            "--clear-groups", "--no-new-privs", PYTHON, "-I", "-B",
            script or os.path.abspath(__file__), "--serve"]


def _serve(stdin=sys.stdin, stdout=sys.stdout) -> int:  # runs inside the netns
    import http.client

    try:
        request = json.loads(stdin.read(4096))
        port, path, query = request["port"], request["path"], request["query"]
        if (type(port) is not int or not 1024 <= port <= 65535
                or path not in CALLBACK_PATHS.values()
                or type(query) is not str or len(query) > MAX_QUERY_BYTES):
            raise ValueError
    except (ValueError, KeyError, TypeError):
        stdout.write(json.dumps({"status": 0}))
        return 2
    try:
        connection = http.client.HTTPConnection("127.0.0.1", port,
                                                timeout=DELIVER_TIMEOUT_SECONDS)
        connection.request("GET", f"{path}?{query}",
                           headers={"Host": f"localhost:{port}", "Connection": "close"})
        response = connection.getresponse()
        response.read(MAX_RESPONSE_BYTES)
        status = response.status
        connection.close()
    except (OSError, http.client.HTTPException):
        status = 0
    stdout.write(json.dumps({"status": status}))
    return 0


def deliver(target: CallbackTarget, query: str, argv: list, run=subprocess.run) -> int:
    """Run `argv` (from serve_argv) and return the listener's HTTP status, or 0
    when it could not be reached. Nothing about the query is logged."""
    payload = json.dumps({"port": target.port, "path": target.path, "query": query})
    try:
        result = run(argv, input=payload, capture_output=True, text=True,
                     timeout=DELIVER_TIMEOUT_SECONDS + 5, env={"PATH": "/usr/bin:/bin"})
        status = int(json.loads(result.stdout or "{}").get("status", 0))
        return status if 100 <= status <= 599 else 0
    except (OSError, ValueError, subprocess.TimeoutExpired):
        return 0


def outcome(status: int) -> str:
    """Fixed classification of the listener's answer. Codex answers 200 (or a
    redirect to its success page); both CLIs answer 400 to a callback they
    refuse, which our validation should already have prevented."""
    if 200 <= status < 400:
        return "delivered"
    if status == 0:
        return "unavailable"
    return "rejected"


if __name__ == "__main__":  # pragma: no cover - netns child entry
    if sys.argv[1:] == ["--serve"]:
        sys.exit(_serve())
    sys.exit(2)
