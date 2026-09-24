#!/usr/bin/env python3
"""Neev supervisor: the seven-operation effect dispatcher (server side of the
control_wire/control_dto Unix-socket protocol).

The Rust coordinator (`agents/control_*`) is the CLIENT; this is the supervisor
that actually performs the effects behind each request and answers with the exact
`neev_supervisor_dto` response the client validates. It replaces the transport's
"future launcher" seam with real behavior for all seven ops:

    start_login / observe_login / submit_code / settle_login
    check_connection / cleanup_connection / reconcile_page

Ownership boundaries:
  * The TRANSPORT (`neev_supervisor_transport`) owns framing, MAC, per-session
    sequencing and coordinator pinning; the FENCE (`neev_supervisor_fence`) owns
    durable coordinator registration + the check-and-commit admission. `serve()`
    wires both around this dispatcher; the dispatcher itself is a pure
    `Request -> Response` function over in-memory state, so it is exercised with a
    FAKE transport (direct `handle()` calls) in tests and the real transport in
    production.
  * The supervisor OWNS the credential-volume lifecycle (create on start, close on
    cleanup — never leaves a volume mounted for a canceled/disconnected attempt).
  * Per-provider sign-in is delegated to a `LoginDriver` (Codex device flow via
    `neev_codex_login_protocol`; Claude browser + paste-code via
    `neev_claude_login_protocol`), injected so the dispatch logic is provider- and
    subprocess-agnostic and unit-testable.

Nothing here logs a credential or an authorization code; the DTO's `AuthorizationCode`
is repr-suppressed and is handed straight to the driver.
"""

from __future__ import annotations

import abc
import asyncio
import contextlib
import json
import os
import sys
import tempfile
from dataclasses import dataclass
from typing import Callable, Dict, Optional, Tuple

import neev_supervisor_dto as dto
import neev_supervisor_fence as fence_mod
import neev_supervisor_transport as transport

try:
    from neev_credential_store import Scope as StoreScope
except Exception:  # pragma: no cover - store import shim for isolated tests
    StoreScope = None


_DEFAULT_POLL_MS = 500
_MAX_PAGE = 32


class Reject(Exception):
    """A dispatch refusal that becomes an `Outcome.ERROR` response. `code` is a
    `dto.ErrorCode`; `retry_after_ms` is required for BUSY/UNAVAILABLE (and only
    those), matching `AppError`'s invariant."""

    def __init__(self, code: dto.ErrorCode, retry_after_ms: Optional[int] = None):
        self.code = code
        self.retry_after_ms = retry_after_ms
        super().__init__(code.value)


class RegistryError(Exception):
    """A durable-registry failure that must ABORT recovery (fail-closed): the daemon
    refuses to serve rather than lose or corrupt committed connection state."""

    def __init__(self, reason: str):
        self.reason = reason
        super().__init__(reason)


# --------------------------------------------------------------------------- #
# Login driver seam
# --------------------------------------------------------------------------- #


@dataclass(frozen=True)
class Challenge:
    """What the user must do, provider-shaped. `user_code` is device-only; a Claude
    authorization-code flow leaves it None."""

    verification_url: str
    expires_at: int
    user_code: Optional[str] = None


@dataclass(frozen=True)
class DriverState:
    """A driver's current view. `phase` is one of: starting, awaiting_user,
    verifying, prepared, failed, expired, canceled, interrupted."""

    phase: str
    challenge: Optional[Challenge] = None
    account_label: Optional[str] = None
    account_state: Optional[str] = None  # supported|disconnected|unsupported (check)
    failure: Optional[str] = None  # dto.LoginFailure value when phase == "failed"


class LoginDriver(abc.ABC):
    """Drives ONE provider sign-in against a mounted credential volume. Concrete
    drivers (Codex/Claude) compose the sandbox launcher + the provider protocol;
    tests use an in-memory fake."""

    @abc.abstractmethod
    def start(self) -> None: ...

    @abc.abstractmethod
    def poll(self) -> DriverState: ...

    @abc.abstractmethod
    def submit_code(self, code: str) -> None: ...

    @abc.abstractmethod
    def cancel(self) -> None: ...

    @abc.abstractmethod
    def logout(self) -> str:
        """Return a `dto.LogoutResult` value (confirmed|not_confirmed|not_needed)."""

    def release(self) -> None:
        """Free the login's TRANSIENT resources after a durable commit — the slot proxy,
        the per-scope run lock, the run workdir — WITHOUT revoking or logging out (the
        committed credentials must persist). Default no-op for in-memory drivers; the
        production drivers tear their channel down. Idempotent."""


class AccountProbe(abc.ABC):
    """A fresh, login-driver-INDEPENDENT probe of a committed connection's stored
    credentials — used by check_connection so a check works after a restart (no
    cached login driver) and reflects the ACTUAL credential state, not a login poll.
    Production probes spawn the provider against the credential volume and read the
    account; a probe that cannot run raises (check reports UNAVAILABLE, fail-closed)."""

    @abc.abstractmethod
    def probe(self) -> str:
        """Return an account state: 'supported' | 'disconnected' | 'unsupported'.
        Optionally a (state, account_label) tuple for 'supported'."""


# --------------------------------------------------------------------------- #
# In-memory state
# --------------------------------------------------------------------------- #

_TERMINAL = frozenset(("failed", "expired", "canceled", "interrupted"))
_LOGIN_STATE = {
    "queued": dto.LoginState.QUEUED,
    "starting": dto.LoginState.STARTING,
    "verifying": dto.LoginState.VERIFYING,
    "expired": dto.LoginState.EXPIRED,
    "canceled": dto.LoginState.CANCELED,
    "interrupted": dto.LoginState.INTERRUPTED,
}


@dataclass
class _Attempt:
    binding: dto.LoginBinding
    ticket: Optional[dto.LoginTicketDto]  # None for attempts rebuilt at recovery
    provider: dto.Provider
    storage_handle: str  # 32-hex volume handle
    start_request_id: str
    state: str = "starting"
    receipt_id: Optional[str] = None
    committed: bool = False
    released: bool = False  # transient login resources freed after commit (idempotent)
    entry_id: Optional[str] = None
    driver: Optional[LoginDriver] = None


@dataclass
class _Entry:
    entry_id: str
    binding: dto.LoginBinding
    phase: dto.RegistryPhase
    storage_handle: Optional[str] = None
    receipt_id: Optional[str] = None
    cleanup_operation_id: Optional[str] = None


# The ONLY stages and codes a sign-in start diagnostic may name. The codes are the
# fixed literals raised by our own storage and channel layers
# (`neev_credential_store._SAFE_ERROR_CODES`, `neev_slot_proxy.SlotProxyError`,
# `neev_agent_supervisor_main.ChannelError`, `neev_agent_credentials.CredentialError`);
# anything else degrades to "unknown". Nothing derived from a provider, a path, a
# handle or an exception message is ever written.
_START_DIAG_STAGES = frozenset(("volume_create", "driver_start"))
_START_DIAG_CODES = frozenset((
    "invalid", "unavailable", "conflict", "busy", "quota",
    "quarantined", "occupied", "slot_quarantined", "teardown_failed",
    "credentials", "handle", "uid", "home", "shape", "mode",
))


def _report_start_failure(provider, stage: str, error: BaseException) -> None:
    """Emit ONE sanitized line when a sign-in dies before it ever reaches the provider.

    `_start_login` maps every exception to a bare UNAVAILABLE, which the browser shows
    as "This service is temporarily unavailable" — and the server, having already
    recorded the attempt, then cancels it. That signature (attempt pending, connect
    abandoned, cancel completed, one second apart) was indistinguishable from a user
    pressing Cancel, and nothing anywhere recorded which step failed. Live on
    2026-09-24 that cost hours: the real cause was the credential store running out of
    loop devices at `cryptsetup open`, and it left no trace at all.

    Diagnostics must never break the effect, so every failure here is swallowed.
    """
    try:
        code = getattr(error, "code", None)
        if type(code) is not str or code not in _START_DIAG_CODES:
            code = "unknown"
        name = getattr(provider, "value", None)
        if type(name) is not str or name not in ("claude", "codex"):
            name = "unknown"
        if stage not in _START_DIAG_STAGES:
            stage = "unknown"
        sys.stderr.write(
            "NEEV-LOGIN-DIAG provider=%s stage=%s code=%s\n" % (name, stage, code)
        )
        sys.stderr.flush()
    except Exception:
        pass


class Supervisor:
    """The seven-op effect dispatcher. `handle(request) -> Response`."""

    def __init__(
        self,
        boot_id: dto.Id,
        volumes,
        driver_for: Callable[[dto.Provider, "StoreScope", str], LoginDriver],
        *,
        now: Callable[[], int],
        mint_id: Callable[[], str],
        probe_for: Optional[Callable[[dto.Provider, "StoreScope", str], "AccountProbe"]] = None,
        durable=None,
    ):
        self._boot_id = boot_id
        self._volumes = volumes
        self._driver_for = driver_for
        # probe_for builds a fresh, driver-INDEPENDENT account probe against a
        # committed connection's credential volume (used by check_connection, which
        # must work after a restart when no login driver is cached).
        self._probe_for = probe_for
        self._now = now
        self._mint_id = mint_id
        self._durable = durable
        self._reconciled = False
        self._attempts: Dict[Tuple[str, str], _Attempt] = {}
        self._entries: Dict[str, _Entry] = {}
        self._revision = 1

    # -- recovery gate ---------------------------------------------------- #

    def recover(self) -> None:
        """Rebuild in-memory state from the durable registry BEFORE serving. Until
        this runs, `handle()` refuses every request (UNAVAILABLE): a supervisor that
        forgot its committed connections on restart would strand live credentials and
        orphan volume files. In-flight logins (their subprocess is gone) come back
        INTERRUPTED; committed connections come back usable; quarantined cleanups come
        back for retry; disposed entries are dropped."""
        if self._reconciled:
            return
        if self._durable is not None:
            highest = self._revision
            for record in self._durable.load():  # RegistryError propagates (fail-closed)
                try:
                    entry = self._entry_from_record(record)
                except (dto.DtoError, KeyError, TypeError, ValueError) as error:
                    # A record that no longer decodes is corruption; abort rather than
                    # skip it (skipping could silently drop a committed connection).
                    raise RegistryError("corrupt") from error
                if entry.phase is dto.RegistryPhase.DISPOSED:
                    continue
                self._entries[entry.entry_id] = entry
                self._rebuild_attempt(entry)
                highest += 1
            self._revision = highest
        self._reconciled = True

    # -- public dispatch -------------------------------------------------- #

    def handle(self, request: dto.Request) -> dto.Response:
        if not self._reconciled:
            # Never serve effects against un-recovered state.
            return self._error(request, dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
        handler = self._DISPATCH.get(request.op())
        if handler is None:  # unreachable: op() is exhaustive over the enum
            return self._error(request, dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
        try:
            body = handler(self, request)
        except Reject as reject:
            return self._error(request, reject.code, reject.retry_after_ms)
        return self._ok(request, body)

    # -- durability ------------------------------------------------------- #

    @staticmethod
    def _record(entry: _Entry) -> dict:
        return {
            "entry_id": entry.entry_id,
            "binding": dto._encode_login_binding(entry.binding),
            "phase": entry.phase.value,
            "storage_handle": entry.storage_handle,
            "receipt_id": entry.receipt_id,
            "cleanup_operation_id": entry.cleanup_operation_id,
        }

    @staticmethod
    def _entry_from_record(record: dict) -> _Entry:
        return _Entry(
            record["entry_id"],
            dto._login_binding(record["binding"]),
            dto.RegistryPhase(record["phase"]),
            record.get("storage_handle"),
            record.get("receipt_id"),
            record.get("cleanup_operation_id"),
        )

    def _persist(self, entry: _Entry) -> None:
        if self._durable is None:
            return
        if entry.phase is dto.RegistryPhase.DISPOSED:
            self._durable.delete(entry.entry_id)
        else:
            self._durable.put(self._record(entry))

    def _rebuild_attempt(self, entry: _Entry) -> None:
        phase = entry.phase
        committed = phase is dto.RegistryPhase.COMMITTED
        # No live driver survives a restart; the login process is gone.
        state = "committed" if committed else "interrupted"
        self._attempts[self._key(entry.binding)] = _Attempt(
            binding=entry.binding, ticket=None, provider=entry.binding.scope.provider,
            storage_handle=entry.storage_handle, start_request_id="", state=state,
            receipt_id=entry.receipt_id, committed=committed, entry_id=entry.entry_id,
            driver=None,
        )

    # -- response builders ------------------------------------------------ #

    def _ok(self, request: dto.Request, body) -> dto.Response:
        return dto.Response(
            request.coordinator, self._boot_id, request.sequence, request.request_id,
            request.op(), dto.Outcome.OK, body, None,
        )

    def error_response(self, request, code, retry_after_ms=None) -> dto.Response:
        """Public: build an Outcome.ERROR response (used by serve() for fence
        rejections that happen outside a dispatch handler)."""
        return self._error(request, code, retry_after_ms)

    def _error(self, request, code, retry_after_ms) -> dto.Response:
        hint = None
        if code in (dto.ErrorCode.BUSY, dto.ErrorCode.UNAVAILABLE):
            hint = dto.RetryMs(retry_after_ms if retry_after_ms is not None else _DEFAULT_POLL_MS)
        return dto.Response(
            request.coordinator, self._boot_id, request.sequence, request.request_id,
            request.op(), dto.Outcome.ERROR, None, dto.AppError(code, hint),
        )

    # -- helpers ---------------------------------------------------------- #

    @staticmethod
    def _key(binding: dto.LoginBinding) -> Tuple[str, str]:
        return (binding.connection_id.value, binding.attempt_id.value)

    def _lookup(self, binding: dto.LoginBinding) -> _Attempt:
        """Find the attempt for this binding, requiring the FULL stored binding to
        equal the incoming one (scope, generation, session, attempt, operation) —
        not just connection+attempt ids. A request that collides on those ids but
        differs anywhere else (another tenant/generation) must NOT reach this
        attempt; it is reported NOT_FOUND without leaking that a near-match exists."""
        attempt = self._attempts.get(self._key(binding))
        if attempt is None or attempt.binding != binding:
            raise Reject(dto.ErrorCode.NOT_FOUND)
        return attempt

    @staticmethod
    def _store_scope(scope: dto.Scope):
        if StoreScope is None:  # pragma: no cover - only when the store is absent
            raise Reject(dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
        return StoreScope(scope.org_id.value, scope.user_id.value, scope.provider.value)

    def _bump(self) -> None:
        if self._revision < dto._I64_MAX:
            self._revision += 1

    def _connection_binding(self, binding: dto.LoginBinding) -> dto.ConnectionBinding:
        return dto.ConnectionBinding(binding.scope, binding.connection_id, binding.generation)

    def _challenge_dto(self, provider: dto.Provider, challenge: Challenge, expires_at: int):
        # The challenge expiry is the attempt's own deadline (the ticket's), so a
        # driver need not surface a provider expiry it may not know. If the driver DID
        # report a tighter one, honor the sooner of the two.
        deadline = expires_at
        if challenge.expires_at and 0 < challenge.expires_at < deadline:
            deadline = challenge.expires_at
        if provider is dto.Provider.CODEX:
            if challenge.user_code is None:
                raise Reject(dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
            return dto.DeviceChallenge(
                challenge.verification_url,
                dto.UserCode(challenge.user_code),
                dto.UnixSec(deadline),
            )
        return dto.AuthorizationCodeChallenge(
            challenge.verification_url, dto.UnixSec(deadline)
        )

    def _advance(self, attempt: _Attempt) -> str:
        """Poll the driver, fold expiry, and return the current phase."""
        if attempt.committed:
            return "committed"
        if attempt.state in _TERMINAL:
            return attempt.state
        expires_at = attempt.ticket.expires_at.value
        state = attempt.driver.poll() if attempt.driver is not None else DriverState("starting")
        phase = state.phase
        if phase not in _TERMINAL and phase != "prepared" and self._now() >= expires_at:
            phase = "expired"
        attempt.state = phase
        attempt._last = state  # type: ignore[attr-defined]
        if phase == "prepared" and attempt.receipt_id is None:
            attempt.receipt_id = self._mint_id()
            self._promote_entry(attempt, dto.RegistryPhase.PREPARED)
        elif phase in ("failed", "expired"):
            # The login process has terminated without committing, so free its transient
            # resources NOW (slot proxy + per-scope run lock + run workdir) instead of
            # holding them until the ticket TTL (~10 min) — that hold blocked the next
            # login/probe/run on the slot. The credential volume is left for the
            # coordinator's cleanup_connection to revoke (retry/cancel unaffected).
            self._release_transient(attempt)
        return phase

    def _snapshot(self, attempt: _Attempt, phase: str):
        poll = dto.RetryMs(_DEFAULT_POLL_MS)
        # Only the non-terminal states carry an expiry; rebuilt (recovered) attempts
        # have no ticket and only ever render committed/interrupted, so access the
        # ticket lazily.
        if phase in ("queued", "starting", "verifying"):
            expires_at = dto.UnixSec(attempt.ticket.expires_at.value)
            return dto.LoginProgress(_LOGIN_STATE[phase], expires_at, poll)
        if phase == "awaiting_user":
            state = getattr(attempt, "_last", None)
            if state is None or state.challenge is None:
                raise Reject(dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
            ticket_expires = attempt.ticket.expires_at.value
            return dto.LoginAwaitingUser(
                dto.LoginState.AWAITING_USER, dto.UnixSec(ticket_expires), poll,
                self._challenge_dto(attempt.provider, state.challenge, ticket_expires),
            )
        if phase == "prepared":
            state = getattr(attempt, "_last", None)
            label = None
            if state is not None and state.account_label is not None:
                label = dto.Label(state.account_label)
            return dto.LoginPrepared(
                dto.LoginState.PREPARED, dto.Id(attempt.receipt_id),
                dto.Id(attempt.storage_handle), label,
            )
        if phase == "committed":
            return dto.LoginCommitted(
                dto.LoginState.COMMITTED, dto.Id(attempt.receipt_id),
                dto.Id(attempt.storage_handle),
            )
        if phase == "failed":
            state = getattr(attempt, "_last", None)
            reason = state.failure if state is not None and state.failure else "provider_unavailable"
            return dto.LoginFailed(dto.LoginState.FAILED, dto.LoginFailure(reason))
        return dto.LoginTerminal(_LOGIN_STATE[phase])

    def _promote_entry(self, attempt: _Attempt, phase: dto.RegistryPhase) -> None:
        if attempt.entry_id is None:
            attempt.entry_id = self._mint_id()
        entry = self._entries.get(attempt.entry_id)
        receipt = attempt.receipt_id if phase in (
            dto.RegistryPhase.PREPARED, dto.RegistryPhase.COMMITTED
        ) else None
        self._entries[attempt.entry_id] = _Entry(
            attempt.entry_id, attempt.binding, phase,
            storage_handle=attempt.storage_handle, receipt_id=receipt,
            cleanup_operation_id=(entry.cleanup_operation_id if entry else None),
        )
        self._persist(self._entries[attempt.entry_id])
        self._bump()

    # -- operation handlers ---------------------------------------------- #

    def _start_login(self, request: dto.Request):
        ticket = request.body.ticket
        binding = ticket.binding
        key = self._key(binding)
        existing = self._attempts.get(key)
        if existing is not None:
            if existing.start_request_id == request.request_id or existing.ticket == ticket:
                return dto.StartLoginResult(dto.Disposition.REPLAYED, dto.RetryMs(_DEFAULT_POLL_MS))
            raise Reject(dto.ErrorCode.CONFLICT)
        store_scope = self._store_scope(binding.scope)
        try:
            handle = self._volumes.create(store_scope)
        except Exception as error:
            _report_start_failure(binding.scope.provider, "volume_create", error)
            raise Reject(dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
        # From here the volume EXISTS; any failure building/starting the driver must
        # tear it down durably (revoke), never leak a half-provisioned volume.
        try:
            driver = self._driver_for(binding.scope.provider, store_scope, handle)
            driver.start()
        except Exception as error:
            _report_start_failure(binding.scope.provider, "driver_start", error)
            self._revoke_volume(binding.scope, handle)
            raise Reject(dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
        attempt = _Attempt(
            binding=binding, ticket=ticket, provider=binding.scope.provider,
            storage_handle=handle, start_request_id=request.request_id, driver=driver,
        )
        self._attempts[key] = attempt
        self._promote_entry(attempt, dto.RegistryPhase.PENDING_LOGIN)
        return dto.StartLoginResult(dto.Disposition.ACCEPTED, dto.RetryMs(_DEFAULT_POLL_MS))

    def _observe_login(self, request: dto.Request):
        attempt = self._lookup(request.body.binding)
        return self._snapshot(attempt, self._advance(attempt))

    def _submit_code(self, request: dto.Request):
        attempt = self._lookup(request.body.binding)
        if attempt.provider is not dto.Provider.CLAUDE:
            # Codex device logins never submit a code.
            raise Reject(dto.ErrorCode.CONFLICT)
        phase = self._advance(attempt)
        if phase == "awaiting_user":
            try:
                attempt.driver.submit_code(request.body.code.value)
            except Exception:
                raise Reject(dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
            phase = self._advance(attempt)
        return self._snapshot(attempt, phase)

    def _settle_login(self, request: dto.Request):
        witness = request.body.witness
        attempt = self._lookup(witness.binding)
        phase = "committed" if attempt.committed else self._advance(attempt)
        if not attempt.committed:
            if phase != "prepared":
                raise Reject(dto.ErrorCode.CONFLICT)
            if (
                attempt.receipt_id != request.body.receipt_id.value
                or attempt.storage_handle != witness.storage_handle.value
            ):
                raise Reject(dto.ErrorCode.CONFLICT)
            attempt.committed = True
            attempt.state = "committed"
            self._promote_entry(attempt, dto.RegistryPhase.COMMITTED)
            # The provider has finished and the credentials are durably committed, so
            # free the login's TRANSIENT resources now — the slot proxy, the per-scope
            # run lock and the run workdir — WITHOUT revoke/logout (the creds persist).
            # Without this the channel keeps holding the slot lock (the health probe hits
            # a busy slot -> UNAVAILABLE) and the scope run lock (the next runtime run
            # waits it out ~120s then fails "provider failed to answer").
            self._release_transient(attempt)
        elif (
            attempt.receipt_id != request.body.receipt_id.value
            or attempt.storage_handle != witness.storage_handle.value
        ):
            raise Reject(dto.ErrorCode.CONFLICT)
        return dto.SettleLoginResult(
            dto.CommittedState.COMMITTED, dto.Id(attempt.receipt_id),
            dto.Id(attempt.storage_handle),
        )

    def _release_transient(self, attempt: _Attempt) -> None:
        """Free a login's transient resources (slot proxy + per-scope run lock + run
        workdir) ONCE, when the login has reached a terminal state — committed OR
        failed/expired. The `released` flag makes it idempotent; the driver reference is
        KEPT so a later cleanup_connection can still logout/teardown (a second channel
        teardown is an idempotent no-op). Never revokes or logs out here: for a committed
        login the credentials must persist, and for a failed/expired one the coordinator's
        cleanup_connection performs the durable revoke — so retry/cancel are unaffected. A
        release failure is swallowed (the channel quarantines the slot on a bad teardown)."""
        if attempt.released or attempt.driver is None:
            return
        attempt.released = True
        try:
            attempt.driver.release()
        except Exception:
            pass  # commit stands; a bad teardown has already quarantined the slot

    def _check_connection(self, request: dto.Request):
        ticket = request.body.ticket
        check_id = ticket.check_id
        attempt = self._find_committed(ticket.binding, ticket.storage_handle.value)
        if attempt is None or self._probe_for is None:
            return dto.CheckUnavailable(
                dto.CheckState.UNAVAILABLE, check_id, dto.CheckFailure.PROVIDER_UNAVAILABLE
            )
        # Run an ACTUAL credential probe (independent of any cached login driver, so it
        # works post-restart) against this connection's own volume. Any probe failure
        # is reported UNAVAILABLE — never a fabricated "finished".
        try:
            store_scope = self._store_scope(ticket.binding.scope)
            probe = self._probe_for(
                ticket.binding.scope.provider, store_scope, attempt.storage_handle
            )
            result = probe.probe()
        except Exception:
            return dto.CheckUnavailable(
                dto.CheckState.UNAVAILABLE, check_id, dto.CheckFailure.PROVIDER_UNAVAILABLE
            )
        return dto.CheckFinished(
            dto.CheckState.FINISHED, check_id, dto.UnixSec(self._now()),
            self._account_status(result),
        )

    def _account_status(self, result):
        state, label = result if isinstance(result, tuple) else (result, None)
        if state == "supported":
            return dto.SupportedAccount(dto.Label(label if label else "account"))
        if state == "unsupported":
            return dto.UnsupportedAccount()
        return dto.DisconnectedAccount()

    def _cleanup_connection(self, request: dto.Request):
        ticket = request.body.ticket
        target = ticket.target
        generation = ticket.target_generation.value
        op_id = ticket.operation_id.value
        if target.kind is dto.CleanupKind.CANCEL:
            attempt = self._attempts.get(
                (ticket.connection_id.value, target.attempt_id.value)
            )
            if attempt is None or not self._cleanup_matches(
                attempt, ticket.scope, ticket.connection_id.value, generation
            ):
                # Already gone (idempotent) or not ours — nothing to tear down.
                return dto.CleanupCompleted(
                    dto.CleanupState.COMPLETED, dto.LogoutResult.NOT_NEEDED
                )
            ok, logout = self._teardown(attempt, op_id, do_logout=False)
        else:  # disconnect
            attempt = self._find_connection(
                ticket.scope, ticket.connection_id.value, generation
            )
            if attempt is None:
                return dto.CleanupCompleted(
                    dto.CleanupState.COMPLETED, dto.LogoutResult.NOT_NEEDED
                )
            ok, logout = self._teardown(attempt, op_id, do_logout=True)
        if not ok:
            # Persistent state was NOT fully torn down; never report a false success.
            return dto.CleanupFailed(
                dto.CleanupState.FAILED, dto.CleanupFailedReason.CLEANUP_FAILED,
                dto.RetryMs(_DEFAULT_POLL_MS),
            )
        return dto.CleanupCompleted(dto.CleanupState.COMPLETED, dto.LogoutResult(logout))

    def _teardown(self, attempt: _Attempt, cleanup_operation_id: str, *, do_logout: bool):
        """Stop the active run and DURABLY revoke the credential state. Returns
        (True, logout_result) only when every step actually completed; on any failure
        the attempt is QUARANTINED (kept for retry) and (False, None) is returned so
        the caller reports CleanupFailed — never a false COMPLETED."""
        logout = "not_needed"
        try:
            if attempt.driver is not None:
                if do_logout:
                    # Confirm the provider session is logged out BEFORE deleting creds.
                    logout = attempt.driver.logout()
                else:
                    # Cancel + wait for an in-flight login run to actually stop.
                    attempt.driver.cancel()
        except Exception:
            self._quarantine(attempt, cleanup_operation_id)
            return (False, None)
        if not self._revoke_volume(attempt.binding.scope, attempt.storage_handle):
            self._quarantine(attempt, cleanup_operation_id)
            return (False, None)
        self._finalize_disposed(attempt, cleanup_operation_id)
        return (True, logout)

    def _revoke_volume(self, scope: dto.Scope, handle: str) -> bool:
        """Unmount AND durably revoke (delete) the persistent credential volume.
        Requires the volumes dependency to expose `revoke`; without a proven durable
        delete we return False so cleanup is honestly reported as failed."""
        try:
            store_scope = self._store_scope(scope)
        except Exception:
            return False
        revoke = getattr(self._volumes, "revoke", None)
        if revoke is None:
            return False
        close = getattr(self._volumes, "close", None)
        try:
            if close is not None:
                close(store_scope, handle)  # unmount (idempotent)
            revoke(store_scope, handle)  # durable delete of persistent creds
        except Exception:
            return False
        return True

    def _quarantine(self, attempt: _Attempt, cleanup_operation_id: str) -> None:
        if attempt.entry_id is None:
            attempt.entry_id = self._mint_id()
        self._entries[attempt.entry_id] = _Entry(
            attempt.entry_id, attempt.binding, dto.RegistryPhase.QUARANTINED,
            storage_handle=attempt.storage_handle,
            cleanup_operation_id=cleanup_operation_id,
        )
        self._persist(self._entries[attempt.entry_id])
        self._bump()

    def _finalize_disposed(self, attempt: _Attempt, cleanup_operation_id: str) -> None:
        self._attempts.pop(self._key(attempt.binding), None)
        if attempt.entry_id is None:
            attempt.entry_id = self._mint_id()
        self._entries[attempt.entry_id] = _Entry(
            attempt.entry_id, attempt.binding, dto.RegistryPhase.DISPOSED,
            storage_handle=attempt.storage_handle,
            cleanup_operation_id=cleanup_operation_id,
        )
        self._persist(self._entries[attempt.entry_id])
        self._bump()

    def _cleanup_matches(self, attempt: _Attempt, scope, connection_id, generation) -> bool:
        b = attempt.binding
        return (
            b.scope == scope
            and b.connection_id.value == connection_id
            and b.generation.value == generation
        )

    def _find_connection(self, scope, connection_id, generation):
        for attempt in self._attempts.values():
            if self._cleanup_matches(attempt, scope, connection_id, generation):
                return attempt
        return None

    def _find_committed(self, binding: dto.ConnectionBinding, storage_handle: str):
        for attempt in self._attempts.values():
            b = attempt.binding
            if (
                attempt.committed
                and b.scope == binding.scope
                and b.connection_id == binding.connection_id
                and b.generation == binding.generation
                and attempt.storage_handle == storage_handle
            ):
                return attempt
        return None

    def _reconcile_page(self, request: dto.Request):
        body = request.body
        revision = dto.Counter(str(self._revision))
        if body.snapshot_revision is not None and body.snapshot_revision.value != revision.value:
            # The caller is paging an older snapshot; a stale cursor cannot be served
            # consistently, so refuse rather than mix revisions.
            raise Reject(dto.ErrorCode.STALE)
        after = body.after_entry_id.value if body.after_entry_id is not None else None
        ordered = sorted(self._entries.values(), key=lambda entry: entry.entry_id)
        selected = [entry for entry in ordered if after is None or entry.entry_id > after]
        page = selected[: min(body.limit, _MAX_PAGE)]
        entries = tuple(self._entry_dto(entry) for entry in page)
        # A continuation cursor is offered ONLY when more entries remain past this
        # page; it is the last returned entry_id (what validate_response requires).
        next_cursor = entries[-1].entry_id if len(page) < len(selected) else None
        return dto.ReconcilePage(revision, entries, next_cursor)

    def _entry_dto(self, entry: _Entry) -> dto.ReconcileEntry:
        optional = lambda value: dto.Id(value) if value is not None else None
        return dto.ReconcileEntry(
            dto.Id(entry.entry_id),
            self._connection_binding(entry.binding),
            entry.binding.attempt_id,
            entry.binding.operation_id,
            entry.phase,
            optional(entry.storage_handle),
            optional(entry.receipt_id),
            optional(entry.cleanup_operation_id),
        )

    _DISPATCH = {
        dto.Op.START_LOGIN: _start_login,
        dto.Op.OBSERVE_LOGIN: _observe_login,
        dto.Op.SUBMIT_CODE: _submit_code,
        dto.Op.SETTLE_LOGIN: _settle_login,
        dto.Op.CHECK_CONNECTION: _check_connection,
        dto.Op.CLEANUP_CONNECTION: _cleanup_connection,
        dto.Op.RECONCILE_PAGE: _reconcile_page,
    }


_FENCE_ERROR_CODE = {
    "Stale": dto.ErrorCode.STALE,
    "Conflict": dto.ErrorCode.CONFLICT,
    "Busy": dto.ErrorCode.BUSY,
}


def _fence_code(error) -> dto.ErrorCode:
    return _FENCE_ERROR_CODE.get(getattr(error, "category", None), dto.ErrorCode.UNAVAILABLE)


async def serve_session(session, fence, supervisor: Supervisor) -> None:
    """Own one accepted, handshaken session: register its coordinator with the fence
    on the first request, then dispatch each request UNDER a fresh fence admission
    (the check-and-commit boundary) and answer it. A fence rejection (stale/conflict/
    busy) becomes the matching error response; a transport failure ends the session.
    """
    registration = None
    try:
        while True:
            try:
                pending = await session.receive()
            except transport.TransportError:
                return
            request = pending.request
            if registration is None:
                try:
                    registration = fence.register(request.coordinator)
                except fence_mod.FenceError as error:
                    response = supervisor.error_response(request, _fence_code(error), _DEFAULT_POLL_MS)
                    await _safe_respond(session, pending, response)
                    continue
            try:
                with fence.admission(registration):
                    response = supervisor.handle(request)
            except fence_mod.FenceError as error:
                response = supervisor.error_response(request, _fence_code(error), _DEFAULT_POLL_MS)
            if not await _safe_respond(session, pending, response):
                return
    finally:
        with contextlib.suppress(Exception):
            session.close()


async def _safe_respond(session, pending, response) -> bool:
    try:
        await session.respond(pending, response)
        return True
    except transport.TransportError:
        return False


async def serve(listener, fence, supervisor: Supervisor) -> None:  # pragma: no cover - Linux service loop
    """Accept sessions forever, serving each concurrently. The supervisor MUST be
    recovered first (its handle() refuses until then)."""
    if not supervisor._reconciled:
        raise RuntimeError("supervisor.recover() must run before serve()")
    sessions = set()
    try:
        while True:
            try:
                session = await listener.accept()
            except transport.TransportError:
                continue
            task = asyncio.ensure_future(serve_session(session, fence, supervisor))
            sessions.add(task)
            task.add_done_callback(sessions.discard)
    finally:
        for task in tuple(sessions):
            task.cancel()
        if sessions:
            await asyncio.gather(*sessions, return_exceptions=True)


class FileRegistry:
    """Durable, crash-safe registry backend for `Supervisor(durable=...)`.

    One JSON object `{entry_id: record}` rewritten ATOMICALLY (temp file + fsync +
    os.replace) on every mutation, so a restart reloads exactly the last committed
    state and never a torn write. Records carry no secret — only bindings, phases,
    handles and receipt ids — so this file is safe at the supervisor's own 0600."""

    def __init__(self, path: str):
        self._path = path
        self._records: Dict[str, dict] = {}

    def load(self) -> list:
        # A MISSING file is a clean first boot (empty). A PRESENT-but-unreadable or
        # corrupt file is FAIL-CLOSED: raise so recover() aborts and the daemon
        # refuses to serve, rather than silently discarding committed connections
        # (which would strand live credentials and orphan volumes).
        try:
            with open(self._path, "r", encoding="utf-8") as handle:
                raw = handle.read()
        except FileNotFoundError:
            self._records = {}
            return []
        except OSError as error:
            raise RegistryError("unreadable") from error
        try:
            data = json.loads(raw)
        except ValueError as error:
            raise RegistryError("corrupt") from error
        if not isinstance(data, dict) or not all(
            isinstance(key, str) and isinstance(value, dict) for key, value in data.items()
        ):
            raise RegistryError("corrupt")
        self._records = dict(data)
        return list(self._records.values())

    def put(self, record: dict) -> None:
        self._records[record["entry_id"]] = record
        self._flush()

    def delete(self, entry_id: str) -> None:
        if self._records.pop(entry_id, None) is not None:
            self._flush()

    def _flush(self) -> None:
        directory = os.path.dirname(self._path) or "."
        descriptor, temporary = tempfile.mkstemp(dir=directory, prefix=".neev-registry.")
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                json.dump(self._records, handle, separators=(",", ":"))
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary, self._path)
        except BaseException:
            with contextlib.suppress(OSError):
                os.unlink(temporary)
            raise
