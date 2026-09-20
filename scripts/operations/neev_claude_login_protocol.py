"""Bounded, memory-only parser for the Claude CLI's browser + paste-code login.

The real ``claude`` login CLI (observed at 2.1.267), run headless in an isolated
``--network none`` / provider-egress netns with an ephemeral HOME, prints on
stdout a browser sign-in URL followed by a paste-code prompt:

    Opening browser to sign in…
    If browser did not open visit: https://<oauth-host>/oauth/authorize?...
    Paste code here if prompted >

Unlike Codex (a JSON-RPC app-server with structured completion notifications),
this is a terminal UI. This module owns ONLY the verifiable, security-relevant
slice of that interaction:

  * extract the verification URL (ANSI-stripped, ``https``, host-allowlisted,
    length-bounded),
  * detect the paste-code prompt,
  * accept exactly ONE bounded authorization code per attempt and return the exact
    bytes to write to the child's stdin.

The authorization code is treated as a secret: it is never stored in a repr'd
field, never placed in an exception message, and never logged. Callers must write
the returned bytes to the child stdin and then discard them.

It deliberately does NOT decide login success/failure. A TUI's success/failure
banner is not a stable contract (the exact lines were not observed and must not be
guessed). The authoritative completion signal is the child's exit status plus a
credential-presence / account check performed by the supervisor against the
mounted credential volume. ``receive()`` surfaces the URL and the prompt; the
caller drives stdin and, on child exit, checks credentials.
"""

from __future__ import annotations

from dataclasses import dataclass, field
import re
from typing import List, Optional
import urllib.parse


MAX_CHUNK_BYTES = 65_536
MAX_BUFFER_BYTES = 65_536
MAX_URL_BYTES = 2_048
MAX_CODE_BYTES = 512

# The only hosts the Claude CLI's OAuth URL may point at: the default ``--claudeai``
# flow (claude.ai) and the ``--console`` flow (console.anthropic.com). A validated
# https URL to any OTHER host is a downgrade/redirect we refuse to surface.
ALLOWED_HOSTS = frozenset(("claude.ai", "console.anthropic.com"))

_ERROR_CODES = frozenset(
    ("invalid", "overflow", "denied", "conflict", "unsupported")
)

# CSI / OSC / two-byte escapes, plus carriage return and backspace — the control
# noise a TUI interleaves with the URL and prompt text.
_ANSI_RE = re.compile(
    r"\x1b\[[0-?]*[ -/]*[@-~]"
    r"|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)"
    r"|\x1b[@-Z\\-_]"
    r"|[\r\x08]"
)
# A candidate URL runs from ``https://`` to the next whitespace or control byte.
_URL_RE = re.compile(r"https://[^\s\x00-\x1f\x7f]{1,%d}" % (MAX_URL_BYTES - 8))
# One authorization code: printable ASCII, no spaces, no control, bounded.
_CODE_RE = re.compile(r"[\x21-\x7e]{1,%d}" % MAX_CODE_BYTES)
_PROMPT_MARKER = "paste code"


class ProtocolError(Exception):
    """A sanitized protocol error with an allowlisted public code (no code/URL)."""

    def __init__(self, code: object):
        safe_code = code if type(code) is str and code in _ERROR_CODES else "invalid"
        self.code = safe_code
        super().__init__(safe_code)


@dataclass(frozen=True)
class LoginUrl:
    """The verification URL to display; public but shown, so repr is suppressed."""

    verification_url: str = field(repr=False)


@dataclass(frozen=True)
class Event:
    """Typed result from processing one input chunk."""

    kind: str
    login: Optional[LoginUrl] = field(default=None, repr=False)


def _iter_complete_urls(text: str):
    """Yield each https URL that is followed by a terminator (whitespace/control)
    in ``text`` — i.e. the URL is whole, not truncated at the buffer's tail. A URL
    running to end-of-buffer is withheld until its terminating newline arrives, so a
    chunk boundary never surfaces a half URL."""
    for match in _URL_RE.finditer(text):
        if match.end() < len(text):
            yield match.group(0)


def _classify_url(candidate: str) -> str:
    """Return ``"ok"`` (allowlisted), ``"deny"`` (valid https, wrong host), or
    ``"skip"`` (not a usable absolute https URL — e.g. a partial line)."""
    if len(candidate) > MAX_URL_BYTES or not candidate.isascii():
        return "skip"
    try:
        parts = urllib.parse.urlsplit(candidate)
    except ValueError:
        return "skip"
    if parts.scheme != "https":
        return "skip"
    host = parts.hostname
    if host is None or not parts.path:
        return "skip"
    # No userinfo, no explicit port, and the netloc must be exactly the bare host.
    try:
        port = parts.port
    except ValueError:
        return "deny"
    if parts.username or parts.password or port is not None or parts.netloc != host:
        return "deny"
    return "ok" if host in ALLOWED_HOSTS else "deny"


class ClaudeLoginProtocol:
    """A single bounded Claude browser+paste-code login attempt."""

    def __init__(self) -> None:
        self._state = "waiting_url"  # waiting_url -> awaiting_code -> submitted / closed
        self._buffer = bytearray()
        self._url: Optional[str] = None
        self._url_emitted = False
        self._prompt_emitted = False

    @property
    def verification_url(self) -> Optional[str]:
        return self._url

    def receive(self, chunk: bytes) -> List[Event]:
        """Feed one stdout chunk; return any newly observed URL / prompt events."""
        if self._state == "closed" or type(chunk) is not bytes:
            self._fatal("invalid")
        if len(chunk) > MAX_CHUNK_BYTES:
            self._fatal("overflow")
        if len(self._buffer) + len(chunk) > MAX_BUFFER_BYTES:
            # The login preamble is a few hundred bytes; anything this large before a
            # URL/prompt is anomalous. Fail closed rather than silently truncating.
            self._fatal("overflow")
        self._buffer.extend(chunk)
        text = _ANSI_RE.sub("", self._buffer.decode("latin-1"))

        events: List[Event] = []
        completed = list(_iter_complete_urls(text))
        if not self._url_emitted:
            for candidate in completed:
                verdict = _classify_url(candidate)
                if verdict == "deny":
                    self._fatal("denied")
                if verdict == "ok":
                    self._url = candidate
                    self._url_emitted = True
                    events.append(Event("login_url", login=LoginUrl(self._url)))
                    break
        else:
            # A second, DIFFERENT allowlisted URL after one was surfaced is a conflict
            # (the user must not be shown two sign-in targets in one attempt); a
            # re-render of the same URL is benign.
            for candidate in completed:
                if candidate != self._url and _classify_url(candidate) == "ok":
                    self._fatal("conflict")
        if self._url_emitted and not self._prompt_emitted and _PROMPT_MARKER in text.lower():
            self._prompt_emitted = True
            self._state = "awaiting_code"
            events.append(Event("awaiting_code"))
        return events

    def submit_code(self, code: str) -> bytes:
        """Validate one pasted authorization code and return the exact bytes to write
        to the child's stdin. The code is never stored, logged, or reflected."""
        if self._state == "closed":
            raise ProtocolError("invalid")
        if self._state != "awaiting_code":
            # Only meaningful once the CLI has printed the paste prompt.
            raise ProtocolError("invalid")
        if type(code) is not str:
            raise ProtocolError("invalid")
        trimmed = code.strip()
        if not trimmed or len(trimmed) > MAX_CODE_BYTES or not trimmed.isascii():
            raise ProtocolError("invalid")
        if _CODE_RE.fullmatch(trimmed) is None:
            raise ProtocolError("invalid")
        self._state = "submitted"
        return trimmed.encode("ascii") + b"\n"

    def close(self) -> None:
        self._state = "closed"
        self._buffer.clear()

    def _fatal(self, code: str) -> None:
        self.close()
        raise ProtocolError(code) from None
