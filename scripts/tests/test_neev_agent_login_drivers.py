"""Tests for the concrete Codex/Claude login drivers + account probes, driven with a
fake Channel that scripts the providers' real protocol output (no subprocess)."""

import sys
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_agent_supervisor as sup  # noqa: E402
import neev_agent_login_drivers as drivers  # noqa: E402

_DEVICE_URL = "https://auth.openai.com/codex/device"
_CLAUDE_URL = "https://claude.com/cai/oauth/authorize?code=abc"

# Codex app-server responses/notifications, in order.
_CODEX_INIT = b'{"id":1,"result":{"userAgent":"neev"}}\n'
_CODEX_LOGIN = (
    b'{"id":2,"result":{"type":"chatgptDeviceCode","loginId":"abc",'
    b'"verificationUrl":"https://auth.openai.com/codex/device","userCode":"ABCD-EFGH"}}\n'
)
_CODEX_COMPLETED = b'{"method":"account/login/completed","params":{"success":true,"loginId":"abc"}}\n'
_CODEX_ACCOUNT = (
    b'{"id":3,"result":{"requiresOpenaiAuth":true,'
    b'"account":{"type":"chatgpt","email":"a@b.co","planType":"plus"}}}\n'
)
# The probe issues account/read as request id 2 (it sends no start_login).
_CODEX_PROBE_ACCOUNT = (
    b'{"id":2,"result":{"requiresOpenaiAuth":true,'
    b'"account":{"type":"chatgpt","email":"a@b.co","planType":"plus"}}}\n'
)


class FakeChannel(drivers.Channel):
    def __init__(self, chunks=None):
        self._chunks = list(chunks or [])
        self.writes = []
        self.started = False
        self.terminated = False
        self._rc = None

    def start(self):
        self.started = True

    def read(self):
        return self._chunks.pop(0) if self._chunks else b""

    def write(self, data):
        self.writes.append(data)

    def returncode(self):
        return self._rc

    def terminate(self):
        self.terminated = True
        if self._rc is None:
            self._rc = -15

    def exit(self, code=0):
        self._rc = code


class CodexDriverTests(unittest.TestCase):
    def test_device_flow_reaches_prepared(self):
        channel = FakeChannel([_CODEX_INIT, _CODEX_LOGIN, _CODEX_COMPLETED, _CODEX_ACCOUNT])
        driver = drivers.CodexLoginDriver(channel)
        driver.start()
        self.assertTrue(channel.started)
        self.assertEqual(len(channel.writes), 1)  # initialize request

        self.assertEqual(driver.poll().phase, "starting")  # init resp -> start_login sent
        awaiting = driver.poll()  # device challenge
        self.assertEqual(awaiting.phase, "awaiting_user")
        self.assertEqual(awaiting.challenge.verification_url, _DEVICE_URL)
        self.assertEqual(awaiting.challenge.user_code, "ABCD-EFGH")

        self.assertEqual(driver.poll().phase, "verifying")  # completed -> account/read
        prepared = driver.poll()  # account supported
        self.assertEqual(prepared.phase, "prepared")
        self.assertEqual(prepared.account_label, "plus")

    def test_provider_exit_before_account_is_failed(self):
        channel = FakeChannel([_CODEX_INIT])
        driver = drivers.CodexLoginDriver(channel)
        driver.start()
        driver.poll()  # initialized
        channel.exit(1)
        self.assertEqual(driver.poll().phase, "failed")

    def test_submit_code_rejected(self):
        driver = drivers.CodexLoginDriver(FakeChannel())
        with self.assertRaises(RuntimeError):
            driver.submit_code("x")


class ClaudeDriverTests(unittest.TestCase):
    def _preamble(self):
        return (
            b"Opening browser to sign in\xe2\x80\xa6\r\n"
            b"If browser did not open visit: " + _CLAUDE_URL.encode() + b"\r\n"
            b"Paste code here if prompted > "
        )

    def test_url_then_submit_then_prepared_on_exit_with_credentials(self):
        channel = FakeChannel([self._preamble()])
        driver = drivers.ClaudeLoginDriver(channel, credential_present=lambda: True)
        driver.start()
        awaiting = driver.poll()
        self.assertEqual(awaiting.phase, "awaiting_user")
        self.assertTrue(awaiting.challenge.verification_url.startswith("https://claude.com/cai/oauth/authorize"))
        self.assertIsNone(awaiting.challenge.user_code)

        driver.submit_code("auth-code-123")
        self.assertEqual(channel.writes[-1], b"auth-code-123\n")
        channel.exit(0)
        self.assertEqual(driver.poll().phase, "prepared")

    def test_exit_without_submit_is_failed(self):
        channel = FakeChannel([])
        driver = drivers.ClaudeLoginDriver(channel, credential_present=lambda: False)
        driver.start()
        channel.exit(1)
        state = driver.poll()
        self.assertEqual(state.phase, "failed")
        self.assertEqual(state.failure, "provider_unavailable")

    def test_submit_then_exit_without_credentials_is_reauth_failed(self):
        channel = FakeChannel([self._preamble()])
        driver = drivers.ClaudeLoginDriver(channel, credential_present=lambda: False)
        driver.start()
        driver.poll()
        driver.submit_code("code")
        channel.exit(0)
        state = driver.poll()
        self.assertEqual(state.phase, "failed")
        self.assertEqual(state.failure, "reauth_required")


class AccountProbeTests(unittest.TestCase):
    def test_codex_probe_reads_supported_account(self):
        channel = FakeChannel([_CODEX_INIT, _CODEX_PROBE_ACCOUNT])
        probe = drivers.CodexAccountProbe(channel)
        self.assertEqual(probe.probe(), ("supported", "plus"))
        self.assertTrue(channel.terminated)

    def test_codex_probe_exit_before_account_raises(self):
        channel = FakeChannel([_CODEX_INIT])
        channel.exit(1)
        probe = drivers.CodexAccountProbe(channel)
        with self.assertRaises(RuntimeError):
            probe.probe()

    def test_claude_probe_presence(self):
        self.assertEqual(drivers.ClaudeAccountProbe(lambda: True).probe(), "supported")
        self.assertEqual(drivers.ClaudeAccountProbe(lambda: False).probe(), "disconnected")


if __name__ == "__main__":
    unittest.main()
