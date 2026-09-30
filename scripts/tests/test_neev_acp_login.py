"""ACP-provider sign-in: output parsers, login drivers, account probe and the
supervisor wiring, driven with fakes. The OpenCode and Gemini transcripts are the
shapes OBSERVED on the pinned binaries (codes/URLs replaced); Cursor's is the
documented shape only (no pinned binary could be verified). Passing these proves
the state machines, NOT that a real sign-in works."""

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))
sys.path.insert(0, str(Path(__file__).parent))

import neev_acp_login_protocol as proto  # noqa: E402
import neev_agent_login_drivers as drivers  # noqa: E402
import neev_agent_supervisor as sup  # noqa: E402
import neev_agent_supervisor_main as main  # noqa: E402
import neev_supervisor_dto as dto  # noqa: E402
from test_neev_agent_login_drivers import FakeChannel  # noqa: E402
from test_neev_agent_supervisor_main import _Scope, _full_env  # noqa: E402

# Observed (pinned opencode 1.18.33 over a pipe), user code replaced.
OPENCODE_OUT = (
    "┌  Add credential\n│\n●  Go to: https://auth.openai.com/codex/device\n"
    "│\n●  Enter code: WXYZ-12345\n\x1b[?25l│\n"
    "◒  Waiting for authorization\x1b[999D\x1b[J◐  Waiting for authorization."
).encode("utf-8")
OPENCODE_DONE = "\x1b[999D\x1b[J◇  Login successful\n".encode("utf-8")
# Observed (pinned gemini 0.62.0), state replaced.
GEMINI_OUT = (
    b"\x1b[?1049h\x1b[2J\x1b[H\x1b[?1006l\x1b[?1002l\x1b[<u\x1b[?7h"
    b"Please visit the following URL to authorize the application:\r\n\r\n"
    b"https://accounts.google.com/o/oauth2/v2/auth?redirect_uri=https%3A%2F%2F"
    b"codeassist.google.com%2Fauthcode&access_type=offline&state=abc\r\n\r\n"
    b"Enter the authorization code: "
)
CURSOR_OUT = b"Open this URL to sign in:\nhttps://cursor.com/loginDeepControl?challenge=c&uuid=u\n"


def feed(protocol, data, size):
    events = []
    for index in range(0, len(data), size):
        events += protocol.receive(data[index:index + size])
    return events


class ParserTests(unittest.TestCase):
    def test_opencode_challenge_survives_any_chunking(self):
        for size in (1, 3, 7, 64, 4096):
            with self.subTest(size=size):
                events = feed(proto.OpenCodeLoginProtocol(), OPENCODE_OUT + OPENCODE_DONE, size)
                kinds = [e.kind for e in events]
                self.assertEqual(kinds, ["challenge", "succeeded"])
                self.assertEqual(events[0].verification_url, proto.OPENAI_DEVICE_URL)
                self.assertEqual(events[0].user_code, "WXYZ-12345")

    def test_opencode_partial_code_is_not_a_challenge(self):
        parser = proto.OpenCodeLoginProtocol()
        self.assertEqual(parser.receive(b"Go to: https://auth.openai.com/codex/device\nEnter code: WXYZ-12"), [])

    def test_opencode_foreign_url_is_refused(self):
        with self.assertRaises(proto.ProtocolError):
            proto.OpenCodeLoginProtocol().receive(
                b"Go to: https://evil.example/codex/device\nEnter code: ABCD-1234\n")

    def test_opencode_failure(self):
        events = proto.OpenCodeLoginProtocol().receive(OPENCODE_OUT + b"\nFailed to authorize\n")
        self.assertEqual([e.kind for e in events], ["challenge", "failed"])

    def test_gemini_url_then_prompt(self):
        for size in (1, 5, 4096):
            with self.subTest(size=size):
                events = feed(proto.GeminiLoginProtocol(), GEMINI_OUT, size)
                self.assertEqual([e.kind for e in events], ["challenge", "awaiting_code"])
                self.assertTrue(events[0].verification_url.startswith(proto.GOOGLE_AUTHORIZE_URL))
                self.assertTrue(events[0].verification_url.endswith("state=abc"))

    def test_gemini_rejected_code(self):
        parser = proto.GeminiLoginProtocol()
        parser.receive(GEMINI_OUT)
        self.assertEqual([e.kind for e in parser.receive(
            b"\r\nFailed to authenticate with authorization code: bad\r\n")], ["failed"])

    def test_cursor_url_only_from_cursor_origin(self):
        events = proto.CursorLoginProtocol().receive(CURSOR_OUT)
        self.assertEqual(events[0].verification_url,
                         "https://cursor.com/loginDeepControl?challenge=c&uuid=u")
        self.assertEqual(proto.CursorLoginProtocol().receive(
            b"https://cursor.com.evil.example/login\n"), [])

    def test_oversized_output_is_refused(self):
        with self.assertRaises(proto.ProtocolError):
            proto.CursorLoginProtocol().receive(b"x" * (proto.MAX_BUFFER_BYTES + 1))

    def test_submit_code_bytes(self):
        self.assertEqual(proto.submit_code_bytes("4/0Abc"), b"4/0Abc\r")
        for bad in ("", "a\nb", "a\rb", "a\x00b", "x" * 513):
            with self.assertRaises(proto.ProtocolError):
                proto.submit_code_bytes(bad)


class DriverTests(unittest.TestCase):
    def test_opencode_needs_marker_exit_zero_and_record(self):
        for present, rc, done, expected in (
            (True, 0, True, "prepared"),
            (False, 0, True, "failed"),   # no auth.json record: not a sign-in
            (True, 1, True, "failed"),
            (True, 0, False, "failed"),   # exited 0 without "Login successful"
        ):
            with self.subTest(present=present, rc=rc, done=done):
                channel = FakeChannel([OPENCODE_OUT] + ([OPENCODE_DONE] if done else []))
                driver = drivers.OpenCodeLoginDriver(channel, lambda p=present: p)
                driver.start()
                state = driver.poll()
                self.assertEqual(state.phase, "awaiting_user")
                self.assertEqual(state.challenge.user_code, "WXYZ-12345")
                driver.poll()
                channel._rc = rc
                self.assertEqual(driver.poll().phase, expected)
        with self.assertRaises(RuntimeError):
            drivers.OpenCodeLoginDriver(FakeChannel(), lambda: True).submit_code("x")

    def test_gemini_shows_url_only_when_reading_and_completes_on_the_record(self):
        record = {"present": False}
        channel = FakeChannel([GEMINI_OUT])
        driver = drivers.GeminiLoginDriver(channel, lambda: record["present"])
        driver.start()
        state = driver.poll()
        self.assertEqual(state.phase, "awaiting_user")
        self.assertIsNone(state.challenge.user_code)
        driver.submit_code("4/0Abc-DEF")
        self.assertEqual(channel.writes, [b"4/0Abc-DEF\r"])
        self.assertEqual(driver.poll().phase, "verifying")
        record["present"] = True  # CLI cached oauth_creds.json; it does NOT exit
        state = driver.poll()
        self.assertEqual((state.phase, state.account_label), ("prepared", "Google"))

    def test_gemini_exit_before_record_fails_and_code_only_once(self):
        channel = FakeChannel([GEMINI_OUT])
        driver = drivers.GeminiLoginDriver(channel, lambda: False)
        driver.start()
        driver.poll()
        driver.submit_code("c")
        with self.assertRaises(RuntimeError):
            driver.submit_code("c")
        channel._rc = 1
        self.assertEqual(driver.poll().failure, "reauth_required")
        early = drivers.GeminiLoginDriver(FakeChannel(), lambda: False)
        with self.assertRaises(RuntimeError):
            early.submit_code("c")  # not reading a code yet

    def test_cursor_prepared_only_with_challenge_exit_zero_and_record(self):
        for present, rc, expected in ((True, 0, "prepared"), (False, 0, "failed"),
                                      (True, 1, "failed")):
            with self.subTest(present=present, rc=rc):
                channel = FakeChannel([CURSOR_OUT])
                driver = drivers.CursorLoginDriver(channel, lambda p=present: p)
                driver.start()
                self.assertEqual(driver.poll().phase, "awaiting_user")
                channel._rc = rc
                self.assertEqual(driver.poll().phase, expected)
        silent = FakeChannel()
        silent._rc = 0
        quiet = drivers.CursorLoginDriver(silent, lambda: True)
        quiet.start()
        self.assertEqual(quiet.poll().failure, "provider_unavailable")


class ProbeTests(unittest.TestCase):
    def probe(self, out, rc=0, present=None, label="Google"):
        channel = FakeChannel([out])
        channel._rc = rc
        return drivers.AcpAccountProbe(channel, label, present).probe(), channel

    def test_verdicts(self):
        result, channel = self.probe(b'{"type":"neev.account","status":"supported"}\n')
        self.assertEqual(result, ("supported", "Google"))
        self.assertTrue(channel.terminated)
        result, _ = self.probe(b'{"type":"neev.account","status":"disconnected"}\n')
        self.assertEqual(result, "disconnected")
        for bad in (b'{"type":"neev.account","status":"unavailable"}\n', b"garbage\n", b""):
            with self.subTest(bad=bad):
                with self.assertRaises(RuntimeError):
                    self.probe(bad, rc=4)

    def test_missing_record_is_disconnected_without_spawning(self):
        channel = FakeChannel([b'{"type":"neev.account","status":"supported"}\n'])
        result = drivers.AcpAccountProbe(channel, "ChatGPT", lambda: False).probe()
        self.assertEqual(result, "disconnected")
        self.assertFalse(channel.started)


class SupervisorWiringTests(unittest.TestCase):
    def factories(self):
        config = main.config_from_env(_full_env("/data/agents"))
        self.built = []

        def channel_factory(provider, store_scope, handle, argv, pty=False):
            self.built.append((provider, tuple(argv), pty))
            return FakeChannel()

        return main.make_factories(config, volumes=object(), channel_factory=channel_factory)

    def test_fixed_official_login_commands(self):
        driver_for, _ = self.factories()
        scope, handle = _Scope(3, 7), "h" * 32
        self.assertIsInstance(driver_for(dto.Provider.OPENCODE, scope, handle),
                              drivers.OpenCodeLoginDriver)
        self.assertIsInstance(driver_for(dto.Provider.GEMINI, scope, handle),
                              drivers.GeminiLoginDriver)
        self.assertIsInstance(driver_for(dto.Provider.CURSOR, scope, handle),
                              drivers.CursorLoginDriver)
        self.assertEqual(self.built, [
            (dto.Provider.OPENCODE, ("/usr/local/bin/opencode", "auth", "login", "--provider",
                                     "openai", "--method", "ChatGPT Pro/Plus (headless)"), False),
            (dto.Provider.GEMINI, ("/usr/local/bin/gemini",), True),
            (dto.Provider.CURSOR, ("/usr/local/bin/agent", "login"), False),
        ])

    def test_probes_run_the_adapter_probe(self):
        _, probe_for = self.factories()
        for provider in (dto.Provider.CURSOR, dto.Provider.GEMINI, dto.Provider.OPENCODE):
            self.assertIsInstance(probe_for(provider, _Scope(3, 7), "h" * 32),
                                  drivers.AcpAccountProbe)
            argv = self.built[-1][1]
            self.assertEqual(argv[-1], "--probe")
            self.assertIn(provider.value, argv)

    def test_challenge_shapes_and_submit_rules(self):
        self.assertEqual(sup.LOGIN_FLOWS[dto.Provider.OPENCODE], "device")
        self.assertEqual(sup.LOGIN_FLOWS[dto.Provider.GEMINI], "authorization_code")
        self.assertEqual(sup.LOGIN_FLOWS[dto.Provider.CURSOR], "browser")
        self.assertEqual(set(sup.LOGIN_FLOWS), set(dto.Provider))


class CredentialRecordTests(unittest.TestCase):
    def present(self, relative, payload, valid):
        with tempfile.TemporaryDirectory() as tmp:
            config = main.config_from_env(dict(_full_env("/data/agents"),
                                               NEEV_SUPERVISOR_STORE_ROOT=tmp))
            handle = "h" * 32
            path = os.path.join(tmp, handle, "mount", *relative)
            os.makedirs(os.path.dirname(path))
            if payload is not None:
                with open(path, "w") as handle_fp:
                    handle_fp.write(payload)
            return main._json_record_present(config, handle, relative, valid)()

    def test_gemini_record(self):
        rel = main._GEMINI_CREDS
        self.assertTrue(self.present(rel, json.dumps({"refresh_token": "r", "access_token": "a"}),
                                     main._gemini_record))
        for bad in (None, "{}", json.dumps({"refresh_token": ""}), "not json", "[]"):
            with self.subTest(bad=bad):
                self.assertFalse(self.present(rel, bad, main._gemini_record))

    def test_opencode_record(self):
        rel = main._OPENCODE_CREDS
        good = {"openai": {"type": "oauth", "refresh": "r", "access": "a", "expires": 1}}
        self.assertTrue(self.present(rel, json.dumps(good), main._opencode_record))
        for bad in ({}, {"openai": {"type": "api", "key": "k"}},
                    {"opencode": {"type": "oauth", "refresh": "r", "access": "a"}},
                    {"openai": {"type": "oauth", "refresh": "", "access": "a"}}):
            with self.subTest(bad=bad):
                self.assertFalse(self.present(rel, json.dumps(bad), main._opencode_record))

    def test_cursor_record(self):
        rel = main._CURSOR_CREDS
        self.assertEqual(rel, (".config", "cursor", "auth.json"))
        good = {"accessToken": "a", "refreshToken": "r", "apiKey": None}
        self.assertTrue(self.present(rel, json.dumps(good), main._cursor_record))
        for bad in ({}, {"accessToken": "a"}, {"accessToken": "", "refreshToken": "r"},
                    {"apiKey": "k"}):
            with self.subTest(bad=bad):
                self.assertFalse(self.present(rel, json.dumps(bad), main._cursor_record))

    def test_symlinked_record_is_not_followed(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = main.config_from_env(dict(_full_env("/data/agents"),
                                               NEEV_SUPERVISOR_STORE_ROOT=tmp))
            handle = "h" * 32
            target = os.path.join(tmp, "elsewhere.json")
            with open(target, "w") as fp:
                json.dump({"refresh_token": "r"}, fp)
            path = os.path.join(tmp, handle, "mount", *main._GEMINI_CREDS)
            os.makedirs(os.path.dirname(path))
            os.symlink(target, path)
            self.assertFalse(main._json_record_present(
                config, handle, main._GEMINI_CREDS, main._gemini_record)())


class BaseImageTests(unittest.TestCase):
    def test_the_base_agent_image_file_set_still_imports(self):
        """Dockerfile.native.agent does not ship the ACP modules; the supervisor,
        launcher and drivers must still import from exactly the files it copies."""
        import re
        import shutil
        import subprocess

        root = Path(__file__).parents[2]
        listed = re.findall(r"scripts/operations/(neev_[a-z_]+\.py)",
                            (root / "Dockerfile.native.agent").read_text())
        with tempfile.TemporaryDirectory() as directory:
            for name in listed:
                shutil.copy(OPERATIONS / name, directory)
            result = subprocess.run(
                [sys.executable, "-I", "-c",
                 "import sys; sys.path.insert(0, %r); import neev_agent_supervisor_main, "
                 "neev_agent_launch, neev_agent_login_drivers" % directory],
                capture_output=True, text=True, timeout=60,
            )
        self.assertEqual(result.returncode, 0, result.stderr[-2000:])
        self.assertNotIn("neev_acp_login_protocol.py", listed)

    def test_the_acp_layer_ships_the_acp_modules(self):
        root = Path(__file__).parents[2]
        layer = (root / "Dockerfile.native.agent.acp").read_text()
        for name in ("neev_acp_stream_adapter.py", "neev_acp_login_protocol.py",
                     "neev_acp_provider_acceptance.py"):
            self.assertIn("scripts/operations/" + name, layer)
            self.assertTrue((OPERATIONS / name).is_file(), name)
        # Every downloaded artifact is sha256-pinned, one per provider, and the
        # linked paths are exactly the launcher's pins.
        import neev_agent_launch as launch

        self.assertEqual(layer.count("ADD --checksum=sha256:"), 3)
        for line in layer.splitlines():
            if line.startswith("ADD "):
                self.assertIn("--checksum=sha256:", line)
        for provider, (path,) in launch.ACP_AGENT_PINS.items():
            self.assertIn(f" {path}", layer, provider)
        self.assertIn("6e4cd936a4866b8a77c50ff51a564460d715772fabc477a01aa0f0455d9559f0", layer)
        # Still opt-in: the layer never turns a provider on.
        self.assertNotIn("NEEV_AGENT_ACP_PROVIDERS=", layer)


if __name__ == "__main__":
    unittest.main()
