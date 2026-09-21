"""Behavioral tests for Neev's bounded Codex managed-login protocol."""

from pathlib import Path
from dataclasses import FrozenInstanceError
import importlib.util
import json
import sys
import unittest


MODULE_PATH = Path(__file__).parents[1] / "operations" / "neev_codex_login_protocol.py"
LOGIN_ID = "12345678-1234-4234-8234-123456789abc"
DEVICE = {
    "type": "chatgptDeviceCode",
    "loginId": LOGIN_ID,
    "verificationUrl": "https://auth.openai.com/codex/device",
    "userCode": "ABCD-1234",
}
ACCOUNT = {
    "requiresOpenaiAuth": True,
    "account": {
        "type": "chatgpt",
        "email": "person@example.invalid",
        "planType": "plus",
    },
}


def load_module():
    spec = importlib.util.spec_from_file_location("neev_codex_login_protocol", MODULE_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError("unable to load protocol module")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def frame(value):
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode("utf-8") + b"\n"


class CodexLoginProtocolTests(unittest.TestCase):
    def setUp(self):
        self.module = load_module()
        self.protocol = self.module.CodexLoginProtocol()

    def assert_protocol_error(self, code, operation, secret=None):
        with self.assertRaises(self.module.ProtocolError) as caught:
            operation()
        self.assertEqual(caught.exception.code, code)
        self.assertEqual(str(caught.exception), code)
        self.assertEqual(repr(caught.exception), "ProtocolError(%r)" % code)
        self.assertIsNone(caught.exception.__cause__)
        if secret is not None:
            self.assertNotIn(secret, str(caught.exception))
            self.assertNotIn(secret, repr(caught.exception))

    def initialize(self):
        request = json.loads(self.protocol.initialize())
        events = self.protocol.receive(
            frame({"id": request["id"], "result": {"userAgent": "invented-profile"}})
        )
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0].kind, "initialized")
        self.assertEqual(
            events[0].outbound,
            b'{"method":"initialized","params":{}}\n',
        )
        return request

    def reply(self, request, result):
        return self.protocol.receive(frame({"id": request["id"], "result": result}))

    def start_login(self):
        request = json.loads(self.protocol.start_login())
        events = self.reply(request, DEVICE)
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0].kind, "login_started")
        return request, events[0]

    def test_initialization_emits_only_the_fixed_handshake_notification(self):
        request_bytes = self.protocol.initialize()
        self.assertTrue(request_bytes.endswith(b"\n"))
        self.assertEqual(
            json.loads(request_bytes),
            {
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "neev",
                        "title": "Neev",
                        "version": "0.1.0",
                    }
                },
            },
        )
        events = self.protocol.receive(
            frame(
                {
                    "id": 1,
                    "result": {
                        "userAgent": "invented-profile",
                        "platformFamily": "invented-linux",
                        "platformOs": "invented-os",
                    },
                }
            )
        )
        self.assertEqual(events[0].kind, "initialized")
        self.assertEqual(json.loads(events[0].outbound), {"method": "initialized", "params": {}})
        with self.assertRaises(FrozenInstanceError):
            events[0].kind = "changed"

    def test_protocol_error_never_exposes_an_unrecognized_code(self):
        for unsafe in ("secret-provider-detail", ["provider-secret"]):
            with self.subTest(unsafe=unsafe):
                error = self.module.ProtocolError(unsafe)
                self.assertEqual(error.code, "invalid")
                self.assertEqual(str(error), "invalid")
                self.assertNotIn("provider-secret", repr(error))

    def test_operations_require_completed_initialization_and_initialize_is_one_shot(self):
        operations = (
            self.protocol.read_account,
            self.protocol.start_login,
            self.protocol.cancel_login,
            self.protocol.logout,
        )
        for operation in operations:
            with self.subTest(operation=operation.__name__):
                self.assert_protocol_error("invalid", operation)
        self.protocol.initialize()
        self.assert_protocol_error("conflict", self.protocol.initialize)
        self.protocol.receive(frame({"id": 1, "result": {"userAgent": "invented"}}))
        self.assert_protocol_error("invalid", self.protocol.initialize)

    def test_request_ids_increase_and_only_one_request_can_be_in_flight(self):
        initialize = self.initialize()
        account = json.loads(self.protocol.read_account())
        self.assertEqual(account, {"id": initialize["id"] + 1, "method": "account/read", "params": {"refreshToken": False}})
        self.assert_protocol_error("conflict", self.protocol.start_login)
        self.reply(account, {"requiresOpenaiAuth": True, "account": None})
        login = json.loads(self.protocol.start_login())
        self.assertEqual(login["id"], account["id"] + 1)
        self.assertEqual(login["method"], "account/login/start")
        self.assertEqual(login["params"], {"type": "chatgptDeviceCode"})

    def test_account_refresh_requires_a_real_bool(self):
        self.initialize()
        for value in (0, 1, None, "true"):
            with self.subTest(value=value):
                self.assert_protocol_error("invalid", lambda: self.protocol.read_account(value))
        request = json.loads(self.protocol.read_account(True))
        self.assertEqual(request["params"], {"refreshToken": True})

    def test_supported_chatgpt_plans_project_without_exposing_email_in_repr(self):
        self.initialize()
        for plan in ("plus", "pro", "team", "business", "enterprise", "edu"):
            with self.subTest(plan=plan):
                request = json.loads(self.protocol.read_account())
                result = dict(ACCOUNT)
                result["account"] = dict(ACCOUNT["account"], planType=plan)
                event = self.reply(request, result)[0]
                self.assertEqual(event.kind, "account")
                self.assertEqual(event.account.status, "supported")
                self.assertEqual(event.account.plan, plan)
                self.assertEqual(event.account.email, "person@example.invalid")
                self.assertNotIn("person@example.invalid", repr(event))
                self.assertNotIn("person@example.invalid", repr(event.account))

    def test_disconnected_and_well_formed_unsupported_accounts_never_connect(self):
        self.initialize()
        cases = (
            ({"requiresOpenaiAuth": True}, "disconnected"),
            ({"requiresOpenaiAuth": True, "account": None}, "disconnected"),
            ({"requiresOpenaiAuth": True, "account": {"type": "apiKey"}}, "unsupported"),
            ({"requiresOpenaiAuth": True, "account": {"type": "amazonBedrock"}}, "unsupported"),
            ({"requiresOpenaiAuth": True, "account": {"type": "amazonBedrock", "usesCodexManagedCredentials": True}}, "unsupported"),
            (dict(ACCOUNT, requiresOpenaiAuth=False), "unsupported"),
            ({"requiresOpenaiAuth": True, "account": {"type": "chatgpt", "email": None, "planType": "free"}}, "unsupported"),
            ({"requiresOpenaiAuth": True, "account": {"type": "chatgpt", "email": None, "planType": "unknown"}}, "unsupported"),
            ({"requiresOpenaiAuth": True, "account": {"type": "chatgpt", "email": None, "planType": "future_bounded_plan"}}, "unsupported"),
            ({"requiresOpenaiAuth": True, "account": {"type": "chatgpt", "email": None, "planType": ""}}, "unsupported"),
            ({"requiresOpenaiAuth": True, "account": {"type": "chatgpt", "email": None, "planType": "bad\nplan"}}, "unsupported"),
            ({"requiresOpenaiAuth": True, "account": {"type": "chatgpt", "email": None, "planType": "x" * 129}}, "unsupported"),
        )
        for result, status in cases:
            with self.subTest(result=result):
                request = json.loads(self.protocol.read_account())
                event = self.reply(request, result)[0]
                self.assertEqual(event.account.status, status)
                self.assertNotEqual(event.account.status, "connected")
                if status == "unsupported":
                    self.assertIsNone(event.account.plan)
                    self.assertIsNone(event.account.email)

    def test_account_shapes_bool_and_email_are_strict(self):
        self.initialize()
        invalid = (
            {},
            {"requiresOpenaiAuth": 1},
            {"requiresOpenaiAuth": True, "extra": None},
            {"requiresOpenaiAuth": True, "account": {}},
            {"requiresOpenaiAuth": True, "account": {"type": "apiKey", "extra": False}},
            {"requiresOpenaiAuth": True, "account": {"type": "amazonBedrock", "usesCodexManagedCredentials": 1}},
            {"requiresOpenaiAuth": True, "account": {"type": "chatgpt", "email": None}},
            {"requiresOpenaiAuth": True, "account": {"type": "chatgpt", "email": 7, "planType": "plus"}},
            {"requiresOpenaiAuth": True, "account": {"type": "chatgpt", "email": "a\n@example.invalid", "planType": "plus"}},
            {"requiresOpenaiAuth": True, "account": {"type": "chatgpt", "email": "a" * 255, "planType": "plus"}},
            {"requiresOpenaiAuth": True, "account": {"type": "chatgpt", "email": None, "planType": ["plus"]}},
            {"requiresOpenaiAuth": True, "account": {"type": "future", "email": None, "planType": "plus"}},
        )
        for result in invalid:
            with self.subTest(result=result):
                request = json.loads(self.protocol.read_account())
                self.assert_protocol_error("invalid", lambda: self.reply(request, result))
                self.assert_protocol_error("invalid", self.protocol.read_account)
                self.protocol = self.module.CodexLoginProtocol()
                self.initialize()

    def test_nullable_email_is_a_valid_private_label(self):
        self.initialize()
        request = json.loads(self.protocol.read_account())
        result = {"requiresOpenaiAuth": True, "account": {"type": "chatgpt", "email": None, "planType": "plus"}}
        event = self.reply(request, result)[0]
        self.assertEqual(event.account.status, "supported")
        self.assertIsNone(event.account.email)

    def test_initialize_response_shape_and_bounded_strings_are_strict(self):
        invalid = (
            [],
            {},
            {"userAgent": ""},
            {"userAgent": "x" * 257},
            {"userAgent": "x", "platformFamily": 1},
            {"userAgent": "x", "platformOs": ""},
            {"userAgent": "x", "extra": "future"},
        )
        for result in invalid:
            with self.subTest(result=result):
                protocol = self.module.CodexLoginProtocol()
                request = json.loads(protocol.initialize())
                self.assert_protocol_error("invalid", lambda: protocol.receive(frame({"id": request["id"], "result": result})))

    def test_initialize_accepts_the_real_four_field_result(self):
        # 0.155.1 returns userAgent, codexHome, platformFamily, platformOs; all are
        # tolerated (codexHome is informational and must not reject the handshake).
        protocol = self.module.CodexLoginProtocol()
        request = json.loads(protocol.initialize())
        events = protocol.receive(frame({"id": request["id"], "result": {
            "userAgent": "codex/0.155.1", "codexHome": "/home/user/.codex",
            "platformFamily": "linux", "platformOs": "debian",
        }}))
        self.assertEqual([event.kind for event in events], ["initialized"])

    def test_device_result_is_exact_and_rejects_url_and_code_attacks(self):
        self.initialize()
        invalid = (
            dict(DEVICE, type="chatgpt"),
            dict(DEVICE, loginId=""),
            dict(DEVICE, loginId="x" * 129),
            dict(DEVICE, loginId="bad.id"),
            dict(DEVICE, userCode="lowercase"),
            dict(DEVICE, userCode="A" * 65),
            dict(DEVICE, verificationUrl="http://auth.openai.com/codex/device"),
            dict(DEVICE, verificationUrl="https://evil.invalid/codex/device"),
            dict(DEVICE, verificationUrl="https://user@auth.openai.com/codex/device"),
            dict(DEVICE, verificationUrl="https://auth.openai.com:443/codex/device"),
            dict(DEVICE, verificationUrl="https://auth.openai.com/codex/device/"),
            dict(DEVICE, verificationUrl="https://auth.openai.com/codex/device?q=secret"),
            dict(DEVICE, verificationUrl="https://auth.openai.com/codex/device#secret"),
            dict(DEVICE, extra="future"),
        )
        for result in invalid:
            with self.subTest(result=result):
                request = json.loads(self.protocol.start_login())
                self.assert_protocol_error("invalid", lambda: self.reply(request, result))
                self.protocol = self.module.CodexLoginProtocol()
                self.initialize()

    def test_fragmented_utf8_and_ndjson_are_parsed_incrementally(self):
        request = json.loads(self.protocol.initialize())
        encoded = frame({"id": request["id"], "result": {"userAgent": "invented-雪"}})
        split = encoded.index("雪".encode("utf-8")) + 1
        self.assertEqual(self.protocol.receive(encoded[:split]), [])
        events = self.protocol.receive(encoded[split:])
        self.assertEqual(events[0].kind, "initialized")

        first = json.loads(self.protocol.read_account())
        payload = frame({"id": first["id"], "result": {"requiresOpenaiAuth": True, "account": None}})
        self.assertEqual(self.protocol.receive(payload[:-1]), [])
        self.assertEqual(self.protocol.receive(payload[-1:])[0].account.status, "disconnected")

    def test_duplicate_keys_nonfinite_and_invalid_utf8_fail_closed(self):
        bad_frames = (
            b'{"id":1,"id":1,"result":{"userAgent":"x"}}\n',
            b'{"id":1,"result":{"userAgent":"x","value":NaN}}\n',
            b'{"id":1,"error":{"data":1e400}}\n',
            b'{"id":1,"result":{"userAgent":"\xff"}}\n',
        )
        for bad in bad_frames:
            with self.subTest(bad=bad):
                protocol = self.module.CodexLoginProtocol()
                protocol.initialize()
                self.assert_protocol_error("invalid", lambda: protocol.receive(bad))
                self.assert_protocol_error("invalid", protocol.initialize)

    def test_deeply_nested_json_is_a_fixed_fatal_invalid_error(self):
        protocol = self.module.CodexLoginProtocol()
        protocol.initialize()
        nested = b"[" * 1_100 + b"0" + b"]" * 1_100
        payload = b'{"id":1,"error":{"data":' + nested + b"}}\n"
        self.assertLess(len(payload), 65_536)
        self.assert_protocol_error("invalid", lambda: protocol.receive(payload))
        self.assert_protocol_error("invalid", protocol.initialize)

    def test_frame_and_chunk_limits_count_the_newline(self):
        self.initialize()
        self.start_login()
        base = {"method": "account/login/completed", "params": {"loginId": LOGIN_ID, "success": False, "error": ""}}
        base_size = len(frame(base))
        exact = dict(base)
        exact["params"] = dict(base["params"], error="x" * (65536 - base_size))
        exact_bytes = frame(exact)
        self.assertEqual(len(exact_bytes), 65536)
        events = self.protocol.receive(exact_bytes)
        self.assertEqual(events[0].kind, "login_failed")

        protocol = self.module.CodexLoginProtocol()
        protocol.initialize()
        self.assert_protocol_error("overflow", lambda: protocol.receive(b" " * 65536 + b"\n"))
        other = self.module.CodexLoginProtocol()
        other.initialize()
        self.assert_protocol_error("overflow", lambda: other.receive(b"x" * 65537))

    def test_fragmented_input_never_extends_retained_buffer_past_limit(self):
        class CappedBuffer(bytearray):
            def extend(buffer, value):
                if len(buffer) + len(value) > 65_536:
                    raise AssertionError("retained buffer exceeded limit")
                super().extend(value)

        self.initialize()
        self.protocol._buffer = CappedBuffer()
        notification = frame({"method": "account/updated", "params": {}})[:-1]
        first = notification + b" " * (65_535 - len(notification))
        self.assertEqual(len(first), 65_535)
        self.assertEqual(self.protocol.receive(first), [])
        second = b"\n" + b" " * 65_535
        self.assertEqual(len(second), 65_536)
        self.assertEqual(self.protocol.receive(second), [])
        self.assertEqual(len(self.protocol._buffer), 65_535)

    def test_response_id_and_envelope_must_exactly_settle_pending_request(self):
        invalid = (
            {"id": True, "result": {"userAgent": "x"}},
            {"id": "1", "result": {"userAgent": "x"}},
            {"id": 2, "result": {"userAgent": "x"}},
            {"id": 1, "result": {"userAgent": "x"}, "error": {"message": "secret"}},
            {"id": 1, "result": {"userAgent": "x"}, "extra": None},
        )
        for response in invalid:
            with self.subTest(response=response):
                protocol = self.module.CodexLoginProtocol()
                protocol.initialize()
                self.assert_protocol_error("invalid", lambda: protocol.receive(frame(response)), secret="secret")

    def test_unsolicited_and_duplicate_responses_fail_closed(self):
        self.assert_protocol_error("invalid", lambda: self.protocol.receive(frame({"id": 1, "result": {}})))
        protocol = self.module.CodexLoginProtocol()
        request = json.loads(protocol.initialize())
        response = frame({"id": request["id"], "result": {"userAgent": "x"}})
        protocol.receive(response)
        self.assert_protocol_error("invalid", lambda: protocol.receive(response))

    def test_provider_error_is_sanitized_and_settles_without_closing_session(self):
        self.initialize()
        request = json.loads(self.protocol.read_account())
        canary = "provider-secret-canary"
        self.assert_protocol_error(
            "provider_failed",
            lambda: self.protocol.receive(frame({"id": request["id"], "error": {"message": canary, "data": canary}})),
            secret=canary,
        )
        retry = json.loads(self.protocol.read_account())
        self.assertGreater(retry["id"], request["id"])

    def test_provider_error_retains_a_fragmented_notification_and_session_alignment(self):
        self.initialize()
        request = json.loads(self.protocol.read_account())
        error = frame({"id": request["id"], "error": {"message": "invented"}})
        notification = frame({"method": "account/updated", "params": {}})
        split = 20
        self.assert_protocol_error(
            "provider_failed",
            lambda: self.protocol.receive(error + notification[:split]),
        )
        self.assertEqual(self.protocol.receive(notification[split:]), [])
        retry = json.loads(self.protocol.read_account())
        event = self.reply(
            retry, {"requiresOpenaiAuth": True, "account": None}
        )[0]
        self.assertEqual(event.account.status, "disconnected")

    def test_provider_error_validates_all_complete_trailing_notifications(self):
        self.initialize()
        request = json.loads(self.protocol.read_account())
        error = frame({"id": request["id"], "error": {"message": "invented"}})
        updates = (
            frame({"method": "account/updated", "params": {}})
            + frame(
                {
                    "method": "account/updated",
                    "params": {"authMode": "chatgpt", "planType": "plus"},
                }
            )
        )
        self.assert_protocol_error(
            "provider_failed", lambda: self.protocol.receive(error + updates)
        )
        retry = json.loads(self.protocol.read_account(refresh=True))
        self.assertEqual(retry["method"], "account/read")

    def test_fatal_tail_error_dominates_recoverable_provider_error(self):
        self.initialize()
        request = json.loads(self.protocol.read_account())
        error = frame({"id": request["id"], "error": {"message": "invented"}})
        malformed = frame(
            {"method": "account/updated", "params": {"authMode": "future"}}
        )
        self.assert_protocol_error(
            "invalid", lambda: self.protocol.receive(error + malformed)
        )
        self.assert_protocol_error("invalid", self.protocol.read_account)

    def test_exact_login_completion_requires_an_independent_refreshed_account_read(self):
        self.initialize()
        unused_request, login_event = self.start_login()
        self.assertEqual(login_event.login.login_id, LOGIN_ID)
        self.assertEqual(login_event.login.verification_url, DEVICE["verificationUrl"])
        self.assertEqual(login_event.login.user_code, DEVICE["userCode"])
        self.assertNotIn(LOGIN_ID, repr(login_event))
        self.assertNotIn(DEVICE["userCode"], repr(login_event))
        self.assert_protocol_error("conflict", self.protocol.read_account)

        completion = {"method": "account/login/completed", "params": {"loginId": LOGIN_ID, "success": True, "error": None}}
        event = self.protocol.receive(frame(completion))[0]
        self.assertEqual(event.kind, "account_check_required")
        self.assertNotEqual(event.kind, "connected")
        self.assert_protocol_error("invalid", self.protocol.read_account)
        account_request = json.loads(self.protocol.read_account(refresh=True))
        self.assertEqual(account_request["params"], {"refreshToken": True})
        account_event = self.reply(account_request, ACCOUNT)[0]
        self.assertEqual(account_event.account.status, "supported")

    def test_foreign_completion_is_harmless_and_cannot_settle_current_login(self):
        self.initialize()
        self.start_login()
        foreign = {"method": "account/login/completed", "params": {"loginId": "foreign", "success": True}}
        events = self.protocol.receive(frame(foreign))
        self.assertEqual(events, [])
        matched = {"method": "account/login/completed", "params": {"loginId": LOGIN_ID, "success": True}}
        self.assertEqual(self.protocol.receive(frame(matched))[0].kind, "account_check_required")

    def test_completion_shape_is_strict_and_raw_failure_is_discarded(self):
        self.initialize()
        self.start_login()
        invalid = (
            {"loginId": LOGIN_ID, "success": 1},
            {"loginId": LOGIN_ID, "success": True, "error": 7},
            {"loginId": LOGIN_ID, "success": True, "onboardingEntrypoint": "other"},
            {"loginId": LOGIN_ID, "success": True, "extra": None},
        )
        for params in invalid:
            with self.subTest(params=params):
                protocol = self.protocol
                self.assert_protocol_error("invalid", lambda: protocol.receive(frame({"method": "account/login/completed", "params": params})))
                self.protocol = self.module.CodexLoginProtocol()
                self.initialize()
                self.start_login()

        canary = "raw-provider-failure-secret"
        event = self.protocol.receive(frame({"method": "account/login/completed", "params": {"loginId": LOGIN_ID, "success": False, "error": canary, "onboardingEntrypoint": None}}))[0]
        self.assertEqual(event.kind, "login_failed")
        self.assertNotIn(canary, repr(event))

    def test_cancel_invalidates_before_ack_and_late_success_cannot_revive(self):
        self.initialize()
        self.start_login()
        cancel = json.loads(self.protocol.cancel_login())
        self.assertEqual(cancel["method"], "account/login/cancel")
        self.assertEqual(cancel["params"], {"loginId": LOGIN_ID})
        late = {"method": "account/login/completed", "params": {"loginId": LOGIN_ID, "success": True}}
        self.assertEqual(self.protocol.receive(frame(late)), [])
        event = self.reply(cancel, {"status": "canceled"})[0]
        self.assertEqual(event.kind, "login_canceled")
        self.assertEqual(event.status, "canceled")
        self.assert_protocol_error("invalid", self.protocol.cancel_login)

    def test_cancel_response_is_exact_and_failure_does_not_restore_login(self):
        self.initialize()
        self.start_login()
        for result in ({"status": "other"}, {"status": "notFound", "extra": None}, {}):
            with self.subTest(result=result):
                protocol = self.protocol
                cancel = json.loads(protocol.cancel_login())
                self.assert_protocol_error("invalid", lambda: protocol.receive(frame({"id": cancel["id"], "result": result})))
                self.protocol = self.module.CodexLoginProtocol()
                self.initialize()
                self.start_login()

    def test_logout_invalidates_before_ack_and_response_must_be_empty(self):
        self.initialize()
        self.start_login()
        logout = json.loads(self.protocol.logout())
        self.assertEqual(logout["method"], "account/logout")
        self.assertEqual(logout["params"], {})
        late = {"method": "account/login/completed", "params": {"loginId": LOGIN_ID, "success": True}}
        self.assertEqual(self.protocol.receive(frame(late)), [])
        event = self.reply(logout, {})[0]
        self.assertEqual(event.kind, "logged_out")

        other = self.module.CodexLoginProtocol()
        request = json.loads(other.initialize())
        other.receive(frame({"id": request["id"], "result": {"userAgent": "x"}}))
        logout = json.loads(other.logout())
        self.assert_protocol_error("invalid", lambda: other.receive(frame({"id": logout["id"], "result": {"status": "ok"}})))

    def test_pinned_account_updates_are_validated_and_ignored(self):
        self.initialize()
        valid = [
            {},
            {"authMode": None},
            {"authMode": "apikey"},
            {"authMode": "chatgpt"},
            {"authMode": "chatgptAuthTokens"},
            {"authMode": "headers"},
            {"authMode": "agentIdentity"},
            {"authMode": "personalAccessToken"},
            {"authMode": "bedrockApiKey"},
            {"authMode": "bedrockAccessKeys"},
            {"planType": None},
            {"authMode": "chatgpt", "planType": "enterprise"},
        ]
        valid.extend(
            {"planType": plan}
            for plan in (
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
        for params in valid:
            with self.subTest(params=params):
                self.assertEqual(
                    self.protocol.receive(
                        frame({"method": "account/updated", "params": params})
                    ),
                    [],
                )

    def test_unknown_notifications_server_requests_and_bad_account_updates_fail_closed(self):
        invalid_updates = (
            {"authMode": "apiKey"},
            {"authMode": "unknown"},
            {"authMode": 7},
            {"authMode": ["chatgpt"]},
            {"planType": "future"},
            {"planType": 7},
            {"planType": ["plus"]},
            {"authMode": "chatgpt", "extra": None},
        )
        for params in invalid_updates:
            with self.subTest(params=params):
                protocol = self.module.CodexLoginProtocol()
                self.initialize_protocol(protocol)
                self.assert_protocol_error(
                    "invalid",
                    lambda: protocol.receive(
                        frame({"method": "account/updated", "params": params})
                    ),
                )

        # A server->client REQUEST (has BOTH id and method) is still fatal.
        protocol = self.module.CodexLoginProtocol()
        self.initialize_protocol(protocol)
        self.assert_protocol_error(
            "invalid",
            lambda: protocol.receive(frame({"id": 88, "method": "account/read", "params": {}})),
        )

    def test_unsolicited_and_unknown_notifications_are_ignored_with_extra_fields(self):
        # The real app-server (0.155.1) emits notifications we do not model — and every
        # notification carries emittedAtMs though the schema omits it. These must be
        # IGNORED (not fatal), even before initialize completes, or the session dies.
        protocol = self.module.CodexLoginProtocol()
        # Before initialize: an unsolicited configWarning must not fatal.
        self.assertEqual(
            protocol.receive(frame({"method": "configWarning", "params": {"emittedAtMs": 1}})), []
        )
        self.initialize_protocol(protocol)
        for value in (
            {"method": "remoteControl/status/changed", "params": {"x": 1}, "emittedAtMs": 9},
            {"method": "unknown/event", "params": {}},
        ):
            self.assertEqual(protocol.receive(frame(value)), [])
        # A KNOWN notification with the extra emittedAtMs is still accepted + validated.
        self.assertEqual(
            protocol.receive(frame({
                "method": "account/updated",
                "params": {"authMode": "chatgpt", "emittedAtMs": 3},
            })),
            [],
        )

    def initialize_protocol(self, protocol):
        request = json.loads(protocol.initialize())
        protocol.receive(
            frame({"id": request["id"], "result": {"userAgent": "x"}})
        )

    def test_close_is_irreversible_and_a_new_instance_cannot_recover_login(self):
        self.initialize()
        self.start_login()
        partial_secret = b'{"method":"account/login/completed","params":{"loginId":"' + LOGIN_ID.encode("ascii")
        self.assertEqual(self.protocol.receive(partial_secret), [])
        self.protocol.close()
        for operation in (
            self.protocol.initialize,
            self.protocol.read_account,
            self.protocol.start_login,
            self.protocol.cancel_login,
            self.protocol.logout,
            lambda: self.protocol.receive(b"\n"),
        ):
            self.assert_protocol_error("invalid", operation, secret=LOGIN_ID)
        restarted = self.module.CodexLoginProtocol()
        self.assert_protocol_error("invalid", restarted.cancel_login, secret=LOGIN_ID)

    def test_receive_requires_bytes_and_does_not_retain_successful_events(self):
        self.protocol.initialize()
        self.assert_protocol_error("invalid", lambda: self.protocol.receive("not-bytes"))
        protocol = self.module.CodexLoginProtocol()
        request = json.loads(protocol.initialize())
        events = protocol.receive(frame({"id": request["id"], "result": {"userAgent": "x"}}))
        self.assertEqual(len(events), 1)
        self.assertEqual(protocol.receive(b""), [])
        self.assertEqual(len(events), 1)


if __name__ == "__main__":
    unittest.main()
