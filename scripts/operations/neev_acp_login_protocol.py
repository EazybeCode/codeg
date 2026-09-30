#!/usr/bin/env python3
"""Pure parsers for the OFFICIAL sign-in commands of the ACP providers.

Each parser reads the CLI's own terminal output (bytes, possibly split anywhere,
possibly with ANSI escapes) and reports events. None of them spawns, logs or
relays anything; the drivers in `neev_agent_login_drivers` own the process.

  * OpenCode `opencode auth login --provider openai --method "ChatGPT Pro/Plus
    (headless)"` (pinned 1.18.33, run over plain pipes and observed 2026-09-30):
        ┌  Add credential
        ●  Go to: https://auth.openai.com/codex/device
        ●  Enter code: XXXX-XXXXX
        ◒  Waiting for authorization...        (spinner, \\x1b[999D\\x1b[J redraws)
    then "Login successful" (auth.json written) or "Failed to authorize".
    Source: packages/opencode/src/cli/cmd/providers.ts + plugin/openai/codex.ts.

  * Gemini CLI `gemini` with NO_BROWSER=true on a TTY (pinned 0.62.0; source
    packages/core/src/code_assist/oauth2.ts `authWithUserCode`, output observed):
        Please visit the following URL to authorize the application:
        https://accounts.google.com/o/oauth2/v2/auth?redirect_uri=...codeassist...
        Enter the authorization code:
    then "Failed to authenticate with ..." on a rejected code. Success is the
    cached ~/.gemini/oauth_creds.json, which the driver checks, not this parser.

  * Cursor `agent login` with NO_OPEN_BROWSER=1 (cursor.com/docs/cli/reference/
    authentication: "print[s] the login URL without opening a browser"). The CLI
    polls and exits. NOT observed on a pinned binary (no vendor checksum exists),
    so only a cursor.com https URL is accepted and the rest is the exit code.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import List, Optional

MAX_BUFFER_BYTES = 64 * 1024
MAX_URL_CHARS = 1024

_ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]")

OPENAI_DEVICE_URL = "https://auth.openai.com/codex/device"
_OPENCODE_URL = re.compile(r"Go to: (https://\S+)")
_OPENCODE_CODE = re.compile(r"Enter code: ([A-Z0-9-]{4,64})(?![A-Za-z0-9-])")
_OPENCODE_OK = "Login successful"
_OPENCODE_FAIL = ("Failed to authorize", "Unknown method", "Unknown provider")

GOOGLE_AUTHORIZE_URL = "https://accounts.google.com/o/oauth2/v2/auth?"
_GEMINI_URL = re.compile(r"(https://accounts\.google\.com/o/oauth2/v2/auth\?[^\s\x00-\x1f]+)")
_GEMINI_PROMPT = "Enter the authorization code"
_GEMINI_FAIL = ("Failed to authenticate with", "Authorization code is required",
                "Authorization timed out")

CURSOR_URL_PREFIX = "https://cursor.com/"
_CURSOR_URL = re.compile(r"(https://cursor\.com/[^\s\x00-\x1f]+)")


class ProtocolError(Exception):
    """Output we cannot trust (oversized, or a challenge we will not relay)."""


@dataclass(frozen=True)
class Event:
    kind: str  # "challenge" | "awaiting_code" | "succeeded" | "failed"
    verification_url: Optional[str] = None
    user_code: Optional[str] = None


def strip_terminal(text: str) -> str:
    """Printable text only: ANSI/OSC escapes removed, CR treated as a newline."""
    return _ANSI.sub("", text).replace("\r", "\n")


class _Accumulator:
    def __init__(self):
        self._raw = bytearray()

    def text(self, chunk: bytes) -> str:
        self._raw.extend(chunk)
        if len(self._raw) > MAX_BUFFER_BYTES:
            raise ProtocolError("oversized")
        return strip_terminal(bytes(self._raw).decode("utf-8", "replace"))


def _bounded_url(url: str, prefix: str) -> str:
    # A URL is only a challenge when it is COMPLETE: the text after it proves the
    # line ended (a chunk boundary could cut it anywhere).
    if len(url) > MAX_URL_CHARS or not url.startswith(prefix):
        raise ProtocolError("url")
    return url


class OpenCodeLoginProtocol:
    """`opencode auth login` ChatGPT headless device flow."""

    def __init__(self):
        self._acc = _Accumulator()
        self._challenged = False
        self._done = False

    def receive(self, chunk: bytes) -> List[Event]:
        text = self._acc.text(chunk)
        events = []
        if not self._challenged:
            url = _OPENCODE_URL.search(text)
            code = _OPENCODE_CODE.search(text)
            if url and code and "\n" in text[code.end():]:
                if url.group(1) != OPENAI_DEVICE_URL:
                    raise ProtocolError("url")
                self._challenged = True
                events.append(Event("challenge", OPENAI_DEVICE_URL, code.group(1)))
        if not self._done:
            if _OPENCODE_OK in text:
                self._done = True
                events.append(Event("succeeded"))
            elif any(marker in text for marker in _OPENCODE_FAIL):
                self._done = True
                events.append(Event("failed"))
        return events


class GeminiLoginProtocol:
    """Gemini CLI NO_BROWSER paste-code sign-in on a TTY."""

    def __init__(self):
        self._acc = _Accumulator()
        self._url = None
        self._prompted = False
        self._failed = False

    def receive(self, chunk: bytes) -> List[Event]:
        text = self._acc.text(chunk)
        events = []
        if self._url is None:
            match = _GEMINI_URL.search(text)
            if match and "\n" in text[match.end():]:
                self._url = _bounded_url(match.group(1), GOOGLE_AUTHORIZE_URL)
                events.append(Event("challenge", self._url))
        if self._url is not None and not self._prompted and _GEMINI_PROMPT in text:
            self._prompted = True
            events.append(Event("awaiting_code", self._url))
        if not self._failed and any(marker in text for marker in _GEMINI_FAIL):
            self._failed = True
            events.append(Event("failed"))
        return events


class CursorLoginProtocol:
    """Cursor `agent login` with NO_OPEN_BROWSER=1: print URL, poll, exit."""

    def __init__(self):
        self._acc = _Accumulator()
        self._url = None

    def receive(self, chunk: bytes) -> List[Event]:
        text = self._acc.text(chunk)
        if self._url is not None:
            return []
        match = _CURSOR_URL.search(text)
        if match and "\n" in text[match.end():]:
            self._url = _bounded_url(match.group(1), CURSOR_URL_PREFIX)
            return [Event("challenge", self._url)]
        return []


def submit_code_bytes(code: str) -> bytes:
    """The paste-code line for a raw-mode TTY reader (Enter is CR). The code is
    validated by the DTO; this refuses anything that would inject a second line."""
    if not code or any(ch in code for ch in "\r\n\x00") or len(code) > 512:
        raise ProtocolError("code")
    return code.encode("utf-8") + b"\r"
