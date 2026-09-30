#!/usr/bin/env python3
"""Concrete per-provider LoginDriver + AccountProbe implementations for the
supervisor (`neev_agent_supervisor`).

Each driver drives ONE provider sign-in against a mounted credential volume by
speaking the provider's real protocol over a `Channel` (the sandboxed provider
process's stdio):

  * Codex — the app-server device flow via `neev_codex_login_protocol`
    (initialize -> account/login/start -> device challenge -> completion ->
    account/read).
  * Claude — the browser + paste-code flow via `neev_claude_login_protocol`
    (print authorize URL -> read pasted authorization code from stdin).
  * OpenCode — `opencode auth login` ChatGPT headless device flow (user code at the
    OpenAI device URL; the CLI polls), via `neev_acp_login_protocol`.
  * Gemini — `gemini` NO_BROWSER paste-code sign-in on a PTY (the CLI refuses the
    flow on a pipe), via `neev_acp_login_protocol`.
  * Cursor — `agent login` with NO_OPEN_BROWSER (open URL; the CLI polls, exits).

`Channel` is the ONLY seam that spawns a process, so the protocol-driving state
machines are exercised with an in-memory fake here and the real sandbox in
production (`SandboxChannel`). No credential or authorization code is logged; the
pasted code goes straight from the DTO to the provider's stdin.

The AccountProbes are driver-INDEPENDENT (check_connection uses them, including
after a restart when no login driver is cached): Codex re-runs the app-server
account/read; Claude checks the stored credential's presence in the volume.
"""

from __future__ import annotations

import abc
import json
import os
import re
import sys
import time
from typing import Optional

import neev_agent_supervisor as sup
import neev_codex_login_protocol as codex_proto
import neev_claude_login_protocol as claude_proto


_READ_CHUNK = 65536
_PROBE_DEADLINE_SECONDS = 30.0
_PROBE_POLL_SECONDS = 0.05
_MAX_STATUS_BYTES = 65536
_MAX_STDERR_BYTES = 65536

# The ONLY hostnames a login-failure diagnostic may name — a fixed allowlist, so the
# classifier can never echo an attacker-influenced or secret-bearing string.
_KNOWN_HOSTS = (
    "api.anthropic.com", "claude.com", "claude.ai", "console.anthropic.com",
    "platform.claude.com", "auth.openai.com", "chatgpt.com", "api.openai.com",
    "api2.cursor.sh", "prod.authentication.cursor.sh", "oauth2.googleapis.com",
    "www.googleapis.com", "cloudcode-pa.googleapis.com",
)
_STATUS_RE = re.compile(r"(?:http|status|code|response)[^0-9]{0,12}([1-5][0-9]{2})", re.I)
_EGRESS_MARKERS = (
    "econnrefused", "connection refused", "etimedout", "enotfound", "eai_again",
    "tunnel", "proxy", "getaddrinfo", "network is unreachable", "socket hang up",
)
_TOKEN_MARKERS = ("oauth", "token", "invalid_grant", "unauthorized", "exchange", "roles")


def classify_login_failure(stderr: bytes) -> tuple:
    """Reduce raw provider stderr to SAFE, structured signals: (http_status, host,
    category). Emits only a numeric status, a host from the fixed `_KNOWN_HOSTS` set, and
    a fixed category string — NEVER raw stderr, tokens, codes, URLs or arbitrary text."""
    text = stderr.decode("utf-8", "replace")[:_MAX_STDERR_BYTES]
    low = text.lower()
    host = next((h for h in _KNOWN_HOSTS if h in low), None)
    match = _STATUS_RE.search(low)
    status = int(match.group(1)) if match else None
    if any(marker in low for marker in _EGRESS_MARKERS):
        category = "egress_blocked"
    elif status is not None and 400 <= status < 600:
        category = "http_error"
    elif any(marker in low for marker in _TOKEN_MARKERS):
        category = "token_exchange"
    elif not text.strip():
        category = "no_stderr"
    else:
        category = "unknown"
    return status, host, category


# --------------------------------------------------------------------------- #
# Channel seam
# --------------------------------------------------------------------------- #


class Channel(abc.ABC):
    """One sandboxed provider process's stdio, non-blocking. Concrete: SandboxChannel;
    tests: an in-memory fake."""

    @abc.abstractmethod
    def start(self) -> None: ...

    @abc.abstractmethod
    def read(self) -> bytes:
        """Return any bytes available on stdout now (b"" if none/blocked)."""

    @abc.abstractmethod
    def write(self, data: bytes) -> None: ...

    @abc.abstractmethod
    def returncode(self) -> Optional[int]:
        """The process exit code, or None while it is still running."""

    @abc.abstractmethod
    def terminate(self) -> None: ...

    def read_stderr(self) -> bytes:
        """Bounded provider stderr captured so far (b"" if not captured). Used ONLY to
        classify a login failure into safe categories — never logged raw."""
        return b""

    def read_browser_capture(self) -> Optional[str]:
        """The URL the CLI handed to `$BROWSER` (neev_browser_capture), if any."""
        return None

    def deliver_loopback(self, target, query: str) -> int:
        """ONE GET to the CLI's loopback listener inside this channel's sandbox
        netns; the listener's HTTP status, or 0 when unreachable."""
        return 0


class SandboxChannel(Channel):  # pragma: no cover - Linux subprocess path
    """A Channel backed by a sandboxed provider process (`neev_agent_sandbox.launch`
    with stdin_pipe). The volume is bound into the sandbox HOME and egress is the
    provider's login allowlist proxy."""

    def __init__(self, spec, bind_source, env):
        self._spec = spec
        self._bind_source = bind_source
        self._env = env
        self._handle = None
        self._stderr = bytearray()

    def start(self) -> None:
        import neev_agent_sandbox as sandbox

        self._handle = sandbox.launch(
            self._spec, env=self._env, bind_source=self._bind_source,
            stdin_pipe=True, stderr_pipe=True,
        )
        os.set_blocking(self._handle.stdout.fileno(), False)
        if self._handle.stderr is not None:
            os.set_blocking(self._handle.stderr.fileno(), False)

    def read(self) -> bytes:
        self._drain_stderr()
        if self._handle is None or self._handle.stdout is None:
            return b""
        try:
            chunk = os.read(self._handle.stdout.fileno(), _READ_CHUNK)
        except BlockingIOError:
            return b""
        except OSError:
            return b""
        return chunk or b""

    def _drain_stderr(self) -> None:
        # Always drain the pipe fully (else a full stderr pipe would BLOCK the provider),
        # but store only up to the cap — overflow is read and discarded, never persisted.
        if self._handle is None or self._handle.stderr is None:
            return
        fd = self._handle.stderr.fileno()
        while True:
            try:
                chunk = os.read(fd, _READ_CHUNK)
            except (BlockingIOError, OSError):
                return
            if not chunk:
                return  # EOF
            room = _MAX_STDERR_BYTES - len(self._stderr)
            if room > 0:
                self._stderr.extend(chunk[:room])

    def read_stderr(self) -> bytes:
        self._drain_stderr()
        return bytes(self._stderr)

    def write(self, data: bytes) -> None:
        if self._handle is None or self._handle.stdin is None:
            raise OSError("no stdin")
        self._handle.stdin.write(data)
        self._handle.stdin.flush()

    def returncode(self) -> Optional[int]:
        if self._handle is None:
            return None
        try:
            return self._handle.wait(timeout=0)
        except Exception:
            return None

    def terminate(self) -> None:
        if self._handle is not None:
            self._handle.teardown()


def _callback_diag(provider: str, reason: str) -> None:
    """A FIXED-vocabulary line (provider + CallbackRejected reason) so a provider
    adding a redirect parameter (e.g. RFC 9207 `iss` -> reason=key) is visible
    at once. Never includes any part of the query, code or state."""
    try:
        sys.stderr.write("NEEV-CALLBACK-DIAG provider=%s reason=%s\n" % (provider, reason))
        sys.stderr.flush()
    except Exception:
        pass


def _deliver(driver, query: str) -> str:
    """Shared direct-sign-in delivery: gate (state/shape/single use) FIRST, then
    one loopback GET in the sandbox. A refused query never reaches the CLI."""
    import neev_login_callback as callback

    gate = driver._gate
    if gate is None or driver._state != "awaiting_user":
        return "rejected"
    try:
        canonical = gate.check(query)
    except callback.CallbackRejected as refused:
        _callback_diag(gate.target.provider, refused.reason)
        return "rejected"
    try:
        status = driver._channel.deliver_loopback(gate.target, canonical)
    except callback.CallbackRejected as refused:
        _callback_diag(gate.target.provider, refused.reason)
        return "unavailable"  # not this login's sandbox any more (I5)
    if status:
        # The listener answered: the code reached the CLI (or was refused by
        # it). Either way this callback is spent.
        gate.commit()
    result = callback.outcome(status)
    if result == "delivered":
        driver._state = "verifying"
    return result


# --------------------------------------------------------------------------- #
# Codex device-flow driver
# --------------------------------------------------------------------------- #


class CodexLoginDriver(sup.LoginDriver):
    """`method="browser"` (direct sign-in, the default for new attempts) runs the
    app-server `chatgpt` login: the CLI's own authorize URL whose redirect is its
    127.0.0.1:1455 listener, fed by deliver_callback. `method="device"` is the
    device-code fallback, unchanged."""

    def __init__(self, channel: Channel, method: str = "device"):
        self._channel = channel
        self._method = method
        self._gate = None
        self._protocol = codex_proto.CodexLoginProtocol(method)
        self._state = "starting"
        self._challenge = None
        self._account_label = None
        self._failure = None

    def start(self) -> None:
        self._channel.start()
        self._channel.write(self._protocol.initialize())

    def poll(self) -> sup.DriverState:
        self._pump()
        return sup.DriverState(
            self._state, challenge=self._challenge,
            account_label=self._account_label, failure=self._failure,
        )

    def _pump(self) -> None:
        if self._state in ("prepared", "failed"):
            return
        chunk = self._channel.read()
        if chunk:
            try:
                for event in self._protocol.receive(chunk):
                    self._handle(event)
            except codex_proto.ProtocolError:
                self._state, self._failure = "failed", "provider_unavailable"
                return
        if self._state not in ("prepared", "failed") and self._channel.returncode() is not None:
            # The app-server exited before a supported account — unavailable.
            self._state, self._failure = "failed", "provider_unavailable"

    def _handle(self, event) -> None:
        if event.outbound:
            self._channel.write(event.outbound)
        if event.kind == "initialized":
            self._channel.write(self._protocol.start_login())
        elif event.kind == "login_started" and isinstance(
            event.login, codex_proto.BrowserLoginChallenge
        ):
            import neev_login_callback as callback

            login = event.login
            self._gate = callback.CallbackGate(callback.CallbackTarget(
                "codex", login.callback_port, login.callback_path, login.state))
            self._challenge = sup.Challenge(
                login.verification_url, 0, kind="browser_callback",
                callback_port=login.callback_port, callback_path=login.callback_path,
            )
            self._state = "awaiting_user"
        elif event.kind == "login_started":
            self._challenge = sup.Challenge(
                event.login.verification_url, 0, user_code=event.login.user_code
            )
            self._state = "awaiting_user"
        elif event.kind == "account_check_required":
            self._channel.write(self._protocol.read_account(refresh=True))
            self._state = "verifying"
        elif event.kind == "account":
            if event.account.status == "supported":
                self._account_label = event.account.plan
                self._state = "prepared"
            else:
                self._state, self._failure = "failed", "unsupported_account"
        elif event.kind == "login_failed":
            self._state, self._failure = "failed", "reauth_required"

    def deliver_callback(self, query: str) -> str:
        return _deliver(self, query)

    def submit_code(self, code: str) -> None:
        raise RuntimeError("codex device login accepts no pasted code")

    def cancel(self) -> None:
        try:
            self._channel.write(self._protocol.cancel_login())
        except Exception:
            pass
        self._channel.terminate()

    def logout(self) -> str:
        # The provider session is torn down with the process; the credential volume is
        # revoked by the supervisor. We do not claim a confirmed provider-side logout.
        self._channel.terminate()
        return "not_needed"

    def release(self) -> None:
        # Committed: the app-server has exited; free the channel's transient resources
        # (slot proxy + run lock + workdir) WITHOUT revoking the credential volume.
        self._channel.terminate()


# --------------------------------------------------------------------------- #
# Claude browser + paste-code driver
# --------------------------------------------------------------------------- #


class ClaudeLoginDriver(sup.LoginDriver):
    def __init__(self, channel: Channel, credential_present, direct: bool = False):
        self._channel = channel
        # Direct sign-in only when the coordinator asked for it (method=browser):
        # the deployed coordinator cannot decode a browser_callback challenge.
        self._direct = direct
        self._protocol = claude_proto.ClaudeLoginProtocol()
        self._credential_present = credential_present
        self._state = "starting"
        self._challenge = None
        self._failure = None
        self._submitted = False
        self._printed_url = None
        self._gate = None

    def start(self) -> None:
        self._channel.start()

    def poll(self) -> sup.DriverState:
        self._pump()
        return sup.DriverState(self._state, challenge=self._challenge, failure=self._failure)

    def _pump(self) -> None:
        if self._state in ("prepared", "failed"):
            return
        chunk = self._channel.read()
        if chunk:
            try:
                for event in self._protocol.receive(chunk):
                    self._handle(event)
            except claude_proto.ProtocolError:
                self._state, self._failure = "failed", "provider_unavailable"
                self._emit_failure_diagnostic(None)
                return
        self._upgrade_to_direct()
        returncode = self._channel.returncode()
        if self._state not in ("prepared", "failed") and returncode is not None:
            # The CLI exited: success iff a code was submitted, the CLI exited CLEANLY
            # (rc 0 — a suppressed nonzero, e.g. a denied post-exchange profile fetch,
            # is NOT success), AND real credentials landed in the volume. The rc-0 gate
            # is essential: the config dir is pre-created by credential delivery, so a
            # dir-presence check alone would falsely report prepared on a failed login.
            credential = self._credential_present()
            if self._submitted and returncode == 0 and credential:
                self._state = "prepared"
            else:
                self._state, self._failure = "failed", (
                    "reauth_required" if self._submitted else "provider_unavailable"
                )
                # Emit a SANITIZED diagnostic (rc, credential-record bool, classified
                # http status/host, category) to the daemon's stderr BEFORE teardown
                # revokes the volume. Never logs raw stderr, tokens, codes or URLs.
                self._emit_failure_diagnostic(returncode, credential)

    def _emit_failure_diagnostic(self, returncode, credential=None) -> None:
        try:
            status, host, category = classify_login_failure(self._channel.read_stderr())
            if credential is None:
                credential = self._credential_present()
            sys.stderr.write(
                "NEEV-LOGIN-DIAG provider=claude rc=%s cred_record=%s http_status=%s "
                "host=%s class=%s\n" % (returncode, bool(credential), status, host, category)
            )
            sys.stderr.flush()
        except Exception:
            pass  # diagnostics must never break the login state machine

    def _upgrade_to_direct(self) -> None:
        """Offer direct sign-in once the SAME login's loopback URL (handed to
        `$BROWSER`, recorded by neev_browser_capture) is known. Only a URL whose
        state matches the printed paste URL's is accepted — same login, same
        PKCE (measured on 2.1.267). The paste URL stays as the fallback."""
        if (not self._direct or self._gate is not None or self._printed_url is None
                or self._submitted):
            return
        captured = self._channel.read_browser_capture()
        if not captured:
            return
        import neev_login_callback as callback
        from urllib.parse import parse_qsl, urlsplit

        try:
            target = callback.target_from_authorize_url("claude", captured)
            printed_state = dict(parse_qsl(urlsplit(self._printed_url).query)).get("state", "")
        except (callback.CallbackRejected, ValueError):
            return
        if not printed_state or printed_state != target.state:
            return
        self._gate = callback.CallbackGate(target)
        self._challenge = sup.Challenge(
            captured, 0, kind="browser_callback", callback_port=target.port,
            callback_path=target.path, fallback_url=self._printed_url,
        )

    def deliver_callback(self, query: str) -> str:
        if self._submitted:
            return "rejected"  # the paste fallback already answered this login
        result = _deliver(self, query)
        if result == "delivered":
            # Completion is judged exactly as for a pasted code: exit 0 AND the
            # credential record (see _pump).
            self._submitted = True
        return result

    def _handle(self, event) -> None:
        if event.kind == "login_url":
            self._printed_url = event.login.verification_url
            self._challenge = sup.Challenge(event.login.verification_url, 0, user_code=None)
            self._state = "awaiting_user"
        elif event.kind == "awaiting_code":
            self._state = "awaiting_user"

    def submit_code(self, code: str) -> None:
        if self._gate is not None and self._gate.used:
            raise RuntimeError("the browser callback already answered this login")
        self._channel.write(self._protocol.submit_code(code))
        self._submitted = True
        self._state = "verifying"

    def cancel(self) -> None:
        self._channel.terminate()

    def logout(self) -> str:
        self._channel.terminate()
        return "not_needed"

    def release(self) -> None:
        # Committed: the CLI has exited; free the channel's transient resources (slot
        # proxy + run lock + workdir) WITHOUT revoking the credential volume.
        self._channel.terminate()


# --------------------------------------------------------------------------- #
# Account probes (driver-independent; used by check_connection)
# --------------------------------------------------------------------------- #


class CodexAccountProbe(sup.AccountProbe):
    """Re-run the Codex app-server against the volume and read the account. Works with
    no cached login driver (post-restart)."""

    def __init__(self, channel: Channel, *, now=time.monotonic):
        self._channel = channel
        self._now = now

    def probe(self):
        protocol = codex_proto.CodexLoginProtocol()
        self._channel.start()
        try:
            self._channel.write(protocol.initialize())
            deadline = self._now() + _PROBE_DEADLINE_SECONDS
            while self._now() < deadline:
                chunk = self._channel.read()
                if chunk:
                    for event in protocol.receive(chunk):
                        if event.outbound:
                            self._channel.write(event.outbound)
                        if event.kind == "initialized":
                            self._channel.write(protocol.read_account(refresh=False))
                        elif event.kind == "account":
                            account = event.account
                            if account.status == "supported":
                                return ("supported", account.plan)
                            return account.status
                    continue
                if self._channel.returncode() is not None:
                    raise RuntimeError("codex app-server exited before account/read")
                time.sleep(_PROBE_POLL_SECONDS)
            raise RuntimeError("codex account probe timed out")
        finally:
            self._channel.terminate()


class ClaudeAccountProbe(sup.AccountProbe):
    """Probe the stored Claude credential by running the pinned CLI's native
    `claude auth status --json` (verified present at 2.1.267) against the volume — NOT
    a mere file-presence check. Parses the native outcome: {"loggedIn": bool,
    "authMethod": str, ...}. loggedIn true -> supported (label = authMethod);
    loggedIn false -> disconnected; any non-JSON / missing field / timeout -> raises
    (check reports UNAVAILABLE, fail-closed)."""

    def __init__(self, channel: Channel, *, now=time.monotonic):
        self._channel = channel
        self._now = now

    def probe(self):
        self._channel.start()
        try:
            buffer = bytearray()
            deadline = self._now() + _PROBE_DEADLINE_SECONDS
            while self._now() < deadline:
                chunk = self._channel.read()
                if chunk:
                    buffer.extend(chunk)
                    if len(buffer) > _MAX_STATUS_BYTES:
                        raise RuntimeError("claude status output too large")
                    continue
                if self._channel.returncode() is not None:
                    break  # process exited; parse what it printed
                time.sleep(_PROBE_POLL_SECONDS)
            else:
                raise RuntimeError("claude status probe timed out")
            try:
                data = json.loads(bytes(buffer).decode("utf-8"))
            except (UnicodeDecodeError, ValueError) as error:
                raise RuntimeError("claude status not JSON") from error
            if not isinstance(data, dict) or "loggedIn" not in data:
                raise RuntimeError("claude status missing loggedIn")
            if data.get("loggedIn") is True:
                method = data.get("authMethod")
                return ("supported", method if isinstance(method, str) and method else "account")
            return "disconnected"
        finally:
            self._channel.terminate()


# --------------------------------------------------------------------------- #
# ACP providers: official CLI sign-in commands (neev_acp_login_protocol)
# --------------------------------------------------------------------------- #


def _acp_proto():
    """Imported on first ACP use only, so the Claude/Codex supervisor keeps working
    in an image that does not ship the ACP modules (Dockerfile.native.agent.acp)."""
    import neev_acp_login_protocol

    return neev_acp_login_protocol


class _CliLoginDriver(sup.LoginDriver):
    """Shared state machine for a sign-in CLI whose output is parsed by an
    `neev_acp_login_protocol` parser. Subclasses decide what completes it."""

    provider = "acp"

    def __init__(self, channel: Channel, protocol, credential_present=None):
        self._channel = channel
        self._protocol = protocol
        self._credential_present = credential_present
        self._state = "starting"
        self._challenge = None
        self._failure = None
        self._succeeded = False
        self._account_label = None

    def start(self) -> None:
        self._channel.start()

    def poll(self) -> sup.DriverState:
        self._pump()
        return sup.DriverState(
            self._state, challenge=self._challenge,
            account_label=self._account_label, failure=self._failure,
        )

    def _fail(self, failure: str) -> None:
        self._state, self._failure = "failed", failure

    def _pump(self) -> None:
        if self._state in ("prepared", "failed"):
            return
        chunk = self._channel.read()
        if chunk:
            try:
                for event in self._protocol.receive(chunk):
                    self._handle(event)
            except _acp_proto().ProtocolError:
                self._fail("provider_unavailable")
                return
        if self._state in ("prepared", "failed"):
            return
        self._check_progress(self._channel.returncode())

    def _handle(self, event) -> None:
        if event.kind == "challenge":
            self._challenge = sup.Challenge(
                event.verification_url, 0, user_code=event.user_code
            )
            self._state = "awaiting_user"
        elif event.kind == "succeeded":
            self._succeeded = True
        elif event.kind == "failed":
            self._fail("reauth_required" if self._challenge else "provider_unavailable")

    def _check_progress(self, returncode) -> None:  # pragma: no cover - abstract
        raise NotImplementedError

    def submit_code(self, code: str) -> None:
        raise RuntimeError("this sign-in accepts no pasted code")

    def cancel(self) -> None:
        self._channel.terminate()

    def logout(self) -> str:
        self._channel.terminate()
        return "not_needed"

    def release(self) -> None:
        self._channel.terminate()


class OpenCodeLoginDriver(_CliLoginDriver):
    """`opencode auth login --provider openai --method "ChatGPT Pro/Plus (headless)"`.

    Prepared ONLY when all three hold: the CLI printed "Login successful", it
    exited 0, and the volume holds an `openai` oauth record in auth.json (shape
    checked by the daemon, never values). Any one alone is not a sign-in."""

    def __init__(self, channel: Channel, credential_present):
        super().__init__(channel, _acp_proto().OpenCodeLoginProtocol(), credential_present)

    def _check_progress(self, returncode) -> None:
        if returncode is None:
            return
        if returncode == 0 and self._succeeded and self._credential_present():
            self._account_label = "ChatGPT"
            self._state = "prepared"
        else:
            self._fail("reauth_required" if self._challenge else "provider_unavailable")


class CursorLoginDriver(_CliLoginDriver):
    """`agent login` with NO_OPEN_BROWSER=1 and the file credential store. The
    pinned CLI prints https://cursor.com/loginDeepControl?... over a plain pipe
    (observed in the pinned image) and polls. Prepared requires the printed
    challenge, exit 0, AND the auth.json record (shape-checked by the daemon) —
    completion itself still needs a real sign-in to observe."""

    def __init__(self, channel: Channel, credential_present):
        super().__init__(channel, _acp_proto().CursorLoginProtocol(), credential_present)

    def _check_progress(self, returncode) -> None:
        if returncode is None:
            return
        if returncode == 0 and self._challenge is not None and self._credential_present():
            self._account_label = "Cursor"
            self._state = "prepared"
        else:
            self._fail("reauth_required" if self._challenge else "provider_unavailable")


class GeminiLoginDriver(_CliLoginDriver):
    """Interactive `gemini` with NO_BROWSER=true on a PTY channel.

    The CLI prints a Google authorize URL and reads the code the user pastes back.
    After the paste the CLI does NOT exit (it continues into its chat UI), so
    completion is the cached credential record (`~/.gemini/oauth_creds.json` with a
    refresh token, shape-checked by the daemon) — never the mere absence of an
    error. A process exit before that record exists is a failure."""

    def __init__(self, channel: Channel, credential_present):
        super().__init__(channel, _acp_proto().GeminiLoginProtocol(), credential_present)
        self._awaiting_code = False
        self._submitted = False

    def _handle(self, event) -> None:
        if event.kind == "awaiting_code":
            self._awaiting_code = True
            return
        if event.kind == "challenge":
            # Shown to the user only once the CLI is actually reading a code.
            self._challenge = sup.Challenge(event.verification_url, 0)
            return
        super()._handle(event)

    def _check_progress(self, returncode) -> None:
        if self._challenge is not None and self._awaiting_code and self._state == "starting":
            self._state = "awaiting_user"
        if self._submitted and self._credential_present():
            self._account_label = "Google"
            self._state = "prepared"
            return
        if returncode is not None:
            self._fail("reauth_required" if self._submitted else "provider_unavailable")

    def submit_code(self, code: str) -> None:
        if not self._awaiting_code or self._submitted:
            raise RuntimeError("gemini is not reading a code")
        self._channel.write(_acp_proto().submit_code_bytes(code))
        self._submitted = True
        self._state = "verifying"


class AcpAccountProbe(sup.AccountProbe):
    """Run `neev_acp_stream_adapter.py --probe` against the volume: ACP initialize +
    session/new, no prompt, no tokens. The adapter prints exactly one
    `{"type":"neev.account","status":...}` line; `supported` -> ("supported", label),
    `disconnected` -> "disconnected", anything else (unavailable, no answer, bad
    JSON, timeout) RAISES so check_connection reports UNAVAILABLE — never a guess.

    `credential_present`, when given, is a daemon-side shape check that must ALSO
    hold for `supported` (defence in depth for a CLI that would otherwise answer
    on an anonymous default)."""

    def __init__(self, channel: Channel, label: str, credential_present=None, *,
                 now=time.monotonic):
        self._channel = channel
        self._label = label
        self._credential_present = credential_present
        self._now = now

    def probe(self):
        if self._credential_present is not None and not self._credential_present():
            return "disconnected"
        self._channel.start()
        try:
            buffer = bytearray()
            deadline = self._now() + _PROBE_DEADLINE_SECONDS + 30.0
            while self._now() < deadline:
                chunk = self._channel.read()
                if chunk:
                    buffer.extend(chunk)
                    if len(buffer) > _MAX_STATUS_BYTES:
                        raise RuntimeError("acp probe output too large")
                    status = _account_status(buffer)
                    if status is not None:
                        return self._status(status)
                    continue
                if self._channel.returncode() is not None:
                    status = _account_status(buffer)
                    if status is None:
                        raise RuntimeError("acp probe exited without an answer")
                    return self._status(status)
                time.sleep(_PROBE_POLL_SECONDS)
            raise RuntimeError("acp probe timed out")
        finally:
            self._channel.terminate()

    def _status(self, value):
        if value == "supported":
            return ("supported", self._label)
        if value == "disconnected":
            return "disconnected"
        raise RuntimeError("acp probe could not decide")


def _account_status(buffer):
    """The probe's verdict from complete lines, or None if not printed yet."""
    for line in bytes(buffer).split(b"\n")[:-1]:
        try:
            record = json.loads(line.decode("utf-8"))
        except (UnicodeDecodeError, ValueError):
            continue
        if isinstance(record, dict) and record.get("type") == "neev.account":
            status = record.get("status")
            if status in ("supported", "disconnected"):
                return status
            raise RuntimeError("acp probe could not decide")
    return None
