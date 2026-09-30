"""Direct sign-in (direct-signin-contract.md): the vendor CLI's own browser login
with its loopback callback relayed by Neev.

URL shapes are the ones MEASURED on the pinned CLIs (codex-cli 0.155.1,
claude 2.1.267) with their secrets replaced. The loopback tests use a REAL local
HTTP listener that behaves like the measured CLIs — including Claude's: it
exits on a wrong state or a missing code — and a real delivery through the same
`--serve` entry point production runs inside the sandbox netns. These prove the
binding, validation and delivery; they do not prove a provider accepts a code
(that needs a real sign-in)."""

import http.server
import json
import os
import socket
import stat
import sys
import tempfile
import threading
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))
sys.path.insert(0, str(Path(__file__).parent))

import neev_agent_login_drivers as drivers  # noqa: E402
import neev_agent_supervisor as sup  # noqa: E402
import neev_browser_capture as capture  # noqa: E402
import neev_codex_login_protocol as codex_proto  # noqa: E402
import neev_login_callback as callback  # noqa: E402
import neev_supervisor_dto as dto  # noqa: E402
from test_neev_agent_login_drivers import FakeChannel  # noqa: E402

STATE = "Qx7bW2mZ0kP9sR4tV1yU8nA3cF6hJ5dL2gE0iO7uK4w"  # 43 chars, like both CLIs
CODEX_URL = (
    "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_EMoamEEZ73f0CkXaXp7hrann"
    "&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&scope=openid+profile+email+offline_access"
    "&code_challenge=abc&code_challenge_method=S256&id_token_add_organizations=true"
    "&codex_cli_simplified_flow=true&state=" + STATE + "&originator=neev")
CLAUDE_BROWSER_URL = (
    "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"
    "&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A33127%2Fcallback&scope=user%3Aprofile"
    "&code_challenge=abc&code_challenge_method=S256&state=" + STATE)
CLAUDE_PASTE_URL = CLAUDE_BROWSER_URL.replace(
    "http%3A%2F%2Flocalhost%3A33127%2Fcallback",
    "https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback")


def codex_target():
    return callback.target_from_authorize_url("codex", CODEX_URL)


class BindingTests(unittest.TestCase):
    def test_targets_come_from_the_clis_own_urls(self):
        self.assertEqual(codex_target(), callback.CallbackTarget("codex", 1455, "/auth/callback", STATE))
        claude = callback.target_from_authorize_url("claude", CLAUDE_BROWSER_URL)
        self.assertEqual((claude.port, claude.path), (33127, "/callback"))
        # The paste URL has no loopback redirect: not a direct-sign-in target.
        with self.assertRaises(callback.CallbackRejected):
            callback.target_from_authorize_url("claude", CLAUDE_PASTE_URL)
        for bad in (CODEX_URL.replace("localhost", "evil.example"),
                    CODEX_URL.replace("%2Fauth%2Fcallback", "%2Fadmin"),
                    CODEX_URL.replace("1455", "80"),
                    CODEX_URL.replace("https://", "http://")):
            with self.subTest(bad=bad[:60]):
                with self.assertRaises(callback.CallbackRejected):
                    callback.target_from_authorize_url("codex", bad)

    def test_a_valid_callback_is_reencoded_in_a_fixed_order(self):
        target = codex_target()
        for raw in (f"state={STATE}&code=ac_123-XYZ.4",
                    f"?code=ac_123-XYZ.4&state={STATE}",
                    f"http://localhost:1455/auth/callback?code=ac_123-XYZ.4&state={STATE}",
                    f"http://127.0.0.1:1455/auth/callback?code=ac_123-XYZ.4&state={STATE}"):
            with self.subTest(raw=raw[:30]):
                self.assertEqual(callback.canonical_query(raw, target),
                                 f"code=ac_123-XYZ.4&state={STATE}")
        # A denial is relayed too, so the CLI ends cleanly.
        self.assertEqual(
            callback.canonical_query(f"error=access_denied&state={STATE}", target),
            f"state={STATE}&error=access_denied")

    def test_everything_else_is_refused_with_a_fixed_reason(self):
        target = codex_target()
        cases = {
            f"code=x&state={STATE[:-1]}Z": "state",
            "code=x": "state",
            f"code=x&state={STATE}&redirect=http://evil": "key",
            f"code=x&code=y&state={STATE}": "key",
            f"state={STATE}": "code",
            f"code=x&error=e&state={STATE}": "code",
            f"code={'x' * 1100}&state={STATE}": "shape",
            f"code=a\tb&state={STATE}": "shape",
            f"code=%0a&state={STATE}": "shape",
            f"http://localhost:1456/auth/callback?code=x&state={STATE}": "target",
            f"http://localhost:1455/other?code=x&state={STATE}": "target",
            f"http://evil.example:1455/auth/callback?code=x&state={STATE}": "target",
            f"https://localhost:1455/auth/callback?code=x&state={STATE}": "target",
            f"http://u:p@localhost:1455/auth/callback?code=x&state={STATE}": "target",
            "": "shape",
        }
        for raw, reason in cases.items():
            with self.subTest(raw=raw[:50]):
                with self.assertRaises(callback.CallbackRejected) as caught:
                    callback.canonical_query(raw, target)
                self.assertEqual(caught.exception.reason, reason)
                self.assertNotIn(STATE, str(caught.exception))

    def test_a_gate_accepts_once(self):
        gate = callback.CallbackGate(codex_target())
        gate.accept(f"code=x&state={STATE}")
        with self.assertRaises(callback.CallbackRejected) as caught:
            gate.accept(f"code=x&state={STATE}")
        self.assertEqual(caught.exception.reason, "used")
        # A rejected attempt does NOT consume the gate: the user can retry.
        fresh = callback.CallbackGate(codex_target())
        with self.assertRaises(callback.CallbackRejected):
            fresh.accept("code=x&state=wrong-state-value-00")
        self.assertFalse(fresh.used)

    def test_serve_argv_is_fixed_and_carries_no_secret(self):
        argv = callback.serve_argv("neev-a-org1", 61000, 61000)
        self.assertEqual(argv[:4], ["/usr/sbin/ip", "netns", "exec", "neev-a-org1"])
        self.assertIn("--no-new-privs", argv)
        self.assertEqual(argv[-1], "--serve")
        for bad in ("", "../x", "-x", "a/b"):
            with self.assertRaises(callback.CallbackRejected):
                callback.serve_argv(bad, 61000, 61000)
        with self.assertRaises(callback.CallbackRejected):
            callback.serve_argv("ns", 0, 61000)


class _CliListener(http.server.BaseHTTPRequestHandler):
    """Behaves like the measured CLIs' loopback listeners."""

    expected_state = STATE
    path_expected = "/callback"
    exit_on_bad = True  # Claude 2.1.267: a wrong state or no code ends the login
    received = []
    dead = False

    def do_GET(self):  # noqa: N802
        cls = type(self)
        path, _, query = self.path.partition("?")
        cls.received.append((path, query, self.headers.get("Host")))
        if path != cls.path_expected:
            self.send_response(404)
            self.end_headers()
            return
        params = dict(p.split("=", 1) for p in query.split("&") if "=" in p)
        if params.get("state") != cls.expected_state or not params.get("code"):
            self.send_response(400)
            self.end_headers()
            if cls.exit_on_bad:
                cls.dead = True
            return
        self.send_response(302)
        self.send_header("Location", "https://platform.claude.com/oauth/code/success")
        self.end_headers()

    def log_message(self, *args):
        pass


class LoopbackDeliveryTests(unittest.TestCase):
    def setUp(self):
        handler = type("Handler", (_CliListener,), {"received": [], "dead": False})
        self.handler = handler
        self.server = http.server.HTTPServer(("127.0.0.1", 0), handler)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        # Production runs this same entry point under `ip netns exec` + setpriv.
        self.argv = [sys.executable, "-I", "-B", str(OPERATIONS / "neev_login_callback.py"), "--serve"]

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()

    def target(self):
        return callback.CallbackTarget("claude", self.port, "/callback", STATE)

    def test_a_bound_callback_is_delivered_through_the_serve_entry_point(self):
        gate = callback.CallbackGate(self.target())
        query = gate.accept(f"http://localhost:{self.port}/callback?code=c0de&state={STATE}")
        status = callback.deliver(gate.target, query, self.argv)
        self.assertEqual(callback.outcome(status), "delivered")
        self.assertEqual(self.handler.received,
                         [("/callback", f"code=c0de&state={STATE}", f"localhost:{self.port}")])
        self.assertFalse(self.handler.dead)

    def test_a_forged_callback_never_reaches_the_cli_so_claude_survives(self):
        gate = callback.CallbackGate(self.target())
        with self.assertRaises(callback.CallbackRejected):
            gate.accept(f"code=c0de&state={'A' * 43}")
        self.assertEqual(self.handler.received, [])
        self.assertFalse(self.handler.dead)

    def test_unreachable_listener_is_unavailable_not_success(self):
        free = socket.socket()
        free.bind(("127.0.0.1", 0))
        port = free.getsockname()[1]
        free.close()
        target = callback.CallbackTarget("claude", port, "/callback", STATE)
        self.assertEqual(callback.outcome(callback.deliver(target, f"code=x&state={STATE}", self.argv)),
                         "unavailable")

    def test_the_serve_child_refuses_other_paths_and_ports(self):
        target = callback.CallbackTarget("claude", self.port, "/callback", STATE)
        forged = callback.CallbackTarget.__new__(callback.CallbackTarget)
        object.__setattr__(forged, "provider", "claude")
        object.__setattr__(forged, "port", self.port)
        object.__setattr__(forged, "path", "/admin")
        object.__setattr__(forged, "state", STATE)
        self.assertEqual(callback.deliver(forged, "code=x", self.argv), 0)
        self.assertEqual(self.handler.received, [])
        self.assertEqual(callback.outcome(callback.deliver(target, f"code=x&state={STATE}", self.argv)),
                         "delivered")


class CaptureHelperTests(unittest.TestCase):
    def test_records_only_the_claude_url_once_and_never_follows_symlinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, ".neev-browser-url")
            env = {"NEEV_BROWSER_CAPTURE": path}
            self.assertEqual(capture.capture(["x", "https://evil.example/"], env), 0)
            self.assertFalse(os.path.exists(path))
            self.assertEqual(capture.capture(["x", CLAUDE_BROWSER_URL], env), 0)
            with open(path) as handle:
                self.assertEqual(handle.read(), CLAUDE_BROWSER_URL + "\n")
            self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o600)
            capture.capture(["x", CLAUDE_PASTE_URL], env)  # no overwrite
            with open(path) as handle:
                self.assertEqual(handle.read(), CLAUDE_BROWSER_URL + "\n")
            victim = os.path.join(tmp, "victim")
            link = os.path.join(tmp, "link")
            os.symlink(victim, link)
            capture.capture(["x", CLAUDE_BROWSER_URL], {"NEEV_BROWSER_CAPTURE": link})
            self.assertFalse(os.path.exists(victim))


class DeliveringChannel(FakeChannel):
    def __init__(self, chunks=None, captured=None, status=302):
        super().__init__(chunks)
        self.captured = captured
        self.deliveries = []
        self.status = status

    def read_browser_capture(self):
        return self.captured

    def deliver_loopback(self, target, query):
        self.deliveries.append((target.port, target.path, query))
        return self.status


def codex_start_result():
    return json.dumps({"id": 2, "result": {"type": "chatgpt", "loginId": "login-1",
                                           "authUrl": CODEX_URL}}).encode() + b"\n"


class CodexDriverTests(unittest.TestCase):
    def test_browser_login_offers_the_clis_url_and_delivers_only_a_bound_callback(self):
        channel = DeliveringChannel([b'{"id":1,"result":{"userAgent":"x"}}\n', codex_start_result()])
        driver = drivers.CodexLoginDriver(channel, "browser")
        driver.start()
        driver.poll()
        state = driver.poll()
        self.assertEqual(state.phase, "awaiting_user")
        self.assertEqual(state.challenge.kind, "browser_callback")
        self.assertEqual((state.challenge.callback_port, state.challenge.callback_path),
                         (1455, "/auth/callback"))
        self.assertIn(b'"type":"chatgpt"', b"".join(channel.writes))
        self.assertEqual(driver.deliver_callback(f"code=x&state={'B' * 43}"), "rejected")
        self.assertEqual(channel.deliveries, [])
        self.assertEqual(driver.deliver_callback(f"code=x&state={STATE}"), "delivered")
        self.assertEqual(channel.deliveries, [(1455, "/auth/callback", f"code=x&state={STATE}")])
        self.assertEqual(driver.poll().phase, "verifying")
        self.assertEqual(driver.deliver_callback(f"code=x&state={STATE}"), "rejected")

    def test_device_method_is_unchanged(self):
        driver = drivers.CodexLoginDriver(DeliveringChannel(), "device")
        self.assertEqual(driver.deliver_callback(f"code=x&state={STATE}"), "rejected")

    def test_a_redirect_other_than_the_pinned_listener_fails_the_login(self):
        bad = CODEX_URL.replace("localhost%3A1455", "localhost%3A1456")
        start = json.dumps({"id": 2, "result": {"type": "chatgpt", "loginId": "l",
                                                "authUrl": bad}}).encode() + b"\n"
        driver = drivers.CodexLoginDriver(
            DeliveringChannel([b'{"id":1,"result":{"userAgent":"x"}}\n', start]), "browser")
        driver.start()
        driver.poll()
        self.assertEqual(driver.poll().phase, "failed")


CLAUDE_OUT = ("Opening browser to sign in…\nIf the browser didn't open, visit: "
              + CLAUDE_PASTE_URL + "\nPaste code here if prompted > ").encode()


class ClaudeDriverTests(unittest.TestCase):
    def test_direct_sign_in_upgrades_with_the_paste_url_as_fallback(self):
        channel = DeliveringChannel([CLAUDE_OUT], captured=CLAUDE_BROWSER_URL)
        present = {"value": False}
        driver = drivers.ClaudeLoginDriver(channel, lambda: present["value"], direct=True)
        driver.start()
        state = driver.poll()
        self.assertEqual(state.challenge.kind, "browser_callback")
        self.assertEqual(state.challenge.verification_url, CLAUDE_BROWSER_URL)
        self.assertEqual(state.challenge.fallback_url, CLAUDE_PASTE_URL)
        self.assertEqual(state.challenge.callback_port, 33127)
        self.assertEqual(driver.deliver_callback(f"code=c&state={STATE}"), "delivered")
        with self.assertRaises(RuntimeError):
            driver.submit_code("pasted")  # the callback already answered
        channel._rc = 0
        self.assertEqual(driver.poll().phase, "failed")  # no credential record
        present["value"] = True
        driver2 = drivers.ClaudeLoginDriver(DeliveringChannel([CLAUDE_OUT], captured=CLAUDE_BROWSER_URL),
                                            lambda: True, direct=True)
        driver2.start()
        driver2.poll()
        driver2.deliver_callback(f"code=c&state={STATE}")
        driver2._channel._rc = 0
        self.assertEqual(driver2.poll().phase, "prepared")

    def test_no_upgrade_without_direct_or_with_a_different_login(self):
        legacy = drivers.ClaudeLoginDriver(DeliveringChannel([CLAUDE_OUT], captured=CLAUDE_BROWSER_URL),
                                           lambda: True)
        legacy.start()
        self.assertIsNone(legacy.poll().challenge.kind)
        other = CLAUDE_BROWSER_URL.replace(STATE, "Z" * 43)
        mismatched = drivers.ClaudeLoginDriver(DeliveringChannel([CLAUDE_OUT], captured=other),
                                               lambda: True, direct=True)
        mismatched.start()
        self.assertIsNone(mismatched.poll().challenge.kind)
        self.assertEqual(mismatched.deliver_callback(f"code=c&state={'Z' * 43}"), "rejected")

    def test_paste_fallback_still_works_after_upgrade(self):
        channel = DeliveringChannel([CLAUDE_OUT], captured=CLAUDE_BROWSER_URL)
        driver = drivers.ClaudeLoginDriver(channel, lambda: True, direct=True)
        driver.start()
        driver.poll()
        driver.submit_code("pasted-code")
        self.assertEqual(driver.deliver_callback(f"code=c&state={STATE}"), "rejected")
        self.assertEqual(channel.deliveries, [])


class WireTests(unittest.TestCase):
    def test_challenge_and_request_round_trip(self):
        challenge = dto.BrowserCallbackChallenge(CODEX_URL, 1455, "/auth/callback", None, dto.UnixSec(9))
        encoded = dto._encode_login_challenge(challenge)
        self.assertEqual(encoded["kind"], "browser_callback")
        self.assertEqual(dto._login_challenge(encoded), challenge)
        claude = dto.BrowserCallbackChallenge(CLAUDE_BROWSER_URL, 33127, "/callback",
                                              CLAUDE_PASTE_URL, dto.UnixSec(9))
        self.assertEqual(dto._login_challenge(dto._encode_login_challenge(claude)), claude)
        for bad in (("https://evil.example/?x", 1455, "/auth/callback"),
                    (CODEX_URL, 80, "/auth/callback"), (CODEX_URL, 1455, "/admin")):
            with self.assertRaises(dto.DtoError):
                dto.BrowserCallbackChallenge(*bad, None, dto.UnixSec(9))
        for bad_query in ("", "a b", "x" * 1025):
            with self.assertRaises(dto.DtoError):
                dto.CallbackQuery(bad_query)
        self.assertNotIn(STATE, repr(dto.CallbackQuery(f"state={STATE}")))

    def test_absent_method_is_the_deployed_coordinators_request(self):
        from test_neev_agent_supervisor import ticket
        legacy = dto.StartLoginRequest(ticket(dto.Provider.CODEX))
        self.assertIsNone(legacy.method)
        self.assertNotIn("method", dto._encode_request_body(legacy)
                         if hasattr(dto, "_encode_request_body") else {})
        with self.assertRaises(dto.DtoError):
            dto.StartLoginRequest(ticket(dto.Provider.CODEX), "magic")


from test_neev_agent_supervisor import (  # noqa: E402
    BOOT, SupervisorTestBase, binding, request, ticket,
)


class SupervisorOpTests(SupervisorTestBase):
    def setUp(self):
        super().setUp()
        self.channels = []

        def driver_for(provider, store_scope, handle, method=None):
            channel = DeliveringChannel(
                [b'{"id":1,"result":{"userAgent":"x"}}\n', codex_start_result()])
            self.channels.append(channel)
            return drivers.CodexLoginDriver(channel, "browser" if method == "browser" else "device")

        self.sup = sup.Supervisor(
            BOOT, self.volumes, driver_for, now=lambda: self.clock[0],
            mint_id=self.mint_id, probe_for=self.probe_for,
        )
        self.sup.recover()

    def test_deliver_callback_end_to_end_and_on_the_wire(self):
        b = binding(dto.Provider.CODEX)
        self.ok(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX), "browser")))
        # The encoded start carries the method; a legacy start does not.
        self.assertEqual(dto.decode_request(dto.encode_request(
            request(dto.StartLoginRequest(ticket(dto.Provider.CODEX), "browser")))).body.method,
            "browser")
        self.assertIsNone(dto.decode_request(dto.encode_request(
            request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))).body.method)
        self.ok(request(dto.ObserveLoginRequest(b), seq="2", rid="b" * 32))
        snap = self.ok(request(dto.ObserveLoginRequest(b), seq="3", rid="c" * 32)).body
        self.assertIsInstance(snap.challenge, dto.BrowserCallbackChallenge)
        self.assertEqual(snap.challenge.callback_port, 1455)
        wrong = request(dto.DeliverCallbackRequest(b, dto.CallbackQuery(f"code=x&state={'Q' * 43}")),
                        seq="4", rid="d" * 32)
        self.assertIs(self.err(wrong), dto.ErrorCode.REJECTED)
        self.assertEqual(self.channels[0].deliveries, [])
        right = request(dto.DeliverCallbackRequest(b, dto.CallbackQuery(f"code=x&state={STATE}")),
                        seq="5", rid="e" * 32)
        self.assertEqual(dto.decode_request(dto.encode_request(right)), right)
        result = self.ok(right)
        self.assertIs(result.op(), dto.Op.DELIVER_CALLBACK)
        self.assertIs(result.body.state, dto.LoginState.VERIFYING)
        self.assertEqual(len(self.channels[0].deliveries), 1)

    def test_deliver_callback_on_a_legacy_device_login_is_conflict(self):
        b = binding(dto.Provider.CODEX)
        self.ok(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
        self.ok(request(dto.ObserveLoginRequest(b), seq="2", rid="b" * 32))
        wrong = request(dto.DeliverCallbackRequest(b, dto.CallbackQuery(f"code=x&state={STATE}")),
                        seq="3", rid="c" * 32)
        self.assertIs(self.err(wrong), dto.ErrorCode.CONFLICT)
        self.assertEqual(self.channels[0].deliveries, [])


if __name__ == "__main__":
    unittest.main()


class ImageTests(unittest.TestCase):
    def test_the_base_image_ships_the_direct_sign_in_modules(self):
        root = Path(__file__).parents[2]
        dockerfile = (root / "Dockerfile.native.agent").read_text()
        for name in ("neev_login_callback.py", "neev_browser_capture.py"):
            self.assertIn("scripts/operations/" + name, dockerfile)
        import neev_agent_supervisor_main as main

        self.assertEqual(os.path.basename(main._CAPTURE_HELPER), "neev_browser_capture.py")
        with open(OPERATIONS / "neev_browser_capture.py") as handle:
            self.assertEqual(handle.readline().strip(), "#!/usr/bin/python3 -I")

    def test_claude_env_points_browser_at_the_capture_helper_only_when_direct(self):
        import neev_agent_supervisor_main as main
        from test_neev_agent_supervisor_main import _full_env

        config = main.config_from_env(_full_env("/data/agents"))
        direct = main._login_env(config, dto.Provider.CLAUDE, "/h", {}, browser_capture="/w/.neev-browser-url")
        self.assertEqual(direct["BROWSER"], main._CAPTURE_HELPER)
        self.assertEqual(direct["NEEV_BROWSER_CAPTURE"], "/w/.neev-browser-url")
        legacy = main._login_env(config, dto.Provider.CLAUDE, "/h", {})
        self.assertNotIn("BROWSER", legacy)

    def test_capture_file_reader_is_bounded_and_refuses_symlinks(self):
        import neev_agent_supervisor_main as main

        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, ".neev-browser-url")
            self.assertIsNone(main._read_capture(path))
            with open(path, "w") as handle:
                handle.write(CLAUDE_BROWSER_URL + "\n")
            self.assertEqual(main._read_capture(path), CLAUDE_BROWSER_URL)
            link = os.path.join(tmp, "link")
            os.symlink(path, link)
            self.assertIsNone(main._read_capture(link))
            with open(path, "w") as handle:
                handle.write("x" * 3000)
            self.assertIsNone(main._read_capture(path))


class ReviewFixTests(unittest.TestCase):
    """direct-signin-review.md B2 / I5."""

    def start_codex(self, status):
        channel = DeliveringChannel([b'{"id":1,"result":{"userAgent":"x"}}\n', codex_start_result()],
                                    status=status)
        driver = drivers.CodexLoginDriver(channel, "browser")
        driver.start()
        driver.poll()
        driver.poll()
        return driver, channel

    def test_a_delivery_that_never_reached_the_listener_does_not_burn_the_gate(self):
        driver, channel = self.start_codex(status=0)
        query = f"code=x&state={STATE}"
        self.assertEqual(driver.deliver_callback(query), "unavailable")
        self.assertEqual(driver.poll().phase, "awaiting_user")
        self.assertFalse(driver._gate.used)
        channel.status = 200  # the listener is reachable again
        self.assertEqual(driver.deliver_callback(query), "delivered")
        self.assertTrue(driver._gate.used)
        self.assertEqual(len(channel.deliveries), 2)
        # Spent now: a replay (extension + paste race) is refused, not re-sent.
        self.assertEqual(driver.deliver_callback(query), "rejected")
        self.assertEqual(len(channel.deliveries), 2)

    def test_a_listener_that_answered_with_an_error_spends_the_callback(self):
        driver, channel = self.start_codex(status=400)
        self.assertEqual(driver.deliver_callback(f"code=x&state={STATE}"), "rejected")
        self.assertTrue(driver._gate.used)

    def test_claude_paste_still_works_after_an_unreachable_delivery(self):
        channel = DeliveringChannel([CLAUDE_OUT], captured=CLAUDE_BROWSER_URL, status=0)
        driver = drivers.ClaudeLoginDriver(channel, lambda: True, direct=True)
        driver.start()
        driver.poll()
        self.assertEqual(driver.deliver_callback(f"code=c&state={STATE}"), "unavailable")
        driver.submit_code("pasted-code")  # not refused as "used"
        self.assertEqual(driver.poll().phase, "verifying")

    def test_delivery_only_into_this_logins_own_slot(self):
        callback.assert_delivery_owner("neev-agent-a", "a", True, True)
        for args in (("neev-agent-b", "a", True, True),   # another slot's netns
                     ("neev-agent-a", "a", False, True),  # slot lock not held
                     ("neev-agent-a", "a", True, False),  # CLI already gone
                     ("", "a", True, True)):
            with self.assertRaises(callback.CallbackRejected) as caught:
                callback.assert_delivery_owner(*args)
            self.assertEqual(caught.exception.reason, "owner")

    def test_login_channel_refuses_delivery_without_its_slot(self):
        import neev_agent_supervisor_main as main
        from test_neev_agent_supervisor_main import _Scope, _full_env

        config = main.config_from_env(_full_env("/data/agents"))
        channel = main.LoginChannel(config, object(), dto.Provider.CODEX, _Scope(3, 7),
                                    "h" * 32, ("codex", "app-server"))

        class Alive:
            netns = "neev-agent-a"

            def wait(self, timeout=0):
                raise TimeoutError

        channel._sandbox = Alive()
        channel._proxy_cm = None  # the slot proxy (and its lock) is not held
        with self.assertRaises(callback.CallbackRejected):
            channel.deliver_loopback(codex_target(), f"code=x&state={STATE}")
        channel._proxy_cm = object()
        Alive.netns = "neev-agent-b"
        with self.assertRaises(callback.CallbackRejected):
            channel.deliver_loopback(codex_target(), f"code=x&state={STATE}")

    def test_the_delivery_timeout_is_short(self):
        self.assertLessEqual(callback.DELIVER_TIMEOUT_SECONDS, 5)


class DiagnosticTests(unittest.TestCase):
    def test_a_refused_callback_logs_only_a_fixed_category(self):
        import contextlib
        import io

        channel = DeliveringChannel([b'{"id":1,"result":{"userAgent":"x"}}\n', codex_start_result()])
        driver = drivers.CodexLoginDriver(channel, "browser")
        driver.start()
        driver.poll()
        driver.poll()
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            driver.deliver_callback(f"code=SECRETCODE&state={STATE}&iss=https://auth.openai.com")
            driver.deliver_callback(f"code=SECRETCODE&state={'W' * 43}")
        log = err.getvalue()
        self.assertIn("NEEV-CALLBACK-DIAG provider=codex reason=key", log)
        self.assertIn("NEEV-CALLBACK-DIAG provider=codex reason=state", log)
        for secret in ("SECRETCODE", STATE, "W" * 43, "auth.openai.com"):
            self.assertNotIn(secret, log)
        self.assertEqual(channel.deliveries, [])
