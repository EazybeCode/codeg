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
import time
from typing import Optional

import neev_agent_supervisor as sup
import neev_codex_login_protocol as codex_proto
import neev_claude_login_protocol as claude_proto


_READ_CHUNK = 65536
_PROBE_DEADLINE_SECONDS = 30.0
_PROBE_POLL_SECONDS = 0.05
_MAX_STATUS_BYTES = 65536


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


class SandboxChannel(Channel):  # pragma: no cover - Linux subprocess path
    """A Channel backed by a sandboxed provider process (`neev_agent_sandbox.launch`
    with stdin_pipe). The volume is bound into the sandbox HOME and egress is the
    provider's login allowlist proxy."""

    def __init__(self, spec, bind_source, env):
        self._spec = spec
        self._bind_source = bind_source
        self._env = env
        self._handle = None

    def start(self) -> None:
        import neev_agent_sandbox as sandbox

        self._handle = sandbox.launch(
            self._spec, env=self._env, bind_source=self._bind_source, stdin_pipe=True
        )
        os.set_blocking(self._handle.stdout.fileno(), False)

    def read(self) -> bytes:
        if self._handle is None or self._handle.stdout is None:
            return b""
        try:
            chunk = os.read(self._handle.stdout.fileno(), _READ_CHUNK)
        except BlockingIOError:
            return b""
        except OSError:
            return b""
        return chunk or b""

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


# --------------------------------------------------------------------------- #
# Codex device-flow driver
# --------------------------------------------------------------------------- #


class CodexLoginDriver(sup.LoginDriver):
    def __init__(self, channel: Channel):
        self._channel = channel
        self._protocol = codex_proto.CodexLoginProtocol()
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


# --------------------------------------------------------------------------- #
# Claude browser + paste-code driver
# --------------------------------------------------------------------------- #


class ClaudeLoginDriver(sup.LoginDriver):
    def __init__(self, channel: Channel, credential_present):
        self._channel = channel
        self._protocol = claude_proto.ClaudeLoginProtocol()
        self._credential_present = credential_present
        self._state = "starting"
        self._challenge = None
        self._failure = None
        self._submitted = False

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
                return
        if self._state not in ("prepared", "failed") and self._channel.returncode() is not None:
            # The CLI exited: success iff a code was submitted AND credentials landed
            # in the volume (the authoritative signal — not a TUI banner).
            if self._submitted and self._credential_present():
                self._state = "prepared"
            else:
                self._state, self._failure = "failed", (
                    "reauth_required" if self._submitted else "provider_unavailable"
                )

    def _handle(self, event) -> None:
        if event.kind == "login_url":
            self._challenge = sup.Challenge(event.login.verification_url, 0, user_code=None)
            self._state = "awaiting_user"
        elif event.kind == "awaiting_code":
            self._state = "awaiting_user"

    def submit_code(self, code: str) -> None:
        self._channel.write(self._protocol.submit_code(code))
        self._submitted = True
        self._state = "verifying"

    def cancel(self) -> None:
        self._channel.terminate()

    def logout(self) -> str:
        self._channel.terminate()
        return "not_needed"


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
