"""Bounded, memory-only protocol for managed Codex account operations.

This module owns framing, response correlation, and transient device-login state.
It intentionally does not own a transport, persistence, authorization, or runtime.
"""

from dataclasses import dataclass, field
import json
import math
import re
from typing import Any, Dict, List, Optional, Tuple


MAX_CHUNK_BYTES = 65_536
MAX_FRAME_BYTES = 65_536
MAX_PROFILE_STRING = 256
MAX_EMAIL_CODEPOINTS = 254
MAX_LOGIN_ID_BYTES = 128
MAX_USER_CODE_BYTES = 64

_ERROR_CODES = frozenset(
    ("invalid", "overflow", "unsupported", "stale", "conflict", "provider_failed")
)
_PLAN_TYPES = frozenset(
    (
        "free",
        "go",
        "plus",
        "pro",
        "prolite",
        "team",
        "self_serve_business_prolite",
        "self_serve_business_usage_based",
        "business",
        "ent26",
        "enterprise_cbp_automation",
        "enterprise_cbp_usage_based",
        "enterprise",
        "edu",
        "edu_plus",
        "edu_pro",
        "unknown",
    )
)
_SUPPORTED_PLANS = frozenset(("plus", "pro", "team", "business", "enterprise", "edu"))
_LOGIN_ID = re.compile(r"[A-Za-z0-9_-]+\Z")
_USER_CODE = re.compile(r"[A-Z0-9-]+\Z")
_VERIFICATION_URL = "https://auth.openai.com/codex/device"
_INITIALIZED = b'{"method":"initialized","params":{}}\n'
_ACCOUNT_UPDATE_MODES = frozenset(
    (
        "apikey",
        "chatgpt",
        "chatgptAuthTokens",
        "headers",
        "agentIdentity",
        "personalAccessToken",
        "bedrockApiKey",
        "bedrockAccessKeys",
    )
)


class ProtocolError(Exception):
    """A sanitized protocol error with an allowlisted public code."""

    def __init__(self, code: Any):
        safe_code = code if type(code) is str and code in _ERROR_CODES else "invalid"
        self.code = safe_code
        super().__init__(safe_code)


@dataclass(frozen=True)
class AccountProjection:
    """Safe account status; email is display-only private data."""

    status: str
    plan: Optional[str] = None
    email: Optional[str] = field(default=None, repr=False)


@dataclass(frozen=True)
class LoginChallenge:
    """Transient values that a caller may display but must never persist."""

    login_id: str = field(repr=False)
    verification_url: str = field(repr=False)
    user_code: str = field(repr=False)


@dataclass(frozen=True)
class Event:
    """Typed result from processing one current input chunk."""

    kind: str
    outbound: Optional[bytes] = field(default=None, repr=False)
    account: Optional[AccountProjection] = None
    login: Optional[LoginChallenge] = field(default=None, repr=False)
    status: Optional[str] = None


class _DuplicateKey(ValueError):
    pass


def _object_from_pairs(pairs: List[Tuple[str, Any]]) -> Dict[str, Any]:
    result: Dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise _DuplicateKey()
        result[key] = value
    return result


def _reject_constant(unused_value: str) -> None:
    raise ValueError()


def _parse_finite_float(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed):
        raise ValueError()
    return parsed


def _encode_request(request_id: int, method: str, params: Dict[str, Any]) -> bytes:
    value = {"id": request_id, "method": method, "params": params}
    return json.dumps(value, separators=(",", ":")).encode("ascii") + b"\n"


def _exact_keys(value: Any, required: frozenset, optional: frozenset = frozenset()) -> bool:
    if type(value) is not dict:
        return False
    keys = frozenset(value)
    return required.issubset(keys) and keys.issubset(required | optional)


def _bounded_string(value: Any, maximum: int, nonempty: bool = True) -> bool:
    return type(value) is str and (bool(value) or not nonempty) and len(value) <= maximum


def _valid_login_id(value: Any) -> bool:
    return (
        type(value) is str
        and 1 <= len(value) <= MAX_LOGIN_ID_BYTES
        and value.isascii()
        and _LOGIN_ID.fullmatch(value) is not None
    )


def _valid_user_code(value: Any) -> bool:
    return (
        type(value) is str
        and 1 <= len(value) <= MAX_USER_CODE_BYTES
        and value.isascii()
        and _USER_CODE.fullmatch(value) is not None
    )


class CodexLoginProtocol:
    """A single bounded app-server protocol session."""

    def __init__(self) -> None:
        self._state = "new"
        self._next_id = 1
        self._pending: Optional[Tuple[int, str]] = None
        self._buffer = bytearray()
        self._login_id: Optional[str] = None
        self._account_check_required = False

    def initialize(self) -> bytes:
        if self._state == "closed":
            raise ProtocolError("invalid")
        if self._state == "initializing" or self._pending is not None:
            raise ProtocolError("conflict")
        if self._state != "new":
            raise ProtocolError("invalid")
        request = self._begin_request(
            "initialize",
            {
                "clientInfo": {
                    "name": "neev",
                    "title": "Neev",
                    "version": "0.1.0",
                }
            },
        )
        self._state = "initializing"
        return request

    def read_account(self, refresh: bool = False) -> bytes:
        self._require_ready()
        if type(refresh) is not bool:
            raise ProtocolError("invalid")
        if self._login_id is not None:
            raise ProtocolError("conflict")
        if self._account_check_required and not refresh:
            raise ProtocolError("invalid")
        return self._begin_request("account/read", {"refreshToken": refresh})

    def start_login(self) -> bytes:
        self._require_ready()
        if self._login_id is not None or self._account_check_required:
            raise ProtocolError("conflict")
        return self._begin_request(
            "account/login/start", {"type": "chatgptDeviceCode"}
        )

    def cancel_login(self) -> bytes:
        self._require_ready()
        if self._pending is not None:
            raise ProtocolError("conflict")
        if self._login_id is None:
            raise ProtocolError("invalid")
        canceled_id = self._login_id
        self._login_id = None
        self._account_check_required = False
        return self._begin_request("account/login/cancel", {"loginId": canceled_id})

    def logout(self) -> bytes:
        self._require_ready()
        if self._pending is not None:
            raise ProtocolError("conflict")
        self._login_id = None
        self._account_check_required = False
        return self._begin_request("account/logout", {})

    def receive(self, chunk: bytes) -> List[Event]:
        if self._state == "closed" or type(chunk) is not bytes:
            self._fatal("invalid")
        if len(chunk) > MAX_CHUNK_BYTES:
            self._fatal("overflow")

        events: List[Event] = []
        provider_failed = False
        offset = 0
        while offset < len(chunk):
            newline = chunk.find(b"\n", offset)
            if newline < 0:
                segment = chunk[offset:]
                if len(self._buffer) + len(segment) > MAX_FRAME_BYTES:
                    self._fatal("overflow")
                self._buffer.extend(segment)
                if provider_failed:
                    raise ProtocolError("provider_failed") from None
                return events
            segment = chunk[offset:newline]
            if len(self._buffer) + len(segment) + 1 > MAX_FRAME_BYTES:
                self._fatal("overflow")
            self._buffer.extend(segment)
            raw = bytes(self._buffer)
            self._buffer.clear()
            value = self._decode(raw)
            try:
                event = self._handle(value)
            except ProtocolError as error:
                if error.code != "provider_failed":
                    raise
                provider_failed = True
                event = None
            if event is not None:
                events.append(event)
            offset = newline + 1
        if provider_failed:
            raise ProtocolError("provider_failed") from None
        return events

    def close(self) -> None:
        self._state = "closed"
        self._pending = None
        self._buffer.clear()
        self._login_id = None
        self._account_check_required = False

    def _require_ready(self) -> None:
        if self._state != "ready":
            raise ProtocolError("invalid")

    def _begin_request(self, method: str, params: Dict[str, Any]) -> bytes:
        if self._pending is not None:
            raise ProtocolError("conflict")
        request_id = self._next_id
        self._next_id += 1
        self._pending = (request_id, method)
        return _encode_request(request_id, method, params)

    def _decode(self, raw: bytes) -> Dict[str, Any]:
        try:
            text = raw.decode("utf-8")
            value = json.loads(
                text,
                object_pairs_hook=_object_from_pairs,
                parse_constant=_reject_constant,
                parse_float=_parse_finite_float,
            )
        except (
            UnicodeDecodeError,
            json.JSONDecodeError,
            ValueError,
            TypeError,
            RecursionError,
        ):
            self._fatal("invalid")
        if type(value) is not dict:
            self._fatal("invalid")
        return value

    def _handle(self, value: Dict[str, Any]) -> Optional[Event]:
        if "id" in value:
            if "method" in value:
                self._fatal("invalid")
            return self._handle_response(value)
        if "method" not in value:
            self._fatal("invalid")
        if self._state != "ready":
            self._fatal("invalid")
        return self._handle_notification(value)

    def _handle_response(self, value: Dict[str, Any]) -> Event:
        has_result = "result" in value
        has_error = "error" in value
        expected_keys = frozenset(("id", "result" if has_result else "error"))
        if (
            has_result == has_error
            or frozenset(value) != expected_keys
            or type(value["id"]) is not int
            or self._pending is None
            or value["id"] != self._pending[0]
        ):
            self._fatal("invalid")

        unused_id, method = self._pending
        self._pending = None
        if has_error:
            if method == "initialize":
                self._state = "new"
            raise ProtocolError("provider_failed") from None

        result = value["result"]
        if method == "initialize":
            self._validate_initialize(result)
            self._state = "ready"
            return Event("initialized", outbound=_INITIALIZED)
        if method == "account/read":
            account = self._project_account(result)
            self._account_check_required = False
            return Event("account", account=account)
        if method == "account/login/start":
            login = self._validate_device(result)
            self._login_id = login.login_id
            return Event("login_started", login=login)
        if method == "account/login/cancel":
            if not _exact_keys(result, frozenset(("status",))):
                self._fatal("invalid")
            status = result["status"]
            if status not in ("canceled", "notFound"):
                self._fatal("invalid")
            return Event("login_canceled", status=status)
        if method == "account/logout":
            if type(result) is not dict or result:
                self._fatal("invalid")
            return Event("logged_out")
        self._fatal("invalid")

    def _handle_notification(self, value: Dict[str, Any]) -> Optional[Event]:
        if not _exact_keys(value, frozenset(("method", "params"))):
            self._fatal("invalid")
        method = value["method"]
        if type(method) is not str:
            self._fatal("invalid")
        if method == "account/login/completed":
            return self._handle_completion(value["params"])
        if method == "account/updated":
            self._validate_account_update(value["params"])
            return None
        self._fatal("invalid")

    def _handle_completion(self, params: Any) -> Optional[Event]:
        required = frozenset(("success",))
        optional = frozenset(("loginId", "error", "onboardingEntrypoint"))
        if not _exact_keys(params, required, optional):
            self._fatal("invalid")
        if type(params["success"]) is not bool:
            self._fatal("invalid")
        login_id = params.get("loginId")
        if login_id is not None and not _valid_login_id(login_id):
            self._fatal("invalid")
        error = params.get("error")
        if error is not None and type(error) is not str:
            self._fatal("invalid")
        onboarding = params.get("onboardingEntrypoint")
        if onboarding not in (None, "life_sciences"):
            self._fatal("invalid")

        if self._login_id is None or login_id != self._login_id:
            return None
        self._login_id = None
        if params["success"]:
            self._account_check_required = True
            return Event("account_check_required")
        self._account_check_required = False
        return Event("login_failed")

    def _validate_initialize(self, result: Any) -> None:
        if not _exact_keys(
            result,
            frozenset(("userAgent",)),
            frozenset(("platformFamily", "platformOs")),
        ):
            self._fatal("invalid")
        if not _bounded_string(result["userAgent"], MAX_PROFILE_STRING):
            self._fatal("invalid")
        for key in ("platformFamily", "platformOs"):
            if key in result and not _bounded_string(result[key], MAX_PROFILE_STRING):
                self._fatal("invalid")

    def _project_account(self, result: Any) -> AccountProjection:
        if not _exact_keys(
            result,
            frozenset(("requiresOpenaiAuth",)),
            frozenset(("account",)),
        ):
            self._fatal("invalid")
        requires_auth = result["requiresOpenaiAuth"]
        if type(requires_auth) is not bool:
            self._fatal("invalid")
        account = result.get("account")
        if account is None:
            return AccountProjection("disconnected")
        if type(account) is not dict or type(account.get("type")) is not str:
            self._fatal("invalid")

        account_type = account["type"]
        if account_type == "apiKey":
            if frozenset(account) != frozenset(("type",)):
                self._fatal("invalid")
            return AccountProjection("unsupported")
        if account_type == "amazonBedrock":
            if not _exact_keys(
                account,
                frozenset(("type",)),
                frozenset(("usesCodexManagedCredentials",)),
            ):
                self._fatal("invalid")
            managed = account.get("usesCodexManagedCredentials", False)
            if type(managed) is not bool:
                self._fatal("invalid")
            return AccountProjection("unsupported")
        if account_type != "chatgpt" or frozenset(account) != frozenset(
            ("type", "email", "planType")
        ):
            self._fatal("invalid")

        email = account["email"]
        if email is not None and (
            type(email) is not str
            or len(email) > MAX_EMAIL_CODEPOINTS
            or not email.isprintable()
        ):
            self._fatal("invalid")
        plan = account["planType"]
        if type(plan) is not str:
            self._fatal("invalid")
        if requires_auth and plan in _SUPPORTED_PLANS:
            return AccountProjection("supported", plan=plan, email=email)
        return AccountProjection("unsupported")

    def _validate_device(self, result: Any) -> LoginChallenge:
        if type(result) is not dict or frozenset(result) != frozenset(
            ("type", "loginId", "verificationUrl", "userCode")
        ):
            self._fatal("invalid")
        if result["type"] != "chatgptDeviceCode":
            self._fatal("invalid")
        if not _valid_login_id(result["loginId"]):
            self._fatal("invalid")
        if result["verificationUrl"] != _VERIFICATION_URL:
            self._fatal("invalid")
        if not _valid_user_code(result["userCode"]):
            self._fatal("invalid")
        return LoginChallenge(
            login_id=result["loginId"],
            verification_url=result["verificationUrl"],
            user_code=result["userCode"],
        )

    def _validate_account_update(self, params: Any) -> None:
        if not _exact_keys(
            params,
            frozenset(),
            frozenset(("authMode", "planType")),
        ):
            self._fatal("invalid")
        auth_mode = params.get("authMode")
        if auth_mode is not None and (
            type(auth_mode) is not str or auth_mode not in _ACCOUNT_UPDATE_MODES
        ):
            self._fatal("invalid")
        plan_type = params.get("planType")
        if plan_type is not None and (
            type(plan_type) is not str or plan_type not in _PLAN_TYPES
        ):
            self._fatal("invalid")

    def _fatal(self, code: str) -> None:
        self.close()
        raise ProtocolError(code) from None
