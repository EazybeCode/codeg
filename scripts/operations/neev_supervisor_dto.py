"""Strict, pure DTO codec and sequencing state for the Neev supervisor channel."""

from dataclasses import dataclass
from enum import Enum
import json
import re


_REQUEST_MAX = 8192
_RESPONSE_MAX = 16384
_MAX_DEPTH = 16
_MAX_MEMBERS = 32
_MAX_KEY_BYTES = 64
_MAX_STRING_BYTES = 1024
_MAX_ARRAY = 32
_I64_MAX = 9223372036854775807
_ID_RE = re.compile(r"^[0-9a-f]{32}$")
_SESSION_RE = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
)
_COUNTER_RE = re.compile(r"^[1-9][0-9]{0,18}$")
_USER_CODE_RE = re.compile(r"^[A-Z0-9-]{1,64}$")
_DEVICE_URL = "https://auth.openai.com/codex/device"
# Verified against pinned Claude 2.1.267 (offline probe); legacy claude.ai is NOT
# accepted. A Claude authorize URL must START WITH this origin+path (it carries a
# query), matching control_dto.rs's `starts_with(CLAUDE_AUTHORIZE_URL)`.
_CLAUDE_AUTHORIZE_URL = "https://claude.com/cai/oauth/authorize"
# Gemini CLI's documented NO_BROWSER sign-in prints a Google authorize URL and
# reads the resulting code back ("Enter the authorization code:"), the same
# paste-code shape as Claude. Observed on the pinned 0.62.0 binary; the redirect
# is https://codeassist.google.com/authcode (source: core/code_assist/oauth2.ts).
_GOOGLE_AUTHORIZE_URL = "https://accounts.google.com/o/oauth2/v2/auth?"
_AUTHORIZATION_CODE_URLS = (_CLAUDE_AUTHORIZE_URL, _GOOGLE_AUTHORIZE_URL)
# Cursor's `agent login` prints a cursor.com sign-in URL and then POLLS: the user
# approves in their browser and pastes nothing back. Origin-level prefix only —
# the exact path is NOT verified against a pinned binary (no vendor checksum).
_BROWSER_URLS = ("https://cursor.com/",)
_MAX_URL_BYTES = 1024
# Direct (browser) sign-in: the vendor CLI's OWN authorize URL whose redirect is
# its loopback listener (direct-signin-contract.md). Pinned per provider.
_CODEX_BROWSER_AUTHORIZE_URL = "https://auth.openai.com/oauth/authorize?"
_CLAUDE_BROWSER_AUTHORIZE_URL = "https://claude.com/cai/oauth/authorize?"
_BROWSER_CALLBACK_URLS = (_CODEX_BROWSER_AUTHORIZE_URL, _CLAUDE_BROWSER_AUTHORIZE_URL)
# The only loopback callback paths the pinned CLIs listen on (measured:
# codex-cli 0.155.1 /auth/callback on :1455; claude 2.1.267 /callback on a
# random port). Anything else is refused, never forwarded.
CALLBACK_PATHS = ("/auth/callback", "/callback")
_MAX_CALLBACK_QUERY_BYTES = 1024  # the strict parser caps every string at 1024
_MAX_BROWSER_URL_BYTES = 1024
_MAX_CODE_BYTES = 512


class DtoError(Exception):
    """A fixed, non-reflective DTO validation failure."""

    _CATEGORIES = ("invalid", "correlation", "sequence", "state", "expired")

    def __init__(self, category):
        self.category = (
            category
            if type(category) is str and category in self._CATEGORIES
            else "invalid"
        )
        super().__init__(self.category)

    def __repr__(self):
        return "DtoError(%r)" % self.category


class _SafeRepr:
    def __repr__(self):
        return type(self).__name__ + "()"


def _exact(value, expected):
    if type(value) is not expected:
        raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class Id(_SafeRepr):
    value: str

    def __post_init__(self):
        if type(self.value) is not str or _ID_RE.fullmatch(self.value) is None:
            raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class SessionId(_SafeRepr):
    value: str

    def __post_init__(self):
        if type(self.value) is not str or _SESSION_RE.fullmatch(self.value) is None:
            raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class Counter(_SafeRepr):
    value: str

    def __post_init__(self):
        if (
            type(self.value) is not str
            or _COUNTER_RE.fullmatch(self.value) is None
            or int(self.value) > _I64_MAX
        ):
            raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class OwnerId(_SafeRepr):
    value: int

    def __post_init__(self):
        if type(self.value) is not int or not 1 <= self.value <= 2147483647:
            raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class UnixSec(_SafeRepr):
    value: int

    def __post_init__(self):
        if type(self.value) is not int or not 0 <= self.value <= 253402300799:
            raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class RetryMs(_SafeRepr):
    value: int

    def __post_init__(self):
        if type(self.value) is not int or not 250 <= self.value <= 30000:
            raise DtoError("invalid")


def _label_character_allowed(character):
    code = ord(character)
    return not (
        code <= 0x1F
        or 0x7F <= code <= 0x9F
        or code == 0x061C
        or code in (0x200E, 0x200F)
        or 0x202A <= code <= 0x202E
        or 0x2066 <= code <= 0x2069
        or 0xD800 <= code <= 0xDFFF
    )


@dataclass(frozen=True, repr=False)
class Label(_SafeRepr):
    value: str

    def __post_init__(self):
        if (
            type(self.value) is not str
            or not 1 <= len(self.value) <= 160
            or len(self.value.encode("utf-8", "surrogatepass")) > 640
            or not all(_label_character_allowed(c) for c in self.value)
        ):
            raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class UserCode(_SafeRepr):
    value: str

    def __post_init__(self):
        if type(self.value) is not str or _USER_CODE_RE.fullmatch(self.value) is None:
            raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class AuthorizationCode(_SafeRepr):
    """An authorization code pasted back by the user. A short-lived bearer secret:
    `_SafeRepr` keeps it out of reprs, and it must never be logged. Bounded, single
    line, printable — matches control_dto.rs's AuthorizationCode."""

    value: str

    def __post_init__(self):
        if (
            type(self.value) is not str
            or not 1 <= len(self.value) <= _MAX_CODE_BYTES
            or any(ch.isspace() or (ord(ch) < 0x20 or ord(ch) == 0x7F) for ch in self.value)
        ):
            raise DtoError("invalid")


class Provider(Enum):
    CODEX = "codex"
    CLAUDE = "claude"
    # ACP providers (neev_acp_stream_adapter). Expressible on the control plane;
    # whether the product offers them is decided by the coordinator, not here.
    CURSOR = "cursor"
    GEMINI = "gemini"
    OPENCODE = "opencode"


class Op(Enum):
    START_LOGIN = "start_login"
    OBSERVE_LOGIN = "observe_login"
    SETTLE_LOGIN = "settle_login"
    CHECK_CONNECTION = "check_connection"
    CLEANUP_CONNECTION = "cleanup_connection"
    RECONCILE_PAGE = "reconcile_page"
    # Delivers the authorization code a Claude-style login asks the user to paste
    # back. Codex device logins never use it. (Seventh op; supersedes the freeze.)
    SUBMIT_CODE = "submit_code"
    # Delivers the provider's redirect query (code/state) captured from the
    # user's browser to the CLI's own loopback listener (direct sign-in).
    DELIVER_CALLBACK = "deliver_callback"


class Outcome(Enum):
    OK = "ok"
    ERROR = "error"


class ErrorCode(Enum):
    NOT_FOUND = "not_found"
    CONFLICT = "conflict"
    STALE = "stale"
    EXPIRED = "expired"
    BUSY = "busy"
    UNAVAILABLE = "unavailable"
    # A delivered callback did not match this attempt (state/shape). The attempt
    # is left alive and the CLI is never touched.
    REJECTED = "rejected"


class WitnessKind(Enum):
    COMPLETED_CONNECT = "completed_connect"


class Disposition(Enum):
    ACCEPTED = "accepted"
    REPLAYED = "replayed"


class CommittedState(Enum):
    COMMITTED = "committed"


class CleanupFailedReason(Enum):
    CLEANUP_FAILED = "cleanup_failed"


class LoginState(Enum):
    QUEUED = "queued"
    STARTING = "starting"
    AWAITING_USER = "awaiting_user"
    VERIFYING = "verifying"
    PREPARED = "prepared"
    COMMITTED = "committed"
    FAILED = "failed"
    EXPIRED = "expired"
    CANCELED = "canceled"
    INTERRUPTED = "interrupted"


class LoginFailure(Enum):
    PROVIDER_UNAVAILABLE = "provider_unavailable"
    REAUTH_REQUIRED = "reauth_required"
    UNSUPPORTED_ACCOUNT = "unsupported_account"


class AccountState(Enum):
    SUPPORTED = "supported"
    DISCONNECTED = "disconnected"
    UNSUPPORTED = "unsupported"


class CheckState(Enum):
    QUEUED = "queued"
    CHECKING = "checking"
    FINISHED = "finished"
    UNAVAILABLE = "unavailable"


class CheckFailure(Enum):
    PROVIDER_UNAVAILABLE = "provider_unavailable"
    INTERRUPTED = "interrupted"
    EXPIRED = "expired"


class CleanupKind(Enum):
    CANCEL = "cancel"
    DISCONNECT = "disconnect"


class CleanupState(Enum):
    PENDING = "pending"
    COMPLETED = "completed"
    FAILED = "failed"


class CleanupPhase(Enum):
    QUEUED = "queued"
    CANCELING = "canceling"
    LOGGING_OUT = "logging_out"
    STOPPING = "stopping"
    DISPOSING = "disposing"


class LogoutResult(Enum):
    CONFIRMED = "confirmed"
    NOT_CONFIRMED = "not_confirmed"
    NOT_NEEDED = "not_needed"


class RegistryPhase(Enum):
    ALLOCATION_PENDING = "allocation_pending"
    PENDING_LOGIN = "pending_login"
    PREPARED = "prepared"
    COMMITTED = "committed"
    CLEANUP_PENDING = "cleanup_pending"
    QUARANTINED = "quarantined"
    DISPOSED = "disposed"


@dataclass(frozen=True, repr=False)
class Scope(_SafeRepr):
    org_id: OwnerId
    user_id: OwnerId
    provider: Provider

    def __post_init__(self):
        _exact(self.org_id, OwnerId)
        _exact(self.user_id, OwnerId)
        _exact(self.provider, Provider)


@dataclass(frozen=True, repr=False)
class Coordinator(_SafeRepr):
    epoch: Counter
    boot_id: Id

    def __post_init__(self):
        _exact(self.epoch, Counter)
        _exact(self.boot_id, Id)


@dataclass(frozen=True, repr=False)
class LoginBinding(_SafeRepr):
    scope: Scope
    connection_id: Id
    generation: Counter
    session_id: SessionId
    attempt_id: Id
    operation_id: Id

    def __post_init__(self):
        _exact(self.scope, Scope)
        _exact(self.connection_id, Id)
        _exact(self.generation, Counter)
        _exact(self.session_id, SessionId)
        _exact(self.attempt_id, Id)
        _exact(self.operation_id, Id)


@dataclass(frozen=True, repr=False)
class LoginTicketDto(_SafeRepr):
    binding: LoginBinding
    lease_owner: Id
    expires_at: UnixSec

    def __post_init__(self):
        _exact(self.binding, LoginBinding)
        _exact(self.lease_owner, Id)
        _exact(self.expires_at, UnixSec)


@dataclass(frozen=True, repr=False)
class ConnectionBinding(_SafeRepr):
    scope: Scope
    connection_id: Id
    generation: Counter

    def __post_init__(self):
        _exact(self.scope, Scope)
        _exact(self.connection_id, Id)
        _exact(self.generation, Counter)


@dataclass(frozen=True, repr=False)
class CheckTicketDto(_SafeRepr):
    binding: ConnectionBinding
    session_id: SessionId
    storage_handle: Id
    check_id: Id
    expires_at: UnixSec

    def __post_init__(self):
        _exact(self.binding, ConnectionBinding)
        _exact(self.session_id, SessionId)
        _exact(self.storage_handle, Id)
        _exact(self.check_id, Id)
        _exact(self.expires_at, UnixSec)


@dataclass(frozen=True, repr=False)
class CommittedLoginWitnessDto(_SafeRepr):
    kind: WitnessKind
    binding: LoginBinding
    storage_handle: Id
    completed_at: UnixSec

    def __post_init__(self):
        _exact(self.kind, WitnessKind)
        if self.kind is not WitnessKind.COMPLETED_CONNECT:
            raise DtoError("invalid")
        _exact(self.binding, LoginBinding)
        _exact(self.storage_handle, Id)
        _exact(self.completed_at, UnixSec)


@dataclass(frozen=True, repr=False)
class CleanupTarget(_SafeRepr):
    kind: CleanupKind
    attempt_id: object

    def __post_init__(self):
        _exact(self.kind, CleanupKind)
        if self.kind is CleanupKind.CANCEL:
            _exact(self.attempt_id, Id)
        elif self.attempt_id is not None:
            raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class CleanupTicketDto(_SafeRepr):
    scope: Scope
    connection_id: Id
    operation_id: Id
    target: CleanupTarget
    target_generation: Counter
    result_generation: Counter
    lease_owner: Id
    lease_expires_at: UnixSec

    def __post_init__(self):
        _exact(self.scope, Scope)
        _exact(self.connection_id, Id)
        _exact(self.operation_id, Id)
        _exact(self.target, CleanupTarget)
        _exact(self.target_generation, Counter)
        _exact(self.result_generation, Counter)
        _exact(self.lease_owner, Id)
        _exact(self.lease_expires_at, UnixSec)
        target = int(self.target_generation.value)
        if target == _I64_MAX or int(self.result_generation.value) != target + 1:
            raise DtoError("invalid")


class LoginSnapshot(_SafeRepr):
    pass


@dataclass(frozen=True, repr=False)
class LoginProgress(LoginSnapshot):
    state: LoginState
    expires_at: UnixSec
    poll_after_ms: RetryMs

    def __post_init__(self):
        _exact(self.state, LoginState)
        if self.state not in (
            LoginState.QUEUED,
            LoginState.STARTING,
            LoginState.VERIFYING,
        ):
            raise DtoError("invalid")
        _exact(self.expires_at, UnixSec)
        _exact(self.poll_after_ms, RetryMs)


@dataclass(frozen=True, repr=False)
class DeviceChallenge(_SafeRepr):
    """Codex (and OpenCode's ChatGPT headless login, which uses the SAME OpenAI
    device URL): the user enters `user_code` at the fixed verification URL."""

    verification_url: str
    user_code: UserCode
    expires_at: UnixSec

    def __post_init__(self):
        if type(self.verification_url) is not str or self.verification_url != _DEVICE_URL:
            raise DtoError("invalid")
        _exact(self.user_code, UserCode)
        _exact(self.expires_at, UnixSec)


@dataclass(frozen=True, repr=False)
class AuthorizationCodeChallenge(_SafeRepr):
    """Claude / Gemini: the user opens `verification_url`, approves, and pastes the
    resulting authorization code back (delivered by SUBMIT_CODE). No user_code."""

    verification_url: str
    expires_at: UnixSec

    def __post_init__(self):
        if (
            type(self.verification_url) is not str
            or len(self.verification_url) > _MAX_URL_BYTES
            or not self.verification_url.startswith(_AUTHORIZATION_CODE_URLS)
        ):
            raise DtoError("invalid")
        _exact(self.expires_at, UnixSec)


@dataclass(frozen=True, repr=False)
class BrowserChallenge(_SafeRepr):
    """Cursor: the user opens `verification_url` and approves; the CLI polls for the
    result itself. Nothing is pasted back and there is no user_code."""

    verification_url: str
    expires_at: UnixSec

    def __post_init__(self):
        if (
            type(self.verification_url) is not str
            or len(self.verification_url) > _MAX_URL_BYTES
            or not self.verification_url.startswith(_BROWSER_URLS)
        ):
            raise DtoError("invalid")
        _exact(self.expires_at, UnixSec)


@dataclass(frozen=True, repr=False)
class BrowserCallbackChallenge(_SafeRepr):
    """Claude / Codex direct sign-in: open `verification_url`; the provider
    redirects the browser to the CLI's loopback `callback_port`/`callback_path`,
    which Neev relays with DELIVER_CALLBACK. `fallback_url` (Claude) is the same
    login's paste-code URL, still accepted through SUBMIT_CODE."""

    verification_url: str
    callback_port: int
    callback_path: str
    fallback_url: object
    expires_at: UnixSec

    def __post_init__(self):
        for url in (self.verification_url,) + (
            (self.fallback_url,) if self.fallback_url is not None else ()
        ):
            if (
                type(url) is not str
                or len(url) > _MAX_BROWSER_URL_BYTES
                or not url.startswith(_BROWSER_CALLBACK_URLS)
                or any(ord(ch) < 0x21 or ord(ch) == 0x7F for ch in url)
            ):
                raise DtoError("invalid")
        if (
            type(self.callback_port) is not int
            or isinstance(self.callback_port, bool)
            or not 1024 <= self.callback_port <= 65535
        ):
            raise DtoError("invalid")
        if self.callback_path not in CALLBACK_PATHS:
            raise DtoError("invalid")
        _exact(self.expires_at, UnixSec)


@dataclass(frozen=True, repr=False)
class CallbackQuery(_SafeRepr):
    """The raw query string of the provider's loopback redirect. A short-lived
    secret (it carries the authorization code): never logged or reflected.
    Shape only here; the supervisor parses and binds it to the attempt."""

    value: str

    def __post_init__(self):
        if (
            type(self.value) is not str
            or not 1 <= len(self.value.encode("utf-8", "surrogatepass")) <= _MAX_CALLBACK_QUERY_BYTES
            or any(ord(ch) < 0x21 or ord(ch) >= 0x7F for ch in self.value)
        ):
            raise DtoError("invalid")


_CHALLENGES = (DeviceChallenge, AuthorizationCodeChallenge, BrowserChallenge,
               BrowserCallbackChallenge)


@dataclass(frozen=True, repr=False)
class LoginAwaitingUser(LoginSnapshot):
    state: LoginState
    expires_at: UnixSec
    poll_after_ms: RetryMs
    challenge: object

    def __post_init__(self):
        if self.state is not LoginState.AWAITING_USER:
            raise DtoError("invalid")
        _exact(self.expires_at, UnixSec)
        _exact(self.poll_after_ms, RetryMs)
        if type(self.challenge) not in _CHALLENGES:
            raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class LoginPrepared(LoginSnapshot):
    state: LoginState
    receipt_id: Id
    storage_handle: Id
    account_label: object

    def __post_init__(self):
        if self.state is not LoginState.PREPARED:
            raise DtoError("invalid")
        _exact(self.receipt_id, Id)
        _exact(self.storage_handle, Id)
        if self.account_label is not None:
            _exact(self.account_label, Label)


@dataclass(frozen=True, repr=False)
class LoginCommitted(LoginSnapshot):
    state: LoginState
    receipt_id: Id
    storage_handle: Id

    def __post_init__(self):
        if self.state is not LoginState.COMMITTED:
            raise DtoError("invalid")
        _exact(self.receipt_id, Id)
        _exact(self.storage_handle, Id)


@dataclass(frozen=True, repr=False)
class LoginFailed(LoginSnapshot):
    state: LoginState
    reason: LoginFailure

    def __post_init__(self):
        if self.state is not LoginState.FAILED:
            raise DtoError("invalid")
        _exact(self.reason, LoginFailure)


@dataclass(frozen=True, repr=False)
class LoginTerminal(LoginSnapshot):
    state: LoginState

    def __post_init__(self):
        _exact(self.state, LoginState)
        if self.state not in (
            LoginState.EXPIRED,
            LoginState.CANCELED,
            LoginState.INTERRUPTED,
        ):
            raise DtoError("invalid")


class AccountStatus(_SafeRepr):
    pass


@dataclass(frozen=True, repr=False)
class SupportedAccount(AccountStatus):
    account_label: Label

    def __post_init__(self):
        _exact(self.account_label, Label)


@dataclass(frozen=True, repr=False)
class DisconnectedAccount(AccountStatus):
    pass


@dataclass(frozen=True, repr=False)
class UnsupportedAccount(AccountStatus):
    pass


LOGIN_METHODS = ("browser", "device")


@dataclass(frozen=True, repr=False)
class StartLoginRequest(_SafeRepr):
    ticket: LoginTicketDto
    # None (ABSENT on the wire) = today's behaviour: Codex device code, Claude
    # paste-code — what the currently deployed coordinator expects, since it
    # cannot decode a browser_callback challenge. "browser" = direct sign-in;
    # "device" = Codex device code, chosen explicitly. Carried beside the
    # ticket, not inside it, so ticket replay comparison is unchanged.
    method: object = None

    def __post_init__(self):
        _exact(self.ticket, LoginTicketDto)
        if self.method is not None and self.method not in LOGIN_METHODS:
            raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class ObserveLoginRequest(_SafeRepr):
    binding: LoginBinding

    def __post_init__(self):
        _exact(self.binding, LoginBinding)


@dataclass(frozen=True, repr=False)
class SettleLoginRequest(_SafeRepr):
    receipt_id: Id
    witness: CommittedLoginWitnessDto

    def __post_init__(self):
        _exact(self.receipt_id, Id)
        _exact(self.witness, CommittedLoginWitnessDto)


@dataclass(frozen=True, repr=False)
class CheckConnectionRequest(_SafeRepr):
    ticket: CheckTicketDto

    def __post_init__(self):
        _exact(self.ticket, CheckTicketDto)


@dataclass(frozen=True, repr=False)
class CleanupConnectionRequest(_SafeRepr):
    ticket: CleanupTicketDto

    def __post_init__(self):
        _exact(self.ticket, CleanupTicketDto)


@dataclass(frozen=True, repr=False)
class SubmitCodeRequest(_SafeRepr):
    binding: LoginBinding
    code: AuthorizationCode

    def __post_init__(self):
        _exact(self.binding, LoginBinding)
        _exact(self.code, AuthorizationCode)


@dataclass(frozen=True, repr=False)
class DeliverCallbackRequest(_SafeRepr):
    binding: LoginBinding
    query: CallbackQuery

    def __post_init__(self):
        _exact(self.binding, LoginBinding)
        _exact(self.query, CallbackQuery)


@dataclass(frozen=True, repr=False)
class ReconcilePageRequest(_SafeRepr):
    snapshot_revision: object
    after_entry_id: object
    limit: int

    def __post_init__(self):
        if (self.snapshot_revision is None) != (self.after_entry_id is None):
            raise DtoError("invalid")
        if self.snapshot_revision is not None:
            _exact(self.snapshot_revision, Counter)
            _exact(self.after_entry_id, Id)
        if type(self.limit) is not int or not 1 <= self.limit <= 32:
            raise DtoError("invalid")


_REQUEST_OPS = {
    StartLoginRequest: Op.START_LOGIN,
    ObserveLoginRequest: Op.OBSERVE_LOGIN,
    SettleLoginRequest: Op.SETTLE_LOGIN,
    CheckConnectionRequest: Op.CHECK_CONNECTION,
    CleanupConnectionRequest: Op.CLEANUP_CONNECTION,
    ReconcilePageRequest: Op.RECONCILE_PAGE,
    SubmitCodeRequest: Op.SUBMIT_CODE,
    DeliverCallbackRequest: Op.DELIVER_CALLBACK,
}


@dataclass(frozen=True, repr=False)
class Request(_SafeRepr):
    coordinator: Coordinator
    sequence: Counter
    request_id: Id
    body: object

    def __post_init__(self):
        _exact(self.coordinator, Coordinator)
        _exact(self.sequence, Counter)
        _exact(self.request_id, Id)
        if type(self.body) not in _REQUEST_OPS:
            raise DtoError("invalid")

    def op(self):
        return _REQUEST_OPS[type(self.body)]


@dataclass(frozen=True, repr=False)
class StartLoginResult(_SafeRepr):
    disposition: Disposition
    poll_after_ms: RetryMs

    def __post_init__(self):
        _exact(self.disposition, Disposition)
        _exact(self.poll_after_ms, RetryMs)


@dataclass(frozen=True, repr=False)
class SettleLoginResult(_SafeRepr):
    state: CommittedState
    receipt_id: Id
    storage_handle: Id

    def __post_init__(self):
        _exact(self.state, CommittedState)
        _exact(self.receipt_id, Id)
        _exact(self.storage_handle, Id)


@dataclass(frozen=True, repr=False)
class CheckPending(_SafeRepr):
    state: CheckState
    check_id: Id
    poll_after_ms: RetryMs

    def __post_init__(self):
        _exact(self.state, CheckState)
        if self.state not in (CheckState.QUEUED, CheckState.CHECKING):
            raise DtoError("invalid")
        _exact(self.check_id, Id)
        _exact(self.poll_after_ms, RetryMs)


@dataclass(frozen=True, repr=False)
class CheckFinished(_SafeRepr):
    state: CheckState
    check_id: Id
    observed_at: UnixSec
    account: AccountStatus

    def __post_init__(self):
        if self.state is not CheckState.FINISHED:
            raise DtoError("invalid")
        _exact(self.check_id, Id)
        _exact(self.observed_at, UnixSec)
        if type(self.account) not in (
            SupportedAccount,
            DisconnectedAccount,
            UnsupportedAccount,
        ):
            raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class CheckUnavailable(_SafeRepr):
    state: CheckState
    check_id: Id
    reason: CheckFailure

    def __post_init__(self):
        if self.state is not CheckState.UNAVAILABLE:
            raise DtoError("invalid")
        _exact(self.check_id, Id)
        _exact(self.reason, CheckFailure)


@dataclass(frozen=True, repr=False)
class CleanupPending(_SafeRepr):
    state: CleanupState
    phase: CleanupPhase
    poll_after_ms: RetryMs

    def __post_init__(self):
        if self.state is not CleanupState.PENDING:
            raise DtoError("invalid")
        _exact(self.phase, CleanupPhase)
        _exact(self.poll_after_ms, RetryMs)


@dataclass(frozen=True, repr=False)
class CleanupCompleted(_SafeRepr):
    state: CleanupState
    provider_logout: LogoutResult

    def __post_init__(self):
        if self.state is not CleanupState.COMPLETED:
            raise DtoError("invalid")
        _exact(self.provider_logout, LogoutResult)


@dataclass(frozen=True, repr=False)
class CleanupFailed(_SafeRepr):
    state: CleanupState
    reason: CleanupFailedReason
    retry_after_ms: RetryMs

    def __post_init__(self):
        if self.state is not CleanupState.FAILED:
            raise DtoError("invalid")
        _exact(self.reason, CleanupFailedReason)
        _exact(self.retry_after_ms, RetryMs)


@dataclass(frozen=True, repr=False)
class ReconcileEntry(_SafeRepr):
    entry_id: Id
    binding: ConnectionBinding
    attempt_id: Id
    operation_id: Id
    phase: RegistryPhase
    storage_handle: object
    receipt_id: object
    cleanup_operation_id: object

    def __post_init__(self):
        _exact(self.entry_id, Id)
        _exact(self.binding, ConnectionBinding)
        _exact(self.attempt_id, Id)
        _exact(self.operation_id, Id)
        _exact(self.phase, RegistryPhase)
        for value in (self.storage_handle, self.receipt_id, self.cleanup_operation_id):
            if value is not None:
                _exact(value, Id)
        handle = self.storage_handle is not None
        receipt = self.receipt_id is not None
        cleanup = self.cleanup_operation_id is not None
        valid = {
            RegistryPhase.ALLOCATION_PENDING: not handle and not receipt and not cleanup,
            RegistryPhase.PENDING_LOGIN: handle and not receipt and not cleanup,
            RegistryPhase.PREPARED: handle and receipt and not cleanup,
            RegistryPhase.COMMITTED: handle and receipt and not cleanup,
            RegistryPhase.CLEANUP_PENDING: cleanup and (not receipt or handle),
            RegistryPhase.QUARANTINED: not receipt or handle,
            RegistryPhase.DISPOSED: not receipt or handle,
        }[self.phase]
        if not valid:
            raise DtoError("invalid")


@dataclass(frozen=True, repr=False)
class ReconcilePage(_SafeRepr):
    snapshot_revision: Counter
    entries: tuple
    next_after_entry_id: object

    def __post_init__(self):
        _exact(self.snapshot_revision, Counter)
        if type(self.entries) is not tuple or len(self.entries) > 32:
            raise DtoError("invalid")
        for entry in self.entries:
            _exact(entry, ReconcileEntry)
        if self.next_after_entry_id is not None:
            _exact(self.next_after_entry_id, Id)


@dataclass(frozen=True, repr=False)
class AppError(_SafeRepr):
    code: ErrorCode
    retry_after_ms: object

    def __post_init__(self):
        _exact(self.code, ErrorCode)
        needs_hint = self.code in (ErrorCode.BUSY, ErrorCode.UNAVAILABLE)
        if needs_hint:
            _exact(self.retry_after_ms, RetryMs)
        elif self.retry_after_ms is not None:
            raise DtoError("invalid")


# A login snapshot answers BOTH observe_login and submit_code (submitting a code
# advances the same login state machine), matching control_dto.rs's
# `Op::ObserveLogin | Op::SubmitCode => schema_login_snapshot`. Every other body
# type answers exactly one op.
_LOGIN_SNAPSHOT_OPS = frozenset((Op.OBSERVE_LOGIN, Op.SUBMIT_CODE, Op.DELIVER_CALLBACK))
_SUCCESS_OPS = {
    StartLoginResult: frozenset((Op.START_LOGIN,)),
    LoginProgress: _LOGIN_SNAPSHOT_OPS,
    LoginAwaitingUser: _LOGIN_SNAPSHOT_OPS,
    LoginPrepared: _LOGIN_SNAPSHOT_OPS,
    LoginCommitted: _LOGIN_SNAPSHOT_OPS,
    LoginFailed: _LOGIN_SNAPSHOT_OPS,
    LoginTerminal: _LOGIN_SNAPSHOT_OPS,
    SettleLoginResult: frozenset((Op.SETTLE_LOGIN,)),
    CheckPending: frozenset((Op.CHECK_CONNECTION,)),
    CheckFinished: frozenset((Op.CHECK_CONNECTION,)),
    CheckUnavailable: frozenset((Op.CHECK_CONNECTION,)),
    CleanupPending: frozenset((Op.CLEANUP_CONNECTION,)),
    CleanupCompleted: frozenset((Op.CLEANUP_CONNECTION,)),
    CleanupFailed: frozenset((Op.CLEANUP_CONNECTION,)),
    ReconcilePage: frozenset((Op.RECONCILE_PAGE,)),
}


@dataclass(frozen=True, repr=False)
class Response(_SafeRepr):
    coordinator: Coordinator
    supervisor_boot_id: Id
    sequence: Counter
    request_id: Id
    operation: Op
    outcome: Outcome
    body: object
    error: object = None

    def __post_init__(self):
        _exact(self.coordinator, Coordinator)
        _exact(self.supervisor_boot_id, Id)
        _exact(self.sequence, Counter)
        _exact(self.request_id, Id)
        _exact(self.operation, Op)
        _exact(self.outcome, Outcome)
        if self.outcome is Outcome.OK:
            if self.error is not None or type(self.body) not in _SUCCESS_OPS:
                raise DtoError("invalid")
            if self.operation not in _SUCCESS_OPS[type(self.body)]:
                raise DtoError("invalid")
        else:
            if self.body is not None:
                raise DtoError("invalid")
            _exact(self.error, AppError)

    def op(self):
        return self.operation

    def tag(self):
        if self.outcome is Outcome.ERROR:
            return self.error.code.value
        if type(self.body) is ReconcilePage:
            return "page"
        if type(self.body) is StartLoginResult:
            return self.body.disposition.value
        return self.body.state.value if isinstance(self.body.state, Enum) else self.body.state


def _object(pairs):
    if len(pairs) > _MAX_MEMBERS:
        raise DtoError("invalid")
    output = {}
    for key, value in pairs:
        if (
            key in output
            or not key.isascii()
            or len(key.encode("ascii")) > _MAX_KEY_BYTES
        ):
            raise DtoError("invalid")
        output[key] = value
    return output


def _scan_nesting(text):
    depth = 0
    in_string = False
    escaped = False
    for character in text:
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
        elif character == '"':
            in_string = True
        elif character in "[{":
            depth += 1
            if depth > _MAX_DEPTH:
                raise DtoError("invalid")
        elif character in "]}":
            depth -= 1
            if depth < 0:
                raise DtoError("invalid")
    if depth != 0 or in_string:
        raise DtoError("invalid")


def _check_tree(value):
    if type(value) is str:
        if (
            len(value.encode("utf-8", "surrogatepass")) > _MAX_STRING_BYTES
            or any(0xD800 <= ord(character) <= 0xDFFF for character in value)
        ):
            raise DtoError("invalid")
    elif type(value) is list:
        if len(value) > _MAX_ARRAY:
            raise DtoError("invalid")
        for item in value:
            _check_tree(item)
    elif type(value) is dict:
        for item in value.values():
            _check_tree(item)
    elif value is not None and type(value) not in (int, bool):
        raise DtoError("invalid")


def _parse_int(token):
    if token != "0" and (not token.isdigit() or token.startswith("0")):
        raise DtoError("invalid")
    return int(token)


def _reject_number(_):
    raise DtoError("invalid")


def _strict_parse(payload, maximum):
    if type(payload) is not bytes or not payload or len(payload) > maximum:
        raise DtoError("invalid")
    try:
        text = payload.decode("utf-8")
        if text.startswith("\ufeff"):
            raise DtoError("invalid")
        _scan_nesting(text)
        value = json.loads(
            text,
            object_pairs_hook=_object,
            parse_float=_reject_number,
            parse_int=_parse_int,
            parse_constant=_reject_number,
        )
        if type(value) is not dict:
            raise DtoError("invalid")
        _check_tree(value)
        return value
    except DtoError:
        raise
    except Exception:
        pass
    raise DtoError("invalid")


def _keys(value, expected):
    if type(value) is not dict or set(value) != set(expected):
        raise DtoError("invalid")


def _enum(kind, value):
    if type(value) is not str:
        raise DtoError("invalid")
    try:
        result = kind(value)
    except ValueError:
        result = None
    if result is None:
        raise DtoError("invalid")
    return result


def _scope(value):
    _keys(value, ("org_id", "user_id", "provider"))
    return Scope(
        OwnerId(value["org_id"]),
        OwnerId(value["user_id"]),
        _enum(Provider, value["provider"]),
    )


def _coordinator(value):
    _keys(value, ("epoch", "boot_id"))
    return Coordinator(Counter(value["epoch"]), Id(value["boot_id"]))


def _login_binding(value):
    _keys(
        value,
        (
            "scope",
            "connection_id",
            "generation",
            "session_id",
            "attempt_id",
            "operation_id",
        ),
    )
    return LoginBinding(
        _scope(value["scope"]),
        Id(value["connection_id"]),
        Counter(value["generation"]),
        SessionId(value["session_id"]),
        Id(value["attempt_id"]),
        Id(value["operation_id"]),
    )


def _connection_binding(value):
    _keys(value, ("scope", "connection_id", "generation"))
    return ConnectionBinding(
        _scope(value["scope"]), Id(value["connection_id"]), Counter(value["generation"])
    )


def _login_ticket(value):
    _keys(value, ("binding", "lease_owner", "expires_at"))
    return LoginTicketDto(
        _login_binding(value["binding"]),
        Id(value["lease_owner"]),
        UnixSec(value["expires_at"]),
    )


def _check_ticket(value):
    _keys(
        value,
        ("binding", "session_id", "storage_handle", "check_id", "expires_at"),
    )
    return CheckTicketDto(
        _connection_binding(value["binding"]),
        SessionId(value["session_id"]),
        Id(value["storage_handle"]),
        Id(value["check_id"]),
        UnixSec(value["expires_at"]),
    )


def _witness(value):
    _keys(value, ("kind", "binding", "storage_handle", "completed_at"))
    if value["kind"] != "completed_connect":
        raise DtoError("invalid")
    return CommittedLoginWitnessDto(
        _enum(WitnessKind, value["kind"]),
        _login_binding(value["binding"]),
        Id(value["storage_handle"]),
        UnixSec(value["completed_at"]),
    )


def _cleanup_target(value):
    if type(value) is not dict or "kind" not in value:
        raise DtoError("invalid")
    kind = _enum(CleanupKind, value["kind"])
    if kind is CleanupKind.CANCEL:
        _keys(value, ("kind", "attempt_id"))
        return CleanupTarget(kind, Id(value["attempt_id"]))
    _keys(value, ("kind",))
    return CleanupTarget(kind, None)


def _cleanup_ticket(value):
    _keys(
        value,
        (
            "scope",
            "connection_id",
            "operation_id",
            "target",
            "target_generation",
            "result_generation",
            "lease_owner",
            "lease_expires_at",
        ),
    )
    return CleanupTicketDto(
        _scope(value["scope"]),
        Id(value["connection_id"]),
        Id(value["operation_id"]),
        _cleanup_target(value["target"]),
        Counter(value["target_generation"]),
        Counter(value["result_generation"]),
        Id(value["lease_owner"]),
        UnixSec(value["lease_expires_at"]),
    )


def _request_body(op, value):
    if op is Op.START_LOGIN:
        if type(value) is dict and "method" in value:
            _keys(value, ("ticket", "method"))
            return StartLoginRequest(_login_ticket(value["ticket"]), value["method"])
        _keys(value, ("ticket",))
        return StartLoginRequest(_login_ticket(value["ticket"]))
    if op is Op.OBSERVE_LOGIN:
        _keys(value, ("binding",))
        return ObserveLoginRequest(_login_binding(value["binding"]))
    if op is Op.SETTLE_LOGIN:
        _keys(value, ("receipt_id", "witness"))
        return SettleLoginRequest(Id(value["receipt_id"]), _witness(value["witness"]))
    if op is Op.CHECK_CONNECTION:
        _keys(value, ("ticket",))
        return CheckConnectionRequest(_check_ticket(value["ticket"]))
    if op is Op.CLEANUP_CONNECTION:
        _keys(value, ("ticket",))
        return CleanupConnectionRequest(_cleanup_ticket(value["ticket"]))
    if op is Op.DELIVER_CALLBACK:
        _keys(value, ("binding", "query"))
        return DeliverCallbackRequest(
            _login_binding(value["binding"]), CallbackQuery(value["query"])
        )
    if op is Op.SUBMIT_CODE:
        _keys(value, ("binding", "code"))
        return SubmitCodeRequest(
            _login_binding(value["binding"]), AuthorizationCode(value["code"])
        )
    _keys(value, ("snapshot_revision", "after_entry_id", "limit"))
    revision = (
        None if value["snapshot_revision"] is None else Counter(value["snapshot_revision"])
    )
    cursor = None if value["after_entry_id"] is None else Id(value["after_entry_id"])
    return ReconcilePageRequest(revision, cursor, value["limit"])


def decode_request(payload_bytes):
    value = _strict_parse(payload_bytes, _REQUEST_MAX)
    _keys(value, ("v", "coordinator", "sequence", "request_id", "op", "body"))
    if type(value["v"]) is not int or value["v"] != 1:
        raise DtoError("invalid")
    op = _enum(Op, value["op"])
    return Request(
        _coordinator(value["coordinator"]),
        Counter(value["sequence"]),
        Id(value["request_id"]),
        _request_body(op, value["body"]),
    )


def _encode_scope(value):
    return {
        "org_id": value.org_id.value,
        "user_id": value.user_id.value,
        "provider": value.provider.value,
    }


def _encode_login_binding(value):
    return {
        "scope": _encode_scope(value.scope),
        "connection_id": value.connection_id.value,
        "generation": value.generation.value,
        "session_id": value.session_id.value,
        "attempt_id": value.attempt_id.value,
        "operation_id": value.operation_id.value,
    }


def _encode_connection_binding(value):
    return {
        "scope": _encode_scope(value.scope),
        "connection_id": value.connection_id.value,
        "generation": value.generation.value,
    }


def _encode_request_body(body):
    if type(body) is StartLoginRequest:
        ticket = body.ticket
        encoded = {
            "ticket": {
                "binding": _encode_login_binding(ticket.binding),
                "lease_owner": ticket.lease_owner.value,
                "expires_at": ticket.expires_at.value,
            }
        }
        if body.method is not None:
            encoded["method"] = body.method
        return encoded
    if type(body) is ObserveLoginRequest:
        return {"binding": _encode_login_binding(body.binding)}
    if type(body) is SettleLoginRequest:
        witness = body.witness
        return {
            "receipt_id": body.receipt_id.value,
            "witness": {
                "kind": witness.kind.value,
                "binding": _encode_login_binding(witness.binding),
                "storage_handle": witness.storage_handle.value,
                "completed_at": witness.completed_at.value,
            },
        }
    if type(body) is CheckConnectionRequest:
        ticket = body.ticket
        return {
            "ticket": {
                "binding": _encode_connection_binding(ticket.binding),
                "session_id": ticket.session_id.value,
                "storage_handle": ticket.storage_handle.value,
                "check_id": ticket.check_id.value,
                "expires_at": ticket.expires_at.value,
            }
        }
    if type(body) is CleanupConnectionRequest:
        ticket = body.ticket
        target = {"kind": ticket.target.kind.value}
        if ticket.target.attempt_id is not None:
            target["attempt_id"] = ticket.target.attempt_id.value
        return {
            "ticket": {
                "scope": _encode_scope(ticket.scope),
                "connection_id": ticket.connection_id.value,
                "operation_id": ticket.operation_id.value,
                "target": target,
                "target_generation": ticket.target_generation.value,
                "result_generation": ticket.result_generation.value,
                "lease_owner": ticket.lease_owner.value,
                "lease_expires_at": ticket.lease_expires_at.value,
            }
        }
    if type(body) is DeliverCallbackRequest:
        return {
            "binding": _encode_login_binding(body.binding),
            "query": body.query.value,
        }
    if type(body) is SubmitCodeRequest:
        return {
            "binding": _encode_login_binding(body.binding),
            "code": body.code.value,
        }
    return {
        "snapshot_revision": (
            None
            if body.snapshot_revision is None
            else body.snapshot_revision.value
        ),
        "after_entry_id": (
            None if body.after_entry_id is None else body.after_entry_id.value
        ),
        "limit": body.limit,
    }


def _encode_json(value, maximum):
    encoded = None
    try:
        encoded = json.dumps(
            value, separators=(",", ":"), ensure_ascii=False, allow_nan=False
        ).encode("utf-8")
    except Exception:
        pass
    if encoded is None:
        raise DtoError("invalid")
    _strict_parse(encoded, maximum)
    return encoded


def _fixed_invalid(call):
    try:
        return call()
    except DtoError:
        raise
    except Exception:
        pass
    raise DtoError("invalid")


def encode_request(request):
    _exact(request, Request)
    encoded = _fixed_invalid(
        lambda: _encode_json(
            {
                "v": 1,
                "coordinator": {
                    "epoch": request.coordinator.epoch.value,
                    "boot_id": request.coordinator.boot_id.value,
                },
                "sequence": request.sequence.value,
                "request_id": request.request_id.value,
                "op": request.op().value,
                "body": _encode_request_body(request.body),
            },
            _REQUEST_MAX,
        )
    )
    decode_request(encoded)
    return encoded


def _login_challenge(value):
    if type(value) is not dict or "kind" not in value:
        raise DtoError("invalid")
    kind = value["kind"]
    if kind == "device":
        _keys(value, ("kind", "verification_url", "user_code", "expires_at"))
        return DeviceChallenge(
            value["verification_url"],
            UserCode(value["user_code"]),
            UnixSec(value["expires_at"]),
        )
    if kind == "authorization_code":
        _keys(value, ("kind", "verification_url", "expires_at"))
        return AuthorizationCodeChallenge(
            value["verification_url"], UnixSec(value["expires_at"])
        )
    if kind == "browser_callback":
        _keys(value, ("kind", "verification_url", "callback_port", "callback_path",
                      "fallback_url", "expires_at"))
        return BrowserCallbackChallenge(
            value["verification_url"], value["callback_port"], value["callback_path"],
            value["fallback_url"], UnixSec(value["expires_at"]),
        )
    if kind == "browser":
        _keys(value, ("kind", "verification_url", "expires_at"))
        return BrowserChallenge(value["verification_url"], UnixSec(value["expires_at"]))
    raise DtoError("invalid")


def _login_snapshot(value):
    if type(value) is not dict or "state" not in value:
        raise DtoError("invalid")
    state = _enum(LoginState, value["state"])
    if state in (LoginState.QUEUED, LoginState.STARTING, LoginState.VERIFYING):
        _keys(value, ("state", "expires_at", "poll_after_ms"))
        return LoginProgress(state, UnixSec(value["expires_at"]), RetryMs(value["poll_after_ms"]))
    if state is LoginState.AWAITING_USER:
        _keys(value, ("state", "expires_at", "poll_after_ms", "challenge"))
        return LoginAwaitingUser(
            state,
            UnixSec(value["expires_at"]),
            RetryMs(value["poll_after_ms"]),
            _login_challenge(value["challenge"]),
        )
    if state is LoginState.PREPARED:
        _keys(value, ("state", "receipt_id", "storage_handle", "account_label"))
        label = None if value["account_label"] is None else Label(value["account_label"])
        return LoginPrepared(state, Id(value["receipt_id"]), Id(value["storage_handle"]), label)
    if state is LoginState.COMMITTED:
        _keys(value, ("state", "receipt_id", "storage_handle"))
        return LoginCommitted(state, Id(value["receipt_id"]), Id(value["storage_handle"]))
    if state is LoginState.FAILED:
        _keys(value, ("state", "reason"))
        return LoginFailed(state, _enum(LoginFailure, value["reason"]))
    _keys(value, ("state",))
    return LoginTerminal(state)


def _account(value):
    if type(value) is not dict or "state" not in value:
        raise DtoError("invalid")
    state = _enum(AccountState, value["state"])
    if state is AccountState.SUPPORTED:
        _keys(value, ("state", "account_label"))
        return SupportedAccount(Label(value["account_label"]))
    _keys(value, ("state",))
    return DisconnectedAccount() if state is AccountState.DISCONNECTED else UnsupportedAccount()


def _reconcile_entry(value):
    _keys(
        value,
        (
            "entry_id",
            "binding",
            "attempt_id",
            "operation_id",
            "phase",
            "storage_handle",
            "receipt_id",
            "cleanup_operation_id",
        ),
    )
    optional_id = lambda raw: None if raw is None else Id(raw)
    return ReconcileEntry(
        Id(value["entry_id"]),
        _connection_binding(value["binding"]),
        Id(value["attempt_id"]),
        Id(value["operation_id"]),
        _enum(RegistryPhase, value["phase"]),
        optional_id(value["storage_handle"]),
        optional_id(value["receipt_id"]),
        optional_id(value["cleanup_operation_id"]),
    )


def _success_body(op, value):
    if op is Op.START_LOGIN:
        _keys(value, ("disposition", "poll_after_ms"))
        return StartLoginResult(
            _enum(Disposition, value["disposition"]),
            RetryMs(value["poll_after_ms"]),
        )
    if op in (Op.OBSERVE_LOGIN, Op.SUBMIT_CODE, Op.DELIVER_CALLBACK):
        return _login_snapshot(value)
    if op is Op.SETTLE_LOGIN:
        _keys(value, ("state", "receipt_id", "storage_handle"))
        return SettleLoginResult(
            _enum(CommittedState, value["state"]),
            Id(value["receipt_id"]),
            Id(value["storage_handle"]),
        )
    if op is Op.CHECK_CONNECTION:
        if type(value) is not dict or "state" not in value:
            raise DtoError("invalid")
        state = _enum(CheckState, value["state"])
        if state in (CheckState.QUEUED, CheckState.CHECKING):
            _keys(value, ("state", "check_id", "poll_after_ms"))
            return CheckPending(state, Id(value["check_id"]), RetryMs(value["poll_after_ms"]))
        if state is CheckState.FINISHED:
            _keys(value, ("state", "check_id", "observed_at", "account"))
            return CheckFinished(
                state,
                Id(value["check_id"]),
                UnixSec(value["observed_at"]),
                _account(value["account"]),
            )
        _keys(value, ("state", "check_id", "reason"))
        return CheckUnavailable(state, Id(value["check_id"]), _enum(CheckFailure, value["reason"]))
    if op is Op.CLEANUP_CONNECTION:
        if type(value) is not dict or "state" not in value:
            raise DtoError("invalid")
        state = _enum(CleanupState, value["state"])
        if state is CleanupState.PENDING:
            _keys(value, ("state", "phase", "poll_after_ms"))
            return CleanupPending(
                state, _enum(CleanupPhase, value["phase"]), RetryMs(value["poll_after_ms"])
            )
        if state is CleanupState.COMPLETED:
            _keys(value, ("state", "provider_logout"))
            return CleanupCompleted(state, _enum(LogoutResult, value["provider_logout"]))
        _keys(value, ("state", "reason", "retry_after_ms"))
        return CleanupFailed(
            state,
            _enum(CleanupFailedReason, value["reason"]),
            RetryMs(value["retry_after_ms"]),
        )
    _keys(value, ("snapshot_revision", "entries", "next_after_entry_id"))
    if type(value["entries"]) is not list:
        raise DtoError("invalid")
    return ReconcilePage(
        Counter(value["snapshot_revision"]),
        tuple(_reconcile_entry(item) for item in value["entries"]),
        None
        if value["next_after_entry_id"] is None
        else Id(value["next_after_entry_id"]),
    )


def decode_response(payload_bytes):
    value = _strict_parse(payload_bytes, _RESPONSE_MAX)
    common = (
        "v",
        "coordinator",
        "supervisor_boot_id",
        "sequence",
        "request_id",
        "op",
        "outcome",
    )
    if type(value.get("v")) is not int or value.get("v") != 1:
        raise DtoError("invalid")
    op = _enum(Op, value.get("op"))
    outcome = _enum(Outcome, value.get("outcome"))
    if outcome is Outcome.OK:
        _keys(value, common + ("body",))
        body = _success_body(op, value["body"])
        error = None
    else:
        _keys(value, common + ("error",))
        raw_error = value["error"]
        _keys(raw_error, ("code", "retry_after_ms"))
        code = _enum(ErrorCode, raw_error["code"])
        hint = (
            None
            if raw_error["retry_after_ms"] is None
            else RetryMs(raw_error["retry_after_ms"])
        )
        body = None
        error = AppError(code, hint)
    return Response(
        _coordinator(value["coordinator"]),
        Id(value["supervisor_boot_id"]),
        Counter(value["sequence"]),
        Id(value["request_id"]),
        op,
        outcome,
        body,
        error,
    )


def _encode_login_challenge(challenge):
    if type(challenge) is DeviceChallenge:
        return {
            "kind": "device",
            "verification_url": challenge.verification_url,
            "user_code": challenge.user_code.value,
            "expires_at": challenge.expires_at.value,
        }
    if type(challenge) is BrowserCallbackChallenge:
        return {
            "kind": "browser_callback",
            "verification_url": challenge.verification_url,
            "callback_port": challenge.callback_port,
            "callback_path": challenge.callback_path,
            "fallback_url": challenge.fallback_url,
            "expires_at": challenge.expires_at.value,
        }
    if type(challenge) is BrowserChallenge:
        return {
            "kind": "browser",
            "verification_url": challenge.verification_url,
            "expires_at": challenge.expires_at.value,
        }
    return {
        "kind": "authorization_code",
        "verification_url": challenge.verification_url,
        "expires_at": challenge.expires_at.value,
    }


def _encode_login_snapshot(body):
    if type(body) is LoginProgress:
        return {
            "state": body.state.value,
            "expires_at": body.expires_at.value,
            "poll_after_ms": body.poll_after_ms.value,
        }
    if type(body) is LoginAwaitingUser:
        return {
            "state": body.state.value,
            "expires_at": body.expires_at.value,
            "poll_after_ms": body.poll_after_ms.value,
            "challenge": _encode_login_challenge(body.challenge),
        }
    if type(body) is LoginPrepared:
        return {
            "state": body.state.value,
            "receipt_id": body.receipt_id.value,
            "storage_handle": body.storage_handle.value,
            "account_label": None if body.account_label is None else body.account_label.value,
        }
    if type(body) is LoginCommitted:
        return {
            "state": body.state.value,
            "receipt_id": body.receipt_id.value,
            "storage_handle": body.storage_handle.value,
        }
    if type(body) is LoginFailed:
        return {"state": body.state.value, "reason": body.reason.value}
    return {"state": body.state.value}


def _encode_account(account):
    if type(account) is SupportedAccount:
        return {"state": "supported", "account_label": account.account_label.value}
    if type(account) is DisconnectedAccount:
        return {"state": "disconnected"}
    return {"state": "unsupported"}


def _encode_entry(entry):
    optional = lambda value: None if value is None else value.value
    return {
        "entry_id": entry.entry_id.value,
        "binding": _encode_connection_binding(entry.binding),
        "attempt_id": entry.attempt_id.value,
        "operation_id": entry.operation_id.value,
        "phase": entry.phase.value,
        "storage_handle": optional(entry.storage_handle),
        "receipt_id": optional(entry.receipt_id),
        "cleanup_operation_id": optional(entry.cleanup_operation_id),
    }


def _encode_success(body):
    if type(body) is StartLoginResult:
        return {
            "disposition": body.disposition.value,
            "poll_after_ms": body.poll_after_ms.value,
        }
    if isinstance(body, LoginSnapshot):
        return _encode_login_snapshot(body)
    if type(body) is SettleLoginResult:
        return {
            "state": body.state.value,
            "receipt_id": body.receipt_id.value,
            "storage_handle": body.storage_handle.value,
        }
    if type(body) is CheckPending:
        return {
            "state": body.state.value,
            "check_id": body.check_id.value,
            "poll_after_ms": body.poll_after_ms.value,
        }
    if type(body) is CheckFinished:
        return {
            "state": body.state.value,
            "check_id": body.check_id.value,
            "observed_at": body.observed_at.value,
            "account": _encode_account(body.account),
        }
    if type(body) is CheckUnavailable:
        return {
            "state": body.state.value,
            "check_id": body.check_id.value,
            "reason": body.reason.value,
        }
    if type(body) is CleanupPending:
        return {
            "state": body.state.value,
            "phase": body.phase.value,
            "poll_after_ms": body.poll_after_ms.value,
        }
    if type(body) is CleanupCompleted:
        return {"state": body.state.value, "provider_logout": body.provider_logout.value}
    if type(body) is CleanupFailed:
        return {
            "state": body.state.value,
            "reason": body.reason.value,
            "retry_after_ms": body.retry_after_ms.value,
        }
    return {
        "snapshot_revision": body.snapshot_revision.value,
        "entries": [_encode_entry(entry) for entry in body.entries],
        "next_after_entry_id": (
            None if body.next_after_entry_id is None else body.next_after_entry_id.value
        ),
    }


def encode_response(response):
    _exact(response, Response)
    def build():
        value = {
            "v": 1,
            "coordinator": {
                "epoch": response.coordinator.epoch.value,
                "boot_id": response.coordinator.boot_id.value,
            },
            "supervisor_boot_id": response.supervisor_boot_id.value,
            "sequence": response.sequence.value,
            "request_id": response.request_id.value,
            "op": response.operation.value,
            "outcome": response.outcome.value,
        }
        if response.outcome is Outcome.OK:
            value["body"] = _encode_success(response.body)
        else:
            value["error"] = {
                "code": response.error.code.value,
                "retry_after_ms": (
                    None
                    if response.error.retry_after_ms is None
                    else response.error.retry_after_ms.value
                ),
            }
        return _encode_json(value, _RESPONSE_MAX)

    encoded = _fixed_invalid(build)
    decode_response(encoded)
    return encoded


def validate_response(request, response, supervisor_boot_id):
    _exact(request, Request)
    _exact(response, Response)
    _exact(supervisor_boot_id, Id)
    encode_request(request)
    encode_response(response)
    if (
        response.coordinator != request.coordinator
        or response.supervisor_boot_id != supervisor_boot_id
        or response.sequence != request.sequence
        or response.request_id != request.request_id
        or response.op() is not request.op()
    ):
        raise DtoError("correlation")
    if response.outcome is Outcome.ERROR:
        return
    if request.op() is Op.SETTLE_LOGIN:
        if (
            response.body.receipt_id != request.body.receipt_id
            or response.body.storage_handle != request.body.witness.storage_handle
        ):
            raise DtoError("correlation")
    elif request.op() is Op.CHECK_CONNECTION:
        if response.body.check_id != request.body.ticket.check_id:
            raise DtoError("correlation")
    elif request.op() is Op.RECONCILE_PAGE:
        _validate_reconcile(request.body, response.body)


def _validate_reconcile(request, page):
    if len(page.entries) > request.limit:
        raise DtoError("correlation")
    if (
        request.snapshot_revision is not None
        and page.snapshot_revision != request.snapshot_revision
    ):
        raise DtoError("correlation")
    previous = request.after_entry_id.value if request.after_entry_id is not None else None
    for entry in page.entries:
        current = entry.entry_id.value
        if previous is not None and current <= previous:
            raise DtoError("correlation")
        previous = current
    if page.next_after_entry_id is not None:
        if not page.entries or page.next_after_entry_id != page.entries[-1].entry_id:
            raise DtoError("correlation")


def validate_login_snapshot(binding, snapshot, expected_expires_at, now):
    _exact(binding, LoginBinding)
    if type(snapshot) not in (
        LoginProgress,
        LoginAwaitingUser,
        LoginPrepared,
        LoginCommitted,
        LoginFailed,
        LoginTerminal,
    ):
        raise DtoError("invalid")
    _exact(expected_expires_at, UnixSec)
    _exact(now, UnixSec)
    if type(snapshot) in (LoginProgress, LoginAwaitingUser):
        if snapshot.expires_at != expected_expires_at:
            raise DtoError("correlation")
        if now.value >= snapshot.expires_at.value:
            raise DtoError("expired")
    if type(snapshot) is LoginAwaitingUser:
        if (
            snapshot.challenge.expires_at.value <= now.value
            or snapshot.challenge.expires_at.value > snapshot.expires_at.value
        ):
            raise DtoError("expired")


@dataclass(frozen=True, repr=False)
class LoginKey(_SafeRepr):
    ticket: LoginTicketDto


@dataclass(frozen=True, repr=False)
class CleanupKey(_SafeRepr):
    scope: Scope
    connection_id: Id
    operation_id: Id
    target: CleanupTarget
    target_generation: Counter
    result_generation: Counter


def login_semantic_key(ticket):
    _exact(ticket, LoginTicketDto)
    return LoginKey(ticket)


def cleanup_semantic_key(ticket):
    _exact(ticket, CleanupTicketDto)
    return CleanupKey(
        ticket.scope,
        ticket.connection_id,
        ticket.operation_id,
        ticket.target,
        ticket.target_generation,
        ticket.result_generation,
    )


class _Channel(_SafeRepr):
    __slots__ = (
        "_coordinator",
        "_supervisor_boot_id",
        "_next_sequence",
        "_waiting",
        "_closed",
    )

    def __init__(self, coordinator, supervisor_boot_id):
        _exact(coordinator, Coordinator)
        _exact(supervisor_boot_id, Id)
        self._coordinator = coordinator
        self._supervisor_boot_id = supervisor_boot_id
        self._next_sequence = 1
        self._waiting = None
        self._closed = False

    def poison(self):
        self._closed = True
        self._waiting = None

    def _fail(self, category):
        self.poison()
        raise DtoError(category)

    def _begin(self, request):
        if self._closed or self._waiting is not None:
            self._fail("state")
        if type(request) is not Request:
            self._fail("invalid")
        try:
            encode_request(request)
        except DtoError as error:
            category = error.category
        else:
            category = None
        if category is not None:
            self._fail(category)
        if request.coordinator != self._coordinator:
            self._fail("correlation")
        if int(request.sequence.value) != self._next_sequence:
            self._fail("sequence")
        self._waiting = request

    def finish(self, response):
        if self._closed or self._waiting is None:
            self._fail("state")
        try:
            validate_response(self._waiting, response, self._supervisor_boot_id)
        except DtoError as error:
            self.poison()
            raise error
        completed = self._next_sequence
        self._waiting = None
        if completed == _I64_MAX:
            self._closed = True
        else:
            self._next_sequence = completed + 1


class ClientChannel(_Channel):
    def begin(self, request):
        self._begin(request)


class ServerChannel(_Channel):
    def accept(self, request):
        self._begin(request)
