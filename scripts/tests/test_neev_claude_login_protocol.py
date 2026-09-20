"""Portable tests for the Claude browser + paste-code login protocol parser.

They prove URL extraction (ANSI-stripped, host-allowlisted), prompt detection,
bounded never-logged code submission, and fail-closed behavior — without spawning
the real CLI (that is a Linux/network acceptance the supervisor drives)."""

import sys
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_claude_login_protocol as proto  # noqa: E402


_URL = (
    "https://claude.com/cai/oauth/authorize?code_challenge=abc123"
    "&client_id=cid&redirect_uri=https://console.anthropic.com/oauth/code/callback"
    "&response_type=code&scope=org"
)
_CONSOLE_URL = "https://console.anthropic.com/oauth/authorize?client_id=cid&x=y"


def _preamble(url):
    # Realistic TUI output with ANSI color/cursor noise interleaved.
    return (
        b"\x1b[2K\x1b[36mOpening browser to sign in\xe2\x80\xa6\x1b[0m\r\n"
        b"If browser did not open visit: \x1b[4m" + url.encode("ascii") + b"\x1b[0m\r\n"
        b"\x1b[1mPaste code here if prompted >\x1b[0m "
    )


class UrlExtractionTests(unittest.TestCase):
    def test_extracts_allowlisted_claudeai_url_and_prompt(self):
        session = proto.ClaudeLoginProtocol()
        events = session.receive(_preamble(_URL))
        kinds = [event.kind for event in events]
        self.assertEqual(kinds, ["login_url", "awaiting_code"])
        self.assertEqual(events[0].login.verification_url, _URL)
        self.assertEqual(session.verification_url, _URL)

    def test_extracts_console_url(self):
        session = proto.ClaudeLoginProtocol()
        events = session.receive(_preamble(_CONSOLE_URL))
        self.assertEqual(events[0].login.verification_url, _CONSOLE_URL)

    def test_url_and_prompt_split_across_chunks(self):
        session = proto.ClaudeLoginProtocol()
        blob = _preamble(_URL)
        cut = blob.index(b"authorize") + 4
        first = session.receive(blob[:cut])
        self.assertEqual(first, [])  # partial URL: nothing surfaced yet
        rest = session.receive(blob[cut:])
        self.assertEqual([event.kind for event in rest], ["login_url", "awaiting_code"])

    def test_non_allowlisted_host_is_denied(self):
        session = proto.ClaudeLoginProtocol()
        with self.assertRaises(proto.ProtocolError) as caught:
            session.receive(_preamble("https://evil.example.com/oauth/authorize?x=y"))
        self.assertEqual(caught.exception.code, "denied")

    def test_url_with_explicit_port_or_userinfo_is_denied(self):
        for url in (
            "https://claude.com:8443/cai/oauth/authorize?x=y",
            "https://user@claude.com/cai/oauth/authorize?x=y",
        ):
            with self.subTest(url=url):
                session = proto.ClaudeLoginProtocol()
                with self.assertRaises(proto.ProtocolError) as caught:
                    session.receive(_preamble(url))
                self.assertEqual(caught.exception.code, "denied")

    def test_second_different_allowlisted_url_is_conflict(self):
        session = proto.ClaudeLoginProtocol()
        session.receive(_preamble(_URL))
        with self.assertRaises(proto.ProtocolError) as caught:
            session.receive(b"visit: " + _CONSOLE_URL.encode("ascii") + b"\r\n")
        self.assertEqual(caught.exception.code, "conflict")

    def test_same_url_re_render_is_not_a_conflict(self):
        session = proto.ClaudeLoginProtocol()
        session.receive(_preamble(_URL))
        # A redraw of the same URL must be tolerated.
        events = session.receive(b"visit: " + _URL.encode("ascii") + b"\r\n")
        self.assertEqual(events, [])


class CodeSubmissionTests(unittest.TestCase):
    def _awaiting(self):
        session = proto.ClaudeLoginProtocol()
        session.receive(_preamble(_URL))
        return session

    def test_valid_code_returns_stdin_bytes_with_newline(self):
        session = self._awaiting()
        self.assertEqual(session.submit_code("Abc-123_def.456#state"), b"Abc-123_def.456#state\n")

    def test_surrounding_whitespace_is_trimmed(self):
        session = self._awaiting()
        self.assertEqual(session.submit_code("  code#s \n"), b"code#s\n")

    def test_submit_before_prompt_is_rejected(self):
        session = proto.ClaudeLoginProtocol()
        session.receive(b"If browser did not open visit: " + _URL.encode("ascii") + b"\r\n")
        # URL seen but no paste prompt yet -> not awaiting_code.
        with self.assertRaises(proto.ProtocolError) as caught:
            session.submit_code("code#s")
        self.assertEqual(caught.exception.code, "invalid")

    def test_malformed_codes_rejected(self):
        for bad in ("", "   ", "has space", "tab\tinside", "line\nbreak",
                    "ctrl\x01char", "unicodeé", "x" * (proto.MAX_CODE_BYTES + 1)):
            with self.subTest(bad=repr(bad)):
                session = self._awaiting()
                with self.assertRaises(proto.ProtocolError) as caught:
                    session.submit_code(bad)
                self.assertEqual(caught.exception.code, "invalid")

    def test_non_str_code_rejected(self):
        session = self._awaiting()
        with self.assertRaises(proto.ProtocolError):
            session.submit_code(b"code#s")

    def test_double_submit_is_rejected(self):
        session = self._awaiting()
        session.submit_code("code#s")
        with self.assertRaises(proto.ProtocolError) as caught:
            session.submit_code("code#s")
        self.assertEqual(caught.exception.code, "invalid")

    def test_code_never_appears_in_error_or_reprs(self):
        secret = "SUPERSECRET-code#state123"
        session = self._awaiting()
        session.close()
        try:
            session.submit_code(secret)
        except proto.ProtocolError as error:
            self.assertNotIn(secret, str(error))
            self.assertNotIn(secret, repr(error))
        # The URL/event/challenge reprs must not leak their (display-only) URL either.
        fresh = proto.ClaudeLoginProtocol()
        events = fresh.receive(_preamble(_URL))
        self.assertNotIn(_URL, repr(events[0]))
        self.assertNotIn(_URL, repr(events[0].login))


class BoundsTests(unittest.TestCase):
    def test_oversize_chunk_is_overflow(self):
        session = proto.ClaudeLoginProtocol()
        with self.assertRaises(proto.ProtocolError) as caught:
            session.receive(b"x" * (proto.MAX_CHUNK_BYTES + 1))
        self.assertEqual(caught.exception.code, "overflow")

    def test_buffer_overflow_before_url_is_fatal(self):
        session = proto.ClaudeLoginProtocol()
        with self.assertRaises(proto.ProtocolError) as caught:
            for _ in range(proto.MAX_BUFFER_BYTES // 1024 + 2):
                session.receive(b"x" * 1024)
        self.assertEqual(caught.exception.code, "overflow")

    def test_receive_after_close_is_invalid(self):
        session = proto.ClaudeLoginProtocol()
        session.close()
        with self.assertRaises(proto.ProtocolError) as caught:
            session.receive(b"anything")
        self.assertEqual(caught.exception.code, "invalid")

    def test_non_bytes_chunk_is_invalid(self):
        session = proto.ClaudeLoginProtocol()
        with self.assertRaises(proto.ProtocolError):
            session.receive("not bytes")


if __name__ == "__main__":
    unittest.main()
