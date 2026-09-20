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
from dataclasses import dataclass, field
from typing import Callable, Dict, List, Optional, Tuple

import neev_supervisor_dto as dto

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
    ticket: dto.LoginTicketDto
    provider: dto.Provider
    storage_handle: str  # 32-hex volume handle
    start_request_id: str
    state: str = "starting"
    receipt_id: Optional[str] = None
    committed: bool = False
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
    ):
        self._boot_id = boot_id
        self._volumes = volumes
        self._driver_for = driver_for
        self._now = now
        self._mint_id = mint_id
        self._attempts: Dict[Tuple[str, str], _Attempt] = {}
        self._entries: Dict[str, _Entry] = {}
        self._revision = 1

    # -- public dispatch -------------------------------------------------- #

    def handle(self, request: dto.Request) -> dto.Response:
        handler = self._DISPATCH.get(request.op())
        if handler is None:  # unreachable: op() is exhaustive over the enum
            return self._error(request, dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
        try:
            body = handler(self, request)
        except Reject as reject:
            return self._error(request, reject.code, reject.retry_after_ms)
        return self._ok(request, body)

    # -- response builders ------------------------------------------------ #

    def _ok(self, request: dto.Request, body) -> dto.Response:
        return dto.Response(
            request.coordinator, self._boot_id, request.sequence, request.request_id,
            request.op(), dto.Outcome.OK, body, None,
        )

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

    def _challenge_dto(self, provider: dto.Provider, challenge: Challenge):
        if provider is dto.Provider.CODEX:
            if challenge.user_code is None:
                raise Reject(dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
            return dto.DeviceChallenge(
                challenge.verification_url,
                dto.UserCode(challenge.user_code),
                dto.UnixSec(challenge.expires_at),
            )
        return dto.AuthorizationCodeChallenge(
            challenge.verification_url, dto.UnixSec(challenge.expires_at)
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
        return phase

    def _snapshot(self, attempt: _Attempt, phase: str):
        expires_at = dto.UnixSec(attempt.ticket.expires_at.value)
        poll = dto.RetryMs(_DEFAULT_POLL_MS)
        if phase in ("queued", "starting", "verifying"):
            return dto.LoginProgress(_LOGIN_STATE[phase], expires_at, poll)
        if phase == "awaiting_user":
            state = getattr(attempt, "_last", None)
            if state is None or state.challenge is None:
                raise Reject(dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
            return dto.LoginAwaitingUser(
                dto.LoginState.AWAITING_USER, expires_at, poll,
                self._challenge_dto(attempt.provider, state.challenge),
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
        except Exception:
            raise Reject(dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
        driver = self._driver_for(binding.scope.provider, store_scope, handle)
        attempt = _Attempt(
            binding=binding, ticket=ticket, provider=binding.scope.provider,
            storage_handle=handle, start_request_id=request.request_id, driver=driver,
        )
        try:
            driver.start()
        except Exception:
            self._safe_close(store_scope, handle)
            raise Reject(dto.ErrorCode.UNAVAILABLE, _DEFAULT_POLL_MS)
        self._attempts[key] = attempt
        self._promote_entry(attempt, dto.RegistryPhase.PENDING_LOGIN)
        return dto.StartLoginResult(dto.Disposition.ACCEPTED, dto.RetryMs(_DEFAULT_POLL_MS))

    def _observe_login(self, request: dto.Request):
        binding = request.body.binding
        attempt = self._attempts.get(self._key(binding))
        if attempt is None:
            raise Reject(dto.ErrorCode.NOT_FOUND)
        return self._snapshot(attempt, self._advance(attempt))

    def _submit_code(self, request: dto.Request):
        binding = request.body.binding
        attempt = self._attempts.get(self._key(binding))
        if attempt is None:
            raise Reject(dto.ErrorCode.NOT_FOUND)
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
        attempt = self._attempts.get(self._key(witness.binding))
        if attempt is None:
            raise Reject(dto.ErrorCode.NOT_FOUND)
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
        elif (
            attempt.receipt_id != request.body.receipt_id.value
            or attempt.storage_handle != witness.storage_handle.value
        ):
            raise Reject(dto.ErrorCode.CONFLICT)
        return dto.SettleLoginResult(
            dto.CommittedState.COMMITTED, dto.Id(attempt.receipt_id),
            dto.Id(attempt.storage_handle),
        )

    def _check_connection(self, request: dto.Request):
        ticket = request.body.ticket
        check_id = ticket.check_id
        attempt = self._find_committed(ticket.binding, ticket.storage_handle.value)
        if attempt is None:
            return dto.CheckUnavailable(
                dto.CheckState.UNAVAILABLE, check_id, dto.CheckFailure.PROVIDER_UNAVAILABLE
            )
        state = attempt.driver.poll() if attempt.driver is not None else DriverState("prepared")
        account = self._account_status(state)
        return dto.CheckFinished(
            dto.CheckState.FINISHED, check_id, dto.UnixSec(self._now()), account
        )

    def _account_status(self, state: DriverState):
        if state.account_state == "supported":
            label = state.account_label if state.account_label else "account"
            return dto.SupportedAccount(dto.Label(label))
        if state.account_state == "unsupported":
            return dto.UnsupportedAccount()
        return dto.DisconnectedAccount()

    def _cleanup_connection(self, request: dto.Request):
        ticket = request.body.ticket
        target = ticket.target
        if target.kind is dto.CleanupKind.CANCEL:
            attempt = self._attempts.get(
                (ticket.connection_id.value, target.attempt_id.value)
            )
            logout = "not_needed"
            if attempt is not None:
                if attempt.driver is not None:
                    try:
                        attempt.driver.cancel()
                    except Exception:
                        pass
                self._dispose(attempt, ticket.operation_id.value)
        else:  # disconnect
            attempt = self._connection_attempt(ticket.connection_id.value)
            logout = "not_needed"
            if attempt is not None:
                if attempt.driver is not None:
                    try:
                        logout = attempt.driver.logout()
                    except Exception:
                        return dto.CleanupFailed(
                            dto.CleanupState.FAILED,
                            dto.CleanupFailedReason.CLEANUP_FAILED,
                            dto.RetryMs(_DEFAULT_POLL_MS),
                        )
                self._dispose(attempt, ticket.operation_id.value)
        return dto.CleanupCompleted(dto.CleanupState.COMPLETED, dto.LogoutResult(logout))

    def _dispose(self, attempt: _Attempt, cleanup_operation_id: str) -> None:
        try:
            self._safe_close(self._store_scope(attempt.binding.scope), attempt.storage_handle)
        except Reject:
            pass
        key = self._key(attempt.binding)
        self._attempts.pop(key, None)
        if attempt.entry_id is not None:
            self._entries[attempt.entry_id] = _Entry(
                attempt.entry_id, attempt.binding, dto.RegistryPhase.DISPOSED,
                storage_handle=attempt.storage_handle,
                cleanup_operation_id=cleanup_operation_id,
            )
            self._bump()

    def _safe_close(self, store_scope, handle: str) -> None:
        closer = getattr(self._volumes, "close", None)
        if closer is None:
            return
        try:
            closer(store_scope, handle)
        except Exception:
            pass

    def _find_committed(self, binding: dto.ConnectionBinding, storage_handle: str):
        for attempt in self._attempts.values():
            if (
                attempt.committed
                and attempt.binding.connection_id == binding.connection_id
                and attempt.storage_handle == storage_handle
            ):
                return attempt
        return None

    def _connection_attempt(self, connection_id: str):
        for attempt in self._attempts.values():
            if attempt.binding.connection_id.value == connection_id:
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
