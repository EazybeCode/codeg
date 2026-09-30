"""The credential-free ACP acceptance probe, driven with FAKE provider binaries.

These prove the probe's own logic: a no-credential "supported" is reported as a
failure, a sign-in that needs a TTY really gets one, challenges are redacted,
and a wrong pinned version fails. They do NOT stand in for running it inside
the Linux image against the real pinned CLIs."""

import json
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_acp_provider_acceptance as acceptance  # noqa: E402

FAKE_ACP = r'''#!{python}
import json, sys
mode = {mode!r}
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": {{"protocolVersion": 1,
               "agentCapabilities": {{}}, "authMethods": []}}}}
    elif msg.get("method") == "session/new":
        if mode == "anonymous":
            out = {{"jsonrpc": "2.0", "id": msg["id"], "result": {{"sessionId": "s"}}}}
        else:
            out = {{"jsonrpc": "2.0", "id": msg["id"],
                   "error": {{"code": -32000, "message": "sign in"}}}}
    else:
        continue
    sys.stdout.write(json.dumps(out) + "\n"); sys.stdout.flush()
'''

FAKE_CURSOR_LOGIN = r'''#!{python}
import sys, time
if sys.argv[1:] == ["--version"]:
    print("{version}"); sys.exit(0)
print("Open this link to sign in:")
print("https://cursor.com/loginDeepControl?challenge=SECRET&uuid=u", flush=True)
time.sleep(60)
'''

FAKE_GEMINI_LOGIN = r'''#!{python}
import os, sys, time
if not os.isatty(0):
    print("Manual authorization is required but the current session is non-interactive.")
    sys.exit(1)
sys.stdout.write("\x1b[?1049h\x1b[2JPlease visit the following URL to authorize the application:\r\n\r\n")
sys.stdout.write("https://accounts.google.com/o/oauth2/v2/auth?state=SECRET&code_challenge=x\r\n\r\n")
sys.stdout.write("Enter the authorization code: "); sys.stdout.flush()
time.sleep(60)
'''


def write_script(directory, name, source, **fields):
    path = os.path.join(directory, name)
    with open(path, "w") as handle:
        handle.write(source.format(python=sys.executable, **fields))
    os.chmod(path, stat.S_IRWXU)
    return path


class PureTests(unittest.TestCase):
    def test_redaction_keeps_only_origin_and_path(self):
        self.assertEqual(
            acceptance.redact_url("https://accounts.google.com/o/oauth2/v2/auth?state=S"),
            "https://accounts.google.com/o/oauth2/v2/auth")

    def test_version_is_a_whole_token(self):
        self.assertTrue(acceptance.version_matches("opencode 1.18.33\n", "1.18.33"))
        self.assertFalse(acceptance.version_matches("1.18.330", "1.18.33"))
        self.assertFalse(acceptance.version_matches("11.18.33", "1.18.33"))

    def test_probe_verdict(self):
        self.assertEqual(acceptance.probe_verdict(
            b'noise\n{"type":"neev.account","status":"disconnected"}\n'), "disconnected")
        self.assertIsNone(acceptance.probe_verdict(b"garbage"))

    def test_gemini_challenge_counts_only_once_the_cli_reads_a_code(self):
        url = b"Please visit the following URL to authorize the application:\r\n" \
              b"https://accounts.google.com/o/oauth2/v2/auth?x=1\r\n"
        self.assertIsNone(acceptance.challenge_from("gemini", [url]))
        self.assertEqual(
            acceptance.challenge_from("gemini", [url, b"Enter the authorization code: "]),
            {"origin": "https://accounts.google.com/o/oauth2/v2/auth", "user_code": False})

    def test_an_empty_run_is_not_a_pass(self):
        self.assertFalse(acceptance.summarize([])["ok"])

    def test_egress_reports_every_allowlisted_host(self):
        seen = []
        check = acceptance.check_egress(
            "opencode", reach=lambda host, port, proxy: seen.append(host) or host != "chatgpt.com")
        self.assertFalse(check.ok)
        self.assertEqual(set(seen), {"auth.openai.com", "chatgpt.com"})
        self.assertTrue(acceptance.check_egress("gemini", reach=lambda *a: True).ok)


class FakeBinaryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()

    def pins(self, provider, path, **overrides):
        pins = json.loads(json.dumps(acceptance.PINS))
        pins[provider].update(path=path, **overrides)
        return mock.patch.object(acceptance, "PINS", pins)

    def test_a_no_credential_session_is_disconnected(self):
        agent = write_script(self.tmp, "agent", FAKE_ACP, mode="signin")
        with self.pins("cursor", agent):
            check = acceptance.check_acp_probe("cursor")
        self.assertTrue(check.ok, check)
        self.assertEqual(check.evidence["status"], "disconnected")

    def test_a_no_credential_success_is_reported_as_fake(self):
        # OpenCode's real behaviour: session/new succeeds anonymously. The
        # adapter pins it to the signed-in upstream, so with no configOptions
        # the probe must still say disconnected — never supported.
        agent = write_script(self.tmp, "opencode", FAKE_ACP, mode="anonymous")
        with self.pins("opencode", agent):
            check = acceptance.check_acp_probe("opencode")
        self.assertTrue(check.ok, check)
        # And a provider with no upstream pin that answers anonymously FAILS.
        cursor = write_script(self.tmp, "agent", FAKE_ACP, mode="anonymous")
        with self.pins("cursor", cursor):
            check = acceptance.check_acp_probe("cursor")
        self.assertFalse(check.ok)
        self.assertIn("FAKE SUCCESS", check.detail)

    def test_pin_requires_install_root_and_version(self):
        agent = write_script(self.tmp, "agent", FAKE_CURSOR_LOGIN, version="2026.09.28-64d2043")
        with self.pins("cursor", agent), mock.patch.object(acceptance, "INSTALL_ROOT", os.path.realpath(self.tmp)):
            os.chmod(agent, 0o555)
            self.assertTrue(acceptance.check_pin("cursor").ok)
        with self.pins("cursor", agent), mock.patch.object(acceptance, "INSTALL_ROOT", "/opt/neev/"):
            self.assertFalse(acceptance.check_pin("cursor").ok)
        stale = write_script(self.tmp, "stale", FAKE_CURSOR_LOGIN, version="2026.01.01-deadbee")
        os.chmod(stale, 0o555)
        with self.pins("cursor", stale), mock.patch.object(acceptance, "INSTALL_ROOT", os.path.realpath(self.tmp)):
            self.assertEqual(acceptance.check_pin("cursor").detail, "version mismatch")

    def test_cursor_login_challenge_is_parsed_and_redacted(self):
        agent = write_script(self.tmp, "agent", FAKE_CURSOR_LOGIN, version="x")
        with self.pins("cursor", agent):
            check = acceptance.check_login_challenge("cursor")
        self.assertTrue(check.ok, check)
        self.assertEqual(check.evidence["origin"], "https://cursor.com/loginDeepControl")
        self.assertNotIn("SECRET", json.dumps(check.evidence))

    def test_gemini_login_gets_a_real_terminal(self):
        gemini = write_script(self.tmp, "gemini", FAKE_GEMINI_LOGIN)
        with self.pins("gemini", gemini):
            check = acceptance.check_login_challenge("gemini")
        self.assertTrue(check.ok, check)
        self.assertNotIn("SECRET", json.dumps(check.evidence))
        # Over a pipe the same CLI refuses: the PTY is what makes it work.
        with self.pins("gemini", gemini, pty=False):
            self.assertFalse(acceptance.check_login_challenge("gemini").ok)


if __name__ == "__main__":
    unittest.main()


class HandshakeSummaryTests(unittest.TestCase):
    def test_only_capability_facts_are_kept(self):
        init = {"protocolVersion": 1, "agentCapabilities": {"loadSession": True},
                "authMethods": [{"id": "cursor_login", "description": "Run agent login"}],
                "agentInfo": {"name": "x", "version": "1", "title": "X"}}
        facts = acceptance.summarize_handshake(
            init, {"error": {"code": -32000, "message": "see /home/secret/path"}})
        self.assertEqual(facts["authMethods"], ["cursor_login"])
        self.assertEqual(facts["sessionNew"], {"error": -32000})
        self.assertNotIn("/home/secret", json.dumps(facts))
        facts = acceptance.summarize_handshake(init, {"result": {"sessionId": "ses_SECRET",
            "configOptions": [{"id": "model", "category": "model",
                               "currentValue": "opencode/big-pickle"}]}})
        self.assertEqual(facts["sessionNew"]["configOptions"][0]["currentValuePrefix"], "opencode")
        self.assertNotIn("ses_SECRET", json.dumps(facts))

    def test_protocol_check_against_a_fake_agent(self):
        tmp = tempfile.mkdtemp()
        agent = write_script(tmp, "agent", FAKE_ACP, mode="signin")
        pins = json.loads(json.dumps(acceptance.PINS))
        pins["cursor"]["path"] = agent
        with mock.patch.object(acceptance, "PINS", pins):
            check = acceptance.check_protocol("cursor", timeout=20)
        self.assertTrue(check.ok, check)
        self.assertEqual(check.evidence["sessionNew"], {"error": -32000})


class LauncherCheckTests(unittest.TestCase):
    def test_the_shipped_launcher_maps_each_provider(self):
        for provider in ("cursor", "gemini", "opencode"):
            with self.subTest(provider=provider):
                check = acceptance.check_launcher(provider)
                self.assertTrue(check.ok, check)
